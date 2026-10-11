use crate::hevc::pel::Pel;
use crate::hevc::picture::ColMv;
use crate::hevc::simd;
use crate::hevc::slicedec::Progress;
use crate::hevc::slicedec::{
    E_PU_H, E_PU_V, E_TU_H, E_TU_V, F_CBF, F_CODED, F_INTRA, F_NOFILTER, MvField, PicState,
};
use crate::hevc::spec_tables::{BETA_TABLE, TC_TABLE};
use crate::hevc::tables::qpc_420;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;

pub fn ctb_decoded<P: Pel>(pic: &PicState<P>, ry: usize) {
    let done = pic.row_ctbs[ry].fetch_add(1, Ordering::SeqCst) + 1;
    pic.row_wakers[ry].wake();
    if done as usize == pic.wctb {
        run(pic);
    }
}

fn row_complete<P: Pel>(pic: &PicState<P>, r: usize) -> bool {
    pic.row_ctbs[r].load(Ordering::SeqCst) as usize >= pic.wctb
}

pub fn run<P: Pel>(pic: &PicState<P>) {
    loop {
        let Ok(mut st) = pic.filter.try_lock() else {
            return;
        };
        advance(pic, &mut st, false);
        let next = st.rows_complete;
        drop(st);
        if next >= pic.hctb || !row_complete(pic, next) {
            return;
        }
    }
}

fn advance<P: Pel>(pic: &PicState<P>, st: &mut Progress, all: bool) {
    let h = pic.hctb;
    while st.rows_complete < h && row_complete(pic, st.rows_complete) {
        st.rows_complete += 1;
    }
    loop {
        let r = st.rows_deblocked;
        if r < h && (all || st.rows_complete > r) {
            deblock_row(pic, r);
            st.rows_deblocked += 1;
            continue;
        }
        let r = st.rows_topped;
        if r < h && st.rows_deblocked > r {
            publish_top(pic, r);
            st.rows_topped += 1;
            continue;
        }
        let r = st.rows_published;
        if r < st.rows_topped && st.rows_deblocked >= (r + 2).min(h) {
            publish_bottom(pic, r);
            st.rows_published += 1;
            continue;
        }
        break;
    }
}

pub fn finish<P: Pel>(pic: &PicState<P>) {
    pic.fill_missing();
    let mut st = pic.filter.lock().unwrap_or_else(PoisonError::into_inner);
    st.rows_complete = pic.hctb;
    advance(pic, &mut st, true);
}

#[inline]
fn tc_beta(q: i32, bs: i32, tc_off: i32, beta_off: i32, bd: u32) -> (i32, i32) {
    let qb = (q + beta_off).clamp(0, 51);
    let qt = (q + 2 * (bs - 1) + tc_off).clamp(0, 53);
    (
        (TC_TABLE[qt as usize] as i32) << (bd - 8),
        (BETA_TABLE[qb as usize] as i32) << (bd - 8),
    )
}

fn boundary_strength<P: Pel>(
    pic: &PicState<P>,
    pi: usize,
    qi: usize,
    tu_edge: bool,
    (ps, qs): (u32, u32),
) -> i32 {
    let (p, q) = (pic.blk.get(pi), pic.blk.get(qi));
    if (p.flags | q.flags) & F_INTRA != 0 {
        return 2;
    }
    if tu_edge && (p.flags | q.flags) & F_CBF != 0 {
        return 1;
    }
    let (mp, mq) = (pic.mvf.get(pi), pic.mvf.get(qi));
    motion_bs(pic, ps, &mp, qs, &mq)
}

fn ref_id<P: Pel>(pic: &PicState<P>, s: u32, f: &MvField, l: usize) -> u32 {
    if s == u32::MAX {
        return u32::MAX;
    }
    pic.slices[s as usize].ref_ids[l]
        .get(f.ref_idx[l] as usize)
        .copied()
        .unwrap_or(u32::MAX - 1)
}

