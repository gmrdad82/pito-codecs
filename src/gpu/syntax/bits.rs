use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitError {
    Eof,
    InvalidExpGolomb,
}

impl fmt::Display for BitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BitError::Eof => f.write_str("unexpected end of bitstream"),
            BitError::InvalidExpGolomb => f.write_str("invalid Exp-Golomb code"),
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
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    pub fn bits_left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }
    pub fn is_byte_aligned(&self) -> bool {
        self.pos.is_multiple_of(8)
    }
    pub fn byte_align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
    pub fn byte_pos(&self) -> usize {
        self.pos / 8
    }
    pub fn seek_bits(&mut self, pos: usize) {
        self.pos = pos;
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

    pub fn read_bits_u64(&mut self, n: u32) -> Result<u64> {
        if n <= 32 {
            return self.read_bits(n).map(u64::from);
        }
        let hi = self.read_bits(n - 32)? as u64;
        let lo = self.read_bits(32)? as u64;
        Ok((hi << 32) | lo)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Result<bool> {
        self.read_bits(1).map(|b| b == 1)
    }
    pub fn read_flag(&mut self) -> Result<bool> {
        self.read_bit()
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        if n > self.bits_left() {
            return Err(BitError::Eof);
        }
        self.pos += n;
        Ok(())
    }

    pub fn read_ue(&mut self) -> Result<u32> {
        let mut zeros = 0u32;
        while !self.read_bit()? {
            zeros += 1;
            if zeros > 32 {
                return Err(BitError::InvalidExpGolomb);
            }
        }
        if zeros == 0 {
            return Ok(0);
        }
        let v = self.read_bits_u64(zeros)?;
        let r = (1u64 << zeros) - 1 + v;
        u32::try_from(r).map_err(|_| BitError::InvalidExpGolomb)
    }

    pub fn read_se(&mut self) -> Result<i32> {
        let k = self.read_ue()? as i64;
        Ok(if k & 1 == 1 {
            ((k + 1) / 2) as i32
        } else {
            -((k / 2) as i32)
        })
    }

    pub fn read_te(&mut self, max: u32) -> Result<u32> {
        if max > 1 {
            self.read_ue()
        } else {
            Ok(!self.read_bit()? as u32)
        }
    }

    pub fn more_rbsp_data(&self) -> bool {
        let total = self.data.len() * 8;
        if self.pos >= total {
            return false;
        }
        let mut last = None;
        for (i, &b) in self.data.iter().enumerate().rev() {
            if b != 0 {
                last = Some(i * 8 + 7 - b.trailing_zeros() as usize);
                break;
            }
        }
        match last {
            Some(stop) => self.pos < stop,
            None => false,
        }
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
    pub fn write_bit(&mut self, b: bool) {
        self.write_bits(b as u32, 1);
    }
    pub fn write_ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros();
        let zeros = len - 1;
        for _ in 0..zeros {
            self.write_bit(false);
        }
        if len > 32 {
            self.write_bits((x >> 32) as u32, len - 32);
            self.write_bits(x as u32, 32);
        } else {
            self.write_bits(x as u32, len);
        }
    }
    pub fn write_se(&mut self, v: i32) {
        let k = if v > 0 {
            (v as u32) * 2 - 1
        } else {
            (-(v as i64) as u32) * 2
        };
        self.write_ue(k);
    }
    pub fn bit_len(&self) -> usize {
        self.buf.len() * 8 + self.nbits as usize
    }
    pub fn is_byte_aligned(&self) -> bool {
        self.nbits == 0
    }
    pub fn align_zero(&mut self) {
        if self.nbits > 0 {
            self.write_bits(0, 8 - self.nbits);
        }
    }
    pub fn rbsp_trailing(&mut self) {
        self.write_bit(true);
        self.align_zero();
    }
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        if self.nbits == 0 {
            self.buf.extend_from_slice(bytes);
        } else {
            for &b in bytes {
                self.write_bits(b as u32, 8);
            }
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        self.align_zero();
        self.buf
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

pub fn unescape_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

pub fn escape_rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 64);
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

pub fn annexb_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = if k + 1 < starts.len() {
            starts[k + 1] - 3
        } else {
            data.len()
        };
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    out
}

pub fn length_prefixed_nals(data: &[u8], length_size: usize) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        if i + length_size > data.len() {
            return Err(BitError::Eof);
        }
        let mut len = 0usize;
        for k in 0..length_size {
            len = (len << 8) | data[i + k] as usize;
        }
        i += length_size;
        if i + len > data.len() {
            return Err(BitError::Eof);
        }
        out.push(&data[i..i + len]);
        i += len;
    }
    Ok(out)
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
