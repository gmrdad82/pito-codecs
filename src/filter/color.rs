#[cfg(target_arch = "x86_64")]
use super::plane::Simd;
use super::plane::{Sample, check, reshape, samples, samples_mut, simd};
use crate::media::{Frame, PixelFormat, Result};
use rayon::prelude::*;

pub(crate) const FRAC: i32 = 16;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Siting {
    #[default]
    Left,
    Centre,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorSpec {
    pub matrix: Matrix,
    pub range: Range,
    pub siting: Siting,
}

impl ColorSpec {
    pub const BT601: ColorSpec = ColorSpec::new(Matrix::Bt601, Range::Limited);
    pub const BT709: ColorSpec = ColorSpec::new(Matrix::Bt709, Range::Limited);
    pub const BT2020: ColorSpec = ColorSpec::new(Matrix::Bt2020, Range::Limited);

    pub const fn new(matrix: Matrix, range: Range) -> Self {
        ColorSpec {
            matrix,
            range,
            siting: Siting::Left,
        }
    }

    pub const fn with_siting(self, siting: Siting) -> Self {
        ColorSpec { siting, ..self }
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

pub(crate) fn fixed(v: f64) -> i32 {
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

pub(crate) fn rows_job(rows: usize) -> usize {
    (rows / (rayon::current_num_threads().max(1) * 4)).max(1)
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
    let simd = simd();
    oy.par_chunks_mut(ty * 2)
        .zip(ou.par_chunks_mut(tu))
        .zip(ov.par_chunks_mut(tv))
        .enumerate()
        .with_min_len(rows_job(h.div_ceil(2)))
        .for_each_init(Vec::new, |c, (cy, ((yrows, urow), vrow))| {
            let u = &iu[cy * su..cy * su + cw];
            let v = &iv[cy * sv..cy * sv + cw];
            let rows = (h - cy * 2).min(2);
            let ys: [&[S]; 2] = std::array::from_fn(|r| {
                let y = cy * 2 + r.min(rows - 1);
                &iy[y * sy..y * sy + w]
            });
            let job = Job {
                a,
                u,
                v,
                ys,
                rows,
                w,
            };
            match simd {
                #[cfg(target_arch = "x86_64")]
                Simd::Avx2 => unsafe { job.run_avx2(c, yrows, ty, urow, vrow) },
                _ => job.run(c, yrows, ty, urow, vrow),
            }
        });
    Ok(())
}

struct Job<'a, S> {
    a: Affine,
    u: &'a [S],
    v: &'a [S],
    ys: [&'a [S]; 2],
    rows: usize,
    w: usize,
}

impl<S: Sample> Job<'_, S> {
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn run_avx2<T: Sample>(
        &self,
        c: &mut Vec<i32>,
        yrows: &mut [T],
        ty: usize,
        urow: &mut [T],
        vrow: &mut [T],
    ) {
        self.run(c, yrows, ty, urow, vrow);
    }

    #[inline(always)]
    fn run<T: Sample>(
        &self,
        c: &mut Vec<i32>,
        yrows: &mut [T],
        ty: usize,
        urow: &mut [T],
        vrow: &mut [T],
    ) {
        let a = self.a;
        let cw = self.u.len();
        c.clear();
        c.resize(cw * 2, 0);
        for (pair, (&u, &v)) in c.chunks_exact_mut(2).zip(self.u.iter().zip(self.v)) {
            let t = a.y[0] + a.y[2] * u.get() + a.y[3] * v.get();
            pair[0] = t;
            pair[1] = t;
        }
        for (r, src) in self.ys.iter().enumerate().take(self.rows) {
            let dst = &mut yrows[r * ty..r * ty + self.w];
            for ((o, &s), &t) in dst.iter_mut().zip(*src).zip(c.iter()) {
                *o = T::put(((t + a.y[1] * s.get()) >> FRAC).clamp(0, a.max));
            }
        }
        for (((o, p), &u), &v) in urow[..cw]
            .iter_mut()
            .zip(vrow[..cw].iter_mut())
            .zip(self.u)
            .zip(self.v)
        {
            let (u, v) = (u.get(), v.get());
            *o = T::put(((a.u[0] + a.u[1] * u + a.u[2] * v) >> FRAC).clamp(0, a.max));
            *p = T::put(((a.v[0] + a.v[1] * u + a.v[2] * v) >> FRAC).clamp(0, a.max));
        }
    }
}
