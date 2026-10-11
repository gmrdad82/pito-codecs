use crate::vp9::frame::{Frame, Pixel};
use crate::vp9::tables::SUBPEL_FILTERS;

#[inline(always)]
fn taps(filter: u8, frac: usize) -> &'static [i16] {
    let o = (filter as usize * 16 + frac) * 8;
    &SUBPEL_FILTERS[o..o + 8]
}

pub struct RefPlane<'a> {
    pub frame: &'a Frame,
    pub plane: usize,
    pub last_x: i32,
    pub last_y: i32,
}

impl RefPlane<'_> {
    #[inline]
    fn rows<P: Pixel>(&self, y0: isize, y1: isize) -> (&[P], usize) {
        let f = self.frame;
        let g = &f.geo[self.plane];
        f.wait_plane_row(self.plane, y1 - 1);
        let start = (g.origin as isize + y0 * g.stride as isize - g.border_x as isize) as usize;
        let len = (y1 - y0) as usize * g.stride;
        (&f.plane::<P>(self.plane)[start..start + len], g.border_x)
    }

    #[inline]
    fn wait_row(&self, y: i32) {
        self.frame
            .wait_plane_row(self.plane, y.clamp(0, self.last_y) as isize);
    }

    #[inline]
    fn row<P: Pixel>(&self, y: i32) -> &[P] {
        let g = &self.frame.geo[self.plane];
        let start = g.origin + y.clamp(0, self.last_y) as usize * g.stride;
        &self.frame.plane::<P>(self.plane)[start..start + self.last_x as usize + 1]
    }
}

#[allow(clippy::too_many_arguments)]
pub fn predict<P: Pixel>(
    r: &RefPlane,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [P],
    os: usize,
    avg: bool,
    tmp: &mut [u16],
    win: &mut [P],
    pred: &mut [P],
) {
    #[cfg(target_arch = "x86_64")]
    if step_x == 16 && step_y == 16 && matches!(w, 4 | 8 | 16 | 32 | 64) && crate::vp9::isa::avx2()
    {
        let (src, ss, so) = source(r, x >> 4, y >> 4, w, h, win);
        let (fx, fy) = ((x & 15) as usize, (y & 15) as usize);
        if let (Some(src), Some(out)) = (P::wide(src), P::wide_mut(&mut *out)) {
            unsafe {
                simd::unscaled(
                    src, ss, so, w, h, fx, fy, filter, bit_depth, out, os, avg, tmp,
                )
            };
            return;
        }
        if let (Some(src), Some(out)) = (P::narrow(src), P::narrow_mut(&mut *out))
            && simd8::unscaled(src, ss, so, w, h, fx, fy, filter, out, os, avg, tmp)
        {
            return;
        }
    }
    if avg {
        predict_portable(
            r, x, y, step_x, step_y, w, h, filter, bit_depth, pred, w, tmp, win,
        );
        average(out, os, pred, w, h);
    } else {
        predict_portable(
            r, x, y, step_x, step_y, w, h, filter, bit_depth, out, os, tmp, win,
        );
    }
}

crate::vp9::isa::dispatch! {
fn average<P: Pixel>(out: &mut [P], os: usize, pred: &[P], w: usize, h: usize) {
    for i in 0..h {
        for (d, &b) in out[i * os..i * os + w].iter_mut().zip(&pred[i * w..i * w + w]) {
            *d = P::put((d.get() + b.get() + 1) >> 1);
        }
    }
}
}

crate::vp9::isa::dispatch! {
#[allow(clippy::too_many_arguments)]
fn predict_portable<P: Pixel>(
    r: &RefPlane,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [P],
    os: usize,
    tmp: &mut [u16],
    win: &mut [P],
) {
    if step_x == 16 && step_y == 16 {
        predict_unscaled(r, x, y, w, h, filter, bit_depth, out, os, tmp, win);
    } else {
        predict_scaled(
            r, x, y, step_x, step_y, w, h, filter, bit_depth, out, os, tmp,
        );
    }
}
}

fn fetch_window<P: Pixel>(
    r: &RefPlane,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    win: &mut [P],
) -> usize {
    let ws = w + 7;
    let xs = x0 - 3;
    let a = (-xs).clamp(0, ws as i32);
    let b = (r.last_x + 1 - xs).clamp(0, ws as i32);
    let (a, b) = (a as usize, b.max(a) as usize);
    r.wait_row(y0 + h as i32 + 3);
    for (row, dst) in win[..(h + 7) * ws].chunks_exact_mut(ws).enumerate() {
        let src = r.row::<P>(y0 - 3 + row as i32);
        dst[..a].fill(src[0]);
        if b > a {
            let s = (xs + a as i32) as usize;
            dst[a..b].copy_from_slice(&src[s..s + b - a]);
        }
        dst[b..].fill(src[r.last_x as usize]);
    }
    ws
}

