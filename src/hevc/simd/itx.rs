use crate::hevc::pel::Pel;

pub(crate) fn inverse_transform(
    c: &mut [i16],
    n: usize,
    dst: bool,
    max_x: usize,
    max_y: usize,
    bd_shift: u32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on() && c.len() >= n * n && max_x < n && max_y < n {
        unsafe {
            match n {
                4 => x86::inverse4(c, dst, bd_shift),
                8 => x86::inverse8(c, max_x, max_y, bd_shift),
                16 => x86::inverse_wide::<16>(c, max_x, max_y, bd_shift),
                32 => x86::inverse_wide::<32>(c, max_x, max_y, bd_shift),
                _ => return false,
            }
        }
        return true;
    }
    let _ = (c, n, dst, max_x, max_y, bd_shift);
    false
}

pub(crate) fn add_residual<P: Pel>(
    plane: &mut [P],
    stride: usize,
    n: usize,
    c: &[i16],
    max: i32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on()
        && matches!(n, 4 | 8 | 16 | 32)
        && c.len() >= n * n
        && plane.len() >= (n - 1) * stride + n
    {
        unsafe { x86::add_residual(plane, stride, n, c, max) };
        return true;
    }
    let _ = (plane, stride, n, c, max);
    false
}

pub(crate) fn dequant(
    c: &mut [i16],
    n: usize,
    max_x: usize,
    max_y: usize,
    m: Option<&[u8]>,
    scale: i64,
    round: i64,
    bd_shift: u32,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    if super::on()
        && matches!(n, 4 | 8 | 16 | 32)
        && max_x < n
        && max_y < n
        && c.len() >= n * n
        && m.is_none_or(|m| m.len() >= n * n)
        && (1..=31).contains(&bd_shift)
        && (0..1 << 26).contains(&scale)
        && round == 1 << (bd_shift - 1)
    {
        let f = if m.is_some() { 255 } else { 16 };
        let peak = 32768 * f * scale + round;
        if peak < 1 << 31
            || unsafe { x86::peak(c, n, max_x, max_y) } as i64 * f * scale + round < 1 << 31
        {
            unsafe { x86::dequant(c, n, max_x, max_y, m, scale as i32, bd_shift) };
            return true;
        }
    }
    let _ = (c, n, max_x, max_y, m, scale, round, bd_shift);
    false
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::super::x86::{gather, scatter_pel};
    use crate::hevc::pel::Pel;
    use crate::hevc::tables::{DST_MATRIX, TRANS_MATRIX};
    use std::arch::x86_64::*;

    const fn coef(n: usize, j: usize, i: usize) -> i32 {
        TRANS_MATRIX[j * (32 / n)][i] as i32
    }

    const fn pack(a: i32, b: i32) -> i32 {
        ((a as u16 as u32) | ((b as u16 as u32) << 16)) as i32
    }

    const fn pairs(n: usize, odd: bool) -> [[i32; 8]; 16] {
        let mut t = [[0i32; 8]; 16];
        let mut i = 0;
        while i < n / 2 {
            let mut p = 0;
            while p < n / 4 {
                let j = 4 * p + odd as usize;
                t[i][p] = pack(coef(n, j, i), coef(n, j + 2, i));
                p += 1;
            }
            i += 1;
        }
        t
    }

    static PAIRS: [[[[i32; 8]; 16]; 2]; 3] = [
        [pairs(8, false), pairs(8, true)],
        [pairs(16, false), pairs(16, true)],
        [pairs(32, false), pairs(32, true)],
    ];

    const fn m4(dst: bool, j: usize, i: usize) -> i32 {
        if dst {
            DST_MATRIX[j][i] as i32
        } else {
            TRANS_MATRIX[j * 8][i] as i32
        }
    }

    const fn k4(dst: bool) -> [[i16; 16]; 6] {
        let mut t = [[0i16; 16]; 6];
        let mut v = 0;
        while v < 4 {
            let (r, i0) = ((v % 2) * 2, (v / 2) * 2);
            let mut l = 0;
            while l < 8 {
                let i = if l < 4 { i0 } else { i0 + 1 };
                t[v][2 * l] = m4(dst, r, i) as i16;
                t[v][2 * l + 1] = m4(dst, r + 1, i) as i16;
                l += 1;
            }
            v += 1;
        }
        let mut l = 0;
        while l < 8 {
            let i = l % 4;
            t[4][2 * l] = m4(dst, 0, i) as i16;
            t[4][2 * l + 1] = m4(dst, 1, i) as i16;
            t[5][2 * l] = m4(dst, 2, i) as i16;
            t[5][2 * l + 1] = m4(dst, 3, i) as i16;
            l += 1;
        }
        t
    }

    static K4: [[[i16; 16]; 6]; 2] = [k4(false), k4(true)];

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn ld(p: *const i16) -> __m256i {
        unsafe { _mm256_loadu_si256(p as *const __m256i) }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn inverse4(c: &mut [i16], dst: bool, bd_shift: u32) {
        unsafe {
            let k = &K4[dst as usize];
            let p = c.as_mut_ptr();
            let r = |j: usize| _mm_loadl_epi64(p.add(4 * j) as *const __m128i);
            let p01 = _mm256_broadcastsi128_si256(_mm_unpacklo_epi16(r(0), r(1)));
            let p23 = _mm256_broadcastsi128_si256(_mm_unpacklo_epi16(r(2), r(3)));
            let r64 = _mm256_set1_epi32(64);
            let a = _mm256_add_epi32(
                _mm256_madd_epi16(p01, ld(k[0].as_ptr())),
                _mm256_madd_epi16(p23, ld(k[1].as_ptr())),
            );
            let b = _mm256_add_epi32(
                _mm256_madd_epi16(p01, ld(k[2].as_ptr())),
                _mm256_madd_epi16(p23, ld(k[3].as_ptr())),
            );
            let a = _mm256_srai_epi32::<7>(_mm256_add_epi32(a, r64));
            let b = _mm256_srai_epi32::<7>(_mm256_add_epi32(b, r64));
            let e = _mm256_packs_epi32(a, b);
            let (m01, m23) = (ld(k[4].as_ptr()), ld(k[5].as_ptr()));
            let rnd = _mm256_set1_epi32(1 << (bd_shift - 1));
            let sh = _mm_cvtsi32_si128(bd_shift as i32);
            let r01 = _mm256_add_epi32(
                _mm256_madd_epi16(_mm256_shuffle_epi32::<0x00>(e), m01),
                _mm256_madd_epi16(_mm256_shuffle_epi32::<0x55>(e), m23),
            );
            let r23 = _mm256_add_epi32(
                _mm256_madd_epi16(_mm256_shuffle_epi32::<0xaa>(e), m01),
                _mm256_madd_epi16(_mm256_shuffle_epi32::<0xff>(e), m23),
            );
            let r01 = _mm256_sra_epi32(_mm256_add_epi32(r01, rnd), sh);
            let r23 = _mm256_sra_epi32(_mm256_add_epi32(r23, rnd), sh);
            let o = _mm256_permute4x64_epi64::<0xd8>(_mm256_packs_epi32(r01, r23));
            _mm256_storeu_si256(p as *mut __m256i, o);
        }
    }

    const fn rows2(n: usize, odd: bool) -> [[i32; 16]; 8] {
        let mut t = [[0i32; 16]; 8];
        let mut p = 0;
        while p < n / 4 {
            let j = 4 * p + odd as usize;
            let mut i = 0;
            while i < n / 2 {
                t[p][i] = pack(coef(n, j, i), coef(n, j + 2, i));
                i += 1;
            }
            p += 1;
        }
        t
    }

    static ROWS: [[[[i32; 16]; 8]; 2]; 3] = [
        [rows2(8, false), rows2(8, true)],
        [rows2(16, false), rows2(16, true)],
        [rows2(32, false), rows2(32, true)],
    ];

    const EVEN_ODD: [i8; 16] = [0, 1, 4, 5, 8, 9, 12, 13, 2, 3, 6, 7, 10, 11, 14, 15];
    const REV4: [i8; 16] = [6, 7, 4, 5, 2, 3, 0, 1, 14, 15, 12, 13, 10, 11, 8, 9];

    fn counts(jmax: usize, q: usize) -> (usize, usize) {
        let pe = (jmax / 4 + 1).min(q);
        let po = if jmax >= 1 {
            ((jmax - 1) / 4 + 1).min(q)
        } else {
            0
        };
        (pe, po)
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn cols8(s: *const i16, jmax: usize, d: *mut i16) {
        unsafe {
            let [te, to] = &PAIRS[0];
            let z = _mm_setzero_si128();
            let row = |j: usize| {
                if j <= jmax {
                    _mm_loadu_si128(s.add(j * 8) as *const __m128i)
                } else {
                    z
                }
            };
            let (pe, po) = counts(jmax, 2);
            let mut ev = [[z; 2]; 2];
            let mut od = [[z; 2]; 2];
            for p in 0..pe {
                let (a, b) = (row(4 * p), row(4 * p + 2));
                ev[p] = [_mm_unpacklo_epi16(a, b), _mm_unpackhi_epi16(a, b)];
            }
            for p in 0..po {
                let (a, b) = (row(4 * p + 1), row(4 * p + 3));
                od[p] = [_mm_unpacklo_epi16(a, b), _mm_unpackhi_epi16(a, b)];
            }
            let rnd = _mm_set1_epi32(64);
            let perm = _mm_loadu_si128(EVEN_ODD.as_ptr() as *const __m128i);
            for i in 0..4 {
                let mut se = [rnd; 2];
                let mut so = [z; 2];
                for p in 0..pe {
                    let k = _mm_set1_epi32(te[i][p]);
                    se[0] = _mm_add_epi32(se[0], _mm_madd_epi16(ev[p][0], k));
                    se[1] = _mm_add_epi32(se[1], _mm_madd_epi16(ev[p][1], k));
                }
                for p in 0..po {
                    let k = _mm_set1_epi32(to[i][p]);
                    so[0] = _mm_add_epi32(so[0], _mm_madd_epi16(od[p][0], k));
                    so[1] = _mm_add_epi32(so[1], _mm_madd_epi16(od[p][1], k));
                }
                let f = |a: __m128i, b: __m128i| _mm_srai_epi32::<7>(_mm_add_epi32(a, b));
                let g = |a: __m128i, b: __m128i| _mm_srai_epi32::<7>(_mm_sub_epi32(a, b));
                let top = _mm_packs_epi32(f(se[0], so[0]), f(se[1], so[1]));
                let bot = _mm_packs_epi32(g(se[0], so[0]), g(se[1], so[1]));
                _mm_storeu_si128(d.add(i * 8) as *mut __m128i, _mm_shuffle_epi8(top, perm));
                _mm_storeu_si128(
                    d.add((7 - i) * 8) as *mut __m128i,
                    _mm_shuffle_epi8(bot, perm),
                );
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn bcast(p: *const i16) -> __m256i {
        unsafe { _mm256_set1_epi32(std::ptr::read_unaligned(p as *const i32)) }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn rows8(e: *const i16, jmax: usize, bd_shift: u32, d: *mut i16) {
        unsafe {
            let [te, to] = &ROWS[0];
            let (pe, po) = counts(jmax, 2);
            let rnd = _mm_set1_epi32(1 << (bd_shift - 1));
            let sh = _mm_cvtsi32_si128(bd_shift as i32);
            let rev = _mm_setr_epi8(0, 1, 2, 3, 4, 5, 6, 7, 14, 15, 12, 13, 10, 11, 8, 9);
            let ke = [
                _mm_loadu_si128(te[0].as_ptr() as *const __m128i),
                _mm_loadu_si128(te[1].as_ptr() as *const __m128i),
            ];
            let ko = [
                _mm_loadu_si128(to[0].as_ptr() as *const __m128i),
                _mm_loadu_si128(to[1].as_ptr() as *const __m128i),
            ];
            for y in 0..8 {
                let r = e.add(y * 8);
                let b =
                    |k: usize| _mm_set1_epi32(std::ptr::read_unaligned(r.add(2 * k) as *const i32));
                let mut se = rnd;
                let mut so = _mm_setzero_si128();
                for p in 0..pe {
                    se = _mm_add_epi32(se, _mm_madd_epi16(b(p), ke[p]));
                }
                for p in 0..po {
                    so = _mm_add_epi32(so, _mm_madd_epi16(b(2 + p), ko[p]));
                }
                let top = _mm_sra_epi32(_mm_add_epi32(se, so), sh);
                let bot = _mm_sra_epi32(_mm_sub_epi32(se, so), sh);
                let o = _mm_shuffle_epi8(_mm_packs_epi32(top, bot), rev);
                _mm_storeu_si128(d.add(y * 8) as *mut __m128i, o);
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn inverse8(c: &mut [i16], max_x: usize, max_y: usize, bd_shift: u32) {
        unsafe {
            let p = c.as_mut_ptr();
            let mut e = std::mem::MaybeUninit::<[i16; 64]>::uninit();
            let ep = e.as_mut_ptr() as *mut i16;
            cols8(p, max_y, ep);
            rows8(ep, max_x, bd_shift, p);
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn cols<const N: usize>(s: *const i16, jmax: usize, xs: usize, d: *mut i16) {
        unsafe {
            let [te, to] = &PAIRS[N.trailing_zeros() as usize - 3];
            let z = _mm256_setzero_si256();
            let (pe, po) = counts(jmax, N / 4);
            let rnd = _mm256_set1_epi32(64);
            let perm =
                _mm256_broadcastsi128_si256(_mm_loadu_si128(EVEN_ODD.as_ptr() as *const __m128i));
            let put = |row: usize, xc: usize, v: __m256i| {
                let v = _mm256_permute4x64_epi64::<0xd8>(_mm256_shuffle_epi8(v, perm));
                let r = d.add(row * N + 8 * xc);
                _mm_storeu_si128(r as *mut __m128i, _mm256_castsi256_si128(v));
                _mm_storeu_si128(
                    r.add(N / 2) as *mut __m128i,
                    _mm256_extracti128_si256::<1>(v),
                );
            };
            for xc in 0..xs {
                let x = xc * 16;
                let row = |j: usize| {
                    if j <= jmax {
                        _mm256_loadu_si256(s.add(j * N + x) as *const __m256i)
                    } else {
                        z
                    }
                };
                let mut ev = [[z; 2]; 8];
                let mut od = [[z; 2]; 8];
                for p in 0..pe {
                    let (a, b) = (row(4 * p), row(4 * p + 2));
                    ev[p] = [_mm256_unpacklo_epi16(a, b), _mm256_unpackhi_epi16(a, b)];
                }
                for p in 0..po {
                    let (a, b) = (row(4 * p + 1), row(4 * p + 3));
                    od[p] = [_mm256_unpacklo_epi16(a, b), _mm256_unpackhi_epi16(a, b)];
                }
                for i in 0..N / 2 {
                    let mut se = [rnd; 2];
                    let mut so = [z; 2];
                    for p in 0..pe {
                        let k = _mm256_set1_epi32(te[i][p]);
                        se[0] = _mm256_add_epi32(se[0], _mm256_madd_epi16(ev[p][0], k));
                        se[1] = _mm256_add_epi32(se[1], _mm256_madd_epi16(ev[p][1], k));
                    }
                    for p in 0..po {
                        let k = _mm256_set1_epi32(to[i][p]);
                        so[0] = _mm256_add_epi32(so[0], _mm256_madd_epi16(od[p][0], k));
                        so[1] = _mm256_add_epi32(so[1], _mm256_madd_epi16(od[p][1], k));
                    }
                    let f = |a: __m256i, b: __m256i| _mm256_srai_epi32::<7>(_mm256_add_epi32(a, b));
                    let g = |a: __m256i, b: __m256i| _mm256_srai_epi32::<7>(_mm256_sub_epi32(a, b));
                    put(i, xc, _mm256_packs_epi32(f(se[0], so[0]), f(se[1], so[1])));
                    put(
                        N - 1 - i,
                        xc,
                        _mm256_packs_epi32(g(se[0], so[0]), g(se[1], so[1])),
                    );
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn rows<const N: usize>(e: *const i16, jmax: usize, bd_shift: u32, d: *mut i16) {
        unsafe {
            let [te, to] = &ROWS[N.trailing_zeros() as usize - 3];
            let (pe, po) = counts(jmax, N / 4);
            let rnd = _mm256_set1_epi32(1 << (bd_shift - 1));
            let sh = _mm_cvtsi32_si128(bd_shift as i32);
            let rev = _mm256_broadcastsi128_si256(_mm_loadu_si128(REV4.as_ptr() as *const __m128i));
            let tail = _mm256_setr_epi8(
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 6, 7, 4, 5, 2, 3, 0, 1, 14,
                15, 12, 13, 10, 11, 8, 9,
            );
            let ld = |t: &[i32; 16], c: usize| {
                _mm256_loadu_si256(t.as_ptr().add(8 * c) as *const __m256i)
            };
            for y in 0..N {
                let r = e.add(y * N);
                let mut se = [rnd; 2];
                let mut so = [_mm256_setzero_si256(); 2];
                for c in 0..N / 16 {
                    for p in 0..pe {
                        se[c] = _mm256_add_epi32(
                            se[c],
                            _mm256_madd_epi16(bcast(r.add(2 * p)), ld(&te[p], c)),
                        );
                    }
                    for p in 0..po {
                        so[c] = _mm256_add_epi32(
                            so[c],
                            _mm256_madd_epi16(bcast(r.add(N / 2 + 2 * p)), ld(&to[p], c)),
                        );
                    }
                }
                let f = |a: __m256i, b: __m256i| _mm256_sra_epi32(_mm256_add_epi32(a, b), sh);
                let g = |a: __m256i, b: __m256i| _mm256_sra_epi32(_mm256_sub_epi32(a, b), sh);
                let o = d.add(y * N);
                if N == 16 {
                    let v = _mm256_packs_epi32(f(se[0], so[0]), g(se[0], so[0]));
                    let v = _mm256_shuffle_epi8(_mm256_permute4x64_epi64::<0x78>(v), tail);
                    _mm256_storeu_si256(o as *mut __m256i, v);
                } else {
                    let t = _mm256_packs_epi32(f(se[0], so[0]), f(se[1], so[1]));
                    let b = _mm256_packs_epi32(g(se[0], so[0]), g(se[1], so[1]));
                    _mm256_storeu_si256(o as *mut __m256i, _mm256_permute4x64_epi64::<0xd8>(t));
                    let b = _mm256_shuffle_epi8(_mm256_permute4x64_epi64::<0x27>(b), rev);
                    _mm256_storeu_si256(o.add(16) as *mut __m256i, b);
                }
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn inverse_wide<const N: usize>(
        c: &mut [i16],
        max_x: usize,
        max_y: usize,
        bd_shift: u32,
    ) {
        unsafe {
            let p = c.as_mut_ptr();
            let mut e = std::mem::MaybeUninit::<[i16; 1024]>::uninit();
            let ep = e.as_mut_ptr() as *mut i16;
            cols::<N>(p, max_y, max_x / 16 + 1, ep);
            rows::<N>(ep, max_x, bd_shift, p);
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn add_residual<P: Pel>(
        plane: &mut [P],
        stride: usize,
        n: usize,
        c: &[i16],
        max: i32,
    ) {
        unsafe {
            let d = plane.as_mut_ptr();
            let r = c.as_ptr();
            let hi = _mm256_set1_epi16(max as i16);
            let z = _mm256_setzero_si256();
            let one = |v: __m256i, res: __m256i| {
                let s = _mm256_adds_epi16(v, res);
                if P::BYTES == 1 {
                    s
                } else {
                    _mm256_min_epi16(_mm256_max_epi16(s, z), hi)
                }
            };
            match n {
                4 => {
                    let v = gather::<P, 4>(|i| d.add(i * stride));
                    let s = one(v, _mm256_loadu_si256(r as *const __m256i));
                    scatter_pel::<P, 4>(s, 4, |i| d.add(i * stride));
                }
                8 => {
                    for y in (0..8).step_by(2) {
                        let v = gather::<P, 8>(|i| d.add((y + i) * stride));
                        let s = one(v, _mm256_loadu_si256(r.add(y * 8) as *const __m256i));
                        scatter_pel::<P, 8>(s, 2, |i| d.add((y + i) * stride));
                    }
                }
                _ => {
                    for y in 0..n {
                        for x in (0..n).step_by(16) {
                            let o = d.add(y * stride + x);
                            let v = gather::<P, 16>(|_| o);
                            let s = one(v, _mm256_loadu_si256(r.add(y * n + x) as *const __m256i));
                            scatter_pel::<P, 16>(s, 1, |_| o);
                        }
                    }
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn region(n: usize, max_x: usize, max_y: usize, mut f: impl FnMut(usize)) {
        if n == 4 {
            f(0);
            if max_y >= 2 {
                f(8);
            }
            return;
        }
        for y in 0..=max_y {
            for x in (0..=max_x).step_by(8) {
                f(y * n + x);
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn peak(c: &[i16], n: usize, max_x: usize, max_y: usize) -> i32 {
        unsafe {
            let p = c.as_ptr();
            let mut m = _mm_setzero_si128();
            region(n, max_x, max_y, |o| {
                let v = _mm_loadu_si128(p.add(o) as *const __m128i);
                m = _mm_max_epu16(m, _mm_abs_epi16(v));
            });
            let m = _mm_max_epu16(m, _mm_unpackhi_epi64(m, m));
            let m = _mm_max_epu16(m, _mm_shuffle_epi32::<1>(m));
            let m = _mm_max_epu16(m, _mm_shufflelo_epi16::<1>(m));
            _mm_extract_epi16::<0>(m)
        }
    }

    #[target_feature(enable = "avx2")]
    pub unsafe fn dequant(
        c: &mut [i16],
        n: usize,
        max_x: usize,
        max_y: usize,
        m: Option<&[u8]>,
        scale: i32,
        bd_shift: u32,
    ) {
        unsafe {
            let p = c.as_mut_ptr();
            let k = _mm256_set1_epi32(scale);
            let k16 = _mm256_set1_epi32(16 * scale);
            let rnd = _mm256_set1_epi32(1 << (bd_shift - 1));
            let sh = _mm_cvtsi32_si128(bd_shift as i32);
            region(n, max_x, max_y, |o| {
                let v = _mm256_cvtepi16_epi32(_mm_loadu_si128(p.add(o) as *const __m128i));
                let f = match m {
                    Some(m) => _mm256_mullo_epi32(
                        _mm256_cvtepu8_epi32(_mm_loadl_epi64(m.as_ptr().add(o) as *const __m128i)),
                        k,
                    ),
                    None => k16,
                };
                let r = _mm256_sra_epi32(_mm256_add_epi32(_mm256_mullo_epi32(v, f), rnd), sh);
                let r = _mm256_packs_epi32(r, _mm256_permute2x128_si256::<0x11>(r, r));
                _mm_storeu_si128(p.add(o) as *mut __m128i, _mm256_castsi256_si128(r));
            });
        }
    }
}
