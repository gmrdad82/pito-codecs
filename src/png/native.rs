use super::{
    Color, Decoder, Entry, Info, Layout, Mode, XORIG, XSPC, YORIG, YSPC, be32, crc, header,
    inflate, invalid, pass_size, unpack_bits,
};
use crate::media::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Identity,
    Expand,
    Normalize8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Output {
    pub width: u32,
    pub height: u32,
    pub color: Color,
    pub depth: u8,
    pub row_bytes: usize,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub keyword: String,
    pub text: String,
    pub language: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub srgb: Option<u8>,
    pub gamma: Option<u32>,
    pub chromaticities: Option<[u32; 8]>,
    pub pixel_dims: Option<(u32, u32, u8)>,
    pub palette: Vec<[u8; 3]>,
    pub transparency: Vec<u8>,
    pub text: Vec<Text>,
}

fn target(info: &Info, trns: bool, t: Transform) -> (Color, u8) {
    if t == Transform::Identity {
        return (info.color, info.depth);
    }
    let depth = if t == Transform::Normalize8 {
        8
    } else {
        info.depth.max(8)
    };
    let color = match (info.color, trns) {
        (Color::Palette, false) | (Color::Rgb, false) => Color::Rgb,
        (Color::Palette, true) | (Color::Rgb, true) | (Color::Rgba, _) => Color::Rgba,
        (Color::Grey, false) => Color::Grey,
        (Color::Grey, true) | (Color::GreyAlpha, _) => Color::GreyAlpha,
    };
    let depth = if info.color == Color::Palette {
        8
    } else {
        depth
    };
    (color, depth)
}

fn describe(info: &Info, trns: bool, t: Transform) -> Result<Output> {
    let (color, depth) = target(info, trns, t);
    let bits = color.samples() * usize::from(depth);
    let width = info.width as usize;
    let row_bytes = (width * bits).div_ceil(8);
    let bytes = row_bytes
        .checked_mul(info.height as usize)
        .ok_or_else(|| invalid("the output is too large"))?;
    Ok(Output {
        width: info.width,
        height: info.height,
        color,
        depth,
        row_bytes,
        bytes,
    })
}

fn chunks<'a>(data: &'a [u8], mut each: impl FnMut(&'a [u8], &'a [u8])) -> Result<Info> {
    let info = header(data)?;
    let mut at = 33usize;
    while at + 8 <= data.len() {
        let len = be32(data, at)? as usize;
        let kind = &data[at + 4..at + 8];
        let Some(body) = data.get(at + 8..at + 8 + len) else {
            break;
        };
        let ok = data
            .get(at + 8 + len..at + 12 + len)
            .is_some_and(|s| be32(s, 0).ok() == Some(crc(&[kind, body])));
        at += 12 + len;
        if kind == b"IEND" {
            break;
        }
        if ok {
            each(kind, body);
        }
    }
    Ok(info)
}

