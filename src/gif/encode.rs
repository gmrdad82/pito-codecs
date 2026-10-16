use super::lzw::LzwEncoder;
use super::quant::{ALPHA_CUT, Dither, Histogram, Palette};
use super::{Error, Result};
use crate::media::Rgba;
use rayon::prelude::*;
use std::cell::RefCell;
use std::collections::VecDeque;

pub const TOLERANCE: u8 = 12;
pub const BEST_TOLERANCE: u8 = 4;
pub const REDRAW_PERCENT: u8 = 20;

const BLOCK: usize = 8;
const DRIFT: i32 = 2;
const SETTLE: u8 = 4;
const ERROR_LIMIT: i32 = 16;
const NEAREST_BITS: u32 = 7;
const UNSET: u16 = u16::MAX;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Global,
    PerFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodeOptions {
    pub colors: usize,
    pub dither: Dither,
    pub palette: PaletteMode,
    pub loop_count: Option<u16>,
    pub diff: bool,
    pub tolerance: u8,
    pub redraw_percent: u8,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            colors: 256,
            dither: Dither::Sierra24A,
            palette: PaletteMode::Global,
            loop_count: Some(0),
            diff: true,
            tolerance: TOLERANCE,
            redraw_percent: REDRAW_PERCENT,
        }
    }
}

pub fn delay_cs(frame: u64, fps_num: u32, fps_den: u32) -> u16 {
    if fps_num == 0 || fps_den == 0 {
        return 0;
    }
    let (num, den) = (u128::from(fps_num), u128::from(fps_den));
    let at = |n: u128| (n * 100 * den * 2 + num) / (num * 2);
    let n = u128::from(frame);
    (at(n + 1) - at(n)).min(u128::from(u16::MAX)) as u16
}

struct Record {
    rect: (usize, usize, usize, usize),
    idx: Vec<u8>,
    palette: Option<Palette>,
    transparent: u8,
    disposal: u8,
    delay: u16,
    body: Vec<u8>,
}

struct Look {
    palette: Palette,
    colors: [u32; 256],
}

impl Look {
    fn new(palette: Palette) -> Look {
        let colors = packed_colors(&palette);
        Look { palette, colors }
    }
}

struct Nearest {
    cells: Vec<u16>,
}

impl Nearest {
    fn new() -> Nearest {
        Nearest {
            cells: vec![UNSET; 1 << (3 * NEAREST_BITS)],
        }
    }

    fn reset(&mut self) {
        self.cells.fill(UNSET);
    }

    fn find(&mut self, palette: &[[u8; 3]], c: [i32; 3]) -> u8 {
        let shift = 8 - NEAREST_BITS;
        let bin = ((c[0] as usize >> shift) << (2 * NEAREST_BITS))
            | ((c[1] as usize >> shift) << NEAREST_BITS)
            | (c[2] as usize >> shift);
        let found = self.cells[bin];
        if found != UNSET {
            return found as u8;
        }
        let mut best = 0;
        let mut distance = i32::MAX;
        for (i, p) in palette.iter().enumerate() {
            let (dr, dg, db) = (p[0] as i32 - c[0], p[1] as i32 - c[1], p[2] as i32 - c[2]);
            let d = dr * dr + dg * dg + db * db;
            if d < distance {
                (best, distance) = (i, d);
            }
        }
        self.cells[bin] = best as u16;
        best as u8
    }
}

struct Area {
    rect: (usize, usize, usize, usize),
    whole: bool,
}

pub struct Encoder {
    width: usize,
    height: usize,
    opts: EncodeOptions,
    global: Option<Look>,
    out: Vec<u8>,
    started: bool,
    queue: VecDeque<(Rgba, u16, bool)>,
    frames: u64,
    prev_alpha: bool,
    reference: Vec<u8>,
    changed: Vec<bool>,
    distances: Vec<u8>,
    canvas: Vec<u32>,
    hot: Vec<bool>,
    near: Vec<bool>,
    drift: Vec<[i32; 3]>,
    errors: Vec<[i32; 3]>,
    nearest: Nearest,
    records: Vec<Record>,
    spare: Vec<Vec<u8>>,
    hist: Option<Histogram>,
}

