mod alloc;
mod bands;
mod energy;
mod mdct;
mod pitch;
mod pvq;
mod tf;

use crate::opus::celt::mode;
use crate::opus::celt::rate::Mode;
use crate::opus::celt::tables::*;
use crate::opus::range::{BITRES, RangeEncoder, ilog};

use alloc::{AllocChoice, compute_allocation_enc};
use bands::{Bands, quant_all_bands};
use energy::{Coarse, quant_coarse_energy, quant_energy_finalise, quant_fine_energy};
use mdct::{Mdct, MdctScratch};
use pitch::{MAX_PERIOD, Pf, Pitch, prefilter};
use pvq::Pvq;

const TRANSIENT_BLOCK: usize = 40;
const TF_LAMBDA: f32 = 0.02;
const PF_STRENGTH: f32 = 0.8;

pub struct CeltEncoder {
    m: &'static Mode,
    channels: usize,
    mdct: [Mdct; MAX_LM + 1],
    in_mem: Vec<f32>,
    preemph: [f32; 2],
    trans_mask: [f32; 2],
    old_band_e: [f32; 2 * NB_EBANDS],
    last_coded: usize,
    consec_transient: u32,
    force_intra: bool,
    analysed: (bool, usize),
    pub vbr: Vbr,
    pub disable_inv: bool,
    inbuf: Vec<f32>,
    freq: Vec<f32>,
    band_amp: [f32; 2 * NB_EBANDS],
    band_log: [f32; 2 * NB_EBANDS],
    error: [f32; 2 * NB_EBANDS],
    mdct_work: MdctScratch,
    pvq: Pvq,
    tmp: Vec<f32>,
    spread_avg: f32,
    raw: Vec<f32>,
    pre_mem: Vec<f32>,
    pitch: Pitch,
    pf: Pf,
    pf_old: Pf,
}

#[derive(Default)]
pub struct Vbr {
    drift: f32,
    avg: f32,
    frames: u32,
}

impl Vbr {
    pub fn bytes(&mut self, base: f32, max: usize, a: &Analysis) -> usize {
        if a.silent {
            return 3;
        }
        self.avg = if self.frames == 0 {
            a.demand
        } else {
            0.95 * self.avg + 0.05 * a.demand
        };
        self.frames += 1;
        let mut factor = ((a.demand + 1.0) / (self.avg + 1.0)).sqrt().clamp(0.4, 1.8);
        if a.transient {
            factor *= 1.2;
        }
        let target = base * factor - self.drift / 24.0;
        let lo = (base * 0.3).max(8.0);
        let hi = (base * 2.5).min(max as f32);
        let bytes = target.round().clamp(lo, hi.max(lo));
        self.drift = (self.drift + bytes - base).clamp(-8.0 * base, 8.0 * base);
        bytes as usize
    }
}

pub struct Analysis {
    pub silent: bool,
    pub transient: bool,
    pub demand: f32,
}

#[inline]
fn clean(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-64.0, 64.0)
    } else {
        0.0
    }
}

fn lm_of(n: usize) -> usize {
    match n {
        120 => 0,
        240 => 1,
        480 => 2,
        _ => 3,
    }
}

pub struct FrameParams {
    pub start: usize,
    pub end: usize,
    pub nbytes: usize,
    pub intensity: usize,
}

impl CeltEncoder {
    pub fn new(channels: usize) -> CeltEncoder {
        CeltEncoder {
            m: mode(),
            channels,
            mdct: [
                Mdct::new(SHORT_MDCT),
                Mdct::new(SHORT_MDCT << 1),
                Mdct::new(SHORT_MDCT << 2),
                Mdct::new(SHORT_MDCT << 3),
            ],
            in_mem: vec![0.0; channels * OVERLAP],
            preemph: [0.0; 2],
            trans_mask: [0.0; 2],
            old_band_e: [0.0; 2 * NB_EBANDS],
            last_coded: 0,
            consec_transient: 0,
            force_intra: true,
            analysed: (false, 0),
            vbr: Vbr::default(),
            disable_inv: channels == 1,
            inbuf: Vec::new(),
            freq: Vec::new(),
            band_amp: [0.0; 2 * NB_EBANDS],
            band_log: [0.0; 2 * NB_EBANDS],
            error: [0.0; 2 * NB_EBANDS],
            mdct_work: MdctScratch::default(),
            pvq: Pvq::default(),
            tmp: Vec::new(),
            spread_avg: 1.0,
            raw: Vec::new(),
            pre_mem: vec![0.0; channels * MAX_PERIOD],
            pitch: Pitch::default(),
            pf: Pf::default(),
            pf_old: Pf::default(),
        }
    }

