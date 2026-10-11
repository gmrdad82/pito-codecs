use crate::h264::dec::inter;
use crate::h264::dec::inter::Weight;
use crate::h264::dec::simd::mc_avx2;
use std::arch::x86_64::*;

#[inline(always)]
fn bytes8(row: &[u8], off: usize) -> __m128i {
    unsafe {
        match row.get(off..off + 8) {
            Some(s) => _mm_loadl_epi64(s.as_ptr().cast()),
            None => {
                let mut t = [0u8; 8];
                let s = &row[off.min(row.len())..];
                let n = s.len().min(8);
                t[..n].copy_from_slice(&s[..n]);
                _mm_loadl_epi64(t.as_ptr().cast())
            }
        }
    }
}

#[inline(always)]
fn bytes16(row: &[u8], off: usize) -> __m128i {
    unsafe {
        match row.get(off..off + 16) {
            Some(s) => _mm_loadu_si128(s.as_ptr().cast()),
            None => {
                let mut t = [0u8; 16];
                let s = &row[off.min(row.len())..];
                let n = s.len().min(16);
                t[..n].copy_from_slice(&s[..n]);
                _mm_loadu_si128(t.as_ptr().cast())
            }
        }
    }
}

#[inline(always)]
fn widen(v: __m128i) -> __m128i {
    unsafe { _mm_unpacklo_epi8(v, _mm_setzero_si128()) }
}

#[inline(always)]
fn load8(row: &[u8], off: usize) -> __m128i {
    widen(bytes8(row, off))
}

#[inline(always)]
fn store(out: &mut [u8], v: __m128i, n: usize) {
    unsafe {
        match n {
            8 => _mm_storel_epi64(out[..8].as_mut_ptr().cast(), v),
            16 => _mm_storeu_si128(out[..16].as_mut_ptr().cast(), v),
            _ => {
                let mut t = [0u8; 16];
                _mm_storeu_si128(t.as_mut_ptr().cast(), v);
                out[..n].copy_from_slice(&t[..n]);
            }
        }
    }
}

#[inline(always)]
fn copy(out: &mut [u8], src: &[u8], n: usize) {
    match n {
        16 => *out.first_chunk_mut::<16>().unwrap() = *src.first_chunk::<16>().unwrap(),
        8 => *out.first_chunk_mut::<8>().unwrap() = *src.first_chunk::<8>().unwrap(),
        4 => *out.first_chunk_mut::<4>().unwrap() = *src.first_chunk::<4>().unwrap(),
        2 => *out.first_chunk_mut::<2>().unwrap() = *src.first_chunk::<2>().unwrap(),
        _ => out[..n].copy_from_slice(&src[..n]),
    }
}

#[inline(always)]
fn tap(a: __m128i, b: __m128i, c: __m128i, d: __m128i, e: __m128i, f: __m128i) -> __m128i {
    unsafe {
        let cd = _mm_add_epi16(c, d);
        let be = _mm_add_epi16(b, e);
        let af = _mm_add_epi16(a, f);
        let cd20 = _mm_add_epi16(_mm_slli_epi16(cd, 4), _mm_slli_epi16(cd, 2));
        let be5 = _mm_add_epi16(_mm_slli_epi16(be, 2), be);
        _mm_sub_epi16(_mm_add_epi16(af, cd20), be5)
    }
}

#[inline(always)]
fn htap(row: &[u8], c: usize) -> __m128i {
    unsafe {
        let v = bytes16(row, c);
        tap(
            widen(v),
            widen(_mm_srli_si128(v, 1)),
            widen(_mm_srli_si128(v, 2)),
            widen(_mm_srli_si128(v, 3)),
            widen(_mm_srli_si128(v, 4)),
            widen(_mm_srli_si128(v, 5)),
        )
    }
}