thread_local! {
    static LZW: RefCell<LzwEncoder> = RefCell::new(LzwEncoder::default());
}

fn batch_size() -> usize {
    rayon::current_num_threads().clamp(8, 32)
}

fn bits_for(entries: usize) -> u8 {
    let mut b = 1;
    while (1usize << b) < entries {
        b += 1;
    }
    b
}

fn check_size(width: u32, height: u32) -> Result<()> {
    if !(1..=65535).contains(&width) || !(1..=65535).contains(&height) {
        return Err(Error::Invalid("GIF sides are 1 to 65535"));
    }
    Ok(())
}

fn check_frame(frame: &Rgba, width: usize, height: usize) -> Result<()> {
    if frame.width as usize != width || frame.height as usize != height {
        return Err(Error::Invalid("every frame has the canvas size"));
    }
    if frame.data.len() < width * height * 4 {
        return Err(Error::Invalid("frame data shorter than its size"));
    }
    Ok(())
}

fn has_alpha(frame: &Rgba) -> bool {
    frame
        .data
        .par_chunks(1 << 16)
        .any(|c| c.chunks_exact(4).any(|p| p[3] < ALPHA_CUT))
}

fn packed_colors(p: &Palette) -> [u32; 256] {
    let mut t = [0u32; 256];
    for (d, c) in t.iter_mut().zip(p.colors()) {
        *d = 0xFF00_0000 | c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16;
    }
    t
}

fn global_palette(frames: &[&Rgba], colors: usize) -> Palette {
    Palette::from_frames(frames, colors.clamp(2, 256).min(255))
}

impl Encoder {
    pub fn new(width: u32, height: u32, opts: EncodeOptions) -> Result<Encoder> {
        check_size(width, height)?;
        let opts = EncodeOptions {
            colors: opts.colors.clamp(2, 256),
            ..opts
        };
        let (w, h) = (width as usize, height as usize);
        Ok(Encoder {
            width: w,
            height: h,
            opts,
            global: None,
            out: Vec::new(),
            started: false,
            queue: VecDeque::new(),
            frames: 0,
            prev_alpha: false,
            reference: vec![0; w * h * 4],
            changed: vec![false; w * h],
            distances: vec![0; w * h],
            canvas: vec![0; w * h],
            hot: Vec::new(),
            near: Vec::new(),
            drift: Vec::new(),
            errors: Vec::new(),
            nearest: Nearest::new(),
            records: Vec::new(),
            spare: Vec::new(),
            hist: None,
        })
    }

    pub fn with_palette(
        width: u32,
        height: u32,
        opts: EncodeOptions,
        palette: Palette,
    ) -> Result<Encoder> {
        let mut e = Encoder::new(width, height, opts)?;
        let palette = Palette::new(palette.colors()[..palette.len().min(255)].to_vec())?;
        e.global = Some(Look::new(palette));
        Ok(e)
    }

    fn max_colors(&self) -> usize {
        self.opts.colors.min(255)
    }

    fn needs_palette(&self) -> bool {
        self.opts.palette == PaletteMode::Global && self.global.is_none()
    }

    pub fn push(&mut self, frame: Rgba, delay_cs: u16) -> Result<()> {
        check_frame(&frame, self.width, self.height)?;
        let alpha = has_alpha(&frame);
        self.queue.push_back((frame, delay_cs, alpha));
        if self.needs_palette() {
            if self.queue.len() < batch_size() {
                return Ok(());
            }
            self.palette_from_queue();
        }
        while self.queue.len() >= 2 {
            self.step();
        }
        Ok(())
    }

    fn palette_from_queue(&mut self) {
        let refs: Vec<&Rgba> = self.queue.iter().map(|q| &q.0).collect();
        let palette = global_palette(&refs, self.max_colors());
        self.global = Some(Look::new(palette));
    }

