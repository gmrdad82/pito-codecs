use super::dist;
use super::inter::{self, MvLimits};
use super::mvcost::MvCost;
use super::*;

pub fn sad_per_bit(lambda: u32) -> u32 {
    (0.247 * (lambda as f64).sqrt() + 2.41).round() as u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subpel {
    None,
    Half,
    Quarter,
    Full,
}

#[derive(Clone, Copy)]
pub struct Search<'a> {
    pub src: &'a [u8],
    pub src_stride: usize,
    pub reference: Plane<'a>,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub limits: MvLimits,
    pub mv_cost: &'a MvCost,
    pub ref_mv: Mv,
    pub allow_hp: bool,
    pub sad_per_bit: u32,
    pub range: i32,
    pub subpel: Subpel,
    pub filter: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MotionResult {
    pub mv: Mv,
    pub dist: u32,
    pub rate: u32,
}

const HEX: [(i32, i32); 6] = [(-1, -2), (1, -2), (-2, 0), (2, 0), (-1, 2), (1, 2)];
const DIAMOND: [(i32, i32); 4] = [(0, -1), (-1, 0), (1, 0), (0, 1)];
const SQUARE: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

impl Search<'_> {
    fn usehp(&self) -> bool {
        self.allow_hp && self.ref_mv.use_hp()
    }

    #[inline(always)]
    fn mv_bits(&self, mv: Mv) -> u32 {
        self.mv_cost.rate(mv, self.ref_mv, self.usehp())
    }

    #[inline(always)]
    fn penalty(&self, mv: Mv) -> u32 {
        (self.mv_bits(mv) * self.sad_per_bit + 128) >> 8
    }

    #[inline(always)]
    fn full_sad(&self, row: i32, col: i32) -> u32 {
        let r = &self.reference;
        let o = r.at(
            self.x as isize + col as isize,
            self.y as isize + row as isize,
        );
        dist::sad(
            self.src,
            self.src_stride,
            &r.data[o..],
            r.stride,
            self.w,
            self.h,
        )
    }

    fn full_limits(&self) -> (i32, i32, i32, i32) {
        let l = &self.limits;
        let b = self.reference.border as i32 - 8;
        let (x, y) = (self.x as i32, self.y as i32);
        let (w, h) = (self.reference.width as i32, self.reference.height as i32);
        let rmin = ((l.row_min + 7) >> 3).max(-y - b);
        let rmax = (l.row_max >> 3).min(h + b - y - self.h as i32);
        let cmin = ((l.col_min + 7) >> 3).max(-x - b);
        let cmax = (l.col_max >> 3).min(w + b - x - self.w as i32);
        (rmin, rmax.max(rmin), cmin, cmax.max(cmin))
    }

    pub fn full_pel(&self, candidates: &[Mv]) -> (i32, i32, u32) {
        let (rmin, rmax, cmin, cmax) = self.full_limits();
        let fit = |r: i32, c: i32| (r.clamp(rmin, rmax), c.clamp(cmin, cmax));
        let cost = |r: i32, c: i32| {
            self.full_sad(r, c) + self.penalty(Mv::new((r * 8) as i16, (c * 8) as i16))
        };
        let (mut br, mut bc) = fit(0, 0);
        let mut best = cost(br, bc);
        let mut seen = [(i32::MIN, i32::MIN); 8];
        for (k, m) in candidates.iter().enumerate() {
            let (r, c) = fit(
                (m.row as i32 + 4).div_euclid(8),
                (m.col as i32 + 4).div_euclid(8),
            );
            if (r, c) == (br, bc) || seen[..k.min(8)].contains(&(r, c)) {
                continue;
            }
            if k < 8 {
                seen[k] = (r, c);
            }
            let v = cost(r, c);
            if v < best {
                (best, br, bc) = (v, r, c);
            }
        }
        let (cr, cc) = (br, bc);
        let inside = |r: i32, c: i32| {
            (rmin..=rmax).contains(&r)
                && (cmin..=cmax).contains(&c)
                && (r - cr).abs() <= self.range
                && (c - cc).abs() <= self.range
        };
        let mut steps = 0;
        loop {
            let (pr, pc) = (br, bc);
            for &(dc, dr) in &HEX {
                let (r, c) = (pr + dr, pc + dc);
                if inside(r, c) {
                    let v = cost(r, c);
                    if v < best {
                        (best, br, bc) = (v, r, c);
                    }
                }
            }
            steps += 1;
            if (br, bc) == (pr, pc) || steps > self.range {
                break;
            }
        }
        loop {
            let (pr, pc) = (br, bc);
            for &(dc, dr) in &DIAMOND {
                let (r, c) = (pr + dr, pc + dc);
                if inside(r, c) {
                    let v = cost(r, c);
                    if v < best {
                        (best, br, bc) = (v, r, c);
                    }
                }
            }
            if (br, bc) == (pr, pc) {
                break;
            }
        }
        let (pr, pc) = (br, bc);
        for &(dc, dr) in &SQUARE {
            let (r, c) = (pr + dr, pc + dc);
            if inside(r, c) && dr != 0 && dc != 0 {
                let v = cost(r, c);
                if v < best {
                    (best, br, bc) = (v, r, c);
                }
            }
        }
        (br, bc, best)
    }

    fn subpel_cost(&self, mv: Mv, pred: &mut [u8; 64 * 64]) -> u32 {
        let x16 = ((self.x as isize) << 4) + 2 * mv.col as isize;
        let y16 = ((self.y as isize) << 4) + 2 * mv.row as isize;
        inter::mc(
            &self.reference,
            x16,
            y16,
            self.w,
            self.h,
            self.filter,
            pred,
            self.w,
        );
        dist::satd(self.src, self.src_stride, pred, self.w, self.w, self.h) + self.penalty(mv)
    }

    pub fn run(&self, candidates: &[Mv]) -> MotionResult {
        let (r, c, _) = self.full_pel(candidates);
        let mut best_mv = Mv::new((r * 8) as i16, (c * 8) as i16);
        let mut pred = [0u8; 64 * 64];
        let mut best = self.subpel_cost(best_mv, &mut pred);
        let mut steps: &[i16] = match self.subpel {
            Subpel::None => &[],
            Subpel::Half => &[4],
            Subpel::Quarter => &[4, 2],
            Subpel::Full => &[4, 2, 1],
        };
        if !self.usehp() && steps.last() == Some(&1) {
            steps = &steps[..steps.len() - 1];
        }
        for &s in steps {
            let center = best_mv;
            let mut cross = [u32::MAX; 4];
            for (k, &(dc, dr)) in DIAMOND.iter().enumerate() {
                let m = Mv::new(center.row + dr as i16 * s, center.col + dc as i16 * s);
                if !self.limits.contains(m) {
                    continue;
                }
                let v = self.subpel_cost(m, &mut pred);
                cross[k] = v;
                if v < best {
                    (best, best_mv) = (v, m);
                }
            }
            let dr = if cross[0] < cross[3] { -s } else { s };
            let dc = if cross[1] < cross[2] { -s } else { s };
            let m = Mv::new(center.row + dr, center.col + dc);
            if self.limits.contains(m) {
                let v = self.subpel_cost(m, &mut pred);
                if v < best {
                    (best, best_mv) = (v, m);
                }
            }
        }
        let rate = self.mv_bits(best_mv);
        MotionResult {
            mv: best_mv,
            dist: best - self.penalty(best_mv),
            rate,
        }
    }
}
