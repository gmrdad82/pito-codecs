pub const BPS: usize = 32;

pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const TM_PRED: u8 = 3;
pub const B_PRED: u8 = 4;

pub const B_DC: u8 = 0;
pub const B_TM: u8 = 1;
pub const B_VE: u8 = 2;
pub const B_HE: u8 = 3;
pub const B_LD: u8 = 4;
pub const B_RD: u8 = 5;
pub const B_VR: u8 = 6;
pub const B_VL: u8 = 7;
pub const B_HD: u8 = 8;

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn mul1(a: i32) -> i32 {
    (a.wrapping_mul(20091) >> 16).wrapping_add(a)
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
fn mul2(a: i32) -> i32 {
    a.wrapping_mul(35468) >> 16
}

#[inline(always)]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[cfg(target_arch = "x86_64")]
pub use simd::{fdct, idct_add, nz_zigzag, sse4, sse8, sse16};

#[cfg(not(target_arch = "x86_64"))]
pub use self::{
    fdct_scalar as fdct, idct_add_scalar as idct_add, nz_zigzag_scalar as nz_zigzag,
    sse4_scalar as sse4, sse8_scalar as sse8, sse16_scalar as sse16,
};

const fn zigzag_lut(high: bool) -> [u16; 256] {
    let zz = super::tables::ZIGZAG;
    let mut inv = [0usize; 16];
    let mut i = 0;
    while i < 16 {
        inv[zz[i]] = i;
        i += 1;
    }
    let mut t = [0u16; 256];
    let mut m = 0;
    while m < 256 {
        let mut v = 0u16;
        let mut b = 0;
        while b < 8 {
            if m & (1 << b) != 0 {
                v |= 1 << inv[b + if high { 8 } else { 0 }];
            }
            b += 1;
        }
        t[m] = v;
        m += 1;
    }
    t
}

pub static ZZ_LO: [u16; 256] = zigzag_lut(false);
pub static ZZ_HI: [u16; 256] = zigzag_lut(true);

#[inline(always)]
pub fn natural_to_zigzag(m: u16) -> u16 {
    ZZ_LO[(m & 0xff) as usize] | ZZ_HI[(m >> 8) as usize]
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn nz_zigzag_scalar(l: &[i16; 16]) -> u16 {
    let mut m = 0u16;
    for (i, v) in l.iter().enumerate() {
        m |= ((*v != 0) as u16) << i;
    }
    natural_to_zigzag(m)
}

#[cfg(any(test, not(target_arch = "x86_64")))]
fn sse_scalar(a: &[u8], ao: usize, b: &[u8], bo: usize, n: usize) -> u64 {
    let mut s = 0u32;
    for r in 0..n {
        for (p, q) in a[ao + r * BPS..ao + r * BPS + n]
            .iter()
            .zip(&b[bo + r * BPS..bo + r * BPS + n])
        {
            let d = *p as i32 - *q as i32;
            s += (d * d) as u32;
        }
    }
    s as u64
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn sse4_scalar(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
    sse_scalar(a, ao, b, bo, 4)
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn sse8_scalar(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
    sse_scalar(a, ao, b, bo, 8)
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn sse16_scalar(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
    sse_scalar(a, ao, b, bo, 16)
}

#[cfg(any(test, not(target_arch = "x86_64")))]
#[inline(always)]
pub fn idct_add_scalar(c: &[i16], dst: &mut [u8], off: usize) {
    let c = &c[..16];
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a = (c[i] as i32).wrapping_add(c[8 + i] as i32);
        let b = (c[i] as i32).wrapping_sub(c[8 + i] as i32);
        let x = mul2(c[4 + i] as i32).wrapping_sub(mul1(c[12 + i] as i32));
        let y = mul1(c[4 + i] as i32).wrapping_add(mul2(c[12 + i] as i32));
        t[i] = a.wrapping_add(y);
        t[4 + i] = b.wrapping_add(x);
        t[8 + i] = b.wrapping_sub(x);
        t[12 + i] = a.wrapping_sub(y);
    }
    for r in 0..4 {
        let s = &t[r * 4..r * 4 + 4];
        let dc = s[0].wrapping_add(4);
        let a = dc.wrapping_add(s[2]);
        let b = dc.wrapping_sub(s[2]);
        let x = mul2(s[1]).wrapping_sub(mul1(s[3]));
        let y = mul1(s[1]).wrapping_add(mul2(s[3]));
        let d = &mut dst[off + r * BPS..off + r * BPS + 4];
        d[0] = clip8(d[0] as i32 + (a.wrapping_add(y) >> 3));
        d[1] = clip8(d[1] as i32 + (b.wrapping_add(x) >> 3));
        d[2] = clip8(d[2] as i32 + (b.wrapping_sub(x) >> 3));
        d[3] = clip8(d[3] as i32 + (a.wrapping_sub(y) >> 3));
    }
}

#[inline(always)]
pub fn dc_add(c: i16, dst: &mut [u8], off: usize) {
    let dc = (c as i32 + 4) >> 3;
    for r in 0..4 {
        for p in &mut dst[off + r * BPS..off + r * BPS + 4] {
            *p = clip8(*p as i32 + dc);
        }
    }
}

pub fn iwht(input: &[i16; 16], out: &mut [i16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a = input[i] as i32 + input[12 + i] as i32;
        let b = input[4 + i] as i32 + input[8 + i] as i32;
        let c = input[4 + i] as i32 - input[8 + i] as i32;
        let d = input[i] as i32 - input[12 + i] as i32;
        t[i] = a + b;
        t[4 + i] = c + d;
        t[8 + i] = a - b;
        t[12 + i] = d - c;
    }
    for r in 0..4 {
        let s = &t[r * 4..r * 4 + 4];
        let a = s[0] + s[3];
        let b = s[1] + s[2];
        let c = s[1] - s[2];
        let d = s[0] - s[3];
        out[(r * 4) * 16] = ((a + b + 3) >> 3) as i16;
        out[(r * 4 + 1) * 16] = ((c + d + 3) >> 3) as i16;
        out[(r * 4 + 2) * 16] = ((a - b + 3) >> 3) as i16;
        out[(r * 4 + 3) * 16] = ((d - c + 3) >> 3) as i16;
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn fdct_scalar(src: &[u8], so: usize, pred: &[u8], po: usize, out: &mut [i16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let s = &src[so + i * BPS..so + i * BPS + 4];
        let p = &pred[po + i * BPS..po + i * BPS + 4];
        let d0 = s[0] as i32 - p[0] as i32;
        let d1 = s[1] as i32 - p[1] as i32;
        let d2 = s[2] as i32 - p[2] as i32;
        let d3 = s[3] as i32 - p[3] as i32;
        let a0 = d0 + d3;
        let a1 = d1 + d2;
        let a2 = d1 - d2;
        let a3 = d0 - d3;
        t[i * 4] = (a0 + a1) * 8;
        t[i * 4 + 1] = (a2 * 2217 + a3 * 5352 + 1812) >> 9;
        t[i * 4 + 2] = (a0 - a1) * 8;
        t[i * 4 + 3] = (a3 * 2217 - a2 * 5352 + 937) >> 9;
    }
    for i in 0..4 {
        let a0 = t[i] + t[12 + i];
        let a1 = t[4 + i] + t[8 + i];
        let a2 = t[4 + i] - t[8 + i];
        let a3 = t[i] - t[12 + i];
        out[i] = ((a0 + a1 + 7) >> 4) as i16;
        out[4 + i] = (((a2 * 2217 + a3 * 5352 + 12000) >> 16) + (a3 != 0) as i32) as i16;
        out[8 + i] = ((a0 - a1 + 7) >> 4) as i16;
        out[12 + i] = ((a3 * 2217 - a2 * 5352 + 51000) >> 16) as i16;
    }
}

pub fn fwht(dc: &[i16], out: &mut [i16; 16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a0 = dc[i * 64] as i32 + dc[i * 64 + 32] as i32;
        let a1 = dc[i * 64 + 16] as i32 + dc[i * 64 + 48] as i32;
        let a2 = dc[i * 64 + 16] as i32 - dc[i * 64 + 48] as i32;
        let a3 = dc[i * 64] as i32 - dc[i * 64 + 32] as i32;
        t[i * 4] = a0 + a1;
        t[i * 4 + 1] = a3 + a2;
        t[i * 4 + 2] = a3 - a2;
        t[i * 4 + 3] = a0 - a1;
    }
    for i in 0..4 {
        let a0 = t[i] + t[8 + i];
        let a1 = t[4 + i] + t[12 + i];
        let a2 = t[4 + i] - t[12 + i];
        let a3 = t[i] - t[8 + i];
        let b0 = a0 + a1;
        let b1 = a3 + a2;
        let b2 = a3 - a2;
        let b3 = a0 - a1;
        out[i] = (b0 >> 1) as i16;
        out[4 + i] = (b1 >> 1) as i16;
        out[8 + i] = (b2 >> 1) as i16;
        out[12 + i] = (b3 >> 1) as i16;
    }
}

fn fill(dst: &mut [u8], off: usize, n: usize, v: u8) {
    for r in 0..n {
        dst[off + r * BPS..off + r * BPS + n].fill(v);
    }
}

pub fn pred_dc(dst: &mut [u8], off: usize, n: usize, top: bool, left: bool) {
    let shift = n.trailing_zeros();
    let mut s = 0u32;
    if top {
        s += dst[off - BPS..off - BPS + n]
            .iter()
            .map(|&v| v as u32)
            .sum::<u32>();
    }
    if left {
        s += (0..n).map(|r| dst[off + r * BPS - 1] as u32).sum::<u32>();
    }
    let v = match (top, left) {
        (true, true) => (s + n as u32) >> (shift + 1),
        (true, false) | (false, true) => (s + (n as u32 >> 1)) >> shift,
        (false, false) => 128,
    };
    fill(dst, off, n, v as u8);
}

pub fn pred_v(dst: &mut [u8], off: usize, n: usize) {
    let mut top = [0u8; 16];
    top[..n].copy_from_slice(&dst[off - BPS..off - BPS + n]);
    for r in 0..n {
        dst[off + r * BPS..off + r * BPS + n].copy_from_slice(&top[..n]);
    }
}

pub fn pred_h(dst: &mut [u8], off: usize, n: usize) {
    for r in 0..n {
        let v = dst[off + r * BPS - 1];
        dst[off + r * BPS..off + r * BPS + n].fill(v);
    }
}

pub fn pred_tm(dst: &mut [u8], off: usize, n: usize) {
    let mut top = [0i32; 16];
    let tl = dst[off - BPS - 1] as i32;
    for (i, t) in top[..n].iter_mut().enumerate() {
        *t = dst[off - BPS + i] as i32 - tl;
    }
    for r in 0..n {
        let l = dst[off + r * BPS - 1] as i32;
        for (p, t) in dst[off + r * BPS..off + r * BPS + n]
            .iter_mut()
            .zip(&top[..n])
        {
            *p = clip8(l + t);
        }
    }
}

pub fn pred_luma(mode: u8, dst: &mut [u8], off: usize, top: bool, left: bool) {
    match mode {
        V_PRED => pred_v(dst, off, 16),
        H_PRED => pred_h(dst, off, 16),
        TM_PRED => pred_tm(dst, off, 16),
        _ => pred_dc(dst, off, 16, top, left),
    }
}

pub fn pred_chroma(mode: u8, dst: &mut [u8], off: usize, top: bool, left: bool) {
    match mode {
        V_PRED => pred_v(dst, off, 8),
        H_PRED => pred_h(dst, off, 8),
        TM_PRED => pred_tm(dst, off, 8),
        _ => pred_dc(dst, off, 8, top, left),
    }
}

#[inline(always)]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((a as u32 + 2 * b as u32 + c as u32 + 2) >> 2) as u8
}

#[inline(always)]
fn avg2(a: u8, b: u8) -> u8 {
    ((a as u32 + b as u32 + 1) >> 1) as u8
}

pub fn pred4(mode: u8, d: &mut [u8], off: usize) {
    let t = off - BPS;
    let a = [
        d[t],
        d[t + 1],
        d[t + 2],
        d[t + 3],
        d[t + 4],
        d[t + 5],
        d[t + 6],
        d[t + 7],
    ];
    let tl = d[t - 1];
    let l = [
        d[off - 1],
        d[off + BPS - 1],
        d[off + 2 * BPS - 1],
        d[off + 3 * BPS - 1],
    ];
    let mut o = [[0u8; 4]; 4];
    match mode {
        B_DC => {
            let s: u32 = a[..4].iter().chain(&l).map(|&v| v as u32).sum();
            o = [[((s + 4) >> 3) as u8; 4]; 4];
        }
        B_TM => {
            for (r, row) in o.iter_mut().enumerate() {
                for (c, p) in row.iter_mut().enumerate() {
                    *p = clip8(l[r] as i32 + a[c] as i32 - tl as i32);
                }
            }
        }
        B_VE => {
            let v = [
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
            ];
            o = [v; 4];
        }
        B_HE => {
            o[0] = [avg3(tl, l[0], l[1]); 4];
            o[1] = [avg3(l[0], l[1], l[2]); 4];
            o[2] = [avg3(l[1], l[2], l[3]); 4];
            o[3] = [avg3(l[2], l[3], l[3]); 4];
        }
        B_LD => {
            let p = [
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
                avg3(a[3], a[4], a[5]),
                avg3(a[4], a[5], a[6]),
                avg3(a[5], a[6], a[7]),
                avg3(a[6], a[7], a[7]),
            ];
            for (r, row) in o.iter_mut().enumerate() {
                row.copy_from_slice(&p[r..r + 4]);
            }
        }
        B_RD => {
            let e = [l[3], l[2], l[1], l[0], tl, a[0], a[1], a[2], a[3]];
            let mut p = [0u8; 7];
            for (i, v) in p.iter_mut().enumerate() {
                *v = avg3(e[i], e[i + 1], e[i + 2]);
            }
            for (r, row) in o.iter_mut().enumerate() {
                row.copy_from_slice(&p[3 - r..7 - r]);
            }
        }
        B_VR => {
            o[0] = [
                avg2(tl, a[0]),
                avg2(a[0], a[1]),
                avg2(a[1], a[2]),
                avg2(a[2], a[3]),
            ];
            o[1] = [
                avg3(l[0], tl, a[0]),
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
            ];
            o[2] = [avg3(l[1], l[0], tl), o[0][0], o[0][1], o[0][2]];
            o[3] = [avg3(l[2], l[1], l[0]), o[1][0], o[1][1], o[1][2]];
        }
        B_VL => {
            o[0] = [
                avg2(a[0], a[1]),
                avg2(a[1], a[2]),
                avg2(a[2], a[3]),
                avg2(a[3], a[4]),
            ];
            o[1] = [
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
                avg3(a[3], a[4], a[5]),
            ];
            o[2] = [o[0][1], o[0][2], o[0][3], avg3(a[4], a[5], a[6])];
            o[3] = [o[1][1], o[1][2], o[1][3], avg3(a[5], a[6], a[7])];
        }
        B_HD => {
            o[0] = [
                avg2(l[0], tl),
                avg3(l[0], tl, a[0]),
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
            ];
            o[1] = [avg2(l[1], l[0]), avg3(l[1], l[0], tl), o[0][0], o[0][1]];
            o[2] = [avg2(l[2], l[1]), avg3(l[2], l[1], l[0]), o[1][0], o[1][1]];
            o[3] = [avg2(l[3], l[2]), avg3(l[3], l[2], l[1]), o[2][0], o[2][1]];
        }
        _ => {
            o[0] = [
                avg2(l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg2(l[1], l[2]),
                avg3(l[1], l[2], l[3]),
            ];
            o[1] = [o[0][2], o[0][3], avg2(l[2], l[3]), avg3(l[2], l[3], l[3])];
            o[2] = [o[1][2], o[1][3], l[3], l[3]];
            o[3] = [l[3]; 4];
        }
    }
    for (r, row) in o.iter().enumerate() {
        d[off + r * BPS..off + r * BPS + 4].copy_from_slice(row);
    }
}

#[cfg(target_arch = "x86_64")]
mod simd {
    use super::BPS;
    use std::arch::x86_64::*;

    #[inline(always)]
    fn mul1(x: __m128i) -> __m128i {
        unsafe { _mm_add_epi16(x, _mm_mulhi_epi16(x, _mm_set1_epi16(20091))) }
    }

    #[inline(always)]
    fn mul2(x: __m128i) -> __m128i {
        unsafe { _mm_add_epi16(x, _mm_mulhi_epi16(x, _mm_set1_epi16(-30068))) }
    }

    #[inline(always)]
    fn ipass(i0: __m128i, i1: __m128i, i2: __m128i, i3: __m128i) -> [__m128i; 4] {
        unsafe {
            let a = _mm_add_epi16(i0, i2);
            let b = _mm_sub_epi16(i0, i2);
            let c = _mm_sub_epi16(mul2(i1), mul1(i3));
            let d = _mm_add_epi16(mul1(i1), mul2(i3));
            [
                _mm_add_epi16(a, d),
                _mm_add_epi16(b, c),
                _mm_sub_epi16(b, c),
                _mm_sub_epi16(a, d),
            ]
        }
    }

    #[inline(always)]
    fn transpose(r: [__m128i; 4]) -> [__m128i; 4] {
        unsafe {
            let t0 = _mm_unpacklo_epi16(r[0], r[1]);
            let t1 = _mm_unpacklo_epi16(r[2], r[3]);
            let u0 = _mm_unpacklo_epi32(t0, t1);
            let u1 = _mm_unpackhi_epi32(t0, t1);
            [
                u0,
                _mm_unpackhi_epi64(u0, u0),
                u1,
                _mm_unpackhi_epi64(u1, u1),
            ]
        }
    }

    #[inline(always)]
    unsafe fn load4(p: *const u8) -> __m128i {
        unsafe { _mm_cvtsi32_si128(std::ptr::read_unaligned(p as *const i32)) }
    }

    #[inline(always)]
    pub fn idct_add(c: &[i16], dst: &mut [u8], off: usize) {
        assert!(c.len() >= 16 && off + 3 * BPS + 4 <= dst.len());
        unsafe {
            let p = c.as_ptr();
            let r = [
                _mm_loadl_epi64(p.cast()),
                _mm_loadl_epi64(p.add(4).cast()),
                _mm_loadl_epi64(p.add(8).cast()),
                _mm_loadl_epi64(p.add(12).cast()),
            ];
            let t = transpose(ipass(r[0], r[1], r[2], r[3]));
            let o = ipass(_mm_add_epi16(t[0], _mm_set1_epi16(4)), t[1], t[2], t[3]);
            let o = transpose([
                _mm_srai_epi16(o[0], 3),
                _mm_srai_epi16(o[1], 3),
                _mm_srai_epi16(o[2], 3),
                _mm_srai_epi16(o[3], 3),
            ]);
            let z = _mm_setzero_si128();
            let d = dst.as_mut_ptr().add(off);
            for (i, row) in o.iter().enumerate() {
                let q = d.add(i * BPS);
                let px = _mm_unpacklo_epi8(load4(q), z);
                let v = _mm_packus_epi16(_mm_add_epi16(px, *row), z);
                std::ptr::write_unaligned(q as *mut i32, _mm_cvtsi128_si32(v));
            }
        }
    }

    #[inline(always)]
    pub fn fdct(src: &[u8], so: usize, pred: &[u8], po: usize, out: &mut [i16]) {
        assert!(so + 3 * BPS + 4 <= src.len() && po + 3 * BPS + 4 <= pred.len() && out.len() >= 16);
        unsafe {
            let z = _mm_setzero_si128();
            let sp = src.as_ptr().add(so);
            let pp = pred.as_ptr().add(po);
            let d: [__m128i; 4] = std::array::from_fn(|r| {
                _mm_sub_epi16(
                    _mm_unpacklo_epi8(load4(sp.add(r * BPS)), z),
                    _mm_unpacklo_epi8(load4(pp.add(r * BPS)), z),
                )
            });
            let c = transpose(d);
            let a0 = _mm_add_epi16(c[0], c[3]);
            let a1 = _mm_add_epi16(c[1], c[2]);
            let a2 = _mm_sub_epi16(c[1], c[2]);
            let a3 = _mm_sub_epi16(c[0], c[3]);
            let k1 = _mm_setr_epi16(2217, 5352, 2217, 5352, 2217, 5352, 2217, 5352);
            let k3 = _mm_setr_epi16(-5352, 2217, -5352, 2217, -5352, 2217, -5352, 2217);
            let m = _mm_unpacklo_epi16(a2, a3);
            let t0 = _mm_slli_epi16(_mm_add_epi16(a0, a1), 3);
            let t2 = _mm_slli_epi16(_mm_sub_epi16(a0, a1), 3);
            let t1 = _mm_srai_epi32(
                _mm_add_epi32(_mm_madd_epi16(m, k1), _mm_set1_epi32(1812)),
                9,
            );
            let t3 = _mm_srai_epi32(_mm_add_epi32(_mm_madd_epi16(m, k3), _mm_set1_epi32(937)), 9);
            let r = transpose([t0, _mm_packs_epi32(t1, t1), t2, _mm_packs_epi32(t3, t3)]);
            let a0 = _mm_add_epi16(r[0], r[3]);
            let a1 = _mm_add_epi16(r[1], r[2]);
            let a2 = _mm_sub_epi16(r[1], r[2]);
            let a3 = _mm_sub_epi16(r[0], r[3]);
            let k7 = _mm_set1_epi16(7);
            let o0 = _mm_srai_epi16(_mm_add_epi16(_mm_add_epi16(a0, a1), k7), 4);
            let o2 = _mm_srai_epi16(_mm_add_epi16(_mm_sub_epi16(a0, a1), k7), 4);
            let m = _mm_unpacklo_epi16(a2, a3);
            let o1 = _mm_srai_epi32(
                _mm_add_epi32(_mm_madd_epi16(m, k1), _mm_set1_epi32(12000)),
                16,
            );
            let o3 = _mm_srai_epi32(
                _mm_add_epi32(_mm_madd_epi16(m, k3), _mm_set1_epi32(51000)),
                16,
            );
            let ne = _mm_andnot_si128(_mm_cmpeq_epi16(a3, z), _mm_set1_epi16(1));
            let o1 = _mm_add_epi16(_mm_packs_epi32(o1, o1), ne);
            let o3 = _mm_packs_epi32(o3, o3);
            let p = out.as_mut_ptr();
            _mm_storeu_si128(p.cast(), _mm_unpacklo_epi64(o0, o1));
            _mm_storeu_si128(p.add(8).cast(), _mm_unpacklo_epi64(o2, o3));
        }
    }

    #[inline(always)]
    pub fn nz_zigzag(l: &[i16; 16]) -> u16 {
        unsafe {
            let p = l.as_ptr();
            let v = _mm_packs_epi16(_mm_loadu_si128(p.cast()), _mm_loadu_si128(p.add(8).cast()));
            let zero = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_setzero_si128())) as u16;
            super::natural_to_zigzag(!zero)
        }
    }

    #[inline(always)]
    fn rows_sse(a: *const u8, b: *const u8, n: usize, wide: bool) -> u64 {
        unsafe {
            let z = _mm_setzero_si128();
            let mut acc = _mm_setzero_si128();
            for r in 0..n {
                let (x, y) = if wide {
                    (
                        _mm_loadu_si128(a.add(r * BPS).cast()),
                        _mm_loadu_si128(b.add(r * BPS).cast()),
                    )
                } else {
                    (
                        _mm_loadl_epi64(a.add(r * BPS).cast()),
                        _mm_loadl_epi64(b.add(r * BPS).cast()),
                    )
                };
                let dl = _mm_sub_epi16(_mm_unpacklo_epi8(x, z), _mm_unpacklo_epi8(y, z));
                acc = _mm_add_epi32(acc, _mm_madd_epi16(dl, dl));
                if wide {
                    let dh = _mm_sub_epi16(_mm_unpackhi_epi8(x, z), _mm_unpackhi_epi8(y, z));
                    acc = _mm_add_epi32(acc, _mm_madd_epi16(dh, dh));
                }
            }
            let s = _mm_add_epi32(acc, _mm_shuffle_epi32(acc, 0b01_00_11_10));
            let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b10_11_00_01));
            _mm_cvtsi128_si32(s) as u32 as u64
        }
    }

    #[inline(always)]
    pub fn sse16(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
        assert!(ao + 15 * BPS + 16 <= a.len() && bo + 15 * BPS + 16 <= b.len());
        unsafe { rows_sse(a.as_ptr().add(ao), b.as_ptr().add(bo), 16, true) }
    }

    #[inline(always)]
    pub fn sse8(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
        assert!(ao + 7 * BPS + 8 <= a.len() && bo + 7 * BPS + 8 <= b.len());
        unsafe { rows_sse(a.as_ptr().add(ao), b.as_ptr().add(bo), 8, false) }
    }

    #[inline(always)]
    pub fn sse4(a: &[u8], ao: usize, b: &[u8], bo: usize) -> u64 {
        assert!(ao + 3 * BPS + 4 <= a.len() && bo + 3 * BPS + 4 <= b.len());
        unsafe {
            let z = _mm_setzero_si128();
            let (pa, pb) = (a.as_ptr().add(ao), b.as_ptr().add(bo));
            let x = _mm_unpacklo_epi32(load4(pa), load4(pa.add(BPS)));
            let x2 = _mm_unpacklo_epi32(load4(pa.add(2 * BPS)), load4(pa.add(3 * BPS)));
            let y = _mm_unpacklo_epi32(load4(pb), load4(pb.add(BPS)));
            let y2 = _mm_unpacklo_epi32(load4(pb.add(2 * BPS)), load4(pb.add(3 * BPS)));
            let d0 = _mm_sub_epi16(_mm_unpacklo_epi8(x, z), _mm_unpacklo_epi8(y, z));
            let d1 = _mm_sub_epi16(_mm_unpacklo_epi8(x2, z), _mm_unpacklo_epi8(y2, z));
            let acc = _mm_add_epi32(_mm_madd_epi16(d0, d0), _mm_madd_epi16(d1, d1));
            let s = _mm_add_epi32(acc, _mm_shuffle_epi32(acc, 0b01_00_11_10));
            let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b10_11_00_01));
            _mm_cvtsi128_si32(s) as u32 as u64
        }
    }
}
