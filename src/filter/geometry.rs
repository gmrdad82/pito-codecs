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
            d.par_chunks_mut(ds * TILE)
                .enumerate()
                .for_each(|(band, chunk)| {
                    for x0 in (0..dw).step_by(TILE) {
                        let x1 = (x0 + TILE).min(dw);
                        for (r, row) in chunk.chunks_mut(ds).enumerate() {
                            let y = band * TILE + r;
                            if rotation == Rotation::R90 {
                                for (x, o) in row[x0..x1].iter_mut().enumerate() {
                                    *o = s[(sh - 1 - (x0 + x)) * ss + y];
                                }
                            } else {
                                let col = sw - 1 - y;
                                for (x, o) in row[x0..x1].iter_mut().enumerate() {
                                    *o = s[(x0 + x) * ss + col];
                                }
                            }
                        }
                    }
                });
        }
    }
    Ok(())
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
