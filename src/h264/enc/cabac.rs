use crate::h264::enc::tables::{NUM_CTX, RANGE_LPS, TRANS_LPS};

pub trait BinSink {
    fn decision(&mut self, ctx: usize, bin: u32);
    fn bypass(&mut self, bin: u32);
    fn terminate(&mut self, bin: u32);
}

#[derive(Clone)]
pub struct Contexts(pub [u8; NUM_CTX]);

impl Contexts {
    pub fn init(table: &[(i8, i8); NUM_CTX], slice_qp: i32) -> Self {
        let qp = slice_qp.clamp(0, 51);
        let mut s = [0u8; NUM_CTX];
        for (i, &(m, n)) in table.iter().enumerate() {
            let pre = (((m as i32) * qp) >> 4) + n as i32;
            let pre = pre.clamp(1, 126);
            s[i] = if pre <= 63 {
                ((63 - pre) as u8) << 1
            } else {
                (((pre - 64) as u8) << 1) | 1
            };
        }
        Contexts(s)
    }
}

#[derive(Clone)]
pub struct CabacEncoder {
    pub ctx: Contexts,
    low: u32,
    range: u32,
    outstanding: u32,
    first_bit: bool,
    buf: Vec<u8>,
    acc: u64,
    nbits: u32,
    bins: u64,
}

impl CabacEncoder {
    pub fn new(ctx: Contexts, header: Vec<u8>) -> Self {
        CabacEncoder {
            ctx,
            low: 0,
            range: 510,
            outstanding: 0,
            first_bit: true,
            buf: header,
            acc: 0,
            nbits: 0,
            bins: 0,
        }
    }

    #[inline]
    fn write_bit_raw(&mut self, b: u32) {
        self.acc = (self.acc << 1) | b as u64;
        self.nbits += 1;
        if self.nbits == 32 {
            self.buf.extend_from_slice(&(self.acc as u32).to_be_bytes());
            self.acc = 0;
            self.nbits = 0;
        }
    }

    #[inline]
    fn put_bit(&mut self, b: u32) {
        if self.first_bit {
            self.first_bit = false;
        } else {
            self.write_bit_raw(b);
        }
        while self.outstanding > 0 {
            self.write_bit_raw(1 - b);
            self.outstanding -= 1;
        }
    }

    #[inline]
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put_bit(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put_bit(1);
            } else {
                self.low -= 256;
                self.outstanding += 1;
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.terminate(1);
        while !self.nbits.is_multiple_of(8) {
            self.write_bit_raw(0);
        }
        let nbytes = self.nbits / 8;
        for i in 0..nbytes {
            self.buf.push((self.acc >> (8 * (nbytes - 1 - i))) as u8);
        }
        self.buf
    }
}

impl BinSink for CabacEncoder {
    #[inline]
    fn decision(&mut self, ctx: usize, bin: u32) {
        self.bins += 1;
        let s = self.ctx.0[ctx];
        let state = (s >> 1) as usize;
        let mps = (s & 1) as u32;
        let lps = RANGE_LPS[state][((self.range >> 6) & 3) as usize] as u32;
        self.range -= lps;
        if bin != mps {
            self.low += self.range;
            self.range = lps;
            let new_mps = if state == 0 { 1 - mps } else { mps };
            self.ctx.0[ctx] = (TRANS_LPS[state] << 1) | new_mps as u8;
        } else if state < 62 {
            self.ctx.0[ctx] = (((state + 1) as u8) << 1) | mps as u8;
        }
        self.renorm();
    }

    #[inline]
    fn bypass(&mut self, bin: u32) {
        self.bins += 1;
        self.low <<= 1;
        if bin != 0 {
            self.low += self.range;
        }
        if self.low >= 1024 {
            self.put_bit(1);
            self.low -= 1024;
        } else if self.low < 512 {
            self.put_bit(0);
        } else {
            self.low -= 512;
            self.outstanding += 1;
        }
    }

    fn terminate(&mut self, bin: u32) {
        self.bins += 1;
        self.range -= 2;
        if bin != 0 {
            self.low += self.range;
            self.range = 2;
            self.renorm();
            self.put_bit((self.low >> 9) & 1);
            let v = ((self.low >> 7) & 3) | 1;
            self.write_bit_raw((v >> 1) & 1);
            self.write_bit_raw(v & 1);
        } else {
            self.renorm();
        }
    }
}

pub struct CostTable {
    pub cost: [[u32; 2]; 64],
}

impl CostTable {
    pub fn new() -> Self {
        let mut cost = [[0u32; 2]; 64];
        for (s, c) in cost.iter_mut().enumerate() {
            let avg: f64 = RANGE_LPS[s].iter().map(|&v| v as f64).sum::<f64>() / 4.0;
            let p_lps = (avg / 384.0).clamp(1e-4, 0.5);
            c[0] = (-(p_lps.log2()) * 256.0).round() as u32;
            c[1] = (-((1.0 - p_lps).log2()) * 256.0).round() as u32;
        }
        CostTable { cost }
    }
}

impl Default for CostTable {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct CabacEstimator<'a> {
    pub ctx: Contexts,
    pub cost: u64,
    table: &'a CostTable,
}

impl<'a> CabacEstimator<'a> {
    pub fn new(ctx: Contexts, table: &'a CostTable) -> Self {
        CabacEstimator {
            ctx,
            cost: 0,
            table,
        }
    }
}

impl BinSink for CabacEstimator<'_> {
    #[inline]
    fn decision(&mut self, ctx: usize, bin: u32) {
        let s = self.ctx.0[ctx];
        let state = (s >> 1) as usize;
        let mps = (s & 1) as u32;
        if bin != mps {
            self.cost += self.table.cost[state][0] as u64;
            let new_mps = if state == 0 { 1 - mps } else { mps };
            self.ctx.0[ctx] = (TRANS_LPS[state] << 1) | new_mps as u8;
        } else {
            self.cost += self.table.cost[state][1] as u64;
            if state < 62 {
                self.ctx.0[ctx] = (((state + 1) as u8) << 1) | mps as u8;
            }
        }
    }
    #[inline]
    fn bypass(&mut self, _bin: u32) {
        self.cost += 256;
    }
    fn terminate(&mut self, bin: u32) {
        self.cost += if bin != 0 { 7 * 256 } else { 2 };
    }
}
