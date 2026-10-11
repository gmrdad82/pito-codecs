use super::loudness::{Loudness, LoudnessMeter, TAPS, interpolated, oversampling, polyphase};
use super::mix::gain;
use crate::media::{AudioBuffer, Error, Result};
use std::collections::VecDeque;

const LOOKAHEAD: f64 = 0.005;
const RELEASE: f64 = 0.08;
const CENTRE: usize = TAPS / 2;

#[derive(Debug, Clone)]
pub struct Limiter {
    channels: usize,
    ceiling: f32,
    look: usize,
    release: f32,
    bank: Vec<[f32; TAPS]>,
    buffers: Vec<Vec<f32>>,
    peaks: Vec<f32>,
    scratch: Vec<f32>,
    mins: VecDeque<(u64, f32)>,
    boxed: Vec<f32>,
    boxed_sum: f64,
    boxed_at: usize,
    released: f32,
    delay: Vec<f32>,
    delay_at: usize,
    step: u64,
}

impl Limiter {
    pub fn new(rate: u32, channels: u16, ceiling_db: f64) -> Result<Self> {
        if rate == 0 || channels == 0 {
            return Err(Error::Invalid(format!(
                "limiter at {rate} Hz with {channels} channels"
            )));
        }
        let look = ((rate as f64 * LOOKAHEAD).round() as usize).max(1);
        let latency = look - 1 + CENTRE;
        let channels = channels as usize;
        Ok(Limiter {
            channels,
            ceiling: 10f64.powf(ceiling_db / 20.0) as f32,
            look,
            release: (1.0 - (-1.0 / (RELEASE * rate as f64)).exp()) as f32,
            bank: polyphase(oversampling(rate as f64).max(2)),
            buffers: vec![vec![0.0; TAPS - 1]; channels],
            peaks: Vec::new(),
            scratch: vec![0.0; 1024],
            mins: VecDeque::with_capacity(look + 2),
            boxed: vec![1.0; look],
            boxed_sum: look as f64,
            boxed_at: 0,
            released: 1.0,
            delay: vec![0.0; (latency + 1) * channels],
            delay_at: 0,
            step: 0,
        })
    }

    pub fn latency(&self) -> usize {
        self.look - 1 + CENTRE
    }

    pub fn process(&mut self, input: &AudioBuffer, out: &mut AudioBuffer) -> Result<()> {
        if input.channels as usize != self.channels {
            return Err(Error::Invalid(format!(
                "limiter expects {} channels, got {}",
                self.channels, input.channels
            )));
        }
        out.rate = input.rate;
        out.channels = input.channels;
        self.run(&input.samples, out);
        Ok(())
    }

    pub fn flush(&mut self, out: &mut AudioBuffer) {
        out.channels = self.channels as u16;
        let silence = vec![0.0; self.latency() * self.channels];
        self.run(&silence, out);
    }

    fn run(&mut self, input: &[f32], out: &mut AudioBuffer) {
        let nch = self.channels;
        let frames = input.len() / nch;
        out.samples.clear();
        out.samples.reserve(frames * nch);
        self.peaks.clear();
        self.peaks.resize(frames, 0.0);
        for (c, buffer) in self.buffers.iter_mut().enumerate() {
            buffer.extend(input.iter().skip(c).step_by(nch).take(frames));
            peaks(
                buffer,
                &self.bank,
                &mut self.scratch,
                &mut self.peaks,
                self.ceiling,
            );
            let n = buffer.len();
            buffer.copy_within(n - (TAPS - 1).., 0);
            buffer.truncate(TAPS - 1);
        }
        let slots = self.delay.len() / nch;
        for (f, frame) in input.chunks_exact(nch).enumerate() {
            let p = self.peaks[f];
            let need = if p > self.ceiling {
                self.ceiling / p
            } else {
                1.0
            };
            while self.mins.back().is_some_and(|&(_, g)| g >= need) {
                self.mins.pop_back();
            }
            self.mins.push_back((self.step, need));
            while self
                .mins
                .front()
                .is_some_and(|&(i, _)| i + (self.look as u64) < self.step)
            {
                self.mins.pop_front();
            }
            let h = self.mins.front().map_or(1.0, |&(_, g)| g);
            self.released = h.min(self.released + (1.0 - self.released) * self.release);
            self.boxed_sum += (self.released - self.boxed[self.boxed_at]) as f64;
            self.boxed[self.boxed_at] = self.released;
            self.boxed_at = (self.boxed_at + 1) % self.look;
            let g = (self.boxed_sum / self.look as f64) as f32;
            let at = self.delay_at * nch;
            let oldest = ((self.delay_at + 1) % slots) * nch;
            self.delay[at..at + nch].copy_from_slice(frame);
            for c in 0..nch {
                out.samples.push(self.delay[oldest + c] * g);
            }
            self.delay_at = (self.delay_at + 1) % slots;
            self.step += 1;
        }
    }
}

fn peaks(buffer: &[f32], bank: &[[f32; TAPS]], scratch: &mut [f32], peaks: &mut [f32], floor: f32) {
    let n = peaks.len();
    let mut start = 0;
    while start < n {
        let len = (n - start).min(scratch.len());
        let out = &mut scratch[..len];
        interpolated(&buffer[start..start + len + TAPS - 1], bank, out, floor);
        for (p, &v) in peaks[start..start + len].iter_mut().zip(out.iter()) {
            *p = p.max(v);
        }
        start += len;
    }
}

pub fn normalize_gain_db(measured: &Loudness, target_lufs: f64) -> f64 {
    if measured.integrated.is_finite() && target_lufs.is_finite() {
        target_lufs - measured.integrated
    } else {
        0.0
    }
}

pub fn normalize(buffer: &mut AudioBuffer, target_lufs: f64, ceiling_db: f64) -> Result<Loudness> {
    let mut first = None;
    for _ in 0..3 {
        let mut meter = LoudnessMeter::new(buffer.rate, buffer.channels)?;
        meter.process(buffer)?;
        let now = meter.summary();
        first.get_or_insert(now);
        let g = normalize_gain_db(&now, target_lufs);
        if g.abs() < 0.05 && now.true_peak <= ceiling_db + 0.05 {
            break;
        }
        gain(buffer, g as f32);
        if now.true_peak + g > ceiling_db {
            limit(buffer, ceiling_db)?;
        }
    }
    first.ok_or_else(|| Error::Invalid("nothing measured".into()))
}

fn limit(buffer: &mut AudioBuffer, ceiling_db: f64) -> Result<()> {
    let mut limiter = Limiter::new(buffer.rate, buffer.channels, ceiling_db)?;
    let mut out = AudioBuffer::new(buffer.rate, buffer.channels);
    let mut tail = AudioBuffer::new(buffer.rate, buffer.channels);
    limiter.process(buffer, &mut out)?;
    limiter.flush(&mut tail);
    out.samples.extend_from_slice(&tail.samples);
    let skip = limiter.latency() * buffer.channels as usize;
    buffer.samples.clear();
    buffer.samples.extend_from_slice(&out.samples[skip..]);
    Ok(())
}
