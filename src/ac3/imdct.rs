use std::f64::consts::PI;

const L: usize = 8;
type V = [f32; L];

pub struct Imdct {
    pre_long: [[f32; 2]; 128],
    pre_short: [[f32; 2]; 64],
    tw128: [[f32; 2]; 64],
    tw64: [[f32; 2]; 32],
    rev128: [u8; 128],
    rev64: [u8; 64],
    window: &'static [f32; 256],
}

fn pre<const K: usize>(div: f64) -> [[f32; 2]; K] {
    std::array::from_fn(|k| {
        let a = 2.0 * PI * (8.0 * k as f64 + 1.0) / div;
        [-a.cos() as f32, -a.sin() as f32]
    })
}

fn twiddles<const K: usize>() -> [[f32; 2]; K] {
    std::array::from_fn(|k| {
        let a = 2.0 * PI * k as f64 / (2 * K) as f64;
        [a.cos() as f32, a.sin() as f32]
    })
}

fn rev<const K: usize>() -> [u8; K] {
    let bits = K.trailing_zeros();
    std::array::from_fn(|i| ((i as u32).reverse_bits() >> (32 - bits)) as u8)
}

#[inline(always)]
fn fft(re: &mut [V], im: &mut [V], tw: &[[f32; 2]]) {
    let n = re.len();
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = n / len;
        let mut start = 0;
        while start < n {
            for k in 0..half {
                let [wr, wi] = tw[k * step];
                let (a, b) = (start + k, start + k + half);
                let (ar, ai, br, bi) = (re[a], im[a], re[b], im[b]);
                let (mut sr, mut si, mut dr, mut di) = ([0f32; L], [0f32; L], [0f32; L], [0f32; L]);
                for l in 0..L {
                    let xr = br[l] * wr - bi[l] * wi;
                    let xi = br[l] * wi + bi[l] * wr;
                    sr[l] = ar[l] + xr;
                    si[l] = ai[l] + xi;
                    dr[l] = ar[l] - xr;
                    di[l] = ai[l] - xi;
                }
                re[a] = sr;
                im[a] = si;
                re[b] = dr;
                im[b] = di;
            }
            start += len;
        }
        len <<= 1;
    }
}

#[inline(always)]
fn post(re: &mut [V], im: &mut [V], tw: &[[f32; 2]]) {
    for ((r, i), &[c, s]) in re.iter_mut().zip(im.iter_mut()).zip(tw) {
        let (x, y) = (*r, *i);
        for l in 0..L {
            r[l] = x[l] * c - y[l] * s;
            i[l] = y[l] * c + x[l] * s;
        }
    }
}

#[inline(always)]
fn twist(
    x: &[V; 256],
    pre: &[[f32; 2]; 128],
    rev: &[u8; 128],
    re: &mut [V; 128],
    im: &mut [V; 128],
) {
    for k in 0..128 {
        let [cr, ci] = pre[k];
        let (a, b) = (x[255 - 2 * k], x[2 * k]);
        let r = rev[k] as usize;
        for l in 0..L {
            re[r][l] = a[l] * cr - b[l] * ci;
            im[r][l] = b[l] * cr + a[l] * ci;
        }
    }
}

#[inline(always)]
fn neg(v: V) -> V {
    v.map(|x| -x)
}

#[inline(always)]
fn row(re: &[V; 128], im: &[V; 128], m: usize) -> V {
    let n = m >> 1;
    match (m >> 7, m & 1) {
        (0, 0) => neg(im[64 + n]),
        (0, _) => re[63 - n],
        (1 | 2, 0) => neg(re[n - 64]),
        (1 | 2, _) => im[191 - n],
        (_, 0) => im[n - 192],
        _ => neg(re[319 - n]),
    }
}

crate::media::isa::dispatch! { fma:
    fn short_fft(re: &mut [V], im: &mut [V], tw: &[[f32; 2]], post_tw: &[[f32; 2]]) {
        fft(re, im, tw);
        post(re, im, post_tw);
    }
}

