use crate::media::{Error, Result};

fn eof() -> Error {
    Error::Invalid("unexpected end of bitstream".into())
}

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    #[inline]
    fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let byte = self.pos / 8;
        let shift = self.pos % 8;
        let mut v: u64 = 0;
        for i in 0..5 {
            v = (v << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
        }
        ((v << (24 + shift)) >> (64 - n)) as u32
    }

    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n as usize > self.bits_left() {
            return Err(eof());
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        if n > self.bits_left() {
            return Err(eof());
        }
        self.pos += n;
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        let v = (value as u64) & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.nbits += n;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.buf.push((self.acc >> self.nbits) as u8);
        }
        self.acc &= (1u64 << self.nbits) - 1;
    }
    pub fn bit_len(&self) -> usize {
        self.buf.len() * 8 + self.nbits as usize
    }
    pub fn align_zero(&mut self) {
        if self.nbits > 0 {
            self.write_bits(0, 8 - self.nbits);
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.align_zero();
        self.buf
    }
}
