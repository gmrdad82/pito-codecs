use crate::h264::dec::cavlc_tables as vlc;
use crate::h264::dec::mbtypes::CBP_ME;
use crate::h264::dec::tables as dec;

pub const ZIGZAG4: [usize; 16] = widen(dec::ZIGZAG4);
pub const ZIGZAG8: [usize; 64] = widen(dec::ZIGZAG8);
pub const RASTER_TO_BLK4: [usize; 16] = widen(dec::RASTER_TO_BLK4);
pub const BLK4_XY: [(usize, usize); 16] = {
    let mut out = [(0, 0); 16];
    let mut i = 0;
    while i < 16 {
        out[i] = (dec::BLK4_XY[i].0 as usize, dec::BLK4_XY[i].1 as usize);
        i += 1;
    }
    out
};

const fn widen<const N: usize>(t: [u8; N]) -> [usize; N] {
    let mut out = [0; N];
    let mut i = 0;
    while i < N {
        out[i] = t[i] as usize;
        i += 1;
    }
    out
}

pub const NUM_CTX: usize = 460;

pub const COEFF_TOKEN_LEN: [[[u8; 4]; 17]; 3] = coeff_token().0;
pub const COEFF_TOKEN_CODE: [[[u8; 4]; 17]; 3] = coeff_token().1;
pub const COEFF_TOKEN_CDC_LEN: [[u8; 4]; 5] = chroma_dc_token().0;
pub const COEFF_TOKEN_CDC_CODE: [[u8; 4]; 5] = chroma_dc_token().1;
pub const TOTAL_ZEROS: [[(u8, u8); 16]; 15] = rows(&vlc::TOTAL_ZEROS_4X4);
pub const TOTAL_ZEROS_CDC: [[(u8, u8); 4]; 3] = rows(&vlc::TOTAL_ZEROS_2X2);
pub const RUN_BEFORE: [[(u8, u8); 15]; 7] = rows(&vlc::RUN_BEFORE);

const fn code(s: &str) -> (u8, u8) {
    let b = s.as_bytes();
    let mut v = 0u8;
    let mut i = 0;
    while i < b.len() {
        v = (v << 1) | (b[i] - b'0');
        i += 1;
    }
    (b.len() as u8, v)
}

const fn rows<const N: usize, const R: usize>(t: &[[&str; N]; R]) -> [[(u8, u8); N]; R] {
    let mut out = [[(0, 0); N]; R];
    let mut r = 0;
    while r < R {
        let mut i = 0;
        while i < N {
            out[r][i] = code(t[r][i]);
            i += 1;
        }
        r += 1;
    }
    out
}

type Token = ([[[u8; 4]; 17]; 3], [[[u8; 4]; 17]; 3]);

const fn coeff_token() -> Token {
    let mut len = [[[0u8; 4]; 17]; 3];
    let mut val = [[[0u8; 4]; 17]; 3];
    let mut k = 0;
    while k < vlc::COEFF_TOKEN.len() {
        let (t1, total, codes) = vlc::COEFF_TOKEN[k];
        let mut c = 0;
        while c < 3 {
            let (l, v) = code(codes[c]);
            len[c][total as usize][t1 as usize] = l;
            val[c][total as usize][t1 as usize] = v;
            c += 1;
        }
        k += 1;
    }
    (len, val)
}

const fn chroma_dc_token() -> ([[u8; 4]; 5], [[u8; 4]; 5]) {
    let mut len = [[0u8; 4]; 5];
    let mut val = [[0u8; 4]; 5];
    let mut k = 0;
    while k < vlc::COEFF_TOKEN.len() {
        let (t1, total, codes) = vlc::COEFF_TOKEN[k];
        if total <= 4 {
            let (l, v) = code(codes[4]);
            len[total as usize][t1 as usize] = l;
            val[total as usize][t1 as usize] = v;
        }
        k += 1;
    }
    (len, val)
}

pub const CBP_INTRA_TO_CODE: [u8; 48] = invert_cbp(0);
pub const CBP_INTER_TO_CODE: [u8; 48] = invert_cbp(1);

const fn invert_cbp(column: usize) -> [u8; 48] {
    let mut out = [0u8; 48];
    let mut i = 0;
    while i < 48 {
        let cbp = if column == 0 {
            CBP_ME[i].0
        } else {
            CBP_ME[i].1
        };
        out[cbp as usize] = i as u8;
        i += 1;
    }
    out
}

pub const QUANT4: [[i32; 3]; 6] = [
    [13107, 5243, 8066],
    [11916, 4660, 7490],
    [10082, 4194, 6554],
    [9362, 3647, 5825],
    [8192, 3355, 5243],
    [7282, 2893, 4559],
];

#[inline]
pub fn pos_class4(i: usize) -> usize {
    let (x, y) = (i & 3, i >> 2);
    if x % 2 == 0 && y % 2 == 0 {
        0
    } else if x % 2 == 1 && y % 2 == 1 {
        1
    } else {
        2
    }
}

pub fn chroma_qp(qpi: i32) -> u8 {
    dec::QPC_TABLE[qpi.clamp(0, 51) as usize]
}
