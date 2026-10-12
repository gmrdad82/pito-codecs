#![allow(clippy::too_many_arguments, clippy::needless_range_loop)]

pub mod dist;
pub mod halfpel;
pub mod inter;
pub mod intra;
#[cfg(target_arch = "x86_64")]
mod mc_avx2;
pub mod mode;
pub mod motion;
pub mod mvcost;
#[cfg(test)]
mod tests;

pub use crate::vp9::tables::{
    ALTREF_FRAME, BLOCK_8X8, BLOCK_64X64, COMPANDED_MVREF_THRESH, D45_PRED, D63_PRED, D117_PRED,
    D135_PRED, D153_PRED, D207_PRED, DC_PRED, GOLDEN_FRAME, H_PRED, INTRA_FRAME, LAST_FRAME,
    NEARESTMV, NEARMV, NEWMV, TM_PRED, V_PRED, ZEROMV,
};

pub const NONE_FRAME: i8 = -1;

pub const EIGHTTAP: u8 = 0;
pub const EIGHTTAP_SMOOTH: u8 = 1;
pub const EIGHTTAP_SHARP: u8 = 2;
pub const BILINEAR: u8 = 3;

pub const BLOCK_4X4: u8 = 0;

pub const BLOCK_W: [usize; 13] = [4, 4, 8, 8, 8, 16, 16, 16, 32, 32, 32, 64, 64];
pub const BLOCK_H: [usize; 13] = [4, 8, 4, 8, 16, 8, 16, 32, 16, 32, 64, 32, 64];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };

    pub const fn new(row: i16, col: i16) -> Self {
        Mv { row, col }
    }

    pub fn use_hp(self) -> bool {
        (self.row as i32).abs() >> 3 < COMPANDED_MVREF_THRESH
            && (self.col as i32).abs() >> 3 < COMPANDED_MVREF_THRESH
    }

    pub fn lower_precision(self, allow_hp: bool) -> Mv {
        if allow_hp && self.use_hp() {
            return self;
        }
        let f = |v: i16| if v & 1 != 0 { v - v.signum() } else { v };
        Mv::new(f(self.row), f(self.col))
    }
}

#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub origin: usize,
    pub width: usize,
    pub height: usize,
    pub border: usize,
}

impl<'a> Plane<'a> {
    #[inline(always)]
    pub fn at(&self, x: isize, y: isize) -> usize {
        (self.origin as isize + y * self.stride as isize + x) as usize
    }

    #[inline(always)]
    pub fn inside(&self, x: isize, y: isize, w: usize, h: usize) -> bool {
        let b = self.border as isize;
        x >= -b
            && y >= -b
            && x + w as isize <= self.width as isize + b
            && y + h as isize <= self.height as isize + b
    }
}

pub fn extend_borders(
    data: &mut [u8],
    stride: usize,
    origin: usize,
    width: usize,
    height: usize,
    border: usize,
) {
    for y in 0..height {
        let row = origin + y * stride;
        let (l, r) = (data[row], data[row + width - 1]);
        data[row - border..row].fill(l);
        data[row + width..row + width + border].fill(r);
    }
    let first = origin - border;
    let span = width + 2 * border;
    for y in 1..=border {
        data.copy_within(first..first + span, first - y * stride);
        let last = first + (height - 1) * stride;
        data.copy_within(last..last + span, last + y * stride);
    }
}
