use super::helpers;
use crate::syntax::bits::BitReader;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone)]
pub struct Reader<'a>(BitReader<'a>);

fn eof<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self(BitReader::new(data))
    }

    pub fn read_bit(&mut self) -> Result<bool> {
        self.0.read_bit().map_err(eof)
    }

    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        if n > 32 {
            return Err(format!("{n} bits requested at once"));
        }
        self.0.read_bits(n).map_err(eof)
    }

    pub fn skip_bits(&mut self, n: usize) -> Result<()> {
        self.0.skip(n).map_err(eof)
    }

    pub fn position(&self) -> usize {
        self.0.position()
    }

    pub fn read_le(&mut self, bytes: u32) -> Result<u32> {
        let mut t = 0u32;
        for i in 0..bytes.min(4) {
            t |= self.read_bits(8)? << (i * 8);
        }
        Ok(t)
    }

    pub fn read_uvlc(&mut self) -> Result<u32> {
        let mut leading_zeros = 0;
        while !self.read_bit()? {
            leading_zeros += 1;
            if leading_zeros >= 32 {
                return Ok(u32::MAX);
            }
        }
        let value = self.read_bits(leading_zeros)?;
        Ok(value.saturating_add((1u32 << leading_zeros) - 1))
    }

    pub fn read_leb128(&mut self) -> Result<u32> {
        if !self.position().is_multiple_of(8) {
            return Err("leb128 is not byte aligned".into());
        }
        let mut value = 0u64;
        for i in 0..8 {
            let byte = u64::from(self.read_bits(8)?);
            value |= (byte & 0x7f) << (i * 7);
            if byte & 0x80 == 0 {
                break;
            }
        }
        u32::try_from(value).map_err(|_| "leb128 value too large".into())
    }

    pub fn read_su(&mut self, num_bits: u32) -> Result<i32> {
        if num_bits == 0 || num_bits > 31 {
            return Err(format!("su({num_bits})"));
        }
        let mut value = self.read_bits(num_bits)? as i32;
        let sign_mask = 1i32 << (num_bits - 1);
        if value & sign_mask != 0 {
            value -= 2 * sign_mask;
        }
        Ok(value)
    }

    pub fn read_ns(&mut self, n: u32) -> Result<u32> {
        if n <= 1 {
            return Ok(0);
        }
        let w = helpers::floor_log2(n) + 1;
        let m = (1u32 << w) - n;
        let v = self.read_bits(w - 1)?;
        if v < m {
            return Ok(v);
        }
        let extra_bit = self.read_bit()?;
        Ok((v << 1) - m + u32::from(extra_bit))
    }

    pub fn read_delta_q(&mut self) -> Result<i32> {
        if self.read_bit()? {
            self.read_su(7)
        } else {
            Ok(0)
        }
    }

    pub fn read_trailing_bits(&mut self, num_bits: usize) -> Result<()> {
        if num_bits == 0 {
            return Err("no room for trailing bits".into());
        }
        if !self.read_bit()? {
            return Err("bad padding: trailing_one_bit is not set".into());
        }
        for _ in 1..num_bits {
            if self.read_bit()? {
                return Err("bad padding: trailing_zero_bit is set".into());
            }
        }
        Ok(())
    }

    fn decode_subexp(&mut self, num_syms: i32) -> Result<u32> {
        let mut i = 0;
        let mut mk = 0;
        let k = 3;
        loop {
            let b2 = if i != 0 { k + i - 1 } else { k };
            if b2 > 30 {
                return Err("subexp too long".into());
            }
            let a = 1 << b2;
            if num_syms <= mk + 3 * a {
                return self.read_ns((num_syms - mk).max(0) as u32);
            }
            if self.read_bit()? {
                i += 1;
                mk += a;
            } else {
                return Ok(self.read_bits(b2 as u32)? + mk as u32);
            }
        }
    }

    pub fn decode_unsigned_subexp_with_ref(&mut self, mx: i32, r: i32) -> Result<i32> {
        let v = self.decode_subexp(mx)? as i32;
        if (r << 1) <= mx {
            Ok(helpers::inverse_recenter(r, v))
        } else {
            Ok(mx - 1 - helpers::inverse_recenter(mx - 1 - r, v))
        }
    }

    pub fn decode_signed_subexp_with_ref(&mut self, low: i32, high: i32, r: i32) -> Result<i32> {
        Ok(self.decode_unsigned_subexp_with_ref(high - low, r - low)? + low)
    }

    pub fn byte_alignment(&mut self) -> Result<()> {
        while self.position() & 7 != 0 {
            self.read_bit()?;
        }
        Ok(())
    }
}
