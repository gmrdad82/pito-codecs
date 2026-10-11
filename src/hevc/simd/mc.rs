use crate::hevc::pel::Pel;

const WIN: usize = 71;

pub(crate) fn interp<const T: usize, P: Pel>(
    lines: &[&[P]],
    tmp: &mut [i16],
    (fx, fy): (usize, usize),
    (hf, vf): (&[i8; T], &[i8; T]),
    (shift1, shift3): (u32, u32),
    w: usize,
    h: usize,
    out: &mut [i16],
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on()
        && w.is_multiple_of(2)
        && (2..=64).contains(&w)
        && h <= 64
        && lines.len() >= h + T - 1
        && out.len() >= w * h
        && tmp.len() >= (h + T - 1) * WIN
        && lines[..h + T - 1].iter().all(|l| l.len() >= w + T - 1)
    {
        unsafe { x86::interp::<T, P>(lines, tmp, fx, fy, hf, vf, shift1, shift3, w, h, out) };
        return true;
    }
    let _ = (lines, tmp, fx, fy, hf, vf, shift1, shift3, w, h, out);
    false
}

fn fits<P>(p: &[i16], w: usize, h: usize, dst: &[P], ds: usize) -> bool {
    w.is_multiple_of(2)
        && w >= 2
        && h >= 1
        && p.len() >= w * h
        && ds >= w
        && dst.len() >= (h - 1) * ds + w
}

pub(crate) fn put_uni<P: Pel>(
    p: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    dst: &mut [P],
    ds: usize,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on() && (8..=12).contains(&bd) && fits(p, w, h, dst, ds) {
        unsafe { x86::put_uni(p, w, h, bd, dst, ds) };
        return true;
    }
    let _ = (p, w, h, bd, dst, ds);
    false
}

pub(crate) fn put_bi<P: Pel>(
    p0: &[i16],
    p1: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    dst: &mut [P],
    ds: usize,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on() && (8..=12).contains(&bd) && fits(p0, w, h, dst, ds) && p1.len() >= w * h {
        unsafe { x86::put_bi(p0, p1, w, h, bd, dst, ds) };
        return true;
    }
    let _ = (p0, p1, w, h, bd, dst, ds);
    false
}

pub(crate) fn put_weighted_uni<P: Pel>(
    p: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    log2wd: u32,
    wt: i32,
    o: i32,
    dst: &mut [P],
    ds: usize,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on()
        && (8..=12).contains(&bd)
        && log2wd <= 14
        && (-32768..=32767).contains(&wt)
        && fits(p, w, h, dst, ds)
    {
        unsafe { x86::put_weighted_uni(p, w, h, bd, log2wd, wt, o, dst, ds) };
        return true;
    }
    let _ = (p, w, h, bd, log2wd, wt, o, dst, ds);
    false
}

