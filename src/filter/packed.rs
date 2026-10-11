use super::color::{ColorSpec, FRAC, Rgba, Siting, fixed, rows_job};
use super::plane::{Sample, Simd, check, reshape, samples, samples_mut, simd};
use crate::media::{Error, Frame, PixelFormat, Result};
use rayon::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Order {
    #[default]
    Rgba,
    Bgra,
    Rgbx,
    Bgrx,
}

impl Order {
    fn swapped(self) -> bool {
        matches!(self, Order::Bgra | Order::Bgrx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Chroma {
    #[default]
    Nearest,
    Bilinear,
}

#[derive(Debug, Clone, Copy)]
pub struct Packed<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub order: Order,
    pub data: &'a [u8],
}

#[derive(Debug)]
pub struct PackedMut<'a> {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub order: Order,
    pub data: &'a mut [u8],
}

impl<'a> Packed<'a> {
    pub fn new(width: u32, height: u32, order: Order, data: &'a [u8]) -> Self {
        Packed {
            width,
            height,
            stride: width as usize * 4,
            order,
            data,
        }
    }
}

impl<'a> PackedMut<'a> {
    pub fn new(width: u32, height: u32, order: Order, data: &'a mut [u8]) -> Self {
        PackedMut {
            width,
            height,
            stride: width as usize * 4,
            order,
            data,
        }
    }
}

impl Rgba {
    pub fn packed(&self) -> Packed<'_> {
        Packed::new(self.width, self.height, Order::Rgba, &self.data)
    }

    pub fn packed_mut(&mut self) -> PackedMut<'_> {
        PackedMut::new(self.width, self.height, Order::Rgba, &mut self.data)
    }
}

