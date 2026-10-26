#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

pub fn sad(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    check(a, sa, b, sb, w, h);
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2() {
        return unsafe { sad_avx2(a, sa, b, sb, w, h) };
    }
    sad_scalar(a, sa, b, sb, w, h)
}

pub fn satd(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let n = if w < 8 || h < 8 { 4 } else { 8 };
    check(a, sa, b, sb, w.next_multiple_of(n), h.next_multiple_of(n));
    if n == 4 {
        #[cfg(target_arch = "x86_64")]
        if crate::media::isa::avx2() {
            return unsafe { satd4_avx2(a, sa, b, sb, w, h) };
        }
        return satd4_scalar(a, sa, b, sb, w, h);
    }
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2() {
        return unsafe { satd8_avx2(a, sa, b, sb, w, h) };
    }
    satd8_scalar(a, sa, b, sb, w, h)
}

pub fn sse(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    check(a, sa, b, sb, w, h);
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2() && w >= 16 {
        let w16 = w & !15;
        let head = unsafe { sse_avx2(a, sa, b, sb, w16, h) };
        if w16 == w {
            return head;
        }
        return head + sse_scalar(&a[w16..], sa, &b[w16..], sb, w - w16, h);
    }
    sse_scalar(a, sa, b, sb, w, h)
}

pub fn variance(a: &[u8], sa: usize, w: usize, h: usize) -> u32 {
    let (mut s, mut q) = (0u64, 0u64);
    for i in 0..h {
        for &v in &a[i * sa..i * sa + w] {
            s += v as u64;
            q += (v as u64) * (v as u64);
        }
    }
    let n = (w * h) as u64;
    (q - s * s / n) as u32
}

#[inline(always)]
fn check(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) {
    assert!(h > 0 && w > 0);
    assert!(a.len() >= (h - 1) * sa + w && b.len() >= (h - 1) * sb + w);
}

pub fn sad_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for i in 0..h {
        for (x, y) in a[i * sa..i * sa + w].iter().zip(&b[i * sb..i * sb + w]) {
            s += x.abs_diff(*y) as u32;
        }
    }
    s
}

pub fn sse_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for i in 0..h {
        for (x, y) in a[i * sa..i * sa + w].iter().zip(&b[i * sb..i * sb + w]) {
            let d = *x as i32 - *y as i32;
            s += (d * d) as u32;
        }
    }
    s
}

fn hadamard_sum<const N: usize>(d: &mut [[i32; N]; N]) -> u32 {
    for r in d.iter_mut() {
        let mut step = 1;
        while step < N {
            for i in 0..N {
                if i & step == 0 {
                    let (x, y) = (r[i], r[i + step]);
                    r[i] = x + y;
                    r[i + step] = x - y;
                }
            }
            step <<= 1;
        }
    }
    let mut s = 0u32;
    for c in 0..N {
        let mut col = [0i32; N];
        for i in 0..N {
            col[i] = d[i][c];
        }
        let mut step = 1;
        while step < N {
            for i in 0..N {
                if i & step == 0 {
                    let (x, y) = (col[i], col[i + step]);
                    col[i] = x + y;
                    col[i + step] = x - y;
                }
            }
            step <<= 1;
        }
        s += col.iter().map(|v| v.unsigned_abs()).sum::<u32>();
    }
    s
}

fn satd_tiles<const N: usize>(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0;
    for by in (0..h).step_by(N) {
        for bx in (0..w).step_by(N) {
            let mut d = [[0i32; N]; N];
            for i in 0..N {
                for j in 0..N {
                    d[i][j] = a[(by + i) * sa + bx + j] as i32 - b[(by + i) * sb + bx + j] as i32;
                }
            }
            s += hadamard_sum(&mut d);
        }
    }
    s
}

pub fn satd4_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    satd_tiles::<4>(a, sa, b, sb, w, h)
}

