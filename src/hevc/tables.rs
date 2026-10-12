pub use crate::hevc::spec_tables::*;

pub use crate::syntax::hevc::tables::*;

#[inline]
pub fn qpc_420(qpi: i32) -> i32 {
    if qpi < 30 {
        qpi
    } else if qpi > 43 {
        qpi - 6
    } else {
        QPC_30_43[(qpi - 30) as usize] as i32
    }
}

pub static CTX_IDX_MAP: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

pub static LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

pub static DST_MATRIX: [[i8; 4]; 4] = [
    [29, 55, 74, 84],
    [74, 74, 0, -74],
    [84, -29, -74, 55],
    [55, -84, 74, -29],
];
