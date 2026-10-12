use crate::media::isa::dispatch;
use crate::opus::range::RangeEncoder;

#[derive(Default)]
pub struct Pvq {
    iy: Vec<i32>,
    ax: Vec<f32>,
    d: Vec<f32>,
}

const L: usize = 8;

#[inline]
fn best_scalar(ax: &[f32], d: &[f32], rxy: f32, base: f32) -> usize {
    let mut b = 0;
    let mut b_n = -1f32;
    let mut b_d = 1f32;
    for j in 0..ax.len() {
        let t = rxy + ax[j];
        let num = t * t;
        let den = base + d[j];
        if num * b_d > b_n * den {
            b = j;
            b_n = num;
            b_d = den;
        }
    }
    b
}

dispatch! {
    fma: fn best(ax: &[f32], d: &[f32], rxy: f32, base: f32) -> usize {
        let n = ax.len();
        let m = n / L * L;
        let mut bn = [-1f32; L];
        let mut bd = [1f32; L];
        let mut bi = [0u32; L];
        for c in (0..m).step_by(L) {
            let a = &ax[c..c + L];
            let e = &d[c..c + L];
            for l in 0..L {
                let t = rxy + a[l];
                let num = t * t;
                let den = base + e[l];
                let better = num * bd[l] > bn[l] * den;
                bn[l] = if better { num } else { bn[l] };
                bd[l] = if better { den } else { bd[l] };
                bi[l] = if better { (c + l) as u32 } else { bi[l] };
            }
        }
        let (mut b, mut b_n, mut b_d) = (bi[0], bn[0], bd[0]);
        for l in 1..L {
            let lhs = bn[l] * b_d;
            let rhs = b_n * bd[l];
            if lhs > rhs || (lhs == rhs && bi[l] < b) {
                b = bi[l];
                b_n = bn[l];
                b_d = bd[l];
            }
        }
        for j in m..n {
            let t = rxy + ax[j];
            let num = t * t;
            let den = base + d[j];
            if num * b_d > b_n * den {
                b = j as u32;
                b_n = num;
                b_d = den;
            }
        }
        b as usize
    }
}

impl Pvq {
    pub fn search(&mut self, x: &[f32], k: usize) {
        let n = x.len();
        let iy = &mut self.iy;
        iy.clear();
        iy.resize(n, 0);
        let ax = &mut self.ax;
        ax.clear();
        ax.extend(x.iter().map(|v| v.abs()));
        let d = &mut self.d;
        d.clear();
        d.resize(n, 0.0);
        let sum: f32 = ax.iter().sum();
        if sum.is_nan() || sum <= 1e-15 {
            iy[0] = k as i32;
            return;
        }
        let mut left = k as i32;
        let mut rxy = 0f32;
        let mut ryy = 0f32;
        if k > n / 2 {
            let r = (k as f32 + 0.8) / sum;
            for j in 0..n {
                let p = (ax[j] * r).floor() as i32;
                iy[j] = p;
                d[j] = 2.0 * p as f32;
                left -= p;
                rxy += ax[j] * p as f32;
                ryy += (p * p) as f32;
            }
        }
        if left < 0 {
            left = k as i32;
            rxy = 0.0;
            ryy = 0.0;
            iy.fill(0);
            d.fill(0.0);
        }
        if left > n as i32 + 3 {
            iy[0] += left;
            left = 0;
        }
        while left > 0 {
            let j = if n < 2 * L {
                best_scalar(ax, d, rxy, ryy + 1.0)
            } else {
                best(ax, d, rxy, ryy + 1.0)
            };
            rxy += ax[j];
            ryy += d[j] + 1.0;
            d[j] += 2.0;
            iy[j] += 1;
            left -= 1;
        }
        for (y, &xv) in iy.iter_mut().zip(x) {
            if xv < 0.0 {
                *y = -*y;
            }
        }
    }

    pub fn encode(&self, enc: &mut RangeEncoder, n: usize, k: usize) {
        let t = tables();
        let total = t.v(n, k);
        if total < 2 {
            return;
        }
        let mut idx = 0u64;
        let mut kk = k;
        for (j, &y) in self.iy[..n].iter().enumerate() {
            if kk == 0 {
                break;
            }
            let rem = n - j;
            let m = y.unsigned_abs() as usize;
            if y < 0 {
                idx += (t.v(rem - 1, kk) as u64 + t.v(rem, kk) as u64) / 2;
            }
            idx += t.p(rem - 1, kk - m) as u64;
            kk -= m;
        }
        enc.uint(idx as u32, total);
    }
}

const NMAX: usize = 177;
const KW: usize = 130;

struct Tables {
    v: Vec<u32>,
    p: Vec<u32>,
}

impl Tables {
    #[inline]
    fn v(&self, n: usize, k: usize) -> u32 {
        self.v[n * KW + k]
    }

    #[inline]
    fn p(&self, n: usize, k: usize) -> u32 {
        self.p[n * KW + k]
    }
}

fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut v = vec![0u32; NMAX * KW];
        let mut p = vec![0u32; NMAX * KW];
        v[0] = 1;
        for n in 1..NMAX {
            v[n * KW] = 1;
            for k in 1..KW {
                v[n * KW + k] = v[(n - 1) * KW + k]
                    .saturating_add(v[n * KW + k - 1])
                    .saturating_add(v[(n - 1) * KW + k - 1]);
            }
        }
        for n in 0..NMAX {
            for k in 1..KW {
                p[n * KW + k] = p[n * KW + k - 1].saturating_add(v[n * KW + k - 1]);
            }
        }
        Tables { v, p }
    })
}
