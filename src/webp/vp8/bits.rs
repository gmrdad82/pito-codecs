#[derive(Clone)]
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    range: u32,
    bits: i32,
}

impl<'a> BoolDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = BoolDecoder {
            data,
            pos: 0,
            value: 0,
            range: 254,
            bits: -8,
        };
        d.load();
        d
    }

    #[inline(always)]
    fn load(&mut self) {
        if self.pos + 8 <= self.data.len() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.data[self.pos..self.pos + 8]);
            self.value = (self.value << 56) | (u64::from_be_bytes(b) >> 8);
            self.pos += 7;
            self.bits += 56;
        } else {
            self.load_tail();
        }
    }

    #[cold]
    fn load_tail(&mut self) {
        while self.bits < 0 {
            let byte = if self.pos < self.data.len() {
                self.pos += 1;
                self.data[self.pos - 1]
            } else {
                0
            };
            self.value = (self.value << 8) | byte as u64;
            self.bits += 8;
        }
    }

    #[inline(always)]
    pub fn get(&mut self, prob: u8) -> bool {
        if self.bits < 0 {
            self.load();
        }
        let pos = self.bits as u32;
        let split = (self.range * prob as u32) >> 8;
        let value = (self.value >> pos) as u32;
        let bit = value > split;
        let range = if bit {
            self.value -= ((split + 1) as u64) << pos;
            self.range - split
        } else {
            split + 1
        };
        let shift = range.leading_zeros() - 24;
        self.range = (range << shift) - 1;
        self.bits -= shift as i32;
        bit
    }

    #[inline(always)]
    pub fn get_signed(&mut self, v: i32) -> i32 {
        if self.get(128) { -v } else { v }
    }

    pub fn literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.get(128) as u32;
        }
        v
    }

    pub fn flag(&mut self) -> bool {
        self.get(128)
    }

    pub fn signed(&mut self, n: u32) -> i32 {
        let v = self.literal(n) as i32;
        if self.flag() { -v } else { v }
    }

    pub fn optional_signed(&mut self, n: u32) -> i32 {
        if self.flag() { self.signed(n) } else { 0 }
    }

    pub fn tree(&mut self, tree: &[i8], probs: &[u8]) -> u8 {
        let mut i = 0usize;
        loop {
            let next = tree[i + self.get(probs[i >> 1]) as usize];
            if next <= 0 {
                return (-next) as u8;
            }
            i = next as usize;
        }
    }
}
