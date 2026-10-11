use super::color::{ColorSpec, Range};
use super::plane::{Sample, check, plane_dims, reshape, samples, samples_mut};
use crate::media::{Error, Frame, PixelFormat, Result};
use rayon::prelude::*;

const TILE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rotation {
    R90,
    R180,
    R270,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fill {
    pub y: u16,
    pub u: u16,
    pub v: u16,
}

impl Fill {
    pub fn black(range: Range, format: PixelFormat) -> Self {
        let d = format.bit_depth() as u32;
        match range {
            Range::Limited => Fill {
                y: 16 << (d - 8),
                u: 128 << (d - 8),
                v: 128 << (d - 8),
            },
            Range::Full => Fill {
                y: 0,
                u: 1 << (d - 1),
                v: 1 << (d - 1),
            },
        }
    }

    pub fn rgb(r: u8, g: u8, b: u8, spec: ColorSpec, format: PixelFormat) -> Self {
        let l = spec.levels(format.bit_depth());
        let m = spec.matrix.forward();
        let c = [r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0];
        let dot = |i: usize| m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2];
        let top = ((1u32 << format.bit_depth()) - 1) as f64;
        let code = |v: f64| v.round().clamp(0.0, top) as u16;
        Fill {
            y: code(l.y_off + l.y_range * dot(0)),
            u: code(l.c_off + l.c_range * dot(1)),
            v: code(l.c_off + l.c_range * dot(2)),
        }
    }
}

pub fn rotate(src: &Frame, rotation: Rotation, out: &mut Frame) -> Result<()> {
    check(src)?;
    let (w, h) = match rotation {
        Rotation::R180 => (src.width, src.height),
        _ => (src.height, src.width),
    };
    reshape(out, src.format, w, h, src.pts, src.time_base);
    for i in 0..3 {
        match src.format {
            PixelFormat::Yuv420p8 => rotate_plane::<u8>(src, out, i, rotation)?,
            PixelFormat::Yuv420p10 => rotate_plane::<u16>(src, out, i, rotation)?,
        }
    }
    Ok(())
}

