use crate::media::{Error, Result};

fn eof() -> Error {
    Error::Invalid("AC-3 frame ends early".into())
}

pub struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    len: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            len: data.len() * 8,
        }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline(always)]
    fn load(&self) -> u64 {
        let byte = self.pos >> 3;
        let v = match self.data.get(byte..byte + 8) {
            Some(b) => u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            None => {
                let mut b = [0u8; 8];
                let rest = self.data.get(byte..).unwrap_or_default();
                b[..rest.len()].copy_from_slice(rest);
                u64::from_be_bytes(b)
            }
        };
        v << (self.pos & 7)
    }

    #[inline(always)]
    fn peek(&self, n: u32) -> u32 {
        (self.load() >> (64 - n)) as u32
    }

    #[inline(always)]
    pub fn take(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline(always)]
    pub fn take0(&mut self, n: u32) -> u32 {
        let v = ((self.load() >> 1) >> (63 - n)) as u32;
        self.pos += n as usize;
        v
    }

    pub fn check(&self) -> Result<()> {
        if self.pos > self.len {
            Err(eof())
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> Result<u32> {
        if n == 0 {
            return Ok(0);
        }
        if self.pos + n as usize > self.len {
            return Err(eof());
        }
        Ok(self.take(n))
    }

    #[inline(always)]
    pub fn signed(&mut self, n: u32) -> Result<i32> {
        let v = self.read(n)? as i32;
        Ok((v << (32 - n)) >> (32 - n))
    }

    #[inline(always)]
    pub fn flag(&mut self) -> Result<bool> {
        Ok(self.read(1)? != 0)
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.len {
            return Err(eof());
        }
        self.pos += n;
        Ok(())
    }
}
