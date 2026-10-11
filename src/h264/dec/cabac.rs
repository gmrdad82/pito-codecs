use crate::h264::dec::cabac_tables::CABAC_INIT_MN;
use crate::h264::dec::error::{Result, ensure};

#[rustfmt::skip]
pub(crate) static RANGE_TAB_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240], [128, 167, 197, 227], [128, 158, 187, 216], [123, 150, 178, 205],
    [116, 142, 169, 195], [111, 135, 160, 185], [105, 128, 152, 175], [100, 122, 144, 166],
    [95, 116, 137, 158], [90, 110, 130, 150], [85, 104, 123, 142], [81, 99, 117, 135],
    [77, 94, 111, 128], [73, 89, 105, 122], [69, 85, 100, 116], [66, 80, 95, 110],
    [62, 76, 90, 104], [59, 72, 86, 99], [56, 69, 81, 94], [53, 65, 77, 89],
    [51, 62, 73, 85], [48, 59, 69, 80], [46, 56, 66, 76], [43, 53, 63, 72],
    [41, 50, 59, 69], [39, 48, 56, 65], [37, 45, 54, 62], [35, 43, 51, 59],
    [33, 41, 48, 56], [32, 39, 46, 53], [30, 37, 43, 50], [29, 35, 41, 48],
    [27, 33, 39, 45], [26, 31, 37, 43], [24, 30, 35, 41], [23, 28, 33, 39],
    [22, 27, 32, 37], [21, 26, 30, 35], [20, 24, 29, 33], [19, 23, 27, 31],
    [18, 22, 26, 30], [17, 21, 25, 28], [16, 20, 23, 27], [15, 19, 22, 25],
    [14, 18, 21, 24], [14, 17, 20, 23], [13, 16, 19, 22], [12, 15, 18, 21],
    [12, 14, 17, 20], [11, 14, 16, 19], [11, 13, 15, 18], [10, 12, 15, 17],
    [10, 12, 14, 16], [9, 11, 13, 15], [9, 11, 12, 14], [8, 10, 12, 14],
    [8, 9, 11, 13], [7, 9, 11, 12], [7, 9, 10, 12], [7, 8, 10, 11],
    [6, 8, 9, 11], [6, 7, 9, 10], [6, 7, 8, 9], [2, 2, 2, 2],
];

#[rustfmt::skip]
static TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12,
    13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21, 21, 22, 22, 23, 24,
    24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33,
    33, 33, 34, 34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

#[rustfmt::skip]
static TRANS_IDX_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48,
    49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

pub(crate) static NEXT_STATE: [[u8; 2]; 128] = {
    let mut t = [[0u8; 2]; 128];
    let mut s = 0;
    while s < 128 {
        let p = s >> 1;
        let mps = s & 1;
        t[s][0] = (TRANS_IDX_MPS[p] << 1) | mps as u8;
        let new_mps = if p == 0 { 1 - mps } else { mps };
        t[s][1] = (TRANS_IDX_LPS[p] << 1) | new_mps as u8;
        s += 1;
    }
    t
};

pub const NUM_CTX: usize = 1024;

pub struct Cabac<'a> {
    data: &'a [u8],
    next_byte: usize,
    range: u32,
    value: u64,
    bits: u32,
    pub ctx: [u8; NUM_CTX],
}

impl<'a> Cabac<'a> {
    pub fn new(data: &'a [u8], pos: usize, slice_qp: i32, init: usize) -> Result<Self> {
        let mut c = Cabac {
            data,
            next_byte: pos / 8,
            range: 0,
            value: 0,
            bits: 0,
            ctx: [0; NUM_CTX],
        };
        c.init_contexts(slice_qp, init);
        c.init_engine()?;
        Ok(c)
    }

    pub fn init_contexts(&mut self, slice_qp: i32, init: usize) {
        let qp = slice_qp.clamp(0, 51);
        let tab = &CABAC_INIT_MN[init];
        for (c, &(m, n)) in self.ctx.iter_mut().zip(tab.iter()) {
            let pre = (((m as i32) * qp) >> 4) + n as i32;
            let pre = pre.clamp(1, 126);
            *c = if pre <= 63 {
                ((63 - pre) << 1) as u8
            } else {
                (((pre - 64) << 1) | 1) as u8
            };
        }
    }

    pub fn init_engine(&mut self) -> Result<()> {
        self.range = 510;
        self.value = 0;
        self.bits = 0;
        self.refill();
        self.bits -= 9;
        ensure!(
            (self.value >> self.bits) < 510,
            "invalid CABAC offset at init"
        );
        Ok(())
    }

    #[inline(always)]
    fn refill(&mut self) {
        let b = self.next_byte;
        let mut w = [0u8; 4];
        if let Some(src) = self.data.get(b..b + 4) {
            w.copy_from_slice(src);
        } else if b < self.data.len() {
            let n = self.data.len() - b;
            w[..n].copy_from_slice(&self.data[b..]);
        }
        self.next_byte += 4;
        self.value = (self.value << 32) | u32::from_be_bytes(w) as u64;
        self.bits += 32;
    }

    pub fn bit_pos(&self) -> usize {
        self.next_byte * 8 - self.bits as usize
    }
    pub fn set_bit_pos(&mut self, pos: usize) {
        self.next_byte = pos / 8;
        self.value = 0;
        self.bits = 0;
    }
    pub fn data(&self) -> &'a [u8] {
        self.data
    }
    pub fn overrun(&self) -> bool {
        self.next_byte > self.data.len() + 16
    }

    #[inline(always)]
    fn renorm(&mut self) {
        let shift = self.range.leading_zeros() - 23;
        self.range <<= shift;
        if self.bits < shift {
            self.refill();
        }
        self.bits -= shift;
    }

    #[inline(always)]
    pub fn decode_decision(&mut self, ctx_idx: usize) -> u32 {
        let ctx_idx = ctx_idx & (NUM_CTX - 1);
        let s = (self.ctx[ctx_idx] & 127) as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps = RANGE_TAB_LPS[s >> 1][q] as u32;
        let mps_range = self.range - lps;
        let scaled = (mps_range as u64) << self.bits;
        let is_lps = self.value >= scaled;
        self.value -= scaled & (is_lps as u64).wrapping_neg();
        self.range = if is_lps { lps } else { mps_range };
        self.ctx[ctx_idx] = NEXT_STATE[s][is_lps as usize];
        self.renorm();
        (s & 1) as u32 ^ is_lps as u32
    }

    #[inline(always)]
    pub fn decode_bypass(&mut self) -> u32 {
        if self.bits == 0 {
            self.refill();
        }
        self.bits -= 1;
        let scaled = (self.range as u64) << self.bits;
        let one = self.value >= scaled;
        self.value -= scaled & (one as u64).wrapping_neg();
        one as u32
    }

    pub fn decode_terminate(&mut self) -> u32 {
        self.range -= 2;
        let scaled = (self.range as u64) << self.bits;
        if self.value >= scaled {
            1
        } else {
            self.renorm();
            0
        }
    }
}
