mod bits;
mod exif;
mod idct;
mod sample;
mod table;
#[cfg(test)]
mod tests;

use super::writer::ZIGZAG;
use super::{Error, Result};
use crate::media::Rgba;
use bits::Bits;
use sample::Plane;
use table::Table;

const MAX_PIXELS: u64 = 1 << 28;
const MAX_SCANS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub sampling: Vec<(u8, u8)>,
    pub progressive: bool,
    pub orientation: u8,
    pub precision: u8,
    pub decodable: bool,
}

pub fn info(data: &[u8]) -> Result<Info> {
    let mut d = Decoder::new(data)?;
    d.headers(true)?;
    d.info()
}

pub fn decode(data: &[u8]) -> Result<Rgba> {
    decode_planes(data)?.rgba()
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Error(format!("JPEG: {}", message.into())))
}

fn zeroed<T: Clone>(n: usize, value: T) -> Result<Vec<T>> {
    let mut probe: Vec<T> = Vec::new();
    if probe.try_reserve_exact(n).is_err() {
        return err(format!("no memory for {n} samples"));
    }
    drop(probe);
    Ok(vec![value; n])
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    dc: usize,
    ac: usize,
    bw: usize,
    bh: usize,
    cw: usize,
    ch: usize,
    pred: i32,
    done: u64,
    quant: Option<[i32; 64]>,
    coefs: Vec<[i16; 64]>,
    plane: Vec<u8>,
}

impl Component {
    fn stride(&self) -> usize {
        self.bw * 8
    }
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
    ready: bool,
    precision: u8,
    decodable: bool,
}

#[derive(Default)]
struct Tables {
    qt: [Option<[i32; 64]>; 4],
    dc: [Option<Box<Table>>; 4],
    ac: [Option<Box<Table>>; 4],
}