    pub fn reset(&mut self) {
        self.in_mem.fill(0.0);
        self.preemph = [0.0; 2];
        self.trans_mask = [0.0; 2];
        self.old_band_e = [0.0; 2 * NB_EBANDS];
        self.last_coded = 0;
        self.consec_transient = 0;
        self.force_intra = true;
        self.vbr = Vbr::default();
        self.spread_avg = 1.0;
        self.pre_mem.fill(0.0);
        self.pf = Pf::default();
        self.pf_old = Pf::default();
    }

    pub fn prime(&mut self, history: &[f32]) {
        self.reset();
        let c = self.channels;
        let n = history.len() / c;
        for ch in 0..c {
            let mut mem = 0f32;
            for j in 0..n {
                let s = clean(history[j * c + ch]) * 32768.0;
                let v = s - mem;
                mem = PREEMPH * s;
                if j + OVERLAP >= n {
                    self.in_mem[ch * OVERLAP + j + OVERLAP - n] = v;
                }
                if j + MAX_PERIOD >= n {
                    self.pre_mem[ch * MAX_PERIOD + j + MAX_PERIOD - n] = v;
                }
            }
            self.preemph[ch] = mem;
        }
    }

    fn load_input(&mut self, pcm: &[f32], n: usize, allow_pf: bool) -> bool {
        let c = self.channels;
        let rl = MAX_PERIOD + n;
        self.raw.clear();
        self.raw.resize(c * rl, 0.0);
        let mut silent = true;
        for ch in 0..c {
            let raw = &mut self.raw[ch * rl..(ch + 1) * rl];
            raw[..MAX_PERIOD]
                .copy_from_slice(&self.pre_mem[ch * MAX_PERIOD..(ch + 1) * MAX_PERIOD]);
            let mut mem = self.preemph[ch];
            for (j, o) in raw[MAX_PERIOD..].iter_mut().enumerate() {
                let x = clean(pcm[j * c + ch]);
                silent &= x == 0.0;
                let s = x * 32768.0;
                *o = s - mem;
                mem = PREEMPH * s;
            }
            self.preemph[ch] = mem;
        }
        self.pf = if allow_pf && !silent {
            self.pitch
                .analyse(&self.raw, c, n, &self.pf_old, PF_STRENGTH)
        } else {
            Pf::default()
        };
        let len = n + OVERLAP;
        self.inbuf.clear();
        self.inbuf.resize(c * len, 0.0);
        for ch in 0..c {
            let raw = &self.raw[ch * rl..(ch + 1) * rl];
            let buf = &mut self.inbuf[ch * len..(ch + 1) * len];
            buf[..OVERLAP].copy_from_slice(&self.in_mem[ch * OVERLAP..(ch + 1) * OVERLAP]);
            prefilter(
                raw,
                &mut buf[OVERLAP..],
                &self.pf_old,
                &self.pf,
                &self.m.window,
            );
            self.in_mem[ch * OVERLAP..(ch + 1) * OVERLAP].copy_from_slice(&buf[n..]);
            self.pre_mem[ch * MAX_PERIOD..(ch + 1) * MAX_PERIOD].copy_from_slice(&raw[n..]);
        }
        self.pf_old = self.pf;
        silent
    }

    fn transient_analysis(&mut self, n: usize) -> bool {
        let len = n + OVERLAP;
        let mut hit = false;
        for ch in 0..self.channels {
            let buf = &self.inbuf[ch * len..(ch + 1) * len];
            let mut mask = self.trans_mask[ch];
            let mut prev = buf[0];
            let blocks = len / TRANSIENT_BLOCK;
            for b in 0..blocks {
                let mut e = 0f32;
                for &v in &buf[b * TRANSIENT_BLOCK..(b + 1) * TRANSIENT_BLOCK] {
                    let d = v - prev;
                    prev = v;
                    e += d * d;
                }
                if e > 4.0e5 && e > 24.0 * (mask + 1.0e4) {
                    hit = true;
                }
                mask = e.max(mask * 0.55);
                if b * TRANSIENT_BLOCK + TRANSIENT_BLOCK == n {
                    self.trans_mask[ch] = mask;
                }
            }
        }
        hit
    }

