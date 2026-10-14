#[derive(Debug, Clone, Copy)]
pub struct Simd {
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    sse41: bool,
}

impl Simd {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Simd {
                sse41: std::arch::is_x86_feature_detected!("sse4.1"),
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Simd { sse41: false }
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

fn sub(raw: &[u8], current: &mut [u8], bpp: usize) {
    if current.len() < bpp || raw.len() < bpp {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    match bpp {
        3 => return sse::sub::<3>(raw, current),
        4 => return sse::sub::<4>(raw, current),
        6 => return sse::sub::<6>(raw, current),
        8 => return sse::sub::<8>(raw, current),
        _ => (),
    }
    current[..bpp].copy_from_slice(&raw[..bpp]);
    let end = current.len().min(raw.len());
    for i in bpp..end {
        current[i] = raw[i].wrapping_add(current[i - bpp]);
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
    #[cfg(target_arch = "x86_64")]
    match bpp {
        3 => return sse::avg::<3>(prev, raw, current),
        4 => return sse::avg::<4>(prev, raw, current),
        6 => return sse::avg::<6>(prev, raw, current),
        8 => return sse::avg::<8>(prev, raw, current),
        _ => (),
    }
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

fn paeth_rows(prev: &[u8], raw: &[u8], current: &mut [u8], bpp: usize, simd: Simd) {
    if raw.len() < bpp || current.len() < bpp || prev.len() < bpp {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if simd.sse41 {
        match bpp {
            3 => return sse::paeth::<3>(prev, raw, current),
            4 => return sse::paeth::<4>(prev, raw, current),
            6 => return sse::paeth::<6>(prev, raw, current),
            8 => return sse::paeth::<8>(prev, raw, current),
            _ => (),
        }
    }
    let _ = simd;
    for i in 0..bpp {
        current[i] = raw[i].wrapping_add(paeth(0, prev[i], 0));
    }
    let end = current.len().min(raw.len()).min(prev.len());
    for i in bpp..end {
        let p = paeth(current[i - bpp], prev[i], prev[i - bpp]);
        current[i] = raw[i].wrapping_add(p);
    }
}

fn paeth_first(raw: &[u8], current: &mut [u8], bpp: usize) {
    if raw.len() < bpp || current.len() < bpp {
        return;
    }
    current[..bpp].copy_from_slice(&raw[..bpp]);
    let end = current.len().min(raw.len());
    for i in bpp..end {
        current[i] = raw[i].wrapping_add(paeth(current[i - bpp], 0, 0));
    }
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

#[cfg(target_arch = "x86_64")]
mod sse {
    use std::arch::x86_64::*;

    pub fn sub<const SIZE: usize>(raw: &[u8], current: &mut [u8]) {
        unsafe { sub_inner::<SIZE>(raw, current) }
    }

    #[target_feature(enable = "sse2")]
    fn sub_inner<const SIZE: usize>(raw: &[u8], current: &mut [u8]) {
        let mut lane = [0u8; 16];
        let mut d = _mm_setzero_si128();
        for (raw, out) in raw.chunks_exact(SIZE).zip(current.chunks_exact_mut(SIZE)) {
            lane[..SIZE].copy_from_slice(raw);
            let a = d;
            d = unsafe { _mm_loadu_si128(lane.as_ptr().cast()) };
            d = _mm_add_epi8(d, a);
            unsafe { _mm_storeu_si128(lane.as_mut_ptr().cast(), d) };
            out.copy_from_slice(&lane[..SIZE]);
        }
    }

    pub fn avg<const SIZE: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        unsafe { avg_inner::<SIZE>(prev, raw, current) }
    }

    #[target_feature(enable = "sse2")]
    fn avg_inner<const SIZE: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        let (mut x, mut y) = ([0u8; 16], [0u8; 16]);
        let mut d = _mm_setzero_si128();
        let one = _mm_set1_epi8(1);
        for ((prev, raw), out) in prev
            .chunks_exact(SIZE)
            .zip(raw.chunks_exact(SIZE))
            .zip(current.chunks_exact_mut(SIZE))
        {
            x[..SIZE].copy_from_slice(raw);
            y[..SIZE].copy_from_slice(prev);
            let b = unsafe { _mm_loadu_si128(y.as_ptr().cast()) };
            let a = d;
            d = unsafe { _mm_loadu_si128(x.as_ptr().cast()) };
            let mut avg = _mm_avg_epu8(a, b);
            avg = _mm_sub_epi8(avg, _mm_and_si128(_mm_xor_si128(a, b), one));
            d = _mm_add_epi8(d, avg);
            unsafe { _mm_storeu_si128(x.as_mut_ptr().cast(), d) };
            out.copy_from_slice(&x[..SIZE]);
        }
    }

    pub fn paeth<const SIZE: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        unsafe { paeth_inner::<SIZE>(prev, raw, current) }
    }

    #[target_feature(enable = "sse4.1")]
    fn paeth_inner<const SIZE: usize>(prev: &[u8], raw: &[u8], current: &mut [u8]) {
        let zero = _mm_setzero_si128();
        let (mut b, mut d) = (zero, zero);
        let (mut f, mut g) = ([0u8; 16], [0u8; 16]);
        for ((prev, raw), out) in prev
            .chunks_exact(SIZE)
            .zip(raw.chunks_exact(SIZE))
            .zip(current.chunks_exact_mut(SIZE))
        {
            f[..SIZE].copy_from_slice(prev);
            g[..SIZE].copy_from_slice(raw);
            let c = b;
            b = _mm_unpacklo_epi8(unsafe { _mm_loadu_si128(f.as_ptr().cast()) }, zero);
            let a = d;
            d = _mm_unpacklo_epi8(unsafe { _mm_loadu_si128(g.as_ptr().cast()) }, zero);
            let pa = _mm_sub_epi16(b, c);
            let pb = _mm_sub_epi16(a, c);
            let pc = _mm_abs_epi16(_mm_add_epi16(pa, pb));
            let pa = _mm_abs_epi16(pa);
            let pb = _mm_abs_epi16(pb);
            let smallest = _mm_min_epi16(pc, _mm_min_epi16(pa, pb));
            let nearest = _mm_blendv_epi8(
                _mm_blendv_epi8(c, b, _mm_cmpeq_epi16(smallest, pb)),
                a,
                _mm_cmpeq_epi16(smallest, pa),
            );
            d = _mm_add_epi8(d, nearest);
            unsafe { _mm_storeu_si128(f.as_mut_ptr().cast(), _mm_packus_epi16(d, d)) };
            out.copy_from_slice(&f[..SIZE]);
        }
    }
}
