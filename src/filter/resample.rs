use crate::media::{AudioBuffer, Error, Result};
use rayon::prelude::*;

const CROSSINGS: f64 = 48.0;
const ROLLOFF: f64 = 0.93;
const BETA: f64 = 9.0;
const MAX_PHASES: u64 = 2048;
const MAX_RATE: u32 = 1_536_000;
const MAX_RATIO: u32 = 256;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

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

#[derive(Debug, Clone)]
pub struct Resampler {
    from: u32,
    to: u32,
    channels: usize,
    up: u64,
    down: u64,
    phases: u64,
    taps: usize,
    table: Vec<f32>,
    history: Vec<Vec<f32>>,
    origin: i64,
    index: i64,
    phase: u64,
    consumed: i64,
    emitted: u64,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: u16) -> Result<Self> {
        if from == 0 || to == 0 || channels == 0 {
            return Err(Error::Invalid(format!(
                "resample {from} Hz to {to} Hz with {channels} channels"
            )));
        }
        if from.max(to) > MAX_RATE || from.max(to) / from.min(to) > MAX_RATIO {
            return Err(Error::Invalid(format!(
                "resample {from} Hz to {to} Hz: rates go up to {MAX_RATE} Hz and {MAX_RATIO} to 1"
            )));
        }
        let g = gcd(from as u64, to as u64);
        let (up, down) = (to as u64 / g, from as u64 / g);
        let phases = up.min(MAX_PHASES);
        let f = (up as f64 / down as f64).min(1.0) * ROLLOFF;
        let half = (CROSSINGS / f).ceil() as usize;
        let taps = 2 * half;
        let i0 = bessel_i0(BETA);
        let mut table = vec![0.0f32; (phases as usize + 1) * taps];
        for p in 0..=phases as usize {
            let frac = p as f64 / phases as f64;
            let row = &mut table[p * taps..(p + 1) * taps];
            let mut w = vec![0.0f64; taps];
            for (m, v) in w.iter_mut().enumerate() {
                let t = frac + (half as f64 - 1.0) - m as f64;
                let r = t / half as f64;
                let window = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0
                };
                let x = f * t;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
                };
                *v = f * sinc * window;
            }
            let sum: f64 = w.iter().sum();
            for (o, v) in row.iter_mut().zip(&w) {
                *o = (v / sum) as f32;
            }
        }
        let channels = channels as usize;
        Ok(Resampler {
            from,
            to,
            channels,
            up,
            down,
            phases,
            taps,
            table,
            history: vec![vec![0.0; half - 1]; channels],
            origin: -(half as i64 - 1),
            index: 0,
            phase: 0,
            consumed: 0,
            emitted: 0,
        })
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from, self.to)
    }

    pub fn process(&mut self, input: &AudioBuffer, out: &mut AudioBuffer) -> Result<()> {
        if input.rate != self.from || input.channels as usize != self.channels {
            return Err(Error::Invalid(format!(
                "resampler expects {} Hz {} channels, got {} Hz {} channels",
                self.from, self.channels, input.rate, input.channels
            )));
        }
        out.rate = self.to;
        out.channels = self.channels as u16;
        out.samples.clear();
        let frames = input.frames();
        for (c, h) in self.history.iter_mut().enumerate() {
            h.extend(
                input
                    .samples
                    .iter()
                    .skip(c)
                    .step_by(self.channels)
                    .take(frames),
            );
        }
        self.consumed += frames as i64;
        self.run(self.consumed, u64::MAX, out);
        Ok(())
    }

    pub fn flush(&mut self, out: &mut AudioBuffer) {
        out.rate = self.to;
        out.channels = self.channels as u16;
        out.samples.clear();
        let total = (self.consumed as u128 * self.up as u128).div_ceil(self.down as u128) as u64;
        let pad = self.taps / 2;
        for h in self.history.iter_mut() {
            h.extend(std::iter::repeat_n(0.0, pad));
        }
        self.run(self.consumed + pad as i64, total, out);
    }

    fn run(&mut self, available: i64, limit: u64, out: &mut AudioBuffer) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            unsafe { self.run_fma(available, limit, out) };
            return;
        }
        self.run_plain(available, limit, out);
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn run_fma(&mut self, available: i64, limit: u64, out: &mut AudioBuffer) {
        self.run_plain(available, limit, out);
    }

    #[inline(always)]
    fn tap(&self, phase: u64, h: &[f32]) -> f32 {
        if self.phases == self.up {
            let p = phase as usize;
            dot(&self.table[p * self.taps..(p + 1) * self.taps], h)
        } else {
            let pos = phase as f64 * self.phases as f64 / self.up as f64;
            let p = pos.floor() as usize;
            let t = (pos - p as f64) as f32;
            let a = &self.table[p * self.taps..(p + 1) * self.taps];
            let b = &self.table[(p + 1) * self.taps..(p + 2) * self.taps];
            dot(a, h) * (1.0 - t) + dot(b, h) * t
        }
    }

    #[inline(always)]
    fn run_plain(&mut self, available: i64, limit: u64, out: &mut AudioBuffer) {
        let half = (self.taps / 2) as i64;
        while self.index + half < available && self.emitted < limit {
            let at = (self.index - (half - 1) - self.origin) as usize;
            for h in &self.history {
                out.samples
                    .push(self.tap(self.phase, &h[at..at + self.taps]));
            }
            self.emitted += 1;
            self.phase += self.down;
            self.index += (self.phase / self.up) as i64;
            self.phase %= self.up;
        }
        let keep_from = (self.index - (half - 1) - self.origin).max(0) as usize;
        let keep_from = keep_from.min(self.history.first().map_or(0, Vec::len));
        if keep_from > 0 {
            for h in self.history.iter_mut() {
                h.drain(..keep_from);
            }
            self.origin += keep_from as i64;
        }
    }

    fn render(&self, padded: &[Vec<f32>], first: usize, out: &mut [f32]) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        {
            unsafe { self.render_fma(padded, first, out) };
            return;
        }
        self.render_plain(padded, first, out);
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    unsafe fn render_fma(&self, padded: &[Vec<f32>], first: usize, out: &mut [f32]) {
        self.render_plain(padded, first, out);
    }

    #[inline(always)]
    fn render_plain(&self, padded: &[Vec<f32>], first: usize, out: &mut [f32]) {
        for (k, frame) in out.chunks_exact_mut(self.channels).enumerate() {
            let pos = (first + k) as u64 * self.down;
            let (index, phase) = ((pos / self.up) as usize, pos % self.up);
            for (o, h) in frame.iter_mut().zip(padded) {
                *o = self.tap(phase, &h[index..index + self.taps]);
            }
        }
    }
}

