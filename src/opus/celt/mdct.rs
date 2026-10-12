#[derive(Clone, Copy, Default, Debug)]
pub struct Cpx {
    pub re: f32,
    pub im: f32,
}

impl Cpx {
    #[inline]
    pub(crate) fn mul(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    #[inline]
    fn sub(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
    #[inline]
    fn add(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}

struct Stage {
    p: usize,
    m: usize,
    tw: Vec<Cpx>,
}

pub struct Fft {
    n: usize,
    perm: Vec<u32>,
    stages: Vec<Stage>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        let mut factors = Vec::new();
        let mut m = n;
        for p in [4usize, 2, 3, 5] {
            while m.is_multiple_of(p) {
                factors.push(p);
                m /= p;
            }
        }
        assert_eq!(m, 1, "unsupported FFT size {n}");
        let mut perm = vec![0u32; n];
        permute(&mut perm, &factors, 0, 0, 1, n, 0);
        let mut stages = Vec::new();
        for fi in (0..factors.len()).rev() {
            let p = factors[fi];
            let level: usize = factors[fi..].iter().product();
            let m = level / p;
            let stride = n / level;
            let mut tw = Vec::with_capacity((p - 1) * m);
            for q in 1..p {
                for k in 0..m {
                    let ph = -2.0 * std::f64::consts::PI * (q * k * stride) as f64 / n as f64;
                    tw.push(Cpx {
                        re: ph.cos() as f32,
                        im: ph.sin() as f32,
                    });
                }
            }
            stages.push(Stage { p, m, tw });
        }
        Fft { n, perm, stages }
    }

    pub fn process(&self, data: &mut [Cpx], scratch: &mut Vec<Cpx>) {
        scratch.clear();
        scratch.extend_from_slice(&data[..self.n]);
        for (d, &i) in data[..self.n].iter_mut().zip(&self.perm) {
            *d = scratch[i as usize];
        }
        for st in &self.stages {
            for b in data[..self.n].chunks_exact_mut(st.p * st.m) {
                butterfly(b, st.p, st.m, &st.tw);
            }
        }
    }
}

fn permute(
    perm: &mut [u32],
    factors: &[usize],
    out: usize,
    base: usize,
    stride: usize,
    n: usize,
    fi: usize,
) {
    let p = factors[fi];
    let m = n / p;
    for q in 0..p {
        if m == 1 {
            perm[out + q] = (base + q * stride) as u32;
        } else {
            permute(
                perm,
                factors,
                out + q * m,
                base + q * stride,
                stride * p,
                m,
                fi + 1,
            );
        }
    }
}

#[inline]
fn butterfly(b: &mut [Cpx], p: usize, m: usize, tw: &[Cpx]) {
    match p {
        4 => {
            for k in 0..m {
                let a0 = b[k];
                let a1 = b[m + k].mul(tw[k]);
                let a2 = b[2 * m + k].mul(tw[m + k]);
                let a3 = b[3 * m + k].mul(tw[2 * m + k]);
                let t0 = a0.add(a2);
                let t1 = a0.sub(a2);
                let t2 = a1.add(a3);
                let t3 = a1.sub(a3);
                let r = Cpx {
                    re: t3.im,
                    im: -t3.re,
                };
                b[k] = t0.add(t2);
                b[m + k] = t1.add(r);
                b[2 * m + k] = t0.sub(t2);
                b[3 * m + k] = t1.sub(r);
            }
        }
        2 => {
            for k in 0..m {
                let a0 = b[k];
                let a1 = b[m + k].mul(tw[k]);
                b[k] = a0.add(a1);
                b[m + k] = a0.sub(a1);
            }
        }
        3 => {
            const S3: f32 = 0.866_025_4;
            for k in 0..m {
                let a0 = b[k];
                let a1 = b[m + k].mul(tw[k]);
                let a2 = b[2 * m + k].mul(tw[m + k]);
                let t = a1.add(a2);
                let d = a1.sub(a2);
                let mid = Cpx {
                    re: a0.re - 0.5 * t.re,
                    im: a0.im - 0.5 * t.im,
                };
                let s = Cpx {
                    re: S3 * d.im,
                    im: -S3 * d.re,
                };
                b[k] = a0.add(t);
                b[m + k] = mid.add(s);
                b[2 * m + k] = mid.sub(s);
            }
        }
        _ => {
            const C1: f32 = 0.309_017;
            const S1: f32 = 0.951_056_5;
            const C2: f32 = -0.809_017;
            const S2: f32 = 0.587_785_25;
            for k in 0..m {
                let a0 = b[k];
                let a1 = b[m + k].mul(tw[k]);
                let a2 = b[2 * m + k].mul(tw[m + k]);
                let a3 = b[3 * m + k].mul(tw[2 * m + k]);
                let a4 = b[4 * m + k].mul(tw[3 * m + k]);
                let b1 = a1.add(a4);
                let b2 = a2.add(a3);
                let d1 = a1.sub(a4);
                let d2 = a2.sub(a3);
                let r1 = Cpx {
                    re: a0.re + C1 * b1.re + C2 * b2.re,
                    im: a0.im + C1 * b1.im + C2 * b2.im,
                };
                let r2 = Cpx {
                    re: a0.re + C2 * b1.re + C1 * b2.re,
                    im: a0.im + C2 * b1.im + C1 * b2.im,
                };
                let i1 = Cpx {
                    re: S1 * d1.im + S2 * d2.im,
                    im: -(S1 * d1.re + S2 * d2.re),
                };
                let i2 = Cpx {
                    re: S2 * d1.im - S1 * d2.im,
                    im: -(S2 * d1.re - S1 * d2.re),
                };
                b[k] = a0.add(b1).add(b2);
                b[m + k] = r1.add(i1);
                b[4 * m + k] = r1.sub(i1);
                b[2 * m + k] = r2.add(i2);
                b[3 * m + k] = r2.sub(i2);
            }
        }
    }
}

pub struct Imdct {
    n: usize,
    trig: Vec<f32>,
    fft: Fft,
}

impl Imdct {
    pub fn new(n: usize) -> Imdct {
        let trig = (0..n / 2)
            .map(|i| (2.0 * std::f64::consts::PI * (i as f64 + 0.125) / n as f64).cos() as f32)
            .collect();
        Imdct {
            n,
            trig,
            fft: Fft::new(n / 4),
        }
    }

    pub fn backward(
        &self,
        input: &[f32],
        stride: usize,
        out: &mut [f32],
        window: &[f32],
        overlap: usize,
        work: &mut ImdctScratch,
    ) {
        let n2 = self.n >> 1;
        let n4 = self.n >> 2;
        let t = &self.trig;
        let z = &mut work.z;
        z.clear();
        for i in 0..n4 {
            let x1 = input[2 * i * stride];
            let x2 = input[stride * (n2 - 1 - 2 * i)];
            let yr = x2 * t[i] + x1 * t[n4 + i];
            let yi = x1 * t[i] - x2 * t[n4 + i];
            z.push(Cpx { re: yi, im: yr });
        }
        self.fft.process(z, &mut work.scratch);
        let base = overlap >> 1;
        let y = &mut out[base..base + n2];
        for i in 0..n4.div_ceil(2) {
            let front = z[i];
            let back = z[n4 - 1 - i];
            let (re, im) = (front.im, front.re);
            let (t0, t1) = (t[i], t[n4 + i]);
            let yr0 = re * t0 + im * t1;
            let yi0 = re * t1 - im * t0;
            let (re, im) = (back.im, back.re);
            let (t0, t1) = (t[n4 - i - 1], t[n2 - i - 1]);
            let yr1 = re * t0 + im * t1;
            let yi1 = re * t1 - im * t0;
            y[2 * i] = yr0;
            y[n2 - 1 - 2 * i] = yi0;
            y[n2 - 2 - 2 * i] = yr1;
            y[2 * i + 1] = yi1;
        }
        for i in 0..overlap / 2 {
            let x1 = out[overlap - 1 - i];
            let x2 = out[i];
            let w1 = window[i];
            let w2 = window[overlap - 1 - i];
            out[i] = x2 * w2 - x1 * w1;
            out[overlap - 1 - i] = x2 * w1 + x1 * w2;
        }
    }
}

#[derive(Default)]
pub struct ImdctScratch {
    z: Vec<Cpx>,
    scratch: Vec<Cpx>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_matches_dft() {
        for &n in &[60usize, 120, 240, 480] {
            let fft = Fft::new(n);
            let x: Vec<Cpx> = (0..n)
                .map(|i| Cpx {
                    re: ((i * 7) % 13) as f32 - 6.0,
                    im: ((i * 3) % 5) as f32 - 2.0,
                })
                .collect();
            let mut y = x.clone();
            fft.process(&mut y, &mut Vec::new());
            for k in [0, 1, 7, n / 2, n - 1] {
                let mut acc = (0f64, 0f64);
                for (i, v) in x.iter().enumerate() {
                    let ph = -2.0 * std::f64::consts::PI * (i * k) as f64 / n as f64;
                    acc.0 += v.re as f64 * ph.cos() - v.im as f64 * ph.sin();
                    acc.1 += v.re as f64 * ph.sin() + v.im as f64 * ph.cos();
                }
                assert!(
                    (acc.0 - y[k].re as f64).abs() < 1e-2 && (acc.1 - y[k].im as f64).abs() < 1e-2,
                    "n={n} k={k}"
                );
            }
        }
    }

    #[test]
    fn imdct_matches_direct_formula() {
        let n = 240;
        let n2 = n / 2;
        let im = Imdct::new(n);
        let x: Vec<f32> = (0..n2)
            .map(|k| ((k * 37 % 11) as f32 - 5.0) / 5.0)
            .collect();
        let overlap = 0;
        let mut out = vec![0f32; n2];
        im.backward(&x, 1, &mut out, &[], overlap, &mut ImdctScratch::default());
        let y: Vec<f64> = (0..n)
            .map(|t| {
                (0..n2)
                    .map(|k| {
                        x[k] as f64
                            * (2.0 * std::f64::consts::PI / n as f64
                                * (t as f64 + 0.5 + n as f64 / 4.0)
                                * (k as f64 + 0.5))
                                .cos()
                    })
                    .sum()
            })
            .collect();
        let mid: Vec<f64> = (0..n2).map(|i| y[n / 4 + i]).collect();
        let dot: f64 = mid.iter().zip(&out).map(|(a, b)| a * *b as f64).sum();
        let ea: f64 = mid.iter().map(|a| a * a).sum();
        let eb: f64 = out.iter().map(|b| (*b as f64).powi(2)).sum();
        let corr = dot.abs() / (ea.sqrt() * eb.sqrt());
        assert!(corr > 0.999, "corr {corr}");
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    #[test]
    #[ignore]
    fn imdct_speed() {
        let im = Imdct::new(1920);
        let x: Vec<f32> = (0..960)
            .map(|k| ((k * 37 % 11) as f32 - 5.0) / 5.0)
            .collect();
        let window = vec![0.5f32; 120];
        let mut out = vec![0f32; 1200];
        let mut work = ImdctScratch::default();
        let t = std::time::Instant::now();
        for _ in 0..12000 {
            im.backward(&x, 1, &mut out, &window, 120, &mut work);
        }
        eprintln!("12000 imdct 1920: {:?}", t.elapsed());
    }
}
