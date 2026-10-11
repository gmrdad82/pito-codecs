use super::{Group, LfFrame};
use crate::vp9::tables::{TX_8X8, TX_16X16};
use std::arch::x86_64::*;

type V = __m256i;

#[inline(always)]
unsafe fn transpose8(a: [V; 8]) -> [V; 8] {
    unsafe {
        let s0 = _mm256_unpacklo_epi16(a[0], a[1]);
        let s1 = _mm256_unpackhi_epi16(a[0], a[1]);
        let s2 = _mm256_unpacklo_epi16(a[2], a[3]);
        let s3 = _mm256_unpackhi_epi16(a[2], a[3]);
        let s4 = _mm256_unpacklo_epi16(a[4], a[5]);
        let s5 = _mm256_unpackhi_epi16(a[4], a[5]);
        let s6 = _mm256_unpacklo_epi16(a[6], a[7]);
        let s7 = _mm256_unpackhi_epi16(a[6], a[7]);
        let t0 = _mm256_unpacklo_epi32(s0, s2);
        let t1 = _mm256_unpackhi_epi32(s0, s2);
        let t2 = _mm256_unpacklo_epi32(s1, s3);
        let t3 = _mm256_unpackhi_epi32(s1, s3);
        let t4 = _mm256_unpacklo_epi32(s4, s6);
        let t5 = _mm256_unpackhi_epi32(s4, s6);
        let t6 = _mm256_unpacklo_epi32(s5, s7);
        let t7 = _mm256_unpackhi_epi32(s5, s7);
        [
            _mm256_unpacklo_epi64(t0, t4),
            _mm256_unpackhi_epi64(t0, t4),
            _mm256_unpacklo_epi64(t1, t5),
            _mm256_unpackhi_epi64(t1, t5),
            _mm256_unpacklo_epi64(t2, t6),
            _mm256_unpackhi_epi64(t2, t6),
            _mm256_unpacklo_epi64(t3, t7),
            _mm256_unpackhi_epi64(t3, t7),
        ]
    }
}

#[inline(always)]
unsafe fn transpose16(m: &[V; 16]) -> [V; 16] {
    unsafe {
        let t = transpose8([m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7]]);
        let u = transpose8([m[8], m[9], m[10], m[11], m[12], m[13], m[14], m[15]]);
        let mut out = [_mm256_setzero_si256(); 16];
        for j in 0..8 {
            out[j] = _mm256_permute2x128_si256(t[j], u[j], 0x20);
            out[j + 8] = _mm256_permute2x128_si256(t[j], u[j], 0x31);
        }
        out
    }
}

#[inline(always)]
unsafe fn halves(a: i16, b: i16) -> V {
    unsafe { _mm256_set_m128i(_mm_set1_epi16(b), _mm_set1_epi16(a)) }
}

#[inline(always)]
unsafe fn ad(a: V, b: V) -> V {
    unsafe { _mm256_abs_epi16(_mm256_sub_epi16(a, b)) }
}

#[inline(always)]
unsafe fn wide<const LOG2: i32>(v: &[V; 16]) -> [V; 16] {
    unsafe {
        let n = (1isize << (LOG2 - 1)) - 1;
        let s = |k: isize| v[(k.clamp(-(n + 1), n) + 8) as usize];
        let mut out = *v;
        let mut sum = _mm256_setzero_si256();
        for j in -n..=n {
            sum = _mm256_add_epi16(sum, s(-n + j));
        }
        let round = _mm256_set1_epi16(1 << (LOG2 - 1));
        for i in -n..n {
            let o = _mm256_add_epi16(_mm256_add_epi16(sum, s(i)), round);
            out[(i + 8) as usize] = _mm256_srli_epi16::<LOG2>(o);
            sum = _mm256_add_epi16(sum, _mm256_sub_epi16(s(i + 1 + n), s(i - n)));
        }
        out
    }
}

