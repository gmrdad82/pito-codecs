use crate::filter::Rgba;
use rayon::prelude::*;
use std::sync::LazyLock;

const LEVELS: usize = 64;
const CELLS: usize = LEVELS * LEVELS * LEVELS;
const ALPHA_CUT: u8 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    None,
    Ordered,
    FloydSteinberg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    colors: Vec<[u8; 3]>,
}

impl Palette {
    pub fn new(colors: Vec<[u8; 3]>) -> Self {
        assert!(
            !colors.is_empty() && colors.len() <= 256,
            "a palette holds 1 to 256 colours"
        );
        Palette { colors }
    }

    pub fn colors(&self) -> &[[u8; 3]] {
        &self.colors
    }

    pub fn len(&self) -> usize {
        self.colors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }

    pub fn from_frames(frames: &[&Rgba], colors: usize) -> Palette {
        let mut h = Histogram::new();
        h.add_frames(frames);
        h.palette(colors)
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn linear_to_oklab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn oklab_to_srgb(c: [f32; 3]) -> [u8; 3] {
    let l = c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2];
    let m = c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2];
    let s = c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    let r = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
    let g = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s;
    let b = -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s;
    [r, g, b].map(|v| (to_srgb(v) * 255.0).round() as u8)
}

fn srgb_f_to_oklab(c: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = c.map(|v| to_linear(v / 255.0));
    linear_to_oklab(r, g, b)
}

#[inline(always)]
pub(crate) fn cell(r: u8, g: u8, b: u8) -> usize {
    ((r as usize >> 2) << 12) | ((g as usize >> 2) << 6) | (b as usize >> 2)
}

pub struct Histogram {
    bins: Vec<[u32; 4]>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self::new()
    }
}

impl Histogram {
    pub fn new() -> Self {
        Histogram {
            bins: vec![[0; 4]; CELLS],
        }
    }

    pub fn clear(&mut self) {
        self.bins.fill([0; 4]);
    }

    pub fn add(&mut self, frame: &Rgba) {
        self.add_bytes(&frame.data);
    }

    pub fn add_frames(&mut self, frames: &[&Rgba]) {
        let rows: Vec<&[u8]> = frames
            .iter()
            .flat_map(|f| f.data.chunks(f.stride().max(4) * 64))
            .collect();
        let parts = rayon::current_num_threads().clamp(1, 8);
        let per = rows.len().div_ceil(parts).max(1);
        let partial: Vec<Histogram> = rows
            .par_chunks(per)
            .map(|chunk| {
                let mut h = Histogram::new();
                for r in chunk {
                    h.add_bytes(r);
                }
                h
            })
            .collect();
        self.merge(&partial);
    }

    fn add_bytes(&mut self, data: &[u8]) {
        let bins = &mut self.bins[..];
        for p in data.chunks_exact(4) {
            if p[3] < ALPHA_CUT {
                continue;
            }
            let bin = &mut bins[cell(p[0], p[1], p[2])];
            bin[0] += 1;
            bin[1] += (p[0] & 3) as u32;
            bin[2] += (p[1] & 3) as u32;
            bin[3] += (p[2] & 3) as u32;
        }
    }

    fn merge(&mut self, parts: &[Histogram]) {
        if parts.is_empty() {
            return;
        }
        self.bins
            .par_chunks_mut(4096)
            .enumerate()
            .for_each(|(i, dst)| {
                for part in parts {
                    let src = &part.bins[i * 4096..i * 4096 + dst.len()];
                    for (d, s) in dst.iter_mut().zip(src) {
                        for k in 0..4 {
                            d[k] = d[k].saturating_add(s[k]);
                        }
                    }
                }
            });
    }

    pub fn palette(&self, colors: usize) -> Palette {
        let colors = colors.clamp(1, 256);
        let mut points: Vec<Point> = self
            .bins
            .iter()
            .enumerate()
            .filter(|(_, b)| b[0] > 0)
            .map(|(i, b)| {
                let n = b[0] as f32;
                let base = [(i >> 12) & 63, (i >> 6) & 63, i & 63].map(|v| (v << 2) as f32);
                let rgb = [
                    base[0] + b[1] as f32 / n,
                    base[1] + b[2] as f32 / n,
                    base[2] + b[3] as f32 / n,
                ];
                Point {
                    lab: srgb_f_to_oklab(rgb),
                    rgb,
                    weight: n,
                }
            })
            .collect();
        if points.is_empty() {
            return Palette::new(vec![[0, 0, 0]]);
        }
        if points.len() <= colors {
            return Palette::new(
                points
                    .iter()
                    .map(|p| p.rgb.map(|v| v.round() as u8))
                    .collect(),
            );
        }
        let centers = median_cut(&mut points, colors);
        let centers = kmeans(&points, centers, 3);
        Palette::new(centers.into_iter().map(oklab_to_srgb).collect())
    }
}

