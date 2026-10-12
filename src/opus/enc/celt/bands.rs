use crate::opus::celt::rate::{Mode, QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE, get_pulses};
use crate::opus::celt::tables::*;
use crate::opus::range::{BITRES, RangeEncoder};

use super::pvq::Pvq;
use crate::opus::celt::bands::{
    bitexact_cos, bitexact_log2tan, compute_qn, deinterleave_hadamard, exp_rotation1, frac_mul16,
    haar1,
};

const SPREAD_NONE: usize = 0;

fn exp_rotation_forward(x: &mut [f32], len: usize, stride: usize, k: usize, spread: usize) {
    const SPREAD_FACTOR: [usize; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[spread - 1];
    let gain = len as f32 / (len + factor * k) as f32;
    let theta = 0.5 * gain * gain;
    let c = (0.5 * std::f32::consts::PI * theta).cos();
    let s = (0.5 * std::f32::consts::PI * (1.0 - theta)).cos();
    let mut stride2 = 0;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let sub = len / stride;
    for i in 0..stride {
        let xs = &mut x[i * sub..(i + 1) * sub];
        exp_rotation1(xs, sub, 1, c, -s);
        if stride2 != 0 {
            exp_rotation1(xs, sub, stride2, s, -c);
        }
    }
}

fn stereo_itheta(x: &[f32], y: &[f32], stereo: bool) -> i32 {
    let (mut emid, mut eside) = (1e-15f32, 1e-15f32);
    if stereo {
        for (a, b) in x.iter().zip(y) {
            let m = a + b;
            let s = a - b;
            emid += m * m;
            eside += s * s;
        }
    } else {
        for (a, b) in x.iter().zip(y) {
            emid += a * a;
            eside += b * b;
        }
    }
    let t = eside.sqrt().atan2(emid.sqrt());
    (0.5 + 16384.0 * std::f32::consts::FRAC_2_PI * t).floor() as i32
}

fn stereo_split(x: &mut [f32], y: &mut [f32]) {
    const S: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for (a, b) in x.iter_mut().zip(y.iter_mut()) {
        let l = S * *a;
        let r = S * *b;
        *a = l + r;
        *b = r - l;
    }
}

fn intensity_stereo(x: &mut [f32], y: &[f32], left: f32, right: f32) {
    let norm = 1e-15 + (1e-15 + left * left + right * right).sqrt();
    let a1 = left / norm;
    let a2 = right / norm;
    for (a, b) in x.iter_mut().zip(y) {
        *a = a1 * *a + a2 * *b;
    }
}

struct Split {
    imid: i32,
    iside: i32,
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

pub struct BandEnc<'m, 'e> {
    pub m: &'m Mode,
    pub enc: &'e mut RangeEncoder,
    pub i: usize,
    pub intensity: usize,
    pub spread: usize,
    pub tf_change: i32,
    pub remaining_bits: i32,
    pub disable_inv: bool,
    pub avoid_split_noise: bool,
    pub band_e: [f32; 2],
    pub pvq: &'m mut Pvq,
    pub tmp: &'m mut Vec<f32>,
}

impl BandEnc<'_, '_> {
    fn compute_theta(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        b: &mut i32,
        b0: usize,
        lm: i32,
        stereo: bool,
    ) -> Split {
        let n = x.len() as i32;
        let i = self.i;
        let pulse_cap = self.m.log_n[i] + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if stereo && n == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = compute_qn(n, *b, offset, pulse_cap, stereo);
        if stereo && i >= self.intensity {
            qn = 1;
        }
        let raw = stereo_itheta(x, y, stereo);
        let tell = self.enc.tell_frac() as i32;
        let mut itheta;
        if qn != 1 {
            itheta = (raw * qn + 8192) >> 14;
            if !stereo && self.avoid_split_noise && itheta > 0 && itheta < qn {
                let unq = itheta * 16384 / qn;
                let imid = bitexact_cos(unq);
                let iside = bitexact_cos(16384 - unq);
                let delta = frac_mul16((n - 1) << 7, bitexact_log2tan(iside, imid));
                if delta > *b {
                    itheta = qn;
                } else if delta < -*b {
                    itheta = 0;
                }
            }
            if stereo && n > 2 {
                let p0 = 3u32;
                let x0 = (qn / 2) as u32;
                let ft = p0 * (x0 + 1) + x0;
                let xv = itheta as u32;
                let (fl, fh) = if xv <= x0 {
                    (p0 * xv, p0 * (xv + 1))
                } else {
                    ((xv - 1 - x0) + (x0 + 1) * p0, (xv - x0) + (x0 + 1) * p0)
                };
                self.enc.encode(fl, fh, ft);
            } else if b0 > 1 || stereo {
                self.enc.uint(itheta as u32, qn as u32 + 1);
            } else {
                let q = qn as u32;
                let half = q >> 1;
                let ft = (half + 1) * (half + 1);
                let it = itheta as u32;
                let (fl, fs) = if it <= half {
                    (it * (it + 1) >> 1, it + 1)
                } else {
                    (ft - ((q + 1 - it) * (q + 2 - it) >> 1), q + 1 - it)
                };
                self.enc.encode(fl, fl + fs, ft);
            }
            itheta = ((itheta as u32 * 16384) / qn as u32) as i32;
            if stereo {
                if itheta == 0 {
                    intensity_stereo(x, y, self.band_e[0], self.band_e[1]);
                } else {
                    stereo_split(x, y);
                }
            }
        } else {
            if stereo {
                let mut inv = raw > 8192 && !self.disable_inv;
                if inv {
                    for v in y.iter_mut() {
                        *v = -*v;
                    }
                }
                intensity_stereo(x, y, self.band_e[0], self.band_e[1]);
                if *b > 2 << BITRES && self.remaining_bits > 2 << BITRES {
                    self.enc.bit_logp(inv, 2);
                } else {
                    inv = false;
                }
                let _ = inv;
            }
            itheta = 0;
        }
        let qalloc = self.enc.tell_frac() as i32 - tell;
        *b -= qalloc;
        let (imid, iside, delta) = if itheta == 0 {
            (32767, 0, -16384)
        } else if itheta == 16384 {
            (0, 32767, 16384)
        } else {
            let imid = bitexact_cos(itheta);
            let iside = bitexact_cos(16384 - itheta);
            (
                imid,
                iside,
                frac_mul16((n - 1) << 7, bitexact_log2tan(iside, imid)),
            )
        };
        Split {
            imid,
            iside,
            delta,
            itheta,
            qalloc,
        }
    }

    fn quant_n1(&mut self, x: &[f32], y: Option<&[f32]>) {
        for v in std::iter::once(x).chain(y) {
            if self.remaining_bits >= 1 << BITRES {
                self.enc.bits((v[0] < 0.0) as u32, 1);
                self.remaining_bits -= 1 << BITRES;
            }
        }
    }

    fn quant_partition(&mut self, x: &mut [f32], mut b: i32, mut bb: usize, mut lm: i32) {
        let n = x.len();
        let b0 = bb;
        let i = self.i;
        let cache = self.m.cache(lm, i);
        let cache_max = cache[cache[0] as usize] as i32;
        if lm != -1 && b > cache_max + 12 && n > 2 {
            let half = n >> 1;
            let (xa, ya) = x.split_at_mut(half);
            lm -= 1;
            bb = (bb + 1) >> 1;
            let sp = self.compute_theta(xa, ya, &mut b, b0, lm, false);
            let mut delta = sp.delta;
            if b0 > 1 && (sp.itheta & 0x3fff) != 0 {
                if sp.itheta > 8192 {
                    delta -= delta >> (4 - lm);
                } else {
                    delta = 0.min(delta + ((half as i32) << BITRES >> (5 - lm)));
                }
            }
            let mut mbits = 0.max(b.min((b - delta) / 2));
            let mut sbits = b - mbits;
            self.remaining_bits -= sp.qalloc;
            let mut rebalance = self.remaining_bits;
            if mbits >= sbits {
                self.quant_partition(xa, mbits, bb, lm);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && sp.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                self.quant_partition(ya, sbits, bb, lm);
            } else {
                self.quant_partition(ya, sbits, bb, lm);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && sp.itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                self.quant_partition(xa, mbits, bb, lm);
            }
            let _ = (sp.imid, sp.iside);
            return;
        }
        let m = self.m;
        let mut q = m.bits2pulses(i, lm, b);
        let mut curr_bits = m.pulses2bits(i, lm, q);
        self.remaining_bits -= curr_bits;
        while self.remaining_bits < 0 && q > 0 {
            self.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = m.pulses2bits(i, lm, q);
            self.remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = get_pulses(q) as usize;
            exp_rotation_forward(x, n, bb, k, self.spread);
            self.pvq.search(x, k);
            self.pvq.encode(self.enc, n, k);
        }
    }

    pub fn quant_band(&mut self, x: &mut [f32], b: i32, mut bb: usize, lm: i32) {
        let n = x.len();
        if n == 1 {
            self.quant_n1(x, None);
            return;
        }
        let long_blocks = bb == 1;
        let mut tf_change = self.tf_change;
        let mut n_b = n / bb;
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
        if bb > 1 {
            deinterleave_hadamard(x, n_b >> recombine, bb << recombine, long_blocks, self.tmp);
        }
        self.quant_partition(x, b, bb, lm);
    }

    pub fn quant_band_stereo(
        &mut self,
        x: &mut [f32],
        y: &mut [f32],
        mut b: i32,
        bb: usize,
        lm: i32,
    ) {
        let n = x.len();
        if n == 1 {
            self.quant_n1(x, Some(y));
            return;
        }
        let sp = self.compute_theta(x, y, &mut b, bb, lm, true);
        if n == 2 {
            let mut mbits = b;
            let mut sbits = 0;
            if sp.itheta != 0 && sp.itheta != 16384 {
                sbits = 1 << BITRES;
            }
            mbits -= sbits;
            let c = sp.itheta > 8192;
            self.remaining_bits -= sp.qalloc + sbits;
            let (x2, y2) = if c { (y, x) } else { (x, y) };
            if sbits != 0 {
                let sign = x2[0] * y2[1] - x2[1] * y2[0] < 0.0;
                self.enc.bits(sign as u32, 1);
            }
            self.quant_band(x2, mbits, bb, lm);
        } else {
            let mut mbits = 0.max(b.min((b - sp.delta) / 2));
            let mut sbits = b - mbits;
            self.remaining_bits -= sp.qalloc;
            let mut rebalance = self.remaining_bits;
            if mbits >= sbits {
                self.quant_band(x, mbits, bb, lm);
                rebalance = mbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && sp.itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                self.quant_band(y, sbits, bb, lm);
            } else {
                self.quant_band(y, sbits, bb, lm);
                rebalance = sbits - (rebalance - self.remaining_bits);
                if rebalance > 3 << BITRES && sp.itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                self.quant_band(x, mbits, bb, lm);
            }
        }
    }
}

pub struct Bands<'a> {
    pub start: usize,
    pub end: usize,
    pub lm: usize,
    pub short_blocks: bool,
    pub spread: usize,
    pub dual_stereo: bool,
    pub intensity: usize,
    pub tf_res: &'a [i32; NB_EBANDS],
    pub pulses: &'a [i32; NB_EBANDS],
    pub total_bits: i32,
    pub balance: i32,
    pub coded_bands: usize,
    pub disable_inv: bool,
    pub band_amp: &'a [f32],
}

pub fn quant_all_bands(
    m: &Mode,
    p: &Bands,
    x_buf: &mut [f32],
    y_buf: Option<&mut [f32]>,
    enc: &mut RangeEncoder,
    pvq: &mut Pvq,
    tmp: &mut Vec<f32>,
) {
    let mm = 1usize << p.lm;
    let bb = if p.short_blocks { mm } else { 1 };
    let lmi = p.lm as i32;
    let mut y_buf = y_buf;
    let mut balance = p.balance;
    let mut dual_stereo = p.dual_stereo;
    let mut ctx = BandEnc {
        m,
        enc,
        i: 0,
        intensity: p.intensity,
        spread: p.spread,
        tf_change: 0,
        remaining_bits: 0,
        disable_inv: p.disable_inv,
        avoid_split_noise: bb > 1,
        band_e: [0.0; 2],
        pvq,
        tmp,
    };
    for i in p.start..p.end {
        ctx.i = i;
        let xo = mm * EBANDS[i] as usize;
        let n = mm * EBANDS[i + 1] as usize - xo;
        let tell = ctx.enc.tell_frac() as i32;
        if i != p.start {
            balance -= tell;
        }
        let remaining_bits = p.total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        let b = if i < p.coded_bands {
            let curr_balance = balance / 3.min(p.coded_bands as i32 - i as i32);
            0.max(16383.min((remaining_bits + 1).min(p.pulses[i] + curr_balance)))
        } else {
            0
        };
        ctx.tf_change = p.tf_res[i];
        ctx.band_e = [p.band_amp[i], p.band_amp[NB_EBANDS + i]];
        if dual_stereo && i == p.intensity {
            dual_stereo = false;
        }
        let x = &mut x_buf[xo..xo + n];
        match y_buf.as_deref_mut() {
            Some(yb) => {
                let y = &mut yb[xo..xo + n];
                if dual_stereo {
                    ctx.quant_band(x, b / 2, bb, lmi);
                    ctx.quant_band(y, b / 2, bb, lmi);
                } else {
                    ctx.quant_band_stereo(x, y, b, bb, lmi);
                }
            }
            None => ctx.quant_band(x, b, bb, lmi),
        }
        balance += p.pulses[i] + tell;
        ctx.avoid_split_noise = false;
    }
}
