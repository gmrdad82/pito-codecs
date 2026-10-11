use crate::h264::dec::picture::{MbKind, MbState};
use crate::h264::dec::slicedec::{PicState, SliceInfo};
use crate::h264::dec::tables::{ALPHA, BETA, TC0};

#[derive(Clone, Copy)]
struct Motion {
    ids: [u32; 2],
    mv: [[i16; 2]; 2],
    count: u8,
}

fn ref_ids8(st: &MbState, sl: &SliceInfo) -> [[u32; 2]; 4] {
    std::array::from_fn(|b8| {
        std::array::from_fn(|l| {
            let r = st.ref_idx[l][b8];
            if r >= 0 {
                sl.ref_ids[l]
                    .get(r as usize)
                    .copied()
                    .unwrap_or(u32::MAX - 1)
            } else {
                u32::MAX
            }
        })
    })
}

#[inline(always)]
fn block_motion(st: &MbState, ids8: &[[u32; 2]; 4], raster: usize) -> Motion {
    let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
    let ids = ids8[b8];
    let count = (ids[0] != u32::MAX) as u8 + (ids[1] != u32::MAX) as u8;
    Motion {
        ids,
        mv: [st.mv[0][raster], st.mv[1][raster]],
        count,
    }
}

#[inline(always)]
fn mv_far(a: [i16; 2], b: [i16; 2]) -> bool {
    (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4
}

#[inline]
fn motion_bs(p: &Motion, q: &Motion) -> u8 {
    if p.ids == q.ids && p.mv == q.mv {
        return 0;
    }
    if p.count != q.count {
        return 1;
    }
    if p.count == 1 {
        let (pi, pm) = if p.ids[0] != u32::MAX {
            (p.ids[0], p.mv[0])
        } else {
            (p.ids[1], p.mv[1])
        };
        let (qi, qm) = if q.ids[0] != u32::MAX {
            (q.ids[0], q.mv[0])
        } else {
            (q.ids[1], q.mv[1])
        };
        if pi != qi {
            return 1;
        }
        return mv_far(pm, qm) as u8;
    }
    if p.count == 0 {
        return 0;
    }
    let same_set = (p.ids[0] == q.ids[0] && p.ids[1] == q.ids[1])
        || (p.ids[0] == q.ids[1] && p.ids[1] == q.ids[0]);
    if !same_set {
        return 1;
    }
    if p.ids[0] != p.ids[1] {
        if p.ids[0] == q.ids[0] {
            (mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1])) as u8
        } else {
            (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0])) as u8
        }
    } else {
        ((mv_far(p.mv[0], q.mv[0]) || mv_far(p.mv[1], q.mv[1]))
            && (mv_far(p.mv[0], q.mv[1]) || mv_far(p.mv[1], q.mv[0]))) as u8
    }
}

struct EdgeParams {
    bs: [u8; 4],
    tc0: [i32; 4],
    alpha: i32,
    beta: i32,
}

#[inline(always)]
fn filter_lanes<const N: usize>(s: &mut [[i16; N]; 8], ep: &EdgeParams, chroma: bool) {
    let per = N / 4;
    let (alpha, beta) = (ep.alpha as i16, ep.beta as i16);
    let mut bs_on = [false; N];
    let mut tc0 = [0i16; N];
    for k in 0..4 {
        for i in k * per..(k + 1) * per {
            bs_on[i] = ep.bs[k] != 0;
            tc0[i] = ep.tc0[k] as i16;
        }
    }
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *s;
    let lt = |x: i16, y: i16| -((x < y) as i16);
    let sel = |m: i16, a: i16, b: i16| (a & m) | (b & !m);
    let mut on = [0i16; N];
    let mut any = 0i16;
    for i in 0..N {
        on[i] = -(bs_on[i] as i16)
            & lt((p0[i] - q0[i]).abs(), alpha)
            & lt((p1[i] - p0[i]).abs(), beta)
            & lt((q1[i] - q0[i]).abs(), beta);
        any |= on[i];
    }
    if any == 0 {
        return;
    }
    let strong_edge = ep.bs[0] >= 4;
    if chroma {
        for i in 0..N {
            let (np0, nq0) = if strong_edge {
                (
                    (2 * p1[i] + p0[i] + q1[i] + 2) >> 2,
                    (2 * q1[i] + q0[i] + p1[i] + 2) >> 2,
                )
            } else {
                let tc = tc0[i] + 1;
                let delta = ((((q0[i] - p0[i]) << 2) + (p1[i] - q1[i]) + 4) >> 3)
                    .max(-tc)
                    .min(tc);
                ((p0[i] + delta).clamp(0, 255), (q0[i] - delta).clamp(0, 255))
            };
            s[3][i] = sel(on[i], np0, p0[i]);
            s[4][i] = sel(on[i], nq0, q0[i]);
        }
        return;
    }
    if strong_edge {
        let lim = (alpha >> 2) + 2;
        for i in 0..N {
            let (a, b, c, d, e, f, g, h) = (p3[i], p2[i], p1[i], p0[i], q0[i], q1[i], q2[i], q3[i]);
            let strong = lt((d - e).abs(), lim);
            let sp = on[i] & lt((b - d).abs(), beta) & strong;
            let sq = on[i] & lt((g - e).abs(), beta) & strong;
            let wp0 = (2 * c + d + f + 2) >> 2;
            let wq0 = (2 * f + e + c + 2) >> 2;
            let sp0 = (b + 2 * c + 2 * d + 2 * e + f + 4) >> 3;
            let sq0 = (c + 2 * d + 2 * e + 2 * f + g + 4) >> 3;
            s[3][i] = sel(sp, sp0, sel(on[i], wp0, d));
            s[2][i] = sel(sp, (b + c + d + e + 2) >> 2, c);
            s[1][i] = sel(sp, (2 * a + 3 * b + c + d + e + 4) >> 3, b);
            s[4][i] = sel(sq, sq0, sel(on[i], wq0, e));
            s[5][i] = sel(sq, (d + e + f + g + 2) >> 2, f);
            s[6][i] = sel(sq, (2 * h + 3 * g + f + e + d + 4) >> 3, g);
        }
    } else {
        for i in 0..N {
            let ap = lt((p2[i] - p0[i]).abs(), beta);
            let aq = lt((q2[i] - q0[i]).abs(), beta);
            let t0 = tc0[i];
            let tc = t0 - ap - aq;
            let delta = ((((q0[i] - p0[i]) << 2) + (p1[i] - q1[i]) + 4) >> 3)
                .max(-tc)
                .min(tc);
            let avg = (p0[i] + q0[i] + 1) >> 1;
            let np1 = p1[i] + ((p2[i] + avg - (p1[i] << 1)) >> 1).max(-t0).min(t0);
            let nq1 = q1[i] + ((q2[i] + avg - (q1[i] << 1)) >> 1).max(-t0).min(t0);
            s[3][i] = sel(on[i], (p0[i] + delta).clamp(0, 255), p0[i]);
            s[4][i] = sel(on[i], (q0[i] - delta).clamp(0, 255), q0[i]);
            s[2][i] = sel(on[i] & ap, np1, p1[i]);
            s[5][i] = sel(on[i] & aq, nq1, q1[i]);
        }
    }
}