#[derive(Clone, Copy)]
struct Point {
    lab: [f32; 3],
    rgb: [f32; 3],
    weight: f32,
}

struct Cut {
    start: usize,
    end: usize,
    score: f64,
    axis: usize,
}

fn measure(points: &[Point], start: usize, end: usize) -> Cut {
    let pts = &points[start..end];
    let mut w = 0.0f64;
    let mut s = [0.0f64; 3];
    let mut q = [0.0f64; 3];
    for p in pts {
        let pw = p.weight as f64;
        w += pw;
        for k in 0..3 {
            let v = p.lab[k] as f64;
            s[k] += pw * v;
            q[k] += pw * v * v;
        }
    }
    let var: [f64; 3] = std::array::from_fn(|k| (q[k] - s[k] * s[k] / w).max(0.0));
    let axis = (0..3)
        .max_by(|&a, &b| var[a].total_cmp(&var[b]))
        .unwrap_or(0);
    let score = if end - start < 2 {
        -1.0
    } else {
        var.iter().sum()
    };
    Cut {
        start,
        end,
        score,
        axis,
    }
}

fn median_cut(points: &mut [Point], colors: usize) -> Vec<[f32; 3]> {
    let mut cuts = vec![measure(points, 0, points.len())];
    while cuts.len() < colors {
        let Some((i, _)) = cuts
            .iter()
            .enumerate()
            .filter(|(_, c)| c.score > 0.0)
            .max_by(|a, b| a.1.score.total_cmp(&b.1.score))
        else {
            break;
        };
        let c = cuts.swap_remove(i);
        let slice = &mut points[c.start..c.end];
        let axis = c.axis;
        slice.sort_unstable_by(|a, b| a.lab[axis].total_cmp(&b.lab[axis]));
        let total: f32 = slice.iter().map(|p| p.weight).sum();
        let mut acc = 0.0;
        let mut mid = 1;
        for (k, p) in slice.iter().enumerate() {
            acc += p.weight;
            if acc >= total / 2.0 {
                mid = (k + 1).clamp(1, slice.len() - 1);
                break;
            }
        }
        cuts.push(measure(points, c.start, c.start + mid));
        cuts.push(measure(points, c.start + mid, c.end));
    }
    cuts.iter()
        .map(|c| {
            let mut w = 0.0f64;
            let mut s = [0.0f64; 3];
            for p in &points[c.start..c.end] {
                w += p.weight as f64;
                for (acc, v) in s.iter_mut().zip(p.lab) {
                    *acc += p.weight as f64 * v as f64;
                }
            }
            s.map(|v| (v / w) as f32)
        })
        .collect()
}

struct Soa {
    l: Vec<f32>,
    a: Vec<f32>,
    b: Vec<f32>,
}

impl Soa {
    fn new(centers: &[[f32; 3]]) -> Soa {
        let n = centers.len().next_multiple_of(8);
        let mut s = Soa {
            l: vec![1e9; n],
            a: vec![1e9; n],
            b: vec![1e9; n],
        };
        for (i, c) in centers.iter().enumerate() {
            s.l[i] = c[0];
            s.a[i] = c[1];
            s.b[i] = c[2];
        }
        s
    }

    #[inline(always)]
    fn nearest(&self, p: [f32; 3]) -> usize {
        let mut best = f32::MAX;
        let mut at = 0;
        for (j, ((l, a), b)) in self
            .l
            .chunks_exact(8)
            .zip(self.a.chunks_exact(8))
            .zip(self.b.chunks_exact(8))
            .enumerate()
        {
            let mut d = [0.0f32; 8];
            for k in 0..8 {
                let x = l[k] - p[0];
                let y = a[k] - p[1];
                let z = b[k] - p[2];
                d[k] = x * x + y * y + z * z;
            }
            let m = d.iter().copied().fold(f32::MAX, f32::min);
            if m < best {
                best = m;
                at = j * 8 + d.iter().position(|&v| v == m).unwrap_or(0);
            }
        }
        at
    }
}

