use crate::media::isa::dispatch;
use crate::opus::celt::tables::{COMB_GAINS, OVERLAP};

pub const MAX_PERIOD: usize = 1024;
const MIN_PERIOD: usize = 15;
const TOP_PERIOD: usize = 1022;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pf {
    pub period: usize,
    pub qg: u32,
    pub on: bool,
    pub tapset: usize,
}

impl Pf {
    pub fn gain(&self) -> f32 {
        if self.on {
            0.09375 * (self.qg + 1) as f32
        } else {
            0.0
        }
    }
}

#[derive(Default)]
pub struct Pitch {
    mono: Vec<f32>,
    dec: Vec<f32>,
    corr: Vec<f32>,
    score: Vec<f32>,
}

#[inline]
fn taps(x: &[f32], at: usize, t: usize, g: &[f32; 3]) -> f32 {
    let p = at - t;
    g[0] * x[p] + g[1] * (x[p + 1] + x[p - 1]) + g[2] * (x[p + 2] + x[p - 2])
}

#[inline(always)]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut s = [0f32; 8];
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for l in 0..8 {
            s[l] += x[l] * y[l];
        }
    }
    let mut t = ((s[0] + s[4]) + (s[1] + s[5])) + ((s[2] + s[6]) + (s[3] + s[7]));
    for (x, y) in ra.iter().zip(rb) {
        t += x * y;
    }
    t
}

