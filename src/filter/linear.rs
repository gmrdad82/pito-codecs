use super::color::{ColorSpec, Siting};
use super::plane::{Sample, check, reshape, samples, samples_mut, span};
use super::scale::Kernel;
use crate::media::{Error, Frame, PixelFormat, Plane, Result, Rgba};
use rayon::prelude::*;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Light {
    #[default]
    Gamma,
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Transfer {
    #[default]
    Bt1886,
    Bt709,
    Srgb,
}

const REC709_ALPHA: f64 = 1.09929682680944;
const REC709_BETA: f64 = 0.018053968510807;
const SRGB_ALPHA: f64 = 1.055010718947587;
const SRGB_BETA: f64 = 0.003041282560128;

impl Transfer {
    fn to_linear(self, x: f64) -> f64 {
        let x = x.max(0.0);
        match self {
            Transfer::Bt1886 => x.powf(2.4),
            Transfer::Bt709 => {
                if x < 4.5 * REC709_BETA {
                    x / 4.5
                } else {
                    ((x + (REC709_ALPHA - 1.0)) / REC709_ALPHA).powf(1.0 / 0.45)
                }
            }
            Transfer::Srgb => {
                if x < 12.92 * SRGB_BETA {
                    x / 12.92
                } else {
                    ((x + (SRGB_ALPHA - 1.0)) / SRGB_ALPHA).powf(2.4)
                }
            }
        }
    }

    fn to_gamma(self, x: f64) -> f64 {
        let x = x.max(0.0);
        match self {
            Transfer::Bt1886 => x.powf(1.0 / 2.4),
            Transfer::Bt709 => {
                if x < REC709_BETA {
                    x * 4.5
                } else {
                    REC709_ALPHA * x.powf(0.45) - (REC709_ALPHA - 1.0)
                }
            }
            Transfer::Srgb => {
                if x < SRGB_BETA {
                    x * 12.92
                } else {
                    SRGB_ALPHA * x.powf(1.0 / 2.4) - (SRGB_ALPHA - 1.0)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Image<'a> {
    Yuv(&'a Frame),
    Yuva(&'a Frame, &'a Plane),
    Rgba(&'a Rgba),
}

#[derive(Debug)]
pub enum ImageMut<'a> {
    Yuv(&'a mut Frame),
    Yuva(&'a mut Frame, &'a mut Plane),
    Rgba(&'a mut Rgba),
}

const STEPS: usize = 4096;
const SPAN: usize = STEPS * 3 / 2;
const OCTAVE_BITS: u32 = 6;
const FLOOR: i32 = -32;
const TOP: i32 = 2;
const DROP: u32 = 23 - OCTAVE_BITS;
const BASE: usize = ((127 + FLOOR) as usize) << OCTAVE_BITS;
const CELLS: usize = ((TOP - FLOOR) as usize) << OCTAVE_BITS;

#[derive(Clone)]
struct Curve {
    byte: Vec<f32>,
    forward: Vec<f32>,
    inverse: Vec<f32>,
    least: f32,
    most: f32,
}

impl fmt::Debug for Curve {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Curve")
    }
}

impl Curve {
    fn new(light: Light, transfer: Transfer) -> Self {
        let on = light == Light::Linear;
        let lin = |x: f64| if on { transfer.to_linear(x) } else { x };
        let gam = |x: f64| if on { transfer.to_gamma(x) } else { x };
        let byte = (0..256).map(|c| lin(c as f64 / 255.0) as f32).collect();
        let forward = (0..=SPAN + 1)
            .map(|i| lin(i as f64 / STEPS as f64) as f32)
            .collect();
        let inverse = (0..=CELLS + 1)
            .map(|i| {
                let e = FLOOR + (i >> OCTAVE_BITS) as i32;
                let m = (i & ((1 << OCTAVE_BITS) - 1)) as f64 / (1 << OCTAVE_BITS) as f64;
                gam(2f64.powi(e) * (1.0 + m)) as f32
            })
            .collect();
        Curve {
            byte,
            forward,
            inverse,
            least: 2f32.powi(FLOOR),
            most: f32::from_bits((((127 + TOP) as u32) << 23) - 1),
        }
    }

    #[inline(always)]
    fn linear(&self, x: f32) -> f32 {
        let p = (x * STEPS as f32).clamp(0.0, SPAN as f32);
        let i = p as usize;
        let f = p - i as f32;
        let (a, b) = (self.forward[i], self.forward[i + 1]);
        a + (b - a) * f
    }

    fn linear_all(&self, v: &mut [f32], wide: bool) {
        #[cfg(target_arch = "x86_64")]
        let from = if wide {
            unsafe { x86::linear(&self.forward, v) }
        } else {
            0
        };
        #[cfg(not(target_arch = "x86_64"))]
        let from = {
            let _ = wide;
            0
        };
        for x in &mut v[from..] {
            *x = self.linear(*x);
        }
    }

    fn gamma_all(&self, v: &mut [f32], wide: bool) {
        #[cfg(target_arch = "x86_64")]
        let from = if wide {
            unsafe { x86::gamma(&self.inverse, self.least, self.most, v) }
        } else {
            0
        };
        #[cfg(not(target_arch = "x86_64"))]
        let from = {
            let _ = wide;
            0
        };
        for x in &mut v[from..] {
            *x = self.gamma(*x);
        }
    }

    #[inline(always)]
    fn gamma(&self, x: f32) -> f32 {
        if x.is_nan() || x < self.least {
            return 0.0;
        }
        let bits = x.min(self.most).to_bits();
        let i = (bits >> DROP) as usize - BASE;
        let f = (bits & ((1 << DROP) - 1)) as f32 * (1.0 / (1u32 << DROP) as f32);
        let (a, b) = (self.inverse[i], self.inverse[i + 1]);
        a + (b - a) * f
    }
}

#[derive(Debug, Clone)]
struct Filter {
    n: usize,
    left: Vec<usize>,
    coef: Vec<f32>,
}

fn round_halfup(x: f64) -> f64 {
    if x < 0.0 {
        (x + 0.5).floor()
    } else {
        (x + 0.49999999999999994).floor()
    }
}

impl Filter {
    fn new(kernel: Kernel, src: usize, dst: usize) -> Self {
        let scale = dst as f64 / src as f64;
        let step = scale.min(1.0);
        let support = kernel.radius() / step;
        let size = ((support.ceil() as usize) * 2).max(1);
        let mut dense = vec![0.0f64; src];
        let mut rows = Vec::with_capacity(dst);
        let mut n = 1;
        for i in 0..dst {
            let pos = (i as f64 + 0.5) / scale;
            let begin = round_halfup(pos - size as f64 / 2.0) + 0.5;
            let mut total: f64 = (0..size)
                .map(|j| kernel.weight((begin + j as f64 - pos) * step))
                .sum();
            if total.abs() < 1e-12 {
                total = 1.0;
            }
            let (mut lo, mut hi) = (src - 1, 0);
            for j in 0..size {
                let x = begin + j as f64;
                let real = if x < 0.0 {
                    -x
                } else if x >= src as f64 {
                    2.0 * src as f64 - x
                } else {
                    x
                };
                let idx = (real.floor().max(0.0) as usize).min(src - 1);
                dense[idx] += kernel.weight((x - pos) * step) / total;
                lo = lo.min(idx);
                hi = hi.max(idx);
            }
            let row: Vec<f64> = dense[lo..=hi].to_vec();
            dense[lo..=hi].iter_mut().for_each(|v| *v = 0.0);
            n = n.max(row.len());
            rows.push((lo, row));
        }
        let mut left = Vec::with_capacity(dst);
        let mut coef = vec![0.0f32; dst * n];
        for (i, (lo, row)) in rows.iter().enumerate() {
            let l = (*lo).min(src - n);
            let mut err = 0.0f64;
            for j in 0..n {
                let at = l + j;
                let c = if at >= *lo && at - lo < row.len() {
                    row[at - lo]
                } else {
                    0.0
                };
                let want = c - err;
                let got = want as f32;
                err = got as f64 - want;
                coef[i * n + j] = got;
            }
            left.push(l);
        }
        Filter { n, left, coef }
    }

    fn pairs(&self) -> (usize, Vec<f32>) {
        let np = self.n.div_ceil(2);
        let rows = self.left.len();
        let mut out = vec![0.0f32; rows * np * 8];
        for x in 0..rows {
            for t in 0..self.n {
                let c = self.coef[x * self.n + t];
                let at = (x * np + t / 2) * 8 + (t % 2) * 4;
                out[at..at + 4].iter_mut().for_each(|v| *v = c);
            }
        }
        (np, out)
    }
}

#[derive(Debug, Clone, Copy)]
struct Decode {
    y_off: f32,
    y_mul: f32,
    c_off: f32,
    c_mul: f32,
    rv: f32,
    gu: f32,
    gv: f32,
    bu: f32,
    a_mul: f32,
    even: (f32, f32),
    odd: (f32, f32),
}

impl Decode {
    fn new(spec: ColorSpec, depth: u8) -> Self {
        let l = spec.levels(depth);
        let m = spec.matrix.inverse();
        let (even, odd) = match spec.siting {
            Siting::Left => ((0.0, 1.0), (0.5, 0.5)),
            Siting::Centre => ((0.25, 0.75), (0.75, 0.25)),
        };
        Decode {
            y_off: l.y_off as f32,
            y_mul: (1.0 / l.y_range) as f32,
            c_off: l.c_off as f32,
            c_mul: (1.0 / l.c_range) as f32,
            rv: m[0][2] as f32,
            gu: m[1][1] as f32,
            gv: m[1][2] as f32,
            bu: m[2][1] as f32,
            a_mul: 1.0 / ((1u32 << depth) - 1) as f32,
            even,
            odd,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Encode {
    y: [f32; 3],
    u: [f32; 3],
    v: [f32; 3],
    y_off: f32,
    y_range: f32,
    c_off: f32,
    c_range: f32,
    max: f32,
    taps: [f32; 3],
}

impl Encode {
    fn new(spec: ColorSpec, depth: u8) -> Self {
        let l = spec.levels(depth);
        let m = spec.matrix.forward();
        let row = |i: usize| [m[i][0] as f32, m[i][1] as f32, m[i][2] as f32];
        Encode {
            y: row(0),
            u: row(1),
            v: row(2),
            y_off: l.y_off as f32,
            y_range: l.y_range as f32,
            c_off: l.c_off as f32,
            c_range: l.c_range as f32,
            max: ((1u32 << depth) - 1) as f32,
            taps: match spec.siting {
                Siting::Left => [1.0, 2.0, 1.0],
                Siting::Centre => [0.0, 1.0, 1.0],
            },
        }
    }
}

struct Yuv<'a, S> {
    y: &'a [S],
    u: &'a [S],
    v: &'a [S],
    a: Option<&'a [S]>,
    ys: usize,
    cs: usize,
    al: usize,
    k: Decode,
}

enum Input<'a> {
    Narrow(Yuv<'a, u8>),
    Wide(Yuv<'a, u16>),
    Rgba(&'a [u8]),
}

#[derive(Default)]
struct Rows {
    r: Vec<f32>,
    g: Vec<f32>,
    b: Vec<f32>,
    a: Vec<f32>,
    ue: Vec<f32>,
    ve: Vec<f32>,
    cu: Vec<f32>,
    cv: Vec<f32>,
}

impl Rows {
    fn fit(&mut self, w: usize) {
        for v in [
            &mut self.r,
            &mut self.g,
            &mut self.b,
            &mut self.a,
            &mut self.ue,
            &mut self.ve,
        ] {
            v.resize(w + 2, 0.0);
        }
        self.cu.resize(w.div_ceil(2) + 2, 0.0);
        self.cv.resize(w.div_ceil(2) + 2, 0.0);
    }
}

#[derive(Default)]
struct Scratch {
    buf: Vec<f32>,
    tmp: Vec<f32>,
    out: Vec<f32>,
    offs: Vec<usize>,
    rows: Rows,
}

#[derive(Debug, Clone)]
pub(crate) struct Linear {
    src: (usize, usize),
    dst: (usize, usize),
    h: Filter,
    v: Filter,
    np: usize,
    pairs: Vec<f32>,
    curve: Curve,
    from: ColorSpec,
    to: ColorSpec,
    output: PixelFormat,
    wide: bool,
}

impl Linear {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        src: (u32, u32),
        dst: (u32, u32),
        kernel: Kernel,
        light: Light,
        transfer: Transfer,
        from: ColorSpec,
        to: ColorSpec,
        output: PixelFormat,
    ) -> Self {
        let src = (src.0 as usize, src.1 as usize);
        let dst = (dst.0 as usize, dst.1 as usize);
        let h = Filter::new(kernel, src.0, dst.0);
        let v = Filter::new(kernel, src.1, dst.1);
        let (np, pairs) = h.pairs();
        Linear {
            src,
            dst,
            h,
            v,
            np,
            pairs,
            curve: Curve::new(light, transfer),
            from,
            to,
            output,
            wide: wide(),
        }
    }

    pub(crate) fn convert(&self, src: Image, dst: ImageMut) -> Result<()> {
        let (sw, sh) = self.src;
        let size = |w: u32, h: u32| (w as usize, h as usize);
        let input = match src {
            Image::Yuv(f) => {
                if size(f.width, f.height) != self.src {
                    return Err(self.mismatch(f.width, f.height));
                }
                check(f)?;
                yuv_input(f, None, self.from)?
            }
            Image::Yuva(f, a) => {
                if size(f.width, f.height) != self.src {
                    return Err(self.mismatch(f.width, f.height));
                }
                check(f)?;
                alpha_fits(a, sw, sh, f.format.bytes_per_sample())?;
                yuv_input(f, Some(a), self.from)?
            }
            Image::Rgba(p) => {
                if size(p.width, p.height) != self.src {
                    return Err(self.mismatch(p.width, p.height));
                }
                if p.data.len() < sw * sh * 4 {
                    return Err(Error::Invalid("rgba data is too short".into()));
                }
                Input::Rgba(&p.data)
            }
        };
        let (dw, dh) = self.dst;
        let (w32, h32) = (dw as u32, dh as u32);
        let stamp = match src {
            Image::Yuv(f) | Image::Yuva(f, _) => Some((f.pts, f.time_base)),
            Image::Rgba(_) => None,
        };
        match dst {
            ImageMut::Rgba(p) => {
                p.width = w32;
                p.height = h32;
                p.data.resize(dw * dh * 4, 0);
                self.to_rgba(&input, &mut p.data);
                Ok(())
            }
            ImageMut::Yuv(f) => {
                let (pts, tb) = stamp.unwrap_or((f.pts, f.time_base));
                reshape(f, self.output, w32, h32, pts, tb);
                self.to_yuv(&input, f, None)
            }
            ImageMut::Yuva(f, a) => {
                let (pts, tb) = stamp.unwrap_or((f.pts, f.time_base));
                reshape(f, self.output, w32, h32, pts, tb);
                let bytes = self.output.bytes_per_sample();
                a.width = dw;
                a.height = dh;
                a.stride = dw * bytes;
                a.data.resize(dw * bytes * dh, 0);
                self.to_yuv(&input, f, Some(a))
            }
        }
    }

    fn mismatch(&self, w: u32, h: u32) -> Error {
        Error::Invalid(format!(
            "scaler expects {}x{}, got {w}x{h}",
            self.src.0, self.src.1
        ))
    }

    fn band_rows(&self) -> usize {
        let threads = rayon::current_num_threads().max(1);
        let rows = self.dst.1.div_ceil(threads).max(16);
        (rows + 1) & !1
    }

    fn band(
        &self,
        input: &Input,
        y0: usize,
        y1: usize,
        s: &mut Scratch,
        mut emit: impl FnMut(usize, &[f32], &mut Rows),
    ) {
        let (sw, sh) = self.src;
        let (dw, _) = self.dst;
        let n = self.v.n;
        let line = dw * 4;
        s.buf.resize(n * line, 0.0);
        s.tmp.resize((sw + 2) * 4, 0.0);
        s.out.resize(line, 0.0);
        s.offs.resize(n, 0);
        s.rows.fit(sw.max(dw));
        let wide = self.wide;
        let stride = sw * 4;
        let mut next = self.v.left[y0];
        let mut have = 0usize;
        for y in y0..y1 {
            let l = self.v.left[y];
            if l > next || l + have < next {
                next = l;
                have = 0;
            }
            while next < l + n {
                let r = next;
                let row = &mut s.tmp[..stride];
                match input {
                    Input::Narrow(p) => yuv_row(p, r, sw, sh, &self.curve, &mut s.rows, row, wide),
                    Input::Wide(p) => yuv_row(p, r, sw, sh, &self.curve, &mut s.rows, row, wide),
                    Input::Rgba(d) => rgba_row(&d[r * stride..(r + 1) * stride], &self.curve, row),
                }
                let slot = (r % n) * line;
                horizontal(
                    &s.tmp,
                    &self.h,
                    self.np,
                    &self.pairs,
                    &mut s.buf[slot..slot + line],
                    wide,
                );
                next += 1;
                have = (have + 1).min(n);
            }
            for (t, o) in s.offs.iter_mut().enumerate() {
                *o = ((l + t) % n) * line;
            }
            let c = &self.v.coef[y * n..(y + 1) * n];
            vertical(&s.buf, &s.offs, c, &mut s.out, wide);
            straight(&s.out, &self.curve, &mut s.rows, dw, wide);
            emit(y, &s.out, &mut s.rows);
        }
    }

    fn to_rgba(&self, input: &Input, data: &mut [u8]) {
        let (dw, dh) = self.dst;
        let rows = self.band_rows();
        let line = dw * 4;
        data[..line * dh]
            .par_chunks_mut(line * rows)
            .enumerate()
            .for_each_init(Scratch::default, |s, (b, chunk)| {
                let y0 = b * rows;
                let y1 = (y0 + rows).min(dh);
                self.band(input, y0, y1, s, |y, _, rows| {
                    let o = &mut chunk[(y - y0) * line..(y - y0 + 1) * line];
                    put_rgba(rows, dw, o);
                });
            });
    }

    fn to_yuv(&self, input: &Input, f: &mut Frame, a: Option<&mut Plane>) -> Result<()> {
        match self.output {
            PixelFormat::Yuv420p8 => self.to_yuv_with::<u8>(input, f, a),
            PixelFormat::Yuv420p10 => self.to_yuv_with::<u16>(input, f, a),
        }
    }

    fn to_yuv_with<S: Sample>(
        &self,
        input: &Input,
        f: &mut Frame,
        a: Option<&mut Plane>,
    ) -> Result<()> {
        let (dw, dh) = self.dst;
        let cw = dw.div_ceil(2);
        let rows = self.band_rows();
        let k = Encode::new(self.to, self.output.bit_depth());
        let [py, pu, pv] = &mut f.planes;
        let (ys, us, vs) = (
            py.stride / S::BYTES,
            pu.stride / S::BYTES,
            pv.stride / S::BYTES,
        );
        let yp = samples_mut::<S>(py)?;
        let up = samples_mut::<S>(pu)?;
        let vp = samples_mut::<S>(pv)?;
        let mut alphas: Vec<Option<&mut [S]>> = match a {
            Some(p) => {
                let st = p.stride / S::BYTES;
                samples_mut::<S>(p)?
                    .chunks_mut(st * rows)
                    .map(Some)
                    .collect()
            }
            None => Vec::new(),
        };
        let has_alpha = !alphas.is_empty();
        let bands = dh.div_ceil(rows);
        alphas.resize_with(bands, || None);
        let jobs: Vec<_> = yp
            .chunks_mut(ys * rows)
            .zip(up.chunks_mut(us * rows / 2))
            .zip(vp.chunks_mut(vs * rows / 2))
            .zip(alphas)
            .enumerate()
            .collect();
        jobs.into_par_iter().for_each_init(
            || (Scratch::default(), vec![0.0f32; dw * 6]),
            |(s, pair), (b, (((yc, uc), vc), mut ac))| {
                let y0 = b * rows;
                let y1 = (y0 + rows).min(dh);
                self.band(input, y0, y1, s, |y, _, rows| {
                    let r = y - y0;
                    let (top, bottom) = pair.split_at_mut(dw * 3);
                    let half = if r.is_multiple_of(2) {
                        &mut *top
                    } else {
                        &mut *bottom
                    };
                    let alpha = ac.as_deref_mut().map(|a| &mut a[r * dw..(r + 1) * dw]);
                    put_luma(
                        rows,
                        dw,
                        &k,
                        has_alpha,
                        &mut yc[r * ys..r * ys + dw],
                        alpha,
                        half,
                    );
                    let last = y + 1 == dh;
                    if !r.is_multiple_of(2) || last {
                        let below: &[f32] = if !r.is_multiple_of(2) { bottom } else { top };
                        let c = r / 2;
                        put_chroma(
                            top,
                            below,
                            &k,
                            dw,
                            &mut uc[c * us..c * us + cw],
                            &mut vc[c * vs..c * vs + cw],
                        );
                    }
                });
            },
        );
        Ok(())
    }
}

fn wide() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

fn alpha_fits(a: &Plane, w: usize, h: usize, bytes: usize) -> Result<()> {
    if a.width < w || a.height < h || a.stride < w * bytes || !a.stride.is_multiple_of(bytes) {
        return Err(Error::Invalid(
            "alpha plane is smaller than the frame".into(),
        ));
    }
    if span(a.stride, h, w * bytes).is_none_or(|n| a.data.len() < n) {
        return Err(Error::Invalid("alpha plane data is too short".into()));
    }
    Ok(())
}

fn yuv_input<'a>(f: &'a Frame, a: Option<&'a Plane>, spec: ColorSpec) -> Result<Input<'a>> {
    let k = Decode::new(spec, f.format.bit_depth());
    Ok(match f.format {
        PixelFormat::Yuv420p8 => Input::Narrow(yuv_view(f, a, k)?),
        PixelFormat::Yuv420p10 => Input::Wide(yuv_view(f, a, k)?),
    })
}

fn yuv_view<'a, S: Sample>(f: &'a Frame, a: Option<&'a Plane>, k: Decode) -> Result<Yuv<'a, S>> {
    if f.planes[1].stride != f.planes[2].stride {
        return Err(Error::Invalid("chroma planes need one stride".into()));
    }
    Ok(Yuv {
        y: samples(&f.planes[0])?,
        u: samples(&f.planes[1])?,
        v: samples(&f.planes[2])?,
        a: a.map(samples).transpose()?,
        ys: f.planes[0].stride / S::BYTES,
        cs: f.planes[1].stride / S::BYTES,
        al: a.map_or(0, |a| a.stride / S::BYTES),
        k,
    })
}

#[allow(clippy::too_many_arguments)]
fn yuv_row<S: Sample>(
    p: &Yuv<S>,
    r: usize,
    w: usize,
    h: usize,
    curve: &Curve,
    s: &mut Rows,
    out: &mut [f32],
    wide: bool,
) {
    let k = p.k;
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    let k0 = r / 2;
    let k1 = if r.is_multiple_of(2) {
        k0.saturating_sub(1)
    } else {
        (k0 + 1).min(ch - 1)
    };
    let (u0, u1) = (&p.u[k0 * p.cs..][..cw], &p.u[k1 * p.cs..][..cw]);
    let (v0, v1) = (&p.v[k0 * p.cs..][..cw], &p.v[k1 * p.cs..][..cw]);
    let (cu, cv) = (&mut s.cu, &mut s.cv);
    for (o, (&a, &b)) in cu[1..=cw].iter_mut().zip(u0.iter().zip(u1)) {
        *o = (0.75 * a.get() as f32 + 0.25 * b.get() as f32 - k.c_off) * k.c_mul;
    }
    for (o, (&a, &b)) in cv[1..=cw].iter_mut().zip(v0.iter().zip(v1)) {
        *o = (0.75 * a.get() as f32 + 0.25 * b.get() as f32 - k.c_off) * k.c_mul;
    }
    cu[0] = cu[1];
    cv[0] = cv[1];
    cu[cw + 1] = cu[cw];
    cv[cw + 1] = cv[cw];
    let (e0, e1) = k.even;
    let (o1, o2) = k.odd;
    for (c, (ue, ve)) in s.ue[..2 * cw]
        .chunks_exact_mut(2)
        .zip(s.ve[..2 * cw].chunks_exact_mut(2))
        .enumerate()
    {
        let c = c + 1;
        ue[0] = e0 * cu[c - 1] + e1 * cu[c];
        ue[1] = o1 * cu[c] + o2 * cu[c + 1];
        ve[0] = e0 * cv[c - 1] + e1 * cv[c];
        ve[1] = o1 * cv[c] + o2 * cv[c + 1];
    }
    let yrow = &p.y[r * p.ys..][..w];
    for (((((&y, &u), &v), rr), gg), bb) in yrow
        .iter()
        .zip(&s.ue[..w])
        .zip(&s.ve[..w])
        .zip(&mut s.r[..w])
        .zip(&mut s.g[..w])
        .zip(&mut s.b[..w])
    {
        let yf = (y.get() as f32 - k.y_off) * k.y_mul;
        *rr = yf + k.rv * v;
        *gg = yf + k.gu * u + k.gv * v;
        *bb = yf + k.bu * u;
    }
    curve.linear_all(&mut s.r[..w], wide);
    curve.linear_all(&mut s.g[..w], wide);
    curve.linear_all(&mut s.b[..w], wide);
    let planes = s.r[..w].iter().zip(&s.g[..w]).zip(&s.b[..w]);
    match p.a.map(|a| &a[r * p.al..][..w]) {
        Some(a) => {
            for ((px, ((&rr, &gg), &bb)), &al) in
                out[..w * 4].chunks_exact_mut(4).zip(planes).zip(a)
            {
                let al = al.get() as f32 * k.a_mul;
                px.copy_from_slice(&[rr * al, gg * al, bb * al, al]);
            }
        }
        None => {
            for (px, ((&rr, &gg), &bb)) in out[..w * 4].chunks_exact_mut(4).zip(planes) {
                px.copy_from_slice(&[rr, gg, bb, 1.0]);
            }
        }
    }
}

fn rgba_row(src: &[u8], curve: &Curve, out: &mut [f32]) {
    let lut = &curve.byte[..256];
    for (s, o) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let a = s[3] as f32 * (1.0 / 255.0);
        o[0] = lut[s[0] as usize] * a;
        o[1] = lut[s[1] as usize] * a;
        o[2] = lut[s[2] as usize] * a;
        o[3] = a;
    }
}

fn straight(row: &[f32], curve: &Curve, s: &mut Rows, w: usize, wide: bool) {
    for ((((px, rr), gg), bb), aa) in row[..w * 4]
        .chunks_exact(4)
        .zip(&mut s.r[..w])
        .zip(&mut s.g[..w])
        .zip(&mut s.b[..w])
        .zip(&mut s.a[..w])
    {
        let a = px[3].clamp(0.0, 1.0);
        let inv = if a > 1e-6 { 1.0 / a } else { 0.0 };
        *rr = px[0] * inv;
        *gg = px[1] * inv;
        *bb = px[2] * inv;
        *aa = a;
    }
    curve.gamma_all(&mut s.r[..w], wide);
    curve.gamma_all(&mut s.g[..w], wide);
    curve.gamma_all(&mut s.b[..w], wide);
}

fn put_rgba(s: &Rows, w: usize, out: &mut [u8]) {
    let q = |v: f32| (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    for ((((o, &r), &g), &b), &a) in out[..w * 4]
        .chunks_exact_mut(4)
        .zip(&s.r[..w])
        .zip(&s.g[..w])
        .zip(&s.b[..w])
        .zip(&s.a[..w])
    {
        o.copy_from_slice(&[q(r), q(g), q(b), q(a)]);
    }
}

fn put_luma<S: Sample>(
    s: &Rows,
    w: usize,
    k: &Encode,
    weigh: bool,
    luma: &mut [S],
    alpha: Option<&mut [S]>,
    half: &mut [f32],
) {
    let (hu, rest) = half.split_at_mut(w);
    let (hv, hw) = rest.split_at_mut(w);
    for (((((((l, &r), &g), &b), &a), u), v), wt) in luma[..w]
        .iter_mut()
        .zip(&s.r[..w])
        .zip(&s.g[..w])
        .zip(&s.b[..w])
        .zip(&s.a[..w])
        .zip(hu.iter_mut())
        .zip(hv.iter_mut())
        .zip(hw.iter_mut())
    {
        let (r, g, b) = (r.min(1.0), g.min(1.0), b.min(1.0));
        let y = k.y[0] * r + k.y[1] * g + k.y[2] * b;
        *l = S::put((k.y_off + y * k.y_range + 0.5).clamp(0.0, k.max) as i32);
        let t = if weigh { a } else { 1.0 };
        *u = t * (k.u[0] * r + k.u[1] * g + k.u[2] * b);
        *v = t * (k.v[0] * r + k.v[1] * g + k.v[2] * b);
        *wt = t;
    }
    if let Some(alpha) = alpha {
        for (o, &a) in alpha[..w].iter_mut().zip(&s.a[..w]) {
            *o = S::put((a * k.max + 0.5) as i32);
        }
    }
}

fn put_chroma<S: Sample>(
    top: &[f32],
    bottom: &[f32],
    k: &Encode,
    w: usize,
    u: &mut [S],
    v: &mut [S],
) {
    let last = w - 1;
    let q = |c: f32| S::put((k.c_off + c * k.c_range + 0.5).clamp(0.0, k.max) as i32);
    for (x, (ou, ov)) in u.iter_mut().zip(v.iter_mut()).enumerate() {
        let a = 2 * x;
        let at = [a.saturating_sub(1), a, (a + 1).min(last)];
        let (mut su, mut sv, mut sw) = (0.0f32, 0.0f32, 0.0f32);
        for (&i, &t) in at.iter().zip(&k.taps) {
            su += t * (top[i] + bottom[i]);
            sv += t * (top[w + i] + bottom[w + i]);
            sw += t * (top[2 * w + i] + bottom[2 * w + i]);
        }
        let (cu, cv) = if sw > 1e-6 {
            (su / sw, sv / sw)
        } else {
            (0.0, 0.0)
        };
        *ou = q(cu);
        *ov = q(cv);
    }
}

fn vertical(buf: &[f32], offs: &[usize], coef: &[f32], out: &mut [f32], wide: bool) {
    let n = coef.len();
    let len = out.len();
    assert!(n > 0 && offs.len() >= n && offs.iter().all(|&o| o + len <= buf.len()));
    #[cfg(target_arch = "x86_64")]
    let from = if wide {
        unsafe { x86::vertical(buf, offs, coef, out) }
    } else {
        0
    };
    #[cfg(not(target_arch = "x86_64"))]
    let from = {
        let _ = wide;
        0
    };
    let out = &mut out[from..];
    let len = out.len();
    let c0 = coef[0];
    for (o, &s) in out
        .iter_mut()
        .zip(&buf[offs[0] + from..offs[0] + from + len])
    {
        *o = c0 * s;
    }
    for (t, &c) in coef.iter().enumerate().skip(1) {
        let row = &buf[offs[t] + from..offs[t] + from + len];
        for (o, &s) in out.iter_mut().zip(row) {
            *o += c * s;
        }
    }
}

fn horizontal(row: &[f32], f: &Filter, np: usize, pairs: &[f32], out: &mut [f32], wide: bool) {
    let dw = f.left.len();
    let reach = f.left.iter().max().copied().unwrap_or(0) + np * 2;
    assert!(row.len() >= reach * 4 && out.len() >= dw * 4 && pairs.len() >= dw * np * 8);
    #[cfg(target_arch = "x86_64")]
    if wide {
        unsafe { x86::horizontal(row, &f.left, np, pairs, out) };
        return;
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = wide;
    let n = f.n;
    for (x, o) in out[..dw * 4].chunks_exact_mut(4).enumerate() {
        let l = f.left[x] * 4;
        let mut acc = [0.0f32; 4];
        for (t, &c) in f.coef[x * n..(x + 1) * n].iter().enumerate() {
            let s = &row[l + t * 4..l + t * 4 + 4];
            for k in 0..4 {
                acc[k] += c * s[k];
            }
        }
        o.copy_from_slice(&acc);
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{BASE, DROP, SPAN, STEPS};
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn linear(t: &[f32], v: &mut [f32]) -> usize {
        assert!(t.len() >= SPAN + 2);
        let steps = _mm256_set1_ps(STEPS as f32);
        let top = _mm256_set1_ps(SPAN as f32);
        let zero = _mm256_setzero_ps();
        let p = v.as_mut_ptr();
        let mut i = 0;
        unsafe {
            while i + 8 <= v.len() {
                let x = _mm256_mul_ps(_mm256_loadu_ps(p.add(i)), steps);
                let x = _mm256_min_ps(_mm256_max_ps(x, zero), top);
                let k = _mm256_cvttps_epi32(x);
                let f = _mm256_sub_ps(x, _mm256_cvtepi32_ps(k));
                let a = _mm256_i32gather_ps::<4>(t.as_ptr(), k);
                let b = _mm256_i32gather_ps::<4>(t.as_ptr().add(1), k);
                _mm256_storeu_ps(p.add(i), _mm256_fmadd_ps(_mm256_sub_ps(b, a), f, a));
                i += 8;
            }
        }
        i
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn gamma(t: &[f32], least: f32, most: f32, v: &mut [f32]) -> usize {
        assert!(t.len() >= super::CELLS + 2);
        let low = _mm256_set1_ps(least);
        let high = _mm256_set1_ps(most);
        let base = _mm256_set1_epi32(BASE as i32);
        let mask = _mm256_set1_epi32((1 << DROP) - 1);
        let scale = _mm256_set1_ps(1.0 / (1u32 << DROP) as f32);
        let p = v.as_mut_ptr();
        let mut i = 0;
        unsafe {
            while i + 8 <= v.len() {
                let x = _mm256_loadu_ps(p.add(i));
                let ok = _mm256_cmp_ps::<_CMP_GE_OQ>(x, low);
                let x = _mm256_blendv_ps(low, _mm256_min_ps(x, high), ok);
                let bits = _mm256_castps_si256(x);
                let k = _mm256_sub_epi32(_mm256_srli_epi32::<{ DROP as i32 }>(bits), base);
                let f = _mm256_mul_ps(_mm256_cvtepi32_ps(_mm256_and_si256(bits, mask)), scale);
                let a = _mm256_i32gather_ps::<4>(t.as_ptr(), k);
                let b = _mm256_i32gather_ps::<4>(t.as_ptr().add(1), k);
                let y = _mm256_fmadd_ps(_mm256_sub_ps(b, a), f, a);
                _mm256_storeu_ps(p.add(i), _mm256_and_ps(y, ok));
                i += 8;
            }
        }
        i
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn vertical(
        buf: &[f32],
        offs: &[usize],
        coef: &[f32],
        out: &mut [f32],
    ) -> usize {
        let len = out.len();
        let b = buf.as_ptr();
        let o = out.as_mut_ptr();
        let mut i = 0;
        unsafe {
            while i + 32 <= len {
                let mut a = [_mm256_setzero_ps(); 4];
                for (t, &c) in coef.iter().enumerate() {
                    let c = _mm256_set1_ps(c);
                    let p = b.add(offs[t] + i);
                    for (j, acc) in a.iter_mut().enumerate() {
                        *acc = _mm256_fmadd_ps(_mm256_loadu_ps(p.add(8 * j)), c, *acc);
                    }
                }
                for (j, acc) in a.iter().enumerate() {
                    _mm256_storeu_ps(o.add(i + 8 * j), *acc);
                }
                i += 32;
            }
            while i + 8 <= len {
                let mut acc = _mm256_setzero_ps();
                for (t, &c) in coef.iter().enumerate() {
                    acc = _mm256_fmadd_ps(
                        _mm256_loadu_ps(b.add(offs[t] + i)),
                        _mm256_set1_ps(c),
                        acc,
                    );
                }
                _mm256_storeu_ps(o.add(i), acc);
                i += 8;
            }
        }
        i
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn horizontal(
        row: &[f32],
        left: &[usize],
        np: usize,
        pairs: &[f32],
        out: &mut [f32],
    ) {
        let dw = left.len();
        let r = row.as_ptr();
        let c = pairs.as_ptr();
        let o = out.as_mut_ptr();
        let fold = |v: __m256| _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps::<1>(v));
        let mut x = 0;
        unsafe {
            while x + 4 <= dw {
                let mut a = [_mm256_setzero_ps(); 4];
                let s = [0, 1, 2, 3].map(|j| r.add(left[x + j] * 4));
                let w = c.add(x * np * 8);
                for p in 0..np {
                    for (j, acc) in a.iter_mut().enumerate() {
                        let k = w.add((j * np + p) * 8);
                        *acc = _mm256_fmadd_ps(
                            _mm256_loadu_ps(s[j].add(8 * p)),
                            _mm256_loadu_ps(k),
                            *acc,
                        );
                    }
                }
                for (j, acc) in a.iter().enumerate() {
                    _mm_storeu_ps(o.add((x + j) * 4), fold(*acc));
                }
                x += 4;
            }
            while x < dw {
                let s = r.add(left[x] * 4);
                let w = c.add(x * np * 8);
                let mut acc = _mm256_setzero_ps();
                for p in 0..np {
                    acc = _mm256_fmadd_ps(
                        _mm256_loadu_ps(s.add(8 * p)),
                        _mm256_loadu_ps(w.add(8 * p)),
                        acc,
                    );
                }
                _mm_storeu_ps(o.add(x * 4), fold(acc));
                x += 1;
            }
        }
    }
}