fn rotate_plane<S: Sample>(
    src: &Frame,
    out: &mut Frame,
    i: usize,
    rotation: Rotation,
) -> Result<()> {
    let (sw, sh) = plane_dims(src.width, src.height, i);
    let (dw, dh) = plane_dims(out.width, out.height, i);
    let ss = src.planes[i].stride / S::BYTES;
    let ds = out.planes[i].stride / S::BYTES;
    let s = samples::<S>(&src.planes[i])?;
    let d = samples_mut::<S>(&mut out.planes[i])?;
    if dw == 0 || dh == 0 {
        return Ok(());
    }
    match rotation {
        Rotation::R180 => {
            d.par_chunks_mut(ds).enumerate().for_each(|(y, row)| {
                let from = &s[(sh - 1 - y) * ss..(sh - 1 - y) * ss + sw];
                for (o, &v) in row[..dw].iter_mut().zip(from.iter().rev()) {
                    *o = v;
                }
            });
        }
        Rotation::R90 | Rotation::R270 => {
            let n = 16 / S::BYTES;
            d.par_chunks_mut(ds * TILE)
                .enumerate()
                .for_each(|(band, chunk)| {
                    let rows = chunk.len().div_ceil(ds).min(dh - band * TILE);
                    let cell = |y: usize, x: usize| {
                        if rotation == Rotation::R90 {
                            s[(sh - 1 - x) * ss + y]
                        } else {
                            s[x * ss + sw - 1 - y]
                        }
                    };
                    for x0 in (0..dw).step_by(n) {
                        for g in (0..rows).step_by(n) {
                            let y0 = band * TILE + g;
                            if x0 + n <= dw
                                && g + n <= rows
                                && transpose::<S>(s, ss, chunk, ds, sh, sw, rotation, g, y0, x0)
                            {
                                continue;
                            }
                            for r in g..(g + n).min(rows) {
                                let end = (x0 + n).min(dw);
                                for (x, o) in
                                    chunk[r * ds + x0..r * ds + end].iter_mut().enumerate()
                                {
                                    *o = cell(band * TILE + r, x0 + x);
                                }
                            }
                        }
                    }
                });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn transpose<S: Sample>(
    s: &[S],
    ss: usize,
    chunk: &mut [S],
    ds: usize,
    sh: usize,
    sw: usize,
    rotation: Rotation,
    g: usize,
    y0: usize,
    x0: usize,
) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        let n = 16 / S::BYTES;
        let mut inp = [std::ptr::null::<u8>(); 16];
        let mut out = [std::ptr::null_mut::<u8>(); 16];
        let base = chunk.as_mut_ptr();
        for k in 0..n {
            let at = if rotation == Rotation::R90 {
                (sh - 1 - x0 - k) * ss + y0
            } else {
                (x0 + k) * ss + sw - n - y0
            };
            inp[k] = s[at..at + n].as_ptr() as *const u8;
            let r = if rotation == Rotation::R90 {
                g + k
            } else {
                g + n - 1 - k
            };
            let at = r * ds + x0;
            assert!(at + n <= chunk.len());
            out[k] = unsafe { base.add(at) } as *mut u8;
        }
        unsafe {
            if S::BYTES == 1 {
                t16(&inp, &out);
            } else {
                t8(&inp, &out);
            }
        }
        true
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (s, ss, chunk, ds, sh, sw, rotation, g, y0, x0);
        false
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn t16(inp: &[*const u8; 16], out: &[*mut u8; 16]) {
    use std::arch::x86_64::*;
    let mut x = [_mm_setzero_si128(); 16];
    for (v, p) in x.iter_mut().zip(inp) {
        *v = unsafe { _mm_loadu_si128(*p as *const __m128i) };
    }
    for _ in 0..4 {
        let mut y = x;
        for i in 0..8 {
            y[2 * i] = _mm_unpacklo_epi8(x[i], x[i + 8]);
            y[2 * i + 1] = _mm_unpackhi_epi8(x[i], x[i + 8]);
        }
        x = y;
    }
    for (v, p) in x.iter().zip(out) {
        unsafe { _mm_storeu_si128(*p as *mut __m128i, *v) };
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn t8(inp: &[*const u8; 16], out: &[*mut u8; 16]) {
    use std::arch::x86_64::*;
    let mut x = [_mm_setzero_si128(); 8];
    for (v, p) in x.iter_mut().zip(inp) {
        *v = unsafe { _mm_loadu_si128(*p as *const __m128i) };
    }
    for _ in 0..3 {
        let mut y = x;
        for i in 0..4 {
            y[2 * i] = _mm_unpacklo_epi16(x[i], x[i + 4]);
            y[2 * i + 1] = _mm_unpackhi_epi16(x[i], x[i + 4]);
        }
        x = y;
    }
    for (v, p) in x.iter().zip(out) {
        unsafe { _mm_storeu_si128(*p as *mut __m128i, *v) };
    }
}

pub fn flip_horizontal(frame: &mut Frame) -> Result<()> {
    check(frame)?;
    let bytes = frame.format.bytes_per_sample();
    for i in 0..3 {
        let (w, _) = plane_dims(frame.width, frame.height, i);
        let p = &mut frame.planes[i];
        let stride = p.stride;
        if bytes == 1 {
            p.data
                .par_chunks_mut(stride)
                .for_each(|row| row[..w].reverse());
        } else {
            samples_mut::<u16>(p)?
                .par_chunks_mut(stride / 2)
                .for_each(|row| row[..w].reverse());
        }
    }
    Ok(())
}

pub fn flip_vertical(frame: &mut Frame) -> Result<()> {
    check(frame)?;
    let bytes = frame.format.bytes_per_sample();
    for i in 0..3 {
        let (w, h) = plane_dims(frame.width, frame.height, i);
        let p = &mut frame.planes[i];
        let stride = p.stride;
        let (top, bottom) = p.data.split_at_mut(stride * (h / 2));
        let bottom = &mut bottom[stride * (h % 2)..];
        top.par_chunks_mut(stride)
            .zip(bottom.par_chunks_mut(stride).rev())
            .for_each(|(a, b)| a[..w * bytes].swap_with_slice(&mut b[..w * bytes]));
    }
    Ok(())
}

pub fn crop(src: &Frame, x: u32, y: u32, width: u32, height: u32, out: &mut Frame) -> Result<()> {
    check(src)?;
    if width == 0 || height == 0 || x + width > src.width || y + height > src.height {
        return Err(Error::Invalid(format!(
            "crop {width}x{height} at {x},{y} is outside {}x{}",
            src.width, src.height
        )));
    }
    reshape(out, src.format, width, height, src.pts, src.time_base);
    let bytes = src.format.bytes_per_sample();
    for i in 0..3 {
        let (ox, oy) = if i == 0 { (x, y) } else { (x / 2, y / 2) };
        let (w, _) = plane_dims(width, height, i);
        let s = &src.planes[i];
        let d = &mut out.planes[i];
        let ds = d.stride;
        d.data.par_chunks_mut(ds).enumerate().for_each(|(r, row)| {
            let at = (oy as usize + r) * s.stride + ox as usize * bytes;
            row[..w * bytes].copy_from_slice(&s.data[at..at + w * bytes]);
        });
    }
    Ok(())
}

pub fn pad(
    src: &Frame,
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    fill: Fill,
    out: &mut Frame,
) -> Result<()> {
    check(src)?;
    if x + src.width > width || y + src.height > height {
        return Err(Error::Invalid(format!(
            "{}x{} at {x},{y} does not fit in {width}x{height}",
            src.width, src.height
        )));
    }
    reshape(out, src.format, width, height, src.pts, src.time_base);
    for i in 0..3 {
        let value = [fill.y, fill.u, fill.v][i];
        match src.format {
            PixelFormat::Yuv420p8 => pad_plane::<u8>(src, out, i, x, y, value as u8)?,
            PixelFormat::Yuv420p10 => pad_plane::<u16>(src, out, i, x, y, value)?,
        }
    }
    Ok(())
}

fn pad_plane<S: Sample>(
    src: &Frame,
    out: &mut Frame,
    i: usize,
    x: u32,
    y: u32,
    value: S,
) -> Result<()> {
    let (ox, oy) = if i == 0 {
        (x as usize, y as usize)
    } else {
        (x as usize / 2, y as usize / 2)
    };
    let (sw, sh) = plane_dims(src.width, src.height, i);
    let (dw, _) = plane_dims(out.width, out.height, i);
    let ss = src.planes[i].stride / S::BYTES;
    let ds = out.planes[i].stride / S::BYTES;
    let s = samples::<S>(&src.planes[i])?;
    let d = samples_mut::<S>(&mut out.planes[i])?;
    d.par_chunks_mut(ds).enumerate().for_each(|(r, row)| {
        let row = &mut row[..dw];
        if r < oy || r >= oy + sh {
            row.fill(value);
            return;
        }
        let end = (ox + sw).min(dw);
        row[..ox].fill(value);
        row[ox..end].copy_from_slice(&s[(r - oy) * ss..(r - oy) * ss + end - ox]);
        row[end..].fill(value);
    });
    Ok(())
}