fn kmeans(points: &[Point], mut centers: Vec<[f32; 3]>, rounds: usize) -> Vec<[f32; 3]> {
    for _ in 0..rounds {
        let soa = Soa::new(&centers);
        let k = centers.len();
        let parts: Vec<Vec<[f64; 4]>> = points
            .par_chunks(4096)
            .map(|chunk| {
                let mut acc = vec![[0.0f64; 4]; k];
                for p in chunk {
                    let i = nearest_lab(&soa, p.lab);
                    let w = p.weight as f64;
                    acc[i][0] += w;
                    for c in 0..3 {
                        acc[i][c + 1] += w * p.lab[c] as f64;
                    }
                }
                acc
            })
            .collect();
        let mut sums = vec![[0.0f64; 4]; k];
        for part in &parts {
            for (x, y) in sums.iter_mut().zip(part) {
                for c in 0..4 {
                    x[c] += y[c];
                }
            }
        }
        for (c, s) in centers.iter_mut().zip(&sums) {
            if s[0] > 0.0 {
                *c = [
                    (s[1] / s[0]) as f32,
                    (s[2] / s[0]) as f32,
                    (s[3] / s[0]) as f32,
                ];
            }
        }
    }
    centers
}

crate::gif::isa::dispatch! {
    fn nearest_lab(soa: &Soa, p: [f32; 3]) -> usize {
        soa.nearest(p)
    }
}

static CELL_LAB: LazyLock<Vec<[f32; 3]>> = LazyLock::new(|| {
    let level: Vec<f32> = (0..LEVELS)
        .map(|v| to_linear(((v << 2) as f32 + 1.5) / 255.0))
        .collect();
    (0..CELLS)
        .map(|i| linear_to_oklab(level[i >> 12], level[(i >> 6) & 63], level[i & 63]))
        .collect()
});

pub(crate) struct Lut {
    pub cells: Vec<u8>,
}

impl Lut {
    pub fn new(palette: &Palette) -> Lut {
        let centers: Vec<[f32; 3]> = palette
            .colors
            .iter()
            .map(|c| c.map(|v| v as f32))
            .map(srgb_f_to_oklab)
            .collect();
        let mut cells = vec![0u8; CELLS];
        cells
            .par_chunks_mut(SLAB)
            .enumerate()
            .for_each(|(br, out)| fill_slab(&centers, br, out));
        Lut { cells }
    }
}

const SLAB: usize = LEVELS * LEVELS * 4;

crate::gif::isa::dispatch! {
    fn fill_slab(centers: &[[f32; 3]], br: usize, out: &mut [u8]) {
        let labs = &CELL_LAB[br * SLAB..(br + 1) * SLAB];
        let mut candidates: Vec<u8> = Vec::with_capacity(256);
        let mut near = vec![0f32; centers.len()];
        for bg in 0..16 {
            for bb in 0..16 {
                let block = |r: usize, g: usize, b: usize| (r << 12) | ((bg * 4 + g) << 6) | (bb * 4 + b);
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                for r in 0..4 {
                    for g in 0..4 {
                        for b in 0..4 {
                            let p = labs[block(r, g, b)];
                            for k in 0..3 {
                                lo[k] = lo[k].min(p[k]);
                                hi[k] = hi[k].max(p[k]);
                            }
                        }
                    }
                }
                let mut bound = f32::MAX;
                for (c, n) in centers.iter().zip(near.iter_mut()) {
                    let mut low = 0.0;
                    let mut high = 0.0;
                    for k in 0..3 {
                        let d = (lo[k] - c[k]).max(c[k] - hi[k]).max(0.0);
                        low += d * d;
                        let f = (c[k] - lo[k]).abs().max((c[k] - hi[k]).abs());
                        high += f * f;
                    }
                    *n = low;
                    bound = bound.min(high);
                }
                candidates.clear();
                for (i, &n) in near.iter().enumerate() {
                    if n <= bound {
                        candidates.push(i as u8);
                    }
                }
                for r in 0..4 {
                    for g in 0..4 {
                        for b in 0..4 {
                            let at = block(r, g, b);
                            let p = labs[at];
                            let mut best = f32::MAX;
                            let mut pick = 0;
                            for &i in &candidates {
                                let c = centers[i as usize];
                                let x = c[0] - p[0];
                                let y = c[1] - p[1];
                                let z = c[2] - p[2];
                                let d = x * x + y * y + z * z;
                                if d < best {
                                    best = d;
                                    pick = i;
                                }
                            }
                            out[at] = pick;
                        }
                    }
                }
            }
        }
    }
}

