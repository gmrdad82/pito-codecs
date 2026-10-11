use super::lane::{Isa, Vector};
use super::net::Lane;
use std::arch::x86_64::*;

#[derive(Clone, Copy)]
pub struct X16(__m256i);

#[derive(Clone, Copy)]
pub struct X8(__m128i);

#[derive(Clone, Copy)]
pub struct X4(__m128i);

#[inline(always)]
fn pair(c0: i32, c1: i32) -> i32 {
    (c0 as u16 as u32 | ((c1 as u16 as u32) << 16)) as i32
}

macro_rules! lane_ops {
    ($t:ident, $v:ty, $w:ty, $add:ident, $sub:ident, $zero:ident, $lo:ident, $hi:ident, $madd:ident, $set32:ident, $set16:ident, $add32:ident, $sub32:ident, $srai32:ident, $packs:ident, $mulhrs:ident) => {
        impl Lane for $t {
            type W = ($v, $v);

            #[inline(always)]
            fn add(a: Self, b: Self) -> Self {
                unsafe { $t($add(a.0, b.0)) }
            }

            #[inline(always)]
            fn sub(a: Self, b: Self) -> Self {
                unsafe { $t($sub(a.0, b.0)) }
            }

            #[inline(always)]
            fn neg(a: Self) -> Self {
                unsafe { $t($sub($zero(), a.0)) }
            }

            #[inline(always)]
            fn rot(a: Self, b: Self, c0: i32, c1: i32) -> Self {
                unsafe {
                    let k = $set32(pair(c0, c1));
                    let lo = $madd($lo(a.0, b.0), k);
                    let hi = $madd($hi(a.0, b.0), k);
                    let r = $set32(1 << 13);
                    $t($packs(
                        $srai32::<14>($add32(lo, r)),
                        $srai32::<14>($add32(hi, r)),
                    ))
                }
            }

            #[inline(always)]
            fn rot4(
                a: Self,
                b: Self,
                c: Self,
                d: Self,
                c0: i32,
                c1: i32,
                c2: i32,
                c3: i32,
            ) -> Self {
                unsafe {
                    let k0 = $set32(pair(c0, c1));
                    let k1 = $set32(pair(c2, c3));
                    let lo = $add32($madd($lo(a.0, b.0), k0), $madd($lo(c.0, d.0), k1));
                    let hi = $add32($madd($hi(a.0, b.0), k0), $madd($hi(c.0, d.0), k1));
                    let r = $set32(1 << 13);
                    $t($packs(
                        $srai32::<14>($add32(lo, r)),
                        $srai32::<14>($add32(hi, r)),
                    ))
                }
            }

            #[inline(always)]
            fn mulr(a: Self, c: i32) -> Self {
                unsafe { $t($mulhrs(a.0, $set16((2 * c) as i16))) }
            }

            #[inline(always)]
            fn prod(a: Self, b: Self, c0: i32, c1: i32) -> ($v, $v) {
                unsafe {
                    let k = $set32(pair(c0, c1));
                    ($madd($lo(a.0, b.0), k), $madd($hi(a.0, b.0), k))
                }
            }

            #[inline(always)]
            fn rsum(a: ($v, $v), b: ($v, $v)) -> Self {
                unsafe {
                    let r = $set32(1 << 13);
                    let lo = $srai32::<14>($add32($add32(a.0, b.0), r));
                    let hi = $srai32::<14>($add32($add32(a.1, b.1), r));
                    $t($packs(lo, hi))
                }
            }

            #[inline(always)]
            fn rdiff(a: ($v, $v), b: ($v, $v)) -> Self {
                unsafe {
                    let r = $set32(1 << 13);
                    let lo = $srai32::<14>($add32($sub32(a.0, b.0), r));
                    let hi = $srai32::<14>($add32($sub32(a.1, b.1), r));
                    $t($packs(lo, hi))
                }
            }
        }
    };
}

lane_ops!(
    X16,
    __m256i,
    (),
    _mm256_add_epi16,
    _mm256_sub_epi16,
    _mm256_setzero_si256,
    _mm256_unpacklo_epi16,
    _mm256_unpackhi_epi16,
    _mm256_madd_epi16,
    _mm256_set1_epi32,
    _mm256_set1_epi16,
    _mm256_add_epi32,
    _mm256_sub_epi32,
    _mm256_srai_epi32,
    _mm256_packs_epi32,
    _mm256_mulhrs_epi16
);
lane_ops!(
    X8,
    __m128i,
    (),
    _mm_add_epi16,
    _mm_sub_epi16,
    _mm_setzero_si128,
    _mm_unpacklo_epi16,
    _mm_unpackhi_epi16,
    _mm_madd_epi16,
    _mm_set1_epi32,
    _mm_set1_epi16,
    _mm_add_epi32,
    _mm_sub_epi32,
    _mm_srai_epi32,
    _mm_packs_epi32,
    _mm_mulhrs_epi16
);
lane_ops!(
    X4,
    __m128i,
    (),
    _mm_add_epi16,
    _mm_sub_epi16,
    _mm_setzero_si128,
    _mm_unpacklo_epi16,
    _mm_unpackhi_epi16,
    _mm_madd_epi16,
    _mm_set1_epi32,
    _mm_set1_epi16,
    _mm_add_epi32,
    _mm_sub_epi32,
    _mm_srai_epi32,
    _mm_packs_epi32,
    _mm_mulhrs_epi16
);

