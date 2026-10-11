use std::f64::consts::PI;

pub(crate) const MAX_ORDER: usize = 16;
const MAX_SEG: usize = 1024;
const MIN_INV_GAIN: f64 = 1.0 / 10000.0;

pub(crate) fn burg(x: &[f32], order: usize, a: &mut [f32]) -> f64 {
    let n = x.len().min(MAX_SEG);
    let mut f = [0f64; MAX_SEG];
    let mut b = [0f64; MAX_SEG];
    for i in 0..n {
        f[i] = x[i] as f64;
        b[i] = x[i] as f64;
    }
    let e0: f64 = f[..n].iter().map(|v| v * v).sum::<f64>() + 1e-9;
    let mut c = [0f64; MAX_ORDER];
    let mut err = e0;
    let cond = e0 * 1e-5;
    for m in 0..order {
        let mut num = 0.0;
        let mut den = cond;
        for i in m + 1..n {
            num += f[i] * b[i - 1];
            den += f[i] * f[i] + b[i - 1] * b[i - 1];
        }
        let mut k = -2.0 * num / den;
        let mut last = false;
        if err * (1.0 - k * k) < e0 * MIN_INV_GAIN {
            let r = (1.0 - e0 * MIN_INV_GAIN / err).max(0.0).sqrt();
            k = if k < 0.0 { -r } else { r };
            last = true;
        }
        let prev = c;
        for j in 0..m {
            c[j] = prev[j] + k * prev[m - 1 - j];
        }
        c[m] = k;
        for i in (m + 1..n).rev() {
            let fi = f[i] + k * b[i - 1];
            let bi = b[i - 1] + k * f[i];
            f[i] = fi;
            b[i] = bi;
        }
        err *= 1.0 - k * k;
        if last {
            break;
        }
    }
    for j in 0..order {
        a[j] = -c[j] as f32;
    }
    err / e0
}

fn pq(a: &[f32], w: f64) -> (f64, f64) {
    let d = a.len();
    let (s, c) = w.sin_cos();
    let mut zr = 1.0;
    let mut zi = 0.0;
    let mut ar = 1.0;
    let mut ai = 0.0;
    for &aj in a {
        let nr = zr * c + zi * s;
        let ni = zi * c - zr * s;
        zr = nr;
        zi = ni;
        ar -= aj as f64 * zr;
        ai -= aj as f64 * zi;
    }
    let phi = w * (d as f64 + 1.0) / 2.0;
    let (ps, pc) = phi.sin_cos();
    (ar * pc - ai * ps, ar * ps + ai * pc)
}

pub(crate) fn a2nlsf(a: &mut [f32], out: &mut [i32]) -> bool {
    let d = a.len();
    for _ in 0..16 {
        if a2nlsf_once(a, out) {
            return true;
        }
        let mut g = 0.97f32;
        for v in a.iter_mut() {
            *v *= g;
            g *= 0.97;
        }
    }
    for (k, o) in out.iter_mut().enumerate().take(d) {
        *o = ((k + 1) * 32768 / (d + 1)) as i32;
    }
    false
}

fn a2nlsf_once(a: &[f32], out: &mut [i32]) -> bool {
    const GRID: usize = 384;
    let d = a.len();
    let mut roots = [0f64; MAX_ORDER];
    let mut found = 0;
    let lo = PI / (8.0 * GRID as f64);
    let hi = PI - lo;
    let step = (hi - lo) / GRID as f64;
    let mut w0 = lo;
    let (mut p0, mut q0) = pq(a, w0);
    for g in 1..=GRID {
        let w1 = lo + step * g as f64;
        let (p1, q1) = pq(a, w1);
        for (sel, v0, v1) in [(0, p0, p1), (1, q0, q1)] {
            if (v0 <= 0.0) != (v1 <= 0.0) && found < d {
                let (mut l, mut r, mut fl) = (w0, w1, v0);
                for _ in 0..20 {
                    let m = 0.5 * (l + r);
                    let (pm, qm) = pq(a, m);
                    let fm = if sel == 0 { pm } else { qm };
                    if (fm <= 0.0) == (fl <= 0.0) {
                        l = m;
                        fl = fm;
                    } else {
                        r = m;
                    }
                }
                roots[found] = 0.5 * (l + r);
                found += 1;
            }
        }
        w0 = w1;
        p0 = p1;
        q0 = q1;
    }
    if found != d {
        return false;
    }
    roots[..d].sort_by(|x, y| x.total_cmp(y));
    for k in 0..d {
        out[k] = ((roots[k] / PI * 32768.0).round() as i32).clamp(1, 32767);
    }
    true
}

