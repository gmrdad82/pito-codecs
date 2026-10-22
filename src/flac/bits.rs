use super::Fault;

pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    cache: u64,
    left: u32,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            pos: 0,
            cache: 0,
            left: 0,
        }
    }

    #[inline(always)]
    fn refill(&mut self) {
        if let Some(b) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            self.cache |= w >> self.left;
            let take = (63 - self.left) >> 3;
            self.pos += take as usize;
            self.left += take << 3;
        } else {
            while self.left <= 56 {
                let Some(&b) = self.data.get(self.pos) else {
                    break;
                };
                self.cache |= (b as u64) << (56 - self.left);
                self.pos += 1;
                self.left += 8;
            }
        }
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> Result<u32, Fault> {
        if n == 0 {
            return Ok(0);
        }
        if self.left < n {
            self.refill();
            if self.left < n {
                return Err(Fault::Short);
            }
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.left -= n;
        Ok(v)
    }

    #[inline(always)]
    pub fn signed(&mut self, n: u32) -> Result<i32, Fault> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read(n)?;
        Ok(((v << (32 - n)) as i32) >> (32 - n))
    }

    #[inline(always)]
    pub fn unary(&mut self) -> Result<u32, Fault> {
        let mut n = 0u32;
        loop {
            let lz = self.cache.leading_zeros();
            if lz < self.left {
                self.cache <<= lz + 1;
                self.left -= lz + 1;
                return Ok(n.saturating_add(lz));
            }
            n = n.saturating_add(self.left);
            self.cache = 0;
            self.left = 0;
            self.refill();
            if self.left == 0 {
                return Err(Fault::Short);
            }
        }
    }

    #[inline(always)]
    pub fn rice(&mut self, k: u32) -> Result<i32, Fault> {
        if self.left < 32 {
            self.refill();
        }
        let lz = self.cache.leading_zeros();
        let v = if lz + 1 + k <= self.left {
            let c = self.cache << (lz + 1);
            let r = ((c >> (63 - k)) >> 1) as u32;
            self.cache = c << k;
            self.left -= lz + 1 + k;
            (lz << k) | r
        } else {
            let q = self.unary()?;
            let r = self.read(k)?;
            (q << k) | r
        };
        Ok(((v >> 1) as i32) ^ -((v & 1) as i32))
    }

    pub fn align(&mut self) -> Result<u32, Fault> {
        let n = self.left & 7;
        self.read(n)
    }

    pub fn bytes(&self) -> usize {
        (self.pos * 8 - self.left as usize).div_ceil(8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_across_bytes_and_ends_short() {
        let data = [0b1010_0101, 0b1110_0001, 0b1101_0010, 0b0101_0101];
        let mut b = Bits::new(&data);
        assert_eq!(b.read(1), Ok(1));
        assert_eq!(b.read(13), Ok(0b0_1001_0111_1000));
        assert_eq!(b.read(9), Ok(0b0_1110_1001));
        assert_eq!(b.read(9), Ok(0b0_0101_0101));
        assert_eq!(b.read(1), Err(Fault::Short));
    }

    #[test]
    fn reads_long_runs_over_the_fast_refill() {
        let data: Vec<u8> = (0..64u32).map(|i| (i * 37 + 11) as u8).collect();
        let mut b = Bits::new(&data);
        for (i, &want) in data.iter().enumerate() {
            assert_eq!(b.read(8), Ok(want as u32), "byte {i}");
        }
        assert_eq!(b.read(8), Err(Fault::Short));
        let mut b = Bits::new(&data);
        let mut got = Vec::new();
        for _ in 0..data.len() / 4 {
            got.extend_from_slice(&b.read(32).unwrap().to_be_bytes());
        }
        assert_eq!(got, data);
    }

    #[test]
    fn signed_extends_the_top_bit() {
        let data = [0b1111_1011, 0b0111_1111, 0xff, 0xff, 0xff, 0xff];
        let mut b = Bits::new(&data);
        assert_eq!(b.signed(4), Ok(-1));
        assert_eq!(b.signed(4), Ok(-5));
        assert_eq!(b.signed(8), Ok(127));
        assert_eq!(b.signed(32), Ok(-1));
        assert_eq!(b.signed(0), Ok(0));
    }

    #[test]
    fn unary_counts_zeros_across_bytes_and_long_runs() {
        let data = [0b1010_0100, 0b1000_0000, 0b0010_0000, 0, 0b0000_1010];
        let mut b = Bits::new(&data);
        assert_eq!(b.unary(), Ok(0));
        assert_eq!(b.unary(), Ok(1));
        assert_eq!(b.unary(), Ok(2));
        assert_eq!(b.unary(), Ok(2));
        assert_eq!(b.unary(), Ok(9));
        assert_eq!(b.unary(), Ok(17));
        assert_eq!(b.read(3), Ok(0b010));
        let mut zeros = vec![0u8; 40];
        zeros.push(0x80);
        let mut b = Bits::new(&zeros);
        assert_eq!(b.unary(), Ok(320));
        let mut b = Bits::new(&[0u8; 20]);
        assert_eq!(b.unary(), Err(Fault::Short));
    }

    fn rice_reference(b: &mut Bits, k: u32) -> Result<i32, Fault> {
        let q = b.unary()?;
        let r = b.read(k)?;
        let v = (q << k) | r;
        Ok(((v >> 1) as i32) ^ -((v & 1) as i32))
    }

    #[test]
    fn rice_fast_path_equals_unary_plus_remainder() {
        let data: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 | ((i % 3 == 0) as u8))
            .collect();
        for k in 0..=30 {
            let mut a = Bits::new(&data);
            let mut b = Bits::new(&data);
            loop {
                let x = a.rice(k);
                let y = rice_reference(&mut b, k);
                assert_eq!(x, y, "parameter {k}");
                if x.is_err() {
                    break;
                }
            }
        }
    }

    #[test]
    fn rice_maps_to_signed_values() {
        let data = [0b1100_1010, 0b0101_1000];
        let mut b = Bits::new(&data);
        assert_eq!(b.rice(1), Ok(-1));
        assert_eq!(b.rice(0), Ok(1));
        assert_eq!(b.rice(0), Ok(-1));
        assert_eq!(b.rice(1), Ok(2));
    }

    #[test]
    fn align_and_bytes_track_the_position() {
        let data = [0xff, 0x80, 0x00];
        let mut b = Bits::new(&data);
        assert_eq!(b.bytes(), 0);
        assert_eq!(b.read(3), Ok(7));
        assert_eq!(b.bytes(), 1);
        assert_eq!(b.align(), Ok(0b11111));
        assert_eq!(b.bytes(), 1);
        assert_eq!(b.read(1), Ok(1));
        assert_eq!(b.align(), Ok(0));
        assert_eq!(b.bytes(), 2);
        assert_eq!(b.align(), Ok(0));
        assert_eq!(b.bytes(), 2);
    }
}