    fn step(&mut self) {
        let Some((frame, delay, alpha)) = self.queue.pop_front() else {
            return;
        };
        let next_alpha = self.queue.front().is_some_and(|q| q.2);
        self.frame(&frame, delay, alpha, next_alpha);
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.needs_palette() && !self.queue.is_empty() {
            self.palette_from_queue();
        }
        while !self.queue.is_empty() {
            self.step();
        }
        self.close()
    }

    fn close(mut self) -> Vec<u8> {
        self.drain(true);
        if !self.started {
            self.header();
        }
        self.out.push(0x3B);
        self.out
    }

    fn frame(&mut self, src: &Rgba, delay: u16, alpha: bool, next_alpha: bool) {
        let (w, h) = (self.width, self.height);
        let forced = self.frames == 0 || alpha || next_alpha || self.prev_alpha || !self.opts.diff;
        self.frames += 1;
        self.prev_alpha = alpha;
        let area = if forced {
            Some(Area {
                rect: (0, 0, w, h),
                whole: true,
            })
        } else {
            self.compare(&src.data)
        };
        let Some(area) = area else {
            self.hold(delay);
            return;
        };
        let local = match self.opts.palette {
            PaletteMode::Global => None,
            PaletteMode::PerFrame => Some(self.local_palette(src, area.rect)),
        };
        let mut idx = self.spare.pop().unwrap_or_default();
        let Some(look) = local.as_ref().or(self.global.as_ref()) else {
            return;
        };
        let t = look.palette.len() as u8;
        if local.is_some() {
            self.nearest.reset();
        }
        let mut errors = std::mem::take(&mut self.errors);
        quantize(
            &Quant {
                look,
                dither: self.opts.dither,
                t,
                width: w,
                rect: area.rect,
                whole: area.whole,
            },
            &src.data,
            &self.changed,
            &mut self.reference,
            &mut self.canvas,
            &mut errors,
            &mut self.nearest,
            &mut idx,
        );
        self.errors = errors;
        if next_alpha {
            self.canvas.fill(0);
        }
        self.records.push(Record {
            rect: area.rect,
            idx,
            palette: local.map(|l| l.palette),
            transparent: t,
            disposal: if next_alpha { 2 } else { 1 },
            delay,
            body: Vec::new(),
        });
        if self.records.len() > batch_size() {
            self.drain(false);
        }
    }

    fn local_palette(&mut self, src: &Rgba, rect: (usize, usize, usize, usize)) -> Look {
        let mut h = self.hist.take().unwrap_or_default();
        h.clear();
        let (x, y, w, rows) = rect;
        for row in y..y + rows {
            let at = (row * self.width + x) * 4;
            h.add_bytes(&src.data[at..at + w * 4]);
        }
        let p = h.palette(self.max_colors());
        self.hist = Some(h);
        Look::new(p)
    }

    fn hold(&mut self, delay: u16) {
        if let Some(r) = self.records.last_mut()
            && let Some(d) = r.delay.checked_add(delay)
        {
            r.delay = d;
            return;
        }
        let (palette, t) = match &self.global {
            Some(g) => (None, g.palette.len() as u8),
            None => (Palette::new(vec![[0, 0, 0]]).ok(), 1),
        };
        let mut idx = self.spare.pop().unwrap_or_default();
        idx.clear();
        idx.push(t);
        self.records.push(Record {
            rect: (0, 0, 1, 1),
            idx,
            palette,
            transparent: t,
            disposal: 1,
            delay,
            body: Vec::new(),
        });
    }

    fn drain(&mut self, last: bool) {
        self.records
            .par_iter_mut()
            .filter(|r| r.body.is_empty())
            .for_each(encode_body);
        let keep = if last { None } else { self.records.pop() };
        let records = std::mem::take(&mut self.records);
        for r in &records {
            self.write(r);
        }
        self.spare.extend(records.into_iter().map(|r| r.idx));
        self.records.extend(keep);
    }