impl Vector for X16 {
    const LANES: usize = 16;

    #[inline(always)]
    fn zero() -> Self {
        unsafe { X16(_mm256_setzero_si256()) }
    }

    #[inline(always)]
    fn load(p: &[i16]) -> Self {
        let p = &p[..16];
        unsafe { X16(_mm256_loadu_si256(p.as_ptr().cast())) }
    }

    #[inline(always)]
    fn store(self, p: &mut [i16]) {
        let p = &mut p[..16];
        unsafe { _mm256_storeu_si256(p.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn shl<const S: i32>(self) -> Self {
        unsafe { X16(_mm256_slli_epi16::<S>(self.0)) }
    }

    #[inline(always)]
    fn rshr<const S: i32>(self) -> Self {
        unsafe {
            X16(_mm256_mulhrs_epi16(
                self.0,
                _mm256_set1_epi16((1 << (15 - S)) as i16),
            ))
        }
    }

    #[inline(always)]
    fn add_to(self, dst: &mut [u8]) {
        let d = &mut dst[..16];
        unsafe {
            let p = _mm256_cvtepu8_epi16(_mm_loadu_si128(d.as_ptr().cast()));
            let s = _mm256_adds_epi16(p, self.0);
            let packed = _mm256_packus_epi16(s, s);
            let r = _mm256_permute4x64_epi64::<0b1000>(packed);
            _mm_storeu_si128(d.as_mut_ptr().cast(), _mm256_castsi256_si128(r));
        }
    }
}

impl Vector for X8 {
    const LANES: usize = 8;

    #[inline(always)]
    fn zero() -> Self {
        unsafe { X8(_mm_setzero_si128()) }
    }

    #[inline(always)]
    fn load(p: &[i16]) -> Self {
        let p = &p[..8];
        unsafe { X8(_mm_loadu_si128(p.as_ptr().cast())) }
    }

    #[inline(always)]
    fn store(self, p: &mut [i16]) {
        let p = &mut p[..8];
        unsafe { _mm_storeu_si128(p.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn shl<const S: i32>(self) -> Self {
        unsafe { X8(_mm_slli_epi16::<S>(self.0)) }
    }

    #[inline(always)]
    fn rshr<const S: i32>(self) -> Self {
        unsafe {
            X8(_mm_mulhrs_epi16(
                self.0,
                _mm_set1_epi16((1 << (15 - S)) as i16),
            ))
        }
    }

    #[inline(always)]
    fn add_to(self, dst: &mut [u8]) {
        let d = &mut dst[..8];
        unsafe {
            let p = _mm_cvtepu8_epi16(_mm_loadl_epi64(d.as_ptr().cast()));
            let s = _mm_adds_epi16(p, self.0);
            _mm_storel_epi64(d.as_mut_ptr().cast(), _mm_packus_epi16(s, s));
        }
    }
}

impl Vector for X4 {
    const LANES: usize = 4;

    #[inline(always)]
    fn zero() -> Self {
        unsafe { X4(_mm_setzero_si128()) }
    }

    #[inline(always)]
    fn load(p: &[i16]) -> Self {
        let p = &p[..4];
        unsafe { X4(_mm_loadl_epi64(p.as_ptr().cast())) }
    }

    #[inline(always)]
    fn store(self, p: &mut [i16]) {
        let p = &mut p[..4];
        unsafe { _mm_storel_epi64(p.as_mut_ptr().cast(), self.0) }
    }

    #[inline(always)]
    fn shl<const S: i32>(self) -> Self {
        unsafe { X4(_mm_slli_epi16::<S>(self.0)) }
    }

    #[inline(always)]
    fn rshr<const S: i32>(self) -> Self {
        unsafe {
            X4(_mm_mulhrs_epi16(
                self.0,
                _mm_set1_epi16((1 << (15 - S)) as i16),
            ))
        }
    }

    #[inline(always)]
    fn add_to(self, dst: &mut [u8]) {
        let d = &mut dst[..4];
        unsafe {
            let p = _mm_cvtepu8_epi16(_mm_cvtsi32_si128(i32::from_le_bytes([
                d[0], d[1], d[2], d[3],
            ])));
            let s = _mm_adds_epi16(p, self.0);
            let v = _mm_cvtsi128_si32(_mm_packus_epi16(s, s)).to_le_bytes();
            d.copy_from_slice(&v);
        }
    }
}

#[inline(always)]
unsafe fn t8(a: [__m128i; 8]) -> [__m128i; 8] {
    unsafe {
        let t0 = _mm_unpacklo_epi16(a[0], a[1]);
        let t1 = _mm_unpackhi_epi16(a[0], a[1]);
        let t2 = _mm_unpacklo_epi16(a[2], a[3]);
        let t3 = _mm_unpackhi_epi16(a[2], a[3]);
        let t4 = _mm_unpacklo_epi16(a[4], a[5]);
        let t5 = _mm_unpackhi_epi16(a[4], a[5]);
        let t6 = _mm_unpacklo_epi16(a[6], a[7]);
        let t7 = _mm_unpackhi_epi16(a[6], a[7]);
        let u0 = _mm_unpacklo_epi32(t0, t2);
        let u1 = _mm_unpackhi_epi32(t0, t2);
        let u2 = _mm_unpacklo_epi32(t1, t3);
        let u3 = _mm_unpackhi_epi32(t1, t3);
        let u4 = _mm_unpacklo_epi32(t4, t6);
        let u5 = _mm_unpackhi_epi32(t4, t6);
        let u6 = _mm_unpacklo_epi32(t5, t7);
        let u7 = _mm_unpackhi_epi32(t5, t7);
        [
            _mm_unpacklo_epi64(u0, u4),
            _mm_unpackhi_epi64(u0, u4),
            _mm_unpacklo_epi64(u1, u5),
            _mm_unpackhi_epi64(u1, u5),
            _mm_unpacklo_epi64(u2, u6),
            _mm_unpackhi_epi64(u2, u6),
            _mm_unpacklo_epi64(u3, u7),
            _mm_unpackhi_epi64(u3, u7),
        ]
    }
}

#[inline(always)]
fn load_tile(buf: &[i16], n: usize, bi: usize, bj: usize) -> [__m128i; 8] {
    unsafe {
        let mut t = [_mm_setzero_si128(); 8];
        for (r, v) in t.iter_mut().enumerate() {
            let p = &buf[(bi * 8 + r) * n + bj * 8..][..8];
            *v = _mm_loadu_si128(p.as_ptr().cast());
        }
        t
    }
}

#[inline(always)]
fn store_tile(buf: &mut [i16], n: usize, bi: usize, bj: usize, t: [__m128i; 8]) {
    for (r, v) in t.into_iter().enumerate() {
        let p = &mut buf[(bi * 8 + r) * n + bj * 8..][..8];
        unsafe { _mm_storeu_si128(p.as_mut_ptr().cast(), v) }
    }
}

pub struct Avx2;

impl Isa for Avx2 {
    type V4 = X4;
    type V8 = X8;
    type V16 = X16;

    super::xform::kernels!(Avx2, X4, X8, X16, #[target_feature(enable = "avx2,fma")]);

    #[inline(always)]
    fn transpose(buf: &mut [i16], n: usize) {
        if n == 4 {
            let b = &mut buf[..16];
            unsafe {
                let a = [
                    _mm_loadl_epi64(b.as_ptr().cast()),
                    _mm_loadl_epi64(b[4..].as_ptr().cast()),
                    _mm_loadl_epi64(b[8..].as_ptr().cast()),
                    _mm_loadl_epi64(b[12..].as_ptr().cast()),
                ];
                let t0 = _mm_unpacklo_epi16(a[0], a[1]);
                let t1 = _mm_unpacklo_epi16(a[2], a[3]);
                let u0 = _mm_unpacklo_epi32(t0, t1);
                let u1 = _mm_unpackhi_epi32(t0, t1);
                _mm_storeu_si128(b.as_mut_ptr().cast(), u0);
                _mm_storeu_si128(b[8..].as_mut_ptr().cast(), u1);
            }
            return;
        }
        let tiles = n / 8;
        for bi in 0..tiles {
            let d = load_tile(buf, n, bi, bi);
            store_tile(buf, n, bi, bi, unsafe { t8(d) });
            for bj in bi + 1..tiles {
                let a = load_tile(buf, n, bi, bj);
                let b = load_tile(buf, n, bj, bi);
                store_tile(buf, n, bj, bi, unsafe { t8(a) });
                store_tile(buf, n, bi, bj, unsafe { t8(b) });
            }
        }
    }
}
