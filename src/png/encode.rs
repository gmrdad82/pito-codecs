use crate::media::{self, Error, Result};

use super::crc::crc;
use super::fastz::Fast;
use super::inflate::{adler32, adler32_combine};
use super::lz::{self, Lz};
use super::pack::{self, Isa};
use super::par;
use super::{Color, SIGNATURE};
use std::sync::atomic::{AtomicUsize, Ordering};

const IDAT_CHUNK: usize = 1 << 20;
const SEGMENT_BYTES: usize = 1 << 19;
const WINDOW: usize = 1 << 15;
const SEGMENT_OUTPUT: usize = 14_000;
const KEEP_TOKENS: usize = 1 << 19;
const KEEP_BUDGET: usize = 1 << 23;

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc(&[kind, data]).to_be_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    Fast,
    #[default]
    Balanced,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    pub compression: Compression,
    pub srgb: Option<u8>,
    pub gamma: Option<u32>,
    pub chromaticities: Option<[u32; 8]>,
    pub pixel_dims: Option<(u32, u32, u8)>,
    pub text: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub color: Color,
    pub depth: u8,
    pub data: Vec<u8>,
}

impl Picture {
    pub fn new(width: u32, height: u32, color: Color, depth: u8) -> Self {
        let bytes = Self::bytes_for(width, height, color, depth);
        Picture {
            width,
            height,
            color,
            depth,
            data: vec![0; bytes],
        }
    }

    fn bytes_for(width: u32, height: u32, color: Color, depth: u8) -> usize {
        (width as usize * color.samples() * usize::from(depth)).div_ceil(8) * height as usize
    }

    pub fn row_bytes(&self) -> usize {
        (self.width as usize * self.color.samples() * usize::from(self.depth)).div_ceil(8)
    }

    pub fn from_u16(width: u32, height: u32, color: Color, samples: &[u16]) -> Result<Self> {
        let want = width as usize * height as usize * color.samples();
        if samples.len() != want {
            return Err(Error::Invalid(format!(
                "PNG: {width}x{height} {color:?} needs {want} samples, got {}",
                samples.len()
            )));
        }
        Ok(Picture {
            width,
            height,
            color,
            depth: 16,
            data: samples.iter().flat_map(|s| s.to_be_bytes()).collect(),
        })
    }

    pub fn samples16(&self) -> Vec<u16> {
        match self.depth {
            16 => self
                .data
                .chunks_exact(2)
                .map(|s| u16::from_be_bytes([s[0], s[1]]))
                .collect(),
            _ => self.data.iter().map(|&b| u16::from(b) * 257).collect(),
        }
    }

    pub fn to_rgba(&self) -> Result<media::Rgba> {
        self.rgba(false)
    }

    pub fn to_rgba_rounded(&self) -> Result<media::Rgba> {
        self.rgba(true)
    }

    fn rgba(&self, round: bool) -> Result<media::Rgba> {
        if self.depth != 8 && self.depth != 16 || self.color == Color::Palette {
            return Err(Error::Invalid(
                "PNG: only 8 and 16 bit pictures convert".into(),
            ));
        }
        let step = usize::from(self.depth / 8);
        let n = self.color.samples();
        let mut out = media::Rgba::new(self.width, self.height);
        for (px, src) in out
            .data
            .chunks_exact_mut(4)
            .zip(self.data.chunks_exact(n * step))
        {
            let s = |i: usize| {
                if step == 2 && round {
                    let v = u32::from(u16::from_be_bytes([src[i * 2], src[i * 2 + 1]]));
                    ((v + 128) / 257) as u8
                } else {
                    src[i * step]
                }
            };
            let (r, g, b, a) = match self.color {
                Color::Grey => (s(0), s(0), s(0), 255),
                Color::GreyAlpha => (s(0), s(0), s(0), s(1)),
                Color::Rgb => (s(0), s(1), s(2), 255),
                _ => (s(0), s(1), s(2), s(3)),
            };
            px.copy_from_slice(&[r, g, b, a]);
        }
        Ok(out)
    }
}

pub fn encode(image: &media::Rgba) -> Result<Vec<u8>> {
    write(image, 3)
}

pub fn encode_rgba(image: &media::Rgba) -> Result<Vec<u8>> {
    write(image, 4)
}