pub(crate) fn nlsf_weights(nlsf: &[i32], w: &mut [f32]) {
    let d = nlsf.len();
    for k in 0..d {
        let prev = if k == 0 { 0 } else { nlsf[k - 1] };
        let next = if k + 1 == d { 32768 } else { nlsf[k + 1] };
        let l = ((nlsf[k] - prev).max(64)) as f32 / 32768.0;
        let r = ((next - nlsf[k]).max(64)) as f32 / 32768.0;
        w[k] = 1.0 / l + 1.0 / r;
    }
}

pub(crate) fn a_q12_to_f32(a_q12: &[i32], out: &mut [f32]) {
    for (o, &v) in out.iter_mut().zip(a_q12) {
        *o = v as f32 / 4096.0;
    }
}

pub(crate) fn lpc_residual(x: &[f32], a: &[f32], start: usize, out: &mut [f32]) {
    let d = a.len();
    for (i, o) in out.iter_mut().enumerate() {
        let n = start + i;
        let mut p = 0f32;
        for j in 0..d {
            p += a[j] * x[n - 1 - j];
        }
        *o = x[n] - p;
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let ra = ca.remainder();
    let rb = cb.remainder();
    for (x, y) in ca.zip(cb) {
        for j in 0..8 {
            acc[j] += x[j] * y[j];
        }
    }
    let mut s = acc.iter().sum::<f32>();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

pub(crate) struct Pitch {
    pub lag_index: i32,
    pub contour: usize,
    pub lags: [i32; 4],
    pub corr: f32,
}

pub(crate) fn pitch_search(
    e: &[f32],
    start: usize,
    fs_khz: usize,
    nb_subfr: usize,
    prev_lag: i32,
    ncorr: &mut [[f32; 280]; 4],
) -> Pitch {
    let sub = 5 * fs_khz;
    let min_lag = 2 * fs_khz as i32;
    let max_lag = 18 * fs_khz as i32;
    let nl = (max_lag - min_lag + 1) as usize;
    for k in 0..nb_subfr {
        let s = start + k * sub;
        let t = &e[s..s + sub];
        let et = dot(t, t) as f64 + 1.0;
        let first = s - min_lag as usize;
        let mut eb = dot(&e[first..first + sub], &e[first..first + sub]) as f64;
        for li in 0..nl {
            let lag = min_lag as usize + li;
            let b = &e[s - lag..s - lag + sub];
            if li > 0 {
                let out = e[s - lag + sub] as f64;
                let inn = e[s - lag] as f64;
                eb += inn * inn - out * out;
                if eb < 0.0 {
                    eb = dot(b, b) as f64;
                }
            }
            let c = dot(t, b) as f64;
            ncorr[k][li] = (c / (et * eb.max(0.0) + 1.0).sqrt()) as f32;
        }
    }
    let (cb, ncb): (&[[i32; 4]], usize) = match (fs_khz, nb_subfr) {
        (8, 2) => (&CB_NB_10_4, 3),
        (8, _) => (&CB_NB_20_4, 11),
        (_, 2) => (&CB_WB_10_4, 12),
        _ => (&CB_WB_20_4, 34),
    };
    let mut best = Pitch {
        lag_index: 0,
        contour: 0,
        lags: [min_lag; 4],
        corr: -1.0,
    };
    let mut best_score = f32::MIN;
    let log_span = ((max_lag as f32) / (min_lag as f32)).log2();
    for li in 0..nl.min(256) {
        let lag = min_lag + li as i32;
        let bias = 0.12 * (lag as f32 / min_lag as f32).log2() / log_span;
        let near = if prev_lag > 0 && (lag - prev_lag).abs() <= fs_khz as i32 / 2 {
            0.04
        } else {
            0.0
        };
        for (c, offs) in cb.iter().enumerate().take(ncb) {
            let mut s = 0f32;
            for k in 0..nb_subfr {
                let l = (lag + offs[k]).clamp(min_lag, max_lag);
                s += ncorr[k][(l - min_lag) as usize];
            }
            let avg = s / nb_subfr as f32;
            let score = avg - bias + near;
            if score > best_score {
                best_score = score;
                best.lag_index = li as i32;
                best.contour = c;
                best.corr = avg;
                for k in 0..nb_subfr {
                    best.lags[k] = (lag + offs[k]).clamp(min_lag, max_lag);
                }
            }
        }
    }
    best
}

use crate::opus::silk::tables::{CB_NB_10, CB_NB_20, CB_WB_10, CB_WB_20};

const fn widen2<const N: usize>(t: &[[i32; 2]; N]) -> [[i32; 4]; N] {
    let mut out = [[0; 4]; N];
    let mut i = 0;
    while i < N {
        out[i][0] = t[i][0];
        out[i][1] = t[i][1];
        i += 1;
    }
    out
}

const CB_NB_10_4: [[i32; 4]; 3] = widen2(&CB_NB_10);
const CB_NB_20_4: [[i32; 4]; 11] = CB_NB_20;
const CB_WB_10_4: [[i32; 4]; 12] = widen2(&CB_WB_10);
const CB_WB_20_4: [[i32; 4]; 34] = CB_WB_20;

pub(crate) struct Ltp {
    pub per_index: usize,
    pub ltp_index: [usize; 4],
    pub err: [f32; 4],
    pub energy: [f32; 4],
}

pub(crate) fn ltp_search(
    e: &[f32],
    start: usize,
    sub: usize,
    nb_subfr: usize,
    lags: &[i32; 4],
    hist_scale: f32,
    bits_per_log2: f32,
) -> Ltp {
    use crate::opus::silk::tables::{
        LTP_FILTER, LTP_PERIODICITY, LTP_TAPS_0, LTP_TAPS_1, LTP_TAPS_2,
    };
    let mut rr = [[[0f32; 5]; 5]; 4];
    let mut rx = [[0f32; 5]; 4];
    let mut en = [0f32; 4];
    let mut col = [[0f32; 80]; 5];
    for k in 0..nb_subfr {
        let s = start + k * sub;
        let lag = lags[k] as usize;
        for (j, cj) in col.iter_mut().enumerate() {
            for i in 0..sub {
                let p = s + i + 2 - lag - j;
                let v = e[p];
                cj[i] = if p < start { v * hist_scale } else { v };
            }
        }
        let t = &e[s..s + sub];
        en[k] = dot(t, t);
        for j in 0..5 {
            rx[k][j] = dot(t, &col[j][..sub]);
            for l in j..5 {
                let v = dot(&col[j][..sub], &col[l][..sub]);
                rr[k][j][l] = v;
                rr[k][l][j] = v;
            }
        }
    }
    let books: [&[[i32; 5]]; 3] = [&LTP_TAPS_0, &LTP_TAPS_1, &LTP_TAPS_2];
    let mut best = Ltp {
        per_index: 0,
        ltp_index: [0; 4],
        err: en,
        energy: en,
    };
    let mut best_cost = f32::MAX;
    for (pi, book) in books.iter().enumerate() {
        let mut cost = sym_bits(&LTP_PERIODICITY, pi);
        let mut idx = [0usize; 4];
        let mut errs = [0f32; 4];
        for k in 0..nb_subfr {
            let floor = en[k] * 1e-3 + 1.0;
            let mut bk = f32::MAX;
            for (ci, taps) in book.iter().enumerate() {
                let mut b = [0f32; 5];
                for j in 0..5 {
                    b[j] = taps[j] as f32 / 128.0;
                }
                let mut err = en[k];
                for j in 0..5 {
                    err -= 2.0 * b[j] * rx[k][j];
                    for l in 0..5 {
                        err += b[j] * rr[k][j][l] * b[l];
                    }
                }
                let err = err.max(0.0);
                let c = bits_per_log2 * ((err + floor) / (en[k] + floor)).log2()
                    + sym_bits(LTP_FILTER[pi], ci);
                if c < bk {
                    bk = c;
                    idx[k] = ci;
                    errs[k] = err;
                }
            }
            cost += bk;
        }
        if cost < best_cost {
            best_cost = cost;
            best.per_index = pi;
            best.ltp_index = idx;
            best.err = errs;
        }
    }
    best
}

pub(crate) fn sym_bits(icdf: &[u8], s: usize) -> f32 {
    let hi = if s == 0 { 256 } else { icdf[s - 1] as u32 };
    let p = hi - icdf[s] as u32;
    8.0 - (p.max(1) as f32).log2()
}
