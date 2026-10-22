use crate::syntax::bits::BitWriter;

use crate::h264::enc::mbinfo::{MbKind, Part};
use crate::h264::enc::nal::SliceType;
use crate::h264::enc::syntax::{MbCode, NbCtx};
use crate::h264::enc::tables::{
    BLK4_XY, CBP_INTER_TO_CODE, CBP_INTRA_TO_CODE, COEFF_TOKEN_CDC_CODE, COEFF_TOKEN_CDC_LEN,
    COEFF_TOKEN_CODE, COEFF_TOKEN_LEN, RUN_BEFORE, TOTAL_ZEROS, TOTAL_ZEROS_CDC,
};

pub const MAX_LEVEL: i32 = 2047;

pub fn write_residual(w: &mut BitWriter, coeffs: &[i16], nc: i32) -> u8 {
    let max = coeffs.len();
    let mut levels = [0i32; 16];
    let mut pos = [0usize; 16];
    let mut total = 0usize;
    for i in (0..max).rev() {
        if coeffs[i] != 0 {
            levels[total] = coeffs[i] as i32;
            pos[total] = i;
            total += 1;
        }
    }
    let mut t1 = 0usize;
    while t1 < total && t1 < 3 && levels[t1].abs() == 1 {
        t1 += 1;
    }
    if nc == -1 {
        w.write_bits(
            COEFF_TOKEN_CDC_CODE[total][t1] as u32,
            COEFF_TOKEN_CDC_LEN[total][t1] as u32,
        );
    } else if nc >= 8 {
        if total == 0 {
            w.write_bits(3, 6);
        } else {
            w.write_bits((((total - 1) << 2) | t1) as u32, 6);
        }
    } else {
        let t = if nc < 2 {
            0
        } else if nc < 4 {
            1
        } else {
            2
        };
        w.write_bits(
            COEFF_TOKEN_CODE[t][total][t1] as u32,
            COEFF_TOKEN_LEN[t][total][t1] as u32,
        );
    }
    if total == 0 {
        return 0;
    }
    let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };
    for i in 0..total {
        let l = levels[i];
        if i < t1 {
            w.write_bit(l < 0);
            continue;
        }
        let mut code = if l > 0 { 2 * l - 2 } else { -2 * l - 1 };
        if i == t1 && t1 < 3 {
            code -= 2;
        }
        let code = code as u32;
        if suffix_len == 0 {
            if code < 14 {
                w.write_bits(1, code + 1);
            } else if code < 30 {
                w.write_bits(1, 15);
                w.write_bits(code - 14, 4);
            } else {
                write_escape(w, code - 30);
            }
        } else if code < (15 << suffix_len) {
            w.write_bits(1, (code >> suffix_len) + 1);
            w.write_bits(code & ((1 << suffix_len) - 1), suffix_len);
        } else {
            write_escape(w, code - (15 << suffix_len));
        }
        if suffix_len == 0 {
            suffix_len = 1;
        }
        if l.unsigned_abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
            suffix_len += 1;
        }
    }
    let total_zeros = pos[0] + 1 - total;
    if total < max {
        let (len, code) = if nc == -1 {
            TOTAL_ZEROS_CDC[total - 1][total_zeros]
        } else {
            TOTAL_ZEROS[total - 1][total_zeros]
        };
        w.write_bits(code as u32, len as u32);
    }
    let mut zeros_left = total_zeros;
    for i in 0..total - 1 {
        if zeros_left == 0 {
            break;
        }
        let run = pos[i] - pos[i + 1] - 1;
        let (len, code) = RUN_BEFORE[zeros_left.min(7) - 1][run];
        w.write_bits(code as u32, len as u32);
        zeros_left -= run;
    }
    total as u8
}

fn write_escape(w: &mut BitWriter, rem: u32) {
    if rem < 4096 {
        w.write_bits(1, 16);
        w.write_bits(rem, 12);
    } else {
        let mut p = 16;
        while rem + 4096 >= (1 << (p - 3)) * 2 {
            p += 1;
        }
        w.write_bits(0, p);
        w.write_bit(true);
        w.write_bits(rem + 4096 - (1 << (p - 3)), p - 3);
    }
}

