use super::dist;
use super::inter::{self, BlockPos, MvLimits};
use super::intra::{EdgeFlags, Edges};
use super::motion::{Search, Subpel, sad_per_bit};
use super::mvcost::MvCost;
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub mode: u8,
    pub sub_modes: [u8; 4],
    pub uv_mode: u8,
    pub ref_frame: [i8; 2],
    pub mv: [Mv; 2],
    pub interp_filter: u8,
    pub dist: u32,
    pub rate: u32,
    pub cost: u64,
}

impl Decision {
    pub fn is_inter(&self) -> bool {
        self.ref_frame[0] > INTRA_FRAME
    }
}

#[inline(always)]
pub fn cost(dist: u32, rate: u32, sad_per_bit: u32) -> u64 {
    ((dist as u64) << 8) + rate as u64 * sad_per_bit as u64
}

pub struct IntraBlock<'a> {
    pub src: &'a [u8],
    pub src_stride: usize,
    pub recon: &'a [u8],
    pub recon_stride: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub tx_size: u8,
    pub have_left: bool,
    pub have_above: bool,
    pub max_x: usize,
    pub max_y: usize,
}

#[derive(Default)]
pub struct IntraScratch {
    canvas: Vec<u8>,
    pred: Vec<u8>,
    edges: Vec<(usize, usize, Edges)>,
}

const CANVAS: usize = 65;

impl IntraScratch {
    fn fill(&mut self, b: &IntraBlock) {
        if self.canvas.is_empty() {
            self.canvas = vec![0; 65 * 65];
            self.pred = vec![0; 64 * 64];
            self.edges = Vec::with_capacity(256);
        }
        let c = &mut self.canvas;
        if b.have_above {
            let row = (b.y - 1) * b.recon_stride;
            for i in 0..=b.w {
                let x = (b.x + i).saturating_sub(1).min(b.max_x);
                c[i] = b.recon[row + x];
            }
        }
        for i in 0..b.h {
            let r = (i + 1) * CANVAS;
            if b.have_left {
                c[r] = b.recon[(b.y + i).min(b.max_y) * b.recon_stride + b.x - 1];
            }
            c[r + 1..r + 1 + b.w].copy_from_slice(&b.src[i * b.src_stride..i * b.src_stride + b.w]);
        }
        self.edges.clear();
        let step = 4usize << b.tx_size;
        let n4w = b.w / 4;
        for ty in (0..b.h).step_by(step) {
            for tx in (0..b.w).step_by(step) {
                let f = EdgeFlags {
                    have_left: b.have_left || tx > 0,
                    have_above: b.have_above || ty > 0,
                    not_on_right: tx / 4 + (step / 4) < n4w,
                    max_x: (b.max_x + 1 - b.x).min(b.w),
                    max_y: (b.max_y + 1 - b.y).min(b.h),
                };
                let e = Edges::build(&self.canvas, CANVAS, tx + 1, ty + 1, b.tx_size, f);
                self.edges.push((tx, ty, e));
            }
        }
    }

    fn predict(&mut self, mode: u8) {
        for (tx, ty, e) in &self.edges {
            e.predict(mode, &mut self.pred[ty * 64 + tx..], 64);
        }
    }

    pub fn evaluate(&mut self, b: &IntraBlock, mode: u8) -> u32 {
        self.fill(b);
        self.predict(mode);
        dist::satd(b.src, b.src_stride, &self.pred, 64, b.w, b.h)
    }

    pub fn best(
        &mut self,
        b: &IntraBlock,
        mode_rates: &[u32; 10],
        sad_per_bit: u32,
        modes: &[u8],
    ) -> (u8, u32, u32, u64) {
        self.fill(b);
        let mut out = (DC_PRED, 0, 0, u64::MAX);
        for &m in modes {
            self.predict(m);
            let d = dist::satd(b.src, b.src_stride, &self.pred, 64, b.w, b.h);
            let j = cost(d, mode_rates[m as usize], sad_per_bit);
            if j < out.3 {
                out = (m, d, mode_rates[m as usize], j);
            }
        }
        out
    }
}

pub const ALL_INTRA: [u8; 10] = [
    DC_PRED, V_PRED, H_PRED, TM_PRED, D45_PRED, D135_PRED, D117_PRED, D153_PRED, D207_PRED,
    D63_PRED,
];
pub const FAST_INTRA: [u8; 4] = [DC_PRED, V_PRED, H_PRED, TM_PRED];

pub struct IntraChoice<'a> {
    pub luma: IntraBlock<'a>,
    pub chroma: [IntraBlock<'a>; 2],
    pub y_mode_rates: &'a [u32; 10],
    pub uv_mode_rates: &'a [[u32; 10]; 10],
    pub lambda: u32,
    pub modes: &'a [u8],
}

