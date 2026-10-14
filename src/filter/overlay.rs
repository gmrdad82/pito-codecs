use rayon::prelude::*;

use super::color::{ColorSpec, Levels};
use super::plane::{Sample, check, samples_mut};
use crate::media::Rgba;
use crate::media::{Error, Frame, PixelFormat, Result};

const BITS: u32 = 20;
const MASK: u64 = (1 << BITS) - 1;
const SLOTS: usize = 1024;
const BLOCKS: usize = 256;

#[derive(Debug, Clone, Default)]
struct Band<T> {
    top: usize,
    spans: Vec<(usize, usize)>,
    cells: Vec<T>,
}

#[derive(Debug, Clone, Default)]
struct Layer<T> {
    left: i64,
    top: i64,
    width: usize,
    bands: Vec<Band<T>>,
}

impl<T: Copy> Band<T> {
    fn new(top: usize, width: usize, cells: Vec<T>, alpha: impl Fn(T) -> u64) -> Self {
        let spans = cells
            .chunks_exact(width.max(1))
            .map(|row| match row.iter().position(|&c| alpha(c) > 0) {
                Some(first) => {
                    let last = row.iter().rposition(|&c| alpha(c) > 0).unwrap_or(first);
                    (first, last + 1)
                }
                None => (0, 0),
            })
            .collect();
        Band { top, spans, cells }
    }
}

impl<T: Copy> Layer<T> {
    #[allow(clippy::too_many_arguments)]
    fn draw<S: Sample>(
        &self,
        dst: &mut [S],
        stride: usize,
        dw: usize,
        dh: usize,
        x: i64,
        y: i64,
        pick: impl Fn(T) -> (u32, u32),
    ) {
        let (x, y) = (x + self.left, y + self.top);
        for band in &self.bands {
            for (r, &(a, b)) in band.spans.iter().enumerate() {
                let ty = y + (band.top + r) as i64;
                if ty < 0 || ty >= dh as i64 {
                    continue;
                }
                let from = (a as i64).max(-x) as usize;
                let to = (b as i64).min(dw as i64 - x);
                if to <= from as i64 {
                    continue;
                }
                let to = to as usize;
                let at = ty as usize * stride + (x + from as i64) as usize;
                let base = r * self.width;
                let (Some(out), Some(cells)) = (
                    dst.get_mut(at..at + (to - from)),
                    band.cells.get(base + from..base + to),
                ) else {
                    continue;
                };
                for (o, &c) in out.iter_mut().zip(cells) {
                    let (al, v) = pick(c);
                    *o = S::put(((o.get() as u32 * (256 - al) + v + 128) >> 8) as i32);
                }
            }
        }
    }
}

struct Convert<'a> {
    data: &'a [u8],
    width: usize,
    matrix: [[f64; 3]; 3],
    levels: Levels,
    top: f64,
    alpha: [f64; 256],
}

struct Cache {
    keys: [u32; SLOTS],
    values: [[f64; 3]; SLOTS],
}

struct Blocks {
    keys: [u32; BLOCKS],
    values: [(u32, u64); BLOCKS],
}

impl Convert<'_> {
    fn word(&self, x: usize, y: usize) -> u32 {
        let i = (y * self.width + x) * 4;
        self.data
            .get(i..i + 4)
            .and_then(|p| p.try_into().ok())
            .map_or(0, u32::from_le_bytes)
    }

    fn pixel(&self, cache: &mut Cache, word: u32) -> (u32, [f64; 3]) {
        let [r, g, b, a] = word.to_le_bytes();
        if a == 0 {
            return (0, [0.0; 3]);
        }
        let key = word & 0xff_ffff;
        let slot = (key.wrapping_mul(0x9e37_79b1) >> 22) as usize % SLOTS;
        if cache.keys[slot] != key {
            let c = [r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0];
            let m = &self.matrix;
            let dot = |k: usize| m[k][0] * c[0] + m[k][1] * c[1] + m[k][2] * c[2];
            let l = &self.levels;
            cache.keys[slot] = key;
            cache.values[slot] = [
                (l.y_off + l.y_range * dot(0)).clamp(0.0, self.top),
                l.c_off + l.c_range * dot(1),
                l.c_off + l.c_range * dot(2),
            ];
        }
        let [y, u, v] = cache.values[slot];
        let a = self.alpha[a as usize];
        let luma = (a as u32) << BITS | round(y * a) as u32;
        (luma, [a, a * u, a * v])
    }

    fn chroma(&self, q: [[f64; 3]; 4]) -> u64 {
        let mut s = [0.0f64; 3];
        for q in q {
            s.iter_mut().zip(q).for_each(|(a, b)| *a += b);
        }
        let a = round(s[0] / 4.0) as u64;
        if a == 0 {
            return 0;
        }
        let max = self.top * 256.0;
        let u = round(s[1] / 4.0).clamp(0.0, max) as u64;
        let v = round(s[2] / 4.0).clamp(0.0, max) as u64;
        a << (2 * BITS) | u << BITS | v
    }
}

