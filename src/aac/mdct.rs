use std::f64::consts::PI;
use std::sync::OnceLock;

use crate::media::fft::Fft;

pub struct Mdct {
    m: usize,
    fft: Fft,
    pre: Vec<[f32; 2]>,
    post: Vec<[f32; 2]>,
}

impl Mdct {
    pub fn new(m: usize) -> Mdct {
        let q = m / 2;
        let pre = (0..q)
            .map(|n| {
                let a = -PI * n as f64 / m as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        let post = (0..q)
            .map(|k| {
                let a = -PI * (k as f64 + 0.25) / m as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        Mdct {
            m,
            fft: Fft::new(q),
            pre,
            post,
        }
    }

    fn dct4(&self, u: &[f32], out: &mut [f32]) {
        let m = self.m;
        let q = m / 2;
        let mut re = vec![0f32; q];
        let mut im = vec![0f32; q];
        for n in 0..q {
            let (a, b) = (u[2 * n], u[m - 1 - 2 * n]);
            let [c, s] = self.pre[n];
            re[n] = a * c - b * s;
            im[n] = a * s + b * c;
        }
        self.fft.run(&mut re, &mut im);
        for k in 0..q {
            let [c, s] = self.post[k];
            let wr = re[k] * c - im[k] * s;
            let wi = re[k] * s + im[k] * c;
            out[2 * k] = wr;
            out[m - 1 - 2 * k] = -wi;
        }
    }

    pub fn forward(&self, x: &[f32], out: &mut [f32]) {
        let m = self.m;
        let h = m / 2;
        let mut u = vec![0f32; m];
        for n in 0..h {
            u[n] = -x[m + h - 1 - n] - x[m + h + n];
            u[h + n] = x[n] - x[m - 1 - n];
        }
        self.dct4(&u, out);
    }

    pub fn inverse(&self, spec: &[f32], y: &mut [f32]) {
        let m = self.m;
        let h = m / 2;
        let mut u = vec![0f32; m];
        self.dct4(spec, &mut u);
        for n in 0..h {
            y[n] = u[h + n];
            y[m - 1 - n] = -u[h + n];
            y[m + h - 1 - n] = -u[n];
            y[m + h + n] = -u[n];
        }
    }
}

pub fn mdct_long() -> &'static Mdct {
    static M: OnceLock<Mdct> = OnceLock::new();
    M.get_or_init(|| Mdct::new(1024))
}
pub fn mdct_short() -> &'static Mdct {
    static M: OnceLock<Mdct> = OnceLock::new();
    M.get_or_init(|| Mdct::new(128))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowShape {
    #[default]
    Sine,
    Kbd,
}

impl WindowShape {
    pub fn from_bit(b: bool) -> Self {
        if b {
            WindowShape::Kbd
        } else {
            WindowShape::Sine
        }
    }
    pub fn bit(self) -> u32 {
        (self == WindowShape::Kbd) as u32
    }
}

fn sine_half(m: usize) -> Vec<f32> {
    let n = 2 * m;
    (0..m)
        .map(|i| (PI / n as f64 * (i as f64 + 0.5)).sin() as f32)
        .collect()
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..60 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn kbd_half(m: usize, alpha: f64) -> Vec<f32> {
    let kernel: Vec<f64> = (0..=m)
        .map(|j| {
            let r = (j as f64 - m as f64 / 2.0) / (m as f64 / 2.0);
            bessel_i0(PI * alpha * (1.0 - r * r).max(0.0).sqrt())
        })
        .collect();
    let total: f64 = kernel.iter().sum();
    let mut acc = 0.0;
    (0..m)
        .map(|n| {
            acc += kernel[n];
            (acc / total).sqrt() as f32
        })
        .collect()
}

pub fn window(shape: WindowShape, long: bool) -> &'static [f32] {
    static W: OnceLock<[Vec<f32>; 4]> = OnceLock::new();
    let w = W.get_or_init(|| {
        [
            sine_half(1024),
            kbd_half(1024, 4.0),
            sine_half(128),
            kbd_half(128, 6.0),
        ]
    });
    let i = match (long, shape) {
        (true, WindowShape::Sine) => 0,
        (true, WindowShape::Kbd) => 1,
        (false, WindowShape::Sine) => 2,
        (false, WindowShape::Kbd) => 3,
    };
    &w[i]
}
