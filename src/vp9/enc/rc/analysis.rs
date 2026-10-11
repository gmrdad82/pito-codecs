use super::FrameKind;
use rayon::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameAnalysis {
    pub intra_cost: u64,
    pub inter_cost: u64,
    pub has_prev: bool,
    pub blocks: u32,
}

impl FrameAnalysis {
    pub fn cost(&self, kind: FrameKind) -> u64 {
        let c = if kind == FrameKind::Key || !self.has_prev {
            self.intra_cost
        } else {
            self.inter_cost
        };
        c.max(1)
    }

    pub fn inter_ratio(&self) -> f64 {
        if !self.has_prev {
            return 1.0;
        }
        self.inter_cost as f64 / self.intra_cost.max(1) as f64
    }

    pub fn scene_cut(&self) -> bool {
        self.has_prev && self.inter_ratio() > 0.72
    }
}

pub struct Analyser {
    width: usize,
    height: usize,
    pw: usize,
    ph: usize,
    bw: usize,
    bh: usize,
    cur: Vec<u8>,
    prev: Vec<u8>,
    has_prev: bool,
    mvs: Vec<(i8, i8)>,
    prev_mvs: Vec<(i8, i8)>,
    activity: Vec<u32>,
    rows: Vec<(u64, u64)>,
    global: (i8, i8),
}

const RANGE: i32 = 48;

impl Analyser {
    pub fn new(width: usize, height: usize) -> Analyser {
        let pw = width.div_ceil(2).max(1).next_multiple_of(8);
        let ph = height.div_ceil(2).max(1).next_multiple_of(8);
        let (bw, bh) = (pw / 8, ph / 8);
        Analyser {
            width,
            height,
            pw,
            ph,
            bw,
            bh,
            cur: vec![0; pw * ph],
            prev: vec![0; pw * ph],
            has_prev: false,
            mvs: vec![(0, 0); bw * bh],
            prev_mvs: vec![(0, 0); bw * bh],
            activity: vec![0; bw * bh],
            rows: vec![(0, 0); bh],
            global: (0, 0),
        }
    }

    pub fn blocks(&self) -> (usize, usize) {
        (self.bw, self.bh)
    }

    pub fn activity(&self) -> &[u32] {
        &self.activity
    }

    pub fn reset(&mut self) {
        self.has_prev = false;
        self.prev_mvs.fill((0, 0));
        self.global = (0, 0);
    }

    pub fn run(&mut self, y: &[u8], stride: usize) -> FrameAnalysis {
        assert!(stride >= self.width && y.len() >= stride * (self.height - 1) + self.width);
        self.downsample(y, stride);
        let (pw, ph, bw) = (self.pw, self.ph, self.bw);
        let cur = &self.cur[..];
        let prev = self.has_prev.then_some(&self.prev[..]);
        let prev_mvs = &self.prev_mvs[..];
        let global = self.global;
        self.activity
            .par_chunks_mut(bw)
            .zip(self.mvs.par_chunks_mut(bw))
            .zip(self.rows.par_iter_mut())
            .enumerate()
            .for_each(|(by, ((act, mvs), sums))| {
                let mut intra_sum = 0u64;
                let mut best_sum = 0u64;
                for bx in 0..bw {
                    let (x0, y0) = (bx * 8, by * 8);
                    let src = &cur[y0 * pw + x0..];
                    let intra = satd8(src, pw, None);
                    act[bx] = intra;
                    intra_sum += intra as u64;
                    let Some(prev) = prev else {
                        best_sum += intra as u64;
                        continue;
                    };
                    let left = if bx > 0 { mvs[bx - 1] } else { (0, 0) };
                    let mv = search(
                        src,
                        prev,
                        pw,
                        ph,
                        x0,
                        y0,
                        [prev_mvs[by * bw + bx], left, global],
                    );
                    mvs[bx] = mv;
                    let rx = (x0 as i32 + mv.0 as i32) as usize;
                    let ry = (y0 as i32 + mv.1 as i32) as usize;
                    let inter = satd8(src, pw, Some(&prev[ry * pw + rx..]));
                    best_sum += inter.min(intra) as u64;
                }
                *sums = (intra_sum, best_sum);
            });
        let (intra, best) = self
            .rows
            .iter()
            .fold((0u64, 0u64), |a, r| (a.0 + r.0, a.1 + r.1));
        let out = FrameAnalysis {
            intra_cost: intra,
            inter_cost: best,
            has_prev: self.has_prev,
            blocks: (self.bw * self.bh) as u32,
        };
        std::mem::swap(&mut self.cur, &mut self.prev);
        if self.has_prev {
            std::mem::swap(&mut self.mvs, &mut self.prev_mvs);
            self.global = dominant(&self.prev_mvs);
        }
        self.has_prev = true;
        out
    }

    fn downsample(&mut self, y: &[u8], stride: usize) {
        let (w, h, pw) = (self.width, self.height, self.pw);
        let full = (w / 2).min(pw);
        self.cur
            .par_chunks_mut(pw)
            .enumerate()
            .for_each(|(r, out)| {
                let r0 = &y[(2 * r).min(h - 1) * stride..][..w];
                let r1 = &y[(2 * r + 1).min(h - 1) * stride..][..w];
                for ((o, a), b) in out[..full]
                    .iter_mut()
                    .zip(r0.chunks_exact(2))
                    .zip(r1.chunks_exact(2))
                {
                    *o = ((a[0] as u16 + a[1] as u16 + b[0] as u16 + b[1] as u16 + 2) >> 2) as u8;
                }
                for (x, o) in out.iter_mut().enumerate().skip(full) {
                    let x0 = (2 * x).min(w - 1);
                    let x1 = (2 * x + 1).min(w - 1);
                    let s = r0[x0] as u16 + r0[x1] as u16 + r1[x0] as u16 + r1[x1] as u16;
                    *o = ((s + 2) >> 2) as u8;
                }
            });
    }
}

