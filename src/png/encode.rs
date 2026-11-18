use crate::media::{self, Error, Result};

use super::crc::crc;
use super::inflate::adler32;
use super::{Color, SIGNATURE};

const LENGTHS: [u8; 286] = [
    2, 3, 4, 5, 5, 6, 6, 7, 7, 7, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 11, 11, 11, 11, 11, 11, 11,
    11, 11, 11, 10, 11, 10, 10, 10, 10, 10, 10, 10, 10, 10, 9, 9, 9, 9, 9, 8, 9, 8, 8, 8, 8, 8, 7,
    7, 7, 6, 6, 6, 5, 4, 3, 12, 12, 12, 9, 9, 11, 10, 11, 11, 10, 11, 11, 11, 11, 11, 11, 12, 11,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 9,
];

const CODES: [u16; 286] = codes(&LENGTHS);

const LENGTH_TO_SYMBOL: [u16; 256] = [
    257, 258, 259, 260, 261, 262, 263, 264, 265, 265, 266, 266, 267, 267, 268, 268, 269, 269, 269,
    269, 270, 270, 270, 270, 271, 271, 271, 271, 272, 272, 272, 272, 273, 273, 273, 273, 273, 273,
    273, 273, 274, 274, 274, 274, 274, 274, 274, 274, 275, 275, 275, 275, 275, 275, 275, 275, 276,
    276, 276, 276, 276, 276, 276, 276, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277,
    277, 277, 277, 277, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278,
    278, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 280, 280,
    280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 281, 281, 281, 281, 281,
    281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281,
    281, 281, 281, 281, 281, 281, 281, 281, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282,
    282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282,
    282, 282, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283,
    283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 284, 284, 284, 284,
    284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284,
    284, 284, 284, 284, 284, 284, 284, 284, 285,
];

const LENGTH_TO_LEN_EXTRA: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0,
];

const HEADER: [u8; 54] = [
    120, 1, 237, 192, 3, 160, 36, 89, 150, 198, 241, 255, 119, 238, 141, 200, 204, 167, 114, 75,
    99, 174, 109, 219, 182, 109, 219, 182, 109, 219, 182, 109, 105, 140, 158, 150, 74, 175, 158,
    50, 51, 34, 238, 249, 118, 183, 106, 122, 166, 135, 59, 107, 213, 15,
];

const IDAT_CHUNK: usize = 1 << 20;

const fn codes(lengths: &[u8; 286]) -> [u16; 286] {
    let mut codes = [0u16; 286];
    let mut code = 0u32;
    let mut len = 1;
    while len <= 16 {
        let mut i = 0;
        while i < lengths.len() {
            if lengths[i] == len {
                codes[i] = (code as u16).reverse_bits() >> (16 - len);
                code += 1;
            }
            i += 1;
        }
        code <<= 1;
        len += 1;
    }
    codes
}

struct Deflate {
    out: Vec<u8>,
    buffer: u64,
    nbits: u8,
}

impl Deflate {
    fn new(capacity: usize) -> Self {
        let mut d = Deflate {
            out: Vec::with_capacity(capacity),
            buffer: 0,
            nbits: 0,
        };
        d.out.extend_from_slice(&HEADER[..53]);
        d.bits(HEADER[53] as u64, 5);
        d
    }

    fn bits(&mut self, bits: u64, nbits: u8) {
        self.buffer |= bits << self.nbits;
        self.nbits += nbits;
        if self.nbits >= 64 {
            self.out.extend_from_slice(&self.buffer.to_le_bytes());
            self.nbits -= 64;
            self.buffer = bits.checked_shr((nbits - self.nbits) as u32).unwrap_or(0);
        }
    }

    fn literal(&mut self, b: u8) {
        self.bits(CODES[b as usize] as u64, LENGTHS[b as usize]);
    }

    fn run(&mut self, mut run: u32) {
        self.bits(CODES[0] as u64, LENGTHS[0]);
        run -= 1;
        while run >= 258 {
            self.bits(CODES[285] as u64, LENGTHS[285] + 1);
            run -= 258;
        }
        if run > 4 {
            let sym = LENGTH_TO_SYMBOL[run as usize - 3] as usize;
            self.bits(CODES[sym] as u64, LENGTHS[sym]);
            let extra_len = LENGTH_TO_LEN_EXTRA[run as usize - 3];
            let extra = ((run - 3) & ((1u32 << extra_len) - 1)) as u64;
            self.bits(extra, extra_len + 1);
        } else {
            self.bits(0, run as u8 * LENGTHS[0]);
        }
    }

