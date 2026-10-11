use std::sync::LazyLock;

pub struct BoolWriter {
    pub buf: Vec<u8>,
    low: u32,
    range: u32,
    count: i32,
}

impl BoolWriter {
    pub fn new(buf: Vec<u8>) -> Self {
        let mut w = BoolWriter {
            buf,
            low: 0,
            range: 255,
            count: -24,
        };
        w.put(false, 128);
        w
    }

    #[inline(always)]
    pub fn put(&mut self, bit: bool, prob: u8) {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let mut low = self.low;
        let mut range = split;
        if bit {
            low += split;
            range = self.range - split;
        }
        let mut shift = range.leading_zeros() as i32 - 24;
        range <<= shift;
        self.count += shift;
        if self.count >= 0 {
            let offset = shift - self.count;
            if (low << (offset - 1)) & 0x8000_0000 != 0 {
                let mut x = self.buf.len();
                while x > 0 && self.buf[x - 1] == 0xff {
                    self.buf[x - 1] = 0;
                    x -= 1;
                }
                if x > 0 {
                    self.buf[x - 1] += 1;
                }
            }
            self.buf.push((low >> (24 - offset)) as u8);
            low <<= offset;
            shift = self.count;
            low &= 0xff_ffff;
            self.count -= 8;
        }
        low <<= shift;
        self.low = low;
        self.range = range;
    }

    pub fn literal(&mut self, v: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.put((v >> i) & 1 == 1, 128);
        }
    }

    pub fn tree(&mut self, tree: &[i8], probs: &[u8], value: u8) {
        let (bits, len) = tree_path(tree, value);
        let mut node = 0usize;
        for i in (0..len).rev() {
            let b = (bits >> i) & 1;
            self.put(b == 1, probs[node >> 1]);
            let next = tree[node + b as usize];
            if next <= 0 {
                break;
            }
            node = next as usize;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..32 {
            self.put(false, 128);
        }
        if self.buf.last().is_some_and(|&b| b & 0xe0 == 0xc0) {
            self.buf.push(0);
        }
        self.buf
    }
}

pub fn tree_path(tree: &[i8], value: u8) -> (u32, u32) {
    fn walk(tree: &[i8], node: usize, value: u8, bits: u32, len: u32) -> Option<(u32, u32)> {
        for b in 0..2u32 {
            let t = tree[node + b as usize];
            let (nb, nl) = ((bits << 1) | b, len + 1);
            if t <= 0 {
                if (-t) as u8 == value {
                    return Some((nb, nl));
                }
            } else if let Some(r) = walk(tree, t as usize, value, nb, nl) {
                return Some(r);
            }
        }
        None
    }
    walk(tree, 0, value, 0, 0).unwrap_or((0, 0))
}

pub static COST: LazyLock<[u16; 257]> = LazyLock::new(|| {
    let mut t = [0u16; 257];
    t[0] = 255 * 16;
    for (i, c) in t.iter_mut().enumerate().skip(1) {
        *c = (-(i as f64 / 256.0).log2() * 256.0).round().min(4095.0) as u16;
    }
    t
});

#[inline(always)]
pub fn cost(bit: bool, prob: u8) -> u32 {
    COST[if bit {
        256 - prob as usize
    } else {
        prob as usize
    }] as u32
}

pub fn tree_costs(tree: &[i8], probs: &[u8], out: &mut [u32]) {
    fn walk(tree: &[i8], probs: &[u8], node: usize, acc: u32, out: &mut [u32]) {
        for b in 0..2usize {
            let t = tree[node + b];
            let c = acc + cost(b == 1, probs[node >> 1]);
            if t <= 0 {
                if let Some(o) = out.get_mut((-t) as usize) {
                    *o = c;
                }
            } else {
                walk(tree, probs, t as usize, c, out);
            }
        }
    }
    walk(tree, probs, 0, 0, out);
}