    fn analyse_bands(&mut self, n: usize, lm: usize, short_blocks: bool, end: usize) {
        let c = self.channels;
        let len = n + OVERLAP;
        let mm = 1usize << lm;
        self.freq.clear();
        self.freq.resize(c * n, 0.0);
        let window = &self.m.window;
        for ch in 0..c {
            let inp = &self.inbuf[ch * len..(ch + 1) * len];
            let out = &mut self.freq[ch * n..(ch + 1) * n];
            if short_blocks {
                for b in 0..mm {
                    let o = b * SHORT_MDCT;
                    self.mdct[0].forward(
                        &inp[o..o + SHORT_MDCT + OVERLAP],
                        window,
                        &mut out[b..],
                        mm,
                        &mut self.mdct_work,
                    );
                }
            } else {
                self.mdct[lm].forward(inp, window, out, 1, &mut self.mdct_work);
            }
            for i in 0..NB_EBANDS {
                let idx = ch * NB_EBANDS + i;
                if i >= end {
                    self.band_amp[idx] = 0.0;
                    self.band_log[idx] = -28.0;
                    continue;
                }
                let band = &mut out[mm * EBANDS[i] as usize..mm * EBANDS[i + 1] as usize];
                let amp = (1e-27 + band.iter().map(|v| v * v).sum::<f32>()).sqrt();
                let g = 1.0 / (1e-27 + amp);
                for v in band.iter_mut() {
                    *v *= g;
                }
                self.band_amp[idx] = amp;
                self.band_log[idx] = amp.log2() - E_MEANS[i];
            }
        }
    }

    fn dual_stereo(&self, n: usize, lm: usize, end: usize) -> bool {
        if self.channels != 2 {
            return false;
        }
        let top = (13.min(end), 1usize << lm);
        let (mut lr, mut ms) = (1e-15f32, 1e-15f32);
        for i in 0..top.0 {
            let (al, ar) = (self.band_amp[i], self.band_amp[NB_EBANDS + i]);
            let lo = top.1 * EBANDS[i] as usize;
            let hi = top.1 * EBANDS[i + 1] as usize;
            for j in lo..hi {
                let l = self.freq[j] * al;
                let r = self.freq[n + j] * ar;
                lr += l.abs() + r.abs();
                ms += (l + r).abs() + (l - r).abs();
            }
        }
        let ratio = ms * std::f32::consts::FRAC_1_SQRT_2 / lr;
        ratio > 0.95
    }

    fn dynalloc(&self, start: usize, end: usize, lm: usize, nbytes: usize) -> [i32; NB_EBANDS] {
        let mut want = [0i32; NB_EBANDS];
        let c = self.channels;
        if nbytes < 30 * c || end - start < 3 {
            return want;
        }
        let mut e = [0f32; NB_EBANDS];
        let mut f = [0f32; NB_EBANDS];
        for i in start..end {
            e[i] = self.band_log[i].max(if c == 2 {
                self.band_log[NB_EBANDS + i]
            } else {
                -28.0
            });
        }
        f[start] = e[start];
        for i in start + 1..end {
            f[i] = (f[i - 1] + 1.5).min(e[i]);
        }
        for i in (start..end - 1).rev() {
            f[i] = f[i].min(f[i + 1] + 2.0);
        }
        let mut budget = (nbytes as i32 * 8) / 6;
        for i in start..end {
            let d = e[i] - f[i] - 0.5;
            if d <= 0.0 {
                continue;
            }
            let width = (c as i32 * (EBANDS[i + 1] - EBANDS[i])) << lm;
            let quanta = width.min(6.max(width >> BITRES));
            let n = ((d * 2.0) as i32).min(budget / quanta.max(1));
            if n > 0 {
                want[i] = n;
                budget -= n * quanta;
            }
        }
        want
    }

    fn alloc_trim(&self, start: usize, end: usize) -> usize {
        let c = self.channels;
        let nb = (end - start) as f32;
        if nb < 2.0 {
            return 5;
        }
        let mid = (start + end - 1) as f32 * 0.5;
        let (mut num, mut den) = (0f32, 0f32);
        for ch in 0..c {
            for i in start..end {
                let d = i as f32 - mid;
                num += d * self.band_log[ch * NB_EBANDS + i].max(-14.0);
                den += d * d;
            }
        }
        let slope = num / den;
        let trim = 5.0 - (12.0 * slope + 0.15).clamp(-2.0, 2.0);
        (trim.round() as i32).clamp(0, 10) as usize
    }