fn nc_of(a: Option<u8>, b: Option<u8>) -> i32 {
    match (a, b) {
        (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
        (Some(a), None) => a as i32,
        (None, Some(b)) => b as i32,
        _ => 0,
    }
}

pub fn write_mb(w: &mut BitWriter, nb: &NbCtx, c: &MbCode) {
    let intra_base = match nb.slice_type {
        SliceType::I => 0,
        SliceType::P => 5,
        SliceType::B => 23,
    };
    match c.kind {
        MbKind::I4x4 | MbKind::I8x8 => w.write_ue(intra_base),
        MbKind::I16x16 => {
            let t = 1
                + c.i16_mode as u32
                + 4 * (c.cbp >> 4) as u32
                + if c.cbp & 15 != 0 { 12 } else { 0 };
            w.write_ue(intra_base + t);
        }
        MbKind::PInter => w.write_ue(match c.part {
            Part::P16x16 => 0,
            Part::P16x8 => 1,
            Part::P8x16 => 2,
            Part::P8x8 => 3,
        }),
        MbKind::BDirect | MbKind::BInter => w.write_ue(c.b_type_number()),
        _ => debug_assert!(false, "write_mb on a skipped macroblock"),
    }
    if c.kind == MbKind::PInter && c.part == Part::P8x8 {
        for _ in 0..4 {
            w.write_ue(0);
        }
    }
    if c.kind.is_inxn() {
        if nb.t8x8_mode {
            w.write_bit(c.t8x8);
        }
        let n = if c.t8x8 { 4 } else { 16 };
        for i in 0..n {
            let p = c.ipred[i];
            if p < 0 {
                w.write_bit(true);
            } else {
                w.write_bit(false);
                w.write_bits(p as u32, 3);
            }
        }
    }
    if c.kind.is_intra() {
        w.write_ue(c.chroma_mode as u32);
    }
    if matches!(c.kind, MbKind::PInter | MbKind::BInter) {
        for list in 0..2 {
            for p in 0..c.part.count() {
                if c.uses_list(p, list) {
                    w.write_se(c.mvd[list][p].x as i32);
                    w.write_se(c.mvd[list][p].y as i32);
                }
            }
        }
    }
    if c.kind != MbKind::I16x16 {
        let code = if c.kind.is_intra() {
            CBP_INTRA_TO_CODE[c.cbp as usize]
        } else {
            CBP_INTER_TO_CODE[c.cbp as usize]
        };
        let cbp_idx = (c.cbp & 15) | ((c.cbp >> 4) << 4);
        debug_assert_eq!(cbp_idx, c.cbp);
        w.write_ue(code as u32);
        if c.cbp & 15 != 0 && nb.t8x8_mode && !c.kind.is_inxn() {
            w.write_bit(c.t8x8);
        }
    }
    if c.cbp == 0 && c.kind != MbKind::I16x16 {
        return;
    }
    w.write_se(c.qp_delta);
    let luma_nc = |r: usize| nc_of(nb.luma_nnz(r, true), nb.luma_nnz(r, false));
    if c.kind == MbKind::I16x16 {
        write_residual(w, &c.luma_dc, luma_nc(0));
        if c.cbp & 15 != 0 {
            for blk in 0..16 {
                let (x, y) = BLK4_XY[blk];
                write_residual(w, &c.luma4[blk][1..16], luma_nc(y * 4 + x));
            }
        }
    } else {
        for b8 in 0..4 {
            if (c.cbp >> b8) & 1 == 0 {
                continue;
            }
            for i in 0..4 {
                let blk = b8 * 4 + i;
                let (x, y) = BLK4_XY[blk];
                if c.t8x8 {
                    let mut sub = [0i16; 16];
                    for k in 0..16 {
                        sub[k] = c.luma8[b8][4 * k + i];
                    }
                    write_residual(w, &sub, luma_nc(y * 4 + x));
                } else {
                    write_residual(w, &c.luma4[blk], luma_nc(y * 4 + x));
                }
            }
        }
    }
    let chroma = c.cbp >> 4;
    if chroma != 0 {
        for comp in 0..2 {
            write_residual(w, &c.cdc[comp], -1);
        }
    }
    if chroma == 2 {
        for comp in 0..2 {
            for b in 0..4 {
                let ncv = nc_of(nb.chroma_nnz(comp, b, true), nb.chroma_nnz(comp, b, false));
                write_residual(w, &c.cac[comp * 4 + b][1..16], ncv);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264::dec::cavlc::residual_block;
    use crate::syntax::bits::BitReader;

    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    fn block(rng: &mut Lcg, len: usize, style: usize) -> Vec<i16> {
        let mut c = vec![0i16; len];
        for v in c.iter_mut() {
            *v = match style {
                0 => 0,
                1 => [0, 0, 0, 1, -1][rng.next() as usize % 5],
                2 => [0, 1, -1, 2, -3, 5][rng.next() as usize % 6],
                3 => (rng.next() % 41) as i16 - 20,
                4 => (rng.next() % (2 * MAX_LEVEL as u32 + 1)) as i16 - MAX_LEVEL as i16,
                5 => [0, 0, 3000, -3000, 1, -1, 4095][rng.next() as usize % 7],
                _ => 1,
            };
        }
        c
    }

    fn round_trip(coeffs: &[i16], nc: i32) {
        let mut w = BitWriter::new();
        let total = write_residual(&mut w, coeffs, nc);
        let bits = w.bit_len();
        let mut data = w.finish();
        data.extend_from_slice(&[0; 8]);
        let mut r = BitReader::new(&data);
        let mut out = [0i32; 16];
        let n = coeffs.len();
        let got = residual_block(&mut r, &mut out, 0, n - 1, n, nc)
            .unwrap_or_else(|e| panic!("nc {nc} {coeffs:?}: {e}"));
        assert_eq!(got, total, "nc {nc} {coeffs:?}: total");
        assert_eq!(
            total as usize,
            coeffs.iter().filter(|&&v| v != 0).count(),
            "nc {nc} {coeffs:?}: count"
        );
        assert_eq!(r.position(), bits, "nc {nc} {coeffs:?}: bits read");
        for i in 0..n {
            assert_eq!(out[i], coeffs[i] as i32, "nc {nc} {coeffs:?}: coeff {i}");
        }
    }

    #[test]
    fn residual_blocks_parse_back_in_the_decoder() {
        let mut rng = Lcg(11);
        for nc in [-1, 0, 1, 2, 3, 4, 5, 7, 8, 9, 16] {
            let lens: &[usize] = if nc == -1 { &[4] } else { &[16, 15] };
            for &len in lens {
                for style in 0..7 {
                    for _ in 0..60 {
                        let mut c = block(&mut rng, len, style);
                        if style == 6 {
                            let keep = rng.next() as usize % (len + 1);
                            for v in c.iter_mut().skip(keep) {
                                *v = 0;
                            }
                        }
                        round_trip(&c, nc);
                    }
                }
            }
        }
    }

    #[test]
    fn every_total_and_trailing_ones_pair_round_trips() {
        for nc in [-1, 0, 2, 4, 8] {
            let len = if nc == -1 { 4 } else { 16 };
            for total in 0..=len {
                for t1 in 0..=total.min(3) {
                    for spread in [false, true] {
                        let mut c = vec![0i16; len];
                        let at: Vec<usize> = if spread && total < len {
                            (0..total).map(|k| k * len / total.max(1)).collect()
                        } else {
                            (0..total).collect()
                        };
                        for (k, &p) in at.iter().enumerate() {
                            let from_top = total - 1 - k;
                            c[p] = if from_top < t1 {
                                if k % 2 == 0 { 1 } else { -1 }
                            } else {
                                2 + k as i16
                            };
                        }
                        round_trip(&c, nc);
                    }
                }
            }
        }
    }

    #[test]
    fn escape_codes_cover_every_suffix_length() {
        for nc in [0, 4, 8] {
            for big in [14, 15, 16, 29, 30, 31, 100, 2047, 2048, 4095, 8000, 16000] {
                for sign in [1i16, -1] {
                    let mut c = vec![0i16; 16];
                    c[0] = sign * big;
                    round_trip(&c, nc);
                    let mut many = [sign * big; 16];
                    many[15] = 1;
                    round_trip(&many, nc);
                    let mut rising = [0i16; 16];
                    for (k, v) in rising.iter_mut().enumerate() {
                        *v = sign * (1 + (k as i32 * big as i32 / 16) as i16);
                    }
                    round_trip(&rising, nc);
                }
            }
        }
    }

    #[test]
    fn empty_blocks_cost_a_token_and_return_zero() {
        for (nc, len) in [(-1, 4usize), (0, 16), (3, 15), (6, 16), (12, 16)] {
            let mut w = BitWriter::new();
            assert_eq!(write_residual(&mut w, &vec![0; len], nc), 0);
            assert!(w.bit_len() > 0);
            if nc >= 8 {
                assert_eq!(w.bit_len(), 6);
            }
        }
    }

    #[test]
    fn neighbour_counts_average_round_up() {
        assert_eq!(nc_of(None, None), 0);
        assert_eq!(nc_of(Some(5), None), 5);
        assert_eq!(nc_of(None, Some(7)), 7);
        assert_eq!(nc_of(Some(2), Some(3)), 3);
        assert_eq!(nc_of(Some(16), Some(16)), 16);
        assert_eq!(nc_of(Some(0), Some(1)), 1);
    }
}