fn xcorr(x: &[f32], y: &[f32], out: &mut [f32]) {
    let n = x.len();
    let lags = out.len();
    if lags == 0 || y.len() + 1 < n + lags {
        out.fill(0.0);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2_fma() {
        let done = unsafe { simd::xcorr(x, y, out) };
        for (k, o) in out.iter_mut().enumerate().skip(done) {
            *o = dot(x, &y[k..k + n]);
        }
        return;
    }
    for (k, o) in out.iter_mut().enumerate() {
        *o = dot(x, &y[k..k + n]);
    }
}

#[cfg(target_arch = "x86_64")]
mod simd {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn xcorr(x: &[f32], y: &[f32], out: &mut [f32]) -> usize {
        let n = x.len();
        let lags = out.len();
        let mut k = 0;
        unsafe {
            let xp = x.as_ptr();
            let yp = y.as_ptr();
            let op = out.as_mut_ptr();
            while k + 32 <= lags {
                let mut a = [_mm256_setzero_ps(); 4];
                for j in 0..n {
                    let xv = _mm256_set1_ps(*xp.add(j));
                    let p = yp.add(j + k);
                    a[0] = _mm256_fmadd_ps(xv, _mm256_loadu_ps(p), a[0]);
                    a[1] = _mm256_fmadd_ps(xv, _mm256_loadu_ps(p.add(8)), a[1]);
                    a[2] = _mm256_fmadd_ps(xv, _mm256_loadu_ps(p.add(16)), a[2]);
                    a[3] = _mm256_fmadd_ps(xv, _mm256_loadu_ps(p.add(24)), a[3]);
                }
                for (g, v) in a.iter().enumerate() {
                    _mm256_storeu_ps(op.add(k + 8 * g), *v);
                }
                k += 32;
            }
            while k + 8 <= lags {
                let mut a = _mm256_setzero_ps();
                let mut b = _mm256_setzero_ps();
                let mut j = 0;
                while j + 2 <= n {
                    a = _mm256_fmadd_ps(
                        _mm256_set1_ps(*xp.add(j)),
                        _mm256_loadu_ps(yp.add(j + k)),
                        a,
                    );
                    b = _mm256_fmadd_ps(
                        _mm256_set1_ps(*xp.add(j + 1)),
                        _mm256_loadu_ps(yp.add(j + 1 + k)),
                        b,
                    );
                    j += 2;
                }
                if j < n {
                    a = _mm256_fmadd_ps(
                        _mm256_set1_ps(*xp.add(j)),
                        _mm256_loadu_ps(yp.add(j + k)),
                        a,
                    );
                }
                _mm256_storeu_ps(op.add(k), _mm256_add_ps(a, b));
                k += 8;
            }
        }
        k
    }
}

impl Pitch {
    pub fn analyse(
        &mut self,
        raw: &[f32],
        channels: usize,
        n: usize,
        prev: &Pf,
        strength: f32,
    ) -> Pf {
        let len = MAX_PERIOD + n;
        self.mono.clear();
        self.mono.resize(len, 0.0);
        for ch in 0..channels {
            for (m, v) in self.mono.iter_mut().zip(&raw[ch * len..(ch + 1) * len]) {
                *m += v;
            }
        }
        let x = &self.mono;
        let frame = &x[MAX_PERIOD..];
        let e0 = dot(frame, frame);
        if e0 < 1e-3 * n as f32 {
            return Pf::default();
        }
        let q = len / 4;
        self.dec.clear();
        self.dec
            .extend((0..q).map(|k| x[4 * k] + x[4 * k + 1] + x[4 * k + 2] + x[4 * k + 3]));
        let d = &self.dec;
        let h = MAX_PERIOD / 4;
        let cur = &d[h..];
        let ed = dot(cur, cur);
        let lo = MIN_PERIOD / 4 + 1;
        let hi = TOP_PERIOD / 4 - 1;
        let nl = hi - lo + 1;
        self.corr.clear();
        self.corr.resize(nl, 0.0);
        xcorr(cur, &d[h - hi..q - lo], &mut self.corr);
        self.score.clear();
        self.score.resize(nl, 0.0);
        let mut et = dot(&d[h - lo..q - lo], &d[h - lo..q - lo]);
        for tau in lo..=hi {
            let c = self.corr[hi - tau];
            if c > 0.0 {
                self.score[tau - lo] = c / (ed * et + 1e-9).sqrt();
            }
            let out = d[q - tau - 1];
            let inn = d[h - tau - 1];
            et += inn * inn - out * out;
        }
        let near = prev.on as usize * prev.period / 4;
        let mut best = (0usize, 0f32);
        for (k, &v) in self.score.iter().enumerate() {
            let tau = lo + k;
            let v = if near > 0 && tau.abs_diff(near) <= 1 {
                v * 1.08
            } else {
                v
            };
            if v > best.1 {
                best = (tau, v);
            }
        }
        if best.0 == 0 || best.1 < 0.3 {
            return Pf::default();
        }
        let base = self.score[best.0 - lo];
        for div in (2..=8).rev() {
            let sub = best.0 / div;
            if sub < lo + 1 {
                continue;
            }
            let (k, v) = (sub - 1..=(sub + 1).min(hi))
                .map(|t| (t, self.score[t - lo]))
                .fold((0, 0f32), |a, b| if b.1 > a.1 { b } else { a });
            if v > 0.9 * base {
                best = (k, v);
                break;
            }
        }
        let a = (4 * best.0).saturating_sub(5).max(MIN_PERIOD);
        let b = (4 * best.0 + 5).min(TOP_PERIOD);
        let nf = b - a + 1;
        self.corr.clear();
        self.corr.resize(nf, 0.0);
        xcorr(frame, &x[MAX_PERIOD - b..len - a], &mut self.corr);
        let mut ep = dot(&x[MAX_PERIOD - a..len - a], &x[MAX_PERIOD - a..len - a]);
        let (mut t, mut s) = (0usize, 0f32);
        for p in a..=b {
            let c = self.corr[b - p];
            if c > 0.0 {
                let v = c / (e0 * ep + 1e-9).sqrt();
                if v > s {
                    (t, s) = (p, v);
                }
            }
            let inn = x[MAX_PERIOD - p - 1];
            let out = x[len - p - 1];
            ep += inn * inn - out * out;
        }
        if s < 0.3 {
            return Pf::default();
        }
        let mut c5 = [0f32; 5];
        xcorr(frame, &x[MAX_PERIOD - t - 2..len - t + 2], &mut c5);
        let mut r5 = [0f32; 5];
        xcorr(
            &x[MAX_PERIOD - t..len - t],
            &x[MAX_PERIOD - t..len - t + 4],
            &mut r5,
        );
        let f = [c5[2], c5[1] + c5[3], c5[0] + c5[4]];
        let gram = [
            [r5[0], 2.0 * r5[1], 2.0 * r5[2]],
            [2.0 * r5[1], 2.0 * (r5[0] + r5[2]), 2.0 * (r5[1] + r5[3])],
            [2.0 * r5[2], 2.0 * (r5[1] + r5[3]), 2.0 * (r5[0] + r5[4])],
        ];
        let mut pick = Pf::default();
        let mut best_gain = 0f32;
        for (tapset, g) in COMB_GAINS.iter().enumerate() {
            let xp = g[0] * f[0] + g[1] * f[1] + g[2] * f[2];
            let mut pp = 0f32;
            for i in 0..3 {
                for j in 0..3 {
                    pp += g[i] * gram[i][j] * g[j];
                }
            }
            if xp <= 0.0 || pp <= 0.0 {
                continue;
            }
            let opt = (xp / pp).min(1.0);
            let gain = opt * xp / (e0 * pp).sqrt();
            if gain > best_gain {
                best_gain = gain;
                let qg = (opt * strength / 0.09375).round() - 1.0;
                pick = if qg < 0.0 {
                    Pf::default()
                } else {
                    Pf {
                        period: t,
                        qg: (qg as u32).min(7),
                        on: true,
                        tapset,
                    }
                };
            }
        }
        pick
    }
}

pub fn prefilter(raw: &[f32], out: &mut [f32], old: &Pf, new: &Pf, window: &[f32; OVERLAP]) {
    let n = out.len();
    let (g0, g1) = (old.gain(), new.gain());
    if g0 == 0.0 && g1 == 0.0 {
        out.copy_from_slice(&raw[MAX_PERIOD..MAX_PERIOD + n]);
        return;
    }
    let t0 = old.period.max(MIN_PERIOD);
    let t1 = new.period.max(MIN_PERIOD);
    let k0 = COMB_GAINS[old.tapset].map(|v| v * g0);
    let k1 = COMB_GAINS[new.tapset].map(|v| v * g1);
    let fade = if g0 == g1 && t0 == t1 && old.tapset == new.tapset {
        0
    } else {
        OVERLAP.min(n)
    };
    for (i, o) in out.iter_mut().enumerate().take(fade) {
        let at = MAX_PERIOD + i;
        let f = window[i] * window[i];
        let a = if g0 == 0.0 {
            0.0
        } else {
            taps(raw, at, t0, &k0)
        };
        let b = if g1 == 0.0 {
            0.0
        } else {
            taps(raw, at, t1, &k1)
        };
        *o = raw[at] - (1.0 - f) * a - f * b;
    }
    if g1 == 0.0 {
        out[fade..].copy_from_slice(&raw[MAX_PERIOD + fade..MAX_PERIOD + n]);
        return;
    }
    comb(
        &raw[MAX_PERIOD - t1 - 2 + fade..MAX_PERIOD + n],
        &mut out[fade..],
        t1,
        &k1,
    );
}

dispatch! {
    fma: fn comb(src: &[f32], out: &mut [f32], t: usize, k: &[f32; 3]) {
        let n = out.len();
        let a = &src[..n];
        let b = &src[1..n + 1];
        let c = &src[2..n + 2];
        let d = &src[3..n + 3];
        let e = &src[4..n + 4];
        let x = &src[t + 2..t + 2 + n];
        for i in 0..n {
            out[i] = x[i] - (k[0] * c[i] + k[1] * (b[i] + d[i]) + k[2] * (a[i] + e[i]));
        }
    }
}
