use super::lzw::LzwEncoder;
use super::quant::{Dither, Histogram, Lut, Mapper, Palette};
use crate::filter::Rgba;
use rayon::prelude::*;
use std::cell::RefCell;

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
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            colors: 256,
            dither: Dither::FloydSteinberg,
            palette: PaletteMode::Global,
            loop_count: Some(0),
            diff: true,
        }
    }
}

struct Mapped {
    idx: Vec<u8>,
    palette: Option<Palette>,
    alpha: bool,
    delay: u16,
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

pub struct Encoder {
    width: usize,
    height: usize,
    opts: EncodeOptions,
    global: Option<(Palette, Lut)>,
    out: Vec<u8>,
    started: bool,
    batch: Vec<(Rgba, u16)>,
    pending: Option<Mapped>,
    carry: Option<Rgba>,
    prev: Option<Rgba>,
    canvas: Vec<u32>,
    held: Option<Record>,
    spare: Vec<Vec<u8>>,
}

thread_local! {
    static LZW: RefCell<LzwEncoder> = RefCell::new(LzwEncoder::default());
    static ERR: RefCell<Vec<i16>> = const { RefCell::new(Vec::new()) };
    static HIST: RefCell<Option<Histogram>> = const { RefCell::new(None) };
}

fn keep(slot: &mut Option<Rgba>, from: &Rgba) {
    match slot {
        Some(r) => {
            r.width = from.width;
            r.height = from.height;
            r.data.clear();
            r.data.extend_from_slice(&from.data);
        }
        None => *slot = Some(from.clone()),
    }
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

fn packed_colors(p: &Palette) -> [u32; 256] {
    let mut t = [0u32; 256];
    for (d, c) in t.iter_mut().zip(p.colors()) {
        *d = 0xFF00_0000 | c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16;
    }
    t
}

impl Encoder {
    pub fn new(width: u32, height: u32, opts: EncodeOptions) -> Encoder {
        assert!(
            (1..=65535).contains(&width) && (1..=65535).contains(&height),
            "GIF sides are 1 to 65535"
        );
        let opts = EncodeOptions {
            colors: opts.colors.clamp(2, 256),
            ..opts
        };
        Encoder {
            width: width as usize,
            height: height as usize,
            opts,
            global: None,
            out: Vec::new(),
            started: false,
            batch: Vec::new(),
            pending: None,
            carry: None,
            prev: None,
            canvas: vec![0; width as usize * height as usize],
            held: None,
            spare: Vec::new(),
        }
    }

    pub fn with_palette(width: u32, height: u32, opts: EncodeOptions, palette: Palette) -> Encoder {
        let mut e = Encoder::new(width, height, opts);
        let palette = Palette::new(palette.colors()[..palette.len().min(255)].to_vec());
        let lut = Lut::new(&palette);
        e.global = Some((palette, lut));
        e
    }

    fn max_colors(&self) -> usize {
        self.opts.colors.min(255)
    }

    pub fn push(&mut self, frame: Rgba, delay_cs: u16) {
        assert!(
            frame.width as usize == self.width && frame.height as usize == self.height,
            "every frame has the canvas size"
        );
        self.batch.push((frame, delay_cs));
        if self.batch.len() >= batch_size() {
            self.flush_batch(false);
        }
    }

    fn flush_batch(&mut self, last: bool) {
        let mut batch = std::mem::take(&mut self.batch);
        let refs: Vec<(&Rgba, u16)> = batch.iter().map(|(f, d)| (f, *d)).collect();
        self.process(&refs, last);
        drop(refs);
        batch.clear();
        self.batch = batch;
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.flush_batch(true);
        self.close()
    }

    fn close(mut self) -> Vec<u8> {
        if let Some(r) = self.held.take() {
            self.write(&r);
        }
        if !self.started {
            self.header();
        }
        self.out.push(0x3B);
        self.out
    }

    fn process(&mut self, frames: &[(&Rgba, u16)], last: bool) {
        assert!(
            frames
                .iter()
                .all(|f| f.0.width as usize == self.width && f.0.height as usize == self.height),
            "every frame has the canvas size"
        );
        if self.opts.palette == PaletteMode::Global && self.global.is_none() && !frames.is_empty() {
            let refs: Vec<&Rgba> = frames.iter().map(|f| f.0).collect();
            let palette = Palette::from_frames(&refs, self.max_colors());
            let lut = Lut::new(&palette);
            self.global = Some((palette, lut));
        }
        let n = self.width * self.height;
        let mut bufs: Vec<Vec<u8>> = (0..frames.len())
            .map(|_| self.spare.pop().unwrap_or_default())
            .collect();
        for b in bufs.iter_mut() {
            b.resize(n, 0);
        }
        let colors = self.max_colors();
        let dither = self.opts.dither;
        let global = self.global.as_ref();
        let mapped: Vec<Mapped> = frames
            .par_iter()
            .zip(bufs.into_par_iter())
            .map(|(&(src, delay), mut idx)| {
                let alpha = src.data.chunks_exact(4).any(|p| p[3] < 128);
                let own;
                let (palette, lut, local) = match global {
                    Some((p, l)) => (p, l, None),
                    None => {
                        let mut h = HIST.take().unwrap_or_default();
                        h.clear();
                        h.add(src);
                        let p = h.palette(colors);
                        HIST.set(Some(h));
                        let l = Lut::new(&p);
                        own = (p, l);
                        (&own.0, &own.1, Some(own.0.clone()))
                    }
                };
                let mapper = Mapper {
                    lut,
                    palette: palette.colors(),
                    dither,
                    transparent: palette.len() as u8,
                };
                let mut err = ERR.take();
                mapper.map(src, &mut idx, &mut err);
                ERR.set(err);
                Mapped {
                    idx,
                    palette: local,
                    alpha,
                    delay,
                }
            })
            .collect();
        let carry = self.carry.take();
        let had_pending = self.pending.is_some();
        let mut items: Vec<Mapped> = self.pending.take().into_iter().chain(mapped).collect();
        let srcs: Vec<&Rgba> = carry
            .iter()
            .filter(|_| had_pending)
            .chain(frames.iter().map(|f| f.0))
            .collect();
        let count = if last {
            items.len()
        } else {
            items.len().saturating_sub(1)
        };
        let alphas: Vec<bool> = items.iter().map(|m| m.alpha).collect();
        let rest = items.split_off(count);
        let mut prev = self.prev.take();
        let mut records = Vec::with_capacity(count);
        for (i, f) in items.into_iter().enumerate() {
            let before = if i == 0 {
                prev.as_ref()
            } else {
                Some(srcs[i - 1])
            };
            let next_alpha = alphas.get(i + 1).copied().unwrap_or(false);
            records.push(self.diff(f, srcs[i], before, next_alpha));
        }
        if count > 0 {
            keep(&mut prev, srcs[count - 1]);
        }
        drop(srcs);
        self.prev = prev;
        if let Some(m) = rest.into_iter().next() {
            if count == 0 && had_pending {
                self.carry = carry;
            } else {
                let mut buf = carry;
                keep(&mut buf, frames[count - had_pending as usize].0);
                self.carry = buf;
            }
            self.pending = Some(m);
        }
        let w = self.width;
        records.par_iter_mut().for_each(|r| encode_body(r, w));
        for r in records {
            self.emit(r);
        }
    }

    fn diff(&mut self, f: Mapped, src: &Rgba, prev: Option<&Rgba>, next_alpha: bool) -> Record {
        let palette = f
            .palette
            .as_ref()
            .unwrap_or_else(|| &self.global.as_ref().unwrap().0);
        let t = palette.len() as u8;
        let colors = packed_colors(palette);
        let full = prev.is_none() || f.alpha || next_alpha || !self.opts.diff;
        let mut idx = f.idx;
        let w = self.width;
        let rect = if full {
            for (c, &i) in self.canvas.iter_mut().zip(&idx) {
                *c = colors[i as usize];
            }
            (0, 0, w, self.height)
        } else {
            diff_rows(
                &mut idx,
                &mut self.canvas,
                &src.data,
                &prev.unwrap().data,
                &colors,
                t,
                w,
            )
        };
        if next_alpha {
            self.canvas.fill(0);
        }
        Record {
            rect,
            idx,
            palette: f.palette,
            transparent: t,
            disposal: if next_alpha { 2 } else { 1 },
            delay: f.delay,
            body: Vec::new(),
        }
    }

    fn emit(&mut self, mut r: Record) {
        if r.rect.2 == 0 {
            if let Some(h) = self.held.as_mut()
                && let Some(d) = h.delay.checked_add(r.delay)
            {
                h.delay = d;
                self.spare.push(r.idx);
                return;
            }
            r.rect = (0, 0, 1, 1);
            r.idx[0] = r.transparent;
            encode_body(&mut r, self.width);
        }
        if let Some(h) = self.held.take() {
            self.write(&h);
            self.spare.push(h.idx);
        }
        self.held = Some(r);
    }

    fn header(&mut self) {
        self.started = true;
        let o = &mut self.out;
        o.extend_from_slice(b"GIF89a");
        o.extend_from_slice(&(self.width as u16).to_le_bytes());
        o.extend_from_slice(&(self.height as u16).to_le_bytes());
        match &self.global {
            Some((p, _)) => {
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
}

fn table(o: &mut Vec<u8>, p: &Palette, bits: u8) {
    for c in p.colors() {
        o.extend_from_slice(c);
    }
    o.resize(o.len() + ((1usize << bits) - p.len()) * 3, 0);
}

fn encode_body(r: &mut Record, stride: usize) {
    let (x, y, w, h) = r.rect;
    if w == 0 {
        return;
    }
    let entries = r
        .palette
        .as_ref()
        .map_or(0, |p| p.len())
        .max(r.transparent as usize)
        + 1;
    let min = bits_for(entries).max(2);
    let idx = &r.idx;
    let rows = (y..y + h).map(|row| &idx[row * stride + x..row * stride + x + w]);
    let mut body = std::mem::take(&mut r.body);
    body.clear();
    LZW.with_borrow_mut(|l| l.encode(min, rows, &mut body));
    r.body = body;
}

fn diff_rows(
    idx: &mut [u8],
    canvas: &mut [u32],
    src: &[u8],
    prev: &[u8],
    colors: &[u32; 256],
    t: u8,
    w: usize,
) -> (usize, usize, usize, usize) {
    let band = 16;
    let boxes: Vec<(usize, usize, usize, usize)> = idx
        .par_chunks_mut(w * band)
        .zip(canvas.par_chunks_mut(w * band))
        .zip(
            src.par_chunks(w * 4 * band)
                .zip(prev.par_chunks(w * 4 * band)),
        )
        .enumerate()
        .map(|(i, ((ib, cb), (sb, pb)))| {
            let (x0, y0, x1, y1) = diff_band(ib, cb, sb, pb, colors, t, w);
            (x0, y0 + i * band, x1, y1 + i * band)
        })
        .collect();
    let mut r = (usize::MAX, usize::MAX, 0, 0);
    for b in boxes {
        if b.2 > b.0 {
            r = (r.0.min(b.0), r.1.min(b.1), r.2.max(b.2), r.3.max(b.3));
        }
    }
    if r.0 == usize::MAX {
        (0, 0, 0, 0)
    } else {
        (r.0, r.1, r.2 - r.0, r.3 - r.1)
    }
}

crate::gif::isa::dispatch! {
    fn diff_band(
        idx: &mut [u8],
        canvas: &mut [u32],
        src: &[u8],
        prev: &[u8],
        colors: &[u32; 256],
        t: u8,
        w: usize,
    ) -> (usize, usize, usize, usize) {
        let mut x0 = usize::MAX;
        let mut x1 = 0;
        let mut y0 = 0;
        let mut y1 = 0;
        for (y, ((irow, crow), (srow, prow))) in idx
            .chunks_exact_mut(w)
            .zip(canvas.chunks_exact_mut(w))
            .zip(src.chunks_exact(w * 4).zip(prev.chunks_exact(w * 4)))
            .enumerate()
        {
            let mut first = usize::MAX;
            let mut lastx = 0;
            for (x, ((i, c), (s, p))) in irow
                .iter_mut()
                .zip(crow.iter_mut())
                .zip(srow.chunks_exact(4).zip(prow.chunks_exact(4)))
                .enumerate()
            {
                let col = colors[*i as usize];
                if s == p || col == *c {
                    *i = t;
                } else {
                    *c = col;
                    first = first.min(x);
                    lastx = x;
                }
            }
            if first != usize::MAX {
                if x0 == usize::MAX {
                    y0 = y;
                }
                x0 = x0.min(first);
                x1 = x1.max(lastx + 1);
                y1 = y + 1;
            }
        }
        if x0 == usize::MAX {
            (0, 0, 0, 0)
        } else {
            (x0, y0, x1, y1)
        }
    }
}

pub fn encode(frames: &[Rgba], delays_cs: &[u16], opts: EncodeOptions) -> Vec<u8> {
    let Some(first) = frames.first() else {
        return Encoder::new(1, 1, opts).finish();
    };
    let mut e = match opts.palette {
        PaletteMode::Global => {
            let refs: Vec<&Rgba> = frames.iter().collect();
            let palette = Palette::from_frames(&refs, opts.colors.clamp(2, 256).min(255));
            Encoder::with_palette(first.width, first.height, opts, palette)
        }
        PaletteMode::PerFrame => Encoder::new(first.width, first.height, opts),
    };
    let delays: Vec<u16> = (0..frames.len())
        .map(|i| delays_cs.get(i).copied().unwrap_or(10))
        .collect();
    let size = batch_size();
    let refs: Vec<(&Rgba, u16)> = frames.iter().zip(delays).collect();
    let chunks = refs.chunks(size).count();
    for (i, chunk) in refs.chunks(size).enumerate() {
        e.process(chunk, i + 1 == chunks);
    }
    e.close()
}
