mod crc;
#[cfg(test)]
mod edges;
mod encode;
mod fastz;
mod filter;
mod inflate;
mod lz;
mod native;
#[cfg(test)]
mod native_tests;
mod pack;
mod par;
#[cfg(test)]
mod tests;

use crate::media::{self, Error, Result};
use crc::crc;
pub use encode::{Compression, Options, Picture, encode, encode_picture, encode_rgba};
use filter::{Simd, unfilter};
pub use native::{Meta, Output, Text, Transform, metadata, output};
#[doc(hidden)]
pub use par::limit_threads;

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const MAX_PIXELS: u64 = 1 << 28;
const PIPELINE_MIN: usize = 1 << 21;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Grey,
    Rgb,
    Palette,
    GreyAlpha,
    Rgba,
}

impl Color {
    fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Color::Grey,
            2 => Color::Rgb,
            3 => Color::Palette,
            4 => Color::GreyAlpha,
            6 => Color::Rgba,
            _ => return None,
        })
    }

    fn samples(self) -> usize {
        match self {
            Color::Grey | Color::Palette => 1,
            Color::GreyAlpha => 2,
            Color::Rgb => 3,
            Color::Rgba => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub depth: u8,
    pub color: Color,
    pub interlaced: bool,
}

fn invalid(what: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("PNG: {what}"))
}

fn be32(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| invalid("the file ends inside a chunk header"))
}

fn header(data: &[u8]) -> Result<Info> {
    if data.get(..8) != Some(&SIGNATURE[..]) {
        return Err(invalid("not a PNG file"));
    }
    if be32(data, 8)? != 13 || data.get(12..16) != Some(b"IHDR") {
        return Err(invalid("the first chunk is not a 13-byte IHDR"));
    }
    let body = data
        .get(16..29)
        .ok_or_else(|| invalid("the file ends inside IHDR"))?;
    let width = be32(body, 0)?;
    let height = be32(body, 4)?;
    let depth = body[8];
    let color = Color::from_byte(body[9])
        .ok_or_else(|| invalid(format!("unknown colour type {}", body[9])))?;
    if width == 0 || height == 0 {
        return Err(invalid("a zero width or height"));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(invalid(format!("{width}x{height} is too large")));
    }
    let depth_ok = match color {
        Color::Grey => matches!(depth, 1 | 2 | 4 | 8 | 16),
        Color::Palette => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok {
        return Err(invalid(format!(
            "bit depth {depth} with colour type {}",
            body[9]
        )));
    }
    if body[10] != 0 || body[11] != 0 {
        return Err(invalid("an unknown compression or filter method"));
    }
    let interlaced = match body[12] {
        0 => false,
        1 => true,
        n => return Err(invalid(format!("unknown interlace method {n}"))),
    };
    if data.get(29..33).and_then(|c| be32(c, 0).ok()) != Some(crc(&[&data[12..29]])) {
        return Err(invalid("a bad CRC on IHDR"));
    }
    Ok(Info {
        width,
        height,
        depth,
        color,
        interlaced,
    })
}

pub fn info(data: &[u8]) -> Result<Info> {
    header(data)
}

pub fn decode(data: &[u8]) -> Result<media::Rgba> {
    let mut out = media::Rgba::new(0, 0);
    Decoder::new().decode_into(data, &mut out)?;
    Ok(out)
}

#[derive(Clone, Copy)]
struct Entry([u8; 4]);

struct Layout {
    info: Info,
    palette: [Entry; 256],
    has_palette: bool,
    palette_len: usize,
    trns: Option<[u16; 3]>,
    trns_present: bool,
    alpha: Vec<u8>,
}

impl Layout {
    fn row_bytes(&self, width: usize) -> usize {
        (width * self.info.color.samples() * usize::from(self.info.depth)).div_ceil(8)
    }

    fn bpp(&self) -> usize {
        (self.info.color.samples() * usize::from(self.info.depth)).div_ceil(8)
    }
}

pub struct Decoder {
    idat: Vec<u8>,
    inflated: Vec<u8>,
    prev: Vec<u8>,
    cur: Vec<u8>,
    unpacked: Vec<u8>,
    pass: Vec<u8>,
    simd: Simd,
    pipeline_min: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) enum Target<'a> {
    Direct(&'a mut [u8]),
    Rows(Mode, &'a mut dyn FnMut(usize, &[u8])),
}

struct Direct<'a> {
    out: &'a mut [u8],
    stride: usize,
    bpp: usize,
    y: usize,
    simd: Simd,
}

impl inflate::Rows for Direct<'_> {
    fn rows(&mut self, data: &[u8]) -> Result<()> {
        for row in data.chunks_exact(self.stride + 1) {
            let y = self.y;
            let (done, rest) = self.out.split_at_mut(y * self.stride);
            let prev = if y == 0 {
                &[][..]
            } else {
                &done[(y - 1) * self.stride..]
            };
            if !unfilter(
                row[0],
                y == 0,
                prev,
                &row[1..],
                &mut rest[..self.stride],
                self.bpp,
                self.simd,
            ) {
                return Err(invalid(format!("unknown filter type {}", row[0])));
            }
            self.y += 1;
        }
        Ok(())
    }
}