crate::media::isa::dispatch! { fma:
    fn overlap_add(window: &[f32; 256], time: &[f32; 512], delay: &mut [f32; 256], gain: f32, out: &mut [f32]) {
        let g = 2.0 * gain;
        for (n, o) in out.iter_mut().enumerate().take(256) {
            *o = g * time[n] * window[n] + delay[n];
        }
        for n in 0..256 {
            delay[n] = g * time[256 + n] * window[255 - n];
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx")]
fn transpose(r: [std::arch::x86_64::__m256; L]) -> [std::arch::x86_64::__m256; L] {
    use std::arch::x86_64::*;
    let t0 = _mm256_unpacklo_ps(r[0], r[1]);
    let t1 = _mm256_unpackhi_ps(r[0], r[1]);
    let t2 = _mm256_unpacklo_ps(r[2], r[3]);
    let t3 = _mm256_unpackhi_ps(r[2], r[3]);
    let t4 = _mm256_unpacklo_ps(r[4], r[5]);
    let t5 = _mm256_unpackhi_ps(r[4], r[5]);
    let t6 = _mm256_unpacklo_ps(r[6], r[7]);
    let t7 = _mm256_unpackhi_ps(r[6], r[7]);
    let u0 = _mm256_shuffle_ps::<0x44>(t0, t2);
    let u1 = _mm256_shuffle_ps::<0xEE>(t0, t2);
    let u2 = _mm256_shuffle_ps::<0x44>(t1, t3);
    let u3 = _mm256_shuffle_ps::<0xEE>(t1, t3);
    let u4 = _mm256_shuffle_ps::<0x44>(t4, t6);
    let u5 = _mm256_shuffle_ps::<0xEE>(t4, t6);
    let u6 = _mm256_shuffle_ps::<0x44>(t5, t7);
    let u7 = _mm256_shuffle_ps::<0xEE>(t5, t7);
    [
        _mm256_permute2f128_ps::<0x20>(u0, u4),
        _mm256_permute2f128_ps::<0x20>(u1, u5),
        _mm256_permute2f128_ps::<0x20>(u2, u6),
        _mm256_permute2f128_ps::<0x20>(u3, u7),
        _mm256_permute2f128_ps::<0x31>(u0, u4),
        _mm256_permute2f128_ps::<0x31>(u1, u5),
        _mm256_permute2f128_ps::<0x31>(u2, u6),
        _mm256_permute2f128_ps::<0x31>(u3, u7),
    ]
}

impl Imdct {
    pub fn new() -> Imdct {
        Imdct {
            pre_long: pre::<128>(4096.0),
            pre_short: pre::<64>(2048.0),
            tw128: twiddles::<64>(),
            tw64: twiddles::<32>(),
            rev128: rev::<128>(),
            rev64: rev::<64>(),
            window: super::tables::window(),
        }
    }

    pub fn overlap(&self, time: &[f32; 512], delay: &mut [f32; 256], gain: f32, out: &mut [f32]) {
        overlap_add(self.window, time, delay, gain, out);
    }

    pub fn run(&self, spec: &[&[f32; 256]], short: &[bool], time: &mut [&mut [f32; 512]]) {
        let mut long = [0usize; L];
        let mut nl = 0;
        let mut halves = [(0usize, 0usize); L];
        let mut nh = 0;
        for i in 0..spec.len() {
            if short[i] {
                halves[nh] = (i, 0);
                halves[nh + 1] = (i, 1);
                nh += 2;
                if nh == L {
                    self.short_batch(spec, &halves[..nh], time);
                    nh = 0;
                }
            } else {
                long[nl] = i;
                nl += 1;
                if nl == L {
                    self.long_batch(spec, &long[..nl], time);
                    nl = 0;
                }
            }
        }
        if nl > 0 {
            self.long_batch(spec, &long[..nl], time);
        }
        if nh > 0 {
            self.short_batch(spec, &halves[..nh], time);
        }
    }

    fn long_batch(&self, spec: &[&[f32; 256]], jobs: &[usize], time: &mut [&mut [f32; 512]]) {
        #[cfg(target_arch = "x86_64")]
        if crate::media::isa::avx2_fma() {
            unsafe { self.long_avx(spec, jobs, time) };
            return;
        }
        let mut x = [[0f32; L]; 256];
        for (l, &j) in jobs.iter().enumerate() {
            for (k, v) in spec[j].iter().enumerate() {
                x[k][l] = *v;
            }
        }
        let mut re = [[0f32; L]; 128];
        let mut im = [[0f32; L]; 128];
        twist(&x, &self.pre_long, &self.rev128, &mut re, &mut im);
        fft(&mut re, &mut im, &self.tw128);
        post(&mut re, &mut im, &self.pre_long);
        for m in 0..512 {
            let v = row(&re, &im, m);
            for (l, &j) in jobs.iter().enumerate() {
                time[j][m] = v[l];
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,fma")]
    fn long_avx(&self, spec: &[&[f32; 256]], jobs: &[usize], time: &mut [&mut [f32; 512]]) {
        use std::arch::x86_64::*;
        let zero = [0f32; 256];
        let lanes: [&[f32; 256]; L] =
            std::array::from_fn(|l| jobs.get(l).map_or(&zero, |&j| spec[j]));
        let mut x = [[0f32; L]; 256];
        for t in 0..32 {
            let r: [__m256; L] =
                std::array::from_fn(|l| unsafe { _mm256_loadu_ps(lanes[l][t * L..].as_ptr()) });
            for (i, v) in transpose(r).into_iter().enumerate() {
                unsafe { _mm256_storeu_ps(x[t * L + i].as_mut_ptr(), v) };
            }
        }
        let mut re = [[0f32; L]; 128];
        let mut im = [[0f32; L]; 128];
        twist(&x, &self.pre_long, &self.rev128, &mut re, &mut im);
        fft(&mut re, &mut im, &self.tw128);
        post(&mut re, &mut im, &self.pre_long);
        for t in 0..64 {
            let r: [__m256; L] = std::array::from_fn(|i| {
                let v = row(&re, &im, t * L + i);
                unsafe { _mm256_loadu_ps(v.as_ptr()) }
            });
            let o = transpose(r);
            for (l, &j) in jobs.iter().enumerate() {
                unsafe { _mm256_storeu_ps(time[j][t * L..].as_mut_ptr(), o[l]) };
            }
        }
    }

    fn short_batch(
        &self,
        spec: &[&[f32; 256]],
        jobs: &[(usize, usize)],
        time: &mut [&mut [f32; 512]],
    ) {
        let mut re = [[0f32; L]; 64];
        let mut im = [[0f32; L]; 64];
        for (l, &(j, h)) in jobs.iter().enumerate() {
            let x = spec[j];
            for k in 0..64 {
                let [cr, ci] = self.pre_short[k];
                let (a, b) = (x[2 * (127 - 2 * k) + h], x[4 * k + h]);
                let r = self.rev64[k] as usize;
                re[r][l] = a * cr - b * ci;
                im[r][l] = b * cr + a * ci;
            }
        }
        short_fft(&mut re, &mut im, &self.tw64, &self.pre_short);
        for (l, &(j, h)) in jobs.iter().enumerate() {
            let out = &mut *time[j];
            if h == 0 {
                for n in 0..64 {
                    out[2 * n] = -im[n][l];
                    out[2 * n + 1] = re[63 - n][l];
                    out[128 + 2 * n] = -re[n][l];
                    out[129 + 2 * n] = im[63 - n][l];
                }
            } else {
                for n in 0..64 {
                    out[256 + 2 * n] = -re[n][l];
                    out[257 + 2 * n] = im[63 - n][l];
                    out[384 + 2 * n] = im[n][l];
                    out[385 + 2 * n] = -re[63 - n][l];
                }
            }
        }
    }
}
