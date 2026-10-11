use crate::opus::range::RangeEncoder;

#[derive(Default)]
pub struct Pvq {
    iy: Vec<i32>,
    ax: Vec<f32>,
    v: Vec<u64>,
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
        let sum: f32 = ax.iter().sum();
        if sum.is_nan() || sum <= 1e-15 {
            iy[0] = k as i32;
            return;
        }
        let mut left = k as i32;
        let mut rxy = 0f32;
        let mut ryy = 0f32;
        if k > n / 2 {
            let r = (k as f32 - 0.2) / sum;
            for (y, &a) in iy.iter_mut().zip(ax.iter()) {
                let p = (a * r).floor() as i32;
                *y = p;
                left -= p;
                rxy += a * p as f32;
                ryy += (p * p) as f32;
            }
        }
        if left > n as i32 + 3 {
            iy[0] += left;
            left = 0;
        }
        while left > 0 {
            let mut best = 0usize;
            let mut best_num = -1f32;
            let mut best_den = 1f32;
            let base = ryy + 1.0;
            for j in 0..n {
                let t = rxy + ax[j];
                let num = t * t;
                let den = base + 2.0 * iy[j] as f32;
                if num * best_den > best_num * den {
                    best_num = num;
                    best_den = den;
                    best = j;
                }
            }
            rxy += ax[best];
            ryy += 2.0 * iy[best] as f32 + 1.0;
            iy[best] += 1;
            left -= 1;
        }
        for (y, &xv) in iy.iter_mut().zip(x) {
            if xv < 0.0 {
                *y = -*y;
            }
        }
    }

    pub fn encode(&mut self, enc: &mut RangeEncoder, n: usize, k: usize) {
        let w = k + 1;
        let v = &mut self.v;
        v.clear();
        v.resize((n + 1) * w, 0);
        v[0] = 1;
        for nn in 1..=n {
            v[nn * w] = 1;
            for kk in 1..=k {
                let a = v[(nn - 1) * w + kk];
                let b = v[nn * w + kk - 1];
                let c = v[(nn - 1) * w + kk - 1];
                v[nn * w + kk] = a.saturating_add(b).saturating_add(c);
            }
        }
        let total = v[n * w + k];
        let mut idx = 0u64;
        let mut kk = k;
        for j in 0..n {
            if kk == 0 {
                break;
            }
            let rem = n - j;
            let y = self.iy[j];
            let m = y.unsigned_abs() as usize;
            let row = &v[(rem - 1) * w..rem * w];
            if y < 0 {
                idx += (row[kk] + v[rem * w + kk]) / 2;
            }
            let upto = if m == 0 { kk } else { kk - m };
            idx += row[..upto].iter().sum::<u64>();
            kk -= m;
        }
        if total >= 2 {
            enc.uint(idx as u32, total as u32);
        }
    }
}