#[inline(always)]
fn filter_vertical<const N: usize>(
    pix: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    ep: &EdgeParams,
    chroma: bool,
) {
    let mut w = [0u64; N];
    for (i, w) in w.iter_mut().enumerate() {
        let o = (y + i) * stride + x - 4;
        let Some(&b) = pix.get(o..).and_then(|p| p.first_chunk::<8>()) else {
            return;
        };
        *w = u64::from_le_bytes(b);
    }
    let mut s = [[0i16; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        for i in 0..N {
            row[i] = ((w[i] >> (8 * k)) & 0xff) as i16;
        }
    }
    filter_lanes(&mut s, ep, chroma);
    for (i, w) in w.iter_mut().enumerate() {
        *w = (0..8).fold(0u64, |acc, k| acc | ((s[k][i] as u8 as u64) << (8 * k)));
    }
    for (i, w) in w.iter().enumerate() {
        let o = (y + i) * stride + x - 4;
        pix[o..o + 8].copy_from_slice(&w.to_le_bytes());
    }
}

#[inline(always)]
fn filter_horizontal<const N: usize>(
    pix: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    ep: &EdgeParams,
    chroma: bool,
) {
    let base = (y - 4) * stride + x;
    let mut s = [[0i16; N]; 8];
    for (k, row) in s.iter_mut().enumerate() {
        let Some(src) = pix
            .get(base + k * stride..)
            .and_then(|p| p.first_chunk::<N>())
        else {
            return;
        };
        for i in 0..N {
            row[i] = src[i] as i16;
        }
    }
    filter_lanes(&mut s, ep, chroma);
    let (k0, k1) = if chroma { (3, 5) } else { (1, 7) };
    for (k, row) in s.iter().enumerate().take(k1).skip(k0) {
        let Some(dst) = pix
            .get_mut(base + k * stride..)
            .and_then(|p| p.first_chunk_mut::<N>())
        else {
            return;
        };
        for i in 0..N {
            dst[i] = row[i] as u8;
        }
    }
}

#[allow(clippy::needless_range_loop)]
pub fn deblock_mb(pic: &mut PicState, addr: usize, mb_w: usize) {
    let q = &pic.mbs[addr];
    if q.slice_num == u32::MAX {
        return;
    }
    let sq = &pic.slices[q.slice_num as usize];
    if sq.disable_deblocking_filter_idc == 1 {
        return;
    }
    let (mx, my) = (addr % mb_w, addr / mb_w);
    let usable = |n: Option<usize>| -> Option<usize> {
        let n = n?;
        let st = &pic.mbs[n];
        if st.slice_num == u32::MAX
            || (sq.disable_deblocking_filter_idc == 2 && st.slice_num != q.slice_num)
        {
            return None;
        }
        Some(n)
    };
    let left = usable(if mx > 0 { Some(addr - 1) } else { None });
    let top = usable(if my > 0 { Some(addr - mb_w) } else { None });
    let alpha_off = sq.alpha_offset;
    let beta_off = sq.beta_offset;
    let t8 = q.transform_8x8;
    let mut bs = [[[0u8; 4]; 4]; 2];
    if q.kind.is_intra() {
        for dir in 0..2 {
            if (if dir == 0 { left } else { top }).is_some() {
                bs[dir][0] = [4; 4];
            }
            for e in 1..4 {
                if !(t8 && e % 2 == 1) {
                    bs[dir][e] = [3; 4];
                }
            }
        }
    } else {
        let qids = ref_ids8(q, sq);
        let q_uniform = (0..2).all(|l| {
            let r = q.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && q.mv[l].iter().all(|m| *m == q.mv[l][0])
        });
        let qm: [Motion; 16] = std::array::from_fn(|r| block_motion(q, &qids, r));
        for dir in 0..2 {
            let neighbor = if dir == 0 { left } else { top };
            if let Some(n) = neighbor {
                let p = &pic.mbs[n];
                if p.kind.is_intra() {
                    bs[dir][0] = [4; 4];
                } else {
                    let pids = ref_ids8(p, &pic.slices[p.slice_num as usize]);
                    for k in 0..4 {
                        let (rq, rp) = if dir == 0 {
                            (k * 4, k * 4 + 3)
                        } else {
                            (k, 12 + k)
                        };
                        bs[dir][0][k] = if (p.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0
                        {
                            2
                        } else {
                            motion_bs(&block_motion(p, &pids, rp), &qm[rq])
                        };
                    }
                }
            }
            if q_uniform && q.nz_mask == 0 {
                continue;
            }
            for e in 1..4 {
                if t8 && e % 2 == 1 {
                    continue;
                }
                for k in 0..4 {
                    let (rq, rp) = if dir == 0 {
                        (k * 4 + e, k * 4 + e - 1)
                    } else {
                        (e * 4 + k, e * 4 + k - 4)
                    };
                    bs[dir][e][k] = if (q.nz_mask >> rp) & 1 != 0 || (q.nz_mask >> rq) & 1 != 0 {
                        2
                    } else {
                        motion_bs(&qm[rp], &qm[rq])
                    };
                }
            }
        }
    }
    let qp_of = |st: &MbState| {
        if st.kind == MbKind::IPcm {
            0
        } else {
            st.qp as i32
        }
    };
    let q_qp = qp_of(q);
    let q_qpc = q.qpc;
    let p_qp = [
        left.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc)),
        top.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc)),
    ];
    let params = |qpp: i32, qpq: i32, b: [u8; 4]| {
        let qpav = (qpp + qpq + 1) >> 1;
        let index_a = (qpav + alpha_off).clamp(0, 51) as usize;
        let index_b = (qpav + beta_off).clamp(0, 51) as usize;
        let tc = |b: u8| {
            if (1..4).contains(&b) {
                TC0[index_a][b as usize - 1] as i32
            } else {
                0
            }
        };
        EdgeParams {
            bs: b,
            tc0: [tc(b[0]), tc(b[1]), tc(b[2]), tc(b[3])],
            alpha: ALPHA[index_a] as i32,
            beta: BETA[index_b] as i32,
        }
    };
    let width = pic.planes.width;
    let cwidth = pic.planes.cwidth;
    for dir in 0..2 {
        for e in 0..4 {
            if bs[dir][e] == [0; 4] {
                continue;
            }
            let qpp = if e == 0 {
                p_qp[dir].map(|p| p.0).unwrap_or(q_qp)
            } else {
                q_qp
            };
            let ep = params(qpp, q_qp, bs[dir][e]);
            if ep.alpha == 0 || ep.beta == 0 {
                continue;
            }
            if dir == 0 {
                let (p, yy) = pic.planes.y.near(width, my * 16);
                filter_vertical::<16>(p, width, mx * 16 + e * 4, yy, &ep, false);
            } else {
                let (p, yy) = pic.planes.y.near(width, my * 16 + e * 4);
                filter_horizontal::<16>(p, width, mx * 16, yy, &ep, false);
            }
        }
    }
    for c in 0..2 {
        for dir in 0..2 {
            for ce in 0..2 {
                let e = ce * 2;
                if bs[dir][e] == [0; 4] {
                    continue;
                }
                let qpp = if e == 0 {
                    p_qp[dir].map(|p| p.1[c]).unwrap_or(q_qpc[c])
                } else {
                    q_qpc[c]
                } as i32;
                let ep = params(qpp, q_qpc[c] as i32, bs[dir][e]);
                if ep.alpha == 0 || ep.beta == 0 {
                    continue;
                }
                let plane = if c == 0 {
                    &mut pic.planes.cb
                } else {
                    &mut pic.planes.cr
                };
                if dir == 0 {
                    let (p, yy) = plane.near(cwidth, my * 8);
                    filter_vertical::<8>(p, cwidth, mx * 8 + ce * 4, yy, &ep, true);
                } else {
                    let (p, yy) = plane.near(cwidth, my * 8 + ce * 4);
                    filter_horizontal::<8>(p, cwidth, mx * 8, yy, &ep, true);
                }
            }
        }
    }
}
