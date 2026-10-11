use super::riff::{self, invalid};
use super::{Info, Options, Parsed, Piece, Rgba, check_image, decode_piece, encode_piece, parse};
use crate::media::Result;

pub struct Frame {
    pub image: Rgba,
    pub start: u32,
    pub duration: u32,
}

pub struct FrameRef<'a> {
    pub image: &'a Rgba,
    pub start: u32,
    pub duration: u32,
}

pub struct Animation {
    pub info: Info,
    pub frames: Vec<Frame>,
}

pub fn decode_animation(data: &[u8]) -> Result<Animation> {
    let mut dec = AnimDecoder::new(data)?;
    let info = dec.info;
    let mut frames = Vec::with_capacity(info.frames as usize);
    while let Some(f) = dec.next_frame()? {
        frames.push(Frame {
            image: f.image.clone(),
            start: f.start,
            duration: f.duration,
        });
    }
    Ok(Animation { info, frames })
}

pub struct AnimDecoder<'a> {
    info: Info,
    pieces: Vec<Piece<'a>>,
    index: usize,
    canvas: Rgba,
    disposed: Rgba,
    tile: Vec<u8>,
    start: u32,
    prev_key: bool,
}

impl<'a> AnimDecoder<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        Ok(Self::from_parsed(parse(data)?))
    }

    pub(crate) fn from_parsed(parsed: Parsed<'a>) -> Self {
        let Parsed { info, pieces } = parsed;
        AnimDecoder {
            info,
            pieces,
            index: 0,
            canvas: Rgba::new(info.width, info.height),
            disposed: Rgba::new(info.width, info.height),
            tile: Vec::new(),
            start: 0,
            prev_key: false,
        }
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn next_frame(&mut self) -> Result<Option<FrameRef<'_>>> {
        let Some(&p) = self.pieces.get(self.index) else {
            return Ok(None);
        };
        let (cw, ch) = (self.info.width, self.info.height);
        let full = |q: &Piece| q.width == cw && q.height == ch;
        let key = self.index == 0 || ((!p.alpha || !p.blend) && full(&p)) || {
            let prev = &self.pieces[self.index - 1];
            prev.dispose && (full(prev) || self.prev_key)
        };
        if key {
            self.canvas.data.fill(0);
        } else {
            self.canvas.data.copy_from_slice(&self.disposed.data);
        }
        self.tile
            .resize(p.width as usize * p.height as usize * 4, 0);
        decode_piece(&p, &mut self.tile)?;
        let stride = cw as usize * 4;
        let row = p.width as usize * 4;
        for (y, src) in self.tile.chunks_exact(row).enumerate() {
            let o = (p.y as usize + y) * stride + p.x as usize * 4;
            self.canvas.data[o..o + row].copy_from_slice(src);
        }
        if self.index > 0 && p.blend && !key {
            let prev = self.pieces[self.index - 1];
            for y in 0..p.height {
                let cy = p.y + y;
                let mut spans = [(p.x, p.x + p.width), (0, 0)];
                if prev.dispose && cy >= prev.y && cy < prev.y + prev.height {
                    let (l, r) = (prev.x.max(p.x), (prev.x + prev.width).min(p.x + p.width));
                    if l < r {
                        spans = [(p.x, l), (r, p.x + p.width)];
                    }
                }
                for (l, r) in spans {
                    if l >= r {
                        continue;
                    }
                    let o = cy as usize * stride + l as usize * 4;
                    let e = cy as usize * stride + r as usize * 4;
                    blend_row(&mut self.canvas.data[o..e], &self.disposed.data[o..e]);
                }
            }
        }
        self.disposed.data.copy_from_slice(&self.canvas.data);
        if p.dispose {
            for y in p.y..p.y + p.height {
                let o = y as usize * stride + p.x as usize * 4;
                self.disposed.data[o..o + row].fill(0);
            }
        }
        self.prev_key = key;
        self.index += 1;
        let start = self.start;
        self.start += p.duration;
        Ok(Some(FrameRef {
            image: &self.canvas,
            start,
            duration: p.duration,
        }))
    }
}

fn blend_row(src: &mut [u8], dst: &[u8]) {
    for (s, d) in src.chunks_exact_mut(4).zip(dst.chunks_exact(4)) {
        let sa = s[3] as u32;
        if sa == 255 {
            continue;
        }
        if sa == 0 {
            s.copy_from_slice(d);
            continue;
        }
        let da = (d[3] as u32 * (256 - sa)) >> 8;
        let ba = sa + da;
        let scale = (1u32 << 24) / ba;
        for c in 0..3 {
            s[c] = (((s[c] as u32 * sa + d[c] as u32 * da) * scale) >> 24) as u8;
        }
        s[3] = ba as u8;
    }
}

pub struct AnimEncoder {
    width: u32,
    height: u32,
    options: Options,
    loop_count: u16,
    background: [u8; 4],
    body: Vec<u8>,
    pending: Option<Pending>,
    prev: Option<Rgba>,
    alpha: bool,
    work: Rgba,
}

struct Pending {
    head: [u32; 4],
    duration: u32,
    blend: bool,
    data: Vec<u8>,
}