    fn data(&mut self, data: &[u8]) {
        let mut run = 0;
        let mut chunks = data.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            let ichunk = u64::from_le_bytes(word);
            if ichunk == 0 {
                run += 8;
                continue;
            } else if run > 0 {
                let run_extra = ichunk.trailing_zeros() / 8;
                self.run(run + run_extra);
                run = 0;
                if run_extra > 0 {
                    run = ichunk.leading_zeros() / 8;
                    for &b in &word[run_extra as usize..8 - run as usize] {
                        self.literal(b);
                    }
                    continue;
                }
            }
            let run_start = ichunk.leading_zeros() / 8;
            if run_start > 0 {
                for &b in &word[..8 - run_start as usize] {
                    self.literal(b);
                }
                run = run_start;
                continue;
            }
            let [c0, c1, c2, c3, c4, c5, c6, c7] = word.map(|b| b as usize);
            let (n0, n1, n2, n3) = (LENGTHS[c0], LENGTHS[c1], LENGTHS[c2], LENGTHS[c3]);
            let bits = CODES[c0] as u64
                | ((CODES[c1] as u64) << n0)
                | ((CODES[c2] as u64) << (n0 + n1))
                | ((CODES[c3] as u64) << (n0 + n1 + n2));
            self.bits(bits, n0 + n1 + n2 + n3);
            let (n4, n5, n6, n7) = (LENGTHS[c4], LENGTHS[c5], LENGTHS[c6], LENGTHS[c7]);
            let bits = CODES[c4] as u64
                | ((CODES[c5] as u64) << n4)
                | ((CODES[c6] as u64) << (n4 + n5))
                | ((CODES[c7] as u64) << (n4 + n5 + n6));
            self.bits(bits, n4 + n5 + n6 + n7);
        }
        if run > 0 {
            self.run(run);
        }
        for &b in chunks.remainder() {
            self.literal(b);
        }
    }

    fn finish(mut self, checksum: u32) -> Vec<u8> {
        self.bits(CODES[256] as u64, LENGTHS[256]);
        if !self.nbits.is_multiple_of(8) {
            self.bits(0, 8 - self.nbits % 8);
        }
        if self.nbits > 0 {
            let bytes = self.buffer.to_le_bytes();
            self.out
                .extend_from_slice(&bytes[..self.nbits as usize / 8]);
        }
        self.out.extend_from_slice(&checksum.to_be_bytes());
        self.out
    }
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

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
        &|y, cur| {
            let line = &image.data[y * w * 4..(y + 1) * w * 4];
            for (dst, px) in cur.chunks_exact_mut(bpp).zip(line.chunks_exact(4)) {
                dst.copy_from_slice(&px[..bpp]);
            }
        },
        &fast,
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
        &|y, cur| cur.copy_from_slice(&picture.data[y * row..(y + 1) * row]),
        options,
    )
}