struct Convert<'a, 'b> {
    dec: &'a mut Decoder,
    layout: &'a Layout,
    width: usize,
    stride: usize,
    bpp: usize,
    y: usize,
    mode: Mode,
    put: &'b mut dyn FnMut(usize, &[u8]),
    pass: Vec<u8>,
}

impl inflate::Rows for Convert<'_, '_> {
    fn rows(&mut self, data: &[u8]) -> Result<()> {
        let px = self.mode.pixel_bytes(self.layout);
        for row in data.chunks_exact(self.stride + 1) {
            if !unfilter(
                row[0],
                self.y == 0,
                &self.dec.prev,
                &row[1..],
                &mut self.dec.cur,
                self.bpp,
                self.dec.simd,
            ) {
                return Err(invalid(format!("unknown filter type {}", row[0])));
            }
            let n = match self.mode {
                Mode::Rgba => {
                    self.dec
                        .expand(self.layout, self.width, &mut self.pass[..self.width * 4]);
                    self.width * 4
                }
                Mode::Native(t) => self.dec.native(
                    self.layout,
                    t,
                    self.width,
                    &mut self.pass[..self.width * px],
                ),
            };
            (self.put)(self.y, &self.pass[..n]);
            std::mem::swap(&mut self.dec.prev, &mut self.dec.cur);
            self.y += 1;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Rgba,
    Native(Transform),
}

const XORIG: [usize; 7] = [0, 4, 0, 2, 0, 1, 0];
const YORIG: [usize; 7] = [0, 0, 4, 0, 2, 0, 1];
const XSPC: [usize; 7] = [8, 8, 4, 4, 2, 2, 1];
const YSPC: [usize; 7] = [8, 8, 8, 4, 4, 2, 2];

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            idat: Vec::new(),
            inflated: Vec::new(),
            prev: Vec::new(),
            cur: Vec::new(),
            unpacked: Vec::new(),
            pass: Vec::new(),
            simd: Simd::detect(),
            pipeline_min: PIPELINE_MIN,
        }
    }

    fn prepare<'d>(&mut self, data: &'d [u8]) -> Result<(Layout, usize, Option<&'d [u8]>)> {
        let info = header(data)?;
        let mut layout = Layout {
            info,
            palette: [Entry([0, 0, 0, 255]); 256],
            has_palette: false,
            palette_len: 0,
            trns: None,
            trns_present: false,
            alpha: Vec::new(),
        };
        let mut single: Option<&[u8]> = None;
        let mut parts = 0usize;
        self.idat.clear();
        let mut at = 33usize;
        let mut ended = false;
        while at + 8 <= data.len() {
            let len = be32(data, at)? as usize;
            let kind = &data[at + 4..at + 8];
            let body = data
                .get(at + 8..at + 8 + len)
                .ok_or_else(|| invalid("a chunk runs past the end of the file"))?;
            let sum = data.get(at + 8 + len..at + 12 + len);
            let critical = kind[0].is_ascii_uppercase();
            if critical || matches!(kind, b"tRNS") {
                let ok = sum.is_some_and(|s| be32(s, 0).ok() == Some(crc(&[kind, body])));
                if !ok && critical {
                    return Err(invalid(format!(
                        "a bad CRC on {}",
                        String::from_utf8_lossy(kind)
                    )));
                }
                if !ok {
                    at = at + 12 + len;
                    continue;
                }
            }
            at = at + 12 + len;
            match kind {
                b"IDAT" => {
                    parts += 1;
                    match (parts, single) {
                        (1, _) => single = Some(body),
                        (2, Some(first)) => {
                            self.idat.extend_from_slice(first);
                            self.idat.extend_from_slice(body);
                        }
                        _ => self.idat.extend_from_slice(body),
                    }
                }
                b"PLTE" => {
                    if !len.is_multiple_of(3) || len > 768 {
                        return Err(invalid("a PLTE chunk of the wrong length"));
                    }
                    for (e, rgb) in layout.palette.iter_mut().zip(body.chunks_exact(3)) {
                        e.0[..3].copy_from_slice(rgb);
                    }
                    layout.has_palette = true;
                    let usable = match info.color {
                        Color::Palette => 1usize << info.depth,
                        _ => 256,
                    };
                    layout.palette_len = (len / 3).min(usable);
                }
                b"tRNS" => match info.color {
                    Color::Palette if !body.is_empty() => {
                        layout.trns_present = true;
                        layout.alpha = body.to_vec();
                        if len > layout.palette_len {
                            continue;
                        }
                        for (e, a) in layout.palette.iter_mut().zip(body) {
                            e.0[3] = *a;
                        }
                    }
                    Color::Grey if len >= 2 => {
                        let g = u16::from_be_bytes([body[0], body[1]]);
                        layout.trns = Some([g, g, g]);
                        layout.trns_present = true;
                    }
                    Color::Rgb if len >= 6 => {
                        let s = |i: usize| u16::from_be_bytes([body[i], body[i + 1]]);
                        layout.trns = Some([s(0), s(2), s(4)]);
                        layout.trns_present = true;
                    }
                    _ => {}
                },
                b"IEND" => {
                    ended = true;
                    break;
                }
                _ => {}
            }
        }
        if parts == 0 {
            return Err(invalid(if ended {
                "no image data"
            } else {
                "the file ends before any image data"
            }));
        }
        if info.color == Color::Palette && !layout.has_palette {
            return Err(invalid("a palette image without PLTE"));
        }
        let (w, h) = (info.width as usize, info.height as usize);
        let expected = if info.interlaced {
            (0..7)
                .map(|p| {
                    let (x, y) = pass_size(p, w, h);
                    if x == 0 || y == 0 {
                        0
                    } else {
                        (layout.row_bytes(x) + 1) * y
                    }
                })
                .sum()
        } else {
            (layout.row_bytes(w) + 1) * h
        };
        let single = if parts == 1 { single } else { None };
        Ok((layout, expected, single))
    }

    fn inflate_all(&mut self, compressed: &[u8], expected: usize, height: usize) -> Result<()> {
        let written = inflate::zlib(
            compressed,
            &mut self.inflated,
            expected,
            expected + 4 * height + 64,
        )?;
        if written < expected {
            return Err(invalid(format!(
                "the image data ends early ({written} of {expected} bytes)"
            )));
        }
        Ok(())
    }

    fn stream(
        &mut self,
        layout: &Layout,
        compressed: &[u8],
        expected: usize,
        width: usize,
        height: usize,
        target: Target,
    ) -> Result<()> {
        let stride = layout.row_bytes(width);
        let bpp = layout.bpp();
        let limit = expected + 4 * height + 64;
        let result = match target {
            Target::Direct(out) => {
                let mut sink = Direct {
                    out,
                    stride,
                    bpp,
                    y: 0,
                    simd: self.simd,
                };
                if expected >= self.pipeline_min && par::workers(2) > 1 {
                    inflate::zlib_piped(compressed, limit, stride + 1, height, &mut sink)
                } else {
                    self.inline(compressed, limit, stride + 1, height, &mut sink)
                }
            }
            Target::Rows(mode, put) => {
                self.prev.clear();
                self.prev.resize(stride, 0);
                self.cur.clear();
                self.cur.resize(stride, 0);
                let mut pass = std::mem::take(&mut self.pass);
                pass.resize(width * mode.pixel_bytes(layout), 0);
                let mut sink = Convert {
                    dec: self,
                    layout,
                    width,
                    stride,
                    bpp,
                    y: 0,
                    mode,
                    put,
                    pass,
                };
                let line = stride + 1;
                let mut ring = std::mem::take(&mut sink.dec.inflated);
                let need = inflate::ring_len(line);
                if ring.len() < need {
                    ring.resize(need, 0);
                }
                let result =
                    inflate::zlib_rows(compressed, &mut ring, limit, line, height, &mut sink);
                let pass = std::mem::take(&mut sink.pass);
                self.inflated = ring;
                self.pass = pass;
                result
            }
        };
        result.map(|_| ())
    }

    fn inline(
        &mut self,
        compressed: &[u8],
        limit: usize,
        line: usize,
        height: usize,
        sink: &mut dyn inflate::Rows,
    ) -> Result<usize> {
        let mut ring = std::mem::take(&mut self.inflated);
        let need = inflate::ring_len(line);
        if ring.len() < need {
            ring.resize(need, 0);
        }
        let result = inflate::zlib_rows(compressed, &mut ring, limit, line, height, sink);
        self.inflated = ring;
        result
    }

    pub fn decode_into(&mut self, data: &[u8], out: &mut media::Rgba) -> Result<()> {
        let (layout, expected, single) = self.prepare(data)?;
        let info = layout.info;
        let (w, h) = (info.width as usize, info.height as usize);
        out.width = info.width;
        out.height = info.height;
        out.data.resize(w * h * 4, 0);
        let idat = std::mem::take(&mut self.idat);
        let compressed = single.unwrap_or(&idat[..]);
        let result = if info.interlaced {
            self.inflate_all(compressed, expected, h).and_then(|()| {
                let rows = std::mem::take(&mut self.inflated);
                let result = self.interlaced(&layout, &rows, out);
                self.inflated = rows;
                result
            })
        } else if info.color == Color::Rgba && info.depth == 8 {
            self.stream(
                &layout,
                compressed,
                expected,
                w,
                h,
                Target::Direct(&mut out.data),
            )
        } else {
            let pixels = &mut out.data;
            self.stream(
                &layout,
                compressed,
                expected,
                w,
                h,
                Target::Rows(Mode::Rgba, &mut |y, rgba| {
                    pixels[y * w * 4..(y + 1) * w * 4].copy_from_slice(rgba);
                }),
            )
        };
        self.idat = idat;
        result
    }

    fn rows(
        &mut self,
        layout: &Layout,
        data: &[u8],
        width: usize,
        height: usize,
        mode: Mode,
        put: &mut dyn FnMut(usize, &[u8]),
    ) -> Result<()> {
        let px = mode.pixel_bytes(layout);
        let stride = layout.row_bytes(width);
        let bpp = layout.bpp();
        self.prev.clear();
        self.prev.resize(stride, 0);
        self.cur.clear();
        self.cur.resize(stride, 0);
        self.pass.resize(width * px, 0);
        let mut pass = std::mem::take(&mut self.pass);
        let mut result = Ok(());
        for (y, line) in data.chunks_exact(stride + 1).take(height).enumerate() {
            if !unfilter(
                line[0],
                y == 0,
                &self.prev,
                &line[1..],
                &mut self.cur,
                bpp,
                self.simd,
            ) {
                result = Err(invalid(format!("unknown filter type {}", line[0])));
                break;
            }
            let n = match mode {
                Mode::Rgba => {
                    self.expand(layout, width, &mut pass[..width * 4]);
                    width * 4
                }
                Mode::Native(t) => self.native(layout, t, width, &mut pass[..width * px]),
            };
            put(y, &pass[..n]);
            std::mem::swap(&mut self.prev, &mut self.cur);
        }
        self.pass = pass;
        result
    }

    fn interlaced(&mut self, layout: &Layout, data: &[u8], out: &mut media::Rgba) -> Result<()> {
        let (w, h) = (out.width as usize, out.height as usize);
        let mut offset = 0usize;
        for p in 0..7 {
            let (x, y) = pass_size(p, w, h);
            if x == 0 || y == 0 {
                continue;
            }
            let len = (layout.row_bytes(x) + 1) * y;
            let slice = data
                .get(offset..offset + len)
                .ok_or_else(|| invalid("the interlaced data ends early"))?;
            let pixels = &mut out.data;
            self.rows(layout, slice, x, y, Mode::Rgba, &mut |j, rgba| {
                let oy = j * YSPC[p] + YORIG[p];
                for (i, px) in rgba.chunks_exact(4).enumerate() {
                    let ox = i * XSPC[p] + XORIG[p];
                    let at = (oy * w + ox) * 4;
                    pixels[at..at + 4].copy_from_slice(px);
                }
            })?;
            offset += len;
        }
        Ok(())
    }

    fn expand(&mut self, layout: &Layout, width: usize, out: &mut [u8]) {
        let info = layout.info;
        let cur = &self.cur;
        if info.depth < 8 {
            let scale = if info.color == Color::Palette {
                1
            } else {
                match info.depth {
                    1 => 0xff,
                    2 => 0x55,
                    _ => 0x11,
                }
            };
            self.unpacked.resize(width, 0);
            unpack_bits(info.depth, scale, cur, &mut self.unpacked[..width]);
            let src = &self.unpacked[..width];
            if info.color == Color::Palette {
                for (px, &i) in out.chunks_exact_mut(4).zip(src) {
                    px.copy_from_slice(&layout.palette[usize::from(i)].0);
                }
            } else {
                let key = layout.trns.map(|t| {
                    let mask = (1u16 << info.depth) - 1;
                    (t[0] & mask) as u8 * scale
                });
                for (px, &g) in out.chunks_exact_mut(4).zip(src) {
                    let a = if key == Some(g) { 0 } else { 255 };
                    px.copy_from_slice(&[g, g, g, a]);
                }
            }
            return;
        }
        let sixteen = info.depth == 16;
        match (info.color, sixteen) {
            (Color::Rgba, false) => out.copy_from_slice(&cur[..width * 4]),
            (Color::Rgb, false) => match layout.trns {
                None => {
                    for (px, rgb) in out.chunks_exact_mut(4).zip(cur.chunks_exact(3)) {
                        px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                    }
                }
                Some(t) => {
                    let key = [t[0] as u8, t[1] as u8, t[2] as u8];
                    let key = if t.iter().all(|v| *v < 256) {
                        Some(key)
                    } else {
                        None
                    };
                    for (px, rgb) in out.chunks_exact_mut(4).zip(cur.chunks_exact(3)) {
                        let a = if Some([rgb[0], rgb[1], rgb[2]]) == key {
                            0
                        } else {
                            255
                        };
                        px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
                    }
                }
            },
            (Color::Palette, _) => {
                for (px, &i) in out.chunks_exact_mut(4).zip(cur.iter()) {
                    px.copy_from_slice(&layout.palette[usize::from(i)].0);
                }
            }
            (Color::Grey, false) => {
                let key = layout.trns.and_then(|t| u8::try_from(t[0]).ok());
                for (px, &g) in out.chunks_exact_mut(4).zip(cur.iter()) {
                    let a = if Some(g) == key { 0 } else { 255 };
                    px.copy_from_slice(&[g, g, g, a]);
                }
            }
            (Color::GreyAlpha, false) => {
                for (px, ga) in out.chunks_exact_mut(4).zip(cur.chunks_exact(2)) {
                    px.copy_from_slice(&[ga[0], ga[0], ga[0], ga[1]]);
                }
            }
            (Color::Rgba, true) => {
                for (px, s) in out.chunks_exact_mut(4).zip(cur.chunks_exact(8)) {
                    px.copy_from_slice(&[s[0], s[2], s[4], s[6]]);
                }
            }
            (Color::Rgb, true) => {
                let key = layout.trns;
                for (px, s) in out.chunks_exact_mut(4).zip(cur.chunks_exact(6)) {
                    let v = |i: usize| u16::from_be_bytes([s[i], s[i + 1]]);
                    let a = if key == Some([v(0), v(2), v(4)]) {
                        0
                    } else {
                        255
                    };
                    px.copy_from_slice(&[s[0], s[2], s[4], a]);
                }
            }
            (Color::Grey, true) => {
                let key = layout.trns.map(|t| t[0]);
                for (px, s) in out.chunks_exact_mut(4).zip(cur.chunks_exact(2)) {
                    let a = if key == Some(u16::from_be_bytes([s[0], s[1]])) {
                        0
                    } else {
                        255
                    };
                    px.copy_from_slice(&[s[0], s[0], s[0], a]);
                }
            }
            (Color::GreyAlpha, true) => {
                for (px, s) in out.chunks_exact_mut(4).zip(cur.chunks_exact(4)) {
                    px.copy_from_slice(&[s[0], s[0], s[0], s[2]]);
                }
            }
        }
    }
}

fn pass_size(p: usize, w: usize, h: usize) -> (usize, usize) {
    (
        w.saturating_sub(XORIG[p]).div_ceil(XSPC[p]),
        h.saturating_sub(YORIG[p]).div_ceil(YSPC[p]),
    )
}

fn unpack_bits(depth: u8, scale: u8, input: &[u8], out: &mut [u8]) {
    let per = usize::from(8 / depth);
    let mask = (1u8 << depth) - 1;
    for (bytes, &v) in out.chunks_mut(per).zip(input) {
        for (k, o) in bytes.iter_mut().enumerate() {
            let shift = 8 - depth as usize * (k + 1);
            *o = scale.wrapping_mul((v >> shift) & mask);
        }
    }
}
