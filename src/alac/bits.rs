use crate::media::{Error, Result};

pub fn overrun() -> Error {
    Error::Invalid("ALAC packet ends early".into())
}

#[inline(always)]
pub fn word(data: &[u8], at: usize) -> u32 {
    match data.get(at..at.saturating_add(4)) {
        Some(&[a, b, c, d]) => u32::from_be_bytes([a, b, c, d]),
        _ => {
            let byte = |i: usize| data.get(at.saturating_add(i)).copied().unwrap_or(0);
            u32::from_be_bytes([byte(0), byte(1), byte(2), byte(3)])
        }
    }
}

#[inline(always)]
pub fn byte(data: &[u8], at: usize) -> u32 {
    data.get(at).copied().unwrap_or(0) as u32
}

#[inline(always)]
pub fn shl(x: u32, n: u32) -> u32 {
    x.checked_shl(n).unwrap_or(0)
}

#[inline(always)]
pub fn shr(x: u32, n: u32) -> u32 {
    x.checked_shr(n).unwrap_or(0)
}

#[derive(Clone, Copy)]
pub struct Bits<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn end(&self) -> usize {
        self.data.len() * 8
    }

    pub fn more(&self) -> bool {
        self.pos < self.end()
    }

    pub fn check(&self) -> Result<()> {
        if self.pos > self.end() {
            Err(overrun())
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let at = self.pos >> 3;
        let wide = ((word(self.data, at) as u64) << 8) | byte(self.data, at + 4) as u64;
        let v = ((wide << (self.pos & 7)) & 0xff_ffff_ffff) >> (40 - n.min(32));
        self.pos = self.pos.saturating_add(n as usize);
        v as u32
    }

    pub fn one(&mut self) -> u32 {
        self.read(1)
    }

    pub fn advance(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    pub fn align(&mut self) {
        self.pos = self.pos.saturating_add(7) & !7;
    }
}