fn round(x: f64) -> f64 {
    let t = x as i64 as f64;
    if x - t >= 0.5 { t + 1.0 } else { t }
}

fn painted(row: &[u8]) -> Option<(usize, usize)> {
    let words = row.chunks_exact(8);
    let tail = words.remainder().get(3).copied().unwrap_or(0);
    let mask = words.fold(0u64, |m, c| m | c.try_into().map_or(0, u64::from_le_bytes));
    if mask & 0xff00_0000_ff00_0000 == 0 && tail == 0 {
        return None;
    }
    let first = row.chunks_exact(4).position(|p| p[3] > 0)?;
    let last = row.chunks_exact(4).rposition(|p| p[3] > 0)?;
    Some((first, last + 1))
}

#[derive(Debug, Clone)]
pub struct Overlay {
    format: PixelFormat,
    width: u32,
    height: u32,
    luma: Layer<u32>,
    chroma: Layer<u64>,
}

impl Overlay {
    pub fn new(image: &Rgba, spec: ColorSpec, format: PixelFormat) -> Result<Self> {
        let w = image.width as usize;
        let h = image.height as usize;
        super::plane::sized(image.width, image.height)?;
        if image.data.len() < w * h * 4 {
            return Err(Error::Invalid(
                "overlay data is shorter than its size".into(),
            ));
        }
        let mut out = Overlay {
            format,
            width: image.width,
            height: image.height,
            luma: Layer::default(),
            chroma: Layer::default(),
        };
        if w == 0 || h == 0 {
            return Ok(out);
        }
        let data = &image.data[..w * h * 4];
        let rows: Vec<Option<(usize, usize)>> = data
            .par_chunks(w * 4)
            .with_min_len(64)
            .map(painted)
            .collect();
        let (Some(y0), Some(y1)) = (
            rows.iter().position(Option::is_some),
            rows.iter().rposition(Option::is_some),
        ) else {
            return Ok(out);
        };
        let (x0, x1) = rows
            .iter()
            .flatten()
            .fold((w, 0), |(a, b), &(f, l)| (a.min(f), b.max(l)));
        let (lx0, ly0) = (x0 & !1, y0 & !1);
        let (lx1, ly1) = (((x1 + 1) & !1).min(w), ((y1 + 2) & !1).min(h));
        let (lw, lh) = (lx1 - lx0, ly1 - ly0);
        let (cw, ch) = (lw.div_ceil(2), lh.div_ceil(2));
        let convert = Convert {
            data,
            width: w,
            matrix: spec.matrix.forward(),
            levels: spec.levels(format.bit_depth()),
            top: ((1u32 << format.bit_depth()) - 1) as f64,
            alpha: std::array::from_fn(|a| (a as f64 * 256.0 / 255.0).round()),
        };
        let k = super::color::rows_job(ch).max(8);
        let (luma, chroma): (Vec<_>, Vec<_>) = (0..ch.div_ceil(k))
            .into_par_iter()
            .map(|n| {
                let (c0, c1) = (n * k, ((n + 1) * k).min(ch));
                let (l0, l1) = (2 * c0, (2 * c1).min(lh));
                let mut yc = vec![0u32; lw * (l1 - l0)];
                let mut cc = vec![0u64; cw * (c1 - c0)];
                let mut cache = Cache {
                    keys: [u32::MAX; SLOTS],
                    values: [[0.0; 3]; SLOTS],
                };
                let mut blocks = Blocks {
                    keys: [0; BLOCKS],
                    values: [(0, 0); BLOCKS],
                };
                for j in 0..c1 - c0 {
                    let r0 = ly0 + l0 + 2 * j;
                    let r1 = (r0 + 1).min(h - 1);
                    let Some((xa, xb)) = [rows.get(r0), rows.get(r1)]
                        .into_iter()
                        .flatten()
                        .flatten()
                        .fold(None, |s: Option<(usize, usize)>, &(f, l)| {
                            Some(s.map_or((f, l), |(a, b)| (a.min(f), b.max(l))))
                        })
                    else {
                        continue;
                    };
                    for bx in xa / 2..xb.div_ceil(2) {
                        let (p0, p1) = (2 * bx, (2 * bx + 1).min(w - 1));
                        let words = [
                            convert.word(p0, r0),
                            convert.word(p1, r0),
                            convert.word(p0, r1),
                            convert.word(p1, r1),
                        ];
                        if words.iter().fold(0, |m, w| m | w) >> 24 == 0 {
                            continue;
                        }
                        let (y, c) = if words.iter().all(|&w| w == words[0]) {
                            let slot = (words[0].wrapping_mul(0x9e37_79b1) >> 24) as usize % BLOCKS;
                            if blocks.keys[slot] != words[0] {
                                let q = convert.pixel(&mut cache, words[0]);
                                blocks.keys[slot] = words[0];
                                blocks.values[slot] = (q.0, convert.chroma([q.1; 4]));
                            }
                            let (y, c) = blocks.values[slot];
                            ([y; 4], c)
                        } else {
                            let q = words.map(|w| convert.pixel(&mut cache, w));
                            (q.map(|q| q.0), convert.chroma(q.map(|q| q.1)))
                        };
                        let row0 = 2 * j * lw;
                        let row1 = row0 + lw;
                        let at = [
                            (row0 + p0 - lx0, true),
                            (row0 + p1 - lx0, p1 != p0),
                            (row1 + p0 - lx0, r1 != r0),
                            (row1 + p1 - lx0, r1 != r0 && p1 != p0),
                        ];
                        for ((i, real), y) in at.into_iter().zip(y) {
                            if real && let Some(o) = yc.get_mut(i) {
                                *o = y;
                            }
                        }
                        if let Some(o) = cc.get_mut(j * cw + bx - lx0 / 2) {
                            *o = c;
                        }
                    }
                }
                (
                    Band::new(l0, lw, yc, |c| (c >> BITS) as u64),
                    Band::new(c0, cw, cc, |c| c >> (2 * BITS)),
                )
            })
            .unzip();
        out.luma = Layer {
            left: lx0 as i64,
            top: ly0 as i64,
            width: lw,
            bands: luma,
        };
        out.chroma = Layer {
            left: (lx0 / 2) as i64,
            top: (ly0 / 2) as i64,
            width: cw,
            bands: chroma,
        };
        Ok(out)
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
        let size = (frame.width as usize, frame.height as usize);
        let at = (x as i64, y as i64);
        for (k, plane) in frame.planes.iter_mut().enumerate().take(3) {
            match self.format {
                PixelFormat::Yuv420p8 => {
                    let stride = plane.stride;
                    self.paint(k, samples_mut::<u8>(plane)?, stride, size, at);
                }
                PixelFormat::Yuv420p10 => {
                    let stride = plane.stride / 2;
                    self.paint(k, samples_mut::<u16>(plane)?, stride, size, at);
                }
            }
        }
        Ok(())
    }

    fn paint<S: Sample>(
        &self,
        k: usize,
        dst: &mut [S],
        stride: usize,
        (fw, fh): (usize, usize),
        (x, y): (i64, i64),
    ) {
        let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
        let (cx, cy) = (x.div_euclid(2), y.div_euclid(2));
        let mask = MASK as u32;
        match k {
            0 => self
                .luma
                .draw(dst, stride, fw, fh, x, y, |c| (c >> BITS, c & mask)),
            1 => self.chroma.draw(dst, stride, cw, ch, cx, cy, |c| {
                ((c >> (2 * BITS)) as u32, ((c >> BITS) & MASK) as u32)
            }),
            _ => self.chroma.draw(dst, stride, cw, ch, cx, cy, |c| {
                ((c >> (2 * BITS)) as u32, (c & MASK) as u32)
            }),
        }
    }
}