fn dominant(mvs: &[(i8, i8)]) -> (i8, i8) {
    let mut xs = [0u32; 256];
    let mut ys = [0u32; 256];
    for &(x, y) in mvs {
        xs[x as u8 as usize] += 1;
        ys[y as u8 as usize] += 1;
    }
    let pick = |h: &[u32; 256]| {
        let i = (0..256).max_by_key(|&i| h[i]).unwrap_or(0);
        i as u8 as i8
    };
    (pick(&xs), pick(&ys))
}

fn search(
    src: &[u8],
    prev: &[u8],
    pw: usize,
    ph: usize,
    x0: usize,
    y0: usize,
    cands: [(i8, i8); 3],
) -> (i8, i8) {
    let (xmin, xmax) = (
        (-(x0 as i32)).max(-RANGE),
        ((pw - 8 - x0) as i32).min(RANGE),
    );
    let (ymin, ymax) = (
        (-(y0 as i32)).max(-RANGE),
        ((ph - 8 - y0) as i32).min(RANGE),
    );
    let sad_at = |mx: i32, my: i32| {
        let o = (y0 as i32 + my) as usize * pw + (x0 as i32 + mx) as usize;
        sad8(src, &prev[o..], pw)
    };
    let mut best = (0i32, 0i32);
    let mut best_sad = sad_at(0, 0);
    for c in cands {
        let (mx, my) = (
            (c.0 as i32).clamp(xmin, xmax),
            (c.1 as i32).clamp(ymin, ymax),
        );
        if (mx, my) != best {
            let s = sad_at(mx, my);
            if s < best_sad {
                best_sad = s;
                best = (mx, my);
            }
        }
    }
    let mut step = 4;
    let mut guard = 0;
    while step > 0 && guard < 24 {
        guard += 1;
        let mut moved = false;
        for (dx, dy) in [
            (-step, 0),
            (step, 0),
            (0, -step),
            (0, step),
            (-step, -step),
            (step, -step),
            (-step, step),
            (step, step),
        ] {
            let (mx, my) = (best.0 + dx, best.1 + dy);
            if mx < xmin || mx > xmax || my < ymin || my > ymax {
                continue;
            }
            let s = sad_at(mx, my);
            if s < best_sad {
                best_sad = s;
                best = (mx, my);
                moved = true;
            }
        }
        if !moved {
            step /= 2;
        }
    }
    (best.0 as i8, best.1 as i8)
}

#[cfg(target_arch = "x86_64")]
fn sad8(a: &[u8], b: &[u8], stride: usize) -> u32 {
    use std::arch::x86_64::*;
    assert!(a.len() >= 7 * stride + 8 && b.len() >= 7 * stride + 8);
    unsafe {
        let mut acc = _mm_setzero_si128();
        for r in (0..8).step_by(2) {
            let pa = a.as_ptr().add(r * stride);
            let pb = b.as_ptr().add(r * stride);
            let va = _mm_unpacklo_epi64(
                _mm_loadl_epi64(pa as *const __m128i),
                _mm_loadl_epi64(pa.add(stride) as *const __m128i),
            );
            let vb = _mm_unpacklo_epi64(
                _mm_loadl_epi64(pb as *const __m128i),
                _mm_loadl_epi64(pb.add(stride) as *const __m128i),
            );
            acc = _mm_add_epi64(acc, _mm_sad_epu8(va, vb));
        }
        (_mm_cvtsi128_si32(acc) + _mm_extract_epi16(acc, 4)) as u32
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn sad8(a: &[u8], b: &[u8], stride: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..8 {
        for (x, y) in a[r * stride..][..8].iter().zip(&b[r * stride..][..8]) {
            s += x.abs_diff(*y) as u32;
        }
    }
    s
}

fn satd8(src: &[u8], stride: usize, pred: Option<&[u8]>) -> u32 {
    let mut d = [[0i32; 8]; 8];
    for (r, row) in d.iter_mut().enumerate() {
        let s = &src[r * stride..][..8];
        match pred {
            Some(p) => {
                let p = &p[r * stride..][..8];
                for x in 0..8 {
                    row[x] = s[x] as i32 - p[x] as i32;
                }
            }
            None => {
                for x in 0..8 {
                    row[x] = s[x] as i32;
                }
            }
        }
        hadamard8(row);
    }
    let mut sum = 0u32;
    let mut dc = 0u32;
    for x in 0..8 {
        let mut col = [
            d[0][x], d[1][x], d[2][x], d[3][x], d[4][x], d[5][x], d[6][x], d[7][x],
        ];
        hadamard8(&mut col);
        if x == 0 {
            dc = col[0].unsigned_abs();
        }
        sum += col.iter().map(|v| v.unsigned_abs()).sum::<u32>();
    }
    if pred.is_none() {
        (sum - dc) >> 2
    } else {
        sum >> 2
    }
}

fn hadamard8(v: &mut [i32; 8]) {
    let mut h = 4;
    while h > 0 {
        for i in 0..8 {
            if i & h == 0 {
                let (a, b) = (v[i], v[i + h]);
                v[i] = a + b;
                v[i + h] = a - b;
            }
        }
        h >>= 1;
    }
}