#[inline(always)]
fn vtap(src: &[u8], ss: usize, r: usize, c: usize) -> __m128i {
    tap(
        load8(&src[r * ss..], c),
        load8(&src[(r + 1) * ss..], c),
        load8(&src[(r + 2) * ss..], c),
        load8(&src[(r + 3) * ss..], c),
        load8(&src[(r + 4) * ss..], c),
        load8(&src[(r + 5) * ss..], c),
    )
}

#[inline(always)]
fn round5(v: __m128i) -> __m128i {
    unsafe {
        let r = _mm_srai_epi16(_mm_add_epi16(v, _mm_set1_epi16(16)), 5);
        _mm_packus_epi16(r, r)
    }
}

#[inline(always)]
fn tap32(h: &[__m128i]) -> __m128i {
    unsafe {
        let k0 = _mm_set1_epi32(((-5i32) << 16) | 1);
        let k1 = _mm_set1_epi32((20 << 16) | 20);
        let k2 = _mm_set1_epi32((1 << 16) | 0xfffb);
        let sum = |u: fn(__m128i, __m128i) -> __m128i| {
            let s = _mm_add_epi32(
                _mm_add_epi32(
                    _mm_madd_epi16(u(h[0], h[1]), k0),
                    _mm_madd_epi16(u(h[2], h[3]), k1),
                ),
                _mm_madd_epi16(u(h[4], h[5]), k2),
            );
            _mm_srai_epi32(_mm_add_epi32(s, _mm_set1_epi32(512)), 10)
        };
        let lo = sum(|a, b| _mm_unpacklo_epi16(a, b));
        let hi = sum(|a, b| _mm_unpackhi_epi16(a, b));
        let r = _mm_packs_epi32(lo, hi);
        _mm_packus_epi16(r, r)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mc_luma_sse2(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    unsafe {
        let row = |r: usize| &src[r * ss..];
        if (fx, fy) == (0, 0) {
            for r in 0..bh {
                copy(&mut out[r * os..], &row(r + 2)[2..], bw);
            }
            return;
        }
        for c0 in (0..bw).step_by(8) {
            let n = (bw - c0).min(8);
            match (fx, fy) {
                (_, 0) => {
                    for r in 0..bh {
                        let rw = row(r + 2);
                        let mut p = round5(htap(rw, c0));
                        if fx != 2 {
                            p = _mm_avg_epu8(p, bytes8(rw, c0 + fx as usize / 2 + 2));
                        }
                        store(&mut out[r * os + c0..], p, n);
                    }
                }
                (0, _) => {
                    for r in 0..bh {
                        let mut p = round5(vtap(src, ss, r, c0 + 2));
                        if fy != 2 {
                            p = _mm_avg_epu8(p, bytes8(row(r + fy as usize / 2 + 2), c0 + 2));
                        }
                        store(&mut out[r * os + c0..], p, n);
                    }
                }
                (2, _) | (_, 2) => {
                    let mut hs = [_mm_setzero_si128(); 21];
                    for (k, h) in hs.iter_mut().enumerate().take(bh + 5) {
                        *h = htap(row(k), c0);
                    }
                    for r in 0..bh {
                        let j = tap32(&hs[r..r + 6]);
                        let p = match (fx, fy) {
                            (2, 2) => j,
                            (2, _) => _mm_avg_epu8(round5(hs[r + fy as usize / 2 + 2]), j),
                            _ => {
                                _mm_avg_epu8(round5(vtap(src, ss, r, c0 + fx as usize / 2 + 2)), j)
                            }
                        };
                        store(&mut out[r * os + c0..], p, n);
                    }
                }
                _ => {
                    let hoff = if fy == 1 { 2 } else { 3 };
                    let voff = if fx == 1 { 2 } else { 3 };
                    for r in 0..bh {
                        let h = round5(htap(row(r + hoff), c0));
                        let v = round5(vtap(src, ss, r, c0 + voff));
                        store(&mut out[r * os + c0..], _mm_avg_epu8(h, v), n);
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mc_chroma_sse2(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    unsafe {
        if fx == 0 && fy == 0 {
            for r in 0..bh {
                copy(&mut out[r * os..], &src[r * ss..], bw);
            }
            return;
        }
        let (fx, fy) = (fx as i16, fy as i16);
        let w00 = _mm_set1_epi16((8 - fx) * (8 - fy));
        let w10 = _mm_set1_epi16(fx * (8 - fy));
        let w01 = _mm_set1_epi16((8 - fx) * fy);
        let w11 = _mm_set1_epi16(fx * fy);
        let mut top = bytes16(src, 0);
        for r in 0..bh {
            let bot = bytes16(&src[(r + 1) * ss..], 0);
            let s = _mm_add_epi16(
                _mm_add_epi16(
                    _mm_mullo_epi16(widen(top), w00),
                    _mm_mullo_epi16(widen(_mm_srli_si128(top, 1)), w10),
                ),
                _mm_add_epi16(
                    _mm_mullo_epi16(widen(bot), w01),
                    _mm_mullo_epi16(widen(_mm_srli_si128(bot, 1)), w11),
                ),
            );
            let v = _mm_srli_epi16(_mm_add_epi16(s, _mm_set1_epi16(32)), 6);
            store(&mut out[r * os..], _mm_packus_epi16(v, v), bw);
            top = bot;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn weighted_store_sse2(
    dst: &mut [u8],
    ds: usize,
    p0: Option<&[u8]>,
    p1: Option<&[u8]>,
    ps: usize,
    bw: usize,
    bh: usize,
    w: Weight,
) {
    unsafe {
        match (p0, p1, w) {
            (Some(a), Some(b), Weight::Default) => {
                for r in 0..bh {
                    let (ra, rb) = (&a[r * ps..], &b[r * ps..]);
                    let v = if bw > 8 {
                        _mm_avg_epu8(bytes16(ra, 0), bytes16(rb, 0))
                    } else {
                        _mm_avg_epu8(bytes8(ra, 0), bytes8(rb, 0))
                    };
                    store(&mut dst[r * ds..], v, bw);
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
                let k = _mm_set1_epi32((w1 << 16) | (w0 & 0xffff));
                let round = _mm_set1_epi32(1 << log_wd);
                let off = _mm_set1_epi32((o0 + o1 + 1) >> 1);
                let sh = _mm_cvtsi32_si128(log_wd + 1);
                for r in 0..bh {
                    for c0 in (0..bw).step_by(8) {
                        let x = load8(&a[r * ps..], c0);
                        let y = load8(&b[r * ps..], c0);
                        let f = |m: __m128i| {
                            _mm_add_epi32(
                                _mm_sra_epi32(_mm_add_epi32(_mm_madd_epi16(m, k), round), sh),
                                off,
                            )
                        };
                        let v = _mm_packs_epi32(
                            f(_mm_unpacklo_epi16(x, y)),
                            f(_mm_unpackhi_epi16(x, y)),
                        );
                        store(
                            &mut dst[r * ds + c0..],
                            _mm_packus_epi16(v, v),
                            (bw - c0).min(8),
                        );
                    }
                }
            }
            (Some(a), None, w) | (None, Some(a), w) => {
                let (wt, o, log_wd) = match (w, p0.is_some()) {
                    (Weight::Default, _) => {
                        for r in 0..bh {
                            copy(&mut dst[r * ds..], &a[r * ps..], bw);
                        }
                        return;
                    }
                    (Weight::Weighted { w0, o0, log_wd, .. }, true) => (w0, o0, log_wd),
                    (Weight::Weighted { w1, o1, log_wd, .. }, false) => (w1, o1, log_wd),
                };
                let k = _mm_set1_epi16(wt as i16);
                let round = _mm_set1_epi16(if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 });
                let off = _mm_set1_epi16(o as i16);
                let sh = _mm_cvtsi32_si128(log_wd);
                for r in 0..bh {
                    for c0 in (0..bw).step_by(8) {
                        let x = load8(&a[r * ps..], c0);
                        let v = _mm_add_epi16(
                            _mm_sra_epi16(_mm_add_epi16(_mm_mullo_epi16(x, k), round), sh),
                            off,
                        );
                        store(
                            &mut dst[r * ds + c0..],
                            _mm_packus_epi16(v, v),
                            (bw - c0).min(8),
                        );
                    }
                }
            }
            (None, None, _) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mc_luma(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    if super::avx2() {
        unsafe { mc_avx2::mc_luma::<false>(src, ss, fx, fy, bw, bh, out, os) }
    } else {
        mc_luma_sse2(src, ss, fx, fy, bw, bh, out, os)
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
pub fn mc_chroma(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    if super::avx2() {
        unsafe { mc_avx2::mc_chroma(src, ss, fx, fy, bw, bh, out, os) }
    } else {
        mc_chroma_sse2(src, ss, fx, fy, bw, bh, out, os)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mc_luma_avg(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    if super::avx2() {
        unsafe { mc_avx2::mc_luma::<true>(src, ss, fx, fy, bw, bh, out, os) }
    } else {
        let mut t = [0u8; 256];
        mc_luma_sse2(src, ss, fx, fy, bw, bh, &mut t, 16);
        inter::avg_into(out, os, &t, 16, bw, bh);
    }
}

#[allow(clippy::too_many_arguments)]
fn chroma2<const AVG: bool>(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    if super::avx2() {
        return unsafe { mc_avx2::mc_chroma2::<AVG>(src, ss, fx, fy, bw, bh, out, os) };
    }
    for (s, o) in src.into_iter().zip(out) {
        if AVG {
            let mut t = [0u8; 64];
            mc_chroma_sse2(s, ss, fx, fy, bw, bh, &mut t, 8);
            inter::avg_into(o, os, &t, 8, bw, bh);
        } else {
            mc_chroma_sse2(s, ss, fx, fy, bw, bh, o, os);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn mc_chroma2(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    chroma2::<false>(src, ss, fx, fy, bw, bh, out, os)
}

#[allow(clippy::too_many_arguments)]
pub fn mc_chroma2_avg(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    chroma2::<true>(src, ss, fx, fy, bw, bh, out, os)
}

#[allow(clippy::too_many_arguments)]
pub fn weighted_store(
    dst: &mut [u8],
    ds: usize,
    p0: Option<&[u8]>,
    p1: Option<&[u8]>,
    ps: usize,
    bw: usize,
    bh: usize,
    w: Weight,
) {
    if super::avx2() {
        unsafe { mc_avx2::weighted_store(dst, ds, p0, p1, ps, bw, bh, w) }
    } else {
        weighted_store_sse2(dst, ds, p0, p1, ps, bw, bh, w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h264::dec::inter;

    fn noise(n: usize, seed: &mut u64) -> Vec<u8> {
        (0..n)
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                (*seed >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn kernels_match_scalar() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..40 {
            let src = noise(40 * 30, &mut seed);
            for (bw, bh) in [(16, 16), (16, 8), (8, 16), (8, 8), (8, 4), (4, 8), (4, 4)] {
                for fx in 0..4 {
                    for fy in 0..4 {
                        let mut b = vec![0u8; 16 * 16];
                        inter::mc_luma_win_scalar(&src, 40, fx, fy, bw, bh, &mut b, 16);
                        let tight = &src[..(bh + 4) * 40 + bw + 5];
                        for (k, f) in [mc_luma, mc_luma_sse2].iter().enumerate() {
                            for s in [&src[..], tight] {
                                let mut a = vec![0u8; 16 * 16];
                                f(s, 40, fx, fy, bw, bh, &mut a, 16);
                                assert_eq!(a, b, "luma {k} {bw}x{bh} {fx},{fy}");
                            }
                        }
                    }
                }
                for fx in 0..4 {
                    for fy in 0..4 {
                        let base = noise(16 * 16, &mut seed);
                        let mut b = base.clone();
                        inter::mc_luma_win_avg_scalar(&src, 40, fx, fy, bw, bh, &mut b, 16);
                        let tight = &src[..(bh + 4) * 40 + bw + 5];
                        for s in [&src[..], tight] {
                            let mut a = base.clone();
                            mc_luma_avg(s, 40, fx, fy, bw, bh, &mut a, 16);
                            assert_eq!(a, b, "luma avg {bw}x{bh} {fx},{fy}");
                        }
                    }
                }
                for fx in 0..8 {
                    for fy in 0..8 {
                        let (cw, ch) = (bw / 2, bh / 2);
                        let other = noise(40 * 30, &mut seed);
                        let base = noise(2 * 64, &mut seed);
                        let (mut b0, mut b1) = (base[..64].to_vec(), base[64..].to_vec());
                        inter::mc_chroma_win2_scalar(
                            [&src, &other],
                            40,
                            fx,
                            fy,
                            cw,
                            ch,
                            [&mut b0, &mut b1],
                            8,
                        );
                        let (mut a0, mut a1) = (base[..64].to_vec(), base[64..].to_vec());
                        mc_chroma2(
                            [&src, &other[..ch * 40 + cw + 1]],
                            40,
                            fx,
                            fy,
                            cw,
                            ch,
                            [&mut a0, &mut a1],
                            8,
                        );
                        assert_eq!((&a0, &a1), (&b0, &b1), "chroma2 {cw}x{ch} {fx},{fy}");
                        let (mut b0, mut b1) = (base[..64].to_vec(), base[64..].to_vec());
                        inter::mc_chroma_win2_avg_scalar(
                            [&src, &other],
                            40,
                            fx,
                            fy,
                            cw,
                            ch,
                            [&mut b0, &mut b1],
                            8,
                        );
                        let (mut a0, mut a1) = (base[..64].to_vec(), base[64..].to_vec());
                        mc_chroma2_avg([&src, &other], 40, fx, fy, cw, ch, [&mut a0, &mut a1], 8);
                        assert_eq!((&a0, &a1), (&b0, &b1), "chroma2 avg {cw}x{ch} {fx},{fy}");
                        let mut b = vec![0u8; 8 * 8];
                        inter::mc_chroma_win_scalar(&src, 40, fx, fy, cw, ch, &mut b, 8);
                        let tight = &src[..ch * 40 + cw + 1];
                        for (k, f) in [mc_chroma, mc_chroma_sse2].iter().enumerate() {
                            for s in [&src[..], tight] {
                                let mut a = vec![0u8; 8 * 8];
                                f(s, 40, fx, fy, cw, ch, &mut a, 8);
                                assert_eq!(a, b, "chroma {k} {cw}x{ch} {fx},{fy}");
                            }
                        }
                    }
                }
                let p0 = noise(16 * 16, &mut seed);
                let p1 = noise(16 * 16, &mut seed);
                let w = noise(5, &mut seed);
                let weights = [
                    Weight::Default,
                    Weight::Weighted {
                        log_wd: (w[0] % 8) as i32,
                        w0: w[1] as i8 as i32,
                        w1: w[2] as i8 as i32,
                        o0: w[3] as i8 as i32,
                        o1: w[4] as i8 as i32,
                    },
                    Weight::Weighted {
                        log_wd: 5,
                        w0: 128,
                        w1: -64,
                        o0: 0,
                        o1: 0,
                    },
                ];
                for wt in weights {
                    for (u0, u1) in [(true, true), (true, false), (false, true)] {
                        let mut b = vec![0u8; 16 * 16];
                        let q0 = u0.then_some(&p0[..]);
                        let q1 = u1.then_some(&p1[..]);
                        inter::weighted_store_scalar(&mut b, 16, q0, q1, 16, bw, bh, wt);
                        for (k, f) in [weighted_store, weighted_store_sse2].iter().enumerate() {
                            let mut a = vec![0u8; 16 * 16];
                            f(&mut a, 16, q0, q1, 16, bw, bh, wt);
                            assert_eq!(a, b, "weighted {k} {bw}x{bh} {wt:?} {u0} {u1}");
                        }
                    }
                }
            }
        }
    }
}
