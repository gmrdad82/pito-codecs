use super::analysis::sym_bits;
use crate::opus::silk::lpc::nlsf_decode;
use crate::opus::silk::tables::{
    NLSF_CB_NBMB, NLSF_CB_WB, NLSF_EXT, NLSF_PRED_NBMB, NLSF_PRED_WB, NLSF_PSEL_NBMB, NLSF_PSEL_WB,
    NLSF_SEL_NBMB, NLSF_SEL_WB, NLSF_STAGE1, NLSF_STAGE2,
};

const SURVIVORS: usize = 4;

fn stage2_bits(cb: usize, v: i32) -> f32 {
    let t = v + 4;
    if t <= 0 {
        sym_bits(NLSF_STAGE2[cb], 0) + sym_bits(&NLSF_EXT, (-t) as usize)
    } else if t >= 8 {
        sym_bits(NLSF_STAGE2[cb], 8) + sym_bits(&NLSF_EXT, (t - 8) as usize)
    } else {
        sym_bits(NLSF_STAGE2[cb], t as usize)
    }
}

fn dequant(v: i32, qstep: i32) -> i32 {
    let mut x = v << 10;
    if x > 0 {
        x -= 102;
    } else if x < 0 {
        x += 102;
    }
    ((x as i64 * qstep as i64) >> 16) as i32
}

pub(crate) fn quantize(
    target: &[i32],
    w: &[f32],
    signal_type: usize,
    lambda: f32,
    indices: &mut [i32; 17],
) -> [i32; 16] {
    let d = target.len();
    let wb = d == 16;
    let s1 = (signal_type >> 1) + if wb { 2 } else { 0 };
    let qstep = if wb { 9830 } else { 11796 };
    let mut cand = [(f32::MAX, 0usize); SURVIVORS];
    for i1 in 0..32 {
        let cb: &[i32] = if wb {
            &NLSF_CB_WB[i1]
        } else {
            &NLSF_CB_NBMB[i1]
        };
        let mut err = 0f32;
        for k in 0..d {
            let diff = (target[k] - (cb[k] << 7)) as f32 / 32768.0;
            err += w[k] * diff * diff;
        }
        if err < cand[SURVIVORS - 1].0 {
            cand[SURVIVORS - 1] = (err, i1);
            cand.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
    }
    let mut best_cost = f32::MAX;
    let mut best = [0i32; 17];
    let mut tmp = [0i32; 17];
    for &(_, i1) in cand.iter() {
        let cb: &[i32] = if wb {
            &NLSF_CB_WB[i1]
        } else {
            &NLSF_CB_NBMB[i1]
        };
        tmp[0] = i1 as i32;
        let mut bits = sym_bits(NLSF_STAGE1[s1], i1);
        let mut out_next = 0i32;
        for i in (0..d).rev() {
            let prev = if i == 0 { 0 } else { cb[i - 1] };
            let next = if i + 1 == d { 256 } else { cb[i + 1] };
            let wq = (1024.0 / (cb[i] - prev).max(1) as f32
                + 1024.0 / (next - cb[i]).max(1) as f32)
                .min(32767.0);
            let w_q9 = (wq * 65536.0).sqrt();
            let tgt = (target[i] - (cb[i] << 7)) as f32 * w_q9 / 16384.0;
            let pred = if i + 1 < d {
                let sel = if wb {
                    NLSF_PSEL_WB[i1][i]
                } else {
                    NLSF_PSEL_NBMB[i1][i]
                } as usize;
                let pw = if wb {
                    NLSF_PRED_WB[sel][i]
                } else {
                    NLSF_PRED_NBMB[sel][i]
                };
                (out_next * pw) >> 8
            } else {
                0
            };
            let cbi = if wb {
                8 + NLSF_SEL_WB[i1][i] as usize
            } else {
                NLSF_SEL_NBMB[i1][i] as usize
            };
            let center = ((tgt - pred as f32) * 65536.0 / (qstep as f32 * 1024.0)).round() as i32;
            let mut bc = f32::MAX;
            let mut bv = 0;
            let mut bo = 0;
            for v in (center - 2).max(-10)..=(center + 2).min(10) {
                let o = pred + dequant(v, qstep);
                let e = (o as f32 - tgt) / 1024.0;
                let c = e * e + lambda * stage2_bits(cbi, v);
                if c < bc {
                    bc = c;
                    bv = v;
                    bo = o;
                }
            }
            if bc == f32::MAX {
                let v = center.clamp(-10, 10);
                bv = v;
                bo = pred + dequant(v, qstep);
            }
            tmp[i + 1] = bv;
            bits += stage2_bits(cbi, bv);
            out_next = bo;
        }
        let dec = nlsf_decode(&tmp, wb);
        let mut err = 0f32;
        for k in 0..d {
            let diff = (dec[k] - target[k]) as f32 / 32768.0;
            err += w[k] * diff * diff;
        }
        let cost = err + lambda * bits;
        if cost < best_cost {
            best_cost = cost;
            best = tmp;
        }
    }
    *indices = best;
    nlsf_decode(indices, wb)
}
