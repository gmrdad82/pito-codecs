#[derive(Clone, Copy, Default)]
pub struct Strength {
    pub limit: u8,
    pub ilimit: u8,
    pub hev: u8,
    pub inner: bool,
}

pub fn strength(level: i32, sharpness: u8, inner: bool) -> Strength {
    let level = level.clamp(0, 63);
    if level == 0 {
        return Strength::default();
    }
    let mut il = level;
    if sharpness > 0 {
        il >>= if sharpness > 4 { 2 } else { 1 };
        il = il.min(9 - sharpness as i32);
    }
    let il = il.max(1);
    Strength {
        limit: level as u8,
        ilimit: il as u8,
        hev: (level >= 15) as u8 + (level >= 40) as u8,
        inner,
    }
}

pub struct Planes<'a> {
    pub y: &'a mut [u8],
    pub u: &'a mut [u8],
    pub v: &'a mut [u8],
    pub ys: usize,
    pub uvs: usize,
    pub yr: usize,
    pub cr: usize,
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub mod scalar {
    use super::{Planes, Strength};

    #[inline(always)]
    fn s8(v: i32) -> i32 {
        v.clamp(-128, 127)
    }

    #[inline(always)]
    fn u8c(v: i32) -> u8 {
        v.clamp(0, 255) as u8
    }

    #[inline(always)]
    fn simple_ok(p1: i32, p0: i32, q0: i32, q1: i32, limit: i32) -> bool {
        (p0 - q0).abs() * 2 + ((p1 - q1).abs() >> 1) <= limit
    }

    #[inline(always)]
    fn common(b: &mut [u8], i: usize, s: usize, outer: bool) {
        let p1 = b[i - 2 * s] as i32;
        let p0 = b[i - s] as i32;
        let q0 = b[i] as i32;
        let q1 = b[i + s] as i32;
        let mut a = 3 * (q0 - p0);
        if outer {
            a += s8(p1 - q1);
        }
        let a = s8(a);
        let f1 = (a + 4).min(127) >> 3;
        let f2 = (a + 3).min(127) >> 3;
        b[i - s] = u8c(p0 + f2);
        b[i] = u8c(q0 - f1);
        if !outer {
            let a = (f1 + 1) >> 1;
            b[i - 2 * s] = u8c(p1 + a);
            b[i + s] = u8c(q1 - a);
        }
    }

    #[inline(always)]
    fn mb_edge(b: &mut [u8], i: usize, s: usize) {
        let p2 = b[i - 3 * s] as i32;
        let p1 = b[i - 2 * s] as i32;
        let p0 = b[i - s] as i32;
        let q0 = b[i] as i32;
        let q1 = b[i + s] as i32;
        let q2 = b[i + 2 * s] as i32;
        let w = s8(s8(p1 - q1) + 3 * (q0 - p0));
        let a = (27 * w + 63) >> 7;
        b[i - s] = u8c(p0 + a);
        b[i] = u8c(q0 - a);
        let a = (18 * w + 63) >> 7;
        b[i - 2 * s] = u8c(p1 + a);
        b[i + s] = u8c(q1 - a);
        let a = (9 * w + 63) >> 7;
        b[i - 3 * s] = u8c(p2 + a);
        b[i + 2 * s] = u8c(q2 - a);
    }

    #[inline(always)]
    fn normal_ok(b: &[u8], i: usize, s: usize, e: i32, l: i32) -> bool {
        let p3 = b[i - 4 * s] as i32;
        let p2 = b[i - 3 * s] as i32;
        let p1 = b[i - 2 * s] as i32;
        let p0 = b[i - s] as i32;
        let q0 = b[i] as i32;
        let q1 = b[i + s] as i32;
        let q2 = b[i + 2 * s] as i32;
        let q3 = b[i + 3 * s] as i32;
        simple_ok(p1, p0, q0, q1, e)
            && (p3 - p2).abs() <= l
            && (p2 - p1).abs() <= l
            && (p1 - p0).abs() <= l
            && (q3 - q2).abs() <= l
            && (q2 - q1).abs() <= l
            && (q1 - q0).abs() <= l
    }

    #[inline(always)]
    fn hev(b: &[u8], i: usize, s: usize, t: i32) -> bool {
        (b[i - 2 * s] as i32 - b[i - s] as i32).abs() > t
            || (b[i + s] as i32 - b[i] as i32).abs() > t
    }

    pub fn simple(b: &mut [u8], at: usize, step: usize, pitch: usize, n: usize, limit: i32) {
        for k in 0..n {
            let i = at + k * pitch;
            let p1 = b[i - 2 * step] as i32;
            let p0 = b[i - step] as i32;
            let q0 = b[i] as i32;
            let q1 = b[i + step] as i32;
            if simple_ok(p1, p0, q0, q1, limit) {
                common(b, i, step, true);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mb(
        b: &mut [u8],
        at: usize,
        step: usize,
        pitch: usize,
        n: usize,
        e: i32,
        l: i32,
        t: i32,
    ) {
        for k in 0..n {
            let i = at + k * pitch;
            if normal_ok(b, i, step, e, l) {
                if hev(b, i, step, t) {
                    common(b, i, step, true);
                } else {
                    mb_edge(b, i, step);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn inner(
        b: &mut [u8],
        at: usize,
        step: usize,
        pitch: usize,
        n: usize,
        e: i32,
        l: i32,
        t: i32,
    ) {
        for k in 0..n {
            let i = at + k * pitch;
            if normal_ok(b, i, step, e, l) {
                let h = hev(b, i, step, t);
                common(b, i, step, h);
            }
        }
    }

    pub fn filter_mb(p: &mut Planes, mx: usize, my: usize, f: Strength, simple_filter: bool) {
        if f.limit == 0 {
            return;
        }
        let lv = f.limit as i32;
        let il = f.ilimit as i32;
        let t = f.hev as i32;
        let ys = p.ys;
        let yo = (my * 16 - p.yr) * ys + mx * 16;
        if simple_filter {
            let mbl = (lv + 2) * 2 + il;
            let bl = lv * 2 + il;
            if mx > 0 {
                simple(p.y, yo, 1, ys, 16, mbl);
            }
            if f.inner {
                for x in [4, 8, 12] {
                    simple(p.y, yo + x, 1, ys, 16, bl);
                }
            }
            if my > 0 {
                simple(p.y, yo, ys, 1, 16, mbl);
            }
            if f.inner {
                for y in [4, 8, 12] {
                    simple(p.y, yo + y * ys, ys, 1, 16, bl);
                }
            }
            return;
        }
        let me = (lv + 2) * 2 + il;
        let ie = lv * 2 + il;
        let uvs = p.uvs;
        let co = (my * 8 - p.cr) * uvs + mx * 8;
        if mx > 0 {
            mb(p.y, yo, 1, ys, 16, me, il, t);
            mb(p.u, co, 1, uvs, 8, me, il, t);
            mb(p.v, co, 1, uvs, 8, me, il, t);
        }
        if f.inner {
            for x in [4, 8, 12] {
                inner(p.y, yo + x, 1, ys, 16, ie, il, t);
            }
            inner(p.u, co + 4, 1, uvs, 8, ie, il, t);
            inner(p.v, co + 4, 1, uvs, 8, ie, il, t);
        }
        if my > 0 {
            mb(p.y, yo, ys, 1, 16, me, il, t);
            mb(p.u, co, uvs, 1, 8, me, il, t);
            mb(p.v, co, uvs, 1, 8, me, il, t);
        }
        if f.inner {
            for y in [4, 8, 12] {
                inner(p.y, yo + y * ys, ys, 1, 16, ie, il, t);
            }
            inner(p.u, co + 4 * uvs, uvs, 1, 8, ie, il, t);
            inner(p.v, co + 4 * uvs, uvs, 1, 8, ie, il, t);
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub mod simd {
    use super::{Planes, Strength};
    use std::arch::x86_64::*;

    #[derive(Clone, Copy)]
    struct V8([__m128i; 8]);

    #[inline(always)]
    fn absd(a: __m128i, b: __m128i) -> __m128i {
        unsafe { _mm_or_si128(_mm_subs_epu8(a, b), _mm_subs_epu8(b, a)) }
    }

    #[inline(always)]
    fn flip(v: __m128i) -> __m128i {
        unsafe { _mm_xor_si128(v, _mm_set1_epi8(-128)) }
    }

    #[inline(always)]
    fn needs(p1: __m128i, p0: __m128i, q0: __m128i, q1: __m128i, t: i32) -> __m128i {
        unsafe {
            let a = _mm_srli_epi16(_mm_and_si128(absd(p1, q1), _mm_set1_epi8(-2)), 1);
            let b = absd(p0, q0);
            let s = _mm_adds_epu8(_mm_adds_epu8(b, b), a);
            _mm_cmpeq_epi8(
                _mm_subs_epu8(s, _mm_set1_epi8(t as u8 as i8)),
                _mm_setzero_si128(),
            )
        }
    }

    #[inline(always)]
    fn not_hev(p1: __m128i, p0: __m128i, q0: __m128i, q1: __m128i, t: i32) -> __m128i {
        unsafe {
            let m = _mm_max_epu8(absd(p1, p0), absd(q1, q0));
            _mm_cmpeq_epi8(
                _mm_subs_epu8(m, _mm_set1_epi8(t as i8)),
                _mm_setzero_si128(),
            )
        }
    }

    #[inline(always)]
    fn sshift3(v: __m128i) -> __m128i {
        unsafe {
            let z = _mm_setzero_si128();
            let lo = _mm_srai_epi16(_mm_unpacklo_epi8(z, v), 11);
            let hi = _mm_srai_epi16(_mm_unpackhi_epi8(z, v), 11);
            _mm_packs_epi16(lo, hi)
        }
    }

    #[inline(always)]
    fn delta(p1: __m128i, p0: __m128i, q0: __m128i, q1: __m128i) -> __m128i {
        unsafe {
            let d = _mm_subs_epi8(q0, p0);
            let a = _mm_subs_epi8(p1, q1);
            _mm_adds_epi8(_mm_adds_epi8(_mm_adds_epi8(a, d), d), d)
        }
    }

    #[inline(always)]
    fn simple_apply(p0: &mut __m128i, q0: &mut __m128i, f: __m128i) {
        unsafe {
            let f1 = sshift3(_mm_adds_epi8(f, _mm_set1_epi8(4)));
            let f2 = sshift3(_mm_adds_epi8(f, _mm_set1_epi8(3)));
            *q0 = _mm_subs_epi8(*q0, f1);
            *p0 = _mm_adds_epi8(*p0, f2);
        }
    }

    #[inline(always)]
    fn simple(v: &mut V8, t: i32) {
        unsafe {
            let [_, _, p1, p0, q0, q1, _, _] = &mut v.0;
            let m = needs(*p1, *p0, *q0, *q1, t);
            let (sp1, mut sp0, mut sq0, sq1) = (flip(*p1), flip(*p0), flip(*q0), flip(*q1));
            let f = _mm_and_si128(delta(sp1, sp0, sq0, sq1), m);
            simple_apply(&mut sp0, &mut sq0, f);
            *p0 = flip(sp0);
            *q0 = flip(sq0);
        }
    }

    #[inline(always)]
    fn mask(v: &V8, t: i32, it: i32) -> __m128i {
        unsafe {
            let [p3, p2, p1, p0, q0, q1, q2, q3] = v.0;
            let mut m = _mm_max_epu8(absd(p3, p2), absd(p2, p1));
            m = _mm_max_epu8(m, absd(p1, p0));
            m = _mm_max_epu8(m, absd(q3, q2));
            m = _mm_max_epu8(m, absd(q2, q1));
            m = _mm_max_epu8(m, absd(q1, q0));
            let im = _mm_cmpeq_epi8(
                _mm_subs_epu8(m, _mm_set1_epi8(it as i8)),
                _mm_setzero_si128(),
            );
            _mm_and_si128(im, needs(p1, p0, q0, q1, t))
        }
    }

    #[inline(always)]
    fn inner(v: &mut V8, t: i32, it: i32, hev: i32) {
        unsafe {
            let m = mask(v, t, it);
            let [_, _, p1, p0, q0, q1, _, _] = &mut v.0;
            let nh = not_hev(*p1, *p0, *q0, *q1, hev);
            let (mut sp1, mut sp0, mut sq0, mut sq1) = (flip(*p1), flip(*p0), flip(*q0), flip(*q1));
            let d = _mm_subs_epi8(sq0, sp0);
            let mut a = _mm_andnot_si128(nh, _mm_subs_epi8(sp1, sq1));
            a = _mm_adds_epi8(_mm_adds_epi8(_mm_adds_epi8(a, d), d), d);
            a = _mm_and_si128(a, m);
            let f2 = sshift3(_mm_adds_epi8(a, _mm_set1_epi8(3)));
            let f1 = sshift3(_mm_adds_epi8(a, _mm_set1_epi8(4)));
            sp0 = _mm_adds_epi8(sp0, f2);
            sq0 = _mm_subs_epi8(sq0, f1);
            let h = _mm_sub_epi8(
                _mm_avg_epu8(_mm_add_epi8(f1, _mm_set1_epi8(-128)), _mm_setzero_si128()),
                _mm_set1_epi8(64),
            );
            let h = _mm_and_si128(nh, h);
            sq1 = _mm_subs_epi8(sq1, h);
            sp1 = _mm_adds_epi8(sp1, h);
            *p1 = flip(sp1);
            *p0 = flip(sp0);
            *q0 = flip(sq0);
            *q1 = flip(sq1);
        }
    }

    #[inline(always)]
    fn update(p: &mut __m128i, q: &mut __m128i, lo: __m128i, hi: __m128i) {
        unsafe {
            let d = _mm_packs_epi16(_mm_srai_epi16(lo, 7), _mm_srai_epi16(hi, 7));
            *p = _mm_adds_epi8(*p, d);
            *q = _mm_subs_epi8(*q, d);
        }
    }

    #[inline(always)]
    fn edge(v: &mut V8, t: i32, it: i32, hev: i32) {
        unsafe {
            let m = mask(v, t, it);
            let [_, p2, p1, p0, q0, q1, q2, _] = &mut v.0;
            let nh = not_hev(*p1, *p0, *q0, *q1, hev);
            let (mut sp2, mut sp1, mut sp0) = (flip(*p2), flip(*p1), flip(*p0));
            let (mut sq0, mut sq1, mut sq2) = (flip(*q0), flip(*q1), flip(*q2));
            let a = delta(sp1, sp0, sq0, sq1);
            let f = _mm_and_si128(a, _mm_andnot_si128(nh, m));
            simple_apply(&mut sp0, &mut sq0, f);
            let f = _mm_and_si128(a, _mm_and_si128(nh, m));
            let z = _mm_setzero_si128();
            let k9 = _mm_set1_epi16(0x0900);
            let k63 = _mm_set1_epi16(63);
            let f9l = _mm_mulhi_epi16(_mm_unpacklo_epi8(z, f), k9);
            let f9h = _mm_mulhi_epi16(_mm_unpackhi_epi8(z, f), k9);
            let a2l = _mm_add_epi16(f9l, k63);
            let a2h = _mm_add_epi16(f9h, k63);
            let a1l = _mm_add_epi16(a2l, f9l);
            let a1h = _mm_add_epi16(a2h, f9h);
            let a0l = _mm_add_epi16(a1l, f9l);
            let a0h = _mm_add_epi16(a1h, f9h);
            update(&mut sp2, &mut sq2, a2l, a2h);
            update(&mut sp1, &mut sq1, a1l, a1h);
            update(&mut sp0, &mut sq0, a0l, a0h);
            *p2 = flip(sp2);
            *p1 = flip(sp1);
            *p0 = flip(sp0);
            *q0 = flip(sq0);
            *q1 = flip(sq1);
            *q2 = flip(sq2);
        }
    }

    #[derive(Clone, Copy)]
    enum Kind {
        Simple(i32),
        Inner(i32, i32, i32),
        Edge(i32, i32, i32),
    }

    #[inline(always)]
    fn apply(v: &mut V8, k: Kind) {
        match k {
            Kind::Simple(t) => simple(v, t),
            Kind::Inner(t, it, h) => inner(v, t, it, h),
            Kind::Edge(t, it, h) => edge(v, t, it, h),
        }
    }

    #[inline(always)]
    fn h16(b: &mut [u8], at: usize, s: usize, k: Kind) {
        unsafe {
            assert!(at >= 4 * s && at + 3 * s + 16 <= b.len());
            let p = b.as_mut_ptr();
            let mut v = V8([_mm_setzero_si128(); 8]);
            for (i, r) in v.0.iter_mut().enumerate() {
                *r = _mm_loadu_si128(p.add(at + i * s - 4 * s).cast());
            }
            apply(&mut v, k);
            for (i, r) in v.0.iter().enumerate().take(7).skip(1) {
                _mm_storeu_si128(p.add(at + i * s - 4 * s).cast(), *r);
            }
        }
    }

    #[inline(always)]
    fn h8x2(u: &mut [u8], v: &mut [u8], at: usize, s: usize, k: Kind) {
        unsafe {
            assert!(at >= 4 * s && at + 3 * s + 8 <= u.len() && at + 3 * s + 8 <= v.len());
            let pu = u.as_mut_ptr();
            let pv = v.as_mut_ptr();
            let mut w = V8([_mm_setzero_si128(); 8]);
            for (i, r) in w.0.iter_mut().enumerate() {
                let o = at + i * s - 4 * s;
                *r = _mm_unpacklo_epi64(
                    _mm_loadl_epi64(pu.add(o).cast()),
                    _mm_loadl_epi64(pv.add(o).cast()),
                );
            }
            apply(&mut w, k);
            for (i, r) in w.0.iter().enumerate().take(7).skip(1) {
                let o = at + i * s - 4 * s;
                _mm_storel_epi64(pu.add(o).cast(), *r);
                _mm_storel_epi64(pv.add(o).cast(), _mm_unpackhi_epi64(*r, *r));
            }
        }
    }

    #[inline(always)]
    fn transpose_in(r: &[__m128i; 16]) -> V8 {
        unsafe {
            let t: [__m128i; 8] =
                std::array::from_fn(|i| _mm_unpacklo_epi8(r[2 * i], r[2 * i + 1]));
            let mut c = [_mm_setzero_si128(); 8];
            for half in 0..2 {
                let t = &t[half * 4..half * 4 + 4];
                let u0 = _mm_unpacklo_epi16(t[0], t[1]);
                let u1 = _mm_unpackhi_epi16(t[0], t[1]);
                let u2 = _mm_unpacklo_epi16(t[2], t[3]);
                let u3 = _mm_unpackhi_epi16(t[2], t[3]);
                let v = [
                    _mm_unpacklo_epi32(u0, u2),
                    _mm_unpackhi_epi32(u0, u2),
                    _mm_unpacklo_epi32(u1, u3),
                    _mm_unpackhi_epi32(u1, u3),
                ];
                if half == 0 {
                    c[..4].copy_from_slice(&v);
                } else {
                    for i in 0..4 {
                        let lo = c[i];
                        c[i] = _mm_unpacklo_epi64(lo, v[i]);
                        c[4 + i] = _mm_unpackhi_epi64(lo, v[i]);
                    }
                }
            }
            V8([c[0], c[4], c[1], c[5], c[2], c[6], c[3], c[7]])
        }
    }

    #[inline(always)]
    fn transpose_out(c: &V8) -> [__m128i; 8] {
        unsafe {
            let c = &c.0;
            let a: [__m128i; 8] = std::array::from_fn(|i| {
                let (x, y) = (c[(i / 2) * 2], c[(i / 2) * 2 + 1]);
                if i % 2 == 0 {
                    _mm_unpacklo_epi8(x, y)
                } else {
                    _mm_unpackhi_epi8(x, y)
                }
            });
            let mut d = [_mm_setzero_si128(); 8];
            for half in 0..2 {
                let (a0, a2, a4, a6) = (a[half], a[2 + half], a[4 + half], a[6 + half]);
                let b0 = _mm_unpacklo_epi16(a0, a2);
                let b1 = _mm_unpackhi_epi16(a0, a2);
                let b2 = _mm_unpacklo_epi16(a4, a6);
                let b3 = _mm_unpackhi_epi16(a4, a6);
                d[half * 4] = _mm_unpacklo_epi32(b0, b2);
                d[half * 4 + 1] = _mm_unpackhi_epi32(b0, b2);
                d[half * 4 + 2] = _mm_unpacklo_epi32(b1, b3);
                d[half * 4 + 3] = _mm_unpackhi_epi32(b1, b3);
            }
            d
        }
    }

    #[inline(always)]
    fn store_rows(d: &[__m128i; 8], ptrs: &[*mut u8; 16]) {
        unsafe {
            for (i, v) in d.iter().enumerate() {
                _mm_storel_epi64(ptrs[2 * i].cast(), *v);
                _mm_storel_epi64(ptrs[2 * i + 1].cast(), _mm_unpackhi_epi64(*v, *v));
            }
        }
    }

    #[inline(always)]
    fn v16(b: &mut [u8], at: usize, s: usize, k: Kind) {
        unsafe {
            assert!(at >= 4 && at + 15 * s + 4 <= b.len());
            let p = b.as_mut_ptr();
            let ptrs: [*mut u8; 16] = std::array::from_fn(|i| p.add(at + i * s - 4));
            let r: [__m128i; 16] = std::array::from_fn(|i| _mm_loadl_epi64(ptrs[i].cast()));
            let mut v = transpose_in(&r);
            apply(&mut v, k);
            store_rows(&transpose_out(&v), &ptrs);
        }
    }

    #[inline(always)]
    fn v8x2(u: &mut [u8], v: &mut [u8], at: usize, s: usize, k: Kind) {
        unsafe {
            assert!(at >= 4 && at + 7 * s + 4 <= u.len() && at + 7 * s + 4 <= v.len());
            let pu = u.as_mut_ptr();
            let pv = v.as_mut_ptr();
            let ptrs: [*mut u8; 16] = std::array::from_fn(|i| {
                if i < 8 {
                    pu.add(at + i * s - 4)
                } else {
                    pv.add(at + (i - 8) * s - 4)
                }
            });
            let r: [__m128i; 16] = std::array::from_fn(|i| _mm_loadl_epi64(ptrs[i].cast()));
            let mut w = transpose_in(&r);
            apply(&mut w, k);
            store_rows(&transpose_out(&w), &ptrs);
        }
    }

    pub fn filter_mb(p: &mut Planes, mx: usize, my: usize, f: Strength, simple_filter: bool) {
        if f.limit == 0 {
            return;
        }
        let lv = f.limit as i32;
        let il = f.ilimit as i32;
        let t = f.hev as i32;
        let ys = p.ys;
        let yo = (my * 16 - p.yr) * ys + mx * 16;
        if simple_filter {
            let mbl = Kind::Simple((lv + 2) * 2 + il);
            let bl = Kind::Simple(lv * 2 + il);
            if mx > 0 {
                v16(p.y, yo, ys, mbl);
            }
            if f.inner {
                for x in [4, 8, 12] {
                    v16(p.y, yo + x, ys, bl);
                }
            }
            if my > 0 {
                h16(p.y, yo, ys, mbl);
            }
            if f.inner {
                for y in [4, 8, 12] {
                    h16(p.y, yo + y * ys, ys, bl);
                }
            }
            return;
        }
        let me = Kind::Edge((lv + 2) * 2 + il, il, t);
        let ie = Kind::Inner(lv * 2 + il, il, t);
        let uvs = p.uvs;
        let co = (my * 8 - p.cr) * uvs + mx * 8;
        if mx > 0 {
            v16(p.y, yo, ys, me);
            v8x2(p.u, p.v, co, uvs, me);
        }
        if f.inner {
            for x in [4, 8, 12] {
                v16(p.y, yo + x, ys, ie);
            }
            v8x2(p.u, p.v, co + 4, uvs, ie);
        }
        if my > 0 {
            h16(p.y, yo, ys, me);
            h8x2(p.u, p.v, co, uvs, me);
        }
        if f.inner {
            for y in [4, 8, 12] {
                h16(p.y, yo + y * ys, ys, ie);
            }
            h8x2(p.u, p.v, co + 4 * uvs, uvs, ie);
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub use simd::filter_mb as filter_mb_fast;

#[cfg(not(target_arch = "x86_64"))]
pub use scalar::filter_mb as filter_mb_fast;
