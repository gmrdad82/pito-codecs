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

pub static DEFAULT_SCALING_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];
pub static DEFAULT_SCALING_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];
