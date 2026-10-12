use crate::vp9::frame::Pixel;
use crate::vp9::tables::{ADST_ADST, ADST_DCT, DCT_ADST, DCT_DCT};
use crate::vp9::transform_gen::{narrow, wide};

const SINPI_1_9: i64 = 5283;
const SINPI_2_9: i64 = 9929;
const SINPI_3_9: i64 = 13377;
const SINPI_4_9: i64 = 15212;

macro_rules! adst4 {
    ($name:ident, $t:ty) => {
        #[inline(always)]
        fn $name(x: [$t; 4]) -> [$t; 4] {
            let (s1_9, s2_9, s3_9, s4_9) = (
                SINPI_1_9 as $t,
                SINPI_2_9 as $t,
                SINPI_3_9 as $t,
                SINPI_4_9 as $t,
            );
            let s0 = s1_9.wrapping_mul(x[0]);
            let s1 = s2_9.wrapping_mul(x[0]);
            let s2 = s3_9.wrapping_mul(x[1]);
            let s3 = s4_9.wrapping_mul(x[2]);
            let s4 = s1_9.wrapping_mul(x[2]);
            let s5 = s2_9.wrapping_mul(x[3]);
            let s6 = s4_9.wrapping_mul(x[3]);
            let v = x[0].wrapping_sub(x[2]).wrapping_add(x[3]);
            let s7 = s3_9.wrapping_mul(v);
            let x0 = s0.wrapping_add(s3).wrapping_add(s5);
            let x1 = s1.wrapping_sub(s4).wrapping_sub(s6);
            let x2 = s7;
            let x3 = s2;
            let r = |v: $t| v.wrapping_add(1 << 13) >> 14;
            [
                r(x0.wrapping_add(x3)),
                r(x1.wrapping_add(x3)),
                r(x2),
                r(x0.wrapping_add(x1).wrapping_sub(x3)),
            ]
        }
    };
}
adst4!(iadst4_narrow, i32);
adst4!(iadst4_wide, i64);

pub(crate) fn iwht4(x: [i32; 4], shift: u32) -> [i32; 4] {
    let mut a = x[0] >> shift;
    let mut c = x[1] >> shift;
    let mut d = x[2] >> shift;
    let mut b = x[3] >> shift;
    a = a.wrapping_add(c);
    d = d.wrapping_sub(b);
    let e = a.wrapping_sub(d) >> 1;
    b = e.wrapping_sub(b);
    c = e.wrapping_sub(c);
    a = a.wrapping_sub(b);
    d = d.wrapping_add(c);
    [a, b, c, d]
}

#[inline(always)]
fn tx1d_narrow(buf: &mut [i32], n: usize, adst: bool) {
    match (n, adst) {
        (4, false) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = narrow::idct4(*a);
            }
        }
        (4, true) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = iadst4_narrow(*a);
            }
        }
        (8, false) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = narrow::idct8(*a);
            }
        }
        (8, true) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = narrow::iadst8(*a);
            }
        }
        (16, false) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = narrow::idct16(*a);
            }
        }
        (16, true) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = narrow::iadst16(*a);
            }
        }
        _ => {
            if let Some(a) = buf.first_chunk_mut::<32>() {
                *a = narrow::idct32(*a);
            }
        }
    }
}

#[inline(always)]
fn tx1d_wide(buf: &mut [i64], n: usize, adst: bool) {
    match (n, adst) {
        (4, false) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = wide::idct4(*a);
            }
        }
        (4, true) => {
            if let Some(a) = buf.first_chunk_mut::<4>() {
                *a = iadst4_wide(*a);
            }
        }
        (8, false) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = wide::idct8(*a);
            }
        }
        (8, true) => {
            if let Some(a) = buf.first_chunk_mut::<8>() {
                *a = wide::iadst8(*a);
            }
        }
        (16, false) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = wide::idct16(*a);
            }
        }
        (16, true) => {
            if let Some(a) = buf.first_chunk_mut::<16>() {
                *a = wide::iadst16(*a);
            }
        }
        _ => {
            if let Some(a) = buf.first_chunk_mut::<32>() {
                *a = wide::idct32(*a);
            }
        }
    }
}

#[inline]
fn sat(v: i32, lim: i32) -> i32 {
    v.clamp(-lim, lim)
}

