use super::super::{Error, Result};

pub const LOOKAHEAD: u32 = 9;
const SIZE: usize = 1 << LOOKAHEAD;

pub struct Table {
    pub maxcode: [i32; 18],
    pub offset: [i32; 18],
    pub lookup: [i32; SIZE],
    pub fast_ac: [i16; SIZE],
    pub values: [u8; 256],
}

impl Table {
    pub fn new(bits: &[u8; 17], symbols: &[u8], dc: bool) -> Result<Table> {
        let bad = || Error("JPEG: a Huffman table with impossible code lengths".into());
        let total: usize = bits[1..].iter().map(|&n| usize::from(n)).sum();
        if total > 256 || total != symbols.len() {
            return Err(bad());
        }
        let mut values = [0u8; 256];
        values[..symbols.len()].copy_from_slice(symbols);
        let too_long = (LOOKAHEAD as i32 + 1) << LOOKAHEAD;
        let mut t = Table {
            maxcode: [0; 18],
            offset: [0; 18],
            lookup: [too_long; SIZE],
            fast_ac: [0; SIZE],
            values,
        };

        let mut huff_size = [0u8; 257];
        let mut huff_code = [0u32; 257];
        let mut p = 0;
        for (l, &n) in bits.iter().enumerate().skip(1) {
            for _ in 0..n {
                huff_size[p] = l as u8;
                p += 1;
            }
        }
        let count = p;
        let mut code = 0u32;
        let mut si = u32::from(huff_size[0]);
        p = 0;
        while huff_size[p] != 0 {
            while u32::from(huff_size[p]) == si {
                huff_code[p] = code;
                code += 1;
                p += 1;
            }
            if code >= (1 << si) {
                return Err(bad());
            }
            t.maxcode[si as usize] = (code << (16 - si)) as i32;
            code <<= 1;
            si += 1;
        }
        p = 0;
        for (l, &n) in bits.iter().enumerate() {
            if n == 0 {
                t.maxcode[l] = -1;
            } else {
                t.offset[l] = p as i32 - huff_code[p] as i32;
                p += usize::from(n);
            }
        }
        t.offset[17] = 0;
        t.maxcode[17] = 0x000F_FFFF;

        p = 0;
        for l in 1..=LOOKAHEAD {
            for _ in 0..bits[l as usize] {
                let first = (huff_code[p] as usize) << (LOOKAHEAD - l);
                let entry = ((l as i32) << LOOKAHEAD) | i32::from(t.values[p]);
                t.lookup[first..first + (1 << (LOOKAHEAD - l))].fill(entry);
                p += 1;
            }
        }

        if dc {
            if t.values[..count].iter().any(|&v| v > 15) {
                return Err(Error(
                    "JPEG: a DC Huffman table with a symbol above 15".into(),
                ));
            }
            return Ok(t);
        }
        let mut fast = [u16::MAX; SIZE];
        for i in 0..count {
            let s = u32::from(huff_size[i]);
            if s <= LOOKAHEAD {
                let c = (huff_code[i] << (LOOKAHEAD - s)) as usize;
                fast[c..c + (1 << (LOOKAHEAD - s))].fill(i as u16);
            }
        }
        for (i, &f) in fast.iter().enumerate() {
            if f == u16::MAX {
                continue;
            }
            let rs = t.values[usize::from(f)];
            let run = i16::from(rs >> 4);
            let mag = i16::from(rs & 15);
            let len = i16::from(huff_size[usize::from(f)]);
            if mag != 0 && len + mag <= LOOKAHEAD as i16 {
                let mut k =
                    (((i as i16) << len) & ((1 << LOOKAHEAD) - 1)) >> (LOOKAHEAD as i16 - mag);
                if k < 1 << (mag - 1) {
                    k += (!0i16 << mag) + 1;
                }
                if (-128..=127).contains(&k) {
                    t.fast_ac[i] = (k << 8) + (run << 4) + (len + mag);
                }
            }
        }
        Ok(t)
    }
}
