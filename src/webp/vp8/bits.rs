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

    #[inline(always)]
    pub fn code(&mut self, c: &Code, probs: &[u8]) {
        for k in 0..c.len as usize {
            self.put((c.bits >> k) & 1 != 0, probs[c.nodes[k] as usize]);
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..32 {
            self.put(false, 128);
        }
        self.buf
    }
}

#[derive(Clone, Copy)]
pub struct Code {
    pub nodes: [u8; 10],
    pub bits: u16,
    pub len: u8,
}

impl Code {
    pub fn cost(&self, probs: &[u8]) -> u32 {
        (0..self.len as usize)
            .map(|k| {
                super::tokens::bit_cost((self.bits >> k) & 1 != 0, probs[self.nodes[k] as usize])
            })
            .sum()
    }
}

pub const fn codes<const N: usize>(tree: &[i8]) -> [Code; N] {
    let mut out = [Code {
        nodes: [0; 10],
        bits: 0,
        len: 0,
    }; N];
    let mut parent = [0usize; 32];
    let mut k = 0;
    while k < tree.len() {
        if tree[k] > 0 {
            parent[tree[k] as usize] = k;
        }
        k += 1;
    }
    k = 0;
    while k < tree.len() {
        if tree[k] <= 0 {
            let mut up = [0usize; 10];
            let mut n = 0;
            let mut p = k;
            loop {
                up[n] = p;
                n += 1;
                let node = p & !1;
                if node == 0 {
                    break;
                }
                p = parent[node];
            }
            let c = &mut out[(-tree[k]) as usize];
            c.len = n as u8;
            let mut j = 0;
            while j < n {
                let q = up[n - 1 - j];
                c.nodes[j] = (q >> 1) as u8;
                c.bits |= ((q & 1) as u16) << j;
                j += 1;
            }
        }
        k += 1;
    }
    out
}