fn motion_bs<P: Pel>(pic: &PicState<P>, ps: u32, p: &MvField, qs: u32, q: &MvField) -> i32 {
    let np = p.pred(0) as u32 + p.pred(1) as u32;
    let nq = q.pred(0) as u32 + q.pred(1) as u32;
    if np != nq {
        return 1;
    }
    let far = |a: [i16; 2], b: [i16; 2]| {
        (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4
    };
    if np == 1 {
        let lp = if p.pred(0) { 0 } else { 1 };
        let lq = if q.pred(0) { 0 } else { 1 };
        if ref_id(pic, ps, p, lp) != ref_id(pic, qs, q, lq) {
            return 1;
        }
        return far(p.mv[lp], q.mv[lq]) as i32;
    }
    let (p0, p1) = (ref_id(pic, ps, p, 0), ref_id(pic, ps, p, 1));
    let (q0, q1) = (ref_id(pic, qs, q, 0), ref_id(pic, qs, q, 1));
    if !((p0 == q0 && p1 == q1) || (p0 == q1 && p1 == q0)) {
        return 1;
    }
    if p0 != p1 {
        if p0 == q0 {
            (far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1])) as i32
        } else {
            (far(p.mv[0], q.mv[1]) || far(p.mv[1], q.mv[0])) as i32
        }
    } else {
        ((far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1]))
            && (far(p.mv[0], q.mv[1]) || far(p.mv[1], q.mv[0]))) as i32
    }
}

fn edge_params<P: Pel>(
    pic: &PicState<P>,
    px: usize,
    py: usize,
    qx: usize,
    qy: usize,
) -> Option<(i32, i32)> {
    let s = pic.log2_ctb;
    let pc = (py >> s) * pic.wctb + (px >> s);
    let qc = (qy >> s) * pic.wctb + (qx >> s);
    let qs = pic.ctb_slice.get(qc);
    let ps = pic.ctb_slice.get(pc);
    if qs == u32::MAX || ps == u32::MAX {
        return None;
    }
    let sq = &pic.slices[qs as usize];
    if sq.deblock_disabled {
        return None;
    }
    if pc != qc {
        if pic.ctb_addr.get(pc) != pic.ctb_addr.get(qc) && !sq.lf_across {
            return None;
        }
        if !pic.pps.loop_filter_across_tiles
            && pic.layout.tile_of_rs(pc as u32) != pic.layout.tile_of_rs(qc as u32)
        {
            return None;
        }
    }
    Some((sq.tc_offset, sq.beta_offset))
}

#[derive(Clone, Copy, Default)]
struct Seg {
    on: bool,
    tc: i32,
    beta: i32,
    nfp: bool,
    nfq: bool,
}

impl Seg {
    fn new(tc: i32, beta: i32, pf: u8, qf: u8) -> Self {
        Seg {
            on: true,
            tc,
            beta,
            nfp: pf & F_NOFILTER != 0,
            nfq: qf & F_NOFILTER != 0,
        }
    }
}

#[inline(always)]
fn luma_edges<P: Pel>(
    s: &mut [P],
    segs: &[Seg],
    at: impl Fn(usize) -> usize,
    step: usize,
    along: usize,
    bd: u32,
    mut batch: impl FnMut(&mut [P], usize, &simd::dbk::Edge4) -> bool,
) {
    let full = segs.len() / 4 * 4;
    for (c, four) in segs[..full].chunks_exact(4).enumerate() {
        if !four.iter().any(|g| g.on) {
            continue;
        }
        let mut e = simd::dbk::Edge4::default();
        for (k, g) in four.iter().enumerate().filter(|(_, g)| g.on) {
            e.tc[k] = g.tc as i16;
            e.beta[k] = g.beta as i16;
            e.nfp[k] = g.nfp;
            e.nfq[k] = g.nfq;
        }
        if batch(s, at(4 * c), &e) {
            continue;
        }
        for (k, g) in four.iter().enumerate().filter(|(_, g)| g.on) {
            filter_luma(
                s,
                at(4 * c + k),
                step,
                along,
                g.tc,
                g.beta,
                g.nfp,
                g.nfq,
                bd,
            );
        }
    }
    for (i, g) in segs.iter().enumerate().skip(full).filter(|(_, g)| g.on) {
        filter_luma(s, at(i), step, along, g.tc, g.beta, g.nfp, g.nfq, bd);
    }
}