    pub fn analyse(&mut self, pcm: &[f32], n: usize, end: usize, allow_pf: bool) -> Analysis {
        let lm = lm_of(n);
        let silent = self.load_input(pcm, n, allow_pf);
        let transient = lm > 0 && self.transient_analysis(n);
        self.analyse_bands(n, lm, transient, end);
        self.analysed = (transient, end);
        let mut demand = 0f32;
        for ch in 0..self.channels {
            for i in 0..end {
                let w = ((EBANDS[i + 1] - EBANDS[i]) << lm) as f32;
                demand +=
                    w * (self.band_amp[ch * NB_EBANDS + i].max(1e-9).log2() - 2.0).clamp(0.0, 16.0);
            }
        }
        Analysis {
            silent,
            transient,
            demand,
        }
    }

    pub fn encode_frame(
        &mut self,
        n: usize,
        p: &FrameParams,
        a: &Analysis,
        enc: &mut RangeEncoder,
    ) {
        let c = self.channels;
        let m = self.m;
        let lm = lm_of(n);
        let mm = 1usize << lm;
        let (start, end) = (p.start, p.end);
        let nbytes = p.nbytes;
        let total_bits = nbytes as i32 * 8;
        let silent = a.silent;
        let transient = a.transient;

        let mut tell = enc.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            enc.bit_logp(silent, 15);
            silent
        } else {
            false
        };
        if silence {
            for v in self.old_band_e[..c * NB_EBANDS].iter_mut() {
                *v = -28.0;
            }
            self.force_intra = false;
            return;
        }
        if start == 0 && tell + 16 <= total_bits {
            let pf = self.pf;
            enc.bit_logp(pf.on, 1);
            if pf.on {
                let t = pf.period as u32 + 1;
                let octave = (ilog(t) - 5) as u32;
                enc.uint(octave, 6);
                enc.bits(t - (16 << octave), 4 + octave);
                enc.bits(pf.qg, 3);
                if enc.tell() + 2 <= total_bits {
                    enc.icdf(pf.tapset, &TAPSET_ICDF, 2);
                }
            }
            tell = enc.tell();
        }
        let short_blocks = if lm > 0 && tell + 3 <= total_bits {
            enc.bit_logp(transient, 3);
            tell = enc.tell();
            transient
        } else {
            false
        };
        if self.analysed != (short_blocks, end) {
            self.analyse_bands(n, lm, short_blocks, end);
        }
        let intra = if tell + 3 <= total_bits {
            let v = self.force_intra;
            enc.bit_logp(v, 3);
            v
        } else {
            false
        };
        self.force_intra = false;
        let coarse = Coarse {
            start,
            end,
            channels: c,
            lm,
            intra,
            max_decay: 16.0,
            budget: total_bits,
            target: &self.band_log,
        };
        quant_coarse_energy(&coarse, &mut self.old_band_e, &mut self.error, enc);

        let tf = if nbytes >= 15 * c {
            tf::analyse(
                &self.freq,
                n,
                c,
                lm,
                short_blocks,
                start,
                end,
                TF_LAMBDA,
                &mut self.tmp,
            )
        } else {
            tf::Tf {
                res: [short_blocks as i32; NB_EBANDS],
                select: 0,
            }
        };
        let mut tf_res = tf.res;
        tf_encode(
            start,
            end,
            short_blocks,
            &mut tf_res,
            tf.select,
            lm,
            enc,
            total_bits,
        );

        tell = enc.tell();
        let spread = tf::spread(&self.freq, n, c, lm, end, &mut self.spread_avg);
        let spread = if tell + 4 <= total_bits {
            enc.icdf(spread, &SPREAD_ICDF, 5);
            spread
        } else {
            2
        };

