#[derive(Debug, Clone, Copy)]
pub struct Simd;

impl Simd {
    pub fn detect() -> Self {
        Simd
    }
}

pub fn unfilter(
    filter: u8,
    first: bool,
    prev: &[u8],
    raw: &[u8],
    current: &mut [u8],
    bpp: usize,
    simd: Simd,
) -> bool {
    match (filter, first) {
        (0, _) | (2, true) => current.copy_from_slice(raw),
        (1, _) => sub(raw, current, bpp),
        (2, false) => up(prev, raw, current),
        (3, true) => avg_first(raw, current, bpp),
        (3, false) => avg(prev, raw, current, bpp),
        (4, true) => paeth_first(raw, current, bpp),
        (4, false) => paeth_rows(prev, raw, current, bpp, simd),
        _ => return false,
    }
    true
}

#[inline(always)]
fn sub_n<const N: usize>(raw: &[u8], current: &mut [u8]) {
    let mut a = [0u8; N];
    for (out, x) in current.chunks_exact_mut(N).zip(raw.chunks_exact(N)) {
        for k in 0..N {
            a[k] = x[k].wrapping_add(a[k]);
        }
        out.copy_from_slice(&a);
    }
}

#[inline(always)]
fn avg_n<const N: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
    let mut a = [0u8; N];
    for ((out, x), b) in current
        .chunks_exact_mut(N)
        .zip(raw.chunks_exact(N))
        .zip(prev.chunks_exact(N))
    {
        for k in 0..N {
            let avg = ((u16::from(a[k]) + u16::from(b[k])) >> 1) as u8;
            a[k] = x[k].wrapping_add(avg);
        }
        out.copy_from_slice(&a);
    }
}

#[inline(always)]
fn paeth_n<const N: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
    let mut a = [0u8; N];
    let mut c = [0u8; N];
    for ((out, x), b) in current
        .chunks_exact_mut(N)
        .zip(raw.chunks_exact(N))
        .zip(prev.chunks_exact(N))
    {
        for k in 0..N {
            a[k] = x[k].wrapping_add(paeth_branchless(a[k], b[k], c[k]));
        }
        c.copy_from_slice(b);
        out.copy_from_slice(&a);
    }
}

fn sub(raw: &[u8], current: &mut [u8], bpp: usize) {
    if current.len() < bpp || raw.len() < bpp {
        return;
    }
    match bpp {
        1 => sub_n::<1>(raw, current),
        2 => sub_n::<2>(raw, current),
        3 => sub_n::<3>(raw, current),
        4 => sub_n::<4>(raw, current),
        6 => sub_n::<6>(raw, current),
        8 => sub_n::<8>(raw, current),
        _ => {
            current[..bpp].copy_from_slice(&raw[..bpp]);
            let end = current.len().min(raw.len());
            for i in bpp..end {
                current[i] = raw[i].wrapping_add(current[i - bpp]);
            }
        }
    }
}

fn up(prev: &[u8], raw: &[u8], current: &mut [u8]) {
    for ((out, filt), above) in current.iter_mut().zip(raw).zip(prev) {
        *out = filt.wrapping_add(*above);
    }
}

fn avg(prev: &[u8], raw: &[u8], current: &mut [u8], bpp: usize) {
    if raw.len() < bpp || current.len() < bpp || prev.len() < bpp {
        return;
    }
    match bpp {
        1 => avg_n::<1>(prev, raw, current),
        2 => avg_n::<2>(prev, raw, current),
        3 => avg_n::<3>(prev, raw, current),
        4 => avg_n::<4>(prev, raw, current),
        6 => avg_n::<6>(prev, raw, current),
        8 => avg_n::<8>(prev, raw, current),
        _ => {
            for i in 0..bpp {
                current[i] = raw[i].wrapping_add(prev[i] >> 1);
            }
            let end = current.len().min(raw.len()).min(prev.len());
            for i in bpp..end {
                let a = current[i - bpp];
                let b = prev[i];
                current[i] = raw[i].wrapping_add((a & b) + ((a ^ b) >> 1));
            }
        }
    }
}

