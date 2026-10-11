use std::arch::x86_64::*;

#[derive(Clone, Copy)]
pub struct Lanes {
    pub alpha: __m256i,
    pub beta: __m256i,
    pub tc0: __m256i,
}

impl Lanes {
    #[target_feature(enable = "avx2")]
    pub fn luma(alpha: i32, beta: i32, tc: [i16; 4]) -> Lanes {
        let g = |t: i16| ((t as u16 as u64) * 0x0001_0001_0001_0001) as i64;
        Lanes {
            alpha: _mm256_set1_epi16(alpha as i16),
            beta: _mm256_set1_epi16(beta as i16),
            tc0: _mm256_set_epi64x(g(tc[3]), g(tc[2]), g(tc[1]), g(tc[0])),
        }
    }

    #[target_feature(enable = "avx2")]
    pub fn chroma(alpha: [i32; 2], beta: [i32; 2], tc: [[i16; 4]; 2]) -> Lanes {
        let g = |t: [i16; 4], k: usize| {
            let a = t[2 * k] as u16 as u64 * 0x0001_0001;
            let b = t[2 * k + 1] as u16 as u64 * 0x0001_0001;
            (a | (b << 32)) as i64
        };
        Lanes {
            alpha: _mm256_set_m128i(
                _mm_set1_epi16(alpha[1] as i16),
                _mm_set1_epi16(alpha[0] as i16),
            ),
            beta: _mm256_set_m128i(
                _mm_set1_epi16(beta[1] as i16),
                _mm_set1_epi16(beta[0] as i16),
            ),
            tc0: _mm256_set_epi64x(g(tc[1], 1), g(tc[1], 0), g(tc[0], 1), g(tc[0], 0)),
        }
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn filter(s: &mut [__m256i; 8], l: &Lanes, strong: bool, chroma: bool) -> bool {
    let [p3, p2, p1, p0, q0, q1, q2, q3] = *s;
    let absd = |a, b| _mm256_abs_epi16(_mm256_sub_epi16(a, b));
    let lt = |a, b| _mm256_cmpgt_epi16(b, a);
    let and = _mm256_and_si256;
    let add = _mm256_add_epi16;
    let sub = _mm256_sub_epi16;
    let sel = |m, a, b| _mm256_blendv_epi8(b, a, m);
    let on = and(
        and(
            _mm256_cmpgt_epi16(l.tc0, _mm256_set1_epi16(-1)),
            lt(absd(p0, q0), l.alpha),
        ),
        and(lt(absd(p1, p0), l.beta), lt(absd(q1, q0), l.beta)),
    );
    if _mm256_testz_si256(on, on) != 0 {
        return false;
    }
    let two = _mm256_set1_epi16(2);
    let four = _mm256_set1_epi16(4);
    let wp0 = _mm256_srai_epi16(add(add(add(p1, p1), add(p0, q1)), two), 2);
    let wq0 = _mm256_srai_epi16(add(add(add(q1, q1), add(q0, p1)), two), 2);
    if chroma && strong {
        s[3] = sel(on, wp0, p0);
        s[4] = sel(on, wq0, q0);
        return true;
    }
    if strong {
        let lim = add(_mm256_srai_epi16(l.alpha, 2), two);
        let st = and(on, lt(absd(p0, q0), lim));
        let sp = and(st, lt(absd(p2, p0), l.beta));
        let sq = and(st, lt(absd(q2, q0), l.beta));
        let pq = add(p0, q0);
        let sp0 = _mm256_srai_epi16(
            add(add(add(p2, q1), add(pq, pq)), add(add(p1, p1), four)),
            3,
        );
        let sq0 = _mm256_srai_epi16(
            add(add(add(q2, p1), add(pq, pq)), add(add(q1, q1), four)),
            3,
        );
        let s4p = add(add(p2, p1), pq);
        let s4q = add(add(q2, q1), pq);
        let sp1 = _mm256_srai_epi16(add(s4p, two), 2);
        let sq1 = _mm256_srai_epi16(add(s4q, two), 2);
        let sp2 = _mm256_srai_epi16(add(add(s4p, add(p3, p3)), add(add(p2, p2), four)), 3);
        let sq2 = _mm256_srai_epi16(add(add(s4q, add(q3, q3)), add(add(q2, q2), four)), 3);
        s[3] = sel(sp, sp0, sel(on, wp0, p0));
        s[2] = sel(sp, sp1, p1);
        s[1] = sel(sp, sp2, p2);
        s[4] = sel(sq, sq0, sel(on, wq0, q0));
        s[5] = sel(sq, sq1, q1);
        s[6] = sel(sq, sq2, q2);
        return true;
    }
    let base = _mm256_srai_epi16(
        add(add(_mm256_slli_epi16(sub(q0, p0), 2), sub(p1, q1)), four),
        3,
    );
    if chroma {
        let tc = add(l.tc0, _mm256_set1_epi16(1));
        let delta = _mm256_max_epi16(_mm256_min_epi16(base, tc), sub(_mm256_setzero_si256(), tc));
        s[3] = sel(on, add(p0, delta), p0);
        s[4] = sel(on, sub(q0, delta), q0);
        return true;
    }
    let ap = lt(absd(p2, p0), l.beta);
    let aq = lt(absd(q2, q0), l.beta);
    let tc = sub(sub(l.tc0, ap), aq);
    let delta = _mm256_max_epi16(_mm256_min_epi16(base, tc), sub(_mm256_setzero_si256(), tc));
    let avg = _mm256_avg_epu16(p0, q0);
    let ntc0 = sub(_mm256_setzero_si256(), l.tc0);
    let dp1 = _mm256_srai_epi16(sub(add(p2, avg), add(p1, p1)), 1);
    let dq1 = _mm256_srai_epi16(sub(add(q2, avg), add(q1, q1)), 1);
    let np1 = add(p1, _mm256_max_epi16(_mm256_min_epi16(dp1, l.tc0), ntc0));
    let nq1 = add(q1, _mm256_max_epi16(_mm256_min_epi16(dq1, l.tc0), ntc0));
    s[3] = sel(on, add(p0, delta), p0);
    s[4] = sel(on, sub(q0, delta), q0);
    s[2] = sel(and(on, ap), np1, p1);
    s[5] = sel(and(on, aq), nq1, q1);
    true
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn load_cols(lo: *const u8, hi: *const u8, stride: usize) -> [__m256i; 8] {
    unsafe {
        let r = |i: usize| {
            let p = if i < 8 {
                lo.add(i * stride)
            } else {
                hi.add((i - 8) * stride)
            };
            _mm_loadl_epi64(p.cast())
        };
        let a: [__m128i; 8] = std::array::from_fn(|k| _mm_unpacklo_epi8(r(2 * k), r(2 * k + 1)));
        let b = [
            _mm_unpacklo_epi16(a[0], a[1]),
            _mm_unpackhi_epi16(a[0], a[1]),
            _mm_unpacklo_epi16(a[2], a[3]),
            _mm_unpackhi_epi16(a[2], a[3]),
            _mm_unpacklo_epi16(a[4], a[5]),
            _mm_unpackhi_epi16(a[4], a[5]),
            _mm_unpacklo_epi16(a[6], a[7]),
            _mm_unpackhi_epi16(a[6], a[7]),
        ];
        let c = [
            _mm_unpacklo_epi32(b[0], b[2]),
            _mm_unpackhi_epi32(b[0], b[2]),
            _mm_unpacklo_epi32(b[1], b[3]),
            _mm_unpackhi_epi32(b[1], b[3]),
            _mm_unpacklo_epi32(b[4], b[6]),
            _mm_unpackhi_epi32(b[4], b[6]),
            _mm_unpacklo_epi32(b[5], b[7]),
            _mm_unpackhi_epi32(b[5], b[7]),
        ];
        let w = _mm256_cvtepu8_epi16;
        [
            w(_mm_unpacklo_epi64(c[0], c[4])),
            w(_mm_unpackhi_epi64(c[0], c[4])),
            w(_mm_unpacklo_epi64(c[1], c[5])),
            w(_mm_unpackhi_epi64(c[1], c[5])),
            w(_mm_unpacklo_epi64(c[2], c[6])),
            w(_mm_unpackhi_epi64(c[2], c[6])),
            w(_mm_unpacklo_epi64(c[3], c[7])),
            w(_mm_unpackhi_epi64(c[3], c[7])),
        ]
    }
}

#[inline]
#[target_feature(enable = "avx2")]
fn pack2(a: __m256i, b: __m256i) -> (__m128i, __m128i) {
    let v = _mm256_permute4x64_epi64(_mm256_packus_epi16(a, b), 0xd8);
    (_mm256_castsi256_si128(v), _mm256_extracti128_si256(v, 1))
}

#[inline]
#[target_feature(enable = "avx2")]
unsafe fn store_cols(lo: *mut u8, hi: *mut u8, stride: usize, s: &[__m256i; 8]) {
    unsafe {
        let (c0, c1) = pack2(s[0], s[1]);
        let (c2, c3) = pack2(s[2], s[3]);
        let (c4, c5) = pack2(s[4], s[5]);
        let (c6, c7) = pack2(s[6], s[7]);
        let a = [
            _mm_unpacklo_epi8(c0, c1),
            _mm_unpackhi_epi8(c0, c1),
            _mm_unpacklo_epi8(c2, c3),
            _mm_unpackhi_epi8(c2, c3),
            _mm_unpacklo_epi8(c4, c5),
            _mm_unpackhi_epi8(c4, c5),
            _mm_unpacklo_epi8(c6, c7),
            _mm_unpackhi_epi8(c6, c7),
        ];
        let b = [
            _mm_unpacklo_epi16(a[0], a[2]),
            _mm_unpackhi_epi16(a[0], a[2]),
            _mm_unpacklo_epi16(a[4], a[6]),
            _mm_unpackhi_epi16(a[4], a[6]),
            _mm_unpacklo_epi16(a[1], a[3]),
            _mm_unpackhi_epi16(a[1], a[3]),
            _mm_unpacklo_epi16(a[5], a[7]),
            _mm_unpackhi_epi16(a[5], a[7]),
        ];
        let d = [
            _mm_unpacklo_epi32(b[0], b[2]),
            _mm_unpackhi_epi32(b[0], b[2]),
            _mm_unpacklo_epi32(b[1], b[3]),
            _mm_unpackhi_epi32(b[1], b[3]),
            _mm_unpacklo_epi32(b[4], b[6]),
            _mm_unpackhi_epi32(b[4], b[6]),
            _mm_unpacklo_epi32(b[5], b[7]),
            _mm_unpackhi_epi32(b[5], b[7]),
        ];
        let row = |i: usize| {
            if i < 8 {
                lo.add(i * stride)
            } else {
                hi.add((i - 8) * stride)
            }
        };
        for (k, v) in d.iter().enumerate() {
            _mm_storel_epi64(row(2 * k).cast(), *v);
            _mm_storeh_pd(row(2 * k + 1).cast(), _mm_castsi128_pd(*v));
        }
    }
}

#[target_feature(enable = "avx2")]
pub fn luma_vertical(pix: &mut [u8], stride: usize, x: usize, y: usize, l: &Lanes, strong: bool) {
    assert!(x >= 4 && (y + 15) * stride + x + 4 <= pix.len());
    unsafe {
        let p = pix.as_mut_ptr().add(y * stride + x - 4);
        let hi = p.add(8 * stride);
        let mut s = load_cols(p, hi, stride);
        if filter(&mut s, l, strong, false) {
            store_cols(p, hi, stride, &s);
        }
    }
}

#[target_feature(enable = "avx2")]
pub fn luma_horizontal(pix: &mut [u8], stride: usize, x: usize, y: usize, l: &Lanes, strong: bool) {
    assert!(y >= 4 && (y + 3) * stride + x + 16 <= pix.len());
    unsafe {
        let p = pix.as_mut_ptr().add((y - 4) * stride + x);
        let mut s: [__m256i; 8] = std::array::from_fn(|k| {
            _mm256_cvtepu8_epi16(_mm_loadu_si128(p.add(k * stride).cast()))
        });
        if filter(&mut s, l, strong, false) {
            for k in (1..7).step_by(2) {
                let (a, b) = pack2(s[k], s[k + 1]);
                _mm_storeu_si128(p.add(k * stride).cast(), a);
                _mm_storeu_si128(p.add((k + 1) * stride).cast(), b);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub fn chroma_vertical(
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    l: &Lanes,
    strong: bool,
) {
    let end = (y + 7) * stride + x + 4;
    assert!(x >= 4 && end <= cb.len() && end <= cr.len());
    unsafe {
        let a = cb.as_mut_ptr().add(y * stride + x - 4);
        let b = cr.as_mut_ptr().add(y * stride + x - 4);
        let mut s = load_cols(a, b, stride);
        if filter(&mut s, l, strong, true) {
            store_cols(a, b, stride, &s);
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub fn chroma_horizontal(
    cb: &mut [u8],
    cr: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    l: &Lanes,
    strong: bool,
) {
    let end = (y + 1) * stride + x + 8;
    assert!(y >= 2 && end <= cb.len() && end <= cr.len());
    unsafe {
        let a = cb.as_mut_ptr().add((y - 2) * stride + x);
        let b = cr.as_mut_ptr().add((y - 2) * stride + x);
        let z = _mm256_setzero_si256();
        let row = |k: usize| {
            _mm256_cvtepu8_epi16(_mm_unpacklo_epi64(
                _mm_loadl_epi64(a.add(k * stride).cast()),
                _mm_loadl_epi64(b.add(k * stride).cast()),
            ))
        };
        let mut s = [z, z, row(0), row(1), row(2), row(3), z, z];
        if filter(&mut s, l, strong, true) {
            let (p0, q0) = pack2(s[3], s[4]);
            _mm_storel_epi64(a.add(stride).cast(), p0);
            _mm_storeh_pd(b.add(stride).cast(), _mm_castsi128_pd(p0));
            _mm_storel_epi64(a.add(2 * stride).cast(), q0);
            _mm_storeh_pd(b.add(2 * stride).cast(), _mm_castsi128_pd(q0));
        }
    }
}