pub fn satd8_scalar(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    satd_tiles::<8>(a, sa, b, sb, w, h)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn hsum_epi64(v: __m256i) -> u32 {
    let s = _mm_add_epi64(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
    (_mm_cvtsi128_si64(s) + _mm_extract_epi64::<1>(s)) as u32
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn hsum_epi32(v: __m256i) -> u32 {
    let s = _mm_add_epi32(_mm256_castsi256_si128(v), _mm256_extracti128_si256::<1>(v));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b01_00_11_10>(s));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32::<0b10_11_00_01>(s));
    _mm_cvtsi128_si32(s) as u32
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sad_avx2(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    unsafe {
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = _mm256_setzero_si256();
        match w {
            64 => {
                for i in 0..h {
                    let (ra, rb) = (pa.add(i * sa), pb.add(i * sb));
                    let x0 = _mm256_loadu_si256(ra as *const __m256i);
                    let y0 = _mm256_loadu_si256(rb as *const __m256i);
                    let x1 = _mm256_loadu_si256(ra.add(32) as *const __m256i);
                    let y1 = _mm256_loadu_si256(rb.add(32) as *const __m256i);
                    acc = _mm256_add_epi64(acc, _mm256_sad_epu8(x0, y0));
                    acc = _mm256_add_epi64(acc, _mm256_sad_epu8(x1, y1));
                }
            }
            32 => {
                for i in 0..h {
                    let x = _mm256_loadu_si256(pa.add(i * sa) as *const __m256i);
                    let y = _mm256_loadu_si256(pb.add(i * sb) as *const __m256i);
                    acc = _mm256_add_epi64(acc, _mm256_sad_epu8(x, y));
                }
            }
            16 => {
                let mut i = 0;
                while i + 1 < h {
                    let x = _mm256_loadu2_m128i(
                        pa.add((i + 1) * sa) as *const __m128i,
                        pa.add(i * sa) as *const __m128i,
                    );
                    let y = _mm256_loadu2_m128i(
                        pb.add((i + 1) * sb) as *const __m128i,
                        pb.add(i * sb) as *const __m128i,
                    );
                    acc = _mm256_add_epi64(acc, _mm256_sad_epu8(x, y));
                    i += 2;
                }
                if i < h {
                    return hsum_epi64(acc)
                        + sad_scalar(&a[i * sa..], sa, &b[i * sb..], sb, w, h - i);
                }
            }
            8 => {
                let mut i = 0;
                while i + 3 < h {
                    let x = _mm256_set_epi64x(
                        (pa.add((i + 3) * sa) as *const i64).read_unaligned(),
                        (pa.add((i + 2) * sa) as *const i64).read_unaligned(),
                        (pa.add((i + 1) * sa) as *const i64).read_unaligned(),
                        (pa.add(i * sa) as *const i64).read_unaligned(),
                    );
                    let y = _mm256_set_epi64x(
                        (pb.add((i + 3) * sb) as *const i64).read_unaligned(),
                        (pb.add((i + 2) * sb) as *const i64).read_unaligned(),
                        (pb.add((i + 1) * sb) as *const i64).read_unaligned(),
                        (pb.add(i * sb) as *const i64).read_unaligned(),
                    );
                    acc = _mm256_add_epi64(acc, _mm256_sad_epu8(x, y));
                    i += 4;
                }
                if i < h {
                    return hsum_epi64(acc)
                        + sad_scalar(&a[i * sa..], sa, &b[i * sb..], sb, w, h - i);
                }
            }
            4 if h.is_multiple_of(4) => {
                let mut acc = _mm_setzero_si128();
                for i in (0..h).step_by(4) {
                    let x = _mm_set_epi32(
                        (pa.add((i + 3) * sa) as *const i32).read_unaligned(),
                        (pa.add((i + 2) * sa) as *const i32).read_unaligned(),
                        (pa.add((i + 1) * sa) as *const i32).read_unaligned(),
                        (pa.add(i * sa) as *const i32).read_unaligned(),
                    );
                    let y = _mm_set_epi32(
                        (pb.add((i + 3) * sb) as *const i32).read_unaligned(),
                        (pb.add((i + 2) * sb) as *const i32).read_unaligned(),
                        (pb.add((i + 1) * sb) as *const i32).read_unaligned(),
                        (pb.add(i * sb) as *const i32).read_unaligned(),
                    );
                    acc = _mm_add_epi64(acc, _mm_sad_epu8(x, y));
                }
                return (_mm_cvtsi128_si64(acc) + _mm_extract_epi64::<1>(acc)) as u32;
            }
            _ => return sad_scalar(a, sa, b, sb, w, h),
        }
        hsum_epi64(acc)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn sse_avx2(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    unsafe {
        let mut acc = _mm256_setzero_si256();
        for i in 0..h {
            let (ra, rb) = (a.as_ptr().add(i * sa), b.as_ptr().add(i * sb));
            for x in (0..w).step_by(16) {
                let p = _mm256_cvtepu8_epi16(_mm_loadu_si128(ra.add(x) as *const __m128i));
                let q = _mm256_cvtepu8_epi16(_mm_loadu_si128(rb.add(x) as *const __m128i));
                let d = _mm256_sub_epi16(p, q);
                acc = _mm256_add_epi32(acc, _mm256_madd_epi16(d, d));
            }
        }
        hsum_epi32(acc)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn butterfly(r: &mut [__m256i; 8], step: usize) {
    for i in 0..8 {
        if i & step == 0 {
            let (x, y) = (r[i], r[i + step]);
            r[i] = _mm256_add_epi16(x, y);
            r[i + step] = _mm256_sub_epi16(x, y);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn transpose8(r: &mut [__m256i; 8]) {
    let t0 = _mm256_unpacklo_epi16(r[0], r[1]);
    let t1 = _mm256_unpackhi_epi16(r[0], r[1]);
    let t2 = _mm256_unpacklo_epi16(r[2], r[3]);
    let t3 = _mm256_unpackhi_epi16(r[2], r[3]);
    let t4 = _mm256_unpacklo_epi16(r[4], r[5]);
    let t5 = _mm256_unpackhi_epi16(r[4], r[5]);
    let t6 = _mm256_unpacklo_epi16(r[6], r[7]);
    let t7 = _mm256_unpackhi_epi16(r[6], r[7]);
    let u0 = _mm256_unpacklo_epi32(t0, t2);
    let u1 = _mm256_unpackhi_epi32(t0, t2);
    let u2 = _mm256_unpacklo_epi32(t1, t3);
    let u3 = _mm256_unpackhi_epi32(t1, t3);
    let u4 = _mm256_unpacklo_epi32(t4, t6);
    let u5 = _mm256_unpackhi_epi32(t4, t6);
    let u6 = _mm256_unpacklo_epi32(t5, t7);
    let u7 = _mm256_unpackhi_epi32(t5, t7);
    r[0] = _mm256_unpacklo_epi64(u0, u4);
    r[1] = _mm256_unpackhi_epi64(u0, u4);
    r[2] = _mm256_unpacklo_epi64(u1, u5);
    r[3] = _mm256_unpackhi_epi64(u1, u5);
    r[4] = _mm256_unpacklo_epi64(u2, u6);
    r[5] = _mm256_unpackhi_epi64(u2, u6);
    r[6] = _mm256_unpacklo_epi64(u3, u7);
    r[7] = _mm256_unpackhi_epi64(u3, u7);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn hadamard_abs(r: &mut [__m256i; 8]) -> __m256i {
    butterfly(r, 1);
    butterfly(r, 2);
    butterfly(r, 4);
    transpose8(r);
    butterfly(r, 1);
    butterfly(r, 2);
    butterfly(r, 4);
    let ones = _mm256_set1_epi16(1);
    let mut acc = _mm256_setzero_si256();
    for k in (0..8).step_by(2) {
        let s = _mm256_add_epi16(_mm256_abs_epi16(r[k]), _mm256_abs_epi16(r[k + 1]));
        acc = _mm256_add_epi32(acc, _mm256_madd_epi16(s, ones));
    }
    acc
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn satd8_avx2(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    unsafe {
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = _mm256_setzero_si256();
        let mut r = [_mm256_setzero_si256(); 8];
        for by in (0..h).step_by(8) {
            let mut bx = 0;
            while bx + 8 < w {
                for i in 0..8 {
                    let p = _mm256_cvtepu8_epi16(_mm_loadu_si128(
                        pa.add((by + i) * sa + bx) as *const __m128i
                    ));
                    let q = _mm256_cvtepu8_epi16(_mm_loadu_si128(
                        pb.add((by + i) * sb + bx) as *const __m128i
                    ));
                    r[i] = _mm256_sub_epi16(p, q);
                }
                acc = _mm256_add_epi32(acc, hadamard_abs(&mut r));
                bx += 16;
            }
            if bx < w {
                let mut i = 0;
                while i < 8 {
                    let p = _mm256_cvtepu8_epi16(_mm_set_epi64x(
                        (pa.add((by + i + 1) * sa + bx) as *const i64).read_unaligned(),
                        (pa.add((by + i) * sa + bx) as *const i64).read_unaligned(),
                    ));
                    let q = _mm256_cvtepu8_epi16(_mm_set_epi64x(
                        (pb.add((by + i + 1) * sb + bx) as *const i64).read_unaligned(),
                        (pb.add((by + i) * sb + bx) as *const i64).read_unaligned(),
                    ));
                    let d = _mm256_sub_epi16(p, q);
                    r[i] = _mm256_permute4x64_epi64::<0b01_00_01_00>(d);
                    r[i + 1] = _mm256_permute4x64_epi64::<0b11_10_11_10>(d);
                    i += 2;
                }
                let half = hadamard_abs(&mut r);
                acc = _mm256_add_epi32(acc, _mm256_permute2x128_si256::<0x80>(half, half));
            }
        }
        hsum_epi32(acc)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn satd4_avx2(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    unsafe {
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let ones = _mm256_set1_epi16(1);
        let mut acc = _mm256_setzero_si256();
        let rows = |p: *const u8, s: usize| {
            _mm256_cvtepu8_epi16(_mm_set_epi32(
                (p.add(3 * s) as *const i32).read_unaligned(),
                (p.add(2 * s) as *const i32).read_unaligned(),
                (p.add(s) as *const i32).read_unaligned(),
                (p as *const i32).read_unaligned(),
            ))
        };
        for by in (0..h).step_by(4) {
            for bx in (0..w).step_by(4) {
                let v = _mm256_sub_epi16(
                    rows(pa.add(by * sa + bx), sa),
                    rows(pb.add(by * sb + bx), sb),
                );
                let p = _mm256_permute2x128_si256::<0x01>(v, v);
                let x = _mm256_permute2x128_si256::<0x20>(
                    _mm256_add_epi16(v, p),
                    _mm256_sub_epi16(v, p),
                );
                let q = _mm256_shuffle_epi32::<0b01_00_11_10>(x);
                let y = _mm256_unpacklo_epi64(_mm256_add_epi16(x, q), _mm256_sub_epi16(x, q));
                let q = _mm256_shuffle_epi32::<0b10_11_00_01>(y);
                let z = _mm256_blend_epi32::<0b1010_1010>(
                    _mm256_add_epi16(y, q),
                    _mm256_sub_epi16(y, q),
                );
                let q = _mm256_shufflehi_epi16::<0b10_11_00_01>(_mm256_shufflelo_epi16::<
                    0b10_11_00_01,
                >(z));
                let f = _mm256_blend_epi16::<0b1010_1010>(
                    _mm256_add_epi16(z, q),
                    _mm256_sub_epi16(z, q),
                );
                acc = _mm256_add_epi32(acc, _mm256_madd_epi16(_mm256_abs_epi16(f), ones));
            }
        }
        hsum_epi32(acc)
    }
}
