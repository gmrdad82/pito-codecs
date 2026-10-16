use super::color::{ColorSpec, Siting};
use super::linear::{Image, ImageMut, Light, Linear, Transfer};
use super::plane::{Sample, Simd, check, plane_dims, reshape, samples, samples_mut, simd, sized};
use crate::media::{Error, Frame, PixelFormat, Result};
use rayon::prelude::*;
use std::sync::Arc;

const BITS: i32 = 14;
const ONE: f64 = (1 << BITS) as f64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kernel {
    Bilinear,
    Bicubic,
    Lanczos,
}

impl Kernel {
    pub(super) fn radius(self) -> f64 {
        match self {
            Kernel::Bilinear => 1.0,
            Kernel::Bicubic => 2.0,
            Kernel::Lanczos => 3.0,
        }
    }

    pub(super) fn weight(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Kernel::Bilinear => (1.0 - x).max(0.0),
            Kernel::Bicubic => {
                if x < 1.0 {
                    (1.5 * x - 2.5) * x * x + 1.0
                } else if x < 2.0 {
                    ((-0.5 * x + 2.5) * x - 4.0) * x + 2.0
                } else {
                    0.0
                }
            }
            Kernel::Lanczos => {
                if x >= 3.0 {
                    0.0
                } else {
                    sinc(x) * sinc(x / 3.0)
                }
            }
        }
    }
}

