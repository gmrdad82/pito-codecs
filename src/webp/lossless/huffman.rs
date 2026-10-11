use super::bits::Bits;
use crate::media::Result;
use crate::webp::riff::invalid;

pub(super) const MAX_LEN: usize = 15;
pub(super) const ROOT: u32 = 8;

#[derive(Clone, Copy)]
pub(super) struct Code {
    pub start: u32,
    pub single: Option<u16>,
}

pub(super) fn build(lengths: &[u8], table: &mut Vec<u32>) -> Result<Code> {
    let start = table.len() as u32;
    let mut count = [0u16; MAX_LEN + 1];
    let mut last = 0;
    let mut used = 0;
    for (s, &l) in lengths.iter().enumerate() {
        if l != 0 {
            count[l as usize] += 1;
            last = s;
            used += 1;
        }
    }
    let root_size = 1usize << ROOT;
    if used == 0 {
        return Err(invalid("VP8L prefix code without symbols"));
    }
    if used == 1 {
        table.extend(std::iter::repeat_n((last as u32) << 8, root_size));
        return Ok(Code {
            start,
            single: Some(last as u16),
        });
    }
    let mut offset = [0u16; MAX_LEN + 2];
    for l in 1..=MAX_LEN {
        offset[l + 1] = offset[l] + count[l];
    }
    let mut sorted = [0u16; 2328 + 16];
    for (s, &l) in lengths.iter().enumerate() {
        if l != 0 {
            sorted[offset[l as usize] as usize] = s as u16;
            offset[l as usize] += 1;
        }
    }
    let base = table.len();
    table.resize(base + root_size, 0);
    let mut key = 0usize;
    let mut sym = 0usize;
    let mut num_open: i32 = 1;
    let mut step = 2usize;
    for (len, &n) in count.iter().enumerate().take(ROOT as usize + 1).skip(1) {
        num_open <<= 1;
        num_open -= n as i32;
        if num_open < 0 {
            return Err(invalid("VP8L prefix code over-subscribed"));
        }
        for _ in 0..n {
            let e = (sorted[sym] as u32) << 8 | len as u32;
            sym += 1;
            let mut k = key;
            while k < root_size {
                table[base + k] = e;
                k += step;
            }
            key = next_key(key, len);
        }
        step <<= 1;
    }
    let mask = root_size - 1;
    let mut low = usize::MAX;
    let mut sub = base + root_size;
    let mut sub_size = 0usize;
    let mut cnt = count;
    step = 2;
    for len in ROOT as usize + 1..=MAX_LEN {
        num_open <<= 1;
        num_open -= count[len] as i32;
        if num_open < 0 {
            return Err(invalid("VP8L prefix code over-subscribed"));
        }
        while cnt[len] > 0 {
            if key & mask != low {
                sub += sub_size;
                let bits = next_table_bits(&cnt, len);
                sub_size = 1 << bits;
                table.resize(sub + sub_size, 0);
                low = key & mask;
                table[base + low] = ((sub - base) as u32) << 8 | (bits as u32 + ROOT);
            }
            let e = (sorted[sym] as u32) << 8 | (len as u32 - ROOT);
            sym += 1;
            let mut k = key >> ROOT;
            while k < sub_size {
                table[sub + k] = e;
                k += step;
            }
            key = next_key(key, len);
            cnt[len] -= 1;
        }
        step <<= 1;
    }
    if num_open != 0 {
        return Err(invalid("VP8L prefix code incomplete"));
    }
    Ok(Code {
        start,
        single: None,
    })
}

fn next_key(key: usize, len: usize) -> usize {
    let mut step = 1 << (len - 1);
    while key & step != 0 {
        step >>= 1;
    }
    if step == 0 {
        key
    } else {
        (key & (step - 1)) + step
    }
}

fn next_table_bits(count: &[u16; MAX_LEN + 1], mut len: usize) -> usize {
    let mut left: i32 = 1 << (len - ROOT as usize);
    while len < MAX_LEN {
        left -= count[len] as i32;
        if left <= 0 {
            break;
        }
        len += 1;
        left <<= 1;
    }
    len - ROOT as usize
}

#[inline(always)]
pub(super) fn read(table: &[u32], code: Code, bits: &mut Bits) -> u32 {
    let v = bits.peek();
    let base = code.start as usize;
    let mut e = table[base + (v as usize & ((1 << ROOT) - 1))];
    let n = e & 0xff;
    if n > ROOT {
        bits.consume(ROOT);
        let sub = (v >> ROOT) as usize & ((1 << (n - ROOT)) - 1);
        e = table[base + (e >> 8) as usize + sub];
    }
    bits.consume(e & 0xff);
    e >> 8
}

const ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

pub(super) fn read_code(
    bits: &mut Bits,
    alphabet: usize,
    lengths: &mut Vec<u8>,
    table: &mut Vec<u32>,
) -> Result<Code> {
    lengths.clear();
    lengths.resize(alphabet, 0);
    if bits.bit() {
        let two = bits.bit();
        let first8 = bits.bit();
        let s0 = bits.read(if first8 { 8 } else { 1 }) as usize;
        if s0 >= alphabet {
            return Err(invalid("VP8L simple code symbol"));
        }
        lengths[s0] = 1;
        if two {
            let s1 = bits.read(8) as usize;
            if s1 >= alphabet {
                return Err(invalid("VP8L simple code symbol"));
            }
            lengths[s1] = 1;
        }
    } else {
        let mut cl = [0u8; 19];
        let n = 4 + bits.read(4) as usize;
        for &i in ORDER.iter().take(n) {
            cl[i] = bits.read(3) as u8;
        }
        let mut cl_table = Vec::with_capacity(256);
        let cl_code = build(&cl, &mut cl_table)?;
        let mut max_symbol = if bits.bit() {
            let nbits = 2 + 2 * bits.read(3);
            let m = 2 + bits.read(nbits) as usize;
            if m > alphabet {
                return Err(invalid("VP8L code length count"));
            }
            m
        } else {
            alphabet
        };
        let mut prev = 8u8;
        let mut s = 0;
        while s < alphabet {
            if max_symbol == 0 {
                break;
            }
            max_symbol -= 1;
            bits.refill();
            let c = read(&cl_table, cl_code, bits) as u8;
            if c < 16 {
                lengths[s] = c;
                s += 1;
                if c != 0 {
                    prev = c;
                }
            } else {
                let (extra, add, v) = match c {
                    16 => (2, 3, prev),
                    17 => (3, 3, 0),
                    _ => (7, 11, 0),
                };
                let rep = add + bits.read(extra) as usize;
                if s + rep > alphabet {
                    return Err(invalid("VP8L code length repeat"));
                }
                lengths[s..s + rep].fill(v);
                s += rep;
            }
        }
    }
    build(lengths, table)
}
