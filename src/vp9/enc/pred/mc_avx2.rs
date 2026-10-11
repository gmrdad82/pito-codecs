use std::arch::x86_64::*;

pub const OVERREAD: usize = 16;

#[inline(always)]
fn pair(f: &[i16; 8], k: usize) -> i16 {
    ((f[k + 1] as u8 as u16) << 8 | f[k] as u8 as u16) as i16
}

#[inline(always)]
unsafe fn narrow128(v: __m128i) -> __m128i {
    unsafe {
        let t = _mm_srli_epi16(_mm_add_epi16(v, _mm_set1_epi16(64)), 7);
        _mm_andnot_si128(_mm_cmpgt_epi16(t, _mm_set1_epi16(383)), t)
    }
}

#[inline(always)]
unsafe fn narrow256(v: __m256i) -> __m256i {
    unsafe {
        let t = _mm256_srli_epi16(_mm256_add_epi16(v, _mm256_set1_epi16(64)), 7);
        _mm256_andnot_si256(_mm256_cmpgt_epi16(t, _mm256_set1_epi16(383)), t)
    }
}

const SHUF: [[i8; 16]; 4] = {
    let mut m = [[0i8; 16]; 4];
    let mut k = 0;
    while k < 4 {
        let mut i = 0;
        while i < 8 {
            m[k][2 * i] = (2 * k + i) as i8;
            m[k][2 * i + 1] = (2 * k + i + 1) as i8;
            i += 1;
        }
        k += 1;
    }
    m
};

#[target_feature(enable = "avx2")]
pub unsafe fn h_pass(
    src: *const u8,
    ss: usize,
    w: usize,
    h: usize,
    f: &[i16; 8],
    dst: *mut u8,
    ds: usize,
) {
    unsafe {
        if w >= 16 {
            let c: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi16(pair(f, 2 * k)));
            let m: [__m256i; 4] = std::array::from_fn(|k| {
                _mm256_broadcastsi128_si256(_mm_loadu_si128(SHUF[k].as_ptr() as *const __m128i))
            });
            for i in 0..h {
                let s = src.add(i * ss);
                let d = dst.add(i * ds);
                let mut j = 0;
                while j < w {
                    let x = _mm256_loadu2_m128i(
                        s.add(j + 8) as *const __m128i,
                        s.add(j) as *const __m128i,
                    );
                    let p0 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m[0]), c[0]);
                    let p1 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m[1]), c[1]);
                    let p2 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m[2]), c[2]);
                    let p3 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m[3]), c[3]);
                    let v = narrow256(_mm256_add_epi16(
                        _mm256_add_epi16(p0, p1),
                        _mm256_add_epi16(p2, p3),
                    ));
                    let b = _mm256_permute4x64_epi64::<0b00_00_10_00>(_mm256_packus_epi16(v, v));
                    _mm_storeu_si128(d.add(j) as *mut __m128i, _mm256_castsi256_si128(b));
                    j += 16;
                }
            }
            return;
        }
        let c2: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi16(pair(f, 2 * k)));
        let m2: [__m256i; 4] = std::array::from_fn(|k| {
            _mm256_broadcastsi128_si256(_mm_loadu_si128(SHUF[k].as_ptr() as *const __m128i))
        });
        let mut i = 0;
        while i + 1 < h {
            let x = _mm256_loadu2_m128i(
                src.add((i + 1) * ss) as *const __m128i,
                src.add(i * ss) as *const __m128i,
            );
            let p0 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m2[0]), c2[0]);
            let p1 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m2[1]), c2[1]);
            let p2 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m2[2]), c2[2]);
            let p3 = _mm256_maddubs_epi16(_mm256_shuffle_epi8(x, m2[3]), c2[3]);
            let v = narrow256(_mm256_add_epi16(
                _mm256_add_epi16(p0, p1),
                _mm256_add_epi16(p2, p3),
            ));
            let b = _mm256_packus_epi16(v, v);
            let (lo, hi) = (_mm256_castsi256_si128(b), _mm256_extracti128_si256::<1>(b));
            let (d0, d1) = (dst.add(i * ds), dst.add((i + 1) * ds));
            if w == 8 {
                _mm_storel_epi64(d0 as *mut __m128i, lo);
                _mm_storel_epi64(d1 as *mut __m128i, hi);
            } else {
                (d0 as *mut i32).write_unaligned(_mm_cvtsi128_si32(lo));
                (d1 as *mut i32).write_unaligned(_mm_cvtsi128_si32(hi));
            }
            i += 2;
        }
        let c: [__m128i; 4] = std::array::from_fn(|k| _mm_set1_epi16(pair(f, 2 * k)));
        let m: [__m128i; 4] =
            std::array::from_fn(|k| _mm_loadu_si128(SHUF[k].as_ptr() as *const __m128i));
        for i in i..h {
            let x = _mm_loadu_si128(src.add(i * ss) as *const __m128i);
            let p0 = _mm_maddubs_epi16(_mm_shuffle_epi8(x, m[0]), c[0]);
            let p1 = _mm_maddubs_epi16(_mm_shuffle_epi8(x, m[1]), c[1]);
            let p2 = _mm_maddubs_epi16(_mm_shuffle_epi8(x, m[2]), c[2]);
            let p3 = _mm_maddubs_epi16(_mm_shuffle_epi8(x, m[3]), c[3]);
            let v = narrow128(_mm_add_epi16(_mm_add_epi16(p0, p1), _mm_add_epi16(p2, p3)));
            let b = _mm_packus_epi16(v, v);
            let d = dst.add(i * ds);
            if w == 8 {
                _mm_storel_epi64(d as *mut __m128i, b);
            } else {
                (d as *mut i32).write_unaligned(_mm_cvtsi128_si32(b));
            }
        }
    }
}

