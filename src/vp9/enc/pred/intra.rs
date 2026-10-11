use super::*;

#[derive(Clone)]
pub struct Edges {
    above: [u8; 65],
    left: [u8; 32],
    have_left: bool,
    have_above: bool,
    log2: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct EdgeFlags {
    pub have_left: bool,
    pub have_above: bool,
    pub not_on_right: bool,
    pub max_x: usize,
    pub max_y: usize,
}

impl Edges {
    pub fn build(
        plane: &[u8],
        stride: usize,
        x: usize,
        y: usize,
        tx_size: u8,
        f: EdgeFlags,
    ) -> Edges {
        let log2 = tx_size as usize + 2;
        let size = 1usize << log2;
        let mut above = [0u8; 65];
        let mut left = [0u8; 32];
        if !f.have_above {
            above[1..=2 * size].fill(127);
        } else {
            let row = &plane[(y - 1) * stride..];
            if x + size <= f.max_x + 1 {
                above[1..=size].copy_from_slice(&row[x..x + size]);
            } else {
                for i in 0..size {
                    above[1 + i] = row[(x + i).min(f.max_x)];
                }
            }
            if f.not_on_right && tx_size == 0 {
                for i in size..2 * size {
                    above[1 + i] = row[(x + i).min(f.max_x)];
                }
            } else {
                let v = above[size];
                above[1 + size..=2 * size].fill(v);
            }
        }
        above[0] = if f.have_above && f.have_left {
            plane[(y - 1) * stride + (x - 1).min(f.max_x)]
        } else if f.have_above {
            129
        } else {
            127
        };
        if f.have_left {
            for i in 0..size {
                left[i] = plane[(y + i).min(f.max_y) * stride + x - 1];
            }
        } else {
            left[..size].fill(129);
        }
        Edges {
            above,
            left,
            have_left: f.have_left,
            have_above: f.have_above,
            log2,
        }
    }

    pub fn size(&self) -> usize {
        1 << self.log2
    }

    pub fn predict(&self, mode: u8, dst: &mut [u8], dst_stride: usize) {
        match self.log2 {
            2 => self.predict_n::<4>(mode, dst, dst_stride),
            3 => self.predict_n::<8>(mode, dst, dst_stride),
            4 => self.predict_n::<16>(mode, dst, dst_stride),
            _ => self.predict_n::<32>(mode, dst, dst_stride),
        }
    }

