use crate::h264::dec::picture::{MbKind, MbState};
use crate::h264::dec::slicedec::{PicState, SliceInfo};
use crate::h264::dec::tables::{ALPHA, BETA, TC0};

type Ids = [[u32; 4]; 2];

fn ref_ids8(st: &MbState, sl: &SliceInfo) -> Ids {
    std::array::from_fn(|l| {
        std::array::from_fn(|b8| {
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

#[derive(Clone, Copy)]
struct Blk {
    ids: [u32; 2],
    mv: [[i16; 2]; 2],
}

#[inline(always)]
fn blk(st: &MbState, ids: &Ids, raster: usize) -> Blk {
    let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
    let ids = [ids[0][b8], ids[1][b8]];
    let mv = |l: usize| {
        let m = -((ids[l] != u32::MAX) as i16);
        [st.mv[l][raster][0] & m, st.mv[l][raster][1] & m]
    };
    Blk {
        ids,
        mv: [mv(0), mv(1)],
    }
}

#[inline(always)]
fn near_mv(a: [i16; 2], b: [i16; 2]) -> bool {
    ((a[0] as i32 - b[0] as i32 + 3) as u32) < 7 && ((a[1] as i32 - b[1] as i32 + 3) as u32) < 7
}

#[inline(always)]
fn motion_bs(p: Blk, q: Blk) -> u8 {
    let same = (p.ids[0] == q.ids[0])
        & (p.ids[1] == q.ids[1])
        & near_mv(p.mv[0], q.mv[0])
        & near_mv(p.mv[1], q.mv[1]);
    let cross = (p.ids[0] == q.ids[1])
        & (p.ids[1] == q.ids[0])
        & near_mv(p.mv[0], q.mv[1])
        & near_mv(p.mv[1], q.mv[0]);
    !(same | cross) as u8
}

#[inline(always)]
fn pair_bs(p: &MbState, pids: &Ids, rp: usize, q: &MbState, qids: &Ids, rq: usize) -> u8 {
    let nz = (((p.nz_mask >> rp) | (q.nz_mask >> rq)) & 1) as u8;
    motion_bs(blk(p, pids, rp), blk(q, qids, rq)).max(nz << 1)
}

#[inline(always)]
fn uniform(st: &MbState) -> bool {
    (0..2).all(|l| {
        let r = st.ref_idx[l];
        r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && st.mv[l] == [st.mv[l][0]; 16]
    })
}

struct EdgeParams {
    bs: [u8; 4],
    tc0: [i32; 4],
    alpha: i32,
    beta: i32,
}

impl EdgeParams {
    #[cfg(target_arch = "x86_64")]
    fn lane_tc(&self) -> [i16; 4] {
        std::array::from_fn(|k| {
            if self.bs[k] == 0 {
                -1
            } else {
                self.tc0[k] as i16
            }
        })
    }
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
    let nbs = [
        usable(if mx > 0 { Some(addr - 1) } else { None }),
        usable(if my > 0 { Some(addr - mb_w) } else { None }),
    ];
    let t8 = q.transform_8x8;
    let mut bs = [[[0u8; 4]; 4]; 2];
    if q.kind.is_intra() {
        for dir in 0..2 {
            if nbs[dir].is_some() {
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
        for dir in 0..2 {
            let Some(n) = nbs[dir] else {
                continue;
            };
            let p = &pic.mbs[n];
            if p.kind.is_intra() {
                bs[dir][0] = [4; 4];
                continue;
            }
            let pids = if p.slice_num == q.slice_num {
                ref_ids8(p, sq)
            } else {
                ref_ids8(p, &pic.slices[p.slice_num as usize])
            };
            for k in 0..4 {
                let (rq, rp) = if dir == 0 {
                    (k * 4, k * 4 + 3)
                } else {
                    (k, 12 + k)
                };
                bs[dir][0][k] = pair_bs(p, &pids, rp, q, &qids, rq);
            }
        }
        if q.nz_mask != 0 || !uniform(q) {
            for dir in 0..2 {
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
                        bs[dir][e][k] = pair_bs(q, &qids, rp, q, &qids, rq);
                    }
                }
            }
        }
    }
    if bs == [[[0; 4]; 4]; 2] {
        return;
    }
    let qp_of = |st: &MbState| {
        if st.kind == MbKind::IPcm {
            0
        } else {
            st.qp as i32
        }
    };
    let q_qp = qp_of(q);
    let q_qpc = q.qpc.map(|v| v as i32);
    let p_qp = nbs.map(|n| n.map(|n| (qp_of(&pic.mbs[n]), pic.mbs[n].qpc.map(|v| v as i32))));
    let (alpha_off, beta_off) = (sq.alpha_offset, sq.beta_offset);
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
    #[cfg(target_arch = "x86_64")]
    let fast = crate::h264::dec::simd::avx2();
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
            let (p, yy) = pic.planes.y.near(width, my * 16 + dir * e * 4);
            let x = mx * 16 + (1 - dir) * e * 4;
            #[cfg(target_arch = "x86_64")]
            if fast {
                use crate::h264::dec::simd::deblock as d;
                unsafe {
                    let l = d::Lanes::luma(ep.alpha, ep.beta, ep.lane_tc());
                    if dir == 0 {
                        d::luma_vertical(p, width, x, yy, &l, ep.bs[0] >= 4);
                    } else {
                        d::luma_horizontal(p, width, x, yy, &l, ep.bs[0] >= 4);
                    }
                }
                continue;
            }
            if dir == 0 {
                filter_vertical::<16>(p, width, x, yy, &ep, false);
            } else {
                filter_horizontal::<16>(p, width, x, yy, &ep, false);
            }
        }
    }
    for dir in 0..2 {
        for ce in 0..2 {
            let e = ce * 2;
            if bs[dir][e] == [0; 4] {
                continue;
            }
            let ep: [EdgeParams; 2] = std::array::from_fn(|c| {
                let qpp = if e == 0 {
                    p_qp[dir].map(|p| p.1[c]).unwrap_or(q_qpc[c])
                } else {
                    q_qpc[c]
                };
                params(qpp, q_qpc[c], bs[dir][e])
            });
            let off = |ep: &EdgeParams| ep.alpha == 0 || ep.beta == 0;
            if off(&ep[0]) && off(&ep[1]) {
                continue;
            }
            let y = my * 8 + dir * ce * 4;
            let x = mx * 8 + (1 - dir) * ce * 4;
            let (cb, yy) = pic.planes.cb.near(cwidth, y);
            let (cr, _) = pic.planes.cr.near(cwidth, y);
            #[cfg(target_arch = "x86_64")]
            if fast {
                use crate::h264::dec::simd::deblock as d;
                unsafe {
                    let l = d::Lanes::chroma(
                        [ep[0].alpha, ep[1].alpha],
                        [ep[0].beta, ep[1].beta],
                        [ep[0].lane_tc(), ep[1].lane_tc()],
                    );
                    if dir == 0 {
                        d::chroma_vertical(cb, cr, cwidth, x, yy, &l, ep[0].bs[0] >= 4);
                    } else {
                        d::chroma_horizontal(cb, cr, cwidth, x, yy, &l, ep[0].bs[0] >= 4);
                    }
                }
                continue;
            }
            for (c, plane) in [cb, cr].into_iter().enumerate() {
                if off(&ep[c]) {
                    continue;
                }
                if dir == 0 {
                    filter_vertical::<8>(plane, cwidth, x, yy, &ep[c], true);
                } else {
                    filter_horizontal::<8>(plane, cwidth, x, yy, &ep[c], true);
                }
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::h264::dec::simd::{self, deblock as d};

    struct Rng(u64);

    impl Rng {
        fn next(&mut self, n: u32) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 24) % n as u64) as u32
        }
    }

    fn plane(r: &mut Rng, n: usize) -> Vec<u8> {
        let base = r.next(200) as i32 + 20;
        let amp = r.next(24) as i32 + 1;
        let step = r.next(40) as i32 - 20;
        (0..n * n)
            .map(|i| {
                let s = if (i % n) >= n / 2 || (i / n) >= n / 2 {
                    step
                } else {
                    0
                };
                (base + s + r.next(amp as u32) as i32 - amp / 2).clamp(0, 255) as u8
            })
            .collect()
    }

    fn edge(r: &mut Rng) -> EdgeParams {
        let ia = 16 + r.next(36) as usize;
        let ib = 16 + r.next(36) as usize;
        let bs = if r.next(4) == 0 {
            [4; 4]
        } else {
            std::array::from_fn(|_| r.next(4) as u8)
        };
        EdgeParams {
            bs,
            tc0: bs.map(|b| {
                if (1..4).contains(&b) {
                    TC0[ia][b as usize - 1] as i32
                } else {
                    0
                }
            }),
            alpha: ALPHA[ia] as i32,
            beta: BETA[ib] as i32,
        }
    }

    #[test]
    fn avx2_filters_match_scalar() {
        if !simd::avx2() {
            return;
        }
        let mut r = Rng(0x2545_f491_4f6c_dd1d);
        for _ in 0..4000 {
            let ep = edge(&mut r);
            let strong = ep.bs[0] >= 4;
            let l = unsafe { d::Lanes::luma(ep.alpha, ep.beta, ep.lane_tc()) };
            let src = plane(&mut r, 32);
            let (mut a, mut b) = (src.clone(), src.clone());
            filter_vertical::<16>(&mut a, 32, 16, 8, &ep, false);
            unsafe { d::luma_vertical(&mut b, 32, 16, 8, &l, strong) };
            assert_eq!(a, b, "luma vertical");
            let (mut a, mut b) = (src.clone(), src);
            filter_horizontal::<16>(&mut a, 32, 8, 16, &ep, false);
            unsafe { d::luma_horizontal(&mut b, 32, 8, 16, &l, strong) };
            assert_eq!(a, b, "luma horizontal");

            let mut ep2 = edge(&mut r);
            ep2.bs = ep.bs;
            ep2.tc0 = ep.tc0;
            let l = unsafe {
                d::Lanes::chroma(
                    [ep.alpha, ep2.alpha],
                    [ep.beta, ep2.beta],
                    [ep.lane_tc(), ep2.lane_tc()],
                )
            };
            let (cb, cr) = (plane(&mut r, 16), plane(&mut r, 16));
            let (mut a0, mut a1, mut b0, mut b1) = (cb.clone(), cr.clone(), cb.clone(), cr.clone());
            filter_vertical::<8>(&mut a0, 16, 8, 4, &ep, true);
            filter_vertical::<8>(&mut a1, 16, 8, 4, &ep2, true);
            unsafe { d::chroma_vertical(&mut b0, &mut b1, 16, 8, 4, &l, strong) };
            assert_eq!((&a0, &a1), (&b0, &b1), "chroma vertical");
            let (mut a0, mut a1, mut b0, mut b1) = (cb.clone(), cr.clone(), cb, cr);
            filter_horizontal::<8>(&mut a0, 16, 4, 8, &ep, true);
            filter_horizontal::<8>(&mut a1, 16, 4, 8, &ep2, true);
            unsafe { d::chroma_horizontal(&mut b0, &mut b1, 16, 4, 8, &l, strong) };
            assert_eq!((&a0, &a1), (&b0, &b1), "chroma horizontal");
        }
    }
}
