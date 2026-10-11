use crate::opus::celt::rate::MAX_FINE_BITS;
use crate::opus::celt::tables::*;
use crate::opus::range::RangeEncoder;

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

fn laplace_freq1(fs0: u32, decay: u32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    (ft * (16384 - decay)) >> 15
}

pub fn laplace_encode(enc: &mut RangeEncoder, value: &mut i32, fs0: u32, decay: u32) {
    let mut fl = 0u32;
    let mut fs = fs0;
    let mut val = *value;
    if val != 0 {
        let s: i32 = if val < 0 { -1 } else { 0 };
        val = val.abs();
        fl = fs;
        fs = laplace_freq1(fs, decay);
        let mut i = 1;
        while fs > 0 && i < val {
            fs *= 2;
            fl += fs + 2 * LAPLACE_MINP;
            fs = (fs * decay) >> 15;
            i += 1;
        }
        if fs == 0 {
            let ndi_max = (32768 - fl + LAPLACE_MINP - 1) as i32;
            let ndi_max = (ndi_max - s) >> 1;
            let di = (val - i).min(ndi_max - 1);
            fl = (fl as i32 + (2 * di + 1 + s) * LAPLACE_MINP as i32) as u32;
            fs = LAPLACE_MINP.min(32768 - fl);
            *value = (i + di + s) ^ s;
        } else {
            fs += LAPLACE_MINP;
            if s == 0 {
                fl += fs;
            }
        }
    }
    enc.encode_bin(fl, fl + fs, 15);
}

pub struct Coarse<'a> {
    pub start: usize,
    pub end: usize,
    pub channels: usize,
    pub lm: usize,
    pub intra: bool,
    pub max_decay: f32,
    pub budget: i32,
    pub target: &'a [f32],
}

pub fn quant_coarse_energy(
    p: &Coarse,
    old_e: &mut [f32],
    error: &mut [f32],
    enc: &mut RangeEncoder,
) {
    let prob = &E_PROB_MODEL[p.lm][p.intra as usize];
    let (coef, beta) = if p.intra {
        (0.0, BETA_INTRA)
    } else {
        (PRED_COEF[p.lm], BETA_COEF[p.lm])
    };
    let mut prev = [0f32; 2];
    for i in p.start..p.end {
        for ch in 0..p.channels {
            let idx = i + ch * NB_EBANDS;
            let x = p.target[idx];
            let old = old_e[idx].max(-9.0);
            let f = x - coef * old - prev[ch];
            let mut qi = (0.5 + f).floor() as i32;
            let decay_bound = old_e[idx].max(-28.0) - p.max_decay;
            if qi < 0 && x < decay_bound {
                qi += (decay_bound - x) as i32;
                qi = qi.min(0);
            }
            let left = p.budget - enc.tell();
            if left >= 15 {
                let pi = 2 * i.min(20);
                laplace_encode(
                    enc,
                    &mut qi,
                    (prob[pi] as u32) << 7,
                    (prob[pi + 1] as u32) << 6,
                );
            } else if left >= 2 {
                qi = qi.clamp(-1, 1);
                let s = if qi == 0 {
                    0
                } else if qi < 0 {
                    1
                } else {
                    2
                };
                enc.icdf(s, &SMALL_ENERGY_ICDF, 2);
            } else if left >= 1 {
                qi = qi.min(0);
                enc.bit_logp(qi != 0, 1);
            } else {
                qi = -1;
            }
            let q = qi as f32;
            let e = coef * old + prev[ch] + q;
            old_e[idx] = e;
            error[idx] = x - e;
            prev[ch] = prev[ch] + q - beta * q;
        }
    }
}

pub fn quant_fine_energy(
    start: usize,
    end: usize,
    old_e: &mut [f32],
    error: &mut [f32],
    fine_quant: &[i32; NB_EBANDS],
    enc: &mut RangeEncoder,
    c: usize,
) {
    for i in start..end {
        let fq = fine_quant[i];
        if fq <= 0 {
            continue;
        }
        let frac = (1 << fq) as f32;
        for ch in 0..c {
            let idx = i + ch * NB_EBANDS;
            let q2 = (((error[idx] + 0.5) * frac).floor() as i32).clamp(0, (1 << fq) - 1);
            enc.bits(q2 as u32, fq as u32);
            let offset = (q2 as f32 + 0.5) * (1 << (14 - fq)) as f32 / 16384.0 - 0.5;
            old_e[idx] += offset;
            error[idx] -= offset;
        }
    }
}

pub fn quant_energy_finalise(
    start: usize,
    end: usize,
    old_e: &mut [f32],
    error: &mut [f32],
    fine_quant: &[i32; NB_EBANDS],
    fine_priority: &[i32; NB_EBANDS],
    mut bits_left: i32,
    enc: &mut RangeEncoder,
    c: usize,
) {
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= c as i32 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            for ch in 0..c {
                let idx = i + ch * NB_EBANDS;
                let q2 = (error[idx] >= 0.0) as u32;
                enc.bits(q2, 1);
                let offset = (q2 as f32 - 0.5) * (1 << (14 - fine_quant[i] - 1)) as f32 / 16384.0;
                old_e[idx] += offset;
                error[idx] -= offset;
                bits_left -= 1;
            }
            i += 1;
        }
    }
}