    fn header(&mut self) {
        self.started = true;
        let o = &mut self.out;
        o.extend_from_slice(b"GIF89a");
        o.extend_from_slice(&(self.width as u16).to_le_bytes());
        o.extend_from_slice(&(self.height as u16).to_le_bytes());
        match &self.global {
            Some(g) => {
                let p = &g.palette;
                let bits = bits_for(p.len() + 1);
                o.push(0x80 | 0x70 | (bits - 1));
                o.push(p.len() as u8);
                o.push(0);
                table(o, p, bits);
            }
            None => {
                o.push(0x70);
                o.push(0);
                o.push(0);
            }
        }
        if let Some(n) = self.opts.loop_count {
            o.extend_from_slice(&[0x21, 0xFF, 11]);
            o.extend_from_slice(b"NETSCAPE2.0");
            o.extend_from_slice(&[3, 1]);
            o.extend_from_slice(&n.to_le_bytes());
            o.push(0);
        }
    }

    fn write(&mut self, r: &Record) {
        if !self.started {
            self.header();
        }
        let o = &mut self.out;
        o.extend_from_slice(&[0x21, 0xF9, 4, (r.disposal << 2) | 1]);
        o.extend_from_slice(&r.delay.to_le_bytes());
        o.extend_from_slice(&[r.transparent, 0, 0x2C]);
        let (x, y, w, h) = r.rect;
        for v in [x, y, w, h] {
            o.extend_from_slice(&(v as u16).to_le_bytes());
        }
        match &r.palette {
            Some(p) => {
                let bits = bits_for(p.len() + 1);
                o.push(0x80 | (bits - 1));
                table(o, p, bits);
            }
            None => o.push(0),
        }
        o.extend_from_slice(&r.body);
    }

    fn compare(&mut self, src: &[u8]) -> Option<Area> {
        let (width, height) = (self.width, self.height);
        let columns = width.div_ceil(BLOCK);
        let rows = height.div_ceil(BLOCK);
        let tolerance = self.opts.tolerance;
        self.hot.clear();
        self.hot.resize(columns * rows, false);
        self.drift.clear();
        self.drift.resize(columns * rows, [0; 3]);
        self.distances
            .par_chunks_mut(width * BLOCK)
            .zip(self.hot.par_chunks_mut(columns))
            .zip(self.drift.par_chunks_mut(columns))
            .zip(
                src.par_chunks(width * BLOCK * 4)
                    .zip(self.reference.par_chunks(width * BLOCK * 4)),
            )
            .for_each(|(((dist, hot), drift), (s, r))| {
                measure(dist, hot, drift, s, r, width, tolerance)
            });
        let drifting: usize = (0..rows)
            .map(|block_row| {
                let tall = BLOCK.min(height - block_row * BLOCK) as i32;
                (0..columns)
                    .filter(|&block_column| {
                        let wide = BLOCK.min(width - block_column * BLOCK) as i32;
                        let block = block_row * columns + block_column;
                        !self.hot[block]
                            && self.drift[block]
                                .iter()
                                .any(|sum| sum.abs() > DRIFT * tall * wide)
                    })
                    .count()
            })
            .sum();
        let whole = drifting * 100 > self.opts.redraw_percent as usize * columns * rows;
        self.near.clear();
        self.near.resize(columns * rows, false);
        for block_row in 0..rows {
            for block_column in 0..columns {
                if !self.hot[block_row * columns + block_column] {
                    continue;
                }
                for near_row in block_row.saturating_sub(1)..(block_row + 2).min(rows) {
                    for near_column in
                        block_column.saturating_sub(1)..(block_column + 2).min(columns)
                    {
                        self.near[near_row * columns + near_column] = true;
                    }
                }
            }
        }
        let settle = tolerance / SETTLE;
        let near = &self.near;
        let bounds = self
            .changed
            .par_chunks_mut(width * BLOCK)
            .zip(self.distances.par_chunks(width * BLOCK))
            .enumerate()
            .map(|(block_row, (changed, dist))| {
                let near = &near[block_row * columns..(block_row + 1) * columns];
                let (mut left, mut right, mut top, mut bottom) = (usize::MAX, 0, usize::MAX, 0);
                for (row, (crow, drow)) in changed
                    .chunks_exact_mut(width)
                    .zip(dist.chunks_exact(width))
                    .enumerate()
                {
                    let mut any = false;
                    for (column, (c, &d)) in crow.iter_mut().zip(drow).enumerate() {
                        let limit = if *c || near[column / BLOCK] {
                            settle
                        } else {
                            tolerance
                        };
                        let moved = d > limit;
                        *c = moved;
                        if moved {
                            any = true;
                            left = left.min(column);
                            right = right.max(column);
                        }
                    }
                    if any {
                        let y = block_row * BLOCK + row;
                        top = top.min(y);
                        bottom = y;
                    }
                }
                (left, right, top, bottom)
            })
            .reduce(
                || (usize::MAX, 0, usize::MAX, 0),
                |a, b| (a.0.min(b.0), a.1.max(b.1), a.2.min(b.2), a.3.max(b.3)),
            );
        if whole {
            return Some(Area {
                rect: (0, 0, width, height),
                whole: true,
            });
        }
        let (left, right, top, bottom) = bounds;
        (left <= right && top <= bottom).then_some(Area {
            rect: (left, top, right - left + 1, bottom - top + 1),
            whole: false,
        })
    }
}