    fn predict_n<const N: usize>(&self, mode: u8, dst: &mut [u8], s: usize) {
        let above = &self.above;
        let left = &self.left;
        let a = |i: isize| above[(i + 1) as usize] as i32;
        let l = |i: usize| left[i] as i32;
        let r1 = |v: i32| ((v + 1) >> 1) as u8;
        let r2 = |v: i32| ((v + 2) >> 2) as u8;
        match mode {
            V_PRED => {
                let row: &[u8; N] = above[1..=N].try_into().unwrap();
                for i in 0..N {
                    dst[i * s..i * s + N].copy_from_slice(row);
                }
            }
            H_PRED => {
                for i in 0..N {
                    dst[i * s..i * s + N].fill(left[i]);
                }
            }
            TM_PRED => {
                let tl = a(-1);
                for i in 0..N {
                    let d = l(i) - tl;
                    let row = &mut dst[i * s..i * s + N];
                    for j in 0..N {
                        row[j] = (above[1 + j] as i32 + d).clamp(0, 255) as u8;
                    }
                }
            }
            D45_PRED => {
                let mut e = [0u8; 64];
                for k in 0..2 * N - 1 {
                    e[k] = if k + 2 < 2 * N {
                        r2(a(k as isize) + 2 * a(k as isize + 1) + a(k as isize + 2))
                    } else {
                        above[2 * N]
                    };
                }
                e[2 * N - 1] = above[2 * N];
                for i in 0..N {
                    dst[i * s..i * s + N].copy_from_slice(&e[i..i + N]);
                }
            }
            D63_PRED => {
                for i in 0..N {
                    let row = &mut dst[i * s..i * s + N];
                    for j in 0..N {
                        let k = (i / 2 + j) as isize;
                        row[j] = if i & 1 == 1 {
                            r2(a(k) + 2 * a(k + 1) + a(k + 2))
                        } else {
                            r1(a(k) + a(k + 1))
                        };
                    }
                }
            }
            D207_PRED => {
                let mut p = [[0u8; N]; N];
                for j in 0..N {
                    p[N - 1][j] = left[N - 1];
                }
                for i in 0..N - 1 {
                    p[i][0] = r1(l(i) + l(i + 1));
                }
                for i in 0..N.saturating_sub(2) {
                    p[i][1] = r2(l(i) + 2 * l(i + 1) + l(i + 2));
                }
                p[N - 2][1] = r2(l(N - 2) + 3 * l(N - 1));
                for j in 2..N {
                    for i in (0..N - 1).rev() {
                        p[i][j] = p[i + 1][j - 2];
                    }
                }
                put(&p, dst, s);
            }
            D117_PRED => {
                let mut p = [[0u8; N]; N];
                for j in 0..N {
                    p[0][j] = r1(a(j as isize - 1) + a(j as isize));
                }
                p[1][0] = r2(l(0) + 2 * a(-1) + a(0));
                for j in 1..N {
                    p[1][j] = r2(a(j as isize - 2) + 2 * a(j as isize - 1) + a(j as isize));
                }
                p[2][0] = r2(a(-1) + 2 * l(0) + l(1));
                for i in 3..N {
                    p[i][0] = r2(l(i - 3) + 2 * l(i - 2) + l(i - 1));
                }
                for i in 2..N {
                    for j in 1..N {
                        p[i][j] = p[i - 2][j - 1];
                    }
                }
                put(&p, dst, s);
            }
            D135_PRED => {
                let mut p = [[0u8; N]; N];
                p[0][0] = r2(l(0) + 2 * a(-1) + a(0));
                for j in 1..N {
                    p[0][j] = r2(a(j as isize - 2) + 2 * a(j as isize - 1) + a(j as isize));
                }
                p[1][0] = r2(a(-1) + 2 * l(0) + l(1));
                for i in 2..N {
                    p[i][0] = r2(l(i - 2) + 2 * l(i - 1) + l(i));
                }
                for i in 1..N {
                    for j in 1..N {
                        p[i][j] = p[i - 1][j - 1];
                    }
                }
                put(&p, dst, s);
            }
            D153_PRED => {
                let mut p = [[0u8; N]; N];
                p[0][0] = r1(l(0) + a(-1));
                for i in 1..N {
                    p[i][0] = r1(l(i - 1) + l(i));
                }
                p[0][1] = r2(l(0) + 2 * a(-1) + a(0));
                p[1][1] = r2(a(-1) + 2 * l(0) + l(1));
                for i in 2..N {
                    p[i][1] = r2(l(i - 2) + 2 * l(i - 1) + l(i));
                }
                for j in 2..N {
                    p[0][j] = r2(a(j as isize - 3) + 2 * a(j as isize - 2) + a(j as isize - 1));
                }
                for i in 1..N {
                    for j in 2..N {
                        p[i][j] = p[i - 1][j - 2];
                    }
                }
                put(&p, dst, s);
            }
            _ => {
                let log2 = N.trailing_zeros();
                let sl = || left[..N].iter().map(|&v| v as u32).sum::<u32>();
                let sa = || above[1..=N].iter().map(|&v| v as u32).sum::<u32>();
                let v = match (self.have_left, self.have_above) {
                    (true, true) => (sl() + sa() + N as u32) >> (log2 + 1),
                    (true, false) => (sl() + (N as u32 >> 1)) >> log2,
                    (false, true) => (sa() + (N as u32 >> 1)) >> log2,
                    (false, false) => 128,
                } as u8;
                for i in 0..N {
                    dst[i * s..i * s + N].fill(v);
                }
            }
        }
    }
}

#[inline(always)]
fn put<const N: usize>(p: &[[u8; N]; N], dst: &mut [u8], s: usize) {
    for i in 0..N {
        dst[i * s..i * s + N].copy_from_slice(&p[i]);
    }
}

pub fn predict_in_place(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    mode: u8,
    tx_size: u8,
    f: EdgeFlags,
) {
    let e = Edges::build(plane, stride, x, y, tx_size, f);
    e.predict(mode, &mut plane[y * stride + x..], stride);
}