fn paeth_rows(prev: &[u8], raw: &[u8], current: &mut [u8], bpp: usize, _simd: Simd) {
    if raw.len() < bpp || current.len() < bpp || prev.len() < bpp {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if current.len() == raw.len()
        && current.len() == prev.len()
        && current.len().is_multiple_of(bpp)
    {
        match bpp {
            3 => return sse::paeth::<3>(prev, raw, current),
            4 => return sse::paeth::<4>(prev, raw, current),
            6 => return sse::paeth::<6>(prev, raw, current),
            8 => return sse::paeth::<8>(prev, raw, current),
            _ => (),
        }
    }
    match bpp {
        1 => paeth_n::<1>(prev, raw, current),
        2 => paeth_n::<2>(prev, raw, current),
        3 => paeth_n::<3>(prev, raw, current),
        4 => paeth_n::<4>(prev, raw, current),
        6 => paeth_n::<6>(prev, raw, current),
        8 => paeth_n::<8>(prev, raw, current),
        _ => {
            for i in 0..bpp {
                current[i] = raw[i].wrapping_add(paeth(0, prev[i], 0));
            }
            let end = current.len().min(raw.len()).min(prev.len());
            for i in bpp..end {
                let p = paeth(current[i - bpp], prev[i], prev[i - bpp]);
                current[i] = raw[i].wrapping_add(p);
            }
        }
    }
}

fn paeth_first(raw: &[u8], current: &mut [u8], bpp: usize) {
    sub(raw, current, bpp)
}

fn avg_first(raw: &[u8], current: &mut [u8], bpp: usize) {
    if raw.len() < bpp || current.len() < bpp {
        return;
    }
    current[..bpp].copy_from_slice(&raw[..bpp]);
    let end = current.len().min(raw.len());
    for i in bpp..end {
        current[i] = raw[i].wrapping_add(current[i - bpp] >> 1);
    }
}

#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i16::from(a), i16::from(b), i16::from(c));
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[inline(always)]
fn paeth_branchless(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i16::from(a), i16::from(b), i16::from(c));
    let thresh = c * 3 - (a + b);
    let lo = a.min(b);
    let hi = a.max(b);
    let t0 = if hi <= thresh { lo } else { c };
    (if thresh <= lo { hi } else { t0 }) as u8
}

#[cfg(target_arch = "x86_64")]
mod sse {
    use std::arch::x86_64::*;

