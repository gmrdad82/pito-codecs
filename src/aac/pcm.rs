use crate::media::AudioBuffer;
use std::f64::consts::PI;
use std::sync::Arc;

const K: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn stereo_matrix(channels: usize) -> Vec<(f32, f32)> {
    match channels {
        1 => vec![(1.0, 1.0)],
        2 => vec![(1.0, 0.0), (0.0, 1.0)],
        3 => vec![(1.0, 0.0), (0.0, 1.0), (K, K)],
        4 => vec![(1.0, 0.0), (0.0, 1.0), (K, K), (0.5, 0.5)],
        5 => vec![(1.0, 0.0), (0.0, 1.0), (K, K), (K, 0.0), (0.0, K)],
        6 => vec![
            (1.0, 0.0),
            (0.0, 1.0),
            (K, K),
            (0.0, 0.0),
            (K, 0.0),
            (0.0, K),
        ],
        7 => vec![
            (1.0, 0.0),
            (0.0, 1.0),
            (K, K),
            (0.0, 0.0),
            (0.5, 0.5),
            (K, 0.0),
            (0.0, K),
        ],
        8 => vec![
            (1.0, 0.0),
            (0.0, 1.0),
            (K, K),
            (0.0, 0.0),
            (K, 0.0),
            (0.0, K),
            (K, 0.0),
            (0.0, K),
        ],
        n => {
            let left = n.div_ceil(2) as f32;
            let right = (n / 2).max(1) as f32;
            (0..n)
                .map(|c| {
                    if c % 2 == 0 {
                        (1.0 / left, 0.0)
                    } else {
                        (0.0, 1.0 / right)
                    }
                })
                .collect()
        }
    }
}

pub fn downmix_stereo(audio: &AudioBuffer) -> AudioBuffer {
    let channels = audio.channels as usize;
    if channels == 2 || channels == 0 {
        return audio.clone();
    }
    let m = stereo_matrix(channels);
    let mut out = AudioBuffer::new(audio.rate, 2);
    out.samples.reserve(audio.frames() * 2);
    for frame in audio.samples.chunks_exact(channels) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (x, (a, b)) in frame.iter().zip(&m) {
            l += x * a;
            r += x * b;
        }
        out.samples.push(l);
        out.samples.push(r);
    }
    out
}

pub fn mono(audio: &AudioBuffer) -> AudioBuffer {
    if audio.channels <= 1 {
        return audio.clone();
    }
    let stereo = downmix_stereo(audio);
    let mut out = AudioBuffer::new(audio.rate, 1);
    out.samples = stereo
        .samples
        .chunks_exact(2)
        .map(|f| 0.5 * (f[0] + f[1]))
        .collect();
    out
}

pub fn gain(audio: &mut AudioBuffer, db: f32) {
    let g = 10f32.powf(db / 20.0);
    for s in &mut audio.samples {
        *s *= g;
    }
}

pub fn resample(audio: &AudioBuffer, rate: u32) -> AudioBuffer {
    if audio.rate == rate || audio.channels == 0 {
        let mut out = audio.clone();
        out.rate = rate;
        return out;
    }
    let mut r = Resampler::new(audio.rate, rate, audio.channels);
    let mut out = r.process(audio);
    out.samples.extend(r.flush().samples);
    out
}

const ZERO_CROSSINGS: f64 = 12.0;
const ROLLOFF: f64 = 0.92;
const BETA: f64 = 8.6;
const MAX_PHASES: u64 = 4096;

struct Kernel {
    taps: usize,
    phases: u64,
    weights: Vec<f32>,
}