crate::media::isa::dispatch! {
#[allow(clippy::too_many_arguments)]
pub fn inverse_transform_add<P: Pixel>(
    coefs: &[i32],
    tx_size: u8,
    tx_type: u8,
    lossless: bool,
    eob: usize,
    rows: usize,
    bit_depth: u8,
    dst: &mut [P],
    stride: usize,
) {
    let n = 4usize << tx_size;
    let max = (1i32 << bit_depth) - 1;
    if lossless {
        let mut t = [0i32; 16];
        for i in 0..4 {
            let c = coefs
                .get(i * 4..)
                .and_then(|c| c.first_chunk::<4>())
                .copied()
                .unwrap_or_default();
            let r = iwht4(c, 2);
            t[i * 4..i * 4 + 4].copy_from_slice(&r);
        }
        for j in 0..4 {
            let r = iwht4([t[j], t[4 + j], t[8 + j], t[12 + j]], 0);
            for i in 0..4 {
                let p = &mut dst[i * stride + j];
                *p = P::put(p.get().saturating_add(r[i]).clamp(0, max));
            }
        }
        return;
    }
    let shift = (tx_size as u32 + 4).min(6);
    let round = 1i32 << (shift - 1);
    if eob == 1 && tx_type == DCT_DCT {
        let dc = coefs[0] as i64;
        let a = (dc * 11585 + (1 << 13)) >> 14;
        let b = (a * 11585 + (1 << 13)) >> 14;
        let v = ((b + round as i64) >> shift).clamp(-(1 << 20), 1 << 20) as i32;
        #[cfg(target_arch = "x86_64")]
        if crate::media::isa::avx2() && dst.len() >= (n - 1) * stride + n {
            unsafe { simd::dc_add(n, v, max, dst, stride) };
            return;
        }
        for i in 0..n {
            for p in dst[i * stride..i * stride + n].iter_mut() {
                *p = P::put((p.get() + v).clamp(0, max));
            }
        }
        return;
    }
    let row_adst = matches!(tx_type, DCT_ADST | ADST_ADST);
    let col_adst = matches!(tx_type, ADST_DCT | ADST_ADST);
    let p = TxParams {
        row_adst,
        col_adst,
        rows,
        shift,
        round,
        max,
    };
    #[cfg(target_arch = "x86_64")]
    if bit_depth == 8
        && crate::media::isa::avx2()
        && coefs.len() >= n * n
        && dst.len() >= (n - 1) * stride + n
    {
        unsafe { simd::itx::<P, true>(coefs, tx_size, &p, dst, stride) };
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if bit_depth > 8
        && crate::media::isa::avx2()
        && coefs.len() >= n * n
        && dst.len() >= (n - 1) * stride + n
        && unsafe { simd::l1(&coefs[..rows.clamp(1, n) * n]) } <= WIDE_L1
    {
        unsafe { simd::itx::<P, false>(coefs, tx_size, &p, dst, stride) };
        return;
    }
    match (tx_size, bit_depth == 8) {
        (0, true) => itx_narrow::<P, 4>(coefs, &p, dst, stride),
        (1, true) => itx_narrow::<P, 8>(coefs, &p, dst, stride),
        (2, true) => itx_narrow::<P, 16>(coefs, &p, dst, stride),
        (_, true) => itx_narrow::<P, 32>(coefs, &p, dst, stride),
        (0, false) => itx_wide::<P, 4>(coefs, &p, dst, stride),
        (1, false) => itx_wide::<P, 8>(coefs, &p, dst, stride),
        (2, false) => itx_wide::<P, 16>(coefs, &p, dst, stride),
        (_, false) => itx_wide::<P, 32>(coefs, &p, dst, stride),
    }
}
}

#[cfg(any(test, target_arch = "x86_64"))]
const WIDE_L1: u64 = 130_000;

struct TxParams {
    row_adst: bool,
    col_adst: bool,
    rows: usize,
    shift: u32,
    round: i32,
    max: i32,
}

fn itx_narrow<P: Pixel, const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [P], stride: usize) {
    let lim = 1 << 24;
    let mut t = [[0i32; N]; N];
    for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
        for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
            *d = sat(*s, lim);
        }
        tx1d_narrow(row, N, p.row_adst);
        for v in row.iter_mut() {
            *v = sat(*v, lim);
        }
    }
    if N == 4 {
        let x: [Cols<4>; N] =
            std::array::from_fn(|i| Cols(t[i].first_chunk::<4>().copied().unwrap_or_default()));
        let y = col_tx4(x, p.col_adst);
        add_cols(&y, 0, p, dst, stride);
    } else {
        for c0 in (0..N).step_by(8) {
            let x: [Cols<8>; N] = std::array::from_fn(|i| {
                Cols(
                    t[i].get(c0..)
                        .and_then(|r| r.first_chunk::<8>())
                        .copied()
                        .unwrap_or_default(),
                )
            });
            let y = col_tx8(x, p.col_adst);
            add_cols(&y, c0, p, dst, stride);
        }
    }
}

#[inline(always)]
fn add_cols<P: Pixel, const N: usize, const W: usize>(
    y: &[Cols<W>; N],
    c0: usize,
    p: &TxParams,
    dst: &mut [P],
    stride: usize,
) {
    for (i, v) in y.iter().enumerate() {
        let Some(d) = dst
            .get_mut(i * stride + c0..)
            .and_then(|d| d.first_chunk_mut::<W>())
        else {
            return;
        };
        for j in 0..W {
            let r = (v.0[j].wrapping_add(p.round) >> p.shift).clamp(-(1 << 20), 1 << 20);
            d[j] = P::put((d[j].get() + r).max(0).min(p.max));
        }
    }
}

#[derive(Clone, Copy)]
pub struct Cols<const W: usize>(pub [i32; W]);

impl<const W: usize> From<i32> for Cols<W> {
    #[inline(always)]
    fn from(v: i32) -> Self {
        Cols([v; W])
    }
}

impl<const W: usize> Cols<W> {
    #[inline(always)]
    pub fn wrapping_add<O: Into<Self>>(self, o: O) -> Self {
        let o = o.into();
        Cols(std::array::from_fn(|i| self.0[i].wrapping_add(o.0[i])))
    }
    #[inline(always)]
    pub fn wrapping_sub(self, o: Self) -> Self {
        Cols(std::array::from_fn(|i| self.0[i].wrapping_sub(o.0[i])))
    }
    #[inline(always)]
    pub fn wrapping_mul(self, c: i32) -> Self {
        Cols(self.0.map(|v| v.wrapping_mul(c)))
    }
    #[inline(always)]
    pub fn wrapping_neg(self) -> Self {
        Cols(self.0.map(|v| v.wrapping_neg()))
    }
}

impl<const W: usize> std::ops::Shr<i32> for Cols<W> {
    type Output = Self;
    #[inline(always)]
    fn shr(self, s: i32) -> Self {
        Cols(self.0.map(|v| v >> s))
    }
}

#[inline(always)]
fn col_tx4<const N: usize>(x: [Cols<4>; N], adst: bool) -> [Cols<4>; N] {
    use crate::vp9::transform_gen::cols4;
    let r = if adst {
        iadst4_cols(fixed(&x))
    } else {
        cols4::idct4(fixed(&x))
    };
    std::array::from_fn(|i| r[i])
}