        let cap = m.init_caps(lm, c);
        let wants = self.dynalloc(start, end, lm, nbytes);
        let mut offsets = [0i32; NB_EBANDS];
        let mut dynalloc_logp = 6;
        let mut total_frac = total_bits << BITRES;
        let mut tellf = enc.tell_frac() as i32;
        for i in start..end {
            let width = (c as i32 * (EBANDS[i + 1] - EBANDS[i])) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            let want = wants[i];
            let mut j = 0;
            while tellf + (loop_logp << BITRES) < total_frac && boost < cap[i] {
                let flag = j < want;
                enc.bit_logp(flag, loop_logp as u32);
                tellf = enc.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_frac -= quanta;
                loop_logp = 1;
                j += 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }
        let alloc_trim = if tellf + (6 << BITRES) <= total_frac {
            let t = self.alloc_trim(start, end);
            enc.icdf(t, &TRIM_ICDF, 7);
            t
        } else {
            5
        };
        let mut bits = ((nbytes as i32 * 8) << BITRES) - enc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if short_blocks && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;
        let choice = AllocChoice {
            prev_coded: self.last_coded,
            signal_bandwidth: end - 1,
            intensity: p.intensity,
            dual_stereo: self.dual_stereo(n, lm, end),
        };
        let alloc = compute_allocation_enc(
            m,
            start,
            end,
            &offsets,
            &cap,
            alloc_trim as i32,
            bits,
            c,
            lm,
            &choice,
            enc,
        );
        self.last_coded = alloc.coded_bands;
        quant_fine_energy(
            start,
            end,
            &mut self.old_band_e,
            &mut self.error,
            &alloc.fine_quant,
            enc,
            c,
        );

        let params = Bands {
            start,
            end,
            lm,
            short_blocks,
            spread,
            dual_stereo: alloc.dual_stereo,
            intensity: alloc.intensity,
            tf_res: &tf_res,
            pulses: &alloc.pulses,
            total_bits: (nbytes as i32 * (8 << BITRES)) - anti_collapse_rsv,
            balance: alloc.balance,
            coded_bands: alloc.coded_bands,
            disable_inv: self.disable_inv,
            band_amp: &self.band_amp,
        };
        let (xa, xb) = self.freq.split_at_mut(n);
        let yb = if c == 2 { Some(xb) } else { None };
        quant_all_bands(m, &params, xa, yb, enc, &mut self.pvq, &mut self.tmp);

        if anti_collapse_rsv > 0 {
            enc.bits((self.consec_transient < 2) as u32, 1);
        }
        let bits_left = nbytes as i32 * 8 - enc.tell();
        quant_energy_finalise(
            start,
            end,
            &mut self.old_band_e,
            &mut self.error,
            &alloc.fine_quant,
            &alloc.fine_priority,
            bits_left,
            enc,
            c,
        );
        if short_blocks {
            self.consec_transient += 1;
        } else {
            self.consec_transient = 0;
        }
        for ch in 0..2 {
            for i in (0..start).chain(end..NB_EBANDS) {
                self.old_band_e[ch * NB_EBANDS + i] = 0.0;
            }
        }
        let _ = mm;
    }
}

#[allow(clippy::too_many_arguments)]
fn tf_encode(
    start: usize,
    end: usize,
    is_transient: bool,
    tf_res: &mut [i32; NB_EBANDS],
    tf_select: usize,
    lm: usize,
    enc: &mut RangeEncoder,
    total_bits: i32,
) {
    let mut budget = total_bits;
    let mut tell = enc.tell();
    let mut logp: u32 = if is_transient { 2 } else { 4 };
    let tf_select_rsv = lm > 0 && tell + logp as i32 + 1 <= budget;
    budget -= tf_select_rsv as i32;
    let mut tf_changed = 0;
    let mut curr = 0;
    for r in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp as i32 <= budget {
            enc.bit_logp((*r ^ curr) != 0, logp);
            curr = *r;
            tell = enc.tell();
            tf_changed |= curr;
        } else {
            *r = curr;
        }
        logp = if is_transient { 4 } else { 5 };
    }
    let ti = 4 * is_transient as usize;
    let tf_select = if tf_select_rsv
        && TF_SELECT_TABLE[lm][ti + tf_changed as usize]
            != TF_SELECT_TABLE[lm][ti + 2 + tf_changed as usize]
    {
        enc.bit_logp(tf_select != 0, 1);
        tf_select
    } else {
        0
    };
    for r in tf_res.iter_mut().take(end).skip(start) {
        *r = TF_SELECT_TABLE[lm][ti + 2 * tf_select + *r as usize] as i32;
    }
}
