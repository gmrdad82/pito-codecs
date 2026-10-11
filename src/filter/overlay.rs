use super::color::{ColorSpec, Rgba};
use super::plane::{Sample, check, samples_mut};
use crate::media::{Error, Frame, PixelFormat, Result};

#[derive(Debug, Clone)]
struct Layer {
    width: usize,
    height: usize,
    alpha: Vec<u16>,
    value: Vec<u32>,
    spans: Vec<(usize, usize)>,
}

impl Layer {
    fn new(width: usize, height: usize, alpha: Vec<u16>, value: Vec<u32>) -> Self {
        let spans = (0..height)
            .map(|y| {
                let row = &alpha[y * width..(y + 1) * width];
                match row.iter().position(|&a| a > 0) {
                    Some(first) => {
                        let last = row.iter().rposition(|&a| a > 0).unwrap_or(first);
                        (first, last + 1)
                    }
                    None => (0, 0),
                }
            })
            .collect();
        Layer {
            width,
            height,
            alpha,
            value,
            spans,
        }
    }

    fn draw<S: Sample>(&self, dst: &mut [S], stride: usize, dw: usize, dh: usize, x: i64, y: i64) {
        for r in 0..self.height {
            let ty = y + r as i64;
            if ty < 0 || ty >= dh as i64 {
                continue;
            }
            let (a, b) = self.spans[r];
            let from = (a as i64).max(-x) as usize;
            let to = (b as i64).min(dw as i64 - x);
            if to <= from as i64 {
                continue;
            }
            let to = to as usize;
            let row = &mut dst[ty as usize * stride..];
            let base = r * self.width;
            for c in from..to {
                let al = self.alpha[base + c] as u32;
                if al == 0 {
                    continue;
                }
                let o = &mut row[(x + c as i64) as usize];
                let v = (o.get() as u32 * (256 - al) + self.value[base + c] + 128) >> 8;
                *o = S::put(v as i32);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Overlay {
    format: PixelFormat,
    width: u32,
    height: u32,
    luma: Layer,
    u: Layer,
    v: Layer,
}

impl Overlay {
    pub fn new(image: &Rgba, spec: ColorSpec, format: PixelFormat) -> Result<Self> {
        let w = image.width as usize;
        let h = image.height as usize;
        if image.data.len() < w * h * 4 {
            return Err(Error::Invalid(
                "overlay data is shorter than its size".into(),
            ));
        }
        let l = spec.levels(format.bit_depth());
        let m = spec.matrix.forward();
        let top = ((1u32 << format.bit_depth()) - 1) as f64;
        let mut ya = vec![0u16; w * h];
        let mut yv = vec![0u32; w * h];
        let mut planes = vec![[0.0f64; 3]; w * h];
        for (i, p) in image.data.chunks_exact(4).take(w * h).enumerate() {
            let c = [
                p[0] as f64 / 255.0,
                p[1] as f64 / 255.0,
                p[2] as f64 / 255.0,
            ];
            let dot = |k: usize| m[k][0] * c[0] + m[k][1] * c[1] + m[k][2] * c[2];
            let a = (p[3] as f64 * 256.0 / 255.0).round();
            let y = (l.y_off + l.y_range * dot(0)).clamp(0.0, top);
            ya[i] = a as u16;
            yv[i] = (y * a).round() as u32;
            planes[i] = [
                a,
                a * (l.c_off + l.c_range * dot(1)),
                a * (l.c_off + l.c_range * dot(2)),
            ];
        }
        let cw = w.div_ceil(2);
        let ch = h.div_ceil(2);
        let mut ca = vec![0u16; cw * ch];
        let mut cu = vec![0u32; cw * ch];
        let mut cv = vec![0u32; cw * ch];
        for cy in 0..ch {
            for cx in 0..cw {
                let mut s = [0.0f64; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (px, py) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                    let q = planes[py * w + px];
                    s.iter_mut().zip(q).for_each(|(a, b)| *a += b);
                }
                let i = cy * cw + cx;
                ca[i] = (s[0] / 4.0).round() as u16;
                cu[i] = (s[1] / 4.0).round().clamp(0.0, top * 256.0) as u32;
                cv[i] = (s[2] / 4.0).round().clamp(0.0, top * 256.0) as u32;
            }
        }
        Ok(Overlay {
            format,
            width: image.width,
            height: image.height,
            luma: Layer::new(w, h, ya, yv),
            u: Layer::new(cw, ch, ca.clone(), cu),
            v: Layer::new(cw, ch, ca, cv),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn draw(&self, frame: &mut Frame, x: i32, y: i32) -> Result<()> {
        check(frame)?;
        if frame.format != self.format {
            return Err(Error::Invalid(format!(
                "overlay made for {:?}, frame is {:?}",
                self.format, frame.format
            )));
        }
        let (fw, fh) = (frame.width as usize, frame.height as usize);
        let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
        let (x, y) = (x as i64, y as i64);
        let (cx, cy) = (x.div_euclid(2), y.div_euclid(2));
        let jobs = [
            (&self.luma, fw, fh, x, y),
            (&self.u, cw, ch, cx, cy),
            (&self.v, cw, ch, cx, cy),
        ];
        for (plane, (layer, w, h, px, py)) in frame.planes.iter_mut().zip(jobs) {
            match self.format {
                PixelFormat::Yuv420p8 => {
                    let stride = plane.stride;
                    layer.draw(samples_mut::<u8>(plane)?, stride, w, h, px, py);
                }
                PixelFormat::Yuv420p10 => {
                    let stride = plane.stride / 2;
                    layer.draw(samples_mut::<u16>(plane)?, stride, w, h, px, py);
                }
            }
        }
        Ok(())
    }
}