#[inline(always)]
fn col_tx8<const N: usize>(x: [Cols<8>; N], adst: bool) -> [Cols<8>; N] {
    use crate::vp9::transform_gen::cols8;
    let mut out = x;
    match (N, adst) {
        (8, false) => out.copy_from_slice(&cols8::idct8(fixed(&x))),
        (8, true) => out.copy_from_slice(&cols8::iadst8(fixed(&x))),
        (16, false) => out.copy_from_slice(&cols8::idct16(fixed(&x))),
        (16, true) => out.copy_from_slice(&cols8::iadst16(fixed(&x))),
        _ => out.copy_from_slice(&cols8::idct32(fixed(&x))),
    }
    out
}

#[inline(always)]
fn fixed<T: Copy + From<i32>, const M: usize>(x: &[T]) -> [T; M] {
    x.first_chunk::<M>().copied().unwrap_or([T::from(0); M])
}

macro_rules! adst4_lanes {
    ($name:ident, $v:ty) => {
        #[inline(always)]
        fn $name(x: [$v; 4]) -> [$v; 4] {
            let s0 = x[0].wrapping_mul(SINPI_1_9 as i32);
            let s1 = x[0].wrapping_mul(SINPI_2_9 as i32);
            let s2 = x[1].wrapping_mul(SINPI_3_9 as i32);
            let s3 = x[2].wrapping_mul(SINPI_4_9 as i32);
            let s4 = x[2].wrapping_mul(SINPI_1_9 as i32);
            let s5 = x[3].wrapping_mul(SINPI_2_9 as i32);
            let s6 = x[3].wrapping_mul(SINPI_4_9 as i32);
            let v = x[0].wrapping_sub(x[2]).wrapping_add(x[3]);
            let s7 = v.wrapping_mul(SINPI_3_9 as i32);
            let x0 = s0.wrapping_add(s3).wrapping_add(s5);
            let x1 = s1.wrapping_sub(s4).wrapping_sub(s6);
            let x2 = s7;
            let x3 = s2;
            let r = |v: $v| v.wrapping_add(1 << 13) >> 14;
            [
                r(x0.wrapping_add(x3)),
                r(x1.wrapping_add(x3)),
                r(x2),
                r(x0.wrapping_add(x1).wrapping_sub(x3)),
            ]
        }
    };
}
adst4_lanes!(iadst4_cols, Cols<4>);