pub(crate) fn put_weighted_bi<P: Pel>(
    p0: &[i16],
    p1: &[i16],
    w: usize,
    h: usize,
    bd: u32,
    log2wd: u32,
    (w0, w1): (i32, i32),
    (o0, o1): (i32, i32),
    dst: &mut [P],
    ds: usize,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on()
        && (8..=12).contains(&bd)
        && log2wd <= 14
        && (-32768..=32767).contains(&w0)
        && (-32768..=32767).contains(&w1)
        && fits(p0, w, h, dst, ds)
        && p1.len() >= w * h
    {
        unsafe { x86::put_weighted_bi(p0, p1, w, h, bd, log2wd, w0, w1, o0, o1, dst, ds) };
        return true;
    }
    let _ = (p0, p1, w, h, bd, log2wd, w0, w1, o0, o1, dst, ds);
    false
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::super::x86::{chunks, gather, pack_trunc, scatter_i16, scatter_pel};
    use super::WIN;
    use crate::hevc::pel::Pel;
    use std::arch::x86_64::*;

    macro_rules! by_width {
        ($n:expr, $f:ident :: <$($g:ty),*> ($($a:expr),*)) => {
            match $n {
                16 => $f::<$($g,)* 16>($($a),*),
                8 => $f::<$($g,)* 8>($($a),*),
                4 => $f::<$($g,)* 4>($($a),*),
                _ => $f::<$($g,)* 2>($($a),*),
            }
        };
    }

    const fn pack(a: i32, b: i32) -> i32 {
        ((a as u16 as u32) | ((b as u16 as u32) << 16)) as i32
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn filt<P: Pel, const T: usize, const N: usize>(
        rows: usize,
        src: &impl Fn(usize, usize) -> *const P,
        c: &[i8; T],
        shift: u32,
        dst: &impl Fn(usize) -> *mut i16,
    ) {
        unsafe {
            let per = 16 / N;
            let narrow = P::BYTES == 1 && shift == 0;
            let sh = _mm_cvtsi32_si128(shift as i32);
            let mut k16 = [_mm256_setzero_si256(); T];
            let mut k32 = [_mm256_setzero_si256(); T];
            for k in 0..T {
                k16[k] = _mm256_set1_epi16(c[k] as i16);
                if k % 2 == 0 {
                    k32[k] = _mm256_set1_epi32(pack(c[k] as i32, c[k + 1] as i32));
                }
            }
            for g in (0..rows).step_by(per) {
                let last = rows - 1;
                let v = |k: usize| gather::<P, N>(|i| src((g + i).min(last), k));
                let res = if narrow {
                    let mut acc = _mm256_setzero_si256();
                    for k in 0..T {
                        if c[k] != 0 {
                            acc = _mm256_add_epi16(acc, _mm256_mullo_epi16(v(k), k16[k]));
                        }
                    }
                    acc
                } else {
                    let mut lo = _mm256_setzero_si256();
                    let mut hi = _mm256_setzero_si256();
                    for k in (0..T).step_by(2) {
                        let (a, b) = (v(k), v(k + 1));
                        lo = _mm256_add_epi32(
                            lo,
                            _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), k32[k]),
                        );
                        hi = _mm256_add_epi32(
                            hi,
                            _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), k32[k]),
                        );
                    }
                    pack_trunc(_mm256_sra_epi32(lo, sh), _mm256_sra_epi32(hi, sh))
                };
                scatter_i16::<N>(res, (rows - g).min(per), |i| dst(g + i));
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn copy<P: Pel, const N: usize>(
        rows: usize,
        src: &impl Fn(usize) -> *const P,
        shift: u32,
        dst: &impl Fn(usize) -> *mut i16,
    ) {
        unsafe {
            let per = 16 / N;
            let sh = _mm_cvtsi32_si128(shift as i32);
            for g in (0..rows).step_by(per) {
                let last = rows - 1;
                let v = gather::<P, N>(|i| src((g + i).min(last)));
                scatter_i16::<N>(_mm256_sll_epi16(v, sh), (rows - g).min(per), |i| dst(g + i));
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn interp<const T: usize, P: Pel>(
        lines: &[&[P]],
        tmp: &mut [i16],
        fx: usize,
        fy: usize,
        hf: &[i8; T],
        vf: &[i8; T],
        shift1: u32,
        shift3: u32,
        w: usize,
        h: usize,
        out: &mut [i16],
    ) {
        unsafe {
            let m = T / 2 - 1;
            let mut lp = [std::ptr::null::<P>(); WIN];
            for (p, l) in lp.iter_mut().zip(&lines[..h + T - 1]) {
                *p = l.as_ptr();
            }
            let o = out.as_mut_ptr();
            let t = tmp.as_mut_ptr();
            if fx == 0 && fy == 0 {
                chunks(w, |j, n| {
                    let src = |r: usize| lp[r + m].add(m + j);
                    let dst = |r: usize| o.add(r * w + j);
                    by_width!(n, copy::<P>(h, &src, shift3, &dst));
                });
            } else if fy == 0 {
                chunks(w, |j, n| {
                    let src = |r: usize, k: usize| lp[r + m].add(j + k);
                    let dst = |r: usize| o.add(r * w + j);
                    by_width!(n, filt::<P, T>(h, &src, hf, shift1, &dst));
                });
            } else if fx == 0 {
                chunks(w, |j, n| {
                    let src = |r: usize, k: usize| lp[r + k].add(m + j);
                    let dst = |r: usize| o.add(r * w + j);
                    by_width!(n, filt::<P, T>(h, &src, vf, shift1, &dst));
                });
            } else {
                chunks(w, |j, n| {
                    let src = |r: usize, k: usize| lp[r].add(j + k);
                    let dst = |r: usize| t.add(r * WIN + j);
                    by_width!(n, filt::<P, T>(h + T - 1, &src, hf, shift1, &dst));
                });
                let tc = t as *const u16;
                chunks(w, |j, n| {
                    let src = |r: usize, k: usize| tc.add((r + k) * WIN + j);
                    let dst = |r: usize| o.add(r * w + j);
                    by_width!(n, filt::<u16, T>(h, &src, vf, 6, &dst));
                });
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn put<P: Pel, const N: usize>(
        h: usize,
        j: usize,
        w: usize,
        p: (*const i16, *const i16),
        max: i32,
        dst: *mut P,
        ds: usize,
        op: &impl Fn(__m256i, __m256i) -> __m256i,
    ) {
        unsafe {
            let per = 16 / N;
            let hi = _mm256_set1_epi16(max as i16);
            let z = _mm256_setzero_si256();
            for g in (0..h).step_by(per) {
                let last = h - 1;
                let a = gather::<u16, N>(|i| p.0.add((g + i).min(last) * w + j) as *const u16);
                let b = gather::<u16, N>(|i| p.1.add((g + i).min(last) * w + j) as *const u16);
                let mut v = op(a, b);
                if P::BYTES == 2 {
                    v = _mm256_min_epi16(_mm256_max_epi16(v, z), hi);
                }
                scatter_pel::<P, N>(v, (h - g).min(per), |i| dst.add((g + i) * ds + j));
            }
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn put_all<P: Pel>(
        w: usize,
        h: usize,
        p: (*const i16, *const i16),
        max: i32,
        dst: *mut P,
        ds: usize,
        op: &impl Fn(__m256i, __m256i) -> __m256i,
    ) {
        chunks(w, |j, n| unsafe {
            by_width!(n, put::<P>(h, j, w, p, max, dst, ds, op));
        });
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn put_uni<P: Pel>(
        p: &[i16],
        w: usize,
        h: usize,
        bd: u32,
        dst: &mut [P],
        ds: usize,
    ) {
        unsafe {
            let k = _mm256_set1_epi16(1 << (15 - (14 - bd)));
            let op = |a: __m256i, _: __m256i| _mm256_mulhrs_epi16(a, k);
            let q = p.as_ptr();
            put_all(w, h, (q, q), (1 << bd) - 1, dst.as_mut_ptr(), ds, &op);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn put_bi<P: Pel>(
        p0: &[i16],
        p1: &[i16],
        w: usize,
        h: usize,
        bd: u32,
        dst: &mut [P],
        ds: usize,
    ) {
        unsafe {
            let k = _mm256_set1_epi16(1 << bd);
            let op = |a: __m256i, b: __m256i| _mm256_mulhrs_epi16(_mm256_adds_epi16(a, b), k);
            let p = (p0.as_ptr(), p1.as_ptr());
            put_all(w, h, p, (1 << bd) - 1, dst.as_mut_ptr(), ds, &op);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn put_weighted_uni<P: Pel>(
        p: &[i16],
        w: usize,
        h: usize,
        bd: u32,
        log2wd: u32,
        wt: i32,
        o: i32,
        dst: &mut [P],
        ds: usize,
    ) {
        unsafe {
            let rnd = if log2wd >= 1 { 1 << (log2wd - 1) } else { 0 };
            let k = _mm256_set1_epi32(pack(wt, rnd));
            let one = _mm256_set1_epi16(1);
            let off = _mm256_set1_epi32(o);
            let sh = _mm_cvtsi32_si128(log2wd as i32);
            let op = |a: __m256i, _: __m256i| {
                let lo = _mm256_madd_epi16(_mm256_unpacklo_epi16(a, one), k);
                let hi = _mm256_madd_epi16(_mm256_unpackhi_epi16(a, one), k);
                let lo = _mm256_add_epi32(_mm256_sra_epi32(lo, sh), off);
                let hi = _mm256_add_epi32(_mm256_sra_epi32(hi, sh), off);
                _mm256_packs_epi32(lo, hi)
            };
            let q = p.as_ptr();
            put_all(w, h, (q, q), (1 << bd) - 1, dst.as_mut_ptr(), ds, &op);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn put_weighted_bi<P: Pel>(
        p0: &[i16],
        p1: &[i16],
        w: usize,
        h: usize,
        bd: u32,
        log2wd: u32,
        w0: i32,
        w1: i32,
        o0: i32,
        o1: i32,
        dst: &mut [P],
        ds: usize,
    ) {
        unsafe {
            let k = _mm256_set1_epi32(pack(w0, w1));
            let off = _mm256_set1_epi32((o0 + o1 + 1) << log2wd);
            let sh = _mm_cvtsi32_si128(log2wd as i32 + 1);
            let op = |a: __m256i, b: __m256i| {
                let lo = _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), k);
                let hi = _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), k);
                let lo = _mm256_sra_epi32(_mm256_add_epi32(lo, off), sh);
                let hi = _mm256_sra_epi32(_mm256_add_epi32(hi, off), sh);
                _mm256_packs_epi32(lo, hi)
            };
            let p = (p0.as_ptr(), p1.as_ptr());
            put_all(w, h, p, (1 << bd) - 1, dst.as_mut_ptr(), ds, &op);
        }
    }
}
