use crate::hevc::pel::Pel;

#[derive(Clone, Copy, Default)]
pub(crate) struct Edge4 {
    pub tc: [i16; 4],
    pub beta: [i16; 4],
    pub nfp: [bool; 4],
    pub nfq: [bool; 4],
}

pub(crate) fn luma_h<P: Pel>(s: &mut [P], o: usize, stride: usize, e: &Edge4, bd: u32) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on() && bd <= 10 && o >= 4 * stride && s.len() >= o + 3 * stride + 16 {
        unsafe { x86::luma_h(s, o, stride, e, bd) };
        return true;
    }
    let _ = (s, o, stride, e, bd);
    false
}

pub(crate) fn luma_v<P: Pel>(s: &mut [P], o: usize, stride: usize, e: &Edge4, bd: u32) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on() && bd <= 10 && o >= 4 && s.len() >= o + 15 * stride + 4 {
        unsafe { x86::luma_v(s, o, stride, e, bd) };
        return true;
    }
    let _ = (s, o, stride, e, bd);
    false
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::super::x86::{load8, load16, scatter_pel};
    use super::Edge4;
    use crate::hevc::pel::Pel;
    use std::arch::x86_64::*;

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn spread(v: [i16; 4]) -> __m256i {
        let x = _mm_set_epi16(0, 0, 0, 0, v[3], v[2], v[1], v[0]);
        let x = _mm_unpacklo_epi16(x, x);
        _mm256_set_m128i(_mm_unpackhi_epi32(x, x), _mm_unpacklo_epi32(x, x))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn mask(v: [bool; 4]) -> __m256i {
        unsafe { spread(v.map(|b| -(b as i16))) }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn clamp(v: __m256i, lo: __m256i, hi: __m256i) -> __m256i {
        _mm256_min_epi16(_mm256_max_epi16(v, lo), hi)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn filter(
        p: [__m256i; 4],
        q: [__m256i; 4],
        e: &Edge4,
        bd: u32,
    ) -> ([__m256i; 3], [__m256i; 3]) {
        unsafe {
            let tc = spread(e.tc);
            let beta = spread(e.beta);
            let nfp = mask(e.nfp);
            let nfq = mask(e.nfq);
            let z = _mm256_setzero_si256();
            let max = _mm256_set1_epi16(((1i32 << bd) - 1) as i16);
            let b0 = _mm256_setr_epi8(
                0, 1, 0, 1, 0, 1, 0, 1, 8, 9, 8, 9, 8, 9, 8, 9, 0, 1, 0, 1, 0, 1, 0, 1, 8, 9, 8, 9,
                8, 9, 8, 9,
            );
            let b3 = _mm256_setr_epi8(
                6, 7, 6, 7, 6, 7, 6, 7, 14, 15, 14, 15, 14, 15, 14, 15, 6, 7, 6, 7, 6, 7, 6, 7, 14,
                15, 14, 15, 14, 15, 14, 15,
            );
            let add = _mm256_add_epi16;
            let sub = _mm256_sub_epi16;
            let abs = _mm256_abs_epi16;
            let gt = _mm256_cmpgt_epi16;
            let and = _mm256_and_si256;
            let sh = |v: __m256i, a: __m256i| _mm256_shuffle_epi8(v, a);
            let [p0, p1, p2, p3] = p;
            let [q0, q1, q2, q3] = q;
            let dp = abs(add(sub(p2, add(p1, p1)), p0));
            let dq = abs(add(sub(q2, add(q1, q1)), q0));
            let (dp0, dp3, dq0, dq3) = (sh(dp, b0), sh(dp, b3), sh(dq, b0), sh(dq, b3));
            let on = gt(beta, add(add(dp0, dq0), add(dp3, dq3)));
            let tc25 = _mm256_srai_epi16::<1>(add(
                add(_mm256_slli_epi16::<2>(tc), tc),
                _mm256_set1_epi16(1),
            ));
            let dpq = add(dp, dq);
            let sam = and(
                gt(_mm256_srai_epi16::<2>(beta), add(dpq, dpq)),
                and(
                    gt(
                        _mm256_srai_epi16::<3>(beta),
                        add(abs(sub(p3, p0)), abs(sub(q0, q3))),
                    ),
                    gt(tc25, abs(sub(p0, q0))),
                ),
            );
            let strong = and(sh(sam, b0), sh(sam, b3));
            let side = _mm256_srai_epi16::<3>(add(beta, _mm256_srai_epi16::<1>(beta)));
            let dep = gt(side, add(dp0, dp3));
            let deq = gt(side, add(dq0, dq3));
            let t2 = add(tc, tc);
            let r = |v: __m256i, k: i16, s: i32| {
                let v = add(v, _mm256_set1_epi16(k));
                if s == 3 {
                    _mm256_srai_epi16::<3>(v)
                } else {
                    _mm256_srai_epi16::<2>(v)
                }
            };
            let lim = |v: __m256i, c: __m256i| clamp(v, sub(c, t2), add(c, t2));
            let p0q0 = add(p0, q0);
            let sp0 = lim(
                r(add(add(p2, add(p1, p1)), add(add(p0q0, p0q0), q1)), 4, 3),
                p0,
            );
            let sp1 = lim(r(add(add(p2, p1), p0q0), 2, 2), p1);
            let sp2 = lim(
                r(
                    add(add(add(p3, p3), add(add(p2, p2), p2)), add(p1, p0q0)),
                    4,
                    3,
                ),
                p2,
            );
            let sq0 = lim(
                r(add(add(p1, add(p0q0, p0q0)), add(add(q1, q1), q2)), 4, 3),
                q0,
            );
            let sq1 = lim(r(add(add(p0q0, q1), q2), 2, 2), q1);
            let sq2 = lim(
                r(
                    add(add(p0q0, q1), add(add(add(q2, q2), q2), add(q3, q3))),
                    4,
                    3,
                ),
                q2,
            );
            let d9 = sub(q0, p0);
            let d3 = sub(q1, p1);
            let delta = _mm256_srai_epi16::<4>(add(
                sub(add(_mm256_slli_epi16::<3>(d9), d9), add(add(d3, d3), d3)),
                _mm256_set1_epi16(8),
            ));
            let tc10 = add(_mm256_slli_epi16::<3>(tc), add(tc, tc));
            let nm = gt(tc10, abs(delta));
            let ntc = sub(z, tc);
            let delta = clamp(delta, ntc, tc);
            let np0 = clamp(add(p0, delta), z, max);
            let nq0 = clamp(sub(q0, delta), z, max);
            let one = _mm256_set1_epi16(1);
            let tch = _mm256_srai_epi16::<1>(tc);
            let ntch = sub(z, tch);
            let avg_p = _mm256_srai_epi16::<1>(add(add(p2, p0), one));
            let avg_q = _mm256_srai_epi16::<1>(add(add(q2, q0), one));
            let dpv = clamp(
                _mm256_srai_epi16::<1>(add(sub(avg_p, p1), delta)),
                ntch,
                tch,
            );
            let dqv = clamp(
                _mm256_srai_epi16::<1>(sub(sub(avg_q, q1), delta)),
                ntch,
                tch,
            );
            let np1 = clamp(add(p1, dpv), z, max);
            let nq1 = clamp(add(q1, dqv), z, max);
            let ap = _mm256_andnot_si256(nfp, on);
            let aq = _mm256_andnot_si256(nfq, on);
            let sel = |a: __m256i, b: __m256i, m: __m256i| _mm256_blendv_epi8(a, b, m);
            let or = _mm256_or_si256;
            let weak = _mm256_andnot_si256(strong, nm);
            let o_p0 = sel(p0, sel(np0, sp0, strong), and(ap, or(strong, nm)));
            let o_p1 = sel(
                p1,
                sel(np1, sp1, strong),
                and(ap, or(strong, and(weak, dep))),
            );
            let o_p2 = sel(p2, sp2, and(ap, strong));
            let o_q0 = sel(q0, sel(nq0, sq0, strong), and(aq, or(strong, nm)));
            let o_q1 = sel(
                q1,
                sel(nq1, sq1, strong),
                and(aq, or(strong, and(weak, deq))),
            );
            let o_q2 = sel(q2, sq2, and(aq, strong));
            ([o_p0, o_p1, o_p2], [o_q0, o_q1, o_q2])
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn luma_h<P: Pel>(s: &mut [P], o: usize, stride: usize, e: &Edge4, bd: u32) {
        unsafe {
            let b = s.as_mut_ptr().add(o);
            let row = |i: isize| b.offset(i * stride as isize);
            let ld = |i: isize| load16::<P>(row(i));
            let p = [ld(-1), ld(-2), ld(-3), ld(-4)];
            let q = [ld(0), ld(1), ld(2), ld(3)];
            let (fp, fq) = filter(p, q, e, bd);
            for k in 0..3 {
                scatter_pel::<P, 16>(fp[k], 1, |_| row(-(k as isize) - 1));
                scatter_pel::<P, 16>(fq[k], 1, |_| row(k as isize));
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn transpose8(a: [__m128i; 8]) -> [__m128i; 8] {
        let b0 = _mm_unpacklo_epi16(a[0], a[1]);
        let b1 = _mm_unpackhi_epi16(a[0], a[1]);
        let b2 = _mm_unpacklo_epi16(a[2], a[3]);
        let b3 = _mm_unpackhi_epi16(a[2], a[3]);
        let b4 = _mm_unpacklo_epi16(a[4], a[5]);
        let b5 = _mm_unpackhi_epi16(a[4], a[5]);
        let b6 = _mm_unpacklo_epi16(a[6], a[7]);
        let b7 = _mm_unpackhi_epi16(a[6], a[7]);
        let c0 = _mm_unpacklo_epi32(b0, b2);
        let c1 = _mm_unpackhi_epi32(b0, b2);
        let c2 = _mm_unpacklo_epi32(b1, b3);
        let c3 = _mm_unpackhi_epi32(b1, b3);
        let c4 = _mm_unpacklo_epi32(b4, b6);
        let c5 = _mm_unpackhi_epi32(b4, b6);
        let c6 = _mm_unpacklo_epi32(b5, b7);
        let c7 = _mm_unpackhi_epi32(b5, b7);
        [
            _mm_unpacklo_epi64(c0, c4),
            _mm_unpackhi_epi64(c0, c4),
            _mm_unpacklo_epi64(c1, c5),
            _mm_unpackhi_epi64(c1, c5),
            _mm_unpacklo_epi64(c2, c6),
            _mm_unpackhi_epi64(c2, c6),
            _mm_unpacklo_epi64(c3, c7),
            _mm_unpackhi_epi64(c3, c7),
        ]
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn luma_v<P: Pel>(s: &mut [P], o: usize, stride: usize, e: &Edge4, bd: u32) {
        unsafe {
            let b = s.as_mut_ptr().add(o - 4);
            let row = |r: usize| b.add(r * stride);
            let lo = transpose8(std::array::from_fn(|r| load8::<P>(row(r))));
            let hi = transpose8(std::array::from_fn(|r| load8::<P>(row(r + 8))));
            let c: [__m256i; 8] = std::array::from_fn(|k| _mm256_set_m128i(hi[k], lo[k]));
            let (fp, fq) = filter([c[3], c[2], c[1], c[0]], [c[4], c[5], c[6], c[7]], e, bd);
            let out = [c[0], fp[2], fp[1], fp[0], fq[0], fq[1], fq[2], c[7]];
            let lo = transpose8(out.map(|v| _mm256_castsi256_si128(v)));
            let hi = transpose8(out.map(|v| _mm256_extracti128_si256::<1>(v)));
            for r in 0..16 {
                let v = if r < 8 { lo[r] } else { hi[r - 8] };
                let d = row(r);
                if P::BYTES == 1 {
                    _mm_storel_epi64(d as *mut __m128i, _mm_packus_epi16(v, v));
                } else {
                    _mm_storeu_si128(d as *mut __m128i, v);
                }
            }
        }
    }
}