fn best_uv(
    c: &IntraChoice,
    y_mode: u8,
    spb: u32,
    scratch: &mut [IntraScratch; 2],
) -> (u8, u32, u32) {
    let rates = &c.uv_mode_rates[y_mode as usize];
    let mut uv = (DC_PRED, 0u32, 0u32, u64::MAX);
    scratch[0].fill(&c.chroma[0]);
    scratch[1].fill(&c.chroma[1]);
    for &m in c.modes {
        let mut d = 0;
        for (s, b) in scratch.iter_mut().zip(&c.chroma) {
            s.predict(m);
            d += dist::satd(b.src, b.src_stride, &s.pred, 64, b.w, b.h);
        }
        let j = cost(d, rates[m as usize], spb);
        if j < uv.3 {
            uv = (m, d, rates[m as usize], j);
        }
    }
    (uv.0, uv.1, uv.2)
}

fn intra_decision(
    mode: u8,
    sub_modes: [u8; 4],
    uv: (u8, u32, u32),
    yd: u32,
    yr: u32,
    spb: u32,
) -> Decision {
    let (dist, rate) = (yd + uv.1, yr + uv.2);
    Decision {
        mode,
        sub_modes,
        uv_mode: uv.0,
        ref_frame: [INTRA_FRAME, NONE_FRAME],
        mv: [Mv::ZERO; 2],
        interp_filter: EIGHTTAP,
        dist,
        rate,
        cost: cost(dist, rate, spb),
    }
}

pub fn best_intra(c: &IntraChoice, scratch: &mut [IntraScratch; 2]) -> Decision {
    let spb = sad_per_bit(c.lambda);
    let (mode, yd, yr, _) = scratch[0].best(&c.luma, c.y_mode_rates, spb, c.modes);
    let uv = best_uv(c, mode, spb, scratch);
    intra_decision(mode, [mode; 4], uv, yd, yr, spb)
}

pub fn best_intra_4x4(
    c: &IntraChoice,
    above: [u8; 2],
    left: [u8; 2],
    sub_rate: &dyn Fn(u8, u8, u8) -> u32,
    scratch: &mut [IntraScratch; 2],
) -> Decision {
    let spb = sad_per_bit(c.lambda);
    let b = &c.luma;
    assert!(b.w == 8 && b.h == 8 && b.tx_size == 0);
    let s = &mut scratch[0];
    s.fill(b);
    let mut modes = [DC_PRED; 4];
    let (mut yd, mut yr) = (0, 0);
    for k in 0..4 {
        let (tx, ty) = (4 * (k & 1), 4 * (k >> 1));
        let a = if k >= 2 { modes[k - 2] } else { above[k] };
        let l = if k & 1 == 1 {
            modes[k - 1]
        } else {
            left[k >> 1]
        };
        let src = &b.src[ty * b.src_stride + tx..];
        let mut best = (DC_PRED, 0, 0, u64::MAX);
        for &m in c.modes {
            s.edges[k].2.predict(m, &mut s.pred[ty * 64 + tx..], 64);
            let d = dist::satd(src, b.src_stride, &s.pred[ty * 64 + tx..], 64, 4, 4);
            let r = sub_rate(a, l, m);
            let j = cost(d, r, spb);
            if j < best.3 {
                best = (m, d, r, j);
            }
        }
        modes[k] = best.0;
        yd += best.1;
        yr += best.2;
    }
    let uv = best_uv(c, modes[3], spb, scratch);
    intra_decision(modes[3], modes, uv, yd, yr, spb)
}

#[derive(Clone, Copy)]
pub struct InterRef<'a> {
    pub frame: i8,
    pub luma: Plane<'a>,
    pub nearest: Mv,
    pub near: Mv,
    pub best: Mv,
    pub hints: &'a [Mv],
    pub ref_rate: u32,
}

pub struct InterChoice<'a> {
    pub src: &'a [u8],
    pub src_stride: usize,
    pub pos: BlockPos,
    pub w: usize,
    pub h: usize,
    pub refs: &'a [InterRef<'a>],
    pub mode_rates: &'a [u32; 4],
    pub is_inter_rate: u32,
    pub mv_cost: &'a MvCost,
    pub allow_hp: bool,
    pub filter: u8,
    pub lambda: u32,
    pub range: i32,
    pub subpel: Subpel,
}

#[derive(Clone, Copy, Debug)]
pub struct CompoundPair {
    pub refs: [usize; 2],
    pub rate: u32,
}

#[derive(Default)]
pub struct InterScratch {
    pred: Vec<u8>,
    pred2: Vec<u8>,
    avg: Vec<u8>,
}

fn predict_satd(
    c: &InterChoice,
    limits: &MvLimits,
    refs: &[&InterRef],
    mvs: &[Mv],
    scratch: &mut InterScratch,
) -> u32 {
    let (x, y) = ((c.pos.mi_col * 8) as isize, (c.pos.mi_row * 8) as isize);
    let at = |mv: Mv| {
        let m = limits.clamp(mv);
        ((x << 4) + 2 * m.col as isize, (y << 4) + 2 * m.row as isize)
    };
    let (x0, y0) = at(mvs[0]);
    inter::mc(
        &refs[0].luma,
        x0,
        y0,
        c.w,
        c.h,
        c.filter,
        &mut scratch.pred,
        64,
    );
    if refs.len() > 1 {
        let (x1, y1) = at(mvs[1]);
        inter::mc(
            &refs[1].luma,
            x1,
            y1,
            c.w,
            c.h,
            c.filter,
            &mut scratch.pred2,
            64,
        );
        for i in 0..c.h {
            let r = i * 64..i * 64 + c.w;
            inter::average(
                &scratch.pred[r.clone()],
                &scratch.pred2[r.clone()],
                c.w,
                1,
                &mut scratch.avg[r],
                c.w,
            );
        }
        return dist::satd(c.src, c.src_stride, &scratch.avg, 64, c.w, c.h);
    }
    dist::satd(c.src, c.src_stride, &scratch.pred, 64, c.w, c.h)
}