crate::media::isa::dispatch! {
    fn measure(
        dist: &mut [u8],
        hot: &mut [bool],
        drift: &mut [[i32; 3]],
        src: &[u8],
        reference: &[u8],
        width: usize,
        tolerance: u8,
    ) {
        for ((drow, srow), rrow) in dist
            .chunks_exact_mut(width)
            .zip(src.chunks_exact(width * 4))
            .zip(reference.chunks_exact(width * 4))
        {
            for (((d, s), r), (h, sum)) in drow
                .chunks_mut(BLOCK)
                .zip(srow.chunks(BLOCK * 4))
                .zip(rrow.chunks(BLOCK * 4))
                .zip(hot.iter_mut().zip(drift.iter_mut()))
            {
                let mut far = 0u8;
                let mut acc = [0i32; 3];
                for ((d, p), q) in d.iter_mut().zip(s.chunks_exact(4)).zip(r.chunks_exact(4)) {
                    let v = p[0].abs_diff(q[0]).max(p[1].abs_diff(q[1])).max(p[2].abs_diff(q[2]));
                    *d = v;
                    far = far.max(v);
                    for c in 0..3 {
                        acc[c] += p[c] as i32 - q[c] as i32;
                    }
                }
                if far > tolerance {
                    *h = true;
                }
                for c in 0..3 {
                    sum[c] += acc[c];
                }
            }
        }
    }
}

struct Quant<'a> {
    look: &'a Look,
    dither: Dither,
    t: u8,
    width: usize,
    rect: (usize, usize, usize, usize),
    whole: bool,
}

