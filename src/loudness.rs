use crate::media::{AudioBuffer, Error, Result};
#[cfg(feature = "filter")]
use rayon::prelude::*;
use std::f64::consts::PI;

const MAX_WINDOW_RATE: u32 = 2_822_400;
const ABSOLUTE_GATE_LUFS: f64 = -70.0;
const SHORT: usize = 30;
const MOMENTARY: usize = 4;
const RANGE_EVERY: u64 = 10;
const REFERENCE_TAPS: f64 = 48.0;
const FOUR_TAPS: usize = 12;
const TWO_TAPS: usize = 24;
#[cfg(feature = "filter")]
pub(crate) const TAPS: usize = 32;
#[cfg(feature = "filter")]
const CUTOFF: f64 = 0.97;
#[cfg(feature = "filter")]
const BETA: f64 = 9.0;
const CHUNK: usize = 1024;
#[cfg(feature = "filter")]
const PARALLEL: usize = 8192;

#[cfg(feature = "filter")]
pub(crate) fn kaiser(r: f64, beta: f64, i0: f64) -> f64 {
    if r.abs() >= 1.0 {
        0.0
    } else {
        bessel_i0(beta * (1.0 - r * r).sqrt()) / i0
    }
}

#[cfg(feature = "filter")]
pub(crate) fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

#[cfg(feature = "filter")]
pub(crate) fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

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
        4 => vec![1.0, 1.0, 1.41, 1.41],
        5 => vec![1.0, 1.0, 1.0, 1.41, 1.41],
        6 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41],
        8 => vec![1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 1.41, 1.41],
        n => vec![1.0; n],
    }
}

#[cfg(feature = "filter")]
pub(crate) fn oversampling(rate: f64) -> usize {
    if rate < 88_200.0 {
        4
    } else if rate < 176_400.0 {
        2
    } else {
        1
    }
}

#[cfg(feature = "filter")]
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
                *v = sinc(CUTOFF * t) * kaiser(t / half, BETA, i0);
            }
            let sum: f64 = w.iter().sum();
            w.map(|v| (v / sum) as f32)
        })
        .collect()
}

fn gate_energy() -> f64 {
    10f64.powf((ABSOLUTE_GATE_LUFS + 0.691) / 10.0)
}

fn relative_threshold(blocks: &[f64]) -> Option<f64> {
    (!blocks.is_empty())
        .then(|| blocks.iter().sum::<f64>() / blocks.len() as f64 * 10f64.powf(-10.0 / 10.0))
}

fn reference_bank<const N: usize>(factor: usize) -> Vec<[f32; N]> {
    let mut bank = vec![[0.0f32; N]; factor];
    for j in 0..N * factor {
        let at = j as f64;
        let w = 0.5 * (1.0 - (2.0 * PI * at / REFERENCE_TAPS).cos());
        let m = at - REFERENCE_TAPS / 2.0;
        let x = m * PI / factor as f64;
        bank[j % factor][j / factor] = if m.abs() > 0.000_001 {
            w * x.sin() / x
        } else {
            w
        } as f32;
    }
    bank
}

#[derive(Debug, Clone)]
enum Oversampler {
    Off,
    Two(Vec<[f32; TWO_TAPS]>),
    Four(Vec<[f32; FOUR_TAPS]>),
}

impl Oversampler {
    fn new(rate: u32) -> Self {
        if rate < 96_000 {
            Oversampler::Four(reference_bank(4))
        } else if rate < 192_000 {
            Oversampler::Two(reference_bank(2))
        } else {
            Oversampler::Off
        }
    }

    fn lead(&self) -> usize {
        match self {
            Oversampler::Off => 0,
            Oversampler::Two(_) => TWO_TAPS - 1,
            Oversampler::Four(_) => FOUR_TAPS - 1,
        }
    }

