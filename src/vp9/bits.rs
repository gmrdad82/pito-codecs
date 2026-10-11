use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitError {
    Eof,
}

impl fmt::Display for BitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitError::Eof => f.write_str("unexpected end of bitstream"),
        }
    }
}
impl std::error::Error for BitError {}

pub type Result<T> = std::result::Result<T, BitError>;

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

    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n <= 32);
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
            return Err(BitError::Eof);
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Result<bool> {
        self.read_bits(1).map(|b| b == 1)
    }
    pub fn read_flag(&mut self) -> Result<bool> {
        self.read_bit()
    }
}