fn itx_wide<P: Pixel, const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [P], stride: usize) {
    let lim = 1i64 << 40;
    let mut t = [[0i64; N]; N];
    for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
        for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
            *d = *s as i64;
        }
        tx1d_wide(row, N, p.row_adst);
        for v in row.iter_mut() {
            *v = (*v).clamp(-lim, lim);
        }
    }
    let mut col = [0i64; N];
    for j in 0..N {
        for i in 0..N {
            col[i] = t[i][j];
        }
        tx1d_wide(&mut col, N, p.col_adst);
        for i in 0..N {
            let d = &mut dst[i * stride + j];
            let r = ((col[i].clamp(-lim, lim) + p.round as i64) >> p.shift)
                .clamp(-(1 << 30), 1 << 30) as i32;
            *d = P::put((d.get() + r).clamp(0, p.max));
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub(crate) mod simd {
    use super::{SINPI_1_9, SINPI_2_9, SINPI_3_9, SINPI_4_9, TxParams, fixed};
    use crate::vp9::frame::Pixel;
    use crate::vp9::transform_gen::{v4, v8};
    use std::arch::x86_64::*;

    macro_rules! lanes {
        ($v:ident, $m:ty, $set1:ident, $add:ident, $sub:ident, $mul:ident, $sra:ident, $zero:ident) => {
            #[derive(Clone, Copy)]
            pub struct $v(pub $m);

            impl From<i32> for $v {
                #[inline(always)]
                fn from(x: i32) -> Self {
                    $v(unsafe { $set1(x) })
                }
            }

            impl $v {
                #[inline(always)]
                pub fn wrapping_add<O: Into<Self>>(self, o: O) -> Self {
                    $v(unsafe { $add(self.0, o.into().0) })
                }
                #[inline(always)]
                pub fn wrapping_sub(self, o: Self) -> Self {
                    $v(unsafe { $sub(self.0, o.0) })
                }
                #[inline(always)]
                pub fn wrapping_mul(self, c: i32) -> Self {
                    $v(unsafe { $mul(self.0, $set1(c)) })
                }
                #[inline(always)]
                pub fn wrapping_neg(self) -> Self {
                    $v(unsafe { $sub($zero(), self.0) })
                }
            }

            impl std::ops::Shr<i32> for $v {
                type Output = Self;
                #[inline(always)]
                fn shr(self, s: i32) -> Self {
                    $v(unsafe { $sra(self.0, _mm_cvtsi32_si128(s)) })
                }
            }
        };
    }

    lanes!(
        V4,
        __m128i,
        _mm_set1_epi32,
        _mm_add_epi32,
        _mm_sub_epi32,
        _mm_mullo_epi32,
        _mm_sra_epi32,
        _mm_setzero_si128
    );
    lanes!(
        V8,
        __m256i,
        _mm256_set1_epi32,
        _mm256_add_epi32,
        _mm256_sub_epi32,
        _mm256_mullo_epi32,
        _mm256_sra_epi32,
        _mm256_setzero_si256
    );

    adst4_lanes!(iadst4_v4, V4);

    const SAT: i32 = 1 << 24;

    #[inline(always)]
    fn load4(s: &[i32]) -> V4 {
        let s = &s[..4];
        V4(unsafe { _mm_loadu_si128(s.as_ptr().cast()) })
    }

    #[inline(always)]
    fn load8(s: &[i32]) -> V8 {
        let s = &s[..8];
        V8(unsafe { _mm256_loadu_si256(s.as_ptr().cast()) })
    }

    #[inline(always)]
    fn sat4(v: V4) -> V4 {
        unsafe {
            V4(_mm_min_epi32(
                _mm_max_epi32(v.0, _mm_set1_epi32(-SAT)),
                _mm_set1_epi32(SAT),
            ))
        }
    }

    #[inline(always)]
    fn sat8(v: V8) -> V8 {
        unsafe {
            V8(_mm256_min_epi32(
                _mm256_max_epi32(v.0, _mm256_set1_epi32(-SAT)),
                _mm256_set1_epi32(SAT),
            ))
        }
    }

    #[inline(always)]
    fn transpose4(r: [V4; 4]) -> [V4; 4] {
        unsafe {
            let t0 = _mm_unpacklo_epi32(r[0].0, r[1].0);
            let t1 = _mm_unpackhi_epi32(r[0].0, r[1].0);
            let t2 = _mm_unpacklo_epi32(r[2].0, r[3].0);
            let t3 = _mm_unpackhi_epi32(r[2].0, r[3].0);
            [
                V4(_mm_unpacklo_epi64(t0, t2)),
                V4(_mm_unpackhi_epi64(t0, t2)),
                V4(_mm_unpacklo_epi64(t1, t3)),
                V4(_mm_unpackhi_epi64(t1, t3)),
            ]
        }
    }

    #[inline(always)]
    fn transpose8(r: &[V8]) -> [V8; 8] {
        let r: &[V8; 8] = r.first_chunk().expect("8 rows");
        unsafe {
            let t0 = _mm256_unpacklo_epi32(r[0].0, r[1].0);
            let t1 = _mm256_unpackhi_epi32(r[0].0, r[1].0);
            let t2 = _mm256_unpacklo_epi32(r[2].0, r[3].0);
            let t3 = _mm256_unpackhi_epi32(r[2].0, r[3].0);
            let t4 = _mm256_unpacklo_epi32(r[4].0, r[5].0);
            let t5 = _mm256_unpackhi_epi32(r[4].0, r[5].0);
            let t6 = _mm256_unpacklo_epi32(r[6].0, r[7].0);
            let t7 = _mm256_unpackhi_epi32(r[6].0, r[7].0);
            let u0 = _mm256_unpacklo_epi64(t0, t2);
            let u1 = _mm256_unpackhi_epi64(t0, t2);
            let u2 = _mm256_unpacklo_epi64(t1, t3);
            let u3 = _mm256_unpackhi_epi64(t1, t3);
            let u4 = _mm256_unpacklo_epi64(t4, t6);
            let u5 = _mm256_unpackhi_epi64(t4, t6);
            let u6 = _mm256_unpacklo_epi64(t5, t7);
            let u7 = _mm256_unpackhi_epi64(t5, t7);
            [
                V8(_mm256_permute2x128_si256::<0x20>(u0, u4)),
                V8(_mm256_permute2x128_si256::<0x20>(u1, u5)),
                V8(_mm256_permute2x128_si256::<0x20>(u2, u6)),
                V8(_mm256_permute2x128_si256::<0x20>(u3, u7)),
                V8(_mm256_permute2x128_si256::<0x31>(u0, u4)),
                V8(_mm256_permute2x128_si256::<0x31>(u1, u5)),
                V8(_mm256_permute2x128_si256::<0x31>(u2, u6)),
                V8(_mm256_permute2x128_si256::<0x31>(u3, u7)),
            ]
        }
    }

    #[inline(always)]
    fn add4<P: Pixel>(d: &mut [P], v: V4, p: &TxParams) {
        let d = &mut d[..4];
        unsafe {
            let px = if P::WIDE {
                _mm_cvtepu16_epi32(_mm_loadl_epi64(d.as_ptr().cast()))
            } else {
                _mm_cvtepu8_epi32(_mm_cvtsi32_si128(d.as_ptr().cast::<i32>().read_unaligned()))
            };
            let r = _mm_sra_epi32(
                _mm_add_epi32(v.0, _mm_set1_epi32(p.round)),
                _mm_cvtsi32_si128(p.shift as i32),
            );
            let s = _mm_min_epi32(
                _mm_max_epi32(_mm_add_epi32(px, r), _mm_setzero_si128()),
                _mm_set1_epi32(p.max),
            );
            let w = _mm_packus_epi32(s, s);
            if P::WIDE {
                _mm_storel_epi64(d.as_mut_ptr().cast(), w);
            } else {
                d.as_mut_ptr()
                    .cast::<i32>()
                    .write_unaligned(_mm_cvtsi128_si32(_mm_packus_epi16(w, w)));
            }
        }
    }

    #[inline(always)]
    fn add8<P: Pixel>(d: &mut [P], v: V8, p: &TxParams) {
        let d = &mut d[..8];
        unsafe {
            let px = if P::WIDE {
                _mm256_cvtepu16_epi32(_mm_loadu_si128(d.as_ptr().cast()))
            } else {
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(d.as_ptr().cast()))
            };
            let r = _mm256_sra_epi32(
                _mm256_add_epi32(v.0, _mm256_set1_epi32(p.round)),
                _mm_cvtsi32_si128(p.shift as i32),
            );
            let s = _mm256_min_epi32(
                _mm256_max_epi32(_mm256_add_epi32(px, r), _mm256_setzero_si256()),
                _mm256_set1_epi32(p.max),
            );
            let o = _mm_packus_epi32(_mm256_castsi256_si128(s), _mm256_extracti128_si256::<1>(s));
            if P::WIDE {
                _mm_storeu_si128(d.as_mut_ptr().cast(), o);
            } else {
                _mm_storel_epi64(d.as_mut_ptr().cast(), _mm_packus_epi16(o, o));
            }
        }
    }

    #[inline(always)]
    fn tx4(x: [V4; 4], adst: bool) -> [V4; 4] {
        if adst { iadst4_v4(x) } else { v4::idct4(x) }
    }

    #[inline(always)]
    fn tx8<const N: usize>(x: [V8; N], adst: bool) -> [V8; N] {
        let mut out = x;
        match (N, adst) {
            (8, false) => out.copy_from_slice(&v8::idct8(fixed(&x))),
            (8, true) => out.copy_from_slice(&v8::iadst8(fixed(&x))),
            (16, false) => out.copy_from_slice(&v8::idct16(fixed(&x))),
            (16, true) => out.copy_from_slice(&v8::iadst16(fixed(&x))),
            _ => out.copy_from_slice(&v8::idct32(fixed(&x))),
        }
        out
    }

    #[target_feature(enable = "avx2")]
    fn itx4<P: Pixel, const SATURATE: bool>(
        coefs: &[i32],
        p: &TxParams,
        dst: &mut [P],
        stride: usize,
    ) {
        let mut r = [V4::from(0); 4];
        for (i, v) in r.iter_mut().enumerate() {
            let c = load4(&coefs[i * 4..]);
            *v = if SATURATE { sat4(c) } else { c };
        }
        let mut y = tx4(transpose4(r), p.row_adst);
        if SATURATE {
            for v in y.iter_mut() {
                *v = sat4(*v);
            }
        }
        let z = tx4(transpose4(y), p.col_adst);
        for (i, v) in z.iter().enumerate() {
            add4(&mut dst[i * stride..], *v, p);
        }
    }

    #[target_feature(enable = "avx2")]
    fn rows8<const N: usize, const G: usize, const KC: usize, const SATURATE: bool>(
        coefs: &[i32],
        g: usize,
        adst: bool,
        tc: &mut [[V8; N]; G],
    ) {
        let mut x = [V8::from(0); N];
        let mut r = [V8::from(0); 8];
        for cg in 0..KC.min(G) {
            for (l, v) in r.iter_mut().enumerate() {
                let c = load8(&coefs[(g * 8 + l) * N + cg * 8..]);
                *v = if SATURATE { sat8(c) } else { c };
            }
            x[cg * 8..cg * 8 + 8].copy_from_slice(&transpose8(&r));
        }
        let mut y = tx8::<N>(x, adst);
        if SATURATE {
            for v in y.iter_mut() {
                *v = sat8(*v);
            }
        }
        for (cg, t) in tc.iter_mut().enumerate() {
            t[g * 8..g * 8 + 8].copy_from_slice(&transpose8(&y[cg * 8..]));
        }
    }

    #[target_feature(enable = "avx2")]
    fn cols8<P: Pixel, const N: usize, const G: usize, const KR: usize>(
        tc: &[[V8; N]; G],
        p: &TxParams,
        dst: &mut [P],
        stride: usize,
    ) {
        let k = (KR * 8).min(N);
        for (cg, t) in tc.iter().enumerate() {
            let mut x = [V8::from(0); N];
            x[..k].copy_from_slice(&t[..k]);
            let y = tx8::<N>(x, p.col_adst);
            for (i, v) in y.iter().enumerate() {
                add8(&mut dst[i * stride + cg * 8..], *v, p);
            }
        }
    }

    #[target_feature(enable = "avx2")]
    fn itx8<P: Pixel, const N: usize, const G: usize, const SATURATE: bool>(
        coefs: &[i32],
        p: &TxParams,
        dst: &mut [P],
        stride: usize,
    ) {
        let coefs = &coefs[..N * N];
        let kr = p.rows.clamp(1, N).div_ceil(8);
        let mut kc = 1;
        for cg in 1..G {
            let mut acc = _mm256_setzero_si256();
            for r in 0..kr * 8 {
                acc = _mm256_or_si256(acc, load8(&coefs[r * N + cg * 8..]).0);
            }
            if _mm256_testz_si256(acc, acc) == 0 {
                kc = cg + 1;
            }
        }
        let mut tc = [[V8::from(0); N]; G];
        for g in 0..kr {
            match kc {
                1 => rows8::<N, G, 1, SATURATE>(coefs, g, p.row_adst, &mut tc),
                2 => rows8::<N, G, 2, SATURATE>(coefs, g, p.row_adst, &mut tc),
                3 => rows8::<N, G, 3, SATURATE>(coefs, g, p.row_adst, &mut tc),
                _ => rows8::<N, G, 4, SATURATE>(coefs, g, p.row_adst, &mut tc),
            }
        }
        match kr {
            1 => cols8::<P, N, G, 1>(&tc, p, dst, stride),
            2 => cols8::<P, N, G, 2>(&tc, p, dst, stride),
            3 => cols8::<P, N, G, 3>(&tc, p, dst, stride),
            _ => cols8::<P, N, G, 4>(&tc, p, dst, stride),
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn itx<P: Pixel, const SATURATE: bool>(
        coefs: &[i32],
        tx_size: u8,
        p: &TxParams,
        dst: &mut [P],
        stride: usize,
    ) {
        match tx_size {
            0 => itx4::<P, SATURATE>(coefs, p, dst, stride),
            1 => itx8::<P, 8, 1, SATURATE>(coefs, p, dst, stride),
            2 => itx8::<P, 16, 2, SATURATE>(coefs, p, dst, stride),
            _ => itx8::<P, 32, 4, SATURATE>(coefs, p, dst, stride),
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn l1(coefs: &[i32]) -> u64 {
        let cap = _mm256_set1_epi32(1 << 24);
        let mut acc = _mm256_setzero_si256();
        let mut it = coefs.chunks_exact(8);
        for c in &mut it {
            acc = _mm256_add_epi32(acc, _mm256_min_epu32(_mm256_abs_epi32(load8(c).0), cap));
        }
        let mut lanes = [0u32; 8];
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast(), acc) };
        lanes.iter().map(|&v| v as u64).sum::<u64>()
            + it.remainder()
                .iter()
                .map(|v| v.unsigned_abs() as u64)
                .sum::<u64>()
    }

    #[target_feature(enable = "avx2")]
    pub(super) fn dc_add<P: Pixel>(n: usize, v: i32, max: i32, dst: &mut [P], stride: usize) {
        if let Some(d) = P::narrow_mut(dst) {
            dc_add8(n, v, d, stride);
            return;
        }
        let Some(dst) = P::wide_mut(dst) else {
            return;
        };
        let v = v.clamp(-max - 1, max + 1) as i16;
        let (vv, mx, z) = (
            _mm256_set1_epi16(v),
            _mm256_set1_epi16(max as i16),
            _mm256_setzero_si256(),
        );
        let px = |s: __m256i| _mm256_min_epi16(_mm256_max_epi16(_mm256_add_epi16(s, vv), z), mx);
        for i in 0..n {
            let row = &mut dst[i * stride..i * stride + n];
            unsafe {
                match n {
                    4 => {
                        let s = _mm256_castsi128_si256(_mm_loadl_epi64(row.as_ptr().cast()));
                        _mm_storel_epi64(row.as_mut_ptr().cast(), _mm256_castsi256_si128(px(s)));
                    }
                    8 => {
                        let s = _mm256_castsi128_si256(_mm_loadu_si128(row.as_ptr().cast()));
                        _mm_storeu_si128(row.as_mut_ptr().cast(), _mm256_castsi256_si128(px(s)));
                    }
                    _ => {
                        for c in row.chunks_exact_mut(16) {
                            let s = _mm256_loadu_si256(c.as_ptr().cast());
                            _mm256_storeu_si256(c.as_mut_ptr().cast(), px(s));
                        }
                    }
                }
            }
        }
    }

    #[target_feature(enable = "avx2")]
    fn dc_add8(n: usize, v: i32, dst: &mut [u8], stride: usize) {
        let m = v.unsigned_abs().min(255) as i8;
        let up = _mm256_set1_epi8(if v >= 0 { m } else { 0 });
        let down = _mm256_set1_epi8(if v < 0 { m } else { 0 });
        let px = |s: __m256i| _mm256_subs_epu8(_mm256_adds_epu8(s, up), down);
        for i in 0..n {
            let row = &mut dst[i * stride..i * stride + n];
            unsafe {
                match n {
                    4 => {
                        let s = _mm256_castsi128_si256(_mm_cvtsi32_si128(
                            row.as_ptr().cast::<i32>().read_unaligned(),
                        ));
                        row.as_mut_ptr()
                            .cast::<i32>()
                            .write_unaligned(_mm_cvtsi128_si32(_mm256_castsi256_si128(px(s))));
                    }
                    8 => {
                        let s = _mm256_castsi128_si256(_mm_loadl_epi64(row.as_ptr().cast()));
                        _mm_storel_epi64(row.as_mut_ptr().cast(), _mm256_castsi256_si128(px(s)));
                    }
                    16 => {
                        let s = _mm256_castsi128_si256(_mm_loadu_si128(row.as_ptr().cast()));
                        _mm_storeu_si128(row.as_mut_ptr().cast(), _mm256_castsi256_si128(px(s)));
                    }
                    _ => {
                        let s = _mm256_loadu_si256(row.as_ptr().cast());
                        _mm256_storeu_si256(row.as_mut_ptr().cast(), px(s));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Clone, Copy)]
    pub(crate) struct Af {
        w: [f64; 32],
        e: f64,
    }

    thread_local! {
        static SEEN: RefCell<Vec<(f64, f64, f64)>> = const { RefCell::new(Vec::new()) };
    }

    impl Af {
        fn seen(self) -> Self {
            let m = self.w.iter().fold(0f64, |a, v| a.max(v.abs()));
            let l = self.w.iter().map(|v| v.abs()).sum();
            SEEN.with(|s| s.borrow_mut().push((m, l, self.e)));
            self
        }
        fn map(self, o: Self, f: impl Fn(f64, f64) -> f64) -> Self {
            Af {
                w: std::array::from_fn(|i| f(self.w[i], o.w[i])),
                e: self.e + o.e,
            }
            .seen()
        }
        pub(crate) fn wrapping_add<O: Into<Self>>(self, o: O) -> Self {
            self.map(o.into(), |a, b| a + b)
        }
        pub(crate) fn wrapping_sub(self, o: Self) -> Self {
            self.map(o, |a, b| a - b)
        }
        pub(crate) fn wrapping_mul(self, c: i32) -> Self {
            Af {
                w: self.w.map(|v| v * c as f64),
                e: self.e * (c as f64).abs(),
            }
            .seen()
        }
        pub(crate) fn wrapping_neg(self) -> Self {
            Af {
                w: self.w.map(|v| -v),
                e: self.e,
            }
        }
    }

    impl From<i32> for Af {
        fn from(c: i32) -> Self {
            Af {
                w: [0.0; 32],
                e: (c as f64).abs(),
            }
        }
    }

    impl std::ops::Shr<i32> for Af {
        type Output = Self;
        fn shr(self, s: i32) -> Self {
            let d = (1i64 << s) as f64;
            Af {
                w: self.w.map(|v| v / d),
                e: self.e / d + 1.0,
            }
            .seen()
        }
    }

    adst4_lanes!(iadst4_af, Af);

    fn analyse(n: usize, adst: bool) -> (Vec<(f64, f64, f64)>, f64, f64) {
        use crate::vp9::transform_gen::affine;
        SEEN.with(|s| s.borrow_mut().clear());
        let x: [Af; 32] = std::array::from_fn(|k| {
            let mut w = [0.0; 32];
            w[k] = 1.0;
            Af { w, e: 0.0 }
        });
        let out: Vec<Af> = match (n, adst) {
            (4, false) => affine::idct4(fixed(&x)).to_vec(),
            (4, true) => iadst4_af(fixed(&x)).to_vec(),
            (8, false) => affine::idct8(fixed(&x)).to_vec(),
            (8, true) => affine::iadst8(fixed(&x)).to_vec(),
            (16, false) => affine::idct16(fixed(&x)).to_vec(),
            (16, true) => affine::iadst16(fixed(&x)).to_vec(),
            _ => affine::idct32(x).to_vec(),
        };
        let out: Vec<Af> = out.into_iter().map(|o| o.seen()).collect();
        let u = out
            .iter()
            .flat_map(|o| o.w.iter())
            .fold(0f64, |a, v| a.max(v.abs()));
        let e = out.iter().fold(0f64, |a, o| a.max(o.e));
        (SEEN.with(|s| s.borrow().clone()), u, e)
    }

    fn l1_bound(tx: u8, row_adst: bool, col_adst: bool) -> f64 {
        let n = 4usize << tx;
        let lim = (1u64 << 31) as f64 - 64.0;
        let (rows, u, e) = analyse(n, row_adst);
        let (cols, _, _) = analyse(n, col_adst);
        let r = rows
            .iter()
            .filter(|v| v.0 > 0.0)
            .map(|&(m, _, ev)| (lim - ev) / m);
        let c = cols
            .iter()
            .filter(|v| v.0 > 0.0)
            .map(|&(m, l, ev)| (lim - l * e - ev) / (m * u));
        r.chain(c).fold(f64::MAX, f64::min)
    }

    #[test]
    fn wide_l1_limits_keep_i32_lanes_exact() {
        for tx in 0..4u8 {
            for t in 0..4u8 {
                let (row_adst, col_adst) = (t & 1 == 1, t & 2 == 2);
                if tx == 3 && t != 0 {
                    continue;
                }
                let b = l1_bound(tx, row_adst, col_adst);
                assert!(
                    WIDE_L1 as f64 <= b * 0.999,
                    "tx {tx} type {t}: limit {WIDE_L1}, bound {b:.0}"
                );
            }
        }
    }

    fn float_idct(input: &[f64]) -> Vec<f64> {
        let n = input.len();
        (0..n)
            .map(|x| {
                let mut s = input[0] / 2f64.sqrt();
                for (k, &v) in input.iter().enumerate().skip(1) {
                    s += v
                        * ((2 * x + 1) as f64 * k as f64 * std::f64::consts::PI / (2 * n) as f64)
                            .cos();
                }
                s
            })
            .collect()
    }

    #[test]
    fn idct_matches_float_reference() {
        for n in [4usize, 8, 16, 32] {
            for k in 0..n {
                let mut buf = vec![0i32; n];
                buf[k] = 1000;
                tx1d_narrow(&mut buf, n, false);
                let mut inp = vec![0f64; n];
                inp[k] = 1000.0;
                let r = float_idct(&inp);
                for (a, b) in buf.iter().zip(&r) {
                    assert!(
                        (*a as f64 - b).abs() <= 3.0,
                        "n {n} k {k}: {buf:?} vs {r:?}"
                    );
                }
                let mut w: Vec<i64> = vec![0; n];
                w[k] = 1000;
                tx1d_wide(&mut w, n, false);
                assert_eq!(w.iter().map(|&v| v as i32).collect::<Vec<_>>(), buf);
            }
        }
    }

    #[test]
    fn adst_is_sine_basis() {
        for n in [8usize, 16] {
            for k in 0..n {
                let mut buf = vec![0i32; n];
                buf[k] = 1000;
                tx1d_narrow(&mut buf, n, true);
                let r: Vec<f64> = (0..n)
                    .map(|x| {
                        1000.0
                            * ((2 * x + 1) as f64 * (2 * k + 1) as f64 * std::f64::consts::PI
                                / (4 * n) as f64)
                                .sin()
                            * 2f64.sqrt()
                            / 2f64.sqrt()
                    })
                    .collect();
                for (a, b) in buf.iter().zip(&r) {
                    assert!(
                        (*a as f64 - b).abs() <= 4.0,
                        "n {n} k {k}: {buf:?} vs {r:?}"
                    );
                }
            }
        }
        for k in 0..4 {
            let mut buf = vec![0i32; 4];
            buf[k] = 1000;
            tx1d_narrow(&mut buf, 4, true);
            let r: Vec<f64> = (0..4)
                .map(|x| {
                    1000.0
                        * (std::f64::consts::PI * (x + 1) as f64 * (2 * k + 1) as f64 / 9.0).sin()
                        * 2.0
                        * 2f64.sqrt()
                        / 3.0
                })
                .collect();
            for (a, b) in buf.iter().zip(&r) {
                assert!((*a as f64 - b).abs() <= 3.0, "k {k}: {buf:?} vs {r:?}");
            }
        }
    }

    #[test]
    fn dc_only_shortcut_matches_full_transform() {
        for tx in 0..4u8 {
            let n = 4usize << tx;
            for dc in [-5000, -37, -1, 1, 2, 57, 1234, 9999] {
                let mut coefs = vec![0i32; n * n];
                coefs[0] = dc;
                let mut a = vec![128u16; n * n];
                let mut b = vec![128u16; n * n];
                inverse_transform_add(&coefs, tx, DCT_DCT, false, 1, 1, 8, &mut a, n);
                inverse_transform_add(&coefs, tx, DCT_DCT, false, 2, n, 8, &mut b, n);
                assert_eq!(a, b, "tx {tx} dc {dc}");
            }
        }
    }

    fn itx_narrow_ref<const N: usize>(coefs: &[i32], p: &TxParams, dst: &mut [u16], stride: usize) {
        let lim = 1 << 24;
        let mut t = [[0i32; N]; N];
        for (i, row) in t.iter_mut().enumerate().take(p.rows.min(N)) {
            for (d, s) in row.iter_mut().zip(&coefs[i * N..i * N + N]) {
                *d = sat(*s, lim);
            }
            tx1d_narrow(row, N, p.row_adst);
            for v in row.iter_mut() {
                *v = sat(*v, lim);
            }
        }
        let mut col = [0i32; N];
        for j in 0..N {
            for i in 0..N {
                col[i] = t[i][j];
            }
            tx1d_narrow(&mut col, N, p.col_adst);
            for i in 0..N {
                let d = &mut dst[i * stride + j];
                *d = (*d as i32
                    + (col[i].wrapping_add(p.round) >> p.shift).clamp(-(1 << 20), 1 << 20))
                .clamp(0, p.max) as u16;
            }
        }
    }

    fn check_cols<const N: usize>(seed: &mut u32) {
        let mut rnd = || {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 17;
            *seed ^= *seed << 5;
            *seed
        };
        let tx_size = N.trailing_zeros() - 2;
        let shift = (tx_size + 4).min(6);
        for case in 0..200 {
            let (row_adst, col_adst) = if N == 32 {
                (false, false)
            } else {
                (case & 1 == 1, case & 2 == 2)
            };
            let rows = 1 + rnd() as usize % N;
            let big = if case % 7 == 0 { 1 << 22 } else { 2000 };
            let coefs: Vec<i32> = (0..N * N)
                .map(|i| {
                    if i / N < rows && rnd() % 3 == 0 {
                        (rnd() % (2 * big)) as i32 - big as i32
                    } else {
                        0
                    }
                })
                .collect();
            let p = TxParams {
                row_adst,
                col_adst,
                rows,
                shift,
                round: 1 << (shift - 1),
                max: 255,
            };
            let stride = N + 5;
            let base: Vec<u16> = (0..N * stride).map(|_| (rnd() % 256) as u16).collect();
            #[cfg(target_arch = "x86_64")]
            let base_c = base.clone();
            let (mut a, mut b) = (base.clone(), base);
            itx_narrow::<u16, N>(&coefs, &p, &mut a, stride);
            itx_narrow_ref::<N>(&coefs, &p, &mut b, stride);
            assert_eq!(a, b, "N {N} case {case}");
            #[cfg(target_arch = "x86_64")]
            if crate::media::isa::avx2() {
                let mut c = base_c.clone();
                unsafe { simd::itx::<u16, true>(&coefs, tx_size as u8, &p, &mut c, stride) };
                assert_eq!(a, c, "simd N {N} case {case}");
                let mut wide_coefs = coefs.clone();
                let mut left = WIDE_L1 as i64;
                for (i, v) in wide_coefs.iter_mut().enumerate() {
                    if i / N >= rows {
                        *v = 0;
                        continue;
                    }
                    let lim = if case % 3 == 0 { left } else { left.min(3000) };
                    *v = (*v as i64).clamp(-lim, lim) as i32;
                    left -= (*v as i64).abs();
                }
                if case % 5 == 0 {
                    wide_coefs.fill(0);
                    let k = rnd() as usize % (rows * N);
                    wide_coefs[k] = if case % 2 == 0 {
                        WIDE_L1 as i32
                    } else {
                        -(WIDE_L1 as i32)
                    };
                }
                let pw = TxParams { max: 1023, ..p };
                let base10: Vec<u16> = base_c.iter().map(|v| v * 4).collect();
                let (mut d, mut e) = (base10.clone(), base10);
                itx_wide::<u16, N>(&wide_coefs, &pw, &mut d, stride);
                unsafe { simd::itx::<u16, false>(&wide_coefs, tx_size as u8, &pw, &mut e, stride) };
                assert_eq!(d, e, "wide simd N {N} case {case}");
            }
        }
    }

    #[test]
    fn column_pass_matches_per_column_transform() {
        let mut seed = 0x9e37_79b9u32;
        check_cols::<4>(&mut seed);
        check_cols::<8>(&mut seed);
        check_cols::<16>(&mut seed);
        check_cols::<32>(&mut seed);
    }

    #[test]
    fn wht_round_trip_identity() {
        let mut coefs = [0i32; 16];
        coefs[0] = 4 * 4;
        let mut dst = [100u16; 16];
        inverse_transform_add(&coefs, 0, DCT_DCT, true, 1, 4, 8, &mut dst, 4);
        let sum: i32 = dst.iter().map(|&v| v as i32 - 100).sum();
        assert_eq!(sum, 16);
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt,fma")]
    fn old_itx(coefs: &[i32], tx: u8, p: &TxParams, dst: &mut [u16], stride: usize) {
        match tx {
            0 => itx_narrow::<u16, 4>(coefs, p, dst, stride),
            1 => itx_narrow::<u16, 8>(coefs, p, dst, stride),
            2 => itx_narrow::<u16, 16>(coefs, p, dst, stride),
            _ => itx_narrow::<u16, 32>(coefs, p, dst, stride),
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    #[ignore]
    fn transform_speed() {
        use std::hint::black_box;
        use std::time::Instant;
        let mut seed = 7u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for tx in 0..4u8 {
            let n = 4usize << tx;
            for (k, adst) in [
                (n, false),
                (n.min(8), false),
                (n.min(4), false),
                (n, tx < 3),
            ] {
                let coefs: Vec<i32> = (0..n * n)
                    .map(|i| {
                        if i / n < k && i % n < k && rnd() % 2 == 0 {
                            (rnd() % 400) as i32 - 200
                        } else {
                            0
                        }
                    })
                    .collect();
                let shift = (tx as u32 + 4).min(6);
                let p = TxParams {
                    row_adst: adst,
                    col_adst: adst,
                    rows: k,
                    shift,
                    round: 1 << (shift - 1),
                    max: 255,
                };
                let stride = 64;
                let mut dst = vec![128u16; n * stride];
                let iters = 2_000_000 / (n * n / 16);
                let t = Instant::now();
                for _ in 0..iters {
                    unsafe { old_itx(black_box(&coefs), tx, &p, black_box(&mut dst), stride) };
                }
                let a = t.elapsed().as_nanos() as f64 / iters as f64;
                let t = Instant::now();
                for _ in 0..iters {
                    unsafe {
                        simd::itx::<u16, true>(
                            black_box(&coefs),
                            tx,
                            &p,
                            black_box(&mut dst),
                            stride,
                        )
                    };
                }
                let b = t.elapsed().as_nanos() as f64 / iters as f64;
                println!(
                    "{n}x{n} k {k} adst {adst}: old {a:.1} ns, new {b:.1} ns, x{:.2}",
                    a / b
                );
            }
        }
    }
}
