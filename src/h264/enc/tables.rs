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

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix_free(codes: &[(u8, u8)]) {
        let mut kraft = 0.0;
        for (i, &(la, va)) in codes.iter().enumerate() {
            assert!(la > 0 && la <= 16, "code {i} has length {la}");
            kraft += 0.5f64.powi(la as i32);
            for &(lb, vb) in &codes[i + 1..] {
                let l = la.min(lb);
                assert_ne!(
                    va as u32 >> (la - l),
                    vb as u32 >> (lb - l),
                    "{la}:{va:b} and {lb}:{vb:b} share a prefix"
                );
            }
        }
        assert!(kraft <= 1.0 + 1e-12, "Kraft sum {kraft}");
    }

    #[test]
    fn tables_hold_the_standard_codes() {
        let tok = |t: usize, total: usize, t1: usize| {
            (
                COEFF_TOKEN_LEN[t][total][t1],
                COEFF_TOKEN_CODE[t][total][t1],
            )
        };
        assert_eq!(tok(0, 0, 0), code("1"));
        assert_eq!(tok(0, 1, 0), code("000101"));
        assert_eq!(tok(0, 1, 1), code("01"));
        assert_eq!(tok(0, 2, 0), code("00000111"));
        assert_eq!(tok(0, 2, 1), code("000100"));
        assert_eq!(tok(0, 2, 2), code("001"));
        assert_eq!(tok(0, 3, 3), code("00011"));
        assert_eq!(tok(1, 0, 0), code("11"));
        assert_eq!(tok(1, 1, 1), code("10"));
        assert_eq!(tok(1, 2, 2), code("011"));
        assert_eq!(tok(2, 0, 0), code("1111"));
        assert_eq!(tok(2, 1, 1), code("1110"));
        let cdc = |total: usize, t1: usize| {
            (
                COEFF_TOKEN_CDC_LEN[total][t1],
                COEFF_TOKEN_CDC_CODE[total][t1],
            )
        };
        assert_eq!(cdc(0, 0), code("01"));
        assert_eq!(cdc(1, 1), code("1"));
        assert_eq!(cdc(1, 0), code("000111"));
        assert_eq!(TOTAL_ZEROS[0][0], code("1"));
        assert_eq!(TOTAL_ZEROS[0][15], code("000000001"));
        assert_eq!(TOTAL_ZEROS[1][0], code("111"));
        assert_eq!(RUN_BEFORE[0][1], code("0"));
        assert_eq!(RUN_BEFORE[6][0], code("111"));
        assert_eq!(RUN_BEFORE[6][14], code("00000000001"));
        assert_eq!((CBP_INTRA_TO_CODE[47], CBP_INTER_TO_CODE[0]), (0, 0));
        assert_eq!((CBP_INTRA_TO_CODE[31], CBP_INTER_TO_CODE[16]), (1, 1));
        assert_eq!(
            ZIGZAG4,
            [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15]
        );
    }

    #[test]
    fn coeff_tokens_are_prefix_free_per_table() {
        let (len, val) = coeff_token();
        for t in 0..3 {
            let mut codes = Vec::new();
            for total in 0..=16 {
                for t1 in 0..=total.min(3) {
                    codes.push((len[t][total][t1], val[t][total][t1]));
                }
            }
            assert_eq!(codes.len(), 62);
            prefix_free(&codes);
        }
        let (len, val) = chroma_dc_token();
        let mut codes = Vec::new();
        for total in 0..=4 {
            for t1 in 0..=total.min(3) {
                codes.push((len[total][t1], val[total][t1]));
            }
        }
        assert_eq!(codes.len(), 14);
        prefix_free(&codes);
    }

    #[test]
    fn total_zeros_and_run_before_rows_are_prefix_free() {
        for (r, row) in TOTAL_ZEROS.iter().enumerate() {
            prefix_free(&row[..16 - (r + 1) + 1]);
        }
        for (r, row) in TOTAL_ZEROS_CDC.iter().enumerate() {
            prefix_free(&row[..4 - (r + 1) + 1]);
        }
        for (r, row) in RUN_BEFORE.iter().enumerate() {
            let n = if r < 6 { r + 2 } else { 15 };
            prefix_free(&row[..n]);
        }
    }

    #[test]
    fn cbp_maps_are_bijections() {
        for (cbp, &code) in CBP_INTRA_TO_CODE.iter().enumerate() {
            assert_eq!(CBP_ME[code as usize].0 as usize, cbp);
        }
        for (cbp, &code) in CBP_INTER_TO_CODE.iter().enumerate() {
            assert_eq!(CBP_ME[code as usize].1 as usize, cbp);
        }
    }

    #[test]
    fn scan_tables_are_permutations_and_agree() {
        let mut seen = [false; 64];
        for &i in &ZIGZAG8 {
            assert!(!seen[i]);
            seen[i] = true;
        }
        let mut seen = [false; 16];
        for &i in &ZIGZAG4 {
            assert!(!seen[i]);
            seen[i] = true;
        }
        for (blk, &(x, y)) in BLK4_XY.iter().enumerate() {
            assert!(x < 4 && y < 4);
            assert_eq!(RASTER_TO_BLK4[y * 4 + x], blk);
        }
    }

    #[test]
    fn position_classes_and_chroma_qp() {
        let classes: Vec<usize> = (0..16).map(pos_class4).collect();
        assert_eq!(classes, [0, 2, 0, 2, 2, 1, 2, 1, 0, 2, 0, 2, 2, 1, 2, 1]);
        assert_eq!(chroma_qp(-7), chroma_qp(0));
        assert_eq!(chroma_qp(0), 0);
        assert_eq!(chroma_qp(29), 29);
        assert_eq!(chroma_qp(30), 29);
        assert_eq!(chroma_qp(51), 39);
        assert_eq!(chroma_qp(99), 39);
    }
}