fn fits(width: u32, height: u32, stride: usize, len: usize) -> Result<()> {
    let (w, h) = (width as usize, height as usize);
    if stride < w * 4 || (h > 0 && len < stride * (h - 1) + w * 4) {
        return Err(Error::Invalid(format!(
            "packed {width}x{height} with stride {stride} needs more than {len} bytes"
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct ToPacked {
    cy: i32,
    ky: i32,
    c_off: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
    swap: bool,
}

#[derive(Default)]
struct Terms {
    r: Vec<i32>,
    g: Vec<i32>,
    b: Vec<i32>,
    u: Vec<i32>,
    v: Vec<i32>,
}

pub fn yuv_to_rgba(frame: &Frame, spec: ColorSpec, out: &mut Rgba) -> Result<()> {
    out.width = frame.width;
    out.height = frame.height;
    out.data
        .resize(frame.width as usize * frame.height as usize * 4, 0);
    yuv_to_packed(frame, spec, Chroma::Nearest, out.packed_mut())
}

pub fn yuv_to_packed(frame: &Frame, spec: ColorSpec, chroma: Chroma, out: PackedMut) -> Result<()> {
    to_packed(frame, spec, chroma, out, simd())
}

pub(crate) fn to_packed(
    frame: &Frame,
    spec: ColorSpec,
    chroma: Chroma,
    out: PackedMut,
    simd: Simd,
) -> Result<()> {
    check(frame)?;
    if (out.width, out.height) != (frame.width, frame.height) {
        return Err(Error::Invalid(format!(
            "packed output {}x{} for a {}x{} frame",
            out.width, out.height, frame.width, frame.height
        )));
    }
    fits(out.width, out.height, out.stride, out.data.len())?;
    let l = spec.levels(frame.format.bit_depth());
    let m = spec.matrix.inverse();
    let cy = fixed(255.0 / l.y_range);
    let k = ToPacked {
        cy,
        ky: (1 << (FRAC - 1)) - cy * l.y_off as i32,
        c_off: l.c_off as i32,
        rv: fixed(255.0 * m[0][2] / l.c_range),
        gu: fixed(255.0 * m[1][1] / l.c_range),
        gv: fixed(255.0 * m[1][2] / l.c_range),
        bu: fixed(255.0 * m[2][1] / l.c_range),
        swap: out.order.swapped(),
    };
    match frame.format {
        PixelFormat::Yuv420p8 => to_packed_with::<u8>(frame, k, chroma, spec.siting, out, simd),
        PixelFormat::Yuv420p10 => to_packed_with::<u16>(frame, k, chroma, spec.siting, out, simd),
    }
}

fn to_packed_with<S: Sample>(
    frame: &Frame,
    k: ToPacked,
    chroma: Chroma,
    siting: Siting,
    out: PackedMut,
    simd: Simd,
) -> Result<()> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 {
        return Ok(());
    }
    let b = S::BYTES;
    let (ys, us, vs) = (
        frame.planes[0].stride / b,
        frame.planes[1].stride / b,
        frame.planes[2].stride / b,
    );
    let (yp, up, vp) = (
        samples::<S>(&frame.planes[0])?,
        samples::<S>(&frame.planes[1])?,
        samples::<S>(&frame.planes[2])?,
    );
    let stride = out.stride;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    out.data[..stride * (h - 1) + w * 4]
        .par_chunks_mut(stride * 2)
        .enumerate()
        .with_min_len(rows_job(ch))
        .for_each_init(Terms::default, |t, (cy, pair)| {
            let crow = |p: &'_ [S], s: usize, r: usize| -> Vec<i32> {
                p[r * s..r * s + cw].iter().map(|v| v.get()).collect()
            };
            match chroma {
                Chroma::Nearest => {
                    let u = &up[cy * us..cy * us + cw];
                    let v = &vp[cy * vs..cy * vs + cw];
                    t.r.clear();
                    t.g.clear();
                    t.b.clear();
                    for (&u, &v) in u.iter().zip(v) {
                        let (u, v) = (u.get() - k.c_off, v.get() - k.c_off);
                        t.r.push(k.rv * v);
                        t.g.push(k.gu * u + k.gv * v);
                        t.b.push(k.bu * u);
                    }
                    for (r, row) in pair.chunks_mut(stride).enumerate() {
                        let y = cy * 2 + r;
                        let luma = &yp[y * ys..y * ys + w];
                        pixels::<S, true>(luma, &t.r, &t.g, &t.b, &mut row[..w * 4], k, simd);
                    }
                }
                Chroma::Bilinear => {
                    for (r, row) in pair.chunks_mut(stride).enumerate() {
                        let y = cy * 2 + r;
                        let near = if r == 0 {
                            cy.saturating_sub(1)
                        } else {
                            (cy + 1).min(ch - 1)
                        };
                        let (ua, ub) = (crow(up, us, cy), crow(up, us, near));
                        let (va, vb) = (crow(vp, vs, cy), crow(vp, vs, near));
                        upsample(&ua, &ub, w, siting, &mut t.u);
                        upsample(&va, &vb, w, siting, &mut t.v);
                        t.r.clear();
                        t.g.clear();
                        t.b.clear();
                        let off = 16 * k.c_off;
                        for (&u, &v) in t.u.iter().zip(&t.v) {
                            let (u, v) = (u - off, v - off);
                            t.r.push((k.rv * v + 8) >> 4);
                            t.g.push((k.gu * u + k.gv * v + 8) >> 4);
                            t.b.push((k.bu * u + 8) >> 4);
                        }
                        let luma = &yp[y * ys..y * ys + w];
                        pixels::<S, false>(luma, &t.r, &t.g, &t.b, &mut row[..w * 4], k, simd);
                    }
                }
            }
        });
    Ok(())
}

fn upsample(a: &[i32], b: &[i32], w: usize, siting: Siting, out: &mut Vec<i32>) {
    let cw = a.len();
    out.clear();
    out.resize(w, 0);
    let v = |i: usize| 3 * a[i] + b[i];
    for (x, o) in out.iter_mut().enumerate() {
        let i = x / 2;
        *o = match (siting, x % 2) {
            (Siting::Left, 0) => 4 * v(i),
            (Siting::Left, _) => 2 * (v(i) + v((i + 1).min(cw - 1))),
            (Siting::Centre, 0) => 3 * v(i) + v(i.saturating_sub(1)),
            (Siting::Centre, _) => 3 * v(i) + v((i + 1).min(cw - 1)),
        };
    }
}

#[inline(always)]
fn pixel<S: Sample>(y: S, tr: i32, tg: i32, tb: i32, k: ToPacked, o: &mut [u8]) {
    let y = k.cy * y.get() + k.ky;
    let r = ((y + tr) >> FRAC).clamp(0, 255) as u8;
    let g = ((y + tg) >> FRAC).clamp(0, 255) as u8;
    let b = ((y + tb) >> FRAC).clamp(0, 255) as u8;
    o.copy_from_slice(&if k.swap {
        [b, g, r, 255]
    } else {
        [r, g, b, 255]
    });
}

fn pixels<S: Sample, const DUP: bool>(
    luma: &[S],
    tr: &[i32],
    tg: &[i32],
    tb: &[i32],
    out: &mut [u8],
    k: ToPacked,
    simd: Simd,
) {
    let w = luma.len();
    let need = if DUP { w.div_ceil(2) } else { w };
    assert!(tr.len() >= need && tg.len() >= need && tb.len() >= need && out.len() >= w * 4);
    let from = match simd {
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { x86::pixels::<S, DUP>(luma, tr, tg, tb, out, k) },
        #[cfg(target_arch = "aarch64")]
        Simd::Neon => neon::pixels::<S, DUP>(luma, tr, tg, tb, out, k),
        _ => 0,
    };
    for x in from..w {
        let i = if DUP { x / 2 } else { x };
        pixel(luma[x], tr[i], tg[i], tb[i], k, &mut out[x * 4..x * 4 + 4]);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ToYuv {
    y: [i32; 3],
    u: [i32; 3],
    v: [i32; 3],
    y_off: i32,
    c_off: i32,
    max: i32,
    swap: bool,
}

pub fn rgba_to_yuv(
    src: &Rgba,
    spec: ColorSpec,
    format: PixelFormat,
    out: &mut Frame,
) -> Result<()> {
    packed_to_yuv(src.packed(), spec, format, out)
}

pub fn packed_to_yuv(
    src: Packed,
    spec: ColorSpec,
    format: PixelFormat,
    out: &mut Frame,
) -> Result<()> {
    from_packed(src, spec, format, out, simd())
}

pub(crate) fn from_packed(
    src: Packed,
    spec: ColorSpec,
    format: PixelFormat,
    out: &mut Frame,
    simd: Simd,
) -> Result<()> {
    fits(src.width, src.height, src.stride, src.data.len())?;
    let depth = format.bit_depth();
    let l = spec.levels(depth);
    let m = spec.matrix.forward();
    let row = |i: usize, range: f64| {
        [
            fixed(m[i][0] * range / 255.0),
            fixed(m[i][1] * range / 255.0),
            fixed(m[i][2] * range / 255.0),
        ]
    };
    let k = ToYuv {
        y: row(0, l.y_range),
        u: row(1, l.c_range),
        v: row(2, l.c_range),
        y_off: ((l.y_off as i32) << FRAC) + (1 << (FRAC - 1)),
        c_off: ((l.c_off as i32) << FRAC) + (1 << (FRAC - 1)),
        max: (1 << depth) - 1,
        swap: src.order.swapped(),
    };
    reshape(out, format, src.width, src.height, out.pts, out.time_base);
    if src.width == 0 || src.height == 0 {
        return Ok(());
    }
    match format {
        PixelFormat::Yuv420p8 => from_packed_with::<u8>(src, k, spec.siting, out, simd),
        PixelFormat::Yuv420p10 => from_packed_with::<u16>(src, k, spec.siting, out, simd),
    }
}

fn from_packed_with<S: Sample>(
    src: Packed,
    k: ToYuv,
    siting: Siting,
    out: &mut Frame,
    simd: Simd,
) -> Result<()> {
    let w = src.width as usize;
    let rows = src.height as usize;
    let stride = src.stride;
    let cw = w.div_ceil(2);
    let [py, pu, pv] = &mut out.planes;
    let (ys, us, vs) = (
        py.stride / S::BYTES,
        pu.stride / S::BYTES,
        pv.stride / S::BYTES,
    );
    let yp = samples_mut::<S>(py)?;
    let up = samples_mut::<S>(pu)?;
    let vp = samples_mut::<S>(pv)?;
    let line = |r: usize| &src.data[r * stride..r * stride + w * 4];
    yp.par_chunks_mut(ys * 2)
        .zip(up.par_chunks_mut(us))
        .zip(vp.par_chunks_mut(vs))
        .enumerate()
        .with_min_len(rows_job(rows.div_ceil(2)))
        .for_each_init(
            || (vec![0i32; w], vec![0i32; w]),
            |(su, sv), (cy, ((yrows, urow), vrow))| {
                let top = line(cy * 2);
                let bottom = if cy * 2 + 1 < rows {
                    line(cy * 2 + 1)
                } else {
                    top
                };
                let (yt, yb) = yrows.split_at_mut(ys.min(yrows.len()));
                luma(top, &mut yt[..w], k, simd);
                if cy * 2 + 1 < rows {
                    luma(bottom, &mut yb[..w], k, simd);
                }
                sums(top, bottom, su, sv, k, simd);
                let last = w - 1;
                for x in 0..cw {
                    let (a, b) = (2 * x, (2 * x + 1).min(last));
                    let (u, v) = match siting {
                        Siting::Centre => (
                            (su[a] + su[b] + 4 * k.c_off) >> (FRAC + 2),
                            (sv[a] + sv[b] + 4 * k.c_off) >> (FRAC + 2),
                        ),
                        Siting::Left => {
                            let p = a.saturating_sub(1);
                            (
                                (su[p] + 2 * su[a] + su[b] + 8 * k.c_off) >> (FRAC + 3),
                                (sv[p] + 2 * sv[a] + sv[b] + 8 * k.c_off) >> (FRAC + 3),
                            )
                        }
                    };
                    urow[x] = S::put(u.clamp(0, k.max));
                    vrow[x] = S::put(v.clamp(0, k.max));
                }
            },
        );
    Ok(())
}

#[inline(always)]
fn rgb(p: &[u8], swap: bool) -> [i32; 3] {
    if swap {
        [p[2] as i32, p[1] as i32, p[0] as i32]
    } else {
        [p[0] as i32, p[1] as i32, p[2] as i32]
    }
}

fn luma<S: Sample>(src: &[u8], out: &mut [S], k: ToYuv, simd: Simd) {
    let w = out.len();
    assert!(src.len() >= w * 4);
    let from = match simd {
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { x86::luma(src, out, k) },
        #[cfg(target_arch = "aarch64")]
        Simd::Neon => neon::luma(src, out, k),
        _ => 0,
    };
    for x in from..w {
        let [r, g, b] = rgb(&src[x * 4..], k.swap);
        let y = (k.y[0] * r + k.y[1] * g + k.y[2] * b + k.y_off) >> FRAC;
        out[x] = S::put(y.clamp(0, k.max));
    }
}

fn sums(top: &[u8], bottom: &[u8], su: &mut [i32], sv: &mut [i32], k: ToYuv, simd: Simd) {
    let w = su.len();
    assert!(top.len() >= w * 4 && bottom.len() >= w * 4 && sv.len() >= w);
    let from = match simd {
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { x86::sums(top, bottom, su, sv, k) },
        #[cfg(target_arch = "aarch64")]
        Simd::Neon => neon::sums(top, bottom, su, sv, k),
        _ => 0,
    };
    for x in from..w {
        let a = rgb(&top[x * 4..], k.swap);
        let b = rgb(&bottom[x * 4..], k.swap);
        let s = [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
        su[x] = k.u[0] * s[0] + k.u[1] * s[1] + k.u[2] * s[2];
        sv[x] = k.v[0] * s[0] + k.v[1] * s[1] + k.v[2] * s[2];
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{FRAC, Sample, ToPacked, ToYuv};
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    unsafe fn luma8<S: Sample>(p: *const S) -> __m256i {
        unsafe {
            if S::BYTES == 1 {
                _mm256_cvtepu8_epi32(_mm_loadl_epi64(p as *const __m128i))
            } else {
                _mm256_cvtepu16_epi32(_mm_loadu_si128(p as *const __m128i))
            }
        }
    }

    #[target_feature(enable = "avx2")]
    fn ordered(a: __m256i, b: __m256i) -> __m256i {
        _mm256_permute4x64_epi64::<0xD8>(_mm256_packs_epi32(a, b))
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn pixels<S: Sample, const DUP: bool>(
        luma: &[S],
        tr: &[i32],
        tg: &[i32],
        tb: &[i32],
        out: &mut [u8],
        k: ToPacked,
    ) -> usize {
        let w = luma.len();
        let cy = _mm256_set1_epi32(k.cy);
        let ky = _mm256_set1_epi32(k.ky);
        let lo_idx = _mm256_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3);
        let hi_idx = _mm256_setr_epi32(4, 4, 5, 5, 6, 6, 7, 7);
        let alpha = _mm256_set1_epi16(255);
        let mut x = 0;
        while x + 16 <= w {
            unsafe {
                let y0 = _mm256_add_epi32(_mm256_mullo_epi32(luma8(luma.as_ptr().add(x)), cy), ky);
                let y1 =
                    _mm256_add_epi32(_mm256_mullo_epi32(luma8(luma.as_ptr().add(x + 8)), cy), ky);
                let term = |t: &[i32]| -> (__m256i, __m256i) {
                    if DUP {
                        let v = _mm256_loadu_si256(t.as_ptr().add(x / 2) as *const __m256i);
                        (
                            _mm256_permutevar8x32_epi32(v, lo_idx),
                            _mm256_permutevar8x32_epi32(v, hi_idx),
                        )
                    } else {
                        (
                            _mm256_loadu_si256(t.as_ptr().add(x) as *const __m256i),
                            _mm256_loadu_si256(t.as_ptr().add(x + 8) as *const __m256i),
                        )
                    }
                };
                let channel = |t: &[i32]| {
                    let (a, b) = term(t);
                    ordered(
                        _mm256_srai_epi32::<FRAC>(_mm256_add_epi32(y0, a)),
                        _mm256_srai_epi32::<FRAC>(_mm256_add_epi32(y1, b)),
                    )
                };
                let (r, g, b) = (channel(tr), channel(tg), channel(tb));
                let (first, third) = if k.swap { (b, r) } else { (r, b) };
                let p01 = _mm256_packus_epi16(first, g);
                let p23 = _mm256_packus_epi16(third, alpha);
                let fg = _mm256_unpacklo_epi8(p01, _mm256_srli_si256::<8>(p01));
                let ba = _mm256_unpacklo_epi8(p23, _mm256_srli_si256::<8>(p23));
                let lo = _mm256_unpacklo_epi16(fg, ba);
                let hi = _mm256_unpackhi_epi16(fg, ba);
                let o = out.as_mut_ptr().add(x * 4) as *mut __m256i;
                _mm256_storeu_si256(o, _mm256_permute2x128_si256::<0x20>(lo, hi));
                _mm256_storeu_si256(o.add(1), _mm256_permute2x128_si256::<0x31>(lo, hi));
            }
            x += 16;
        }
        x
    }

    #[target_feature(enable = "avx2")]
    fn channels(v: __m256i, swap: bool) -> [__m256i; 3] {
        let m = _mm256_set1_epi32(0xff);
        let c0 = _mm256_and_si256(v, m);
        let c1 = _mm256_and_si256(_mm256_srli_epi32::<8>(v), m);
        let c2 = _mm256_and_si256(_mm256_srli_epi32::<16>(v), m);
        if swap { [c2, c1, c0] } else { [c0, c1, c2] }
    }

    #[target_feature(enable = "avx2")]
    fn dot(c: [__m256i; 3], k: [i32; 3]) -> __m256i {
        _mm256_add_epi32(
            _mm256_add_epi32(
                _mm256_mullo_epi32(c[0], _mm256_set1_epi32(k[0])),
                _mm256_mullo_epi32(c[1], _mm256_set1_epi32(k[1])),
            ),
            _mm256_mullo_epi32(c[2], _mm256_set1_epi32(k[2])),
        )
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn luma<S: Sample>(src: &[u8], out: &mut [S], k: ToYuv) -> usize {
        let w = out.len();
        let off = _mm256_set1_epi32(k.y_off);
        let max = _mm_set1_epi16(k.max as i16);
        let zero = _mm_setzero_si128();
        let mut x = 0;
        while x + 8 <= w {
            unsafe {
                let v = _mm256_loadu_si256(src.as_ptr().add(x * 4) as *const __m256i);
                let y =
                    _mm256_srai_epi32::<FRAC>(_mm256_add_epi32(dot(channels(v, k.swap), k.y), off));
                let p =
                    _mm256_castsi256_si128(_mm256_permute4x64_epi64::<8>(_mm256_packs_epi32(y, y)));
                if S::BYTES == 1 {
                    _mm_storel_epi64(
                        out.as_mut_ptr().add(x) as *mut __m128i,
                        _mm_packus_epi16(p, p),
                    );
                } else {
                    let p = _mm_min_epi16(_mm_max_epi16(p, zero), max);
                    _mm_storeu_si128(out.as_mut_ptr().add(x) as *mut __m128i, p);
                }
            }
            x += 8;
        }
        x
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn sums(
        top: &[u8],
        bottom: &[u8],
        su: &mut [i32],
        sv: &mut [i32],
        k: ToYuv,
    ) -> usize {
        let w = su.len();
        let mut x = 0;
        while x + 8 <= w {
            unsafe {
                let a = channels(
                    _mm256_loadu_si256(top.as_ptr().add(x * 4) as *const __m256i),
                    k.swap,
                );
                let b = channels(
                    _mm256_loadu_si256(bottom.as_ptr().add(x * 4) as *const __m256i),
                    k.swap,
                );
                let s = [
                    _mm256_add_epi32(a[0], b[0]),
                    _mm256_add_epi32(a[1], b[1]),
                    _mm256_add_epi32(a[2], b[2]),
                ];
                _mm256_storeu_si256(su.as_mut_ptr().add(x) as *mut __m256i, dot(s, k.u));
                _mm256_storeu_si256(sv.as_mut_ptr().add(x) as *mut __m256i, dot(s, k.v));
            }
            x += 8;
        }
        x
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{FRAC, Sample, ToPacked, ToYuv};
    use std::arch::aarch64::*;

    unsafe fn luma16<S: Sample>(p: *const S) -> [int32x4_t; 4] {
        unsafe {
            let (lo, hi) = if S::BYTES == 1 {
                let v = vld1q_u8(p as *const u8);
                (vmovl_u8(vget_low_u8(v)), vmovl_high_u8(v))
            } else {
                (
                    vld1q_u16(p as *const u16),
                    vld1q_u16((p as *const u16).add(8)),
                )
            };
            [
                vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(lo))),
                vreinterpretq_s32_u32(vmovl_high_u16(lo)),
                vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(hi))),
                vreinterpretq_s32_u32(vmovl_high_u16(hi)),
            ]
        }
    }

    fn narrow(v: [int32x4_t; 4]) -> uint8x16_t {
        unsafe {
            let a = vcombine_s16(vqmovn_s32(v[0]), vqmovn_s32(v[1]));
            let b = vcombine_s16(vqmovn_s32(v[2]), vqmovn_s32(v[3]));
            vcombine_u8(vqmovun_s16(a), vqmovun_s16(b))
        }
    }

    pub(super) fn pixels<S: Sample, const DUP: bool>(
        luma: &[S],
        tr: &[i32],
        tg: &[i32],
        tb: &[i32],
        out: &mut [u8],
        k: ToPacked,
    ) -> usize {
        let w = luma.len();
        let mut x = 0;
        while x + 16 <= w {
            unsafe {
                let ky = vdupq_n_s32(k.ky);
                let y = luma16(luma.as_ptr().add(x)).map(|v| vmlaq_n_s32(ky, v, k.cy));
                let term = |t: &[i32]| -> [int32x4_t; 4] {
                    if DUP {
                        let a = vld1q_s32(t.as_ptr().add(x / 2));
                        let b = vld1q_s32(t.as_ptr().add(x / 2 + 4));
                        [
                            vzip1q_s32(a, a),
                            vzip2q_s32(a, a),
                            vzip1q_s32(b, b),
                            vzip2q_s32(b, b),
                        ]
                    } else {
                        [0, 4, 8, 12].map(|o| vld1q_s32(t.as_ptr().add(x + o)))
                    }
                };
                let channel = |t: &[i32]| {
                    let t = term(t);
                    narrow([0, 1, 2, 3].map(|i| vshrq_n_s32::<FRAC>(vaddq_s32(y[i], t[i]))))
                };
                let (r, g, b) = (channel(tr), channel(tg), channel(tb));
                let (first, third) = if k.swap { (b, r) } else { (r, b) };
                vst4q_u8(
                    out.as_mut_ptr().add(x * 4),
                    uint8x16x4_t(first, g, third, vdupq_n_u8(255)),
                );
            }
            x += 16;
        }
        x
    }

    fn widen(v: uint8x16_t) -> [int32x4_t; 4] {
        unsafe {
            let lo = vmovl_u8(vget_low_u8(v));
            let hi = vmovl_high_u8(v);
            [
                vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(lo))),
                vreinterpretq_s32_u32(vmovl_high_u16(lo)),
                vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(hi))),
                vreinterpretq_s32_u32(vmovl_high_u16(hi)),
            ]
        }
    }

    fn dot(c: [[int32x4_t; 4]; 3], k: [i32; 3], i: usize, base: int32x4_t) -> int32x4_t {
        unsafe {
            let s = vmlaq_n_s32(base, c[0][i], k[0]);
            let s = vmlaq_n_s32(s, c[1][i], k[1]);
            vmlaq_n_s32(s, c[2][i], k[2])
        }
    }

    pub(super) fn luma<S: Sample>(src: &[u8], out: &mut [S], k: ToYuv) -> usize {
        let w = out.len();
        let mut x = 0;
        while x + 16 <= w {
            unsafe {
                let p = vld4q_u8(src.as_ptr().add(x * 4));
                let (r, b) = if k.swap { (p.2, p.0) } else { (p.0, p.2) };
                let c = [widen(r), widen(p.1), widen(b)];
                let off = vdupq_n_s32(k.y_off);
                let y = [0, 1, 2, 3].map(|i| vshrq_n_s32::<FRAC>(dot(c, k.y, i, off)));
                if S::BYTES == 1 {
                    vst1q_u8(out.as_mut_ptr().add(x) as *mut u8, narrow(y));
                } else {
                    let max = vdupq_n_u16(k.max as u16);
                    let lo = vminq_u16(vcombine_u16(vqmovun_s32(y[0]), vqmovun_s32(y[1])), max);
                    let hi = vminq_u16(vcombine_u16(vqmovun_s32(y[2]), vqmovun_s32(y[3])), max);
                    let o = out.as_mut_ptr().add(x) as *mut u16;
                    vst1q_u16(o, lo);
                    vst1q_u16(o.add(8), hi);
                }
            }
            x += 16;
        }
        x
    }

    pub(super) fn sums(
        top: &[u8],
        bottom: &[u8],
        su: &mut [i32],
        sv: &mut [i32],
        k: ToYuv,
    ) -> usize {
        let w = su.len();
        let mut x = 0;
        while x + 16 <= w {
            unsafe {
                let a = vld4q_u8(top.as_ptr().add(x * 4));
                let b = vld4q_u8(bottom.as_ptr().add(x * 4));
                let add = |p: uint8x16_t, q: uint8x16_t| -> [int32x4_t; 4] {
                    let lo = vaddl_u8(vget_low_u8(p), vget_low_u8(q));
                    let hi = vaddl_high_u8(p, q);
                    [
                        vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(lo))),
                        vreinterpretq_s32_u32(vmovl_high_u16(lo)),
                        vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(hi))),
                        vreinterpretq_s32_u32(vmovl_high_u16(hi)),
                    ]
                };
                let (ra, ba) = if k.swap { (a.2, a.0) } else { (a.0, a.2) };
                let (rb, bb) = if k.swap { (b.2, b.0) } else { (b.0, b.2) };
                let c = [add(ra, rb), add(a.1, b.1), add(ba, bb)];
                let zero = vdupq_n_s32(0);
                for i in 0..4 {
                    vst1q_s32(su.as_mut_ptr().add(x + 4 * i), dot(c, k.u, i, zero));
                    vst1q_s32(sv.as_mut_ptr().add(x + 4 * i), dot(c, k.v, i, zero));
                }
            }
            x += 16;
        }
        x
    }
}