pub fn metadata(data: &[u8]) -> Result<Meta> {
    let mut m = Meta::default();
    chunks(data, |kind, body| match kind {
        b"sRGB" if !body.is_empty() => m.srgb = Some(body[0]),
        b"gAMA" if body.len() == 4 => m.gamma = be32(body, 0).ok(),
        b"cHRM" if body.len() == 32 => {
            let mut v = [0u32; 8];
            for (i, o) in v.iter_mut().enumerate() {
                *o = be32(body, i * 4).unwrap_or(0);
            }
            m.chromaticities = Some(v);
        }
        b"pHYs" if body.len() == 9 => {
            m.pixel_dims = Some((
                be32(body, 0).unwrap_or(0),
                be32(body, 4).unwrap_or(0),
                body[8],
            ));
        }
        b"PLTE" => m.palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
        b"tRNS" => m.transparency = body.to_vec(),
        b"tEXt" => {
            if let Some(z) = body.iter().position(|&b| b == 0) {
                m.text.push(Text {
                    keyword: latin1(&body[..z]),
                    text: latin1(&body[z + 1..]),
                    language: String::new(),
                });
            }
        }
        b"zTXt" => {
            if let Some(z) = body.iter().position(|&b| b == 0)
                && body.get(z + 1) == Some(&0)
                && let Some(text) = unzip(&body[z + 2..])
            {
                m.text.push(Text {
                    keyword: latin1(&body[..z]),
                    text: latin1(&text),
                    language: String::new(),
                });
            }
        }
        b"iTXt" => {
            if let Some(t) = international(body) {
                m.text.push(t);
            }
        }
        _ => {}
    })?;
    Ok(m)
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

fn unzip(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let n = inflate::zlib(data, &mut out, data.len() * 4, 1 << 24).ok()?;
    out.truncate(n);
    Some(out)
}

fn international(body: &[u8]) -> Option<Text> {
    let z = body.iter().position(|&b| b == 0)?;
    let flag = *body.get(z + 1)?;
    let rest = body.get(z + 3..)?;
    let l = rest.iter().position(|&b| b == 0)?;
    let rest2 = &rest[l + 1..];
    let t = rest2.iter().position(|&b| b == 0)?;
    let raw = &rest2[t + 1..];
    let text = if flag == 1 { unzip(raw)? } else { raw.to_vec() };
    Some(Text {
        keyword: latin1(&body[..z]),
        text: String::from_utf8(text).ok()?,
        language: String::from_utf8_lossy(&rest[..l]).into_owned(),
    })
}

fn trns_of(data: &[u8], info: &Info) -> bool {
    let mut present = false;
    let _ = chunks(data, |kind, body| {
        if kind == b"tRNS" {
            present = match info.color {
                Color::Palette => !body.is_empty(),
                Color::Grey => body.len() >= 2,
                Color::Rgb => body.len() >= 6,
                _ => false,
            }
        }
    });
    present
}

pub fn output(data: &[u8], t: Transform) -> Result<Output> {
    let info = header(data)?;
    describe(&info, trns_of(data, &info), t)
}

impl Decoder {
    pub fn decode_picture(&mut self, data: &[u8], t: Transform) -> Result<super::Picture> {
        let out = output(data, t)?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(out.bytes)
            .map_err(|_| invalid("no memory for the image"))?;
        buf.resize(out.bytes, 0);
        self.decode_slice(data, t, &mut buf)?;
        Ok(super::Picture {
            width: out.width,
            height: out.height,
            color: out.color,
            depth: out.depth,
            data: buf,
        })
    }

    pub fn decode_slice(&mut self, data: &[u8], t: Transform, buf: &mut [u8]) -> Result<Output> {
        let layout = self.prepare(data)?;
        let out = describe(&layout.info, layout.trns_present, t)?;
        if buf.len() < out.bytes {
            return Err(Error::Invalid(format!(
                "PNG: the buffer holds {} bytes, the image needs {}",
                buf.len(),
                out.bytes
            )));
        }
        let (w, h) = (out.width as usize, out.height as usize);
        let rows = std::mem::take(&mut self.inflated);
        let result = if layout.info.interlaced {
            self.interlaced_native(&layout, t, &rows, &out, buf)
        } else {
            let stride = out.row_bytes;
            self.rows(&layout, &rows, w, h, Mode::Native(t), &mut |y, row| {
                buf[y * stride..(y + 1) * stride].copy_from_slice(row);
            })
        };
        self.inflated = rows;
        result.map(|()| out)
    }

    fn interlaced_native(
        &mut self,
        layout: &Layout,
        t: Transform,
        data: &[u8],
        out: &Output,
        buf: &mut [u8],
    ) -> Result<()> {
        let bits = out.color.samples() * usize::from(out.depth);
        if bits < 8 {
            return Err(invalid(
                "an interlaced image below 8 bits a pixel needs the expanded transform",
            ));
        }
        let px = bits / 8;
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
            self.rows(layout, slice, x, y, Mode::Native(t), &mut |j, row| {
                let oy = j * YSPC[p] + YORIG[p];
                for (i, src) in row.chunks_exact(px).enumerate() {
                    let ox = i * XSPC[p] + XORIG[p];
                    let at = (oy * w + ox) * px;
                    buf[at..at + px].copy_from_slice(src);
                }
            })?;
            offset += len;
        }
        Ok(())
    }

    pub(super) fn native(
        &mut self,
        layout: &Layout,
        t: Transform,
        width: usize,
        out: &mut [u8],
    ) -> usize {
        let info = layout.info;
        let cur = &self.cur;
        let trns = layout.trns_present;
        if t == Transform::Identity {
            let n = layout.row_bytes(width);
            out[..n].copy_from_slice(&cur[..n]);
            return n;
        }
        let (color, _) = target(&info, trns, t);
        let wide = info.depth == 16;
        let keep = if wide && t == Transform::Expand { 2 } else { 1 };
        let samples = color.samples();
        let n = width * samples * keep;
        let sample = |i: usize| -> &[u8] {
            if wide {
                &cur[i * 2..i * 2 + keep]
            } else {
                &cur[i..i + 1]
            }
        };
        match info.color {
            Color::Palette | Color::Grey if info.depth < 8 => {
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
                    expand_palette(&layout.palette, &layout.alpha, trns, src, out);
                } else if trns {
                    let mask = (1u16 << info.depth) - 1;
                    let key = layout.trns.map(|t| (t[0] & mask) as u8 * scale);
                    for (px, &g) in out.chunks_exact_mut(2).zip(src) {
                        px.copy_from_slice(&[g, if key == Some(g) { 0 } else { 255 }]);
                    }
                } else {
                    out[..width].copy_from_slice(src);
                }
            }
            Color::Palette => {
                expand_palette(&layout.palette, &layout.alpha, trns, &cur[..width], out)
            }
            Color::Grey => {
                if !trns {
                    copy_samples(cur, out, width, wide, keep);
                } else {
                    let key = layout.trns.map(|t| t[0]);
                    for (i, px) in out.chunks_exact_mut(2 * keep).take(width).enumerate() {
                        let v = if wide {
                            u16::from_be_bytes([cur[i * 2], cur[i * 2 + 1]])
                        } else {
                            u16::from(cur[i])
                        };
                        px[..keep].copy_from_slice(sample(i));
                        let alpha = if key == Some(v) { 0 } else { 255 };
                        fill_alpha(&mut px[keep..], alpha);
                    }
                }
            }
            Color::GreyAlpha | Color::Rgba => copy_samples(cur, out, width * samples, wide, keep),
            Color::Rgb => {
                if !trns {
                    copy_samples(cur, out, width * 3, wide, keep);
                } else {
                    let key = layout.trns;
                    for (i, px) in out.chunks_exact_mut(4 * keep).take(width).enumerate() {
                        let get = |c: usize| -> u16 {
                            let k = i * 3 + c;
                            if wide {
                                u16::from_be_bytes([cur[k * 2], cur[k * 2 + 1]])
                            } else {
                                u16::from(cur[k])
                            }
                        };
                        for c in 0..3 {
                            px[c * keep..(c + 1) * keep].copy_from_slice(sample(i * 3 + c));
                        }
                        let alpha = if key == Some([get(0), get(1), get(2)]) {
                            0
                        } else {
                            255
                        };
                        fill_alpha(&mut px[3 * keep..], alpha);
                    }
                }
            }
        }
        n
    }
}

