mod encode;
mod filter;
mod inflate;
#[cfg(test)]
mod tests;

use crate::media::{self, Error, Result};
pub use encode::encode;
use filter::{Simd, unfilter};

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
const MAX_PIXELS: u64 = 1 << 28;

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
    trns: Option<[u16; 3]>,
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
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
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
        }
    }

    pub fn decode_into(&mut self, data: &[u8], out: &mut media::Rgba) -> Result<()> {
        let info = header(data)?;
        let mut layout = Layout {
            info,
            palette: [Entry([0, 0, 0, 255]); 256],
            has_palette: false,
            trns: None,
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
                }
                b"tRNS" => match info.color {
                    Color::Palette => {
                        for (e, a) in layout.palette.iter_mut().zip(body) {
                            e.0[3] = *a;
                        }
                    }
                    Color::Grey if len >= 2 => {
                        let g = u16::from_be_bytes([body[0], body[1]]);
                        layout.trns = Some([g, g, g]);
                    }
                    Color::Rgb if len >= 6 => {
                        let s = |i: usize| u16::from_be_bytes([body[i], body[i + 1]]);
                        layout.trns = Some([s(0), s(2), s(4)]);
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
        let compressed = match single {
            Some(s) if parts == 1 => s,
            _ => &self.idat[..],
        };
        let written = inflate::zlib(
            compressed,
            &mut self.inflated,
            expected,
            expected + 4 * h + 64,
        )?;
        if written < expected {
            return Err(invalid(format!(
                "the image data ends early ({written} of {expected} bytes)"
            )));
        }
        out.width = info.width;
        out.height = info.height;
        out.data.resize(w * h * 4, 0);
        let rows = std::mem::take(&mut self.inflated);
        let result = if info.interlaced {
            self.interlaced(&layout, &rows, out)
        } else {
            self.rows(&layout, &rows, w, h, &mut |y, rgba| {
                out.data[y * w * 4..(y + 1) * w * 4].copy_from_slice(rgba);
            })
        };
        self.inflated = rows;
        result
    }

    fn rows(
        &mut self,
        layout: &Layout,
        data: &[u8],
        width: usize,
        height: usize,
        put: &mut dyn FnMut(usize, &[u8]),
    ) -> Result<()> {
        let stride = layout.row_bytes(width);
        let bpp = layout.bpp();
        self.prev.clear();
        self.prev.resize(stride, 0);
        self.cur.clear();
        self.cur.resize(stride, 0);
        self.pass.resize(width * 4, 0);
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
            self.expand(layout, width, &mut pass[..width * 4]);
            put(y, &pass[..width * 4]);
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
            self.rows(layout, slice, x, y, &mut |j, rgba| {
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
