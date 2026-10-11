pub(super) struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    nbits: u32,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut b = Bits {
            data,
            pos: 0,
            buf: 0,
            nbits: 0,
        };
        b.refill();
        b
    }

    #[inline(always)]
    pub fn refill(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let v = u64::from_le_bytes(chunk.try_into().unwrap());
            self.buf |= v << self.nbits;
            self.pos += ((63 - self.nbits) >> 3) as usize;
            self.nbits |= 56;
        } else {
            self.refill_slow();
        }
    }

    #[cold]
    fn refill_slow(&mut self) {
        while self.nbits <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.buf |= (b as u64) << self.nbits;
            self.pos += 1;
            self.nbits += 8;
        }
    }

    #[inline(always)]
    pub fn peek(&self) -> u64 {
        self.buf
    }

    #[inline(always)]
    pub fn consume(&mut self, n: u32) {
        self.buf >>= n;
        self.nbits -= n;
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if self.nbits < n {
            self.refill();
        }
        let v = (self.buf & ((1u64 << n) - 1)) as u32;
        self.consume(n);
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        self.read(1) == 1
    }

    pub fn overrun(&self) -> bool {
        self.pos * 8 > self.data.len() * 8 + self.nbits as usize
    }
}