fn quantize(
    q: &Quant,
    src: &[u8],
    changed: &[bool],
    reference: &mut [u8],
    canvas: &mut [u32],
    errors: &mut Vec<[i32; 3]>,
    nearest: &mut Nearest,
    idx: &mut Vec<u8>,
) {
    let (x, y, w, h) = q.rect;
    let palette = q.look.palette.colors();
    let colors = &q.look.colors;
    let diffuse = matches!(q.dither, Dither::FloydSteinberg | Dither::Sierra24A);
    let amp = (96.0 / (palette.len() as f32).cbrt()).round() as i32;
    idx.clear();
    idx.reserve(w * h);
    errors.clear();
    errors.resize((w + 2) * 2, [0; 3]);
    let (mut cur, mut nxt) = errors.split_at_mut(w + 2);
    for row in y..y + h {
        nxt.fill([0; 3]);
        let bayer = &BAYER[row & 7];
        for (i, column) in (x..x + w).enumerate() {
            let at = row * q.width + column;
            let p = &src[at * 4..at * 4 + 4];
            if p[3] < ALPHA_CUT {
                reference[at * 4..at * 4 + 4].copy_from_slice(p);
                canvas[at] = 0;
                idx.push(q.t);
                continue;
            }
            let offset = match q.dither {
                Dither::Ordered => [((bayer[column & 7] as i32 * 2 - 63) * amp) >> 7; 3],
                Dither::None => [0; 3],
                _ => cur[i + 1],
            };
            let wanted = [
                (p[0] as i32 + offset[0]).clamp(0, 255),
                (p[1] as i32 + offset[1]).clamp(0, 255),
                (p[2] as i32 + offset[2]).clamp(0, 255),
            ];
            let shown = if q.whole || changed[at] {
                reference[at * 4..at * 4 + 4].copy_from_slice(p);
                let index = nearest.find(palette, wanted);
                canvas[at] = colors[index as usize];
                idx.push(index);
                palette[index as usize].map(i32::from)
            } else {
                idx.push(q.t);
                let c = canvas[at];
                if c == 0 {
                    wanted
                } else {
                    [
                        (c & 0xFF) as i32,
                        (c >> 8 & 0xFF) as i32,
                        (c >> 16 & 0xFF) as i32,
                    ]
                }
            };
            if !diffuse {
                continue;
            }
            for c in 0..3 {
                let e = (wanted[c] - shown[c]).clamp(-ERROR_LIMIT, ERROR_LIMIT);
                if q.dither == Dither::Sierra24A {
                    cur[i + 2][c] += e / 2;
                    nxt[i][c] += e / 4;
                    nxt[i + 1][c] += e / 4;
                } else {
                    cur[i + 2][c] += e * 7 / 16;
                    nxt[i][c] += e * 3 / 16;
                    nxt[i + 1][c] += e * 5 / 16;
                    nxt[i + 2][c] += e / 16;
                }
            }
        }
        std::mem::swap(&mut cur, &mut nxt);
    }
}

fn table(o: &mut Vec<u8>, p: &Palette, bits: u8) {
    for c in p.colors() {
        o.extend_from_slice(c);
    }
    o.resize(o.len() + ((1usize << bits) - p.len()) * 3, 0);
}

fn encode_body(r: &mut Record) {
    let (_, _, w, h) = r.rect;
    if w == 0 || h == 0 {
        return;
    }
    let entries = r
        .palette
        .as_ref()
        .map_or(0, |p| p.len())
        .max(r.transparent as usize)
        + 1;
    let min = bits_for(entries).max(2);
    let mut body = std::mem::take(&mut r.body);
    body.clear();
    LZW.with_borrow_mut(|l| l.encode(min, r.idx[..w * h].chunks_exact(w), &mut body));
    r.body = body;
}

pub fn encode(frames: &[Rgba], delays_cs: &[u16], opts: EncodeOptions) -> Result<Vec<u8>> {
    let Some(first) = frames.first() else {
        return Err(Error::Invalid("no frames to encode"));
    };
    check_size(first.width, first.height)?;
    for f in frames {
        check_frame(f, first.width as usize, first.height as usize)?;
    }
    let mut e = match opts.palette {
        PaletteMode::Global => {
            let refs: Vec<&Rgba> = frames.iter().collect();
            let palette = global_palette(&refs, opts.colors);
            Encoder::with_palette(first.width, first.height, opts, palette)?
        }
        PaletteMode::PerFrame => Encoder::new(first.width, first.height, opts)?,
    };
    let alphas: Vec<bool> = frames.par_iter().map(has_alpha).collect();
    for (i, f) in frames.iter().enumerate() {
        let delay = delays_cs.get(i).copied().unwrap_or(10);
        let next_alpha = alphas.get(i + 1).copied().unwrap_or(false);
        e.frame(f, delay, alphas[i], next_alpha);
    }
    Ok(e.close())
}