    fn scan(&self, buffer: &[f32], scratch: &mut [f32]) -> f32 {
        match self {
            Oversampler::Off => 0.0,
            Oversampler::Two(bank) => true_peak(buffer, bank, scratch),
            Oversampler::Four(bank) => true_peak(buffer, bank, scratch),
        }
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
    squares: Vec<f64>,
    buffer: Vec<f32>,
    out: Vec<f32>,
    sample_peak: f32,
    true_peak: f32,
}

impl Channel {
    fn new(lead: usize) -> Self {
        Channel {
            s: [0.0; 4],
            carry: 0.0,
            sums: Vec::new(),
            squares: Vec::new(),
            buffer: vec![0.0; lead],
            out: vec![0.0; CHUNK],
            sample_peak: 0.0,
            true_peak: 0.0,
        }
    }

    fn peaks(&mut self, over: &Oversampler) {
        let lead = over.lead();
        let peak = peak(&self.buffer[lead..]);
        self.sample_peak = self.sample_peak.max(peak);
        let tp = over.scan(&self.buffer, &mut self.out);
        self.true_peak = self.true_peak.max(tp).max(peak);
        let n = self.buffer.len();
        self.buffer.copy_within(n - lead.., 0);
        self.buffer.truncate(lead);
    }
}

fn run(chans: &mut [Channel], k: &Filters, segments: &[usize], over: &Oversampler) {
    let lead = over.lead();
    if chans.len() == 2 {
        weigh::<2>(chans, k, segments, lead);
    } else {
        weigh::<1>(chans, k, segments, lead);
    }
    chans.iter_mut().for_each(|c| c.peaks(over));
}

fn weigh<const N: usize>(chans: &mut [Channel], k: &Filters, segments: &[usize], lead: usize) {
    let mut s: [[f64; 4]; N] = std::array::from_fn(|i| chans[i].s);
    let mut sums: [Vec<f64>; N] = std::array::from_fn(|i| std::mem::take(&mut chans[i].sums));
    sums.iter_mut().for_each(Vec::clear);
    let mut squares: [Vec<f64>; N] = std::array::from_fn(|i| std::mem::take(&mut chans[i].squares));
    squares.iter_mut().for_each(Vec::clear);
    let keep = k.keep;
    let data: [&[f32]; N] = std::array::from_fn(|i| &chans[i].buffer[lead..]);
    let (pre, rlb) = (k.pre, k.rlb);
    let mut at = 0;
    for &len in segments {
        let mut acc = [0.0f64; N];
        let part: [&[f32]; N] = std::array::from_fn(|i| &data[i][at..at + len]);
        (0..len).for_each(|t| {
            for (i, ((st, a), p)) in s.iter_mut().zip(acc.iter_mut()).zip(&part).enumerate() {
                let x = p[t] as f64;
                let [p1, p2, r1, r2] = st;
                let y = pre.b0 * x + *p1;
                *p1 = pre.b1 * x - pre.a1 * y + *p2;
                *p2 = pre.b2 * x - pre.a2 * y;
                let z = y + *r1;
                *r1 = -2.0 * y - rlb.a1 * z + *r2;
                *r2 = y - rlb.a2 * z;
                *a += z * z;
                if keep {
                    squares[i].push(z * z);
                }
            }
        });
        for i in 0..N {
            sums[i].push(acc[i]);
            s[i] = s[i].map(|v| if v.abs() < 1e-30 { 0.0 } else { v });
        }
        at += len;
    }
    for (c, ((st, sum), sq)) in chans.iter_mut().zip(s.into_iter().zip(sums).zip(squares)) {
        c.s = st;
        c.sums = sum;
        c.squares = sq;
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

pub(crate) fn true_peak<const N: usize>(
    buffer: &[f32],
    bank: &[[f32; N]],
    scratch: &mut [f32],
) -> f32 {
    if bank.is_empty() {
        return 0.0;
    }
    let n = buffer.len().saturating_sub(N - 1);
    let mut peak = 0.0f32;
    let mut start = 0;
    while start < n {
        let len = (n - start).min(scratch.len());
        let out = &mut scratch[..len];
        interpolated(&buffer[start..start + len + N - 1], bank, out, peak);
        peak = peak.max(self::peak(out));
        start += len;
    }
    peak
}

pub(crate) fn interpolated<const N: usize>(
    buffer: &[f32],
    bank: &[[f32; N]],
    out: &mut [f32],
    floor: f32,
) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma") {
        unsafe { interpolated_fma(buffer, bank, out, floor) };
        return;
    }
    interpolated_plain(buffer, bank, out, floor)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn interpolated_fma<const N: usize>(
    buffer: &[f32],
    bank: &[[f32; N]],
    out: &mut [f32],
    floor: f32,
) {
    interpolated_plain(buffer, bank, out, floor)
}

#[inline(always)]
fn interpolated_plain<const N: usize>(
    buffer: &[f32],
    bank: &[[f32; N]],
    out: &mut [f32],
    floor: f32,
) {
    const B: usize = 64;
    let n = out.len();
    assert!(buffer.len() >= n + N - 1);
    let gain = bank
        .iter()
        .map(|c| c.iter().map(|v| v.abs()).sum::<f32>())
        .fold(0.0f32, f32::max)
        * 1.000_01;
    let mut o = 0;
    while o + B <= n {
        if peak(&buffer[o..o + B + N - 1]) * gain <= floor {
            out[o..o + B].fill(0.0);
            o += B;
            continue;
        }
        let mut m = [0.0f32; B];
        for c in bank {
            let mut acc = [0.0f32; B];
            for (j, &w) in c.iter().enumerate() {
                let x: &[f32; B] = buffer[o + N - 1 - j..o + N - 1 - j + B].try_into().unwrap();
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
                    .map(|(j, &w)| w * buffer[i + N - 1 - j])
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
    keep: bool,
}

#[derive(Debug, Clone)]
pub struct LoudnessMeter {
    rate: u32,
    weights: Vec<f64>,
    filters: Filters,
    over: Oversampler,
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
    integrated: Vec<f64>,
    range: Vec<f64>,
    history: Option<History>,
}

#[derive(Debug, Clone)]
struct History {
    energy: Vec<f64>,
    at: usize,
}

impl History {
    fn push(&mut self, e: f64) {
        let n = self.energy.len();
        self.energy[self.at] = e;
        self.at = (self.at + 1) % n;
    }

    fn mean(&self, frames: usize) -> f64 {
        let n = self.energy.len();
        let frames = frames.clamp(1, n);
        let mut sum = 0.0;
        for k in 0..frames {
            sum += self.energy[(self.at + n - 1 - k) % n];
        }
        sum / frames as f64
    }
}

impl LoudnessMeter {
    pub fn new(rate: u32, channels: u16) -> Result<Self> {
        if rate == 0 || channels == 0 {
            return Err(Error::Invalid(format!(
                "loudness meter at {rate} Hz with {channels} channels"
            )));
        }
        let r = rate as f64;
        let over = Oversampler::new(rate);
        Ok(LoudnessMeter {
            rate,
            weights: weights(channels as usize),
            filters: Filters {
                pre: prefilter(r),
                rlb: rlb(r),
                keep: false,
            },
            channels: vec![Channel::new(over.lead()); channels as usize],
            over,
            segments: Vec::new(),
            block: ((rate as usize + 5) / 10).max(1),
            position: 0,
            ring: [0.0; SHORT],
            ring_at: 0,
            seen: 0,
            momentary: f64::NEG_INFINITY,
            short_term: f64::NEG_INFINITY,
            momentary_max: f64::NEG_INFINITY,
            short_term_max: f64::NEG_INFINITY,
            integrated: Vec::new(),
            range: Vec::new(),
            history: None,
        })
    }

    pub fn windows(mut self) -> Result<Self> {
        if self.rate > MAX_WINDOW_RATE {
            return Err(Error::Invalid(format!(
                "window history at {} Hz is above {MAX_WINDOW_RATE} Hz",
                self.rate
            )));
        }
        let frames = (self.rate as usize * 3).max(SHORT * self.block);
        let mut energy = Vec::new();
        energy
            .try_reserve_exact(frames)
            .map_err(|_| Error::Invalid("no memory for the window history".into()))?;
        energy.resize(frames, 0.0);
        self.filters.keep = true;
        self.history = Some(History { energy, at: 0 });
        Ok(self)
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
        self.add(&input.samples)
    }

    pub fn add(&mut self, samples: &[f32]) -> Result<()> {
        let nch = self.channels.len();
        if !samples.len().is_multiple_of(nch) {
            return Err(Error::Invalid(format!(
                "meter expects whole frames of {nch} channels, got {} samples",
                samples.len()
            )));
        }
        let frames = samples.len() / nch;
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
            for (o, f) in ch.buffer[start..].iter_mut().zip(samples.chunks_exact(nch)) {
                *o = f[c];
            }
        }
        let (filters, segments, over) = (self.filters, &self.segments, &self.over);
        #[cfg(feature = "filter")]
        if frames * nch >= PARALLEL && nch > 1 {
            self.channels
                .par_chunks_mut(2)
                .for_each(|pair| run(pair, &filters, segments, over));
        } else {
            self.channels
                .chunks_mut(2)
                .for_each(|pair| run(pair, &filters, segments, over));
        }
        #[cfg(not(feature = "filter"))]
        self.channels
            .chunks_mut(2)
            .for_each(|pair| run(pair, &filters, segments, over));
        if let Some(h) = &mut self.history {
            for t in 0..frames {
                let e: f64 = self
                    .channels
                    .iter()
                    .zip(&self.weights)
                    .map(|(c, w)| w * c.squares[t])
                    .sum();
                h.push(e);
            }
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
        let gate = gate_energy();
        if self.seen >= MOMENTARY as u64 {
            if em >= gate {
                self.integrated.push(em);
            }
            self.momentary_max = self.momentary_max.max(self.momentary);
        }
        if self.seen >= SHORT as u64 {
            if (self.seen - SHORT as u64).is_multiple_of(RANGE_EVERY) && es >= gate {
                self.range.push(es);
            }
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
        let blocks = &self.integrated;
        let Some(gate) = relative_threshold(blocks) else {
            return f64::NEG_INFINITY;
        };
        let (sum, count) = blocks
            .iter()
            .filter(|&&e| e >= gate)
            .fold((0.0f64, 0u64), |(s, n), &e| (s + e, n + 1));
        if count == 0 {
            f64::NEG_INFINITY
        } else {
            lufs(sum / count as f64)
        }
    }

    pub fn momentary_now(&self) -> Result<f64> {
        self.recent(MOMENTARY * self.block)
    }

    pub fn short_term_now(&self) -> Result<f64> {
        self.recent(SHORT * self.block)
    }

    pub fn window(&self, ms: u32) -> Result<f64> {
        if ms == 0 || ms > 3000 {
            return Err(Error::Invalid(format!(
                "a loudness window of {ms} ms is outside 1 to 3000"
            )));
        }
        self.recent((u64::from(self.rate) * u64::from(ms) / 1000).max(1) as usize)
    }

    fn recent(&self, frames: usize) -> Result<f64> {
        let Some(h) = &self.history else {
            return Err(Error::Invalid(
                "the meter keeps no window history; build it with windows()".into(),
            ));
        };
        Ok(lufs(h.mean(frames)))
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    pub fn sample_peak_of(&self, channel: usize) -> Option<f64> {
        self.channels.get(channel).map(|c| f64::from(c.sample_peak))
    }

    pub fn true_peak_of(&self, channel: usize) -> Option<f64> {
        self.channels.get(channel).map(|c| f64::from(c.true_peak))
    }

    pub fn threshold(&self) -> f64 {
        relative_threshold(&self.integrated).map_or(ABSOLUTE_GATE_LUFS, lufs)
    }

    pub fn range(&self) -> f64 {
        let mut blocks = self.range.clone();
        blocks.sort_unstable_by(f64::total_cmp);
        if blocks.is_empty() {
            return 0.0;
        }
        let power = blocks.iter().sum::<f64>() / blocks.len() as f64;
        let gate = 10f64.powf(-20.0 / 10.0) * power;
        let below = blocks.iter().take_while(|&&v| v < gate).count();
        let Some(span) = (blocks.len() - below).checked_sub(1) else {
            return 0.0;
        };
        let span = span as f64;
        let high = blocks[below + (span * 0.95 + 0.5) as usize];
        let low = blocks[below + (span * 0.1 + 0.5) as usize];
        lufs(high) - lufs(low)
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