crate::hevc::isa::dispatch! {
fn deblock_row<P: Pel>(pic: &PicState<P>, r: usize) {
    if !pic.deblocking_enabled_anywhere || pic.draft {
        return;
    }
    let ctb = 1usize << pic.log2_ctb;
    let y0 = r * ctb;
    let y1 = (y0 + ctb).min(pic.height);
    let (w, cw) = (pic.stride[0], pic.stride[1]);
    let lbase = y0.saturating_sub(4) * w;
    let lend = (y1 - 1) * w + pic.width;
    let luma = pic.plane(0).slice_mut(pic.org[0] + lbase..pic.org[0] + lend);
    let cbase = (y0 / 2).saturating_sub(2) * cw;
    let cend = ((y1 / 2).min(pic.cheight) - 1) * cw + pic.cwidth;
    let chroma = [
        pic.plane(1).slice_mut(pic.org[1] + cbase..pic.org[1] + cend),
        pic.plane(2).slice_mut(pic.org[2] + cbase..pic.org[2] + cend),
    ];
    let w4 = pic.w4;
    let (bd, bdc) = (pic.bd_y, pic.bd_c);
    let (cb_off, cr_off) = (pic.pps.cb_qp_offset, pic.pps.cr_qp_offset);
    let ctb_params: Vec<Option<(i32, i32)>> = (0..pic.wctb)
        .map(|cx| edge_params(pic, cx << pic.log2_ctb, y0, cx << pic.log2_ctb, y0))
        .collect();
    let rows4 = (y1 - y0) / 4;
    let ry = r;
    let mut segs = vec![Seg::default(); w4.max(rows4)];
    let mut bs_v = vec![0i8; w4 * rows4];
    for bx in (2..w4).step_by(2) {
        let x = bx * 4;
        for (i, by) in (y0 / 4..y1 / 4).enumerate() {
            segs[i].on = false;
            let qi = by * w4 + bx;
            let qb = pic.blk.get(qi);
            let e = qb.edges;
            if e & (E_TU_V | E_PU_V) == 0 || qb.flags & F_CODED == 0 {
                continue;
            }
            let y = by * 4;
            let params = if x & (ctb - 1) != 0 {
                ctb_params[x >> pic.log2_ctb]
            } else {
                edge_params(pic, x - 1, y, x, y)
            };
            let Some((tc_off, beta_off)) = params else {
                continue;
            };
            let cq = ry * pic.wctb + (x >> pic.log2_ctb);
            let cp = ry * pic.wctb + ((x - 1) >> pic.log2_ctb);
            let sl = (pic.ctb_slice.get(cp), pic.ctb_slice.get(cq));
            let bs = boundary_strength(pic, qi - 1, qi, e & E_TU_V != 0, sl);
            bs_v[i * w4 + bx] = bs as i8;
            if bs == 0 {
                continue;
            }
            let (p, q) = (pic.blk.get(qi - 1), qb);
            let qpl = (p.qp as i32 + q.qp as i32 + 1) >> 1;
            let (tc, beta) = tc_beta(qpl, bs, tc_off, beta_off, bd);
            segs[i] = Seg::new(tc, beta, p.flags, q.flags);
        }
        let at = |i: usize| (y0 + 4 * i) * w + x - lbase;
        luma_edges(luma, &segs[..rows4], at, 1, w, bd, |s, o, e| {
            simd::dbk::luma_v(s, o, w, e, bd)
        });
    }
    for by in (y0 / 4..y1 / 4).step_by(2) {
        for bx in (4..w4).step_by(4) {
            if bs_v[(by - y0 / 4) * w4 + bx] != 2 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let qi = by * w4 + bx;
            let Some((tc_off, _)) = edge_params(pic, x - 1, y, x, y) else {
                continue;
            };
            let (p, q) = (pic.blk.get(qi - 1), pic.blk.get(qi));
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            for (c, off) in [(1, cb_off), (2, cr_off)] {
                let qpi = ((p.qp as i32 + q.qp as i32 + 1) >> 1) + off;
                let qt = (qpc_420(qpi) + 2 + tc_off).clamp(0, 53);
                let tc = (TC_TABLE[qt as usize] as i32) << (bdc - 8);
                filter_chroma(
                    chroma[c - 1],
                    (y / 2) * cw + x / 2 - cbase,
                    1,
                    cw,
                    tc,
                    nfp,
                    nfq,
                    bdc,
                );
            }
        }
    }
    let mut bs_h = vec![0i8; w4 * rows4];
    for by in (y0 / 4..y1 / 4).filter(|b| b % 2 == 0 && *b > 0) {
        let y = by * 4;
        for bx in 0..w4 {
            segs[bx].on = false;
            let qi = by * w4 + bx;
            let qb = pic.blk.get(qi);
            let e = qb.edges;
            if e & (E_TU_H | E_PU_H) == 0 || qb.flags & F_CODED == 0 {
                continue;
            }
            let x = bx * 4;
            let params = if y & (ctb - 1) != 0 {
                ctb_params[x >> pic.log2_ctb]
            } else {
                edge_params(pic, x, y - 1, x, y)
            };
            let Some((tc_off, beta_off)) = params else {
                continue;
            };
            let pi = qi - w4;
            let cq = (y >> pic.log2_ctb) * pic.wctb + (x >> pic.log2_ctb);
            let cp = ((y - 1) >> pic.log2_ctb) * pic.wctb + (x >> pic.log2_ctb);
            let sl = (pic.ctb_slice.get(cp), pic.ctb_slice.get(cq));
            let bs = boundary_strength(pic, pi, qi, e & E_TU_H != 0, sl);
            bs_h[(by - y0 / 4) * w4 + bx] = bs as i8;
            if bs == 0 {
                continue;
            }
            let (p, q) = (pic.blk.get(pi), qb);
            let qpl = (p.qp as i32 + q.qp as i32 + 1) >> 1;
            let (tc, beta) = tc_beta(qpl, bs, tc_off, beta_off, bd);
            segs[bx] = Seg::new(tc, beta, p.flags, q.flags);
        }
        let at = |i: usize| y * w + 4 * i - lbase;
        luma_edges(luma, &segs[..w4], at, w, 1, bd, |s, o, e| {
            simd::dbk::luma_h(s, o, w, e, bd)
        });
    }
    for by in (y0 / 4..y1 / 4).filter(|b| b % 4 == 0 && *b > 0) {
        for bx in (0..w4).step_by(2) {
            if bs_h[(by - y0 / 4) * w4 + bx] != 2 {
                continue;
            }
            let (x, y) = (bx * 4, by * 4);
            let qi = by * w4 + bx;
            let Some((tc_off, _)) = edge_params(pic, x, y - 1, x, y) else {
                continue;
            };
            let (p, q) = (pic.blk.get(qi - w4), pic.blk.get(qi));
            let (nfp, nfq) = (p.flags & F_NOFILTER != 0, q.flags & F_NOFILTER != 0);
            for (c, off) in [(1, cb_off), (2, cr_off)] {
                let qpi = ((p.qp as i32 + q.qp as i32 + 1) >> 1) + off;
                let qt = (qpc_420(qpi) + 2 + tc_off).clamp(0, 53);
                let tc = (TC_TABLE[qt as usize] as i32) << (bdc - 8);
                filter_chroma(
                    chroma[c - 1],
                    (y / 2) * cw + x / 2 - cbase,
                    cw,
                    1,
                    tc,
                    nfp,
                    nfq,
                    bdc,
                );
            }
        }
    }
}
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
pub(crate) fn filter_luma<P: Pel>(
    s: &mut [P],
    o: usize,
    step: usize,
    along: usize,
    tc: i32,
    beta: i32,
    nfp: bool,
    nfq: bool,
    bd: u32,
) {
    let px = |s: &[P], k: usize, i: usize| s[o + k * along - (i + 1) * step].into();
    let qx = |s: &[P], k: usize, i: usize| s[o + k * along + i * step].into();
    let dp0 = (px(s, 0, 2) - 2 * px(s, 0, 1) + px(s, 0, 0)).abs();
    let dp3 = (px(s, 3, 2) - 2 * px(s, 3, 1) + px(s, 3, 0)).abs();
    let dq0 = (qx(s, 0, 2) - 2 * qx(s, 0, 1) + qx(s, 0, 0)).abs();
    let dq3 = (qx(s, 3, 2) - 2 * qx(s, 3, 1) + qx(s, 3, 0)).abs();
    let (dpq0, dpq3) = (dp0 + dq0, dp3 + dq3);
    let (dp, dq) = (dp0 + dp3, dq0 + dq3);
    let d = dpq0 + dpq3;
    if d >= beta {
        return;
    }
    let dsam = |s: &[P], k: usize, dpq: i32| -> bool {
        dpq < (beta >> 2)
            && (px(s, k, 3) - px(s, k, 0)).abs() + (qx(s, k, 0) - qx(s, k, 3)).abs() < (beta >> 3)
            && (px(s, k, 0) - qx(s, k, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = dsam(s, 0, 2 * dpq0) && dsam(s, 3, 2 * dpq3);
    let dep = dp < ((beta + (beta >> 1)) >> 3);
    let deq = dq < ((beta + (beta >> 1)) >> 3);
    let max = (1i32 << bd) - 1;
    for k in 0..4 {
        let base = o + k * along;
        let p = |i: usize| -> i32 { s[base - (i + 1) * step].into() };
        let q = |i: usize| -> i32 { s[base + i * step].into() };
        let (p0, p1, p2, p3) = (p(0), p(1), p(2), p(3));
        let (q0, q1, q2, q3) = (q(0), q(1), q(2), q(3));
        if strong {
            let t2 = 2 * tc;
            if !nfp {
                s[base - step] = P::from_i32(
                    ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - t2, p0 + t2),
                );
                s[base - 2 * step] =
                    P::from_i32(((p2 + p1 + p0 + q0 + 2) >> 2).clamp(p1 - t2, p1 + t2));
                s[base - 3 * step] = P::from_i32(
                    ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(p2 - t2, p2 + t2),
                );
            }
            if !nfq {
                s[base] = P::from_i32(
                    ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - t2, q0 + t2),
                );
                s[base + step] =
                    P::from_i32(((p0 + q0 + q1 + q2 + 2) >> 2).clamp(q1 - t2, q1 + t2));
                s[base + 2 * step] = P::from_i32(
                    ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3).clamp(q2 - t2, q2 + t2),
                );
            }
        } else {
            let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            delta = delta.clamp(-tc, tc);
            if !nfp {
                s[base - step] = P::from_i32((p0 + delta).clamp(0, max));
            }
            if !nfq {
                s[base] = P::from_i32((q0 - delta).clamp(0, max));
            }
            if dep && !nfp {
                let dp = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                s[base - 2 * step] = P::from_i32((p1 + dp).clamp(0, max));
            }
            if deq && !nfq {
                let dq = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                s[base + step] = P::from_i32((q1 + dq).clamp(0, max));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn filter_chroma<P: Pel>(
    s: &mut [P],
    o: usize,
    step: usize,
    along: usize,
    tc: i32,
    nfp: bool,
    nfq: bool,
    bd: u32,
) {
    let max = (1i32 << bd) - 1;
    for k in 0..4 {
        let base = o + k * along;
        let (p0, p1): (i32, i32) = (s[base - step].into(), s[base - 2 * step].into());
        let (q0, q1): (i32, i32) = (s[base].into(), s[base + step].into());
        let delta = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
        if !nfp {
            s[base - step] = P::from_i32((p0 + delta).clamp(0, max));
        }
        if !nfq {
            s[base] = P::from_i32((q0 - delta).clamp(0, max));
        }
    }
}

struct SaoCtb {
    x0: usize,
    x1: usize,
    nofilter: bool,
    p: crate::hevc::slicedec::SaoParams,
    edge_rows: [bool; 2],
    near: [[bool; 3]; 3],
}

crate::hevc::isa::dispatch! {
fn sao_row<P: Pel>(pic: &PicState<P>, r: usize, bottom: bool) {
    let ctb = 1usize << pic.log2_ctb;
    for c in 0..3 {
        let (w, h, cs) = if c == 0 {
            (pic.width, pic.height, ctb)
        } else {
            (pic.cwidth, pic.cheight, ctb / 2)
        };
        let y0 = r * cs;
        let y1 = (y0 + cs).min(h);
        let (ly0, ly1) = pic.frame.part_range(r, c, bottom);
        if ly0 == ly1 {
            continue;
        }
        let g = pic.plane(c);
        let (st, org) = (pic.stride[c], pic.org[c]);
        let line = |y: usize| g.slice(org + y * st..org + y * st + w);
        let edges = &pic.sao_edges[c];
        if !bottom {
            edges.slice_mut(2 * r * w..2 * r * w + w).copy_from_slice(line(ly0));
            edges
                .slice_mut((2 * r + 1) * w..(2 * r + 2) * w)
                .copy_from_slice(line(ly1 - 1));
        }
        let ctbs: Vec<SaoCtb> = (0..pic.wctb)
            .filter(|_| !pic.draft)
            .filter_map(|rx| {
                let rs = r * pic.wctb + rx;
                let si = pic.ctb_slice.get(rs);
                let sl = pic.slices.get(si as usize)?;
                if !(if c == 0 { sl.sao_luma } else { sl.sao_chroma }) {
                    return None;
                }
                let p = pic.sao.get(rs);
                if p.type_idx[c] == 0 {
                    return None;
                }
                let x0 = rx * cs;
                let x1 = (x0 + cs).min(w);
                Some(sao_prepare(pic, c, rs, (x0, x1), (y0, y1), (w, h), p))
            })
            .collect();
        if !ctbs.is_empty() {
            let above0: &[P] = if !bottom {
                if ly0 > 0 { line(ly0 - 1) } else { &[] }
            } else {
                edges.slice((2 * r + 1) * w..(2 * r + 2) * w)
            };
            let below1: &[P] = if !bottom {
                if ly1 < h { line(ly1) } else { &[] }
            } else if ly1 < h {
                edges.slice(2 * (r + 1) * w..2 * (r + 1) * w + w)
            } else {
                &[]
            };
            let mut prev: Vec<P> = vec![P::default(); w];
            let mut cur: Vec<P> = vec![P::default(); w];
            for y in ly0..ly1 {
                cur.copy_from_slice(line(y));
                let above = if y == ly0 { above0 } else { &prev[..] };
                let below = if y + 1 == ly1 { below1 } else { line(y + 1) };
                let dst = g.slice_mut(org + y * st..org + y * st + w);
                for k in &ctbs {
                    sao_line(pic, c, k, (y0, y1), (w, h), y, [above, &cur, below], dst);
                }
                std::mem::swap(&mut prev, &mut cur);
            }
        }
        pic.frame.pad_rows::<P>(c, ly0, ly1);
        pic.frame.emit::<P>(c, ly0, ly1);
    }
}
}

fn cross_ok<P: Pel>(pic: &PicState<P>, c: usize, rs: usize, xn: usize, yn: usize) -> bool {
    let s = pic.log2_ctb as usize - if c == 0 { 0 } else { 1 };
    let nrs = (yn >> s) * pic.wctb + (xn >> s);
    if nrs == rs {
        return true;
    }
    let ns = pic.ctb_slice.get(nrs);
    let Some(nsl) = pic.slices.get(ns as usize) else {
        return false;
    };
    let cur_ts = pic.layout.rs_to_ts[rs];
    if pic.ctb_addr.get(nrs) != pic.ctb_addr.get(rs) {
        let nts = pic.layout.rs_to_ts[nrs];
        let lf_cur = pic
            .slices
            .get(pic.ctb_slice.get(rs) as usize)
            .is_some_and(|s| s.lf_across);
        if nts < cur_ts && !lf_cur {
            return false;
        }
        if cur_ts < nts && !nsl.lf_across {
            return false;
        }
    }
    pic.pps.loop_filter_across_tiles
        || pic.layout.tile_id[pic.layout.rs_to_ts[nrs] as usize]
            == pic.layout.tile_id[cur_ts as usize]
}

fn eo_dirs(class: u8) -> ([i32; 2], [i32; 2]) {
    match class {
        0 => ([-1, 1], [0, 0]),
        1 => ([0, 0], [-1, 1]),
        2 => ([-1, 1], [-1, 1]),
        _ => ([1, -1], [-1, 1]),
    }
}

#[inline(always)]
fn sao_nofilter<P: Pel>(pic: &PicState<P>, c: usize, x: usize, y: usize) -> bool {
    let sc = if c == 0 { 0 } else { 1 };
    pic.blk
        .get(((y << sc) >> 2) * pic.w4 + ((x << sc) >> 2))
        .flags
        & F_NOFILTER
        != 0
}

fn sao_prepare<P: Pel>(
    pic: &PicState<P>,
    c: usize,
    rs: usize,
    (x0, x1): (usize, usize),
    (y0, y1): (usize, usize),
    (w, h): (usize, usize),
    p: crate::hevc::slicedec::SaoParams,
) -> SaoCtb {
    let sc = if c == 0 { 0 } else { 1 };
    let mut nofilter = false;
    if pic.nofilter_possible {
        for by in ((y0 << sc) >> 2)..(((y1 << sc) + 3) >> 2).min(pic.h4) {
            for bx in ((x0 << sc) >> 2)..(((x1 << sc) + 3) >> 2).min(pic.w4) {
                nofilter |= pic.blk.get(by * pic.w4 + bx).flags & F_NOFILTER != 0;
            }
        }
    }
    let (hp, vp) = eo_dirs(p.eo_class[c]);
    let mx = (hp[0] != 0) as usize;
    let vert_ok = |yn: i32| -> bool {
        if yn < 0 || yn >= h as i32 {
            return false;
        }
        let yn = yn as usize;
        (y0..y1).contains(&yn) || cross_ok(pic, c, rs, x0 + mx, yn)
    };
    let edge_rows = [y0, y1 - 1]
        .map(|y| vp[0] == 0 || (vert_ok(y as i32 + vp[0]) && vert_ok(y as i32 + vp[1])));
    let mut near = [[false; 3]; 3];
    for (i, row) in near.iter_mut().enumerate() {
        for (j, ok) in row.iter_mut().enumerate() {
            let xn = [x0 as i32 - 1, x0 as i32, x1 as i32][j];
            let yn = [y0 as i32 - 1, y0 as i32, y1 as i32][i];
            *ok = xn >= 0
                && yn >= 0
                && (yn as usize) < h
                && (xn as usize) < w
                && cross_ok(pic, c, rs, xn as usize, yn as usize);
        }
    }
    SaoCtb {
        x0,
        x1,
        nofilter,
        p,
        edge_rows,
        near,
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn sao_line<P: Pel>(
    pic: &PicState<P>,
    c: usize,
    k: &SaoCtb,
    (y0, y1): (usize, usize),
    (w, h): (usize, usize),
    y: usize,
    lines: [&[P]; 3],
    dst: &mut [P],
) {
    let (x0, x1, p) = (k.x0, k.x1, &k.p);
    let bd = if c == 0 { pic.bd_y } else { pic.bd_c };
    let max = (1i32 << bd) - 1;
    let offs = p.offsets[c].map(|v| v as i32);
    let cur = lines[1];
    if p.type_idx[c] == 1 {
        let shift = bd - 5;
        if !k.nofilter {
            let band = (p.band_pos[c] as i32, offs);
            sao_band_run(&cur[x0..x1], &mut dst[x0..x1], shift, band, max);
            return;
        }
        let mut table = [0i32; 32];
        for j in 0..4 {
            table[(j + p.band_pos[c] as usize) & 31] = offs[j];
        }
        for x in x0..x1 {
            if sao_nofilter(pic, c, x, y) {
                continue;
            }
            let v: i32 = cur[x].into();
            dst[x] = P::from_i32((v + table[(v >> shift) as usize]).clamp(0, max));
        }
        return;
    }
    let (hp, vp) = eo_dirs(p.eo_class[c]);
    let edge_map = [1usize, 2, 0, 3, 4];
    let by_sum = edge_map.map(|e| if e == 0 { 0 } else { offs[e - 1] });
    let general = |x: usize| -> Option<P> {
        if k.nofilter && sao_nofilter(pic, c, x, y) {
            return None;
        }
        let mut sum = 0i32;
        let v: i32 = cur[x].into();
        for j in 0..2 {
            let xn = x as i32 + hp[j];
            let yn = y as i32 + vp[j];
            if xn < 0 || yn < 0 || xn >= w as i32 || yn >= h as i32 {
                return None;
            }
            let (xn, yn) = (xn as usize, yn as usize);
            let ni = if yn < y0 {
                0
            } else if yn >= y1 {
                2
            } else {
                1
            };
            let nj = if xn < x0 {
                0
            } else if xn >= x1 {
                2
            } else {
                1
            };
            if !k.near[ni][nj] {
                return None;
            }
            let n = lines[(1 + vp[j]) as usize].get(xn).copied()?;
            sum += (v - n.into()).signum();
        }
        Some(P::from_i32((v + by_sum[(2 + sum) as usize]).clamp(0, max)))
    };
    let mx = (hp[0] != 0) as usize;
    let run_ok = if y == y0 {
        k.edge_rows[0]
    } else if y == y1 - 1 {
        k.edge_rows[1]
    } else {
        true
    };
    let inner = !k.nofilter && x1 - x0 > 2 * mx;
    if !inner {
        for x in x0..x1 {
            if let Some(v) = general(x) {
                dst[x] = v;
            }
        }
        return;
    }
    let edge = y == y0 || y == y1 - 1;
    let (mut lo, mut hi) = (x0 + mx, x1 - mx);
    if mx == 1 {
        if edge {
            for x in [x0, x1 - 1] {
                if let Some(v) = general(x) {
                    dst[x] = v;
                }
            }
        } else {
            lo -= k.near[1][0] as usize;
            hi += k.near[1][2] as usize;
        }
    }
    if !run_ok {
        return;
    }
    let n = hi - lo;
    let (xa, xb) = ((lo as i32 + hp[0]) as usize, (lo as i32 + hp[1]) as usize);
    let (la, lb) = (lines[(1 + vp[0]) as usize], lines[(1 + vp[1]) as usize]);
    let (Some(na), Some(nb)) = (la.get(xa..xa + n), lb.get(xb..xb + n)) else {
        return;
    };
    sao_edge_run(&cur[lo..hi], na, nb, &mut dst[lo..hi], &by_sum, max);
}

const SAO_LANES: usize = 16;

#[inline(always)]
fn sao_edge_run<P: Pel>(cur: &[P], na: &[P], nb: &[P], out: &mut [P], by_sum: &[i32; 5], max: i32) {
    let o = by_sum.map(|v| v as i16);
    let max = max as i16;
    let lane = |v: i16, a: i16, b: i16| -> P {
        let s = ((v > a) as i16 - (v < a) as i16) + ((v > b) as i16 - (v < b) as i16);
        let m = |k: i16| -((s == k) as i16);
        let off = (o[0] & m(-2)) | (o[1] & m(-1)) | (o[3] & m(1)) | (o[4] & m(2));
        P::from_i32((v + off).max(0).min(max) as i32)
    };
    let n = cur.len();
    let full = n - n % SAO_LANES;
    for i in (0..full).step_by(SAO_LANES) {
        let (Some(c), Some(a), Some(b), Some(d)) =
            (lanes(cur, i), lanes(na, i), lanes(nb, i), lanes_mut(out, i))
        else {
            return;
        };
        for l in 0..SAO_LANES {
            d[l] = lane(c[l].into() as i16, a[l].into() as i16, b[l].into() as i16);
        }
    }
    for i in full..n {
        out[i] = lane(
            cur[i].into() as i16,
            na[i].into() as i16,
            nb[i].into() as i16,
        );
    }
}

#[inline(always)]
fn lanes<P>(s: &[P], i: usize) -> Option<&[P; SAO_LANES]> {
    s.get(i..)?.first_chunk()
}

#[inline(always)]
fn lanes_mut<P>(s: &mut [P], i: usize) -> Option<&mut [P; SAO_LANES]> {
    s.get_mut(i..)?.first_chunk_mut()
}

#[inline(always)]
fn sao_band_run<P: Pel>(
    src: &[P],
    out: &mut [P],
    shift: u32,
    (band_pos, offs): (i32, [i32; 4]),
    max: i32,
) {
    let o = offs.map(|v| v as i16);
    let (bp, max) = (band_pos as i16, max as i16);
    let lane = |v: i16| -> P {
        let k = ((v >> shift) - bp) & 31;
        let m = |j: i16| -((k == j) as i16);
        let off = (o[0] & m(0)) | (o[1] & m(1)) | (o[2] & m(2)) | (o[3] & m(3));
        P::from_i32((v + off).max(0).min(max) as i32)
    };
    let n = src.len();
    let full = n - n % SAO_LANES;
    for i in (0..full).step_by(SAO_LANES) {
        let (Some(c), Some(d)) = (lanes(src, i), lanes_mut(out, i)) else {
            return;
        };
        for l in 0..SAO_LANES {
            d[l] = lane(c[l].into() as i16);
        }
    }
    for i in full..n {
        out[i] = lane(src[i].into() as i16);
    }
}

fn publish_top<P: Pel>(pic: &PicState<P>, r: usize) {
    let ctb = 1usize << pic.log2_ctb;
    let (y0, y1) = (r * ctb, ((r + 1) * ctb).min(pic.height));
    let (c0, c1) = (y0 / 2, y1.div_ceil(2));
    let (w, cw) = (pic.width, pic.cwidth);
    let w16 = pic.width.div_ceil(16);
    let rows16 = (y1 - y0).div_ceil(16);
    let mut col = vec![ColMv::default(); w16 * rows16];
    for j in 0..rows16 {
        for i in 0..w16 {
            let (x, y) = (i * 16, y0 + j * 16);
            let bi = (y >> 2) * pic.w4 + (x >> 2);
            let b = pic.blk.get(bi);
            if b.flags & F_CODED == 0 || b.flags & F_INTRA != 0 {
                continue;
            }
            let f = pic.mvf.get(bi);
            let ctbi = (y >> pic.log2_ctb) * pic.wctb + (x >> pic.log2_ctb);
            let si = pic.ctb_slice.get(ctbi);
            if si == u32::MAX {
                continue;
            }
            let sl = &pic.slices[si as usize];
            let mut cm = ColMv {
                mv: f.mv,
                ref_poc: [0; 2],
                flags: 0,
            };
            for l in 0..2 {
                if f.pred(l) {
                    let ri = f.ref_idx[l] as usize;
                    if ri < sl.ref_pocs[l].len() {
                        cm.flags |= 1 << l;
                        cm.ref_poc[l] = sl.ref_pocs[l][ri];
                        if sl.ref_lt[l][ri] {
                            cm.flags |= 4 << l;
                        }
                    }
                }
            }
            col[j * w16 + i] = cm;
        }
    }
    sao_row(pic, r, false);
    let _ = (w, cw, c0, c1);
    pic.frame.publish_top(r, col.into());
}

fn publish_bottom<P: Pel>(pic: &PicState<P>, r: usize) {
    sao_row(pic, r, true);
    pic.frame.publish_bottom(r);
}