fn build(
    width: u32,
    height: u32,
    color: Color,
    depth: u8,
    fill: &dyn Fn(usize, &mut [u8]),
    options: &Options,
) -> Result<Vec<u8>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || w > 1 << 24 || h > 1 << 24 {
        return Err(Error::Invalid(format!("cannot write a {w}x{h} PNG")));
    }
    let bpp = (color.samples() * usize::from(depth) / 8).max(1);
    let row = w * color.samples() * usize::from(depth) / 8;
    let mut raw = Vec::new();
    raw.try_reserve_exact((row + 1) * h)
        .map_err(|_| Error::Invalid("PNG: no memory for the picture".into()))?;
    raw.resize((row + 1) * h, 0);
    let mut prev = vec![0u8; row];
    let mut cur = vec![0u8; row];
    let mut trial = vec![0u8; row];
    for y in 0..h {
        fill(y, &mut cur);
        let out = &mut raw[y * (row + 1)..(y + 1) * (row + 1)];
        if options.compression == Compression::Fast {
            out[0] = 4;
            filter_row(4, &cur, &prev, bpp, &mut out[1..]);
        } else {
            let mut best = (u64::MAX, 0u8);
            for kind in 0..5u8 {
                filter_row(kind, &cur, &prev, bpp, &mut trial);
                let cost: u64 = trial
                    .iter()
                    .map(|&b| u64::from((b as i8).unsigned_abs()))
                    .sum();
                if cost < best.0 {
                    best = (cost, kind);
                }
            }
            out[0] = best.1;
            filter_row(best.1, &cur, &prev, bpp, &mut out[1..]);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let z = match options.compression {
        Compression::Fast => {
            let mut z = Deflate::new(raw.len() / 3);
            z.data(&raw);
            z.finish(adler32(&raw))
        }
        Compression::Balanced => fixed::zlib(&raw, 12),
        Compression::High => fixed::zlib(&raw, 96),
    };
    let mut out = Vec::with_capacity(z.len() + 64 + z.len() / IDAT_CHUNK * 12);
    out.extend_from_slice(&SIGNATURE);
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
    chunk(&mut out, b"IHDR", &ihdr);
    if let Some(c) = options.chromaticities {
        let body: Vec<u8> = c.iter().flat_map(|v| v.to_be_bytes()).collect();
        chunk(&mut out, b"cHRM", &body);
    }
    if let Some(g) = options.gamma {
        chunk(&mut out, b"gAMA", &g.to_be_bytes());
    }
    if let Some(i) = options.srgb {
        chunk(&mut out, b"sRGB", &[i]);
    }
    if let Some((x, y, unit)) = options.pixel_dims {
        let mut body = Vec::with_capacity(9);
        body.extend_from_slice(&x.to_be_bytes());
        body.extend_from_slice(&y.to_be_bytes());
        body.push(unit);
        chunk(&mut out, b"pHYs", &body);
    }
    for (key, value) in &options.text {
        text_chunk(&mut out, key, value)?;
    }
    for part in z.chunks(IDAT_CHUNK) {
        chunk(&mut out, b"IDAT", part);
    }
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

fn filter_row(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    for i in 0..cur.len() {
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let p = match kind {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
            _ => paeth(a, b, c),
        };
        out[i] = cur[i].wrapping_sub(p);
    }
}

mod fixed {
    use super::adler32;

    const MIN: usize = 3;
    const MAX: usize = 258;
    const WINDOW: usize = 32768;
    const HASH: usize = 1 << 15;

    struct Bits {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl Bits {
        fn put(&mut self, v: u32, n: u32) {
            self.acc |= u64::from(v) << self.n;
            self.n += n;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }

        fn code(&mut self, code: u32, n: u32) {
            self.put(code.reverse_bits() >> (32 - n), n);
        }

        fn literal(&mut self, sym: u32) {
            match sym {
                0..=143 => self.code(0x30 + sym, 8),
                144..=255 => self.code(0x190 + sym - 144, 9),
                256..=279 => self.code(sym - 256, 7),
                _ => self.code(0xC0 + sym - 280, 8),
            }
        }
    }

    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    fn hash(d: &[u8], i: usize) -> usize {
        let v = u32::from(d[i]) | u32::from(d[i + 1]) << 8 | u32::from(d[i + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> 17) as usize & (HASH - 1)
    }

    fn matched(d: &[u8], from: usize, at: usize) -> usize {
        let limit = (d.len() - at).min(MAX);
        let mut n = 0;
        while n < limit && d[from + n] == d[at + n] {
            n += 1;
        }
        n
    }

    pub fn zlib(data: &[u8], chain: usize) -> Vec<u8> {
        let mut b = Bits {
            out: Vec::with_capacity(data.len() / 2 + 16),
            acc: 0,
            n: 0,
        };
        b.out.extend_from_slice(&[0x78, 0x9C]);
        b.put(1, 1);
        b.put(1, 2);
        let mut head = vec![usize::MAX; HASH];
        let mut prev = vec![usize::MAX; data.len()];
        let mut i = 0;
        while i < data.len() {
            let mut best = (0usize, 0usize);
            if i + MIN <= data.len() {
                let h = hash(data, i);
                let mut cand = head[h];
                let mut steps = chain;
                while cand != usize::MAX && i - cand <= WINDOW && steps > 0 {
                    let n = matched(data, cand, i);
                    if n > best.0 {
                        best = (n, i - cand);
                        if n == MAX {
                            break;
                        }
                    }
                    cand = prev[cand];
                    steps -= 1;
                }
                prev[i] = head[h];
                head[h] = i;
            }
            if best.0 >= MIN {
                let (len, dist) = best;
                let li = LEN_BASE
                    .iter()
                    .rposition(|&v| usize::from(v) <= len)
                    .unwrap_or(0);
                b.literal(257 + li as u32);
                b.put(
                    (len - usize::from(LEN_BASE[li])) as u32,
                    u32::from(LEN_EXTRA[li]),
                );
                let di = DIST_BASE
                    .iter()
                    .rposition(|&v| usize::from(v) <= dist)
                    .unwrap_or(0);
                b.code(di as u32, 5);
                b.put(
                    (dist - usize::from(DIST_BASE[di])) as u32,
                    u32::from(DIST_EXTRA[di]),
                );
                for k in 1..len {
                    let at = i + k;
                    if at + MIN <= data.len() {
                        let h = hash(data, at);
                        prev[at] = head[h];
                        head[h] = at;
                    }
                }
                i += len;
            } else {
                b.literal(u32::from(data[i]));
                i += 1;
            }
        }
        b.literal(256);
        if b.n > 0 {
            b.put(0, 8 - b.n);
        }
        b.out.extend_from_slice(&adler32(data).to_be_bytes());
        b.out
    }
}
