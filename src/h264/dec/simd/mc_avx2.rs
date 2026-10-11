use crate::h264::dec::inter::Weight;
use std::arch::x86_64::*;

#[derive(Clone, Copy)]
struct Unit {
    a: *const u8,
    d: usize,
    ss: usize,
}

impl Unit {
    #[inline(always)]
    fn at(self, r: usize, c: usize) -> *const u8 {
        unsafe { self.a.add(r * self.ss + c) }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn ld16(u: Unit, r: usize, c: usize) -> __m256i {
    unsafe {
        let p = u.at(r, c);
        _mm256_inserti128_si256(
            _mm256_castsi128_si256(_mm_loadu_si128(p.cast())),
            _mm_loadu_si128(p.add(u.d).cast()),
            1,
        )
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn ld8(u: Unit, r: usize, c: usize) -> __m256i {
    unsafe {
        let p = u.at(r, c);
        _mm256_inserti128_si256(
            _mm256_castsi128_si256(_mm_loadl_epi64(p.cast())),
            _mm_loadl_epi64(p.add(u.d).cast()),
            1,
        )
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn k(a: i8, b: i8) -> __m256i {
    _mm256_set1_epi16(((b as u8 as i16) << 8) | a as u8 as i16)
}

#[inline]
#[target_feature(enable = "avx2")]
fn shuf(o: i8) -> __m256i {
    _mm256_add_epi8(
        _mm256_setr_epi8(
            0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6,
            7, 7, 8,
        ),
        _mm256_set1_epi8(o),
    )
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn htap(u: Unit, r: usize, c: usize) -> __m256i {
    unsafe {
        let v = ld16(u, r, c);
        let a = _mm256_maddubs_epi16(_mm256_shuffle_epi8(v, shuf(0)), k(1, -5));
        let b = _mm256_maddubs_epi16(_mm256_shuffle_epi8(v, shuf(2)), k(20, 20));
        let e = _mm256_maddubs_epi16(_mm256_shuffle_epi8(v, shuf(4)), k(-5, 1));
        _mm256_add_epi16(_mm256_add_epi16(a, b), e)
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn vtap(u: Unit, r: usize, c: usize) -> __m256i {
    unsafe {
        let row = |i: usize| ld8(u, r + i, c);
        let a = _mm256_maddubs_epi16(_mm256_unpacklo_epi8(row(0), row(1)), k(1, -5));
        let b = _mm256_maddubs_epi16(_mm256_unpacklo_epi8(row(2), row(3)), k(20, 20));
        let e = _mm256_maddubs_epi16(_mm256_unpacklo_epi8(row(4), row(5)), k(-5, 1));
        _mm256_add_epi16(_mm256_add_epi16(a, b), e)
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn round5(v: __m256i) -> __m256i {
    let r = _mm256_srai_epi16(_mm256_add_epi16(v, _mm256_set1_epi16(16)), 5);
    _mm256_packus_epi16(r, r)
}

#[inline]
#[target_feature(enable = "avx2")]
fn tap32(h: &[__m256i]) -> __m256i {
    let k0 = _mm256_set1_epi32(((-5i32) << 16) | 1);
    let k1 = _mm256_set1_epi32((20 << 16) | 20);
    let k2 = _mm256_set1_epi32((1 << 16) | 0xfffb);
    let rnd = _mm256_set1_epi32(512);
    let lo = _mm256_add_epi32(
        _mm256_add_epi32(
            _mm256_madd_epi16(_mm256_unpacklo_epi16(h[0], h[1]), k0),
            _mm256_madd_epi16(_mm256_unpacklo_epi16(h[2], h[3]), k1),
        ),
        _mm256_add_epi32(
            _mm256_madd_epi16(_mm256_unpacklo_epi16(h[4], h[5]), k2),
            rnd,
        ),
    );
    let hi = _mm256_add_epi32(
        _mm256_add_epi32(
            _mm256_madd_epi16(_mm256_unpackhi_epi16(h[0], h[1]), k0),
            _mm256_madd_epi16(_mm256_unpackhi_epi16(h[2], h[3]), k1),
        ),
        _mm256_add_epi32(
            _mm256_madd_epi16(_mm256_unpackhi_epi16(h[4], h[5]), k2),
            rnd,
        ),
    );
    let r = _mm256_packs_epi32(_mm256_srai_epi32(lo, 10), _mm256_srai_epi32(hi, 10));
    _mm256_packus_epi16(r, r)
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn get_pair(a: *const u8, b: *const u8, bw: usize) -> __m256i {
    unsafe {
        let one = |p: *const u8| match bw {
            8 => _mm_loadl_epi64(p.cast()),
            4 => _mm_cvtsi32_si128(p.cast::<i32>().read_unaligned()),
            _ => _mm_cvtsi32_si128(p.cast::<u16>().read_unaligned() as i32),
        };
        _mm256_inserti128_si256(_mm256_castsi128_si256(one(a)), one(b), 1)
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn put_pair<const AVG: bool>(a: *mut u8, b: *mut u8, bw: usize, v: __m256i) {
    unsafe {
        let v = if AVG {
            _mm256_avg_epu8(v, get_pair(a, b, bw))
        } else {
            v
        };
        let lo = _mm256_castsi256_si128(v);
        let hi = _mm256_extracti128_si256(v, 1);
        match bw {
            8 => {
                _mm_storel_epi64(a.cast(), lo);
                _mm_storel_epi64(b.cast(), hi);
            }
            4 => {
                a.cast::<i32>().write_unaligned(_mm_cvtsi128_si32(lo));
                b.cast::<i32>().write_unaligned(_mm_cvtsi128_si32(hi));
            }
            _ => {
                a.cast::<i16>()
                    .write_unaligned(_mm_cvtsi128_si32(lo) as i16);
                b.cast::<i16>()
                    .write_unaligned(_mm_cvtsi128_si32(hi) as i16);
            }
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn put<const AVG: bool>(out: *mut u8, os: usize, wide: bool, bw: usize, v: __m256i) {
    unsafe {
        if wide {
            let mut p = _mm256_castsi256_si128(_mm256_permute4x64_epi64(v, 0x08));
            if AVG {
                p = _mm_avg_epu8(p, _mm_loadu_si128(out.cast()));
            }
            _mm_storeu_si128(out.cast(), p);
        } else {
            put_pair::<AVG>(out, out.add(os), bw, v);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub fn mc_luma<const AVG: bool>(
    src: &[u8],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: &mut [u8],
    os: usize,
) {
    let wide = bw == 16;
    let need = (bh + 4) * ss + if wide { 24 } else { 16 };
    if src.len() < need || ss < bw + 5 {
        let mut pad = [0u8; 32 * 21];
        for r in 0..bh + 5 {
            pad[r * 32..r * 32 + bw + 5].copy_from_slice(&src[r * ss..r * ss + bw + 5]);
        }
        return mc_luma::<AVG>(&pad, 32, fx, fy, bw, bh, out, os);
    }
    assert!(bw <= 16 && bh <= 16 && out.len() >= (bh - 1) * os + bw);
    let u = Unit {
        a: src.as_ptr(),
        d: if wide { 8 } else { ss },
        ss,
    };
    let step = if wide { 1 } else { 2 };
    let o = out.as_mut_ptr();
    let (fxu, fyu) = (fx as usize, fy as usize);
    unsafe {
        match (fx, fy) {
            (0, 0) => {
                for r in (0..bh).step_by(step) {
                    put::<AVG>(o.add(r * os), os, wide, bw, ld8(u, r + 2, 2));
                }
            }
            (_, 0) => {
                for r in (0..bh).step_by(step) {
                    let mut p = round5(htap(u, r + 2, 0));
                    if fx != 2 {
                        p = _mm256_avg_epu8(p, ld8(u, r + 2, fxu / 2 + 2));
                    }
                    put::<AVG>(o.add(r * os), os, wide, bw, p);
                }
            }
            (0, _) => {
                for r in (0..bh).step_by(step) {
                    let mut p = round5(vtap(u, r, 2));
                    if fy != 2 {
                        p = _mm256_avg_epu8(p, ld8(u, r + fyu / 2 + 2, 2));
                    }
                    put::<AVG>(o.add(r * os), os, wide, bw, p);
                }
            }
            (2, _) | (_, 2) => {
                let mut hs = [_mm256_setzero_si256(); 21];
                for (m, h) in hs.iter_mut().enumerate().take(bh + 6 - step) {
                    *h = htap(u, m, 0);
                }
                for r in (0..bh).step_by(step) {
                    let j = tap32(&hs[r..r + 6]);
                    let p = match (fx, fy) {
                        (2, 2) => j,
                        (2, _) => _mm256_avg_epu8(round5(hs[r + fyu / 2 + 2]), j),
                        _ => _mm256_avg_epu8(round5(vtap(u, r, fxu / 2 + 2)), j),
                    };
                    put::<AVG>(o.add(r * os), os, wide, bw, p);
                }
            }
            _ => {
                let hoff = if fy == 1 { 2 } else { 3 };
                let voff = if fx == 1 { 2 } else { 3 };
                for r in (0..bh).step_by(step) {
                    let h = round5(htap(u, r + hoff, 0));
                    let v = round5(vtap(u, r, voff));
                    put::<AVG>(o.add(r * os), os, wide, bw, _mm256_avg_epu8(h, v));
                }
            }
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn bilinear(top: __m256i, bot: __m256i, ab: __m256i, cd: __m256i) -> __m256i {
    let m = shuf(0);
    let s = _mm256_add_epi16(
        _mm256_add_epi16(
            _mm256_maddubs_epi16(_mm256_shuffle_epi8(top, m), ab),
            _mm256_set1_epi16(32),
        ),
        _mm256_maddubs_epi16(_mm256_shuffle_epi8(bot, m), cd),
    );
    let v = _mm256_srli_epi16(s, 6);
    _mm256_packus_epi16(v, v)
}

#[inline]
#[target_feature(enable = "avx2")]
fn weights(fx: u32, fy: u32) -> (__m256i, __m256i) {
    let (fx, fy) = (fx as i8, fy as i8);
    (
        k((8 - fx) * (8 - fy), fx * (8 - fy)),
        k((8 - fx) * fy, fx * fy),
    )
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
#[target_feature(enable = "avx2")]
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
    if src.len() < bh * ss + 16 {
        let mut pad = [0u8; 16 * 9];
        for r in 0..bh + 1 {
            pad[r * 16..r * 16 + bw + 1].copy_from_slice(&src[r * ss..r * ss + bw + 1]);
        }
        return mc_chroma(&pad, 16, fx, fy, bw, bh, out, os);
    }
    assert!(bw <= 8 && bh <= 8 && out.len() >= (bh - 1) * os + bw);
    if fx == 0 && fy == 0 {
        for r in 0..bh {
            out[r * os..r * os + bw].copy_from_slice(&src[r * ss..r * ss + bw]);
        }
        return;
    }
    let (ab, cd) = weights(fx, fy);
    let u = Unit {
        a: src.as_ptr(),
        d: ss,
        ss,
    };
    let o = out.as_mut_ptr();
    unsafe {
        for r in (0..bh).step_by(2) {
            let v = bilinear(ld16(u, r, 0), ld16(u, r + 1, 0), ab, cd);
            put::<false>(o.add(r * os), os, false, bw, v);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub fn mc_chroma2<const AVG: bool>(
    src: [&[u8]; 2],
    ss: usize,
    fx: u32,
    fy: u32,
    bw: usize,
    bh: usize,
    out: [&mut [u8]; 2],
    os: usize,
) {
    if src.iter().any(|s| s.len() < bh * ss + 16) {
        let mut pad = [[0u8; 16 * 9]; 2];
        for (p, s) in pad.iter_mut().zip(src) {
            for r in 0..bh + 1 {
                p[r * 16..r * 16 + bw + 1].copy_from_slice(&s[r * ss..r * ss + bw + 1]);
            }
        }
        return mc_chroma2::<AVG>([&pad[0], &pad[1]], 16, fx, fy, bw, bh, out, os);
    }
    assert!(bw <= 8 && bh <= 8 && out.iter().all(|o| o.len() >= (bh - 1) * os + bw));
    let (ab, cd) = weights(fx, fy);
    let (a, b) = (src[0].as_ptr(), src[1].as_ptr());
    let [oa, ob] = out;
    let (oa, ob) = (oa.as_mut_ptr(), ob.as_mut_ptr());
    unsafe {
        let row = |r: usize| {
            _mm256_inserti128_si256(
                _mm256_castsi128_si256(_mm_loadu_si128(a.add(r * ss).cast())),
                _mm_loadu_si128(b.add(r * ss).cast()),
                1,
            )
        };
        if fx == 0 && fy == 0 {
            for r in 0..bh {
                put_pair::<AVG>(oa.add(r * os), ob.add(r * os), bw, row(r));
            }
            return;
        }
        let mut top = row(0);
        for r in 0..bh {
            let bot = row(r + 1);
            put_pair::<AVG>(
                oa.add(r * os),
                ob.add(r * os),
                bw,
                bilinear(top, bot, ab, cd),
            );
            top = bot;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
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
    let wide = bw == 16;
    let fits = |p: &[u8]| p.len() >= (bh - 1) * ps + bw.max(8);
    assert!(bw <= 16 && bh.is_multiple_of(2) && dst.len() >= (bh - 1) * ds + bw);
    if !p0.is_none_or(fits) || !p1.is_none_or(fits) {
        return super::mc::weighted_store_sse2(dst, ds, p0, p1, ps, bw, bh, w);
    }
    let step = if wide { 1 } else { 2 };
    let unit = |p: &[u8]| Unit {
        a: p.as_ptr(),
        d: if wide { 8 } else { ps },
        ss: ps,
    };
    let o = dst.as_mut_ptr();
    unsafe {
        match (p0, p1, w) {
            (Some(a), Some(b), Weight::Default) => {
                let (ua, ub) = (unit(a), unit(b));
                for r in (0..bh).step_by(step) {
                    let v = _mm256_avg_epu8(ld8(ua, r, 0), ld8(ub, r, 0));
                    put::<false>(o.add(r * ds), ds, wide, bw, v);
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
                let (ua, ub) = (unit(a), unit(b));
                let kw = _mm256_set1_epi32((w1 << 16) | (w0 & 0xffff));
                let round = _mm256_set1_epi32(1 << log_wd);
                let off = _mm256_set1_epi32((o0 + o1 + 1) >> 1);
                let sh = _mm_cvtsi32_si128(log_wd + 1);
                let z = _mm256_setzero_si256();
                for r in (0..bh).step_by(step) {
                    let x = _mm256_unpacklo_epi8(ld8(ua, r, 0), z);
                    let y = _mm256_unpacklo_epi8(ld8(ub, r, 0), z);
                    let f = |m: __m256i| {
                        _mm256_add_epi32(
                            _mm256_sra_epi32(_mm256_add_epi32(_mm256_madd_epi16(m, kw), round), sh),
                            off,
                        )
                    };
                    let v = _mm256_packs_epi32(
                        f(_mm256_unpacklo_epi16(x, y)),
                        f(_mm256_unpackhi_epi16(x, y)),
                    );
                    put::<false>(o.add(r * ds), ds, wide, bw, _mm256_packus_epi16(v, v));
                }
            }
            (Some(a), None, w) | (None, Some(a), w) => {
                let (wt, of, log_wd) = match (w, p0.is_some()) {
                    (Weight::Default, _) => {
                        for r in 0..bh {
                            dst[r * ds..r * ds + bw].copy_from_slice(&a[r * ps..r * ps + bw]);
                        }
                        return;
                    }
                    (Weight::Weighted { w0, o0, log_wd, .. }, true) => (w0, o0, log_wd),
                    (Weight::Weighted { w1, o1, log_wd, .. }, false) => (w1, o1, log_wd),
                };
                let ua = unit(a);
                let kw = _mm256_set1_epi16(wt as i16);
                let round = _mm256_set1_epi16(if log_wd >= 1 { 1 << (log_wd - 1) } else { 0 });
                let off = _mm256_set1_epi16(of as i16);
                let sh = _mm_cvtsi32_si128(log_wd);
                let z = _mm256_setzero_si256();
                for r in (0..bh).step_by(step) {
                    let x = _mm256_unpacklo_epi8(ld8(ua, r, 0), z);
                    let v = _mm256_add_epi16(
                        _mm256_sra_epi16(_mm256_add_epi16(_mm256_mullo_epi16(x, kw), round), sh),
                        off,
                    );
                    put::<false>(o.add(r * ds), ds, wide, bw, _mm256_packus_epi16(v, v));
                }
            }
            (None, None, _) => {}
        }
    }
}
