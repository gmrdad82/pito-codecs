use super::lzw::LzwDecoder;
use super::{Error, Result};
use crate::media::Rgba;
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

const AHEAD: usize = 16;
const MAX_PIXELS: usize = 1 << 28;
const MIN_BUDGET: usize = 1 << 20;
const PIXELS_PER_BYTE: usize = 1 << 12;
const LZW_PIXELS_PER_BYTE: usize = 1 << 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposal {
    Keep,
    Background,
    Previous,
}

pub struct Frame<'a> {
    pub image: &'a Rgba,
    pub delay_cs: u16,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub disposal: Disposal,
}

pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub loop_count: Option<u16>,
    pub frames: Vec<(Rgba, u16)>,
}

struct Raw {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    interlaced: bool,
    palette: Option<(usize, usize)>,
    transparent: Option<u8>,
    delay_cs: u16,
    disposal: Disposal,
    min_code: u8,
    data: Vec<u8>,
    indices: Vec<u8>,
    decoded: usize,
}

pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    width: u16,
    height: u16,
    global: Option<(usize, usize)>,
    loop_count: Option<u16>,
    canvas: Rgba,
    saved: Vec<u8>,
    last: Option<(Disposal, u16, u16, u16, u16)>,
    queue: VecDeque<Receiver<Raw>>,
    spare: Vec<Raw>,
    current: Option<Raw>,
    ended: bool,
    gce: Option<(Disposal, u16, Option<u8>)>,
}