#[inline(always)]
pub(crate) fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 16];
    let mut ca = a.chunks_exact(16);
    let mut cb = b.chunks_exact(16);
    for (x, y) in (&mut ca).zip(&mut cb) {
        for i in 0..16 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for v in acc {
        s += v;
    }
    s
}

const BLOCK: usize = 2048;

pub fn resample(input: &AudioBuffer, rate: u32) -> Result<AudioBuffer> {
    if input.rate == rate {
        return Ok(input.clone());
    }
    let r = Resampler::new(input.rate, rate, input.channels)?;
    let ch = r.channels;
    let frames = input.frames();
    let half = r.taps / 2;
    let total = (frames as u128 * r.up as u128).div_ceil(r.down as u128) as usize;
    let padded: Vec<Vec<f32>> = (0..ch)
        .into_par_iter()
        .map(|c| {
            let mut h = Vec::with_capacity(frames + 2 * half);
            h.resize(half - 1, 0.0);
            h.extend(input.samples.iter().skip(c).step_by(ch).take(frames));
            h.resize(frames + 2 * half - 1, 0.0);
            h
        })
        .collect();
    let mut out = AudioBuffer::new(rate, input.channels);
    out.samples = vec![0.0; total * ch];
    out.samples
        .par_chunks_mut(BLOCK * ch)
        .enumerate()
        .for_each(|(b, chunk)| r.render(&padded, b * BLOCK, chunk));
    Ok(out)
}
