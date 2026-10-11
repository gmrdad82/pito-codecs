use super::bits::Bits;
use super::huffman::{self, Code};
use super::transform::{self, Transform, add_pixels};
use crate::media::Result;
use crate::webp::riff::invalid;

const NUM_LENGTH: usize = 24;
const NUM_DIST: usize = 40;

#[derive(Clone, Copy)]
struct Group {
    codes: [Code; 5],
    literal: Option<u32>,
}

struct Codes {
    table: Vec<u32>,
    groups: Vec<Group>,
    meta: Vec<u32>,
    meta_bits: u32,
    meta_width: usize,
}

pub(super) struct Decoder<'a> {
    bits: Bits<'a>,
    lengths: Vec<u8>,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Decoder {
            bits: Bits::new(data),
            lengths: Vec::new(),
        }
    }

    pub fn header(&mut self) -> Result<(u32, u32, bool)> {
        if self.bits.read(8) != 0x2f {
            return Err(invalid("VP8L signature"));
        }
        let w = self.bits.read(14) + 1;
        let h = self.bits.read(14) + 1;
        let alpha = self.bits.bit();
        if self.bits.read(3) != 0 {
            return Err(invalid("VP8L version"));
        }
        Ok((w, h, alpha))
    }

    pub fn image(&mut self, width: usize, height: usize) -> Result<Vec<u32>> {
        let mut transforms = Vec::new();
        let mut xsize = width;
        while self.bits.bit() {
            let t = self.transform(xsize, height)?;
            if transforms.iter().any(|o: &Transform| o.kind() == t.kind()) {
                return Err(invalid("VP8L transform repeated"));
            }
            if let Transform::Index { bits, .. } = &t {
                xsize = xsize.div_ceil(1 << bits);
            }
            transforms.push(t);
        }
        let mut data = vec![0u32; xsize * height];
        self.stream(xsize, height, true, &mut data)?;
        if self.bits.overrun() {
            return Err(invalid("VP8L data truncated"));
        }
        if transforms.is_empty() {
            return Ok(data);
        }
        transform::inverse(&transforms, width, height, data)
    }

    fn transform(&mut self, xsize: usize, height: usize) -> Result<Transform> {
        Ok(match self.bits.read(2) {
            0 => {
                let bits = self.bits.read(3) + 2;
                let data = self.sub_image(xsize.div_ceil(1 << bits), height.div_ceil(1 << bits))?;
                Transform::Predict { bits, data }
            }
            1 => {
                let bits = self.bits.read(3) + 2;
                let data = self.sub_image(xsize.div_ceil(1 << bits), height.div_ceil(1 << bits))?;
                Transform::Color { bits, data }
            }
            2 => Transform::SubtractGreen,
            _ => {
                let size = self.bits.read(8) as usize + 1;
                let mut palette = self.sub_image(size, 1)?;
                for i in 1..size {
                    palette[i] = add_pixels(palette[i], palette[i - 1]);
                }
                let bits = match size {
                    0..=2 => 3,
                    3..=4 => 2,
                    5..=16 => 1,
                    _ => 0,
                };
                palette.resize(256, 0);
                Transform::Index {
                    bits,
                    palette,
                    width: xsize,
                }
            }
        })
    }

    fn sub_image(&mut self, width: usize, height: usize) -> Result<Vec<u32>> {
        let mut data = vec![0u32; width * height];
        self.stream(width, height, false, &mut data)?;
        Ok(data)
    }

    fn stream(&mut self, width: usize, height: usize, top: bool, out: &mut [u32]) -> Result<()> {
        let cache_bits = if self.bits.bit() {
            let b = self.bits.read(4);
            if !(1..=11).contains(&b) {
                return Err(invalid("VP8L color cache size"));
            }
            b
        } else {
            0
        };
        let codes = self.codes(width, height, top, cache_bits)?;
        self.pixels(width, height, &codes, cache_bits, out)
    }

    fn codes(&mut self, width: usize, height: usize, top: bool, cache_bits: u32) -> Result<Codes> {
        let mut meta = Vec::new();
        let mut meta_bits = 0;
        let mut meta_width = 0;
        let mut count = 1;
        if top && self.bits.bit() {
            meta_bits = self.bits.read(3) + 2;
            meta_width = width.div_ceil(1 << meta_bits);
            let mh = height.div_ceil(1 << meta_bits);
            meta = self.sub_image(meta_width, mh)?;
            for m in meta.iter_mut() {
                *m = (*m >> 8) & 0xffff;
                count = count.max(*m as usize + 1);
            }
        }
        let cache = if cache_bits > 0 { 1 << cache_bits } else { 0 };
        let sizes = [256 + NUM_LENGTH + cache, 256, 256, 256, NUM_DIST];
        let mut table = Vec::with_capacity(count * 5 * 300);
        let mut groups = Vec::with_capacity(count);
        for _ in 0..count {
            let mut codes = [Code {
                start: 0,
                single: None,
            }; 5];
            for (c, &size) in codes.iter_mut().zip(&sizes) {
                *c = huffman::read_code(&mut self.bits, size, &mut self.lengths, &mut table)?;
            }
            let literal = match (codes[1].single, codes[2].single, codes[3].single) {
                (Some(r), Some(b), Some(a)) => Some((a as u32) << 24 | (r as u32) << 16 | b as u32),
                _ => None,
            };
            groups.push(Group { codes, literal });
            if self.bits.overrun() {
                return Err(invalid("VP8L data truncated"));
            }
        }
        Ok(Codes {
            table,
            groups,
            meta,
            meta_bits,
            meta_width,
        })
    }

    fn pixels(
        &mut self,
        width: usize,
        height: usize,
        codes: &Codes,
        cache_bits: u32,
        out: &mut [u32],
    ) -> Result<()> {
        let bits = &mut self.bits;
        let table = &codes.table[..];
        let total = width * height;
        let mut cache = vec![0u32; if cache_bits > 0 { 1 << cache_bits } else { 0 }];
        let cache_shift = 32 - cache_bits;
        let mut last_cached = 0usize;
        let mask = if codes.meta.is_empty() {
            usize::MAX
        } else {
            (1usize << codes.meta_bits) - 1
        };
        let group_at = |x: usize, y: usize| -> Group {
            if codes.meta.is_empty() {
                codes.groups[0]
            } else {
                let i = (y >> codes.meta_bits) * codes.meta_width + (x >> codes.meta_bits);
                codes.groups[codes.meta[i] as usize]
            }
        };
        let mut x = 0usize;
        let mut y = 0usize;
        let mut pos = 0usize;
        let mut g = group_at(0, 0);
        while pos < total {
            if x & mask == 0 {
                g = group_at(x, y);
            }
            bits.refill();
            let s = huffman::read(table, g.codes[0], bits);
            if s < 256 {
                let argb = if let Some(lit) = g.literal {
                    lit | s << 8
                } else {
                    let r = huffman::read(table, g.codes[1], bits);
                    let b = huffman::read(table, g.codes[2], bits);
                    bits.refill();
                    let a = huffman::read(table, g.codes[3], bits);
                    a << 24 | r << 16 | s << 8 | b
                };
                out[pos] = argb;
                pos += 1;
                x += 1;
                if x == width {
                    x = 0;
                    y += 1;
                    if bits.overrun() {
                        return Err(invalid("VP8L data truncated"));
                    }
                }
            } else if s < 256 + NUM_LENGTH as u32 {
                let len = prefix_value(s - 256, bits) as usize;
                bits.refill();
                let dsym = huffman::read(table, g.codes[4], bits);
                let dcode = prefix_value(dsym, bits) as usize;
                let dist = plane_distance(dcode, width);
                if dist > pos || len > total - pos {
                    return Err(invalid("VP8L back reference out of range"));
                }
                copy(out, pos, dist, len);
                pos += len;
                x += len;
                while x >= width {
                    x -= width;
                    y += 1;
                }
                if bits.overrun() {
                    return Err(invalid("VP8L data truncated"));
                }
                if pos < total && !codes.meta.is_empty() {
                    g = group_at(x, y);
                }
            } else {
                let i = (s - 256 - NUM_LENGTH as u32) as usize;
                if i >= cache.len() {
                    return Err(invalid("VP8L color cache index"));
                }
                while last_cached < pos {
                    let p = out[last_cached];
                    cache[(p.wrapping_mul(0x1e35a7bd) >> cache_shift) as usize] = p;
                    last_cached += 1;
                }
                out[pos] = cache[i];
                pos += 1;
                x += 1;
                if x == width {
                    x = 0;
                    y += 1;
                    if bits.overrun() {
                        return Err(invalid("VP8L data truncated"));
                    }
                }
            }
        }
        if bits.overrun() {
            return Err(invalid("VP8L data truncated"));
        }
        Ok(())
    }
}

#[inline(always)]
fn prefix_value(prefix: u32, bits: &mut Bits) -> u32 {
    if prefix < 4 {
        return prefix + 1;
    }
    let extra = (prefix - 2) >> 1;
    let offset = (2 + (prefix & 1)) << extra;
    offset + bits.read(extra) + 1
}

pub(super) const PLANE: [(i8, i8); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

#[inline(always)]
fn plane_distance(code: usize, width: usize) -> usize {
    if code > 120 {
        return code - 120;
    }
    let (xi, yi) = PLANE[code - 1];
    let d = xi as isize + yi as isize * width as isize;
    d.max(1) as usize
}

#[inline(always)]
fn copy(out: &mut [u32], pos: usize, dist: usize, len: usize) {
    if dist >= len {
        out.copy_within(pos - dist..pos - dist + len, pos);
    } else if dist == 1 {
        let v = out[pos - 1];
        out[pos..pos + len].fill(v);
    } else {
        for i in pos..pos + len {
            out[i] = out[i - dist];
        }
    }
}
