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

pub struct BoolEncoder {
    buf: Vec<u8>,
    low: u32,
    range: u32,
    count: i32,
}

impl BoolEncoder {
    pub fn with_capacity(n: usize) -> Self {
        BoolEncoder {
            buf: Vec::with_capacity(n),
            low: 0,
            range: 255,
            count: -24,
        }
    }

    #[inline(always)]
    pub fn put(&mut self, bit: bool, prob: u8) {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        if bit {
            self.low = self.low.wrapping_add(split);
            self.range -= split;
        } else {
            self.range = split;
        }
        let mut shift = self.range.leading_zeros() as i32 - 24;
        self.range <<= shift;
        self.count += shift;
        if self.count >= 0 {
            let offset = shift - self.count;
            if (self.low << (offset - 1)) & 0x8000_0000 != 0 {
                self.carry();
            }
            self.buf.push((self.low >> (24 - offset)) as u8);
            self.low <<= offset;
            shift = self.count;
            self.low &= 0xff_ffff;
            self.count -= 8;
        }
        self.low <<= shift;
    }

    #[cold]
    fn carry(&mut self) {
        let mut i = self.buf.len();
        while i > 0 && self.buf[i - 1] == 0xff {
            self.buf[i - 1] = 0;
            i -= 1;
        }
        if i > 0 {
            self.buf[i - 1] += 1;
        }
    }

    pub fn flag(&mut self, bit: bool) {
        self.put(bit, 128);
    }

    pub fn literal(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.put((v >> i) & 1 != 0, 128);
        }
    }

    pub fn signed(&mut self, v: i32, n: u32) {
        self.literal(v.unsigned_abs(), n);
        self.flag(v < 0);
    }

    pub fn optional_signed(&mut self, v: i32, n: u32) {
        self.flag(v != 0);
        if v != 0 {
            self.signed(v, n);
        }
    }

    pub fn tree(&mut self, tree: &[i8], probs: &[u8], value: u8) {
        let mut path = [(0usize, false); 16];
        let mut len = 0;
        if !Self::find(tree, 0, value, &mut path, &mut len) {
            return;
        }
        for &(i, b) in &path[..len] {
            self.put(b, probs[i >> 1]);
        }
    }

    fn find(
        tree: &[i8],
        i: usize,
        value: u8,
        path: &mut [(usize, bool); 16],
        len: &mut usize,
    ) -> bool {
        for b in 0..2 {
            let next = tree[i + b];
            path[*len] = (i, b == 1);
            *len += 1;
            if next <= 0 {
                if (-next) as u8 == value {
                    return true;
                }
            } else if Self::find(tree, next as usize, value, path, len) {
                return true;
            }
            *len -= 1;
        }
        false
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..32 {
            self.put(false, 128);
        }
        self.buf
    }
}