    #[inline]
    #[target_feature(enable = "sse2")]
    fn select(mask: __m128i, yes: __m128i, no: __m128i) -> __m128i {
        _mm_or_si128(_mm_and_si128(mask, yes), _mm_andnot_si128(mask, no))
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    fn load<const N: usize>(bytes: &[u8], at: usize) -> __m128i {
        if N > 4 {
            let w: [u8; 8] = bytes[at..at + 8].try_into().unwrap_or([0; 8]);
            _mm_unpacklo_epi8(
                _mm_cvtsi64_si128(u64::from_le_bytes(w) as i64),
                _mm_setzero_si128(),
            )
        } else {
            let w: [u8; 4] = bytes[at..at + 4].try_into().unwrap_or([0; 4]);
            _mm_unpacklo_epi8(
                _mm_cvtsi32_si128(u32::from_le_bytes(w) as i32),
                _mm_setzero_si128(),
            )
        }
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    fn store<const N: usize>(bytes: &mut [u8], at: usize, v: __m128i) {
        let packed = _mm_packus_epi16(v, v);
        if N > 4 {
            let w = _mm_cvtsi128_si64(packed) as u64;
            bytes[at..at + 8].copy_from_slice(&w.to_le_bytes());
        } else {
            let w = _mm_cvtsi128_si32(packed) as u32;
            bytes[at..at + 4].copy_from_slice(&w.to_le_bytes());
        }
    }

    pub fn paeth<const N: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        unsafe { paeth_inner::<N>(prev, raw, current) }
    }

    #[target_feature(enable = "sse2")]
    fn paeth_inner<const N: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        let pixels = current.len() / N;
        let keep = _mm_set1_epi16(0xff);
        let mut a = _mm_setzero_si128();
        let mut c = _mm_setzero_si128();
        for i in 0..pixels - 1 {
            let at = i * N;
            let b = load::<N>(prev, at);
            let x = load::<N>(raw, at);
            let c3 = _mm_add_epi16(_mm_add_epi16(c, c), c);
            let thresh = _mm_sub_epi16(_mm_sub_epi16(c3, b), a);
            let lo = _mm_min_epi16(a, b);
            let hi = _mm_max_epi16(a, b);
            let t0 = select(_mm_cmpgt_epi16(hi, thresh), c, lo);
            let pred = select(_mm_cmpgt_epi16(thresh, lo), t0, hi);
            a = _mm_and_si128(_mm_add_epi16(x, pred), keep);
            store::<N>(current, at, a);
            c = b;
        }
        let mut av = [0i16; 8];
        let mut cv = [0i16; 8];
        unsafe {
            _mm_storeu_si128(av.as_mut_ptr().cast(), a);
            _mm_storeu_si128(cv.as_mut_ptr().cast(), c);
        }
        let at = (pixels - 1) * N;
        for k in 0..N {
            let b = prev[at + k];
            let p = super::paeth_branchless(av[k] as u8, b, cv[k] as u8);
            current[at + k] = raw[at + k].wrapping_add(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_branchless_predictor_is_the_specification_for_every_input() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                for c in 0..=255u8 {
                    assert_eq!(paeth_branchless(a, b, c), paeth(a, b, c), "{a} {b} {c}");
                }
            }
        }
    }

    fn reference(filter: u8, prev: &[u8], raw: &[u8], bpp: usize) -> Vec<u8> {
        let mut out = vec![0u8; raw.len()];
        for i in 0..raw.len() {
            let a = if i >= bpp { out[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let p = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                _ => paeth(a, b, c),
            };
            out[i] = raw[i].wrapping_add(p);
        }
        out
    }

    #[test]
    fn every_filter_unfilters_as_the_specification_says_at_every_pixel_size() {
        let mut s = 7u32;
        let mut next = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        };
        for bpp in [1usize, 2, 3, 4, 5, 6, 7, 8] {
            for pixels in [1usize, 2, 9, 40] {
                let n = bpp * pixels;
                let prev: Vec<u8> = (0..n).map(|_| next()).collect();
                let raw: Vec<u8> = (0..n).map(|_| next()).collect();
                for filter in 0..5u8 {
                    let mut got = vec![0u8; n];
                    assert!(unfilter(
                        filter,
                        false,
                        &prev,
                        &raw,
                        &mut got,
                        bpp,
                        Simd::detect()
                    ));
                    assert_eq!(
                        got,
                        reference(filter, &prev, &raw, bpp),
                        "{filter} {bpp} {pixels}"
                    );
                    let zeros = vec![0u8; n];
                    let mut got = vec![0u8; n];
                    assert!(unfilter(
                        filter,
                        true,
                        &prev,
                        &raw,
                        &mut got,
                        bpp,
                        Simd::detect()
                    ));
                    assert_eq!(
                        got,
                        reference(filter, &zeros, &raw, bpp),
                        "first {filter} {bpp} {pixels}"
                    );
                }
            }
        }
        let mut got = vec![0u8; 4];
        assert!(!unfilter(
            5,
            false,
            &[0; 4],
            &[0; 4],
            &mut got,
            4,
            Simd::detect()
        ));
    }
}