fn source<'a, P: Pixel>(
    r: &'a RefPlane,
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    win: &'a mut [P],
) -> (&'a [P], usize, usize) {
    let g = &r.frame.geo[r.plane];
    let inside = x0 - 3 >= -(g.border_x as i32)
        && y0 - 3 >= -(g.border_y as i32)
        && x0 + w as i32 + 5 <= (g.stride - g.border_x) as i32
        && y0 + h as i32 + 5 <= (g.aligned_h + g.border_y) as i32
        && r.last_x == g.width as i32 - 1
        && r.last_y == g.height as i32 - 1;
    if inside {
        let (rows, bx) = r.rows::<P>((y0 - 3) as isize, (y0 + h as i32 + 5) as isize);
        (rows, g.stride, (bx as i32 + x0 - 3) as usize)
    } else {
        let ws = fetch_window(r, x0, y0, w, h, win);
        (&win[..], ws, 0)
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_unscaled<P: Pixel>(
    r: &RefPlane,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [P],
    os: usize,
    tmp: &mut [u16],
    win: &mut [P],
) {
    let (src, ss, so) = source(r, x >> 4, y >> 4, w, h, win);
    let (fx, fy) = ((x & 15) as usize, (y & 15) as usize);
    unscaled_scalar(src, ss, so, w, h, fx, fy, filter, bit_depth, out, os, tmp);
}

#[allow(clippy::too_many_arguments)]
fn unscaled_scalar<P: Pixel>(
    src: &[P],
    ss: usize,
    so: usize,
    w: usize,
    h: usize,
    fx: usize,
    fy: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [P],
    os: usize,
    tmp: &mut [u16],
) {
    let max = (1i32 << bit_depth) - 1;
    match (fx, fy) {
        (0, 0) => {
            for i in 0..h {
                let s = so + (i + 3) * ss + 3;
                out[i * os..i * os + w].copy_from_slice(&src[s..s + w]);
            }
        }
        (_, 0) => {
            let f = taps(filter, fx);
            for i in 0..h {
                filter_h(
                    &src[so + (i + 3) * ss..],
                    f,
                    max,
                    &mut out[i * os..i * os + w],
                );
            }
        }
        (0, _) => {
            let f = taps(filter, fy);
            for i in 0..h {
                filter_v(
                    &src[so + i * ss + 3..],
                    ss,
                    f,
                    max,
                    &mut out[i * os..i * os + w],
                );
            }
        }
        _ => {
            let fh = taps(filter, fx);
            let fv = taps(filter, fy);
            for i in 0..h + 7 {
                filter_h(&src[so + i * ss..], fh, max, &mut tmp[i * w..i * w + w]);
            }
            for i in 0..h {
                filter_v(&tmp[i * w..], w, fv, max, &mut out[i * os..i * os + w]);
            }
        }
    }
}

#[inline(always)]
fn filter_h<S: Pixel, D: Pixel>(s: &[S], f: &[i16], max: i32, o: &mut [D]) {
    match o.len() {
        4 => filter_h_n::<S, D, 4>(s, f, max, o),
        8 => filter_h_n::<S, D, 8>(s, f, max, o),
        16 => filter_h_n::<S, D, 16>(s, f, max, o),
        32 => filter_h_n::<S, D, 32>(s, f, max, o),
        64 => filter_h_n::<S, D, 64>(s, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * s[j + t].get()).sum();
                *d = D::put(((acc + 64) >> 7).clamp(0, max));
            }
        }
    }
}

#[inline(always)]
fn filter_h_n<S: Pixel, D: Pixel, const W: usize>(s: &[S], f: &[i16], max: i32, o: &mut [D]) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j].get() as u16));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(t..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j].get();
        }
    }
    for j in 0..W {
        o[j] = D::put((acc[j] >> 7).clamp(0, max));
    }
}

#[inline(always)]
fn narrow8<D: Pixel, const W: usize>(acc: &[u16; W], o: &mut [D; W]) {
    for j in 0..W {
        o[j] = D::put(if acc[j] < 0xC000 {
            (acc[j] >> 7).min(255) as i32
        } else {
            0
        });
    }
}

#[inline(always)]
fn filter_v<S: Pixel, D: Pixel>(s: &[S], ss: usize, f: &[i16], max: i32, o: &mut [D]) {
    match o.len() {
        4 => filter_v_n::<S, D, 4>(s, ss, f, max, o),
        8 => filter_v_n::<S, D, 8>(s, ss, f, max, o),
        16 => filter_v_n::<S, D, 16>(s, ss, f, max, o),
        32 => filter_v_n::<S, D, 32>(s, ss, f, max, o),
        64 => filter_v_n::<S, D, 64>(s, ss, f, max, o),
        _ => {
            for (j, d) in o.iter_mut().enumerate() {
                let acc: i32 = (0..8).map(|t| f[t] as i32 * s[j + t * ss].get()).sum();
                *d = D::put(((acc + 64) >> 7).clamp(0, max));
            }
        }
    }
}

