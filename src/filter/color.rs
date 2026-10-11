use super::plane::{Sample, check, reshape, samples, samples_mut};
use crate::media::{Error, Frame, PixelFormat, Result};
use rayon::prelude::*;

const FRAC: i32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl Matrix {
    fn kr_kb(self) -> (f64, f64) {
        match self {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        }
    }

    pub(crate) fn inverse(self) -> [[f64; 3]; 3] {
        let (kr, kb) = self.kr_kb();
        let kg = 1.0 - kr - kb;
        [
            [1.0, 0.0, 2.0 * (1.0 - kr)],
            [
                1.0,
                -2.0 * kb * (1.0 - kb) / kg,
                -2.0 * kr * (1.0 - kr) / kg,
            ],
            [1.0, 2.0 * (1.0 - kb), 0.0],
        ]
    }

    pub(crate) fn forward(self) -> [[f64; 3]; 3] {
        let (kr, kb) = self.kr_kb();
        let kg = 1.0 - kr - kb;
        let sb = 2.0 * (1.0 - kb);
        let sr = 2.0 * (1.0 - kr);
        [
            [kr, kg, kb],
            [-kr / sb, -kg / sb, 0.5],
            [0.5, -kg / sr, -kb / sr],
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Range {
    Limited,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorSpec {
    pub matrix: Matrix,
    pub range: Range,
}

impl ColorSpec {
    pub const BT601: ColorSpec = ColorSpec::new(Matrix::Bt601, Range::Limited);
    pub const BT709: ColorSpec = ColorSpec::new(Matrix::Bt709, Range::Limited);
    pub const BT2020: ColorSpec = ColorSpec::new(Matrix::Bt2020, Range::Limited);

    pub const fn new(matrix: Matrix, range: Range) -> Self {
        ColorSpec { matrix, range }
    }

    pub(crate) fn levels(self, depth: u8) -> Levels {
        let d = depth as i32;
        match self.range {
            Range::Limited => {
                let s = (1i32 << (d - 8)) as f64;
                Levels {
                    y_off: 16.0 * s,
                    y_range: 219.0 * s,
                    c_off: 128.0 * s,
                    c_range: 224.0 * s,
                }
            }
            Range::Full => {
                let top = ((1i32 << d) - 1) as f64;
                Levels {
                    y_off: 0.0,
                    y_range: top,
                    c_off: (1i32 << (d - 1)) as f64,
                    c_range: top,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Levels {
    pub y_off: f64,
    pub y_range: f64,
    pub c_off: f64,
    pub c_range: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Self {
        Rgba {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

fn fixed(v: f64) -> i32 {
    (v * (1 << FRAC) as f64).round() as i32
}

fn mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn rows_job(rows: usize) -> usize {
    (rows / (rayon::current_num_threads().max(1) * 4)).max(1)
}

#[derive(Clone, Copy)]
struct ToRgb {
    y_off: i32,
    c_off: i32,
    y: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

pub fn yuv_to_rgba(frame: &Frame, spec: ColorSpec, out: &mut Rgba) -> Result<()> {
    check(frame)?;
    let l = spec.levels(frame.format.bit_depth());
    let m = spec.matrix.inverse();
    let k = ToRgb {
        y_off: l.y_off as i32,
        c_off: l.c_off as i32,
        y: fixed(255.0 / l.y_range),
        rv: fixed(255.0 * m[0][2] / l.c_range),
        gu: fixed(255.0 * m[1][1] / l.c_range),
        gv: fixed(255.0 * m[1][2] / l.c_range),
        bu: fixed(255.0 * m[2][1] / l.c_range),
    };
    out.width = frame.width;
    out.height = frame.height;
    out.data
        .resize(frame.width as usize * frame.height as usize * 4, 0);
    match frame.format {
        PixelFormat::Yuv420p8 => to_rgba::<u8>(frame, k, out),
        PixelFormat::Yuv420p10 => to_rgba::<u16>(frame, k, out),
    }
}

fn to_rgba<S: Sample>(frame: &Frame, k: ToRgb, out: &mut Rgba) -> Result<()> {
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
    let stride = w * 4;
    let cw = w.div_ceil(2);
    out.data
        .par_chunks_mut(stride * 2)
        .enumerate()
        .with_min_len(rows_job(h.div_ceil(2)))
        .for_each_init(
            || vec![[0i32; 3]; cw],
            |terms, (cy, pair)| {
                let u = &up[cy * us..cy * us + cw];
                let v = &vp[cy * vs..cy * vs + cw];
                for ((t, &u), &v) in terms.iter_mut().zip(u).zip(v) {
                    let u = u.get() - k.c_off;
                    let v = v.get() - k.c_off;
                    *t = [
                        k.rv * v + (1 << (FRAC - 1)),
                        k.gu * u + k.gv * v + (1 << (FRAC - 1)),
                        k.bu * u + (1 << (FRAC - 1)),
                    ];
                }
                for (r, row) in pair.chunks_mut(stride).enumerate() {
                    let y = cy * 2 + r;
                    let luma = &yp[y * ys..y * ys + w];
                    rgba_row(luma, terms, row, k);
                }
            },
        );
    Ok(())
}

#[inline(always)]
fn rgba_pixel<S: Sample>(y: S, t: &[i32; 3], k: ToRgb) -> [u8; 4] {
    let y = k.y * (y.get() - k.y_off);
    [
        ((y + t[0]) >> FRAC).clamp(0, 255) as u8,
        ((y + t[1]) >> FRAC).clamp(0, 255) as u8,
        ((y + t[2]) >> FRAC).clamp(0, 255) as u8,
        255,
    ]
}

fn rgba_row<S: Sample>(luma: &[S], terms: &[[i32; 3]], row: &mut [u8], k: ToRgb) {
    let mut out = row.chunks_exact_mut(8);
    let mut pairs = luma.chunks_exact(2);
    for ((o, l), t) in (&mut out).zip(&mut pairs).zip(terms) {
        o[..4].copy_from_slice(&rgba_pixel(l[0], t, k));
        o[4..].copy_from_slice(&rgba_pixel(l[1], t, k));
    }
    if let (Some(&l), Some(t)) = (pairs.remainder().first(), terms.last()) {
        let n = row.len();
        row[n - 4..].copy_from_slice(&rgba_pixel(l, t, k));
    }
}

#[derive(Clone, Copy)]
struct ToYuv {
    y: [i32; 3],
    u: [i32; 3],
    v: [i32; 3],
    y_off: i32,
    c_off: i32,
    max: i32,
}

pub fn rgba_to_yuv(
    src: &Rgba,
    spec: ColorSpec,
    format: PixelFormat,
    out: &mut Frame,
) -> Result<()> {
    let w = src.width as usize;
    let h = src.height as usize;
    if src.data.len() < w * h * 4 {
        return Err(Error::Invalid("rgba data is shorter than its size".into()));
    }
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
    };
    reshape(out, format, src.width, src.height, out.pts, out.time_base);
    if w == 0 || h == 0 {
        return Ok(());
    }
    match format {
        PixelFormat::Yuv420p8 => from_rgba::<u8>(src, k, out),
        PixelFormat::Yuv420p10 => from_rgba::<u16>(src, k, out),
    }
}

fn from_rgba<S: Sample>(src: &Rgba, k: ToYuv, out: &mut Frame) -> Result<()> {
    let w = src.width as usize;
    let stride = w * 4;
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
    let rows = src.height as usize;
    yp.par_chunks_mut(ys * 2)
        .zip(up.par_chunks_mut(us))
        .zip(vp.par_chunks_mut(vs))
        .enumerate()
        .with_min_len(rows_job(rows.div_ceil(2)))
        .for_each(|(cy, ((yrows, urow), vrow))| {
            let top = &src.data[cy * 2 * stride..cy * 2 * stride + stride];
            let bottom = if cy * 2 + 1 < rows {
                &src.data[(cy * 2 + 1) * stride..(cy * 2 + 2) * stride]
            } else {
                top
            };
            let (yt, yb) = yrows.split_at_mut(ys.min(yrows.len()));
            luma_row(top, &mut yt[..w], k);
            if cy * 2 + 1 < rows {
                luma_row(bottom, &mut yb[..w], k);
            }
            for x in 0..cw {
                let a = x * 8;
                let b = if x * 2 + 1 < w { a + 4 } else { a };
                let mut s = [0i32; 3];
                for (c, v) in s.iter_mut().enumerate() {
                    *v = top[a + c] as i32
                        + top[b + c] as i32
                        + bottom[a + c] as i32
                        + bottom[b + c] as i32;
                }
                let u = (k.u[0] * s[0] + k.u[1] * s[1] + k.u[2] * s[2] + 4 * k.c_off) >> (FRAC + 2);
                let v = (k.v[0] * s[0] + k.v[1] * s[1] + k.v[2] * s[2] + 4 * k.c_off) >> (FRAC + 2);
                urow[x] = S::put(u.clamp(0, k.max));
                vrow[x] = S::put(v.clamp(0, k.max));
            }
        });
    Ok(())
}

fn luma_row<S: Sample>(src: &[u8], out: &mut [S], k: ToYuv) {
    for (o, p) in out.iter_mut().zip(src.chunks_exact(4)) {
        let y =
            (k.y[0] * p[0] as i32 + k.y[1] * p[1] as i32 + k.y[2] * p[2] as i32 + k.y_off) >> FRAC;
        *o = S::put(y.clamp(0, k.max));
    }
}

#[derive(Clone, Copy)]
struct Affine {
    y: [i32; 4],
    u: [i32; 3],
    v: [i32; 3],
    max: i32,
}

pub fn convert_color(
    src: &Frame,
    from: ColorSpec,
    to: ColorSpec,
    format: PixelFormat,
    out: &mut Frame,
) -> Result<()> {
    check(src)?;
    let ls = from.levels(src.format.bit_depth());
    let lt = to.levels(format.bit_depth());
    let m = mul(&to.matrix.forward(), &from.matrix.inverse());
    let ky = lt.y_range / ls.y_range;
    let kc = lt.y_range / ls.c_range;
    let cc = lt.c_range / ls.c_range;
    let half = 0.5 * (1 << FRAC) as f64;
    let a = Affine {
        y: [
            fixed(lt.y_off - ky * ls.y_off - kc * (m[0][1] + m[0][2]) * ls.c_off) + half as i32,
            fixed(ky * m[0][0]),
            fixed(kc * m[0][1]),
            fixed(kc * m[0][2]),
        ],
        u: [
            fixed(lt.c_off - cc * (m[1][1] + m[1][2]) * ls.c_off) + half as i32,
            fixed(cc * m[1][1]),
            fixed(cc * m[1][2]),
        ],
        v: [
            fixed(lt.c_off - cc * (m[2][1] + m[2][2]) * ls.c_off) + half as i32,
            fixed(cc * m[2][1]),
            fixed(cc * m[2][2]),
        ],
        max: (1 << format.bit_depth()) - 1,
    };
    reshape(out, format, src.width, src.height, src.pts, src.time_base);
    match (src.format, format) {
        (PixelFormat::Yuv420p8, PixelFormat::Yuv420p8) => affine::<u8, u8>(src, a, out),
        (PixelFormat::Yuv420p8, PixelFormat::Yuv420p10) => affine::<u8, u16>(src, a, out),
        (PixelFormat::Yuv420p10, PixelFormat::Yuv420p8) => affine::<u16, u8>(src, a, out),
        (PixelFormat::Yuv420p10, PixelFormat::Yuv420p10) => affine::<u16, u16>(src, a, out),
    }
}

fn affine<S: Sample, T: Sample>(src: &Frame, a: Affine, out: &mut Frame) -> Result<()> {
    let w = src.width as usize;
    let h = src.height as usize;
    if w == 0 || h == 0 {
        return Ok(());
    }
    let cw = w.div_ceil(2);
    let (sy, su, sv) = (
        src.planes[0].stride / S::BYTES,
        src.planes[1].stride / S::BYTES,
        src.planes[2].stride / S::BYTES,
    );
    let (iy, iu, iv) = (
        samples::<S>(&src.planes[0])?,
        samples::<S>(&src.planes[1])?,
        samples::<S>(&src.planes[2])?,
    );
    let [py, pu, pv] = &mut out.planes;
    let (ty, tu, tv) = (
        py.stride / T::BYTES,
        pu.stride / T::BYTES,
        pv.stride / T::BYTES,
    );
    let oy = samples_mut::<T>(py)?;
    let ou = samples_mut::<T>(pu)?;
    let ov = samples_mut::<T>(pv)?;
    let luma_only = a.y[2] == 0 && a.y[3] == 0;
    oy.par_chunks_mut(ty * 2)
        .zip(ou.par_chunks_mut(tu))
        .zip(ov.par_chunks_mut(tv))
        .enumerate()
        .with_min_len(rows_job(h.div_ceil(2)))
        .for_each(|(cy, ((yrows, urow), vrow))| {
            let u = &iu[cy * su..cy * su + cw];
            let v = &iv[cy * sv..cy * sv + cw];
            for (r, yrow) in yrows.chunks_mut(ty).enumerate() {
                let y = cy * 2 + r;
                let src_row = &iy[y * sy..y * sy + w];
                let dst = &mut yrow[..w];
                if luma_only {
                    for (o, &s) in dst.iter_mut().zip(src_row) {
                        *o = T::put(((a.y[0] + a.y[1] * s.get()) >> FRAC).clamp(0, a.max));
                    }
                } else {
                    for (x, (o, &s)) in dst.iter_mut().zip(src_row).enumerate() {
                        let c = a.y[2] * u[x / 2].get() + a.y[3] * v[x / 2].get();
                        *o = T::put(((a.y[0] + a.y[1] * s.get() + c) >> FRAC).clamp(0, a.max));
                    }
                }
            }
            for x in 0..cw {
                let (cu, cv) = (u[x].get(), v[x].get());
                urow[x] = T::put(((a.u[0] + a.u[1] * cu + a.u[2] * cv) >> FRAC).clamp(0, a.max));
                vrow[x] = T::put(((a.v[0] + a.v[1] * cu + a.v[2] * cv) >> FRAC).clamp(0, a.max));
            }
        });
    Ok(())
}
