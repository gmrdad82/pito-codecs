#[cfg(any(test, not(target_arch = "x86_64")))]
const WIN: usize = 16 + 5;

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn tap6(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
    a - 5 * b + 20 * c + 20 * d - 5 * e + f
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn copy_n(dst: &mut [u8], src: &[u8], n: usize) {
    fn fixed<const N: usize>(dst: &mut [u8], src: &[u8]) {
        if let (Some(d), Some(s)) = (dst.first_chunk_mut::<N>(), src.first_chunk::<N>()) {
            *d = *s;
        }
    }
    match n {
        16 => fixed::<16>(dst, src),
        8 => fixed::<8>(dst, src),
        4 => fixed::<4>(dst, src),
        2 => fixed::<2>(dst, src),
        _ => dst[..n].copy_from_slice(&src[..n]),
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn avg(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16 + 1) >> 1) as u8
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn htap_row(row: &[u8], out: &mut [i32], n: usize) {
    let row = &row[..n + 5];
    for (o, w) in out[..n].iter_mut().zip(row.windows(6)) {
        *o = tap6(
            w[0] as i32,
            w[1] as i32,
            w[2] as i32,
            w[3] as i32,
            w[4] as i32,
            w[5] as i32,
        );
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn vtap_row(rows: [&[u8]; 6], out: &mut [i32], n: usize) {
    let [a, b, c, d, e, f] = rows.map(|r| &r[..n]);
    for i in 0..n {
        out[i] = tap6(
            a[i] as i32,
            b[i] as i32,
            c[i] as i32,
            d[i] as i32,
            e[i] as i32,
            f[i] as i32,
        );
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_win_scalar(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    let wrow = |r: usize| &src[r * ss..r * ss + bw + 5];
    let mut t = [0i32; 16];
    match (fx, fy) {
        (0, 0) => {
            for r in 0..bh {
                copy_n(&mut out[r * os..], &wrow(r + 2)[2..], bw);
            }
        }
        (_, 0) => {
            for r in 0..bh {
                let row = wrow(r + 2);
                htap_row(row, &mut t, bw);
                let o = &mut out[r * os..r * os + bw];
                match fx {
                    1 => {
                        for c in 0..bw {
                            o[c] = avg(row[c + 2], clip1((t[c] + 16) >> 5));
                        }
                    }
                    2 => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 16) >> 5);
                        }
                    }
                    _ => {
                        for c in 0..bw {
                            o[c] = avg(row[c + 3], clip1((t[c] + 16) >> 5));
                        }
                    }
                }
            }
        }
        (0, _) => {
            for r in 0..bh {
                let rows = [
                    wrow(r),
                    wrow(r + 1),
                    wrow(r + 2),
                    wrow(r + 3),
                    wrow(r + 4),
                    wrow(r + 5),
                ]
                .map(|x| &x[2..]);
                vtap_row(rows, &mut t, bw);
                let g = if fy == 1 { rows[2] } else { rows[3] };
                let o = &mut out[r * os..r * os + bw];
                if fy == 2 {
                    for c in 0..bw {
                        o[c] = clip1((t[c] + 16) >> 5);
                    }
                } else {
                    for c in 0..bw {
                        o[c] = avg(g[c], clip1((t[c] + 16) >> 5));
                    }
                }
            }
        }
        (2, _) | (_, 2) => {
            let mut b1 = [[0i32; 16]; WIN];
            for (r, bt) in b1.iter_mut().enumerate().take(bh + 5) {
                htap_row(wrow(r), bt, bw);
            }
            let mut v = [0i32; 16];
            for r in 0..bh {
                let o = &mut out[r * os..r * os + bw];
                let (b0, b1r, b2, b3, b4, b5) = (
                    &b1[r],
                    &b1[r + 1],
                    &b1[r + 2],
                    &b1[r + 3],
                    &b1[r + 4],
                    &b1[r + 5],
                );
                for c in 0..bw {
                    t[c] = tap6(b0[c], b1r[c], b2[c], b3[c], b4[c], b5[c]);
                }
                match (fx, fy) {
                    (2, 2) => {
                        for c in 0..bw {
                            o[c] = clip1((t[c] + 512) >> 10);
                        }
                    }
                    (2, 1) | (2, 3) => {
                        let bh_ = if fy == 1 { b2 } else { b3 };
                        for c in 0..bw {
                            o[c] = avg(clip1((bh_[c] + 16) >> 5), clip1((t[c] + 512) >> 10));
                        }
                    }
                    _ => {
                        let off = if fx == 1 { 2 } else { 3 };
                        let rows = [
                            wrow(r),
                            wrow(r + 1),
                            wrow(r + 2),
                            wrow(r + 3),
                            wrow(r + 4),
                            wrow(r + 5),
                        ]
                        .map(|x| &x[off..]);
                        vtap_row(rows, &mut v, bw);
                        for c in 0..bw {
                            o[c] = avg(clip1((v[c] + 16) >> 5), clip1((t[c] + 512) >> 10));
                        }
                    }
                }
            }
        }
        _ => {
            let hoff = if fy == 1 { 2 } else { 3 };
            let voff = if fx == 1 { 2 } else { 3 };
            let mut v = [0i32; 16];
            for r in 0..bh {
                htap_row(wrow(r + hoff), &mut t, bw);
                let rows = [
                    wrow(r),
                    wrow(r + 1),
                    wrow(r + 2),
                    wrow(r + 3),
                    wrow(r + 4),
                    wrow(r + 5),
                ]
                .map(|x| &x[voff..]);
                vtap_row(rows, &mut v, bw);
                let o = &mut out[r * os..r * os + bw];
                for c in 0..bw {
                    o[c] = avg(clip1((t[c] + 16) >> 5), clip1((v[c] + 16) >> 5));
                }
            }
        }
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win_scalar(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    let (fx, fy) = (fx as u16, fy as u16);
    let (w00, w10, w01, w11) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            copy_n(&mut out[r * os..], &src[r * ss..], bw);
        }
        return;
    }
    for r in 0..bh {
        let r0 = &src[r * ss..r * ss + bw + 1];
        let r1 = &src[(r + 1) * ss..(r + 1) * ss + bw + 1];
        let o = &mut out[r * os..r * os + bw];
        for (c, o) in o.iter_mut().enumerate() {
            let v = w00 * r0[c] as u16
                + w10 * r0[c + 1] as u16
                + w01 * r1[c] as u16
                + w11 * r1[c + 1] as u16;
            *o = ((v + 32) >> 6) as u8;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Default,
    Weighted {
        log_wd: i32,
        w0: i32,
        w1: i32,
        o0: i32,
        o1: i32,
    },
}

impl Weight {
    pub fn simplify(self, use0: bool, use1: bool) -> Weight {
        let Weight::Weighted {
            log_wd,
            w0,
            w1,
            o0,
            o1,
        } = self
        else {
            return self;
        };
        let unit = |w: i32, o: i32| w == 1 << log_wd && o == 0;
        let plain = match (use0, use1) {
            (true, true) => unit(w0, o0) && unit(w1, o1),
            (true, false) => unit(w0, o0),
            (false, true) => unit(w1, o1),
            (false, false) => true,
        };
        if plain { Weight::Default } else { self }
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn weighted_store_scalar(
    dst: &mut [u8],
    ds: usize,
    p0: Option<&[u8]>,
    p1: Option<&[u8]>,
    ps: usize,
    bw: usize,
    bh: usize,
    w: Weight,
) {
    match (p0, p1, w) {
        (Some(a), Some(b), Weight::Default) => {
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = ((x as u32 + y as u32 + 1) >> 1) as u8;
                }
            }
        }
        (
            Some(a),
            Some(b),
            Weight::Weighted {
                log_wd,
                w0,
                w1,
                o0,
                o1,
            },
        ) => {
            let round = 1 << log_wd;
            let off = (o0 + o1 + 1) >> 1;
            let sh = log_wd + 1;
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                let rb = &b[r * ps..r * ps + bw];
                for ((d, &x), &y) in d.iter_mut().zip(ra).zip(rb) {
                    *d = clip1(((x as i32 * w0 + y as i32 * w1 + round) >> sh) + off);
                }
            }
        }
        (Some(a), None, w) | (None, Some(a), w) => {
            let (wt, o, log_wd) = match (w, p0.is_some()) {
                (Weight::Default, _) => {
                    for r in 0..bh {
                        copy_n(&mut dst[r * ds..], &a[r * ps..], bw);
                    }
                    return;
                }
                (Weight::Weighted { w0, o0, log_wd, .. }, true) => (w0, o0, log_wd),
                (Weight::Weighted { w1, o1, log_wd, .. }, false) => (w1, o1, log_wd),
            };
            let round = if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 };
            for r in 0..bh {
                let d = &mut dst[r * ds..r * ds + bw];
                let ra = &a[r * ps..r * ps + bw];
                for (d, &x) in d.iter_mut().zip(ra) {
                    *d = clip1(((x as i32 * wt + round) >> log_wd) + o);
                }
            }
        }
        (None, None, _) => {}
    }
}

pub fn avg_into(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, bw: usize, bh: usize) {
    for r in 0..bh {
        let d = &mut dst[r * ds..r * ds + bw];
        for (d, &s) in d.iter_mut().zip(&src[r * ss..r * ss + bw]) {
            *d = ((*d as u16 + s as u16 + 1) >> 1) as u8;
        }
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn mc_luma_win_avg_scalar(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    let mut t = [0u8; 256];
    mc_luma_win_scalar(src, ss, fx, fy, bw, bh, &mut t, 16);
    avg_into(out, os, &t, 16, bw, bh);
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win2_scalar(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    for (s, o) in src.into_iter().zip(out) {
        mc_chroma_win_scalar(s, ss, fx, fy, bw, bh, o, os);
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_win2_avg_scalar(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    for (s, o) in src.into_iter().zip(out) {
        let mut t = [0u8; 64];
        mc_chroma_win_scalar(s, ss, fx, fy, bw, bh, &mut t, 8);
        avg_into(o, os, &t, 8, bw, bh);
    }
}

#[cfg(target_arch = "x86_64")]
pub use crate::h264::dec::simd::{
    mc_chroma2 as mc_chroma_win2, mc_chroma2_avg as mc_chroma_win2_avg, mc_luma as mc_luma_win,
    mc_luma_avg as mc_luma_win_avg, weighted_store,
};

#[cfg(not(target_arch = "x86_64"))]
pub use self::{
    mc_chroma_win2_avg_scalar as mc_chroma_win2_avg, mc_chroma_win2_scalar as mc_chroma_win2,
    mc_luma_win_avg_scalar as mc_luma_win_avg, mc_luma_win_scalar as mc_luma_win,
    weighted_store_scalar as weighted_store,
};
