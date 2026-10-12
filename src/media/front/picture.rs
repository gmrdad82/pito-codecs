use std::sync::Arc;

use crate::filter::{self, ColorSpec, Kernel, Matrix, Range};

use crate::media::{Frame, Rgba};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub matrix: u8,
    pub full_range: bool,
}

fn spec(color: Color, height: u32) -> ColorSpec {
    let matrix = match color.matrix {
        1 => Matrix::Bt709,
        5 | 6 => Matrix::Bt601,
        9 | 10 => Matrix::Bt2020,
        _ if height >= 720 => Matrix::Bt709,
        _ => Matrix::Bt601,
    };
    let range = if color.full_range {
        Range::Full
    } else {
        Range::Limited
    };
    ColorSpec::new(matrix, range)
}

fn rgba_of(frame: &Frame, spec: ColorSpec) -> Rgba {
    let mut out = Rgba::new(frame.width, frame.height);
    if filter::yuv_to_rgba(frame, spec, &mut out).is_err() {
        return Rgba::new(frame.width, frame.height);
    }
    Rgba {
        width: out.width,
        height: out.height,
        data: out.data,
    }
}

pub fn to_rgba(frame: &Frame) -> Rgba {
    rgba_of(frame, spec(Color::default(), frame.height))
}

pub fn to_rgba_height(frame: &Frame, height: u32) -> Rgba {
    to_rgba_color(frame, height, Color::default())
}

pub fn to_rgba_color(frame: &Frame, height: u32, color: Color) -> Rgba {
    let c = spec(color, frame.height);
    if height == 0 || height >= frame.height || frame.height == 0 {
        return rgba_of(frame, c);
    }
    let w = fit_width(frame.width, frame.height, height);
    rgba_of(&resize(frame, w, height), c)
}

pub fn resize(frame: &Frame, width: u32, height: u32) -> Frame {
    if (frame.width, frame.height) == (width, height) {
        return frame.clone();
    }
    filter::scale(frame, width, height, Kernel::Bilinear).unwrap_or_else(|_| frame.clone())
}

pub fn scale_to_height(img: &Rgba, height: u32) -> Rgba {
    if height == 0 || img.height == 0 || img.height == height {
        return img.clone();
    }
    scale(img, fit_width(img.width, img.height, height), height)
}

pub(super) fn even(v: u32) -> u32 {
    v.max(2) & !1
}

pub(super) fn fit_width(width: u32, height: u32, to: u32) -> u32 {
    even((width as u64 * to as u64 / height as u64) as u32)
}

pub fn scale(img: &Rgba, width: u32, height: u32) -> Rgba {
    let mut out = Rgba::new(width, height);
    let (sw, sh) = (img.width as usize, img.height as usize);
    if sw == 0 || sh == 0 || width == 0 || height == 0 || img.data.len() < sw * sh * 4 {
        return out;
    }
    let fx = sw as f32 / width as f32;
    let fy = sh as f32 / height as f32;
    let span = |o: usize, f: f32, max: usize| {
        let a = ((o as f32 * f).floor() as usize).min(max - 1);
        let b = (((o + 1) as f32 * f).ceil() as usize).clamp(a + 1, max);
        (a, b)
    };
    for oy in 0..height as usize {
        let (y0, y1) = span(oy, fy, sh);
        for ox in 0..width as usize {
            let (x0, x1) = span(ox, fx, sw);
            let mut acc = [0u64; 4];
            for y in y0..y1 {
                let row = &img.data[(y * sw + x0) * 4..(y * sw + x1) * 4];
                for px in row.chunks_exact(4) {
                    for (a, &v) in acc.iter_mut().zip(px) {
                        *a += v as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let o = (oy * width as usize + ox) * 4;
            for (d, a) in out.data[o..o + 4].iter_mut().zip(acc) {
                *d = ((a + n / 2) / n) as u8;
            }
        }
    }
    out
}

pub fn rotate(img: &Rgba, clockwise: u16) -> Rgba {
    let (w, h) = (img.width as usize, img.height as usize);
    let turns = (clockwise / 90) % 4;
    if turns == 0 || img.data.len() < w * h * 4 {
        return img.clone();
    }
    let (ow, oh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = Rgba::new(ow as u32, oh as u32);
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turns {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            let s = (y * w + x) * 4;
            let d = (ny * ow + nx) * 4;
            out.data[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    out
}

pub struct Blender {
    layer: Option<(Arc<Rgba>, filter::Overlay)>,
}

impl Default for Blender {
    fn default() -> Self {
        Self::new()
    }
}

impl Blender {
    pub fn new() -> Self {
        Blender { layer: None }
    }

    pub fn draw(&mut self, frame: &mut Frame, layer: &Arc<Rgba>) {
        let fresh = match &self.layer {
            Some((held, o)) => !Arc::ptr_eq(held, layer) || o.size() != (frame.width, frame.height),
            None => true,
        };
        if fresh {
            let fitted;
            let image = if (layer.width, layer.height) == (frame.width, frame.height) {
                layer.as_ref()
            } else {
                fitted = scale(layer, frame.width, frame.height);
                &fitted
            };
            self.layer =
                filter::Overlay::new(image, spec(Color::default(), frame.height), frame.format)
                    .ok()
                    .map(|o| (layer.clone(), o));
        }
        if let Some((_, o)) = &self.layer {
            let _ = o.draw(frame, 0, 0);
        }
    }
}

pub fn blend(frame: &mut Frame, layer: &Rgba) {
    Blender::new().draw(frame, &Arc::new(layer.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::PixelFormat;
    use crate::media::TimeBase;

    #[test]
    fn an_opaque_layer_paints_its_colour_and_a_clear_one_keeps_the_frame() {
        let mut f = Frame::new(PixelFormat::Yuv420p8, 4, 4, 0, TimeBase::new(1, 30));
        for p in &mut f.planes {
            p.data.fill(128);
        }
        f.planes[0].data.fill(100);
        let mut layer = Rgba::new(4, 4);
        for px in layer.data.chunks_exact_mut(4).take(8) {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
        blend(&mut f, &layer);
        let rgb = to_rgba(&f);
        assert!(rgb.data[0] > 245 && rgb.data[1] < 10 && rgb.data[2] < 10);
        let bottom = &rgb.data[(3 * 4) * 4..(3 * 4) * 4 + 3];
        assert!(bottom.iter().all(|c| c.abs_diff(bottom[0]) <= 1));
        assert_eq!(f.planes[0].data[15], 100);
    }

    #[test]
    fn a_kept_blender_paints_what_a_fresh_blend_paints() {
        let mut layer = Rgba::new(64, 36);
        for (i, px) in layer.data.chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(&[(i * 7) as u8, 200, (i * 3) as u8, (i % 256) as u8]);
        }
        let layer = Arc::new(layer);
        let mut kept = Blender::default();
        for shade in [40u8, 170, 230] {
            let mut a = Frame::new(PixelFormat::Yuv420p8, 64, 36, 0, TimeBase::new(1, 30));
            for (k, p) in a.planes.iter_mut().enumerate() {
                p.data.fill(shade.wrapping_add(k as u8 * 30));
            }
            let mut b = a.clone();
            kept.draw(&mut a, &layer);
            blend(&mut b, &layer);
            assert_eq!(a, b, "{shade}");
        }
    }
}
