use crate::media::{self, Error, Result};

use super::SIGNATURE;
use super::inflate::adler32;

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

const CRC: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

fn crc(parts: &[&[u8]]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for part in parts {
        for &b in *part {
            c = CRC[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xffff_ffff
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

pub fn encode(image: &media::Rgba) -> Result<Vec<u8>> {
    let (w, h) = (image.width as usize, image.height as usize);
    let row = w * 3;
    if w == 0 || h == 0 || w > 1 << 24 || h > 1 << 24 {
        return Err(Error::Invalid(format!("cannot write a {w}x{h} PNG")));
    }
    if image.data.len() < w * h * 4 {
        return Err(Error::Invalid("image data is shorter than its size".into()));
    }
    let mut raw = vec![0u8; (row + 1) * h];
    let mut prev = vec![0u8; row];
    let mut cur = vec![0u8; row];
    for (y, line) in image.data.chunks_exact(w * 4).take(h).enumerate() {
        for (dst, px) in cur.chunks_exact_mut(3).zip(line.chunks_exact(4)) {
            dst.copy_from_slice(&px[..3]);
        }
        let out = &mut raw[y * (row + 1)..(y + 1) * (row + 1)];
        out[0] = 4;
        for i in 0..row {
            let a = if i >= 3 { cur[i - 3] } else { 0 };
            let c = if i >= 3 { prev[i - 3] } else { 0 };
            out[i + 1] = cur[i].wrapping_sub(paeth(a, prev[i], c));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut z = Deflate::new(raw.len() / 3);
    z.data(&raw);
    let z = z.finish(adler32(&raw));
    let mut out = Vec::with_capacity(z.len() + 64 + z.len() / IDAT_CHUNK * 12);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&(w as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(h as u32).to_be_bytes());
    ihdr[8] = 8;
    ihdr[9] = 2;
    chunk(&mut out, b"IHDR", &ihdr);
    for part in z.chunks(IDAT_CHUNK) {
        chunk(&mut out, b"IDAT", part);
    }
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}