#[target_feature(enable = "avx2")]
pub unsafe fn v_pass(
    src: *const u8,
    ss: usize,
    w: usize,
    h: usize,
    f: &[i16; 8],
    dst: *mut u8,
    ds: usize,
) {
    unsafe {
        if w >= 32 {
            let c: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi16(pair(f, 2 * k)));
            let mut j = 0;
            while j < w {
                let s = src.add(j);
                let mut r = [_mm256_setzero_si256(); 8];
                for t in 0..7 {
                    r[t] = _mm256_loadu_si256(s.add(t * ss) as *const __m256i);
                }
                for i in 0..h {
                    r[7] = _mm256_loadu_si256(s.add((i + 7) * ss) as *const __m256i);
                    let mut lo = _mm256_setzero_si256();
                    let mut hi = _mm256_setzero_si256();
                    for k in 0..4 {
                        let (a, b) = (r[2 * k], r[2 * k + 1]);
                        lo = _mm256_add_epi16(
                            lo,
                            _mm256_maddubs_epi16(_mm256_unpacklo_epi8(a, b), c[k]),
                        );
                        hi = _mm256_add_epi16(
                            hi,
                            _mm256_maddubs_epi16(_mm256_unpackhi_epi8(a, b), c[k]),
                        );
                    }
                    let out = _mm256_packus_epi16(narrow256(lo), narrow256(hi));
                    _mm256_storeu_si256(dst.add(i * ds + j) as *mut __m256i, out);
                    r.copy_within(1..8, 0);
                }
                j += 32;
            }
            return;
        }
        let c: [__m128i; 4] = std::array::from_fn(|k| _mm_set1_epi16(pair(f, 2 * k)));
        let load = |p: *const u8| -> __m128i {
            match w {
                16 => _mm_loadu_si128(p as *const __m128i),
                8 => _mm_loadl_epi64(p as *const __m128i),
                _ => _mm_cvtsi32_si128((p as *const i32).read_unaligned()),
            }
        };
        let mut r = [_mm_setzero_si128(); 8];
        for t in 0..7 {
            r[t] = load(src.add(t * ss));
        }
        for i in 0..h {
            r[7] = load(src.add((i + 7) * ss));
            let mut lo = _mm_setzero_si128();
            let mut hi = _mm_setzero_si128();
            for k in 0..4 {
                let (a, b) = (r[2 * k], r[2 * k + 1]);
                lo = _mm_add_epi16(lo, _mm_maddubs_epi16(_mm_unpacklo_epi8(a, b), c[k]));
                if w == 16 {
                    hi = _mm_add_epi16(hi, _mm_maddubs_epi16(_mm_unpackhi_epi8(a, b), c[k]));
                }
            }
            let out = _mm_packus_epi16(narrow128(lo), narrow128(hi));
            let d = dst.add(i * ds);
            match w {
                16 => _mm_storeu_si128(d as *mut __m128i, out),
                8 => _mm_storel_epi64(d as *mut __m128i, out),
                _ => (d as *mut i32).write_unaligned(_mm_cvtsi128_si32(out)),
            }
            r.copy_within(1..8, 0);
        }
    }
}
