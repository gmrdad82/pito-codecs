use crate::opus::celt::tables::*;

use super::bands::haar1;

const SPREAD_NONE: usize = 0;
const SPREAD_LIGHT: usize = 1;
const SPREAD_NORMAL: usize = 2;
const SPREAD_AGGRESSIVE: usize = 3;

pub struct Tf {
    pub res: [i32; NB_EBANDS],
    pub select: usize,
}

fn l1_at(band: &[f32], bb: usize, change: i32, tmp: &mut [f32]) -> f32 {
    let n = band.len();
    let x = &mut tmp[..n];
    x.copy_from_slice(band);
    let mut bb = bb;
    let mut n_b = n / bb;
    let mut tf_change = change;
    let recombine = tf_change.max(0) as usize;
    for k in 0..recombine {
        haar1(x, n >> k, 1 << k);
    }
    bb >>= recombine;
    n_b <<= recombine;
    while (n_b & 1) == 0 && tf_change < 0 {
        haar1(x, n_b, bb);
        bb <<= 1;
        n_b >>= 1;
        tf_change += 1;
    }
    x.iter().map(|v| v.abs()).sum()
}

#[allow(clippy::too_many_arguments)]
pub fn analyse(
    freq: &[f32],
    n: usize,
    channels: usize,
    lm: usize,
    transient: bool,
    start: usize,
    end: usize,
    lambda: f32,
    tmp: &mut Vec<f32>,
) -> Tf {
    let mut out = Tf {
        res: [transient as i32; NB_EBANDS],
        select: 0,
    };
    if lm == 0 || end <= start {
        return out;
    }
    let mm = 1usize << lm;
    let bb = if transient { mm } else { 1 };
    let ti = 4 * transient as usize;
    let table = &TF_SELECT_TABLE[lm];
    let mut cost = [[[0f32; 2]; 2]; NB_EBANDS];
    tmp.clear();
    tmp.resize(mm * EBANDS[NB_EBANDS] as usize, 0.0);
    for i in start..end {
        let lo = mm * EBANDS[i] as usize;
        let hi = mm * EBANDS[i + 1] as usize;
        let mut seen: [(i32, f32); 4] = [(i32::MIN, 0.0); 4];
        let mut seen_n = 0;
        for sel in 0..2 {
            for r in 0..2 {
                let change = table[ti + 2 * sel + r] as i32;
                let l1 = if let Some(&(_, v)) = seen[..seen_n].iter().find(|s| s.0 == change) {
                    v
                } else {
                    let mut v = 0f32;
                    for ch in 0..channels {
                        let band = &freq[ch * n + lo..ch * n + hi];
                        v += l1_at(band, bb, change, tmp);
                    }
                    seen[seen_n] = (change, v);
                    seen_n += 1;
                    v
                };
                cost[i][sel][r] = l1 * (1.0 + 0.04 * change.unsigned_abs() as f32);
            }
        }
    }
    let mut best = (f32::MAX, 0usize, [0i32; NB_EBANDS]);
    for sel in 0..2 {
        let first_logp = if transient { 2.0 } else { 4.0 };
        let next_logp = if transient { 4.0 } else { 5.0 };
        let mut acc = [
            cost[start][sel][0],
            cost[start][sel][1] + lambda * first_logp,
        ];
        let mut from = [[0u8; 2]; NB_EBANDS];
        for i in start + 1..end {
            let mut next = [0f32; 2];
            for r in 0..2 {
                let stay = acc[r];
                let flip = acc[1 - r] + lambda * next_logp;
                if stay <= flip {
                    next[r] = stay + cost[i][sel][r];
                    from[i][r] = r as u8;
                } else {
                    next[r] = flip + cost[i][sel][r];
                    from[i][r] = (1 - r) as u8;
                }
            }
            acc = next;
        }
        let mut r = if acc[0] <= acc[1] { 0 } else { 1 };
        let total = acc[r] + lambda * sel as f32;
        if total < best.0 {
            let mut res = [0i32; NB_EBANDS];
            for i in (start..end).rev() {
                res[i] = r as i32;
                r = from[i][r] as usize;
            }
            best = (total, sel, res);
        }
    }
    out.select = best.1;
    out.res[start..end].copy_from_slice(&best.2[start..end]);
    out
}

pub fn spread(
    freq: &[f32],
    n: usize,
    channels: usize,
    lm: usize,
    end: usize,
    average: &mut f32,
) -> usize {
    let mm = 1usize << lm;
    let mut score = 0f32;
    let mut weight = 0f32;
    for ch in 0..channels {
        for i in 0..end {
            let lo = mm * EBANDS[i] as usize;
            let hi = mm * EBANDS[i + 1] as usize;
            let w = hi - lo;
            if w < 8 {
                continue;
            }
            let band = &freq[ch * n + lo..ch * n + hi];
            let nf = w as f32;
            let mut t = [0u32; 3];
            for &v in band {
                let e = v * v * nf;
                t[0] += (e < 0.25) as u32;
                t[1] += (e < 0.0625) as u32;
                t[2] += (e < 0.015625) as u32;
            }
            score += (t[0] + t[1] + t[2]) as f32;
            weight += nf;
        }
    }
    if weight == 0.0 {
        return SPREAD_NORMAL;
    }
    let s = score / weight;
    *average = 0.5 * *average + 0.5 * s;
    match *average {
        a if a > 2.1 => SPREAD_NONE,
        a if a > 1.5 => SPREAD_LIGHT,
        a if a > 0.45 => SPREAD_NORMAL,
        _ => SPREAD_AGGRESSIVE,
    }
}