fn write(image: &media::Rgba, bpp: usize) -> Result<Vec<u8>> {
    let (w, h) = (image.width as usize, image.height as usize);
    if image.data.len() < w * h * 4 {
        return Err(Error::Invalid("image data is shorter than its size".into()));
    }
    let color = if bpp == 4 { Color::Rgba } else { Color::Rgb };
    let fast = Options {
        compression: Compression::Fast,
        ..Options::default()
    };
    build(
        image.width,
        image.height,
        color,
        8,
        Source::Rgba {
            data: &image.data,
            width: w,
            bpp,
        },
        &fast,
        Plan::default(),
    )
}

pub fn encode_picture(picture: &Picture, options: &Options) -> Result<Vec<u8>> {
    if picture.color == Color::Palette {
        return Err(Error::Invalid(
            "PNG: palette pictures are not written".into(),
        ));
    }
    if picture.depth != 8 && picture.depth != 16 {
        return Err(Error::Invalid(format!(
            "PNG: bit depth {} is not written",
            picture.depth
        )));
    }
    let row = picture.row_bytes();
    let need = row
        .checked_mul(picture.height as usize)
        .ok_or_else(|| Error::Invalid("PNG: the picture is too large".into()))?;
    if picture.data.len() < need {
        return Err(Error::Invalid(format!(
            "PNG: the picture holds {} bytes, {}x{} needs {need}",
            picture.data.len(),
            picture.width,
            picture.height
        )));
    }
    build(
        picture.width,
        picture.height,
        picture.color,
        picture.depth,
        Source::Direct {
            data: &picture.data,
            row,
        },
        options,
        Plan::default(),
    )
}

enum Source<'a> {
    Direct {
        data: &'a [u8],
        row: usize,
    },
    Rgba {
        data: &'a [u8],
        width: usize,
        bpp: usize,
    },
}