#[inline(always)]
fn filter_v_n<S: Pixel, D: Pixel, const W: usize>(
    s: &[S],
    ss: usize,
    f: &[i16],
    max: i32,
    o: &mut [D],
) {
    let Some(o) = o.first_chunk_mut::<W>() else {
        return;
    };
    if max == 255 {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(t * ss..).and_then(|s| s.first_chunk::<W>()) else {
                return;
            };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j].get() as u16));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(t * ss..).and_then(|s| s.first_chunk::<W>()) else {
            return;
        };
        for j in 0..W {
            acc[j] += c * p[j].get();
        }
    }
    for j in 0..W {
        o[j] = D::put((acc[j] >> 7).clamp(0, max));
    }
}

#[allow(clippy::too_many_arguments)]
fn predict_scaled<P: Pixel>(
    r: &RefPlane,
    x: i32,
    y: i32,
    step_x: i32,
    step_y: i32,
    w: usize,
    h: usize,
    filter: u8,
    bit_depth: u8,
    out: &mut [P],
    os: usize,
    tmp: &mut [u16],
) {
    let max = (1i32 << bit_depth) - 1;
    let ih = ((((h as i32 - 1) * step_y + 15) >> 4) + 8) as usize;
    r.wait_row((y >> 4) + ih as i32 - 4);
    for row in 0..ih {
        let src = r.row::<P>((y >> 4) + row as i32 - 3);
        for c in 0..w {
            let p = x + step_x * c as i32;
            let f = taps(filter, (p & 15) as usize);
            let mut acc = 0i32;
            for t in 0..8 {
                acc +=
                    f[t] as i32 * src[((p >> 4) + t as i32 - 3).clamp(0, r.last_x) as usize].get();
            }
            tmp[row * w + c] = ((acc + 64) >> 7).clamp(0, max) as u16;
        }
    }
    for rr in 0..h {
        let p = (y & 15) + step_y * rr as i32;
        let f = taps(filter, (p & 15) as usize);
        let base = (p >> 4) as usize;
        for c in 0..w {
            let mut acc = 0i32;
            for t in 0..8 {
                acc += f[t] as i32 * tmp[(base + t) * w + c] as i32;
            }
            out[rr * os + c] = P::put(((acc + 64) >> 7).clamp(0, max));
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod simd {
    use super::taps;
    use std::arch::x86_64::*;

    #[allow(clippy::too_many_arguments)]
    pub unsafe fn unscaled(
        src: &[u16],
        ss: usize,
        so: usize,
        w: usize,
        h: usize,
        fx: usize,
        fy: usize,
        filter: u8,
        bit_depth: u8,
        out: &mut [u16],
        os: usize,
        avg: bool,
        tmp: &mut [u16],
    ) {
        assert!(
            w.is_power_of_two()
                && (4..=64).contains(&w)
                && h > 0
                && src.len() >= so + (h + 6) * ss + w + 7
                && out.len() >= (h - 1) * os + w
                && tmp.len() >= (h + 7) * w
        );
        let k = Kernel {
            src: unsafe { src.as_ptr().add(so) },
            ss,
            h,
            fx,
            fy,
            filter,
            max: (1i32 << bit_depth) - 1,
            out: out.as_mut_ptr(),
            os,
            tmp: tmp.as_mut_ptr(),
        };
        unsafe {
            match (w, bit_depth > 8, avg) {
                (4, false, false) => k.run::<4, false, false>(),
                (4, false, true) => k.run::<4, false, true>(),
                (4, true, false) => k.run::<4, true, false>(),
                (4, true, true) => k.run::<4, true, true>(),
                (8, false, false) => k.run::<8, false, false>(),
                (8, false, true) => k.run::<8, false, true>(),
                (8, true, false) => k.run::<8, true, false>(),
                (8, true, true) => k.run::<8, true, true>(),
                (16, false, false) => k.run::<16, false, false>(),
                (16, false, true) => k.run::<16, false, true>(),
                (16, true, false) => k.run::<16, true, false>(),
                (16, true, true) => k.run::<16, true, true>(),
                (32, false, false) => k.run::<32, false, false>(),
                (32, false, true) => k.run::<32, false, true>(),
                (32, true, false) => k.run::<32, true, false>(),
                (32, true, true) => k.run::<32, true, true>(),
                (64, false, false) => k.run::<64, false, false>(),
                (64, false, true) => k.run::<64, false, true>(),
                (64, true, false) => k.run::<64, true, false>(),
                _ => k.run::<64, true, true>(),
            }
        }
    }

    struct Kernel {
        src: *const u16,
        ss: usize,
        h: usize,
        fx: usize,
        fy: usize,
        filter: u8,
        max: i32,
        out: *mut u16,
        os: usize,
        tmp: *mut u16,
    }

    impl Kernel {
        #[target_feature(enable = "avx2")]
        unsafe fn run<const W: usize, const HBD: bool, const AVG: bool>(&self) {
            let (s, ss, h, o, os) = (self.src, self.ss, self.h, self.out, self.os);
            unsafe {
                match (self.fx, self.fy) {
                    (0, 0) => copy::<W, AVG>(s.add(3 * ss + 3), ss, h, o, os),
                    (fx, 0) => pass::<W, HBD, AVG>(
                        s.add(3 * ss),
                        ss,
                        1,
                        h,
                        taps(self.filter, fx),
                        self.max,
                        o,
                        os,
                    ),
                    (0, fy) => pass::<W, HBD, AVG>(
                        s.add(3),
                        ss,
                        ss,
                        h,
                        taps(self.filter, fy),
                        self.max,
                        o,
                        os,
                    ),
                    (fx, fy) => {
                        pass::<W, HBD, false>(
                            s,
                            ss,
                            1,
                            h + 7,
                            taps(self.filter, fx),
                            self.max,
                            self.tmp,
                            W,
                        );
                        pass::<W, HBD, AVG>(
                            self.tmp,
                            W,
                            W,
                            h,
                            taps(self.filter, fy),
                            self.max,
                            o,
                            os,
                        );
                    }
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn load<const N: usize>(p: *const u16) -> __m128i {
        unsafe {
            if N == 4 {
                _mm_loadl_epi64(p.cast())
            } else {
                _mm_loadu_si128(p.cast())
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn store<const N: usize>(p: *mut u16, v: __m128i) {
        unsafe {
            if N == 4 {
                _mm_storel_epi64(p.cast(), v)
            } else {
                _mm_storeu_si128(p.cast(), v)
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn copy<const W: usize, const AVG: bool>(
        s: *const u16,
        ss: usize,
        h: usize,
        o: *mut u16,
        os: usize,
    ) {
        unsafe {
            for i in 0..h {
                let (p, d) = (s.add(i * ss), o.add(i * os));
                if W >= 16 {
                    for j in (0..W).step_by(16) {
                        let mut v = _mm256_loadu_si256(p.add(j).cast());
                        if AVG {
                            v = _mm256_avg_epu16(v, _mm256_loadu_si256(d.add(j).cast()));
                        }
                        _mm256_storeu_si256(d.add(j).cast(), v);
                    }
                } else {
                    let mut v = load::<W>(p);
                    if AVG {
                        v = _mm_avg_epu16(v, load::<W>(d));
                    }
                    store::<W>(d, v);
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn pairs(f: &[i16]) -> [i32; 4] {
        std::array::from_fn(|k| (f[2 * k] as u16 as i32) | ((f[2 * k + 1] as i32) << 16))
    }

    #[allow(clippy::too_many_arguments)]
    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn pass<const W: usize, const HBD: bool, const AVG: bool>(
        s: *const u16,
        ss: usize,
        step: usize,
        rows: usize,
        f: &[i16],
        max: i32,
        o: *mut u16,
        os: usize,
    ) {
        unsafe {
            let p2 = pairs(f);
            if W >= 16 {
                let c8: [__m256i; 8] = std::array::from_fn(|t| _mm256_set1_epi16(f[t]));
                let c4: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi32(p2[k]));
                let m = _mm256_set1_epi16(max as i16);
                for i in 0..rows {
                    let (p, d) = (s.add(i * ss), o.add(i * os));
                    for j in (0..W).step_by(16) {
                        let mut v = if HBD {
                            wide_hbd(p.add(j), step, &c4, m)
                        } else {
                            wide_8(p.add(j), step, &c8)
                        };
                        if AVG {
                            v = _mm256_avg_epu16(v, _mm256_loadu_si256(d.add(j).cast()));
                        }
                        _mm256_storeu_si256(d.add(j).cast(), v);
                    }
                }
            } else {
                let c8: [__m128i; 8] = std::array::from_fn(|t| _mm_set1_epi16(f[t]));
                let c4: [__m128i; 4] = std::array::from_fn(|k| _mm_set1_epi32(p2[k]));
                let m = _mm_set1_epi16(max as i16);
                for i in 0..rows {
                    let (p, d) = (s.add(i * ss), o.add(i * os));
                    let mut v = if HBD {
                        narrow_hbd::<W>(p, step, &c4, m)
                    } else {
                        narrow_8::<W>(p, step, &c8)
                    };
                    if AVG {
                        v = _mm_avg_epu16(v, load::<W>(d));
                    }
                    store::<W>(d, v);
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn wide_8(p: *const u16, step: usize, c: &[__m256i; 8]) -> __m256i {
        unsafe {
            let mut a = _mm256_set1_epi16(0x4040);
            for (t, &k) in c.iter().enumerate() {
                let x = _mm256_loadu_si256(p.add(t * step).cast());
                a = _mm256_add_epi16(a, _mm256_mullo_epi16(x, k));
            }
            _mm256_min_epu16(
                _mm256_subs_epu16(_mm256_srli_epi16::<7>(a), _mm256_set1_epi16(128)),
                _mm256_set1_epi16(255),
            )
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn wide_hbd(p: *const u16, step: usize, c: &[__m256i; 4], m: __m256i) -> __m256i {
        unsafe {
            let mut lo = _mm256_set1_epi32(64);
            let mut hi = lo;
            for (k, &ck) in c.iter().enumerate() {
                let a = _mm256_loadu_si256(p.add(2 * k * step).cast());
                let b = _mm256_loadu_si256(p.add((2 * k + 1) * step).cast());
                lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), ck));
                hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), ck));
            }
            _mm256_min_epu16(
                _mm256_packus_epi32(_mm256_srai_epi32::<7>(lo), _mm256_srai_epi32::<7>(hi)),
                m,
            )
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn narrow_8<const N: usize>(p: *const u16, step: usize, c: &[__m128i; 8]) -> __m128i {
        unsafe {
            let mut a = _mm_set1_epi16(0x4040);
            for (t, &k) in c.iter().enumerate() {
                a = _mm_add_epi16(a, _mm_mullo_epi16(load::<N>(p.add(t * step)), k));
            }
            _mm_min_epu16(
                _mm_subs_epu16(_mm_srli_epi16::<7>(a), _mm_set1_epi16(128)),
                _mm_set1_epi16(255),
            )
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn narrow_hbd<const N: usize>(
        p: *const u16,
        step: usize,
        c: &[__m128i; 4],
        m: __m128i,
    ) -> __m128i {
        unsafe {
            let mut lo = _mm_set1_epi32(64);
            let mut hi = lo;
            for (k, &ck) in c.iter().enumerate() {
                let a = load::<N>(p.add(2 * k * step));
                let b = load::<N>(p.add((2 * k + 1) * step));
                lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), ck));
                if N == 8 {
                    hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), ck));
                }
            }
            _mm_min_epu16(
                _mm_packus_epi32(_mm_srai_epi32::<7>(lo), _mm_srai_epi32::<7>(hi)),
                m,
            )
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod simd8 {
    use super::taps;
    use std::arch::x86_64::*;

    #[allow(clippy::too_many_arguments)]
    pub fn unscaled(
        src: &[u8],
        ss: usize,
        so: usize,
        w: usize,
        h: usize,
        fx: usize,
        fy: usize,
        filter: u8,
        out: &mut [u8],
        os: usize,
        avg: bool,
        tmp: &mut [u16],
    ) -> bool {
        let fits = w.is_power_of_two()
            && (4..=64).contains(&w)
            && h > 0
            && src.len() >= so + (h + 6) * ss + w + 16
            && out.len() >= (h - 1) * os + w
            && tmp.len() * 2 >= (h + 7) * w;
        if !fits {
            return false;
        }
        let k = Kernel {
            src: unsafe { src.as_ptr().add(so) },
            ss,
            h,
            fx,
            fy,
            filter,
            out: out.as_mut_ptr(),
            os,
            tmp: tmp.as_mut_ptr().cast(),
        };
        unsafe {
            match (w, avg) {
                (4, false) => k.run::<4, false>(),
                (4, true) => k.run::<4, true>(),
                (8, false) => k.run::<8, false>(),
                (8, true) => k.run::<8, true>(),
                (16, false) => k.run::<16, false>(),
                (16, true) => k.run::<16, true>(),
                (32, false) => k.run::<32, false>(),
                (32, true) => k.run::<32, true>(),
                (64, false) => k.run::<64, false>(),
                _ => k.run::<64, true>(),
            }
        }
        true
    }

    struct Kernel {
        src: *const u8,
        ss: usize,
        h: usize,
        fx: usize,
        fy: usize,
        filter: u8,
        out: *mut u8,
        os: usize,
        tmp: *mut u8,
    }

    impl Kernel {
        #[target_feature(enable = "avx2")]
        unsafe fn run<const W: usize, const AVG: bool>(&self) {
            let (s, ss, h, o, os) = (self.src, self.ss, self.h, self.out, self.os);
            unsafe {
                match (self.fx, self.fy) {
                    (0, 0) => copy::<W, AVG>(s.add(3 * ss + 3), ss, h, o, os),
                    (fx, 0) => {
                        hpass::<W, AVG>(s.add(3 * ss), ss, h, coefs(taps(self.filter, fx)), o, os)
                    }
                    (0, fy) => {
                        vpass::<W, AVG>(s.add(3), ss, h, coefs(taps(self.filter, fy)), o, os)
                    }
                    (fx, fy) => {
                        hpass::<W, false>(s, ss, h + 7, coefs(taps(self.filter, fx)), self.tmp, W);
                        vpass::<W, AVG>(self.tmp, W, h, coefs(taps(self.filter, fy)), o, os);
                    }
                }
            }
        }
    }

    #[inline]
    fn coefs(f: &[i16]) -> [i16; 4] {
        std::array::from_fn(|k| {
            ((f[2 * k] as i8 as u8 as u16) | ((f[2 * k + 1] as i8 as u8 as u16) << 8)) as i16
        })
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn sum(x: [__m256i; 4]) -> __m256i {
        let s = _mm256_adds_epi16(x[0], x[3]);
        let s = _mm256_adds_epi16(s, _mm256_min_epi16(x[1], x[2]));
        let s = _mm256_adds_epi16(s, _mm256_max_epi16(x[1], x[2]));
        _mm256_srai_epi16::<7>(_mm256_adds_epi16(s, _mm256_set1_epi16(64)))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn sum128(x: [__m128i; 4]) -> __m128i {
        let s = _mm_adds_epi16(x[0], x[3]);
        let s = _mm_adds_epi16(s, _mm_min_epi16(x[1], x[2]));
        let s = _mm_adds_epi16(s, _mm_max_epi16(x[1], x[2]));
        _mm_srai_epi16::<7>(_mm_adds_epi16(s, _mm_set1_epi16(64)))
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn put<const N: usize, const AVG: bool>(d: *mut u8, v: __m128i) {
        unsafe {
            match N {
                4 => {
                    let mut v = v;
                    if AVG {
                        v = _mm_avg_epu8(v, _mm_cvtsi32_si128(d.cast::<i32>().read_unaligned()));
                    }
                    d.cast::<i32>().write_unaligned(_mm_cvtsi128_si32(v));
                }
                8 => {
                    let mut v = v;
                    if AVG {
                        v = _mm_avg_epu8(v, _mm_loadl_epi64(d.cast()));
                    }
                    _mm_storel_epi64(d.cast(), v);
                }
                _ => {
                    let mut v = v;
                    if AVG {
                        v = _mm_avg_epu8(v, _mm_loadu_si128(d.cast()));
                    }
                    _mm_storeu_si128(d.cast(), v);
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn copy<const W: usize, const AVG: bool>(
        s: *const u8,
        ss: usize,
        h: usize,
        o: *mut u8,
        os: usize,
    ) {
        unsafe {
            for i in 0..h {
                let (p, d) = (s.add(i * ss), o.add(i * os));
                if W >= 32 {
                    for j in (0..W).step_by(32) {
                        let mut v = _mm256_loadu_si256(p.add(j).cast());
                        if AVG {
                            v = _mm256_avg_epu8(v, _mm256_loadu_si256(d.add(j).cast()));
                        }
                        _mm256_storeu_si256(d.add(j).cast(), v);
                    }
                } else {
                    let v = match W {
                        4 => _mm_cvtsi32_si128(p.cast::<i32>().read_unaligned()),
                        8 => _mm_loadl_epi64(p.cast()),
                        _ => _mm_loadu_si128(p.cast()),
                    };
                    put::<W, AVG>(d, v);
                }
            }
        }
    }

    const SHUF: [[i8; 16]; 4] = {
        let mut m = [[0i8; 16]; 4];
        let mut k = 0;
        while k < 4 {
            let mut j = 0;
            while j < 8 {
                m[k][2 * j] = (2 * k + j) as i8;
                m[k][2 * j + 1] = (2 * k + j + 1) as i8;
                j += 1;
            }
            k += 1;
        }
        m
    };

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn hpass<const W: usize, const AVG: bool>(
        s: *const u8,
        ss: usize,
        rows: usize,
        c: [i16; 4],
        o: *mut u8,
        os: usize,
    ) {
        unsafe {
            if W >= 16 {
                let sh: [__m256i; 4] = std::array::from_fn(|k| {
                    _mm256_broadcastsi128_si256(_mm_loadu_si128(SHUF[k].as_ptr().cast()))
                });
                let cv: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi16(c[k]));
                for i in 0..rows {
                    let (p, d) = (s.add(i * ss), o.add(i * os));
                    let mut j = 0;
                    while j < W {
                        let a = _mm256_loadu2_m128i(p.add(j + 8).cast(), p.add(j).cast());
                        let x = sum(std::array::from_fn(|k| {
                            _mm256_maddubs_epi16(_mm256_shuffle_epi8(a, sh[k]), cv[k])
                        }));
                        if W == 16 {
                            let v = _mm256_packus_epi16(x, x);
                            let v = _mm256_castsi256_si128(_mm256_permute4x64_epi64::<0b1000>(v));
                            put::<16, AVG>(d.add(j), v);
                            j += 16;
                            continue;
                        }
                        let b = _mm256_loadu2_m128i(p.add(j + 24).cast(), p.add(j + 16).cast());
                        let y = sum(std::array::from_fn(|k| {
                            _mm256_maddubs_epi16(_mm256_shuffle_epi8(b, sh[k]), cv[k])
                        }));
                        let v = _mm256_permute4x64_epi64::<0b11011000>(_mm256_packus_epi16(x, y));
                        let mut v = v;
                        if AVG {
                            v = _mm256_avg_epu8(v, _mm256_loadu_si256(d.add(j).cast()));
                        }
                        _mm256_storeu_si256(d.add(j).cast(), v);
                        j += 32;
                    }
                }
            } else {
                let sh: [__m128i; 4] =
                    std::array::from_fn(|k| _mm_loadu_si128(SHUF[k].as_ptr().cast()));
                let cv: [__m128i; 4] = std::array::from_fn(|k| _mm_set1_epi16(c[k]));
                for i in 0..rows {
                    let (p, d) = (s.add(i * ss), o.add(i * os));
                    let a = _mm_loadu_si128(p.cast());
                    let x = sum128(std::array::from_fn(|k| {
                        _mm_maddubs_epi16(_mm_shuffle_epi8(a, sh[k]), cv[k])
                    }));
                    put::<W, AVG>(d, _mm_packus_epi16(x, x));
                }
            }
        }
    }

    #[inline]
    #[target_feature(enable = "avx2")]
    unsafe fn vpass<const W: usize, const AVG: bool>(
        s: *const u8,
        ss: usize,
        rows: usize,
        c: [i16; 4],
        o: *mut u8,
        os: usize,
    ) {
        unsafe {
            if W >= 32 {
                let cv: [__m256i; 4] = std::array::from_fn(|k| _mm256_set1_epi16(c[k]));
                for j in (0..W).step_by(32) {
                    for i in 0..rows {
                        let r: [__m256i; 8] = std::array::from_fn(|t| {
                            _mm256_loadu_si256(s.add((i + t) * ss + j).cast())
                        });
                        let lo = sum(std::array::from_fn(|k| {
                            _mm256_maddubs_epi16(
                                _mm256_unpacklo_epi8(r[2 * k], r[2 * k + 1]),
                                cv[k],
                            )
                        }));
                        let hi = sum(std::array::from_fn(|k| {
                            _mm256_maddubs_epi16(
                                _mm256_unpackhi_epi8(r[2 * k], r[2 * k + 1]),
                                cv[k],
                            )
                        }));
                        let mut v = _mm256_packus_epi16(lo, hi);
                        let d = o.add(i * os + j);
                        if AVG {
                            v = _mm256_avg_epu8(v, _mm256_loadu_si256(d.cast()));
                        }
                        _mm256_storeu_si256(d.cast(), v);
                    }
                }
            } else {
                let cv: [__m128i; 4] = std::array::from_fn(|k| _mm_set1_epi16(c[k]));
                let ld = |r: usize| match W {
                    4 => _mm_cvtsi32_si128(s.add(r * ss).cast::<i32>().read_unaligned()),
                    8 => _mm_loadl_epi64(s.add(r * ss).cast()),
                    _ => _mm_loadu_si128(s.add(r * ss).cast()),
                };
                for i in 0..rows {
                    let r: [__m128i; 8] = std::array::from_fn(|t| ld(i + t));
                    let lo = sum128(std::array::from_fn(|k| {
                        _mm_maddubs_epi16(_mm_unpacklo_epi8(r[2 * k], r[2 * k + 1]), cv[k])
                    }));
                    let v = if W == 16 {
                        let hi = sum128(std::array::from_fn(|k| {
                            _mm_maddubs_epi16(_mm_unpackhi_epi8(r[2 * k], r[2 * k + 1]), cv[k])
                        }));
                        _mm_packus_epi16(lo, hi)
                    } else {
                        _mm_packus_epi16(lo, lo)
                    };
                    put::<W, AVG>(o.add(i * os), v);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_path_with_unit_step_matches_unscaled() {
        let (w, h) = (40usize, 30usize);
        let data: Vec<u8> = (0..w * h)
            .map(|i| ((i * 37 + (i / w) * 11) % 256) as u8)
            .collect();
        let info = crate::vp9::frame::FrameInfo {
            width: w as u32,
            height: h as u32,
            ss_x: true,
            ss_y: true,
            bit_depth: 8,
            color_space: 0,
            color_range: false,
            render_width: w as u32,
            render_height: h as u32,
            key: true,
            intra_only: false,
        };
        let chroma = vec![0u8; w.div_ceil(2) * h.div_ceil(2)];
        let frame = Frame::from_planes(info, [&data, &chroma, &chroma]);
        let r = RefPlane {
            frame: &frame,
            plane: 0,
            last_x: w as i32 - 1,
            last_y: h as i32 - 1,
        };
        let mut tmp = vec![0u16; 80 * 80];
        for filter in 0..4u8 {
            for &(x, y) in &[
                (0, 0),
                (5, 3),
                (16 * 7 + 5, 16 * 9 + 11),
                (-40, -3),
                (16 * 36 + 1, 16 * 25 + 15),
                (16 * 10, 16 * 10 + 8),
            ] {
                for &(bw, bh) in &[(4usize, 4usize), (8, 8), (16, 8)] {
                    let mut a = vec![0u8; bw * bh];
                    let mut b = vec![0u8; bw * bh];
                    predict_unscaled(
                        &r,
                        x,
                        y,
                        bw,
                        bh,
                        filter,
                        8,
                        &mut a,
                        bw,
                        &mut tmp,
                        &mut [0u8; 71 * 71],
                    );
                    predict_scaled(&r, x, y, 16, 16, bw, bh, filter, 8, &mut b, bw, &mut tmp);
                    assert_eq!(a, b, "filter {filter} pos ({x},{y}) {bw}x{bh}");
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_unscaled_matches_scalar() {
        if !crate::vp9::isa::avx2() {
            return;
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let ss = 100usize;
        let mut tmp = vec![0u16; 64 * 71];
        for bd in [8u8, 10, 12] {
            let src: Vec<u16> = (0..ss * 80)
                .map(|_| {
                    let v = rnd();
                    if v & 3 == 0 {
                        ((v >> 8) & 1) as u16 * ((1 << bd) - 1)
                    } else {
                        ((v >> 8) % (1 << bd)) as u16
                    }
                })
                .collect();
            for w in [4usize, 8, 16, 32, 64] {
                for h in [4usize, 8, 16, 32, 64] {
                    for _ in 0..24 {
                        let r = rnd();
                        let (fx, fy) = ((r & 15) as usize, ((r >> 4) & 15) as usize);
                        let filter = ((r >> 8) & 3) as u8;
                        let so = ((r >> 10) % (ss as u64 - 71)) as usize;
                        let avg = (r >> 20) & 1 == 1;
                        let os = 70;
                        let base: Vec<u16> =
                            (0..os * h).map(|_| (rnd() % (1 << bd)) as u16).collect();
                        let mut a = base.clone();
                        let mut b = base.clone();
                        let mut p = vec![0u16; w * h];
                        unscaled_scalar(
                            &src, ss, so, w, h, fx, fy, filter, bd, &mut p, w, &mut tmp,
                        );
                        if avg {
                            average(&mut a, os, &p, w, h);
                        } else {
                            for i in 0..h {
                                a[i * os..i * os + w].copy_from_slice(&p[i * w..i * w + w]);
                            }
                        }
                        unsafe {
                            simd::unscaled(
                                &src, ss, so, w, h, fx, fy, filter, bd, &mut b, os, avg, &mut tmp,
                            )
                        };
                        assert_eq!(a, b, "bd {bd} {w}x{h} f{filter} ({fx},{fy}) avg {avg}");
                    }
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd8_unscaled_matches_scalar() {
        if !crate::vp9::isa::avx2() {
            return;
        }
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let ss = 100usize;
        let mut tmp = vec![0u16; 64 * 71];
        let src: Vec<u8> = (0..ss * 90)
            .map(|_| {
                let v = rnd();
                if v & 3 == 0 {
                    ((v >> 8) & 1) as u8 * 255
                } else {
                    (v >> 8) as u8
                }
            })
            .collect();
        for w in [4usize, 8, 16, 32, 64] {
            for h in [4usize, 8, 16, 32, 64] {
                for filter in 0..4u8 {
                    for _ in 0..12 {
                        let r = rnd();
                        let (fx, fy) = ((r & 15) as usize, ((r >> 4) & 15) as usize);
                        let so = ((r >> 10) % (ss as u64 - 80)) as usize;
                        let avg = (r >> 20) & 1 == 1;
                        let os = 70;
                        let base: Vec<u8> = (0..os * h).map(|_| rnd() as u8).collect();
                        let mut a = base.clone();
                        let mut b = base.clone();
                        let mut p = vec![0u8; w * h];
                        unscaled_scalar(&src, ss, so, w, h, fx, fy, filter, 8, &mut p, w, &mut tmp);
                        if avg {
                            average(&mut a, os, &p, w, h);
                        } else {
                            for i in 0..h {
                                a[i * os..i * os + w].copy_from_slice(&p[i * w..i * w + w]);
                            }
                        }
                        assert!(simd8::unscaled(
                            &src, ss, so, w, h, fx, fy, filter, &mut b, os, avg, &mut tmp
                        ));
                        assert_eq!(a, b, "{w}x{h} f{filter} ({fx},{fy}) avg {avg}");
                    }
                }
            }
        }
    }
}