#[target_feature(enable = "avx2")]
pub(super) unsafe fn filter16(
    d: &mut [u16],
    pos: usize,
    along: usize,
    across: usize,
    g: [Group; 2],
    f: &LfFrame,
    rows: bool,
) {
    unsafe {
        let r: usize = if g[0].size == TX_16X16 || g[1].size == TX_16X16 {
            8
        } else {
            4
        };
        let lo_edge = pos - r * across;
        let hi_edge = if rows {
            pos + (r - 1) * across + 16
        } else {
            pos + 15 * along + r
        };
        assert!(lo_edge < d.len() && hi_edge <= d.len());
        let base = d.as_mut_ptr().add(pos);
        let shift = f.bit_depth as i32 - 8;
        let lims = g.map(|g| f.limits[g.lvl as usize]);
        let limit = halves((lims[0].0 << shift) as i16, (lims[1].0 << shift) as i16);
        let blimit = halves((lims[0].1 << shift) as i16, (lims[1].1 << shift) as i16);
        let thresh = halves((lims[0].2 << shift) as i16, (lims[1].2 << shift) as i16);
        let mut apply = [0i16; 16];
        for (l, a) in apply.iter_mut().enumerate() {
            *a = -((g[l / 8].apply >> (l % 8) & 1) as i16);
        }
        let apply = _mm256_loadu_si256(apply.as_ptr() as *const V);
        let can_flat = halves(
            -((g[0].size >= TX_8X8) as i16),
            -((g[1].size >= TX_8X8) as i16),
        );
        let can_flat2 = halves(
            -((g[0].size == TX_16X16) as i16),
            -((g[1].size == TX_16X16) as i16),
        );
        let zero = _mm256_setzero_si256();
        let mut v = [zero; 16];
        if rows {
            for k in 8 - r..8 + r {
                v[k] =
                    _mm256_loadu_si256(base.offset((k as isize - 8) * across as isize) as *const V);
            }
        } else if r == 8 {
            let mut m = [zero; 16];
            for (l, m) in m.iter_mut().enumerate() {
                *m = _mm256_loadu_si256(base.add(l * along).sub(8) as *const V);
            }
            v = transpose16(&m);
        } else {
            let mut m = [zero; 8];
            for (l, m) in m.iter_mut().enumerate() {
                let a = _mm_loadu_si128(base.add(l * along).sub(4) as *const __m128i);
                let b = _mm_loadu_si128(base.add((l + 8) * along).sub(4) as *const __m128i);
                *m = _mm256_set_m128i(b, a);
            }
            let t = transpose8(m);
            v[4..12].copy_from_slice(&t);
        }
        let (p3, p2, p1, p0, q0, q1, q2, q3) = (v[4], v[5], v[6], v[7], v[8], v[9], v[10], v[11]);
        let gt = |a: V, b: V| _mm256_cmpgt_epi16(a, b);
        let or = |a: V, b: V| _mm256_or_si256(a, b);
        let d_p1p0 = ad(p1, p0);
        let d_q1q0 = ad(q1, q0);
        let mut exceed = or(gt(ad(p3, p2), limit), gt(ad(p2, p1), limit));
        exceed = or(exceed, gt(d_p1p0, limit));
        exceed = or(exceed, gt(d_q1q0, limit));
        exceed = or(exceed, gt(ad(q2, q1), limit));
        exceed = or(exceed, gt(ad(q3, q2), limit));
        let edge = _mm256_add_epi16(
            _mm256_add_epi16(ad(p0, q0), ad(p0, q0)),
            _mm256_srli_epi16::<1>(ad(p1, q1)),
        );
        exceed = or(exceed, gt(edge, blimit));
        let mask = _mm256_andnot_si256(exceed, apply);
        if _mm256_testz_si256(mask, mask) != 0 {
            return;
        }
        let hev = or(gt(d_p1p0, thresh), gt(d_q1q0, thresh));
        let one = _mm256_set1_epi16(1 << shift);
        let mut far = or(gt(d_p1p0, one), gt(d_q1q0, one));
        far = or(far, gt(ad(p2, p0), one));
        far = or(far, gt(ad(q2, q0), one));
        far = or(far, gt(ad(p3, p0), one));
        far = or(far, gt(ad(q3, q0), one));
        let flat = _mm256_andnot_si256(far, _mm256_and_si256(mask, can_flat));
        let flat2 = if r == 8 {
            let mut far2 = zero;
            for k in 0..4 {
                far2 = or(far2, gt(ad(v[k], p0), one));
                far2 = or(far2, gt(ad(v[12 + k], q0), one));
            }
            _mm256_andnot_si256(far2, _mm256_and_si256(flat, can_flat2))
        } else {
            zero
        };
        let mut out = v;
        let off = _mm256_set1_epi16(0x80 << shift);
        let lo = _mm256_set1_epi16(-(1i16 << (f.bit_depth - 1)));
        let hi = _mm256_set1_epi16((1i16 << (f.bit_depth - 1)) - 1);
        let c = |x: V| _mm256_min_epi16(_mm256_max_epi16(x, lo), hi);
        let ps1 = _mm256_sub_epi16(p1, off);
        let ps0 = _mm256_sub_epi16(p0, off);
        let qs0 = _mm256_sub_epi16(q0, off);
        let qs1 = _mm256_sub_epi16(q1, off);
        let t = _mm256_sub_epi16(qs0, ps0);
        let t3 = _mm256_add_epi16(_mm256_add_epi16(t, t), t);
        let filter = c(_mm256_add_epi16(
            _mm256_and_si256(c(_mm256_sub_epi16(ps1, qs1)), hev),
            t3,
        ));
        let filter1 = _mm256_srai_epi16::<3>(c(_mm256_add_epi16(filter, _mm256_set1_epi16(4))));
        let filter2 = _mm256_srai_epi16::<3>(c(_mm256_add_epi16(filter, _mm256_set1_epi16(3))));
        let fo = _mm256_srai_epi16::<1>(_mm256_add_epi16(filter1, _mm256_set1_epi16(1)));
        let narrow = _mm256_andnot_si256(flat, mask);
        let outer = _mm256_andnot_si256(hev, narrow);
        let sel = |m: V, a: V, b: V| _mm256_blendv_epi8(b, a, m);
        out[8] = sel(
            narrow,
            _mm256_add_epi16(c(_mm256_sub_epi16(qs0, filter1)), off),
            q0,
        );
        out[7] = sel(
            narrow,
            _mm256_add_epi16(c(_mm256_add_epi16(ps0, filter2)), off),
            p0,
        );
        out[9] = sel(
            outer,
            _mm256_add_epi16(c(_mm256_sub_epi16(qs1, fo)), off),
            q1,
        );
        out[6] = sel(
            outer,
            _mm256_add_epi16(c(_mm256_add_epi16(ps1, fo)), off),
            p1,
        );
        let any_flat = _mm256_testz_si256(flat, flat) == 0;
        let any_flat2 = _mm256_testz_si256(flat2, flat2) == 0;
        if any_flat {
            let w8 = wide::<3>(&v);
            let m = _mm256_andnot_si256(flat2, flat);
            for k in 5..11 {
                out[k] = sel(m, w8[k], out[k]);
            }
        }
        if any_flat2 {
            let w16 = wide::<4>(&v);
            for k in 1..15 {
                out[k] = sel(flat2, w16[k], out[k]);
            }
        }
        if rows {
            let (lo, hi) = if any_flat2 {
                (1, 15)
            } else if any_flat {
                (5, 11)
            } else {
                (6, 10)
            };
            for (k, o) in out.iter().enumerate().take(hi).skip(lo) {
                _mm256_storeu_si256(
                    base.offset((k as isize - 8) * across as isize) as *mut V,
                    *o,
                );
            }
        } else if r == 8 {
            let m = transpose16(&out);
            for (l, m) in m.iter().enumerate() {
                _mm256_storeu_si256(base.add(l * along).sub(8) as *mut V, *m);
            }
        } else {
            let m = transpose8([
                out[4], out[5], out[6], out[7], out[8], out[9], out[10], out[11],
            ]);
            for (l, m) in m.iter().enumerate() {
                _mm_storeu_si128(
                    base.add(l * along).sub(4) as *mut __m128i,
                    _mm256_castsi256_si128(*m),
                );
                _mm_storeu_si128(
                    base.add((l + 8) * along).sub(4) as *mut __m128i,
                    _mm256_extracti128_si256::<1>(*m),
                );
            }
        }
    }
}
