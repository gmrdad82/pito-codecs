use std::f64::consts::PI;

pub(crate) struct Fft {
    n: usize,
    twiddle: Vec<[f32; 2]>,
    rev: Vec<u32>,
}

impl Fft {
    pub(crate) fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two());
        let bits = n.trailing_zeros();
        let rev = (0..n as u32)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (32 - bits)
                }
            })
            .collect();
        let twiddle = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                [a.cos() as f32, a.sin() as f32]
            })
            .collect();
        Fft { n, twiddle, rev }
    }

    pub(crate) fn run(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let [wr, wi] = self.twiddle[k * step];
                    let a = start + k;
                    let b = a + half;
                    let xr = re[b] * wr - im[b] * wi;
                    let xi = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - xr;
                    im[b] = im[a] - xi;
                    re[a] += xr;
                    im[a] += xi;
                }
            }
            len <<= 1;
        }
    }
}