#[derive(Clone, Copy)]
enum Coding<'t> {
    Baseline(&'t Table, &'t Table, &'t [i32; 64]),
    DcFirst(&'t Table),
    DcRefine,
    AcFirst(&'t Table),
    AcRefine(&'t Table),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Baseline,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    tables: Tables,
    restart: usize,
    frame: Option<Frame>,
    adobe: Option<u8>,
    jfif: bool,
    orientation: Option<u8>,
    scans: usize,
}

pub(crate) struct Planes {
    pub width: usize,
    pub height: usize,
    pub rgb: bool,
    pub planes: Vec<Decoded>,
}

pub(crate) struct Decoded {
    pub width: usize,
    pub height: usize,
    pub fx: usize,
    pub fy: usize,
    pub stride: usize,
    pub data: Vec<u8>,
}

#[cfg(test)]
impl Decoded {
    pub fn cropped(&self) -> Vec<u8> {
        self.data
            .chunks(self.stride)
            .take(self.height)
            .flat_map(|r| &r[..self.width.min(r.len())])
            .copied()
            .collect()
    }
}

pub(crate) fn decode_planes(data: &[u8]) -> Result<Planes> {
    let mut d = Decoder::new(data)?;
    d.headers(false)?;
    d.finish()
}

impl Planes {
    pub fn rgba(&self) -> Result<Rgba> {
        let (w, h) = (self.width, self.height);
        let mut out = Rgba {
            width: w as u32,
            height: h as u32,
            data: zeroed(w * h * 4, 0u8)?,
        };
        if w == 0 {
            return Ok(out);
        }
        let planes: Vec<Plane> = self
            .planes
            .iter()
            .map(|p| Plane {
                data: &p.data,
                stride: p.stride,
                width: p.width,
                height: p.height,
            })
            .collect();
        let mut ups = Vec::with_capacity(planes.len());
        for (plane, p) in planes.iter().zip(&self.planes) {
            ups.push(sample::Upsampler::new(plane, p.fx, w)?);
        }
        let f: Vec<(usize, usize)> = self.planes.iter().map(|p| (p.fx, p.fy)).collect();
        let rows = out.data.chunks_exact_mut(w * 4).enumerate();
        match (ups.as_mut_slice(), planes.as_slice(), f.as_slice()) {
            ([u], [p], [(fx, fy)]) => {
                for (y, dst) in rows {
                    sample::grey(u.row(p, *fx, *fy, y), dst);
                }
            }
            ([u0, u1, u2], [p0, p1, p2], [f0, f1, f2]) => {
                for (y, dst) in rows {
                    let ra = u0.row(p0, f0.0, f0.1, y);
                    let rb = u1.row(p1, f1.0, f1.1, y);
                    let rc = u2.row(p2, f2.0, f2.1, y);
                    if self.rgb {
                        for (((px, &r), &g), &b) in dst.chunks_exact_mut(4).zip(ra).zip(rb).zip(rc)
                        {
                            px.copy_from_slice(&[r, g, b, 255]);
                        }
                    } else {
                        sample::ycbcr(ra, rb, rc, dst);
                    }
                }
            }
            _ => return err(format!("{} colour components", planes.len())),
        }
        Ok(out)
    }
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Result<Decoder<'a>> {
        if !data.starts_with(&[0xff, 0xd8]) {
            return err("not a JPEG file (no start-of-image marker)");
        }
        Ok(Decoder {
            data,
            pos: 2,
            tables: Tables::default(),
            restart: 0,
            frame: None,
            adobe: None,
            jfif: false,
            orientation: None,
            scans: 0,
        })
    }

    fn marker(&mut self) -> Result<u8> {
        match self.data.get(self.pos) {
            Some(0xff) => {}
            Some(_) => return err(format!("no marker at byte {}", self.pos)),
            None => return err("the file ends before its end-of-image marker"),
        }
        while self.data.get(self.pos) == Some(&0xff) {
            self.pos += 1;
        }
        let Some(&m) = self.data.get(self.pos) else {
            return err("the file ends before its end-of-image marker");
        };
        self.pos += 1;
        Ok(m)
    }

    fn segment(&mut self, m: u8) -> Result<&'a [u8]> {
        let at = self.pos;
        let Some(len) = self.data.get(at..at + 2) else {
            return err(format!("the file ends inside the 0xFF{m:02X} segment"));
        };
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        if len < 2 {
            return err(format!("a 0xFF{m:02X} segment of length {len}"));
        }
        let Some(body) = self.data.get(at + 2..at + len) else {
            return err(format!("the file ends inside the 0xFF{m:02X} segment"));
        };
        self.pos = at + len;
        Ok(body)
    }

    fn complete(&self) -> bool {
        let rest = self.data.get(self.pos..).unwrap_or(&[]);
        self.scans > 0
            && rest.iter().all(|&b| b == 0xff)
            && self
                .frame
                .as_ref()
                .is_some_and(|f| f.comps.iter().all(|c| c.done == u64::MAX))
    }

    fn headers(&mut self, header_only: bool) -> Result<()> {
        loop {
            if !header_only && self.complete() {
                return Ok(());
            }
            let at = self.pos;
            let m = self.marker()?;
            match m {
                0xd8 | 0x01 | 0xd0..=0xd7 => {}
                0xd9 => return Ok(()),
                0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf if header_only => {
                    let body = self.segment(m)?;
                    let progressive = matches!(m, 0xc2 | 0xc6 | 0xca | 0xce);
                    return self.sof(body, progressive, m <= 0xc2, false);
                }
                0xcc | 0xc4 | 0xdb | 0xdd if header_only => {
                    self.segment(m)?;
                }
                0xc0..=0xc2 => {
                    let body = self.segment(m)?;
                    self.sof(body, m == 0xc2, true, true)?;
                }
                0xc3 | 0xc7 | 0xcb | 0xcf => return err("lossless JPEG is not supported"),
                0xc5 | 0xc6 => return err("hierarchical JPEG is not supported"),
                0xc9 | 0xca | 0xcc | 0xcd | 0xce => {
                    return err("arithmetic-coded JPEG is not supported");
                }
                0xc4 => {
                    let body = self.segment(m)?;
                    self.dht(body)?;
                }
                0xdb => {
                    let body = self.segment(m)?;
                    self.dqt(body)?;
                }
                0xdd => {
                    let body = self.segment(m)?;
                    let [a, b] = body else {
                        return err(format!("a restart segment of {} bytes", body.len()));
                    };
                    self.restart = usize::from(u16::from_be_bytes([*a, *b]));
                }
                0xdc => return err("a height given after the image (DNL) is not supported"),
                0xe0..=0xef | 0xfe => {
                    let body = self.segment(m)?;
                    if m == 0xe1 && self.orientation.is_none() {
                        self.orientation = exif::orientation(body);
                    }
                    if m == 0xe0 && body.starts_with(b"JFIF\0") {
                        self.jfif = true;
                    }
                    if m == 0xee && body.starts_with(b"Adobe") && body.len() >= 12 {
                        self.adobe = Some(body[11]);
                    }
                }
                0xda => {
                    if header_only {
                        return Ok(());
                    }
                    let body = self.segment(m)?;
                    self.sos(body)?;
                }
                _ if header_only => {
                    self.segment(m)?;
                }
                _ => return err(format!("unknown marker 0xFF{m:02X} at byte {at}")),
            }
        }
    }

    fn info(&self) -> Result<Info> {
        let Some(f) = &self.frame else {
            return err("no frame header before the image data");
        };
        Ok(Info {
            width: f.width as u32,
            height: f.height as u32,
            sampling: f.comps.iter().map(|c| (c.h as u8, c.v as u8)).collect(),
            progressive: f.progressive,
            orientation: self.orientation.unwrap_or(1),
            precision: f.precision,
            decodable: f.decodable,
        })
    }

    fn sof(&mut self, body: &[u8], progressive: bool, known: bool, strict: bool) -> Result<()> {
        if self.frame.is_some() {
            return err("two frame headers");
        }
        let [p, h1, h0, w1, w0, nf, rest @ ..] = body else {
            return err("a short frame header");
        };
        if strict && *p != 8 {
            return err(format!("{p}-bit samples are not supported, only 8-bit"));
        }
        let height = usize::from(u16::from_be_bytes([*h1, *h0]));
        let width = usize::from(u16::from_be_bytes([*w1, *w0]));
        if width == 0 || height == 0 {
            return err(format!("a {width}x{height} frame"));
        }
        let large = (width as u64) * (height as u64) > MAX_PIXELS;
        if strict && large {
            return err(format!(
                "{width}x{height} is larger than {MAX_PIXELS} pixels"
            ));
        }
        let nf = usize::from(*nf);
        if nf == 0 || rest.len() != nf * 3 {
            return err(format!(
                "the frame header's length doesn't fit its {nf} components"
            ));
        }
        let mut comps: Vec<Component> = Vec::with_capacity(nf);
        for c in rest.chunks_exact(3) {
            let (id, h, v, tq) = (c[0], usize::from(c[1] >> 4), usize::from(c[1] & 15), c[2]);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                return err(format!("sampling factors {h}x{v} on component {id}"));
            }
            if tq > 3 {
                return err(format!("quantization table {tq} on component {id}"));
            }
            if comps.iter().any(|o| o.id == id) {
                return err(format!("component {id} named twice"));
            }
            let (h, v) = if nf == 1 { (1, 1) } else { (h, v) };
            comps.push(Component {
                id,
                h,
                v,
                tq: usize::from(tq),
                dc: 0,
                ac: 0,
                bw: 0,
                bh: 0,
                cw: 0,
                ch: 0,
                pred: 0,
                quant: None,
                done: 0,
                coefs: Vec::new(),
                plane: Vec::new(),
            });
        }
        let decodable = known && *p == 8 && !large && matches!(nf, 1 | 3);
        let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
        let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
        self.frame = Some(Frame {
            width,
            height,
            progressive,
            comps,
            hmax,
            vmax,
            mcux: width.div_ceil(8 * hmax),
            mcuy: height.div_ceil(8 * vmax),
            ready: false,
            precision: *p,
            decodable,
        });
        Ok(())
    }

    fn dht(&mut self, mut body: &[u8]) -> Result<()> {
        while let Some((&info, rest)) = body.split_first() {
            let (class, slot) = (info >> 4, usize::from(info & 15));
            if class > 1 || slot > 3 {
                return err(format!(
                    "a Huffman table with class {class} and slot {slot}"
                ));
            }
            let Some(counts) = rest.get(..16) else {
                return err("a short Huffman table segment");
            };
            let mut lengths = [0u8; 17];
            lengths[1..].copy_from_slice(counts);
            let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
            if total > 256 {
                return err("a Huffman table with more than 256 symbols");
            }
            let Some(symbols) = rest.get(16..16 + total) else {
                return err("a short Huffman table segment");
            };
            let t = Box::new(Table::new(&lengths, symbols, class == 0)?);
            if class == 0 {
                self.tables.dc[slot] = Some(t);
            } else {
                self.tables.ac[slot] = Some(t);
            }
            body = &rest[16 + total..];
        }
        Ok(())
    }

    fn dqt(&mut self, mut body: &[u8]) -> Result<()> {
        while let Some((&info, rest)) = body.split_first() {
            let (precision, slot) = (info >> 4, usize::from(info & 15));
            if precision > 1 || slot > 3 {
                return err(format!(
                    "a quantization table with precision {precision} and slot {slot}"
                ));
            }
            let size = 64 * (usize::from(precision) + 1);
            let Some(values) = rest.get(..size) else {
                return err("a short quantization table segment");
            };
            let mut q = [0i32; 64];
            for (k, &z) in ZIGZAG.iter().enumerate() {
                q[usize::from(z)] = if precision == 0 {
                    i32::from(values[k])
                } else {
                    i32::from(u16::from_be_bytes([values[2 * k], values[2 * k + 1]]))
                };
            }
            self.tables.qt[slot] = Some(q);
            body = &rest[size..];
        }
        Ok(())
    }

    fn sos(&mut self, body: &[u8]) -> Result<()> {
        let f = layout(&mut self.frame, self.adobe)?;
        let Some((&ns, rest)) = body.split_first() else {
            return err("an empty scan header");
        };
        let ns = usize::from(ns);
        if ns == 0 || ns > 4 || rest.len() != ns * 2 + 3 {
            return err(format!(
                "a scan header for {ns} components of {} bytes",
                body.len()
            ));
        }
        let mut parts: Vec<usize> = Vec::with_capacity(ns);
        for s in rest[..ns * 2].chunks_exact(2) {
            let Some(ci) = f.comps.iter().position(|c| c.id == s[0]) else {
                return err(format!(
                    "a scan names component {}, which the frame lacks",
                    s[0]
                ));
            };
            if parts.contains(&ci) {
                return err(format!("a scan names component {} twice", s[0]));
            }
            let (td, ta) = (usize::from(s[1] >> 4), usize::from(s[1] & 15));
            if td > 3 || ta > 3 {
                return err(format!("Huffman tables {td}/{ta} on component {}", s[0]));
            }
            f.comps[ci].dc = td;
            f.comps[ci].ac = ta;
            parts.push(ci);
        }
        let (ss, se, ah, al) = (
            rest[ns * 2],
            rest[ns * 2 + 1],
            rest[ns * 2 + 2] >> 4,
            rest[ns * 2 + 2] & 15,
        );
        let kind = if !f.progressive {
            Kind::Baseline
        } else {
            if ss > se || se > 63 || (ss == 0) != (se == 0) || (ss > 0 && ns != 1) {
                return err(format!(
                    "a progressive scan over {ss} to {se} on {ns} components"
                ));
            }
            if ah > 13 || al > 13 {
                return err(format!("successive approximation {ah}/{al}"));
            }
            match (ss, ah) {
                (0, 0) => Kind::DcFirst,
                (0, _) => Kind::DcRefine,
                (_, 0) => Kind::AcFirst,
                _ => Kind::AcRefine,
            }
        };
        if ns > 1
            && parts
                .iter()
                .map(|&ci| f.comps[ci].h * f.comps[ci].v)
                .sum::<usize>()
                > 10
        {
            return err("a scan with more than 10 blocks per MCU");
        }
        let t = &self.tables;
        let mut coded = Vec::with_capacity(ns);
        for &ci in &parts {
            let c = &f.comps[ci];
            let dc = || match t.dc[c.dc].as_deref() {
                Some(dc) => Ok(dc),
                None => err(format!("DC Huffman table {} is missing", c.dc)),
            };
            let ac = || match t.ac[c.ac].as_deref() {
                Some(ac) => Ok(ac),
                None => err(format!("AC Huffman table {} is missing", c.ac)),
            };
            let coding = match kind {
                Kind::Baseline => {
                    let (dc, ac) = (dc()?, ac()?);
                    let Some(q) = &t.qt[c.tq] else {
                        return err(format!("quantization table {} is missing", c.tq));
                    };
                    Coding::Baseline(dc, ac, q)
                }
                Kind::DcFirst => Coding::DcFirst(dc()?),
                Kind::DcRefine => Coding::DcRefine,
                Kind::AcFirst => Coding::AcFirst(ac()?),
                Kind::AcRefine => Coding::AcRefine(ac()?),
            };
            coded.push((ci, coding));
        }
        if self.scans >= MAX_SCANS {
            return err(format!("more than {MAX_SCANS} scans"));
        }
        if f.progressive {
            for &ci in &parts {
                let c = &mut f.comps[ci];
                if c.quant.is_none() {
                    c.quant = self.tables.qt[c.tq];
                }
            }
        }
        let mut bits = Bits::new(self.data, self.pos);
        bits.progressive(ss, se, al);
        scan(f, &coded, self.restart, &mut bits)?;
        bits.finish()?;
        self.pos = bits.resume_at();
        self.scans += 1;
        if kind == Kind::Baseline || al == 0 {
            let mask = (u64::MAX >> (63 - usize::from(se.min(63)))) & (u64::MAX << ss.min(63));
            let mask = if kind == Kind::Baseline {
                u64::MAX
            } else {
                mask
            };
            for &ci in &parts {
                f.comps[ci].done |= mask;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Planes> {
        let Some(mut f) = self.frame else {
            return err("no frame header");
        };
        if self.scans == 0 {
            return err("no start-of-scan marker, so no image data");
        }
        if f.progressive {
            for c in &mut f.comps {
                let Some(q) = c.quant.or(self.tables.qt[c.tq]) else {
                    return err(format!("quantization table {} is missing", c.tq));
                };
                let stride = c.stride();
                for (i, coefs) in c.coefs.iter().enumerate() {
                    let (bx, by) = (i % c.bw, i / c.bw);
                    let mut block = [0i32; 64];
                    for ((b, &v), &q) in block.iter_mut().zip(coefs).zip(&q) {
                        *b = i32::from(v).wrapping_mul(q);
                    }
                    if let Some(out) = c.plane.get_mut(by * 8 * stride + bx * 8..) {
                        idct::idct(&mut block, out, stride);
                    }
                }
            }
        }
        let rgb = f.comps.len() == 3
            && !self.jfif
            && match self.adobe {
                Some(transform) => transform == 0,
                None => f.comps.iter().map(|c| c.id).eq(*b"RGB"),
            };
        let (hmax, vmax) = (f.hmax, f.vmax);
        Ok(Planes {
            width: f.width,
            height: f.height,
            rgb,
            planes: f
                .comps
                .into_iter()
                .map(|c| Decoded {
                    width: c.cw,
                    height: c.ch,
                    fx: hmax / c.h,
                    fy: vmax / c.v,
                    stride: c.bw * 8,
                    data: c.plane,
                })
                .collect(),
        })
    }
}

fn layout(frame: &mut Option<Frame>, adobe: Option<u8>) -> Result<&mut Frame> {
    let Some(f) = frame.as_mut() else {
        return err("a scan before the frame header");
    };
    if f.ready {
        return Ok(f);
    }
    match f.comps.len() {
        1 | 3 => {}
        4 if adobe == Some(2) => return err("YCCK colour is not supported"),
        4 => return err("CMYK colour is not supported"),
        n => return err(format!("{n} colour components are not supported")),
    }
    let (hmax, vmax) = (f.hmax, f.vmax);
    for c in &mut f.comps {
        if hmax % c.h != 0 || vmax % c.v != 0 {
            return err(format!(
                "sampling {}x{} against {hmax}x{vmax} is not a whole ratio",
                c.h, c.v
            ));
        }
        c.bw = f.mcux * c.h;
        c.bh = f.mcuy * c.v;
        c.cw = (f.width * c.h).div_ceil(hmax);
        c.ch = (f.height * c.v).div_ceil(vmax);
        c.plane = zeroed(c.bw * 8 * c.bh * 8, 0u8)?;
        if f.progressive {
            c.coefs = zeroed(c.bw * c.bh, [0i16; 64])?;
        }
    }
    f.ready = true;
    Ok(f)
}

fn scan(f: &mut Frame, parts: &[(usize, Coding)], restart: usize, bits: &mut Bits) -> Result<()> {
    let (cols, rows) = match parts {
        [(ci, _)] => {
            let c = &f.comps[*ci];
            (c.cw.div_ceil(8), c.ch.div_ceil(8))
        }
        _ => (f.mcux, f.mcuy),
    };
    for &(ci, _) in parts {
        f.comps[ci].pred = 0;
    }
    for n in 0..cols * rows {
        if restart > 0 && n > 0 && n % restart == 0 {
            bits.restart()?;
            for &(ci, _) in parts {
                f.comps[ci].pred = 0;
            }
        }
        let (ux, uy) = (n % cols, n / cols);
        if let [(ci, coding)] = parts {
            block(&mut f.comps[*ci], *coding, ux, uy, bits)?;
            continue;
        }
        for &(ci, coding) in parts {
            let c = &mut f.comps[ci];
            for by in 0..c.v {
                for bx in 0..c.h {
                    block(c, coding, ux * c.h + bx, uy * c.v + by, bits)?;
                }
            }
        }
    }
    Ok(())
}

fn coefs(all: &mut [[i16; 64]], i: usize) -> Result<&mut [i16; 64]> {
    match all.get_mut(i) {
        Some(block) => Ok(block),
        None => err("a block outside the frame"),
    }
}

fn block(c: &mut Component, coding: Coding, bx: usize, by: usize, bits: &mut Bits) -> Result<()> {
    let i = by * c.bw + bx;
    match coding {
        Coding::Baseline(dc, ac, q) => {
            let mut b = [0i32; 64];
            bits.baseline(dc, ac, q, &mut b, &mut c.pred)?;
            let stride = c.stride();
            if let Some(out) = c.plane.get_mut(by * 8 * stride + bx * 8..) {
                idct::idct(&mut b, out, stride);
            }
            Ok(())
        }
        Coding::DcFirst(dc) => bits.dc_first(dc, coefs(&mut c.coefs, i)?, &mut c.pred),
        Coding::DcRefine => bits.dc_refine(coefs(&mut c.coefs, i)?),
        Coding::AcFirst(ac) => bits.ac_first(ac, coefs(&mut c.coefs, i)?),
        Coding::AcRefine(ac) => bits.ac_refine(ac, coefs(&mut c.coefs, i)?),
    }
}
