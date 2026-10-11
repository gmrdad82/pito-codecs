const TAPS: usize = 193;
pub(crate) const DELAY_48K: usize = (TAPS - 1) / 2;

pub(crate) struct Downsampler {
    factor: usize,
    taps: [f32; TAPS],
    buf: Vec<f32>,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..40 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

impl Downsampler {
    pub(crate) fn new(fs_khz: usize) -> Downsampler {
        let factor = 48 / fs_khz;
        let out_nyq = 24000.0 / factor as f64;
        let cutoff = (out_nyq - 450.0) / 48000.0;
        let beta = 6.76;
        let m = (TAPS - 1) as f64 / 2.0;
        let norm = bessel_i0(beta);
        let mut h = [0f64; TAPS];
        for (k, v) in h.iter_mut().enumerate() {
            let t = k as f64 - m;
            let sinc = if t == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * t).sin() / (std::f64::consts::PI * t)
            };
            let r = t / m;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / norm;
            *v = sinc * w;
        }
        let sum: f64 = h.iter().sum();
        let mut taps = [0f32; TAPS];
        for (t, v) in taps.iter_mut().zip(h.iter()) {
            *t = (v / sum) as f32;
        }
        Downsampler {
            factor,
            taps,
            buf: vec![0.0; TAPS - 1],
        }
    }

    pub(crate) fn factor(&self) -> usize {
        self.factor
    }

    pub(crate) fn process(&mut self, input: impl Iterator<Item = f32>, out: &mut [f32]) {
        self.buf.extend(input);
        let n_in = self.buf.len() - (TAPS - 1);
        debug_assert_eq!(n_in, out.len() * self.factor);
        for (i, o) in out.iter_mut().enumerate() {
            let start = i * self.factor + self.factor - 1;
            let w = &self.buf[start..start + TAPS];
            let mut acc = [0f32; 8];
            for (cw, ct) in w.chunks_exact(8).zip(self.taps.chunks_exact(8)) {
                for j in 0..8 {
                    acc[j] += cw[j] * ct[j];
                }
            }
            let mut s = acc.iter().sum::<f32>();
            for j in TAPS / 8 * 8..TAPS {
                s += w[j] * self.taps[j];
            }
            *o = s;
        }
        self.buf.drain(..n_in);
    }
}
