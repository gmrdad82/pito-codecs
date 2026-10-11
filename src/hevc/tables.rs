pub use crate::hevc::spec_tables::*;

const fn diag<const L: usize>(n: usize) -> [(u8, u8); L] {
    let mut out = [(0u8, 0u8); L];
    let mut i = 0;
    let mut x: i32 = 0;
    let mut y: i32 = 0;
    while i < L {
        while y >= 0 {
            if (x as usize) < n && (y as usize) < n {
                out[i] = (x as u8, y as u8);
                i += 1;
            }
            y -= 1;
            x += 1;
        }
        y = x;
        x = 0;
    }
    out
}

const fn hv<const L: usize>(n: usize, vertical: bool) -> [(u8, u8); L] {
    let mut out = [(0u8, 0u8); L];
    let mut i = 0;
    while i < L {
        let (a, b) = ((i % n) as u8, (i / n) as u8);
        out[i] = if vertical { (b, a) } else { (a, b) };
        i += 1;
    }
    out
}

pub static SCAN_1: [[(u8, u8); 1]; 3] = [[(0, 0)], [(0, 0)], [(0, 0)]];
pub static SCAN_2: [[(u8, u8); 4]; 3] = [diag::<4>(2), hv::<4>(2, false), hv::<4>(2, true)];
pub static SCAN_4: [[(u8, u8); 16]; 3] = [diag::<16>(4), hv::<16>(4, false), hv::<16>(4, true)];
pub static SCAN_8: [[(u8, u8); 64]; 3] = [diag::<64>(8), hv::<64>(8, false), hv::<64>(8, true)];

#[inline]
pub fn scan_order(log2: u32, scan_idx: usize) -> &'static [(u8, u8)] {
    match log2 {
        0 => &SCAN_1[scan_idx],
        1 => &SCAN_2[scan_idx],
        2 => &SCAN_4[scan_idx],
        _ => &SCAN_8[scan_idx],
    }
}

pub fn scan_diag(log2: u32) -> &'static [(u8, u8)] {
    scan_order(log2, 0)
}

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