impl AnimEncoder {
    pub fn new(width: u32, height: u32, options: Options, loop_count: u16) -> Result<Self> {
        check_image(&Rgba::new(width, height))?;
        Ok(AnimEncoder {
            width,
            height,
            options,
            loop_count,
            background: [0; 4],
            body: Vec::new(),
            pending: None,
            prev: None,
            alpha: false,
            work: Rgba::new(0, 0),
        })
    }

    pub fn background(&mut self, rgba: [u8; 4]) {
        self.background = rgba;
    }

    pub fn add(&mut self, image: &Rgba, duration: u32) -> Result<()> {
        if image.width != self.width || image.height != self.height {
            return Err(invalid("animation frame size differs from the canvas"));
        }
        check_image(image)?;
        let duration = duration.min(0xff_ffff);
        let Some(prev) = &self.prev else {
            let (data, alpha) = encode_piece(image, &self.options)?;
            self.alpha |= alpha;
            self.pending = Some(Pending {
                head: [0, 0, self.width, self.height],
                duration,
                blend: false,
                data,
            });
            self.prev = Some(image.clone());
            return Ok(());
        };
        let Some((x0, y0, x1, y1)) = changed(prev, image) else {
            if let Some(p) = &mut self.pending {
                p.duration = (p.duration + duration).min(0xff_ffff);
            }
            return Ok(());
        };
        let (x0, y0) = (x0 & !1, y0 & !1);
        let (w, h) = (x1 - x0, y1 - y0);
        self.work.width = w;
        self.work.height = h;
        self.work.data.clear();
        let stride = self.width as usize * 4;
        let mut opaque = true;
        for y in y0..y1 {
            let o = y as usize * stride + x0 as usize * 4;
            let row = &image.data[o..o + w as usize * 4];
            opaque &= row.chunks_exact(4).all(|p| p[3] == 255);
            self.work.data.extend_from_slice(row);
        }
        let blend = opaque && self.options.lossless;
        if blend {
            for y in y0..y1 {
                let o = y as usize * stride + x0 as usize * 4;
                let before = &prev.data[o..o + w as usize * 4];
                let wo = (y - y0) as usize * w as usize * 4;
                for (dst, old) in self.work.data[wo..wo + w as usize * 4]
                    .chunks_exact_mut(4)
                    .zip(before.chunks_exact(4))
                {
                    if dst == old {
                        dst.copy_from_slice(&[0, 0, 0, 0]);
                    }
                }
            }
        }
        let (data, alpha) = encode_piece(&self.work, &self.options)?;
        self.alpha |= alpha;
        self.flush();
        self.pending = Some(Pending {
            head: [x0, y0, w, h],
            duration,
            blend,
            data,
        });
        self.prev
            .as_mut()
            .unwrap()
            .data
            .copy_from_slice(&image.data);
        Ok(())
    }

    fn flush(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let mut f = Vec::with_capacity(16 + p.data.len());
        riff::put_u24(&mut f, p.head[0] / 2);
        riff::put_u24(&mut f, p.head[1] / 2);
        riff::put_u24(&mut f, p.head[2] - 1);
        riff::put_u24(&mut f, p.head[3] - 1);
        riff::put_u24(&mut f, p.duration);
        f.push(if p.blend { 0 } else { 0x02 });
        f.extend_from_slice(&p.data);
        riff::put_chunk(&mut self.body, b"ANMF", &f);
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        if self.pending.is_none() {
            return Err(invalid("animation without frames"));
        }
        self.flush();
        let mut w = riff::Writer::new();
        let mut x = Vec::with_capacity(10);
        x.extend_from_slice(&[0x02 | if self.alpha { 0x10 } else { 0 }, 0, 0, 0]);
        riff::put_u24(&mut x, self.width - 1);
        riff::put_u24(&mut x, self.height - 1);
        w.chunk(b"VP8X", &x);
        let b = self.background;
        let mut anim = vec![b[2], b[1], b[0], b[3]];
        anim.extend_from_slice(&self.loop_count.to_le_bytes());
        w.chunk(b"ANIM", &anim);
        w.out.extend_from_slice(&self.body);
        w.finish()
    }
}

fn changed(a: &Rgba, b: &Rgba) -> Option<(u32, u32, u32, u32)> {
    let w = a.width as usize;
    let rows_a = a.data.chunks_exact(w * 4);
    let mut y0 = None;
    let mut y1 = 0;
    let mut x0 = w;
    let mut x1 = 0;
    for (y, (ra, rb)) in rows_a.zip(b.data.chunks_exact(w * 4)).enumerate() {
        if ra == rb {
            continue;
        }
        y0.get_or_insert(y);
        y1 = y + 1;
        let pa = ra.chunks_exact(4);
        let first = pa
            .clone()
            .zip(rb.chunks_exact(4))
            .position(|(p, q)| p != q)
            .unwrap();
        let last = w - pa
            .rev()
            .zip(rb.chunks_exact(4).rev())
            .position(|(p, q)| p != q)
            .unwrap();
        x0 = x0.min(first);
        x1 = x1.max(last);
    }
    y0.map(|y0| (x0 as u32, y0 as u32, x1 as u32, y1 as u32))
}
