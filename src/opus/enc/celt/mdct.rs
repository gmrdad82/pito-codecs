use crate::opus::celt::mdct::{Cpx, Fft};

pub struct Mdct {
    n2: usize,
    pre: Vec<Cpx>,
    post: Vec<Cpx>,
    fft: Fft,
}

#[derive(Default)]
pub struct MdctScratch {
    u: Vec<f32>,
    z: Vec<Cpx>,
    fft: Vec<Cpx>,
}

impl Mdct {
    pub fn new(n2: usize) -> Mdct {
        let n4 = n2 / 2;
        let m = n2 as f64;
        let rot = |k: usize| {
            let ph = -std::f64::consts::PI * (4 * k + 1) as f64 / (4.0 * m);
            Cpx {
                re: ph.cos() as f32,
                im: ph.sin() as f32,
            }
        };
        let scale = 1.0 / n4 as f64;
        let pre = (0..n4).map(rot).collect();
        let post = (0..n4)
            .map(|k| {
                let ph = -std::f64::consts::PI * k as f64 / m;
                Cpx {
                    re: (ph.cos() * scale) as f32,
                    im: (ph.sin() * scale) as f32,
                }
            })
            .collect();
        Mdct {
            n2,
            pre,
            post,
            fft: Fft::new(n4),
        }
    }

    pub fn forward(
        &self,
        input: &[f32],
        window: &[f32],
        out: &mut [f32],
        stride: usize,
        work: &mut MdctScratch,
    ) {
        let n2 = self.n2;
        let n4 = n2 / 2;
        let ov = window.len();
        let pad = (n2 - ov) / 2;
        let z = |m: usize| -> f32 {
            if m < pad || m >= pad + n2 + ov {
                return 0.0;
            }
            let j = m - pad;
            if j < ov {
                input[j] * window[j]
            } else if j < n2 {
                input[j]
            } else {
                input[j] * window[ov - 1 - (j - n2)]
            }
        };
        let u = &mut work.u;
        u.clear();
        u.resize(n2, 0.0);
        for n in 0..n4 {
            u[n] = -z(n2 + n4 - 1 - n) - z(n2 + n4 + n);
            u[n4 + n] = z(n) - z(n2 - 1 - n);
        }
        let zc = &mut work.z;
        zc.clear();
        for n in 0..n4 {
            let v = Cpx {
                re: u[2 * n],
                im: u[n2 - 1 - 2 * n],
            };
            zc.push(v.mul(self.pre[n]));
        }
        self.fft.process(zc, &mut work.fft);
        for k in 0..n4 {
            let d = zc[k].mul(self.post[k]);
            out[2 * k * stride] = d.re;
            out[(n2 - 1 - 2 * k) * stride] = -d.im;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus::celt::mdct::{Imdct, ImdctScratch};
    use crate::opus::celt::mode;
    use crate::opus::celt::tables::OVERLAP;

    #[test]
    fn forward_then_decoder_imdct_reconstructs() {
        let m = mode();
        for (n2, shift) in [(960usize, 0usize), (120, 3)] {
            let fwd = Mdct::new(n2);
            let inv: &Imdct = &m.mdct[shift];
            let frames = 6;
            let sig: Vec<f32> = (0..n2 * frames + OVERLAP)
                .map(|i| (i as f32 * 0.037).sin() * 1000.0 + (i as f32 * 0.31).cos() * 300.0)
                .collect();
            let mut out = vec![0f32; n2 * (frames + 2) + OVERLAP];
            let mut coefs = vec![0f32; n2];
            let mut ws = MdctScratch::default();
            let mut wi = ImdctScratch::default();
            for f in 0..frames {
                fwd.forward(
                    &sig[f * n2..f * n2 + n2 + OVERLAP],
                    &m.window,
                    &mut coefs,
                    1,
                    &mut ws,
                );
                inv.backward(&coefs, 1, &mut out[f * n2..], &m.window, OVERLAP, &mut wi);
            }
            for i in n2..n2 * (frames - 1) {
                let err = (out[i] - sig[i]).abs();
                assert!(err < 0.01, "n2={n2} i={i} got {} want {}", out[i], sig[i]);
            }
        }
    }
}