pub fn best_inter(c: &InterChoice, scratch: &mut InterScratch) -> Decision {
    best_inter_with(c, &[], scratch)
}

pub fn best_inter_with(
    c: &InterChoice,
    compound: &[CompoundPair],
    scratch: &mut InterScratch,
) -> Decision {
    if scratch.pred.is_empty() {
        scratch.pred = vec![0; 64 * 64];
        scratch.pred2 = vec![0; 64 * 64];
        scratch.avg = vec![0; 64 * 64];
    }
    let spb = sad_per_bit(c.lambda);
    let p = &c.pos;
    let limits = MvLimits::new(p.mi_row, p.mi_col, p.bsize, p.mi_rows, p.mi_cols);
    let (x, y) = (p.mi_col * 8, p.mi_row * 8);
    let mut out = Decision {
        mode: ZEROMV,
        sub_modes: [ZEROMV; 4],
        uv_mode: DC_PRED,
        ref_frame: [LAST_FRAME, NONE_FRAME],
        mv: [Mv::ZERO; 2],
        interp_filter: c.filter,
        dist: u32::MAX,
        rate: 0,
        cost: u64::MAX,
    };
    let mut offer = |mode: u8, frames: [i8; 2], mv: [Mv; 2], d: u32, rate: u32| {
        let j = cost(d, rate, spb);
        if j < out.cost {
            out = Decision {
                mode,
                sub_modes: [mode; 4],
                uv_mode: DC_PRED,
                ref_frame: frames,
                mv,
                interp_filter: c.filter,
                dist: d,
                rate,
                cost: j,
            };
        }
    };
    let mut found = [(Mv::ZERO, 0u32); 4];
    for (i, r) in c.refs.iter().enumerate() {
        let base = c.is_inter_rate + r.ref_rate;
        let frames = [r.frame, NONE_FRAME];
        let mut tried = [Mv::new(i16::MIN, i16::MIN); 3];
        for (k, (mode, mv)) in [(ZEROMV, Mv::ZERO), (NEARESTMV, r.nearest), (NEARMV, r.near)]
            .into_iter()
            .enumerate()
        {
            if tried[..k].contains(&mv) {
                continue;
            }
            tried[k] = mv;
            let d = predict_satd(c, &limits, &[r], &[mv], scratch);
            offer(
                mode,
                frames,
                [mv, Mv::ZERO],
                d,
                base + c.mode_rates[(mode - NEARESTMV) as usize],
            );
        }
        let mut cands = [Mv::ZERO; 12];
        let mut n = 0;
        for &m in [r.best, r.nearest, r.near].iter().chain(r.hints.iter()) {
            if n < cands.len() {
                cands[n] = m;
                n += 1;
            }
        }
        let s = Search {
            src: c.src,
            src_stride: c.src_stride,
            reference: r.luma,
            x,
            y,
            w: c.w,
            h: c.h,
            limits,
            mv_cost: c.mv_cost,
            ref_mv: r.best,
            allow_hp: c.allow_hp,
            sad_per_bit: spb,
            range: c.range,
            subpel: c.subpel,
            filter: c.filter,
        };
        let m = s.run(&cands[..n]);
        if let Some(f) = found.get_mut(i) {
            *f = (m.mv, m.rate);
        }
        if !tried.contains(&m.mv) {
            let rate = base + c.mode_rates[(NEWMV - NEARESTMV) as usize] + m.rate;
            offer(NEWMV, frames, [m.mv, Mv::ZERO], m.dist, rate);
        }
    }
    for pair in compound {
        let [i, k] = pair.refs;
        if i.max(k) >= c.refs.len().min(found.len()) {
            continue;
        }
        let (a, b) = (&c.refs[i], &c.refs[k]);
        let base = c.is_inter_rate + pair.rate;
        let frames = [a.frame, b.frame];
        for (mode, mvs, extra) in [
            (ZEROMV, [Mv::ZERO; 2], 0),
            (NEARESTMV, [a.nearest, b.nearest], 0),
            (NEARMV, [a.near, b.near], 0),
            (NEWMV, [found[i].0, found[k].0], found[i].1 + found[k].1),
        ] {
            let d = predict_satd(c, &limits, &[a, b], &mvs, scratch);
            offer(
                mode,
                frames,
                mvs,
                d,
                base + c.mode_rates[(mode - NEARESTMV) as usize] + extra,
            );
        }
    }
    out
}