fn fill_alpha(dst: &mut [u8], alpha: u8) {
    dst.fill(alpha);
}

fn copy_samples(cur: &[u8], out: &mut [u8], count: usize, wide: bool, keep: usize) {
    if wide && keep == 1 {
        for (o, s) in out[..count].iter_mut().zip(cur.chunks_exact(2)) {
            *o = s[0];
        }
    } else {
        let n = count * keep;
        out[..n].copy_from_slice(&cur[..n]);
    }
}

fn expand_palette(palette: &[Entry; 256], alpha: &[u8], trns: bool, idx: &[u8], out: &mut [u8]) {
    if trns {
        for (px, &i) in out.chunks_exact_mut(4).zip(idx) {
            let e = palette[usize::from(i)].0;
            px.copy_from_slice(&[
                e[0],
                e[1],
                e[2],
                alpha.get(usize::from(i)).copied().unwrap_or(255),
            ]);
        }
    } else {
        for (px, &i) in out.chunks_exact_mut(3).zip(idx) {
            px.copy_from_slice(&palette[usize::from(i)].0[..3]);
        }
    }
}

impl Mode {
    pub(super) fn pixel_bytes(self, layout: &Layout) -> usize {
        match self {
            Mode::Rgba => 4,
            Mode::Native(t) => {
                let (color, depth) = target(&layout.info, layout.trns_present, t);
                (color.samples() * usize::from(depth)).div_ceil(8)
            }
        }
    }
}
