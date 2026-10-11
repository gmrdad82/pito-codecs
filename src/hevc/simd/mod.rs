pub(crate) mod itx;
pub(crate) mod mc;
#[cfg(test)]
mod tests;

#[cfg(test)]
thread_local! {
    static OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn force_off(off: bool) {
    OFF.set(off);
}

#[inline(always)]
pub(crate) fn on() -> bool {
    #[cfg(test)]
    if OFF.get() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        crate::hevc::isa::avx2()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use crate::hevc::pel::Pel;
    use std::arch::x86_64::*;
    use std::ptr::{read_unaligned, write_unaligned};

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn load16<P: Pel>(p: *const P) -> __m256i {
        unsafe {
            if P::BYTES == 1 {
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p as *const __m128i))
            } else {
                _mm256_loadu_si256(p as *const __m256i)
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn load8<P: Pel>(p: *const P) -> __m128i {
        unsafe {
            if P::BYTES == 1 {
                _mm_cvtepu8_epi16(_mm_loadl_epi64(p as *const __m128i))
            } else {
                _mm_loadu_si128(p as *const __m128i)
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn load4<P: Pel>(p: *const P) -> __m128i {
        unsafe {
            if P::BYTES == 1 {
                _mm_cvtepu8_epi16(_mm_cvtsi32_si128(read_unaligned(p as *const i32)))
            } else {
                _mm_loadl_epi64(p as *const __m128i)
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn load2<P: Pel>(p: *const P) -> __m128i {
        unsafe {
            if P::BYTES == 1 {
                _mm_cvtepu8_epi16(_mm_cvtsi32_si128(read_unaligned(p as *const u16) as i32))
            } else {
                _mm_cvtsi32_si128(read_unaligned(p as *const i32))
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn gather<P: Pel, const N: usize>(at: impl Fn(usize) -> *const P) -> __m256i {
        unsafe {
            match N {
                16 => load16(at(0)),
                8 => _mm256_set_m128i(load8(at(1)), load8(at(0))),
                4 => {
                    let a = _mm_unpacklo_epi64(load4(at(0)), load4(at(1)));
                    let b = _mm_unpacklo_epi64(load4(at(2)), load4(at(3)));
                    _mm256_set_m128i(b, a)
                }
                _ => {
                    let q = |i: usize| _mm_unpacklo_epi32(load2(at(i)), load2(at(i + 1)));
                    let a = _mm_unpacklo_epi64(q(0), q(2));
                    let b = _mm_unpacklo_epi64(q(4), q(6));
                    _mm256_set_m128i(b, a)
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn scatter_i16<const N: usize>(
        v: __m256i,
        rows: usize,
        at: impl Fn(usize) -> *mut i16,
    ) {
        unsafe {
            let lo = _mm256_castsi256_si128(v);
            let hi = _mm256_extracti128_si256::<1>(v);
            match N {
                16 => _mm256_storeu_si256(at(0) as *mut __m256i, v),
                8 => {
                    _mm_storeu_si128(at(0) as *mut __m128i, lo);
                    if rows > 1 {
                        _mm_storeu_si128(at(1) as *mut __m128i, hi);
                    }
                }
                4 => {
                    let q = [
                        lo,
                        _mm_unpackhi_epi64(lo, lo),
                        hi,
                        _mm_unpackhi_epi64(hi, hi),
                    ];
                    for (i, &q) in q.iter().enumerate().take(rows) {
                        _mm_storel_epi64(at(i) as *mut __m128i, q);
                    }
                }
                _ => {
                    for i in 0..rows.min(8) {
                        let h = if i < 4 { lo } else { hi };
                        let d = match i & 3 {
                            0 => _mm_cvtsi128_si32(h),
                            1 => _mm_extract_epi32::<1>(h),
                            2 => _mm_extract_epi32::<2>(h),
                            _ => _mm_extract_epi32::<3>(h),
                        };
                        write_unaligned(at(i) as *mut i32, d);
                    }
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn scatter_pel<P: Pel, const N: usize>(
        v: __m256i,
        rows: usize,
        at: impl Fn(usize) -> *mut P,
    ) {
        unsafe {
            if P::BYTES == 2 {
                return scatter_i16::<N>(v, rows, |i| at(i) as *mut i16);
            }
            let b = _mm256_permute4x64_epi64::<0b1000>(_mm256_packus_epi16(v, v));
            let b = _mm256_castsi256_si128(b);
            match N {
                16 => _mm_storeu_si128(at(0) as *mut __m128i, b),
                8 => {
                    _mm_storel_epi64(at(0) as *mut __m128i, b);
                    if rows > 1 {
                        _mm_storel_epi64(at(1) as *mut __m128i, _mm_unpackhi_epi64(b, b));
                    }
                }
                4 => {
                    for i in 0..rows.min(4) {
                        let d = match i {
                            0 => _mm_cvtsi128_si32(b),
                            1 => _mm_extract_epi32::<1>(b),
                            2 => _mm_extract_epi32::<2>(b),
                            _ => _mm_extract_epi32::<3>(b),
                        };
                        write_unaligned(at(i) as *mut i32, d);
                    }
                }
                _ => {
                    for i in 0..rows.min(8) {
                        let d = match i {
                            0 => _mm_extract_epi16::<0>(b),
                            1 => _mm_extract_epi16::<1>(b),
                            2 => _mm_extract_epi16::<2>(b),
                            3 => _mm_extract_epi16::<3>(b),
                            4 => _mm_extract_epi16::<4>(b),
                            5 => _mm_extract_epi16::<5>(b),
                            6 => _mm_extract_epi16::<6>(b),
                            _ => _mm_extract_epi16::<7>(b),
                        };
                        write_unaligned(at(i) as *mut u16, d as u16);
                    }
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    pub unsafe fn pack_trunc(lo: __m256i, hi: __m256i) -> __m256i {
        let m = _mm256_set1_epi32(0xffff);
        _mm256_packus_epi32(_mm256_and_si256(lo, m), _mm256_and_si256(hi, m))
    }

    pub fn chunks(w: usize, mut f: impl FnMut(usize, usize)) {
        let mut j = 0;
        while j < w {
            let n = match w - j {
                16.. => 16,
                8.. => 8,
                4.. => 4,
                _ => 2,
            };
            f(j, n);
            j += n;
        }
    }
}