fn sinc(x: f64) -> f64 {
    if x < 1e-9 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

#[derive(Debug, Clone)]
struct Taps {
    n: usize,
    start: Vec<usize>,
    coef: Vec<i16>,
    blocks: usize,
    reach: usize,
    packed: Vec<i16>,
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    quad: Vec<i16>,
}

impl Taps {
    fn new(src: usize, dst: usize, ratio: f64, sub: f64, offset: f64, kernel: Kernel) -> Self {
        let widen = ratio.max(1.0);
        let support = kernel.radius() * widen;
        let n = ((2.0 * support).ceil() as usize).clamp(1, src);
        let mut start = Vec::with_capacity(dst);
        let mut coef = Vec::with_capacity(dst * n);
        let mut w = vec![0.0f64; n];
        for j in 0..dst {
            let c = ((j as f64 * sub + offset + 0.5) * ratio - 0.5 - offset) / sub;
            let first = (c - support).floor() as i64 + 1;
            let s = first.clamp(0, (src - n) as i64);
            w.iter_mut().for_each(|v| *v = 0.0);
            for k in 0..n as i64 {
                let i = first + k;
                let at = i.clamp(0, src as i64 - 1);
                w[(at - s) as usize] += kernel.weight((i as f64 - c) / widen);
            }
            let sum: f64 = w.iter().sum();
            let mut q: Vec<i16> = w.iter().map(|v| (v / sum * ONE).round() as i16).collect();
            let total: i32 = q.iter().map(|&v| v as i32).sum();
            let big = (0..n).max_by_key(|&k| q[k]).unwrap_or(0);
            q[big] = (q[big] as i32 + (1 << BITS) - total) as i16;
            start.push(s as usize);
            coef.extend_from_slice(&q);
        }
        let blocks = n.div_ceil(8);
        let pairs = dst.div_ceil(2);
        let mut packed = vec![0i16; pairs * blocks * 16];
        for j in 0..dst {
            let (pair, side) = (j / 2, j % 2);
            for t in 0..n {
                let (b, k) = (t / 8, t % 8);
                packed[(pair * blocks + b) * 16 + side * 8 + k] = coef[j * n + t];
            }
        }
        let reach = start.iter().max().copied().unwrap_or(0) + blocks * 8;
        let mut quad = Vec::new();
        if n <= 4 {
            quad = vec![0i16; dst.div_ceil(4) * 16];
            for j in 0..dst {
                for t in 0..n {
                    quad[j / 4 * 16 + j % 4 * 4 + t] = coef[j * n + t];
                }
            }
        }
        Taps {
            n,
            start,
            coef,
            blocks,
            reach,
            packed,
            quad,
        }
    }
}

#[derive(Debug, Clone)]
struct PlaneTaps {
    src: (usize, usize),
    dst: (usize, usize),
    h: Taps,
    v: Taps,
}

impl PlaneTaps {
    fn new(src: (u32, u32), dst: (u32, u32), i: usize, offset: f64, kernel: Kernel) -> Self {
        let rx = src.0 as f64 / dst.0 as f64;
        let ry = src.1 as f64 / dst.1 as f64;
        let (sub, oy) = if i == 0 { (1.0, 0.0) } else { (2.0, 0.5) };
        let s = plane_dims(src.0, src.1, i);
        let d = plane_dims(dst.0, dst.1, i);
        PlaneTaps {
            src: s,
            dst: d,
            h: Taps::new(s.0, d.0, rx, sub, offset, kernel),
            v: Taps::new(s.1, d.1, ry, sub, oy, kernel),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scaler {
    format: PixelFormat,
    kernel: Kernel,
    src: (u32, u32),
    dst: (u32, u32),
    planes: [PlaneTaps; 2],
    simd: Simd,
    light: Light,
    transfer: Transfer,
    from: ColorSpec,
    to: ColorSpec,
    output: PixelFormat,
    linear: Option<Arc<Linear>>,
}

impl Scaler {
    pub fn new(
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
        format: PixelFormat,
        kernel: Kernel,
    ) -> Result<Self> {
        if src_width == 0 || src_height == 0 || dst_width == 0 || dst_height == 0 {
            return Err(Error::Invalid("scale needs non-zero sizes".into()));
        }
        sized(src_width, src_height)?;
        sized(dst_width, dst_height)?;
        let src = (src_width, src_height);
        let dst = (dst_width, dst_height);
        Ok(Scaler {
            format,
            kernel,
            src,
            dst,
            planes: [
                PlaneTaps::new(src, dst, 0, 0.0, kernel),
                PlaneTaps::new(src, dst, 1, 0.0, kernel),
            ],
            simd: simd(),
            light: Light::Gamma,
            transfer: Transfer::Bt1886,
            from: ColorSpec::BT709,
            to: ColorSpec::BT709,
            output: format,
            linear: None,
        })
    }

    pub fn light(mut self, light: Light) -> Self {
        self.light = light;
        self.rebuild()
    }

    pub fn transfer(mut self, transfer: Transfer) -> Self {
        self.transfer = transfer;
        self.rebuild()
    }

    pub fn color(mut self, from: ColorSpec, to: ColorSpec) -> Self {
        self.from = from;
        self.to = to;
        self.rebuild()
    }

    pub fn output(mut self, format: PixelFormat) -> Self {
        self.output = format;
        self.rebuild()
    }

    fn engine(&self) -> Linear {
        Linear::new(
            self.src,
            self.dst,
            self.kernel,
            self.light,
            self.transfer,
            self.from,
            self.to,
            self.output,
        )
    }

    fn rebuild(mut self) -> Self {
        self.linear = Some(Arc::new(self.engine()));
        self
    }

    pub fn convert(&self, src: Image, dst: ImageMut) -> Result<()> {
        match &self.linear {
            Some(engine) => engine.convert(src, dst),
            None => self.engine().convert(src, dst),
        }
    }

    pub fn siting(mut self, siting: Siting) -> Self {
        let offset = match siting {
            Siting::Left => 0.0,
            Siting::Centre => 0.5,
        };
        self.planes[1] = PlaneTaps::new(self.src, self.dst, 1, offset, self.kernel);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_simd(mut self, simd: Simd) -> Self {
        self.simd = simd;
        self
    }

    pub fn run(&self, src: &Frame, dst: &mut Frame) -> Result<()> {
        if src.format != self.format || (src.width, src.height) != self.src {
            return Err(Error::Invalid(format!(
                "scaler expects {:?} {}x{}, got {:?} {}x{}",
                self.format, self.src.0, self.src.1, src.format, src.width, src.height
            )));
        }
        if self.light == Light::Linear {
            return self.convert(Image::Yuv(src), ImageMut::Yuv(dst));
        }
        if self.output != self.format {
            return Err(Error::Invalid(
                "a gamma-light scaler keeps its pixel format".into(),
            ));
        }
        check(src)?;
        reshape(
            dst,
            self.format,
            self.dst.0,
            self.dst.1,
            src.pts,
            src.time_base,
        );
        let depth = self.format.bit_depth() as i32;
        for i in 0..3 {
            let taps = &self.planes[i.min(1)];
            let (s, d) = (&src.planes[i], &mut dst.planes[i]);
            let (ss, ds) = (s.stride, d.stride);
            match self.format {
                PixelFormat::Yuv420p8 => {
                    run_plane::<u8>(samples(s)?, ss, samples_mut(d)?, ds, taps, depth, self.simd)
                }
                PixelFormat::Yuv420p10 => run_plane::<u16>(
                    samples(s)?,
                    ss / 2,
                    samples_mut(d)?,
                    ds / 2,
                    taps,
                    depth,
                    self.simd,
                ),
            }
        }
        Ok(())
    }
}

pub fn scale(frame: &Frame, width: u32, height: u32, kernel: Kernel) -> Result<Frame> {
    let scaler = Scaler::new(
        frame.width,
        frame.height,
        width,
        height,
        frame.format,
        kernel,
    )?;
    let mut out = Frame::new(frame.format, width, height, frame.pts, frame.time_base);
    scaler.run(frame, &mut out)?;
    Ok(out)
}

fn run_plane<S: Sample>(
    src: &[S],
    src_stride: usize,
    dst: &mut [S],
    dst_stride: usize,
    taps: &PlaneTaps,
    depth: i32,
    simd: Simd,
) {
    let (sw, sh) = taps.src;
    let (dw, dh) = taps.dst;
    if dw == 0 || dh == 0 {
        return;
    }
    if (sw, sh) == (dw, dh) {
        dst.par_chunks_mut(dst_stride)
            .zip(src.par_chunks(src_stride))
            .for_each(|(d, s)| d[..dw].copy_from_slice(&s[..dw]));
        return;
    }
    let rows = rows_per_job(dh, sw + dw);
    let pad = taps.h.blocks * 8 + 16;
    dst.par_chunks_mut(dst_stride * rows)
        .enumerate()
        .for_each_init(
            || vec![0i16; sw + pad],
            |tmp, (job, chunk)| {
                for (r, out) in chunk.chunks_mut(dst_stride).enumerate() {
                    let y = job * rows + r;
                    let n = taps.v.n;
                    let coef = &taps.v.coef[y * n..(y + 1) * n];
                    let base = &src[taps.v.start[y] * src_stride..];
                    vertical(base, src_stride, coef, sw, tmp, depth, simd);
                    horizontal(tmp, &taps.h, &mut out[..dw], depth, simd);
                }
            },
        );
}

fn rows_per_job(height: usize, width: usize) -> usize {
    let threads = rayon::current_num_threads().max(1);
    let by_threads = height.div_ceil(threads * 4);
    let by_work = (16384 / width.max(1)).max(1);
    by_threads.max(by_work).clamp(1, height)
}

fn vertical<S: Sample>(
    src: &[S],
    stride: usize,
    coef: &[i16],
    width: usize,
    tmp: &mut [i16],
    depth: i32,
    simd: Simd,
) {
    let n = coef.len();
    assert!(src.len() >= (n - 1) * stride + width && tmp.len() >= width);
    let from = match simd {
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { x86::vertical(src, stride, coef, width, tmp, depth) },
        #[cfg(target_arch = "aarch64")]
        Simd::Neon => neon::vertical(src, stride, coef, width, tmp, depth),
        _ => 0,
    };
    vertical_scalar(src, stride, coef, from, width, tmp, depth);
}

fn vertical_scalar<S: Sample>(
    src: &[S],
    stride: usize,
    coef: &[i16],
    from: usize,
    width: usize,
    tmp: &mut [i16],
    depth: i32,
) {
    let round = 1 << (depth - 1);
    let mut acc = [0i32; 64];
    let mut x = from;
    while x < width {
        let len = (width - x).min(64);
        acc[..len].iter_mut().for_each(|a| *a = round);
        for (t, &c) in coef.iter().enumerate() {
            let row = &src[t * stride + x..t * stride + x + len];
            for (a, &s) in acc[..len].iter_mut().zip(row) {
                *a += s.get() * c as i32;
            }
        }
        for (o, &a) in tmp[x..x + len].iter_mut().zip(&acc[..len]) {
            *o = (a >> depth).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        x += len;
    }
}

fn horizontal<S: Sample>(tmp: &[i16], taps: &Taps, out: &mut [S], depth: i32, simd: Simd) {
    assert!(
        tmp.len() >= taps.reach && taps.packed.len() >= out.len().div_ceil(2) * taps.blocks * 16
    );
    let from = match simd {
        #[cfg(target_arch = "x86_64")]
        Simd::Avx2 => unsafe { x86::horizontal(tmp, taps, out, depth) },
        #[cfg(target_arch = "aarch64")]
        Simd::Neon => neon::horizontal(tmp, taps, out, depth),
        _ => 0,
    };
    horizontal_scalar(tmp, taps, out, from, depth);
}

fn horizontal_scalar<S: Sample>(tmp: &[i16], taps: &Taps, out: &mut [S], from: usize, depth: i32) {
    let shift = 2 * BITS - depth;
    let round = 1 << (shift - 1);
    let max = (1 << depth) - 1;
    let n = taps.n;
    for (x, o) in out.iter_mut().enumerate().skip(from) {
        let s = taps.start[x];
        let c = &taps.coef[x * n..(x + 1) * n];
        let acc: i32 = tmp[s..s + n]
            .iter()
            .zip(c)
            .map(|(&a, &b)| a as i32 * b as i32)
            .sum();
        *o = S::put(((acc + round) >> shift).clamp(0, max));
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{BITS, Sample, Taps};
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    unsafe fn load16<S: Sample>(p: *const S) -> __m256i {
        unsafe {
            if S::BYTES == 1 {
                _mm256_cvtepu8_epi16(_mm_loadu_si128(p as *const __m128i))
            } else {
                _mm256_loadu_si256(p as *const __m256i)
            }
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn vertical<S: Sample>(
        src: &[S],
        stride: usize,
        coef: &[i16],
        width: usize,
        tmp: &mut [i16],
        depth: i32,
    ) -> usize {
        let n = coef.len();
        let base = src.as_ptr();
        let round = _mm256_set1_epi32(1 << (depth - 1));
        let shift = _mm_cvtsi32_si128(depth);
        let zero = _mm256_setzero_si256();
        let mut x = 0;
        while x + 16 <= width {
            let mut lo = round;
            let mut hi = round;
            let mut t = 0;
            unsafe {
                while t + 2 <= n {
                    let a = load16(base.add(t * stride + x));
                    let b = load16(base.add((t + 1) * stride + x));
                    let c =
                        _mm256_set1_epi32((coef[t] as u16 as i32) | ((coef[t + 1] as i32) << 16));
                    lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, b), c));
                    hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, b), c));
                    t += 2;
                }
                if t < n {
                    let a = load16(base.add(t * stride + x));
                    let c = _mm256_set1_epi32(coef[t] as u16 as i32);
                    lo = _mm256_add_epi32(lo, _mm256_madd_epi16(_mm256_unpacklo_epi16(a, zero), c));
                    hi = _mm256_add_epi32(hi, _mm256_madd_epi16(_mm256_unpackhi_epi16(a, zero), c));
                }
                lo = _mm256_sra_epi32(lo, shift);
                hi = _mm256_sra_epi32(hi, shift);
                _mm256_storeu_si256(
                    tmp.as_mut_ptr().add(x) as *mut __m256i,
                    _mm256_packs_epi32(lo, hi),
                );
            }
            x += 16;
        }
        x
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn horizontal<S: Sample>(
        tmp: &[i16],
        taps: &Taps,
        out: &mut [S],
        depth: i32,
    ) -> usize {
        let width = out.len();
        let blocks = taps.blocks;
        let shift = 2 * BITS - depth;
        let round = _mm256_set1_epi32(1 << (shift - 1));
        let count = _mm_cvtsi32_si128(shift);
        let max = _mm256_set1_epi32((1 << depth) - 1);
        let zero = _mm256_setzero_si256();
        let order = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);
        let t = tmp.as_ptr();
        let mut x = 0;
        if taps.n <= 4 && taps.quad.len() >= width.div_ceil(4) * 16 {
            let order = _mm256_setr_epi32(0, 1, 4, 5, 2, 3, 6, 7);
            let q = taps.quad.as_ptr();
            let at = |p: usize| unsafe { (t.add(taps.start[p]) as *const i64).read_unaligned() };
            while x + 8 <= width {
                let a = _mm256_setr_epi64x(at(x), at(x + 1), at(x + 2), at(x + 3));
                let b = _mm256_setr_epi64x(at(x + 4), at(x + 5), at(x + 6), at(x + 7));
                let (wa, wb) = unsafe {
                    (
                        _mm256_loadu_si256(q.add(x / 4 * 16) as *const __m256i),
                        _mm256_loadu_si256(q.add(x / 4 * 16 + 16) as *const __m256i),
                    )
                };
                let h = _mm256_hadd_epi32(_mm256_madd_epi16(a, wa), _mm256_madd_epi16(b, wb));
                let h = _mm256_permutevar8x32_epi32(h, order);
                let v = _mm256_sra_epi32(_mm256_add_epi32(h, round), count);
                let v = _mm256_min_epi32(_mm256_max_epi32(v, zero), max);
                let v = _mm256_castsi256_si128(_mm256_permute4x64_epi64::<8>(_mm256_packus_epi32(
                    v, v,
                )));
                unsafe {
                    let o = out.as_mut_ptr().add(x);
                    if S::BYTES == 1 {
                        _mm_storel_epi64(o as *mut __m128i, _mm_packus_epi16(v, v));
                    } else {
                        _mm_storeu_si128(o as *mut __m128i, v);
                    }
                }
                x += 8;
            }
            return x;
        }
        while x + 8 <= width {
            let mut r = [zero; 4];
            for (k, acc) in r.iter_mut().enumerate() {
                let p = x + 2 * k;
                let s0 = taps.start[p];
                let s1 = taps.start[p + 1];
                let c = unsafe { taps.packed.as_ptr().add(p / 2 * blocks * 16) };
                for b in 0..blocks {
                    unsafe {
                        let a = _mm256_inserti128_si256::<1>(
                            _mm256_castsi128_si256(_mm_loadu_si128(
                                t.add(s0 + 8 * b) as *const __m128i
                            )),
                            _mm_loadu_si128(t.add(s1 + 8 * b) as *const __m128i),
                        );
                        let w = _mm256_loadu_si256(c.add(16 * b) as *const __m256i);
                        *acc = _mm256_add_epi32(*acc, _mm256_madd_epi16(a, w));
                    }
                }
            }
            let h01 = _mm256_hadd_epi32(r[0], r[1]);
            let h23 = _mm256_hadd_epi32(r[2], r[3]);
            let h = _mm256_permutevar8x32_epi32(_mm256_hadd_epi32(h01, h23), order);
            let v = _mm256_sra_epi32(_mm256_add_epi32(h, round), count);
            let v = _mm256_min_epi32(_mm256_max_epi32(v, zero), max);
            let v =
                _mm256_castsi256_si128(_mm256_permute4x64_epi64::<8>(_mm256_packus_epi32(v, v)));
            unsafe {
                let o = out.as_mut_ptr().add(x);
                if S::BYTES == 1 {
                    _mm_storel_epi64(o as *mut __m128i, _mm_packus_epi16(v, v));
                } else {
                    _mm_storeu_si128(o as *mut __m128i, v);
                }
            }
            x += 8;
        }
        x
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{BITS, Sample, Taps};
    use std::arch::aarch64::*;

    unsafe fn load8<S: Sample>(p: *const S) -> int16x8_t {
        unsafe {
            if S::BYTES == 1 {
                vreinterpretq_s16_u16(vmovl_u8(vld1_u8(p as *const u8)))
            } else {
                vreinterpretq_s16_u16(vld1q_u16(p as *const u16))
            }
        }
    }

    pub(super) fn vertical<S: Sample>(
        src: &[S],
        stride: usize,
        coef: &[i16],
        width: usize,
        tmp: &mut [i16],
        depth: i32,
    ) -> usize {
        let base = src.as_ptr();
        let mut x = 0;
        while x + 8 <= width {
            unsafe {
                let mut lo = vdupq_n_s32(1 << (depth - 1));
                let mut hi = lo;
                for (t, &c) in coef.iter().enumerate() {
                    let row = load8(base.add(t * stride + x));
                    lo = vmlal_n_s16(lo, vget_low_s16(row), c);
                    hi = vmlal_high_n_s16(hi, row, c);
                }
                let shift = vdupq_n_s32(-depth);
                let v = vcombine_s16(
                    vqmovn_s32(vshlq_s32(lo, shift)),
                    vqmovn_s32(vshlq_s32(hi, shift)),
                );
                vst1q_s16(tmp.as_mut_ptr().add(x), v);
            }
            x += 8;
        }
        x
    }

    pub(super) fn horizontal<S: Sample>(
        tmp: &[i16],
        taps: &Taps,
        out: &mut [S],
        depth: i32,
    ) -> usize {
        let shift = 2 * BITS - depth;
        let round = 1 << (shift - 1);
        let max = (1 << depth) - 1;
        let blocks = taps.blocks;
        for (p, o) in out.iter_mut().enumerate() {
            let s = taps.start[p];
            let c = &taps.packed[(p / 2 * blocks) * 16 + (p % 2) * 8..];
            let sum = unsafe {
                let mut acc = vdupq_n_s32(0);
                for b in 0..blocks {
                    let v = vld1q_s16(tmp.as_ptr().add(s + 8 * b));
                    let w = vld1q_s16(c.as_ptr().add(16 * b));
                    acc = vmlal_s16(acc, vget_low_s16(v), vget_low_s16(w));
                    acc = vmlal_high_s16(acc, v, w);
                }
                vaddvq_s32(acc)
            };
            *o = S::put(((sum + round) >> shift).clamp(0, max));
        }
        out.len()
    }
}