const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

pub(crate) struct Mapper<'a> {
    pub lut: &'a Lut,
    pub palette: &'a [[u8; 3]],
    pub dither: Dither,
    pub transparent: u8,
}

impl Mapper<'_> {
    pub fn map(&self, src: &Rgba, out: &mut [u8], err: &mut Vec<i16>) {
        let w = src.width as usize;
        if w == 0 || src.height == 0 {
            return;
        }
        match self.dither {
            Dither::FloydSteinberg => self.map_fs(src, out, err),
            _ => {
                let band = 16;
                out.par_chunks_mut(w * band)
                    .zip(src.data.par_chunks(w * 4 * band))
                    .enumerate()
                    .for_each(|(i, (o, s))| self.map_rows(s, o, w, i * band));
            }
        }
    }

    fn map_rows(&self, src: &[u8], out: &mut [u8], w: usize, y0: usize) {
        let t = self.transparent;
        let cells = &self.lut.cells[..];
        if self.dither == Dither::None {
            for (o, p) in out.iter_mut().zip(src.chunks_exact(4)) {
                *o = if p[3] < ALPHA_CUT {
                    t
                } else {
                    cells[cell(p[0], p[1], p[2])]
                };
            }
            return;
        }
        let amp = (96.0 / (self.palette.len() as f32).cbrt()).round() as i32;
        for (y, (orow, srow)) in out.chunks_mut(w).zip(src.chunks(w * 4)).enumerate() {
            let brow = &BAYER[(y0 + y) & 7];
            for (x, (o, p)) in orow.iter_mut().zip(srow.chunks_exact(4)).enumerate() {
                if p[3] < ALPHA_CUT {
                    *o = t;
                    continue;
                }
                let d = ((brow[x & 7] as i32 * 2 - 63) * amp) >> 7;
                let r = (p[0] as i32 + d).clamp(0, 255) as u8;
                let g = (p[1] as i32 + d).clamp(0, 255) as u8;
                let b = (p[2] as i32 + d).clamp(0, 255) as u8;
                *o = cells[cell(r, g, b)];
            }
        }
    }

    fn map_fs(&self, src: &Rgba, out: &mut [u8], err: &mut Vec<i16>) {
        let w = src.width as usize;
        let h = src.height as usize;
        let n = (w + 2) * 3;
        err.clear();
        err.resize(n * 2, 0);
        let (mut cur, mut nxt) = err.split_at_mut(n);
        let cells = &self.lut.cells[..];
        let pal = self.palette;
        for y in 0..h {
            nxt.fill(0);
            let srow = &src.data[y * w * 4..(y + 1) * w * 4];
            let orow = &mut out[y * w..(y + 1) * w];
            let rev = y & 1 == 1;
            for i in 0..w {
                let x = if rev { w - 1 - i } else { i };
                let p = &srow[x * 4..x * 4 + 4];
                if p[3] < ALPHA_CUT {
                    orow[x] = self.transparent;
                    continue;
                }
                let e = (x + 1) * 3;
                let want = [
                    (p[0] as i32 + cur[e] as i32).clamp(0, 255),
                    (p[1] as i32 + cur[e + 1] as i32).clamp(0, 255),
                    (p[2] as i32 + cur[e + 2] as i32).clamp(0, 255),
                ];
                let idx = cells[cell(want[0] as u8, want[1] as u8, want[2] as u8)];
                orow[x] = idx;
                let got = pal[idx as usize];
                let (fwd, back) = if rev { (e - 3, e + 3) } else { (e + 3, e - 3) };
                for c in 0..3 {
                    let d = want[c] - got[c] as i32;
                    cur[fwd + c] = (cur[fwd + c] as i32 + d * 7 / 16).clamp(-255, 255) as i16;
                    nxt[back + c] += (d * 3 / 16) as i16;
                    nxt[e + c] += (d * 5 / 16) as i16;
                    nxt[fwd + c] += (d / 16) as i16;
                }
            }
            std::mem::swap(&mut cur, &mut nxt);
        }
    }
}
