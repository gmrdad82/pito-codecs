use super::super::writer::ZIGZAG;
use super::super::{Error, Result};
use super::table::{LOOKAHEAD, Table};

pub struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    left: u32,
    pub marker: Option<u8>,
    eof: bool,
    pad: u32,
    pub eob_run: i32,
    low_mask: i16,
    ss: usize,
    se: usize,
}

fn truncated() -> Error {
    Error("JPEG: the file ends inside the image data".into())
}

fn extend(x: i32, s: i32) -> i32 {
    x + (((x - (1 << (s - 1))) >> 31) & ((-1 << s) + 1))
}

fn natural(k: usize) -> usize {
    usize::from(ZIGZAG[k.min(63)])
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Bits<'a> {
        Bits {
            data,
            pos,
            buf: 0,
            left: 0,
            marker: None,
            eof: false,
            pad: 0,
            eob_run: 0,
            low_mask: 1,
            ss: 0,
            se: 63,
        }
    }

    pub fn progressive(&mut self, ss: u8, se: u8, al: u8) {
        self.ss = usize::from(ss);
        self.se = usize::from(se);
        self.low_mask = 1 << al;
    }

    fn next_byte(&mut self) -> u8 {
        if self.marker.is_some() {
            self.pad += 8;
            return 0;
        }
        let Some(&b) = self.data.get(self.pos) else {
            self.eof = true;
            self.pad += 8;
            return 0;
        };
        if b != 0xff {
            self.pos += 1;
            return b;
        }
        let mut p = self.pos + 1;
        while self.data.get(p) == Some(&0xff) {
            p += 1;
        }
        match self.data.get(p) {
            Some(0) => {
                self.pos = p + 1;
                0xff
            }
            Some(&m) => {
                self.marker = Some(m);
                self.pos = p + 1;
                self.pad += 8;
                0
            }
            None => {
                self.pos = p;
                self.eof = true;
                self.pad += 8;
                0
            }
        }
    }

    #[inline]
    fn refill(&mut self) -> Result<()> {
        if self.left > 32 {
            return Ok(());
        }
        if self.left < self.pad {
            return Err(self.short());
        }
        if self.marker.is_none()
            && let Some(four) = self.data.get(self.pos..self.pos + 4)
            && !four.contains(&0xff)
        {
            let word = u32::from_be_bytes([four[0], four[1], four[2], four[3]]);
            self.buf |= u64::from(word) << (32 - self.left);
            self.left += 32;
            self.pos += 4;
            return Ok(());
        }
        while self.left <= 56 {
            let b = self.next_byte();
            self.buf |= u64::from(b) << (56 - self.left);
            self.left += 8;
        }
        Ok(())
    }

    fn short(&self) -> Error {
        match self.marker {
            Some(m) if !self.eof => Error(format!(
                "JPEG: the image data stops at marker 0xFF{m:02X} before its scan ends"
            )),
            _ => truncated(),
        }
    }

    pub fn finish(&self) -> Result<()> {
        if self.left < self.pad {
            return Err(self.short());
        }
        Ok(())
    }

    #[inline]
    fn peek(&self, n: u32) -> i32 {
        (self.buf >> (64 - n)) as i32
    }

    #[inline]
    fn drop_bits(&mut self, n: u32) {
        let n = n.min(self.left);
        self.buf = self.buf.checked_shl(n).unwrap_or(0);
        self.left -= n;
    }

    #[inline]
    fn get_bits(&mut self, n: u32) -> Result<i32> {
        if n == 0 {
            return Ok(0);
        }
        if self.left < n {
            self.refill()?;
        }
        let v = self.peek(n);
        self.drop_bits(n);
        Ok(v)
    }

    #[inline]
    fn get_bit(&mut self) -> Result<bool> {
        Ok(self.get_bits(1)? == 1)
    }

    #[inline]
    fn huff(&mut self, t: &Table) -> Result<i32> {
        self.refill()?;
        let entry = t.lookup[self.peek(LOOKAHEAD) as usize];
        let mut len = entry >> LOOKAHEAD;
        let mut sym = entry & ((1 << LOOKAHEAD) - 1);
        if len > LOOKAHEAD as i32 {
            let code = self.peek(16);
            while len < 17 && code >= t.maxcode[len as usize] {
                len += 1;
            }
            if len == 17 {
                return Err(Error(format!(
                    "JPEG: a bad Huffman code 0x{code:X} in the image data"
                )));
            }
            let at = (code >> (16 - len)) + t.offset[len as usize];
            sym = i32::from(t.values[(at & 0xff) as usize]);
        }
        self.drop_bits(len as u32);
        Ok(sym)
    }

    fn dc(&mut self, t: &Table, pred: &mut i32) -> Result<()> {
        let s = self.huff(t)?;
        if s != 0 {
            let r = self.get_bits(s as u32)?;
            *pred = pred.wrapping_add(extend(r, s));
        }
        Ok(())
    }

    pub fn baseline(
        &mut self,
        dc: &Table,
        ac: &Table,
        q: &[i32; 64],
        block: &mut [i32; 64],
        pred: &mut i32,
    ) -> Result<()> {
        self.dc(dc, pred)?;
        block[0] = pred.wrapping_mul(q[0]);
        let mut k = 1usize;
        while k < 64 {
            self.refill()?;
            let look = self.peek(LOOKAHEAD) as usize;
            let fast = ac.fast_ac[look];
            if fast != 0 {
                k += ((fast >> 4) & 15) as usize;
                let z = natural(k);
                block[z] = i32::from(fast >> 8).wrapping_mul(q[z]);
                self.drop_bits((fast & 15) as u32);
                k += 1;
                continue;
            }
            let rs = self.huff(ac)?;
            let run = rs >> 4;
            let s = rs & 15;
            if s != 0 {
                k += run as usize;
                let r = self.get_bits(s as u32)?;
                let z = natural(k);
                block[z] = extend(r, s).wrapping_mul(q[z]);
                k += 1;
            } else if run != 15 {
                return Ok(());
            } else {
                k += 16;
            }
        }
        Ok(())
    }

    pub fn dc_first(&mut self, t: &Table, block: &mut [i16; 64], pred: &mut i32) -> Result<()> {
        self.dc(t, pred)?;
        block[0] = (*pred as i16).wrapping_mul(self.low_mask);
        Ok(())
    }

    pub fn dc_refine(&mut self, block: &mut [i16; 64]) -> Result<()> {
        if self.get_bit()? {
            block[0] |= self.low_mask;
        }
        Ok(())
    }

    pub fn ac_first(&mut self, t: &Table, block: &mut [i16; 64]) -> Result<()> {
        if self.eob_run > 0 {
            self.eob_run -= 1;
            return Ok(());
        }
        let bit = self.low_mask;
        let mut k = self.ss;
        while k <= self.se {
            self.refill()?;
            let fast = t.fast_ac[self.peek(LOOKAHEAD) as usize];
            if fast != 0 {
                k += ((fast >> 4) & 15) as usize;
                block[natural(k)] = (fast >> 8).wrapping_mul(bit);
                self.drop_bits((fast & 15) as u32);
                k += 1;
                continue;
            }
            let rs = self.huff(t)?;
            let run = rs >> 4;
            let s = rs & 15;
            if s != 0 {
                k += run as usize;
                let r = self.get_bits(s as u32)?;
                block[natural(k)] = (extend(r, s) as i16).wrapping_mul(bit);
                k += 1;
            } else if run != 15 {
                self.eob_run = (1 << run) + self.get_bits(run as u32)? - 1;
                break;
            } else {
                k += 16;
            }
        }
        Ok(())
    }

    pub fn ac_refine(&mut self, t: &Table, block: &mut [i16; 64]) -> Result<()> {
        let p1 = self.low_mask;
        let m1 = -p1;
        let mut k = self.ss;
        if self.eob_run == 0 {
            while k <= self.se {
                let rs = self.huff(t)?;
                let mut run = rs >> 4;
                let s = rs & 15;
                let mut value = 0i16;
                if s == 0 {
                    if run != 15 {
                        self.eob_run = (1 << run) + self.get_bits(run as u32)?;
                        break;
                    }
                } else {
                    value = if self.get_bit()? { p1 } else { m1 };
                }
                while k <= self.se {
                    let z = natural(k);
                    if block[z] != 0 {
                        if self.get_bit()? && block[z] & p1 == 0 {
                            block[z] = if block[z] >= 0 {
                                block[z].wrapping_add(p1)
                            } else {
                                block[z].wrapping_add(m1)
                            };
                        }
                    } else {
                        if run == 0 {
                            break;
                        }
                        run -= 1;
                    }
                    k += 1;
                }
                if value != 0 && k <= self.se {
                    block[natural(k)] = value;
                }
                k += 1;
            }
        }
        if self.eob_run > 0 {
            while k <= self.se {
                let z = natural(k);
                if block[z] != 0 && self.get_bit()? && block[z] & p1 == 0 {
                    block[z] = if block[z] >= 0 {
                        block[z].wrapping_add(p1)
                    } else {
                        block[z].wrapping_add(m1)
                    };
                }
                k += 1;
            }
            self.eob_run -= 1;
        }
        Ok(())
    }

    pub fn restart(&mut self) -> Result<()> {
        self.finish()?;
        self.buf = 0;
        self.left = 0;
        self.pad = 0;
        self.eob_run = 0;
        let marker = match self.marker.take() {
            Some(m) => Some(m),
            None => self.seek_marker().map(|(m, at)| {
                self.pos = at + 2;
                m
            }),
        };
        match marker {
            Some(0xd0..=0xd7) => Ok(()),
            Some(m) => Err(Error(format!(
                "JPEG: marker 0xFF{m:02X} where a restart marker belongs"
            ))),
            None => Err(truncated()),
        }
    }

    fn seek_marker(&self) -> Option<(u8, usize)> {
        let mut p = self.pos;
        loop {
            let b = *self.data.get(p)?;
            if b == 0xff {
                match *self.data.get(p + 1)? {
                    0 | 0xff => p += 1,
                    m => return Some((m, p)),
                }
            } else {
                p += 1;
            }
        }
    }

    pub fn resume_at(&self) -> usize {
        if self.marker.is_some() {
            return self.pos.saturating_sub(2);
        }
        self.seek_marker().map_or(self.data.len(), |(_, at)| at)
    }
}

#[cfg(test)]
pub fn extend_for_test(x: i32, s: i32) -> i32 {
    extend(x, s)
}

#[cfg(test)]
impl Bits<'_> {
    pub fn huff_for_test(&mut self, t: &Table) -> Result<i32> {
        self.huff(t)
    }

    pub fn get_for_test(&mut self, n: u32) -> Result<i32> {
        self.get_bits(n)
    }
}