fn bessel_i0(x: f64) -> f64 {
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

impl Kernel {
    fn new(from: u32, to: u32, phases: u64) -> Self {
        let fc = 0.5 * ROLLOFF * (to as f64 / from as f64).min(1.0);
        let half = (ZERO_CROSSINGS / (2.0 * fc)).ceil() as usize;
        let taps = 2 * half;
        let norm = bessel_i0(BETA);
        let mut weights = Vec::with_capacity(taps * phases as usize);
        for p in 0..phases {
            let frac = p as f64 / phases as f64;
            let start = weights.len();
            for j in 0..taps {
                let x = j as f64 - (half as f64 - 1.0) - frac;
                let w = x / (half as f64 + 1.0);
                let window = if w.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - w * w).sqrt()) / norm
                };
                let arg = 2.0 * fc * x;
                let sinc = if arg.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * arg).sin() / (PI * arg)
                };
                weights.push((2.0 * fc * sinc * window) as f32);
            }
            let sum: f32 = weights[start..].iter().sum();
            if sum.abs() > 1e-9 {
                for w in &mut weights[start..] {
                    *w /= sum;
                }
            }
        }
        Self {
            taps,
            phases,
            weights,
        }
    }
}

pub struct Resampler {
    from: u32,
    to: u32,
    channels: usize,
    up: u64,
    down: u64,
    kernel: Arc<Kernel>,
    input: Vec<f32>,
    base: i64,
    total_in: u64,
    next_out: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: u16) -> Self {
        let (from, to) = (from.max(1), to.max(1));
        let g = gcd(from as u64, to as u64);
        let (up, down) = (to as u64 / g, from as u64 / g);
        let kernel = Arc::new(Kernel::new(from, to, up.min(MAX_PHASES)));
        let channels = channels.max(1) as usize;
        let half = kernel.taps / 2;
        Self {
            from,
            to,
            channels,
            up,
            down,
            kernel,
            input: vec![0.0; half * channels],
            base: -(half as i64),
            total_in: 0,
            next_out: 0,
        }
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from, self.to)
    }

    pub fn process(&mut self, audio: &AudioBuffer) -> AudioBuffer {
        let take = audio.samples.len() / self.channels * self.channels;
        self.input.extend_from_slice(&audio.samples[..take]);
        self.total_in += (take / self.channels) as u64;
        self.run(false)
    }

    pub fn flush(&mut self) -> AudioBuffer {
        let pad = self.kernel.taps * self.channels;
        self.input.extend(std::iter::repeat_n(0.0, pad));
        self.run(true)
    }

    fn run(&mut self, last: bool) -> AudioBuffer {
        let ch = self.channels;
        let taps = self.kernel.taps;
        let half = (taps / 2) as i64;
        let available = self.base + (self.input.len() / ch) as i64;
        let limit = if last {
            (self.total_in * self.up).div_ceil(self.down)
        } else {
            u64::MAX
        };
        let mut out = AudioBuffer::new(self.to, ch as u16);
        let mut acc = vec![0.0f32; ch];
        while self.next_out < limit {
            let pos = self.next_out as u128 * self.down as u128;
            let i = (pos / self.up as u128) as i64;
            let rem = (pos % self.up as u128) as u64;
            let phase = if self.kernel.phases == self.up {
                rem
            } else {
                ((rem as u128 * self.kernel.phases as u128 + self.up as u128 / 2) / self.up as u128)
                    as u64
            };
            let (i, phase) = if phase == self.kernel.phases {
                (i + 1, 0)
            } else {
                (i, phase)
            };
            let first = i - half + 1;
            if first + taps as i64 > available {
                break;
            }
            let w = &self.kernel.weights[phase as usize * taps..(phase as usize + 1) * taps];
            let start = (first - self.base) as usize * ch;
            acc.fill(0.0);
            if ch == 1 {
                let x = &self.input[start..start + taps];
                acc[0] = x.iter().zip(w).map(|(a, b)| a * b).sum();
            } else {
                for (j, wj) in w.iter().enumerate() {
                    let frame = &self.input[start + j * ch..start + (j + 1) * ch];
                    for (a, x) in acc.iter_mut().zip(frame) {
                        *a += x * wj;
                    }
                }
            }
            out.samples.extend_from_slice(&acc);
            self.next_out += 1;
        }
        let pos = self.next_out as u128 * self.down as u128;
        let keep_from = (pos / self.up as u128) as i64 - half;
        let drop = (keep_from - self.base).clamp(0, (self.input.len() / ch) as i64) as usize;
        self.input.drain(..drop * ch);
        self.base += drop as i64;
        out
    }
}