fn table(packed: u8) -> usize {
    3 << ((packed & 7) + 1)
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        if data.len() < 13 || !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
            return Err(Error::NotGif);
        }
        let width = u16::from_le_bytes([data[6], data[7]]);
        let height = u16::from_le_bytes([data[8], data[9]]);
        let packed = data[10];
        let mut pos = 13;
        let mut global = None;
        if packed & 0x80 != 0 {
            let n = table(packed);
            if pos + n > data.len() {
                return Err(Error::Truncated);
            }
            global = Some((pos, n / 3));
            pos += n;
        }
        if width == 0 || height == 0 {
            return Err(Error::Invalid("zero canvas size"));
        }
        let pixels = width as usize * height as usize;
        if pixels > MAX_PIXELS {
            return Err(Error::Invalid("canvas too large"));
        }
        if pixels > data.len().saturating_mul(PIXELS_PER_BYTE).max(MIN_BUDGET) {
            return Err(Error::Invalid("canvas far larger than the file"));
        }
        let mut d = Decoder {
            data,
            pos,
            width,
            height,
            global,
            loop_count: None,
            canvas: Rgba::new(width as u32, height as u32),
            saved: Vec::new(),
            last: None,
            queue: VecDeque::new(),
            spare: Vec::new(),
            current: None,
            ended: false,
            gce: None,
        };
        d.fill()?;
        Ok(d)
    }

    pub fn width(&self) -> u32 {
        self.width as u32
    }

    pub fn height(&self) -> u32 {
        self.height as u32
    }

    pub fn loop_count(&self) -> Option<u16> {
        self.loop_count
    }

    fn sub_blocks(&mut self, mut sink: Option<&mut Vec<u8>>) -> Result<()> {
        loop {
            let Some(&n) = self.data.get(self.pos) else {
                return Err(Error::Truncated);
            };
            self.pos += 1;
            if n == 0 {
                return Ok(());
            }
            let end = self.pos + n as usize;
            if end > self.data.len() {
                if let Some(s) = sink.as_deref_mut() {
                    s.extend_from_slice(&self.data[self.pos..]);
                }
                self.pos = self.data.len();
                return Err(Error::Truncated);
            }
            if let Some(s) = sink.as_deref_mut() {
                s.extend_from_slice(&self.data[self.pos..end]);
            }
            self.pos = end;
        }
    }

    fn extension(&mut self) -> Result<()> {
        let Some(&label) = self.data.get(self.pos) else {
            return Err(Error::Truncated);
        };
        self.pos += 1;
        match label {
            0xF9 => {
                let d = self.data;
                if self.pos + 6 <= d.len() && d[self.pos] >= 4 {
                    let p = self.pos + 1;
                    let flags = d[p];
                    let delay = u16::from_le_bytes([d[p + 1], d[p + 2]]);
                    let disposal = match (flags >> 2) & 7 {
                        2 => Disposal::Background,
                        3 => Disposal::Previous,
                        _ => Disposal::Keep,
                    };
                    let transparent = (flags & 1 != 0).then_some(d[p + 3]);
                    self.gce = Some((disposal, delay, transparent));
                }
                self.sub_blocks(None)
            }
            0xFF => {
                let d = self.data;
                let p = self.pos;
                if p + 1 + 11 <= d.len() && d[p] == 11 {
                    let id = &d[p + 1..p + 12];
                    if id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0" {
                        let q = p + 12;
                        if q + 4 <= d.len() && d[q] >= 3 && d[q + 1] == 1 {
                            self.loop_count = Some(u16::from_le_bytes([d[q + 2], d[q + 3]]));
                        }
                    }
                }
                self.sub_blocks(None)
            }
            0x01 => {
                self.gce = None;
                self.sub_blocks(None)
            }
            _ => self.sub_blocks(None),
        }
    }

    fn scan(&mut self) -> Result<Option<Raw>> {
        loop {
            let Some(&b) = self.data.get(self.pos) else {
                return Ok(None);
            };
            self.pos += 1;
            match b {
                0x21 => self.extension()?,
                0x2C => return self.image().map(Some),
                0x3B => return Ok(None),
                _ => return Ok(None),
            }
        }
    }

    fn image(&mut self) -> Result<Raw> {
        let d = self.data;
        let p = self.pos;
        if p + 9 > d.len() {
            return Err(Error::Truncated);
        }
        let word = |i: usize| u16::from_le_bytes([d[p + i], d[p + i + 1]]);
        let (left, top, width, height) = (word(0), word(2), word(4), word(6));
        let packed = d[p + 8];
        self.pos += 9;
        let canvas = self.width as usize * self.height as usize;
        if width as usize * height as usize > canvas.max(1 << 20) * 4 {
            return Err(Error::Invalid("frame far larger than the canvas"));
        }
        let mut palette = None;
        if packed & 0x80 != 0 {
            let n = table(packed);
            if self.pos + n > d.len() {
                return Err(Error::Truncated);
            }
            palette = Some((self.pos, n / 3));
            self.pos += n;
        }
        let Some(&min_code) = d.get(self.pos) else {
            return Err(Error::Truncated);
        };
        self.pos += 1;
        let (disposal, delay_cs, transparent) =
            self.gce.take().unwrap_or((Disposal::Keep, 0, None));
        let mut raw = self.spare.pop().unwrap_or_else(|| Raw {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            interlaced: false,
            palette: None,
            transparent: None,
            delay_cs: 0,
            disposal: Disposal::Keep,
            min_code: 0,
            data: Vec::new(),
            indices: Vec::new(),
            decoded: 0,
        });
        raw.left = left;
        raw.top = top;
        raw.width = width;
        raw.height = height;
        raw.interlaced = packed & 0x40 != 0;
        raw.palette = palette;
        raw.transparent = transparent;
        raw.delay_cs = delay_cs;
        raw.disposal = disposal;
        raw.min_code = min_code;
        raw.data.clear();
        raw.decoded = 0;
        let mut data = std::mem::take(&mut raw.data);
        let r = self.sub_blocks(Some(&mut data));
        raw.data = data;
        match r {
            Ok(()) | Err(Error::Truncated) => Ok(raw),
            Err(e) => Err(e),
        }
    }

    fn fill(&mut self) -> Result<()> {
        if self.ended {
            return Ok(());
        }
        while self.queue.len() < AHEAD {
            match self.scan() {
                Ok(Some(mut raw)) => {
                    let (tx, rx) = sync_channel(1);
                    rayon::spawn(move || {
                        decode_raw(&mut raw);
                        let _ = tx.send(raw);
                    });
                    self.queue.push_back(rx);
                }
                Ok(None) | Err(Error::Truncated) => {
                    self.ended = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame<'_>>> {
        if let Some(r) = self.current.take() {
            self.spare.push(r);
        }
        self.fill()?;
        let Some(rx) = self.queue.pop_front() else {
            return Ok(None);
        };
        let raw = wait(rx).ok_or(Error::Invalid("frame decode task failed"))?;
        self.dispose();
        self.draw(&raw);
        let frame = (
            raw.delay_cs,
            raw.left,
            raw.top,
            raw.width,
            raw.height,
            raw.disposal,
        );
        self.last = Some((raw.disposal, raw.left, raw.top, raw.width, raw.height));
        self.current = Some(raw);
        Ok(Some(Frame {
            image: &self.canvas,
            delay_cs: frame.0,
            left: frame.1,
            top: frame.2,
            width: frame.3,
            height: frame.4,
            disposal: frame.5,
        }))
    }

    fn clip(&self, left: u16, top: u16, w: u16, h: u16) -> (usize, usize, usize, usize) {
        let cw = self.width as usize;
        let ch = self.height as usize;
        let x0 = (left as usize).min(cw);
        let y0 = (top as usize).min(ch);
        let x1 = (left as usize + w as usize).min(cw);
        let y1 = (top as usize + h as usize).min(ch);
        (x0, y0, x1, y1)
    }

    fn dispose(&mut self) {
        let Some((disposal, left, top, w, h)) = self.last.take() else {
            return;
        };
        let (x0, y0, x1, y1) = self.clip(left, top, w, h);
        let stride = self.canvas.stride();
        match disposal {
            Disposal::Keep => {}
            Disposal::Background => {
                for y in y0..y1 {
                    self.canvas.data[y * stride + x0 * 4..y * stride + x1 * 4].fill(0);
                }
            }
            Disposal::Previous => {
                let n = (x1 - x0) * 4;
                for (i, y) in (y0..y1).enumerate() {
                    if self.saved.len() >= (i + 1) * n {
                        self.canvas.data[y * stride + x0 * 4..y * stride + x1 * 4]
                            .copy_from_slice(&self.saved[i * n..(i + 1) * n]);
                    }
                }
            }
        }
    }

    fn draw(&mut self, raw: &Raw) {
        let (x0, y0, x1, y1) = self.clip(raw.left, raw.top, raw.width, raw.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.canvas.stride();
        if raw.disposal == Disposal::Previous {
            self.saved.clear();
            for y in y0..y1 {
                self.saved
                    .extend_from_slice(&self.canvas.data[y * stride + x0 * 4..y * stride + x1 * 4]);
            }
        }
        let mut colors = [[0u8; 4]; 256];
        let pal = raw.palette.or(self.global);
        if let Some((at, n)) = pal {
            for (i, c) in colors.iter_mut().enumerate().take(n.min(256)) {
                let p = at + i * 3;
                *c = [self.data[p], self.data[p + 1], self.data[p + 2], 255];
            }
            for c in colors.iter_mut().skip(n.min(256)) {
                *c = [0, 0, 0, 255];
            }
        } else {
            colors.fill([0, 0, 0, 255]);
        }
        let opaque = raw.transparent.is_none();
        let t = raw.transparent.unwrap_or(0) as usize;
        let w = raw.width as usize;
        let full_rows = raw.decoded / w.max(1);
        let tail = raw.decoded % w.max(1);
        let rows = raw.height as usize;
        for k in 0..rows.min(full_rows + 1) {
            let y = if raw.interlaced {
                interlaced_row(k, rows)
            } else {
                k
            };
            let cy = raw.top as usize + y;
            if cy < y0 || cy >= y1 {
                continue;
            }
            let n = if k < full_rows { w } else { tail };
            let src = &raw.indices[k * w..k * w + n];
            let lim = (x1 - x0).min(n);
            let src = &src[..lim];
            let dst = &mut self.canvas.data[cy * stride + x0 * 4..cy * stride + (x0 + lim) * 4];
            blit(src, dst, &colors, opaque, t);
        }
    }

    pub fn canvas(&self) -> &Rgba {
        &self.canvas
    }
}

crate::media::isa::dispatch! {
    fn blit(src: &[u8], dst: &mut [u8], colors: &[[u8; 4]; 256], opaque: bool, t: usize) {
        if opaque {
            for (d, &i) in dst.chunks_exact_mut(4).zip(src) {
                d.copy_from_slice(&colors[i as usize]);
            }
        } else {
            for (d, &i) in dst.chunks_exact_mut(4).zip(src) {
                if i as usize != t {
                    d.copy_from_slice(&colors[i as usize]);
                }
            }
        }
    }
}

fn interlaced_row(k: usize, h: usize) -> usize {
    let p1 = h.div_ceil(8);
    let p2 = (h + 3) / 8;
    let p3 = (h + 1) / 4;
    if k < p1 {
        k * 8
    } else if k < p1 + p2 {
        (k - p1) * 8 + 4
    } else if k < p1 + p2 + p3 {
        (k - p1 - p2) * 4 + 2
    } else {
        (k - p1 - p2 - p3) * 2 + 1
    }
}

fn wait(rx: Receiver<Raw>) -> Option<Raw> {
    if rayon::current_thread_index().is_none() {
        return rx.recv().ok();
    }
    loop {
        match rx.try_recv() {
            Ok(raw) => return Some(raw),
            Err(TryRecvError::Empty) => {
                if rayon::yield_now() != Some(rayon::Yield::Executed) {
                    std::thread::yield_now();
                }
            }
            Err(TryRecvError::Disconnected) => return None,
        }
    }
}

fn decode_raw(raw: &mut Raw) {
    thread_local! {
        static LZW: std::cell::RefCell<LzwDecoder> = std::cell::RefCell::new(LzwDecoder::default());
    }
    let n = (raw.width as usize * raw.height as usize)
        .min(raw.data.len().saturating_mul(LZW_PIXELS_PER_BYTE));
    raw.indices.clear();
    raw.indices.resize(n, 0);
    raw.decoded = LZW.with_borrow_mut(|l| l.decode(raw.min_code, &raw.data, &mut raw.indices));
}

pub fn decode(data: &[u8]) -> Result<Animation> {
    let mut d = Decoder::new(data)?;
    let mut frames = Vec::new();
    while let Some(f) = d.next_frame()? {
        let delay = f.delay_cs;
        frames.push((f.image.clone(), delay));
    }
    Ok(Animation {
        width: d.width(),
        height: d.height(),
        loop_count: d.loop_count(),
        frames,
    })
}
