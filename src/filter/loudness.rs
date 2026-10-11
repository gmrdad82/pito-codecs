use super::resample::bessel_i0;
use crate::media::{AudioBuffer, Error, Result};
use rayon::prelude::*;
use std::f64::consts::PI;

const HIST_MIN: f64 = -70.0;
const HIST_MAX: f64 = 20.0;
const HIST_STEP: f64 = 0.01;
const HIST_BINS: usize = ((HIST_MAX - HIST_MIN) / HIST_STEP) as usize;
const SHORT: usize = 30;
const MOMENTARY: usize = 4;
pub(crate) const TAPS: usize = 32;
const CUTOFF: f64 = 0.97;
const BETA: f64 = 9.0;
const CHUNK: usize = 1024;
const PARALLEL: usize = 8192;

pub(crate) fn lufs(e: f64) -> f64 {
    if e <= 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * e.log10()
    }
}

#[derive(Debug, Clone, Copy)]
struct Coeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

fn prefilter(rate: f64) -> Coeffs {
    let f0 = 1_681.974_450_955_533;
    let gain_db = 3.999_843_853_973_347;
    let q = 0.707_175_236_955_419_6;
    let k = (PI * f0 / rate).tan();
    let vh = 10f64.powf(gain_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    Coeffs {
        b0: (vh + vb * k / q + k * k) / a0,
        b1: 2.0 * (k * k - vh) / a0,
        b2: (vh - vb * k / q + k * k) / a0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    }
}

fn rlb(rate: f64) -> Coeffs {
    let f0 = 38.135_470_876_024_44;
    let q = 0.500_327_037_323_877_3;
    let k = (PI * f0 / rate).tan();
    let a0 = 1.0 + k / q + k * k;
    Coeffs {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: 2.0 * (k * k - 1.0) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    }
}

fn weights(channels: usize) -> Vec<f64> {
    match channels {
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        8 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

pub(crate) fn oversampling(rate: f64) -> usize {
    if rate < 88_200.0 {
        4
    } else if rate < 176_400.0 {
        2
    } else {
        1
    }
}

pub(crate) fn polyphase(factor: usize) -> Vec<[f32; TAPS]> {
    if factor < 2 {
        return Vec::new();
    }
    let half = TAPS as f64 / 2.0;
    let i0 = bessel_i0(BETA);
    (0..factor)
        .map(|p| {
            let mut w = [0.0f64; TAPS];
            for (j, v) in w.iter_mut().enumerate() {
                let t = j as f64 - (half - 1.0) - p as f64 / factor as f64;
                let r = t / half;
                let window = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0
                };
                let x = CUTOFF * t;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                *v = sinc * window;
            }
            let sum: f64 = w.iter().sum();
            w.map(|v| (v / sum) as f32)
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Histogram {
    count: Vec<u64>,
    energy: Vec<f64>,
}

impl Histogram {
    fn new() -> Self {
        Histogram {
            count: vec![0; HIST_BINS],
            energy: vec![0.0; HIST_BINS],
        }
    }

    fn bin(l: f64) -> usize {
        (((l - HIST_MIN) / HIST_STEP).floor().max(0.0) as usize).min(HIST_BINS - 1)
    }

    fn centre(i: usize) -> f64 {
        HIST_MIN + (i as f64 + 0.5) * HIST_STEP
    }

    fn add(&mut self, energy: f64) {
        let l = lufs(energy);
        if l >= HIST_MIN {
            let b = Self::bin(l);
            self.count[b] += 1;
            self.energy[b] += energy;
        }
    }

    fn mean_from(&self, from: usize) -> Option<f64> {
        let e: f64 = self.energy[from..].iter().sum();
        let n: u64 = self.count[from..].iter().sum();
        (n > 0).then(|| e / n as f64)
    }

    fn gate(&self, relative: f64) -> Option<usize> {
        let t = lufs(self.mean_from(0)?) + relative;
        Some(if t < HIST_MIN { 0 } else { Self::bin(t) })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    pub integrated: f64,
    pub range: f64,
    pub true_peak: f64,
    pub sample_peak: f64,
    pub momentary_max: f64,
    pub short_term_max: f64,
}

#[derive(Debug, Clone)]
struct Channel {
    s: [f64; 4],
    carry: f64,
    sums: Vec<f64>,
    buffer: Vec<f32>,
    out: Vec<f32>,
    sample_peak: f32,
    true_peak: f32,
}

impl Channel {
    fn new() -> Self {
        Channel {
            s: [0.0; 4],
            carry: 0.0,
            sums: Vec::new(),
            buffer: vec![0.0; TAPS - 1],
            out: vec![0.0; CHUNK],
            sample_peak: 0.0,
            true_peak: 0.0,
        }
    }

    fn peaks(&mut self, bank: &[[f32; TAPS]]) {
        let peak = peak(&self.buffer[TAPS - 1..]);
        self.sample_peak = self.sample_peak.max(peak);
        let tp = true_peak(&self.buffer, bank, &mut self.out);
        self.true_peak = self.true_peak.max(tp).max(peak);
        let n = self.buffer.len();
        self.buffer.copy_within(n - (TAPS - 1).., 0);
        self.buffer.truncate(TAPS - 1);
    }
}

fn run(chans: &mut [Channel], k: &Filters, segments: &[usize], bank: &[[f32; TAPS]]) {
    if chans.len() == 2 {
        weigh::<2>(chans, k, segments);
    } else {
        weigh::<1>(chans, k, segments);
    }
    chans.iter_mut().for_each(|c| c.peaks(bank));
}

fn weigh<const N: usize>(chans: &mut [Channel], k: &Filters, segments: &[usize]) {
    let mut s: [[f64; 4]; N] = std::array::from_fn(|i| chans[i].s);
    let mut sums: [Vec<f64>; N] = std::array::from_fn(|i| std::mem::take(&mut chans[i].sums));
    sums.iter_mut().for_each(Vec::clear);
    let data: [&[f32]; N] = std::array::from_fn(|i| &chans[i].buffer[TAPS - 1..]);
    let (pre, rlb) = (k.pre, k.rlb);
    let mut at = 0;
    for &len in segments {
        let mut acc = [0.0f64; N];
        let part: [&[f32]; N] = std::array::from_fn(|i| &data[i][at..at + len]);
        (0..len).for_each(|t| {
            for ((st, a), p) in s.iter_mut().zip(acc.iter_mut()).zip(&part) {
                let x = p[t] as f64;
                let [p1, p2, r1, r2] = st;
                let y = pre.b0 * x + *p1;
                *p1 = pre.b1 * x - pre.a1 * y + *p2;
                *p2 = pre.b2 * x - pre.a2 * y;
                let z = y + *r1;
                *r1 = -2.0 * y - rlb.a1 * z + *r2;
                *r2 = y - rlb.a2 * z;
                *a += z * z;
            }
        });
        for i in 0..N {
            sums[i].push(acc[i]);
            s[i] = s[i].map(|v| if v.abs() < 1e-30 { 0.0 } else { v });
        }
        at += len;
    }
    for (c, (st, sum)) in chans.iter_mut().zip(s.into_iter().zip(sums)) {
        c.s = st;
        c.sums = sum;
    }
}

pub(crate) fn peak(x: &[f32]) -> f32 {
    let mut acc = [0.0f32; 16];
    let mut chunks = x.chunks_exact(16);
    for c in &mut chunks {
        for k in 0..16 {
            acc[k] = acc[k].max(c[k].abs());
        }
    }
    let tail = chunks
        .remainder()
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()));
    acc.iter().fold(tail, |m, &v| m.max(v))
}

pub(crate) fn true_peak(buffer: &[f32], bank: &[[f32; TAPS]], scratch: &mut [f32]) -> f32 {
    if bank.is_empty() {
        return 0.0;
    }
    let n = buffer.len().saturating_sub(TAPS - 1);
    let mut peak = 0.0f32;
    let mut start = 0;
    while start < n {
        let len = (n - start).min(scratch.len());
        let out = &mut scratch[..len];
        interpolated(&buffer[start..start + len + TAPS - 1], bank, out, peak);
        peak = peak.max(self::peak(out));
        start += len;
    }
    peak
}

pub(crate) fn interpolated(buffer: &[f32], bank: &[[f32; TAPS]], out: &mut [f32], floor: f32) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
        unsafe { interpolated_fma(buffer, bank, out, floor) };
        return;
    }
    interpolated_plain(buffer, bank, out, floor)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn interpolated_fma(buffer: &[f32], bank: &[[f32; TAPS]], out: &mut [f32], floor: f32) {
    interpolated_plain(buffer, bank, out, floor)
}

#[inline(always)]
fn interpolated_plain(buffer: &[f32], bank: &[[f32; TAPS]], out: &mut [f32], floor: f32) {
    const B: usize = 64;
    let n = out.len();
    assert!(buffer.len() >= n + TAPS - 1);
    let gain = bank
        .iter()
        .map(|c| c.iter().map(|v| v.abs()).sum::<f32>())
        .fold(0.0f32, f32::max);
    let mut o = 0;
    while o + B <= n {
        if peak(&buffer[o..o + B + TAPS - 1]) * gain <= floor {
            out[o..o + B].fill(0.0);
            o += B;
            continue;
        }
        let mut m = [0.0f32; B];
        for c in bank {
            let mut acc = [0.0f32; B];
            for (j, &w) in c.iter().enumerate() {
                let x: &[f32; B] = buffer[o + TAPS - 1 - j..o + TAPS - 1 - j + B]
                    .try_into()
                    .unwrap();
                for k in 0..B {
                    acc[k] += w * x[k];
                }
            }
            for k in 0..B {
                m[k] = m[k].max(acc[k].abs());
            }
        }
        out[o..o + B].copy_from_slice(&m);
        o += B;
    }
    for (i, v) in out.iter_mut().enumerate().skip(o) {
        *v = bank
            .iter()
            .map(|c| {
                c.iter()
                    .enumerate()
                    .map(|(j, &w)| w * buffer[i + TAPS - 1 - j])
                    .sum::<f32>()
                    .abs()
            })
            .fold(0.0, f32::max);
    }
}

#[derive(Debug, Clone, Copy)]
struct Filters {
    pre: Coeffs,
    rlb: Coeffs,
}

#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    rate: u32,
    weights: Vec<f64>,
    filters: Filters,
    bank: Vec<[f32; TAPS]>,
    channels: Vec<Channel>,
    segments: Vec<usize>,
    block: usize,
    position: usize,
    ring: [f64; SHORT],
    ring_at: usize,
    seen: u64,
    momentary: f64,
    short_term: f64,
    momentary_max: f64,
    short_term_max: f64,
    integrated: Histogram,
    range: Histogram,
}

impl LoudnessMeter {
    pub fn new(rate: u32, channels: u16) -> Result<Self> {
        if rate == 0 || channels == 0 {
            return Err(Error::Invalid(format!(
                "loudness meter at {rate} Hz with {channels} channels"
            )));
        }
        let r = rate as f64;
        Ok(LoudnessMeter {
            rate,
            weights: weights(channels as usize),
            filters: Filters {
                pre: prefilter(r),
                rlb: rlb(r),
            },
            bank: polyphase(oversampling(r)),
            channels: vec![Channel::new(); channels as usize],
            segments: Vec::new(),
            block: ((r * 0.1).round() as usize).max(1),
            position: 0,
            ring: [0.0; SHORT],
            ring_at: 0,
            seen: 0,
            momentary: f64::NEG_INFINITY,
            short_term: f64::NEG_INFINITY,
            momentary_max: f64::NEG_INFINITY,
            short_term_max: f64::NEG_INFINITY,
            integrated: Histogram::new(),
            range: Histogram::new(),
        })
    }

    pub fn set_weights(&mut self, weights: &[f64]) {
        for (i, w) in self.weights.iter_mut().enumerate() {
            *w = weights.get(i).copied().unwrap_or(1.0);
        }
    }

    pub fn process(&mut self, input: &AudioBuffer) -> Result<()> {
        let nch = self.channels.len();
        if input.rate != self.rate || input.channels as usize != nch {
            return Err(Error::Invalid(format!(
                "meter expects {} Hz {} channels, got {} Hz {} channels",
                self.rate, nch, input.rate, input.channels
            )));
        }
        let frames = input.frames();
        if frames == 0 {
            return Ok(());
        }
        self.segments.clear();
        let mut left = frames;
        let mut room = self.block - self.position;
        while left > 0 {
            let take = left.min(room);
            self.segments.push(take);
            left -= take;
            room = self.block;
        }
        for (c, ch) in self.channels.iter_mut().enumerate() {
            let start = ch.buffer.len();
            ch.buffer.resize(start + frames, 0.0);
            for (o, f) in ch.buffer[start..]
                .iter_mut()
                .zip(input.samples.chunks_exact(nch))
            {
                *o = f[c];
            }
        }
        let (filters, segments, bank) = (self.filters, &self.segments, &self.bank);
        if frames * nch >= PARALLEL && nch > 1 {
            self.channels
                .par_chunks_mut(2)
                .for_each(|pair| run(pair, &filters, segments, bank));
        } else {
            self.channels
                .chunks_mut(2)
                .for_each(|pair| run(pair, &filters, segments, bank));
        }
        for k in 0..self.segments.len() {
            let len = self.segments[k];
            for ch in self.channels.iter_mut() {
                ch.carry += ch.sums[k];
            }
            self.position += len;
            if self.position == self.block {
                let e = self
                    .channels
                    .iter_mut()
                    .zip(&self.weights)
                    .map(|(ch, w)| w * std::mem::take(&mut ch.carry))
                    .sum();
                self.finish(e);
            }
        }
        Ok(())
    }

    fn finish(&mut self, e: f64) {
        self.ring[self.ring_at] = e;
        self.ring_at = (self.ring_at + 1) % SHORT;
        self.position = 0;
        self.seen += 1;
        let window = |k: usize| {
            let s: f64 = (0..k)
                .map(|i| self.ring[(self.ring_at + SHORT - 1 - i) % SHORT])
                .sum();
            s / (k * self.block) as f64
        };
        let em = window(MOMENTARY);
        let es = window(SHORT);
        self.momentary = lufs(em);
        self.short_term = lufs(es);
        if self.seen >= MOMENTARY as u64 {
            self.integrated.add(em);
            self.momentary_max = self.momentary_max.max(self.momentary);
        }
        if self.seen >= SHORT as u64 {
            self.range.add(es);
            self.short_term_max = self.short_term_max.max(self.short_term);
        }
    }

    pub fn momentary(&self) -> f64 {
        self.momentary
    }

    pub fn short_term(&self) -> f64 {
        self.short_term
    }

    pub fn integrated(&self) -> f64 {
        let h = &self.integrated;
        h.gate(-10.0)
            .and_then(|b| h.mean_from(b))
            .map_or(f64::NEG_INFINITY, lufs)
    }

    pub fn range(&self) -> f64 {
        let h = &self.range;
        let Some(start) = h.gate(-20.0) else {
            return 0.0;
        };
        let n: u64 = h.count[start..].iter().sum();
        if n == 0 {
            return 0.0;
        }
        let percentile = |p: f64| {
            let rank = ((p * (n - 1) as f64).round() as u64).min(n - 1);
            let mut c = 0u64;
            for i in start..HIST_BINS {
                c += h.count[i];
                if c > rank {
                    return Histogram::centre(i);
                }
            }
            Histogram::centre(HIST_BINS - 1)
        };
        (percentile(0.95) - percentile(0.10)).max(0.0)
    }

    pub fn true_peak(&self) -> f64 {
        let p = self.channels.iter().fold(0.0f32, |m, c| m.max(c.true_peak));
        20.0 * (p as f64).log10()
    }

    pub fn sample_peak(&self) -> f64 {
        let p = self
            .channels
            .iter()
            .fold(0.0f32, |m, c| m.max(c.sample_peak));
        20.0 * (p as f64).log10()
    }

    pub fn summary(&self) -> Loudness {
        Loudness {
            integrated: self.integrated(),
            range: self.range(),
            true_peak: self.true_peak(),
            sample_peak: self.sample_peak(),
            momentary_max: self.momentary_max,
            short_term_max: self.short_term_max,
        }
    }
}
