use crate::h264::dec::cabac_tables::CABAC_INIT_MN;
use crate::syntax::cabac::{NEXT_STATE, RANGE_TAB_LPS, ctx_state};
use crate::syntax::error::{Result, ensure};

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
            *c = ctx_state(m as i32, n as i32, qp);
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