impl Source<'_> {
    fn rows<'s>(&'s self, y0: usize, y1: usize, scratch: &'s mut Vec<u8>) -> &'s [u8] {
        match self {
            Source::Direct { data, row } => &data[y0 * row..y1 * row],
            Source::Rgba { data, width, bpp } => {
                let src = &data[y0 * width * 4..y1 * width * 4];
                if *bpp == 4 {
                    return src;
                }
                scratch.clear();
                scratch.resize(src.len() / 4 * bpp, 0);
                for (dst, px) in scratch.chunks_exact_mut(*bpp).zip(src.chunks_exact(4)) {
                    dst.copy_from_slice(&px[..*bpp]);
                }
                scratch
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Plan {
    pub segment: usize,
    pub threads: usize,
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            segment: SEGMENT_BYTES,
            threads: 0,
        }
    }
}

struct Worker {
    isa: Isa,
    trial: [Vec<u8>; 4],
    zeros: Vec<u8>,
    buf: Vec<u8>,
    scratch: Vec<u8>,
    lz: Option<Lz>,
}

struct Piece {
    bytes: Vec<u8>,
    adler: u32,
    raw: usize,
    tokens: Option<Vec<u32>>,
}

struct Shape {
    row: usize,
    bpp: usize,
    height: usize,
    rows_per: usize,
    count: usize,
    level: Compression,
    kept: AtomicUsize,
}

fn segment(w: &mut Worker, src: &Source, shape: &Shape, k: usize) -> Piece {
    let stride = shape.row + 1;
    let r0 = k * shape.rows_per;
    let r1 = (r0 + shape.rows_per).min(shape.height);
    let single = shape.count == 1;
    let dict_rows = if shape.level == Compression::Fast {
        0
    } else {
        WINDOW.div_ceil(stride).min(r0)
    };
    let first = r0 - dict_rows;
    let from = first.saturating_sub(1);
    let rows = src.rows(from, r1, &mut w.scratch);
    w.buf.clear();
    w.buf.resize((r1 - first) * stride, 0);
    for y in first..r1 {
        let cur = &rows[(y - from) * shape.row..][..shape.row];
        let prev = if y == 0 {
            &w.zeros[..shape.row]
        } else {
            &rows[(y - 1 - from) * shape.row..][..shape.row]
        };
        let out = &mut w.buf[(y - first) * stride..][..stride];
        if shape.level == Compression::Fast {
            out[0] = pack::PAETH;
            pack::filter(w.isa, pack::PAETH, cur, prev, shape.bpp, &mut out[1..]);
        } else {
            out[0] = pack::choose(w.isa, cur, prev, shape.bpp, &mut w.trial, &mut out[1..]);
        }
    }
    let own = dict_rows * stride;
    let raw = w.buf.len() - own;
    let adler = adler32(&w.buf[own..]);
    let mut tokens = None;
    let bytes = match shape.level {
        Compression::Fast => {
            let mut z = Fast::new(raw / 3 + 64, single);
            z.data(&w.buf[own..]);
            z.finish(single)
        }
        level => {
            let params = if level == Compression::High {
                lz::HIGH
            } else {
                lz::BALANCED
            };
            let mut out = Vec::with_capacity(raw / 2 + 64);
            let lz = w.lz.get_or_insert_with(Lz::new);
            lz.segment(&w.buf, own, w.buf.len(), &params, single, &mut out);
            let n = lz.tokens().len();
            if !single && n <= KEEP_TOKENS {
                if shape.kept.fetch_add(n, Ordering::Relaxed) + n <= KEEP_BUDGET {
                    tokens = Some(lz.tokens().to_vec());
                } else {
                    shape.kept.fetch_sub(n, Ordering::Relaxed);
                }
            }
            out
        }
    };
    Piece {
        bytes,
        adler,
        raw,
        tokens,
    }
}

struct Idat {
    out: Vec<u8>,
    chunks: Vec<(usize, usize)>,
    start: usize,
    len: usize,
}

impl Idat {
    fn push(&mut self, mut piece: &[u8]) {
        while !piece.is_empty() {
            if self.len == 0 {
                self.out.extend_from_slice(&[0; 4]);
                self.out.extend_from_slice(b"IDAT");
                self.start = self.out.len();
            }
            let take = piece.len().min(IDAT_CHUNK - self.len);
            self.out.extend_from_slice(&piece[..take]);
            self.len += take;
            piece = &piece[take..];
            if self.len == IDAT_CHUNK {
                self.close();
            }
        }
    }

    fn close(&mut self) {
        if self.len == 0 {
            return;
        }
        self.out[self.start - 8..self.start - 4].copy_from_slice(&(self.len as u32).to_be_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.chunks.push((self.start, self.len));
        self.len = 0;
    }

    fn seal(mut self) -> Vec<u8> {
        self.close();
        let sums = par::map(
            self.chunks.len(),
            par::workers(self.chunks.len() / 4),
            || (),
            |_, i| {
                let (at, len) = self.chunks[i];
                crc(&[b"IDAT", &self.out[at..at + len]])
            },
        );
        for (&(at, len), sum) in self.chunks.iter().zip(sums) {
            self.out[at + len..at + len + 4].copy_from_slice(&sum.to_be_bytes());
        }
        self.out
    }
}

fn build(
    width: u32,
    height: u32,
    color: Color,
    depth: u8,
    source: Source,
    options: &Options,
    plan: Plan,
) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w > 1 << 24 || h > 1 << 24 {
        return Err(Error::Invalid(format!("cannot write a {w}x{h} PNG")));
    }
    let bpp = (color.samples() * usize::from(depth) / 8).max(1);
    let row = w * color.samples() * usize::from(depth) / 8;
    let total = (row + 1)
        .checked_mul(h)
        .ok_or_else(|| Error::Invalid("PNG: the picture is too large".into()))?;
    let shape_for = |count: usize| Shape {
        row,
        bpp,
        height: h,
        rows_per: h.div_ceil(count),
        count: h.div_ceil(h.div_ceil(count)),
        level: options.compression,
        kept: AtomicUsize::new(0),
    };
    let run = |shape: &Shape| {
        let threads = match plan.threads {
            0 => par::workers(shape.count),
            n => n.min(shape.count),
        };
        par::map(
            shape.count,
            threads,
            || Worker {
                isa: Isa::detect(),
                trial: pack::trials(row),
                zeros: vec![0; row],
                buf: Vec::new(),
                scratch: Vec::new(),
                lz: None,
            },
            |worker, k| segment(worker, &source, shape, k),
        )
    };
    let mut shape = shape_for((total / plan.segment.max(1)).clamp(1, h));
    let mut pieces = run(&shape);
    let mut whole = shape.count == 1;
    if shape.count > 1 && options.compression == Compression::Fast {
        let produced: usize = pieces.iter().map(|p| p.bytes.len()).sum();
        let fewer = (produced / SEGMENT_OUTPUT).max(1);
        if fewer * 4 < shape.count * 3 {
            shape = shape_for(fewer);
            pieces = run(&shape);
            whole = shape.count == 1;
        }
    }
    if shape.count > 1 && pieces.iter().all(|p| p.tokens.is_some()) {
        let all: Vec<u32> = pieces
            .iter()
            .flat_map(|p| p.tokens.iter().flatten().copied())
            .collect();
        let mut body = Vec::with_capacity(pieces.iter().map(|p| p.bytes.len()).sum());
        Lz::new().merged(&all, &mut body);
        let adler = pieces
            .iter()
            .skip(1)
            .fold(pieces[0].adler, |a, p| adler32_combine(a, p.adler, p.raw));
        let raw = pieces.iter().map(|p| p.raw).sum();
        pieces = vec![Piece {
            bytes: body,
            adler,
            raw,
            tokens: None,
        }];
        whole = true;
    }
    let adler = pieces
        .iter()
        .skip(1)
        .fold(pieces[0].adler, |a, p| adler32_combine(a, p.adler, p.raw));
    let zlib: [u8; 2] = if options.compression == Compression::Fast {
        [0x78, 0x01]
    } else {
        [0x78, 0x9c]
    };
    let z_len: usize = 2 + pieces.iter().map(|p| p.bytes.len()).sum::<usize>() + 9;
    let mut idat = Idat {
        out: Vec::with_capacity(z_len + 256 + z_len / IDAT_CHUNK * 12),
        chunks: Vec::new(),
        start: 0,
        len: 0,
    };
    idat.out.extend_from_slice(&SIGNATURE);
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = depth;
    ihdr[9] = match color {
        Color::Grey => 0,
        Color::Rgb => 2,
        Color::Palette => 3,
        Color::GreyAlpha => 4,
        Color::Rgba => 6,
    };
    chunk(&mut idat.out, b"IHDR", &ihdr);
    if let Some(c) = options.chromaticities {
        let body: Vec<u8> = c.iter().flat_map(|v| v.to_be_bytes()).collect();
        chunk(&mut idat.out, b"cHRM", &body);
    }
    if let Some(g) = options.gamma {
        chunk(&mut idat.out, b"gAMA", &g.to_be_bytes());
    }
    if let Some(i) = options.srgb {
        chunk(&mut idat.out, b"sRGB", &[i]);
    }
    if let Some((x, y, unit)) = options.pixel_dims {
        let mut body = Vec::with_capacity(9);
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&y.to_be_bytes());
        body.push(unit);
        chunk(&mut idat.out, b"pHYs", &body);
    }
    for (key, value) in &options.text {
        text_chunk(&mut idat.out, key, value)?;
    }
    idat.push(&zlib);
    for p in &pieces {
        idat.push(&p.bytes);
    }
    if !whole {
        idat.push(&[1, 0, 0, 0xff, 0xff]);
    }
    idat.push(&adler.to_be_bytes());
    let mut out = idat.seal();
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn text_chunk(out: &mut Vec<u8>, key: &str, value: &str) -> Result<()> {
    if key.is_empty() || key.len() > 79 || !key.chars().all(|c| (' '..='~').contains(&c)) {
        return Err(Error::Invalid(format!(
            "PNG: {key:?} is not a text keyword"
        )));
    }
    let mut body = Vec::new();
    body.extend_from_slice(key.as_bytes());
    body.push(0);
    if value.chars().all(|c| c as u32 <= 0xff && c != '\0') {
        body.extend(value.chars().map(|c| c as u8));
        chunk(out, b"tEXt", &body);
    } else {
        body.extend_from_slice(&[0, 0, 0, 0]);
        body.extend_from_slice(value.as_bytes());
        chunk(out, b"iTXt", &body);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn encode_with(picture: &Picture, options: &Options, plan: Plan) -> Result<Vec<u8>> {
    let row = picture.row_bytes();
    build(
        picture.width,
        picture.height,
        picture.color,
        picture.depth,
        Source::Direct {
            data: &picture.data,
            row,
        },
        options,
        plan,
    )
}
