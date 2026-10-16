use std::sync::Arc;

use crate::av1::cpu::c_box::CRef;
use crate::av1::cpu::error::Rav1dError;
use crate::av1::cpu::in_range::InRange;
use crate::av1::cpu::include::common::bitdepth::BitDepth8;
use crate::av1::cpu::include::dav1d::data::Rav1dData;
use crate::av1::cpu::include::dav1d::dav1d::Rav1dSettings;
use crate::av1::cpu::include::dav1d::headers::Rav1dPixelLayout;
use crate::av1::cpu::include::dav1d::picture::Rav1dPicture;
use crate::av1::cpu::internal::Rav1dContext;
use crate::av1::cpu::obu::rav1d_parse_sequence_header;
use crate::av1::cpu::{rav1d_close, rav1d_flush, rav1d_get_picture, rav1d_open, rav1d_send_data};
use crate::media::{Error, Frame, Packet, PixelFormat, Plane, Result, TimeBase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    I400,
    I420,
    I422,
    I444,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceInfo {
    pub profile: u8,
    pub max_width: u32,
    pub max_height: u32,
    pub bit_depth: u8,
    pub layout: Layout,
    pub matrix: u8,
    pub full_range: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub layout: Layout,
    pub pts: i64,
    pub planes: [Plane; 3],
}

pub struct Decoder {
    ctx: Option<Arc<Rav1dContext>>,
    threads: usize,
    time_base: TimeBase,
    apply_grain: bool,
    sequence: Option<SequenceInfo>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn err(e: Rav1dError) -> Error {
    Error::Invalid(format!("av1: {e}"))
}

fn layout_of(l: Rav1dPixelLayout) -> Layout {
    match l {
        Rav1dPixelLayout::I400 => Layout::I400,
        Rav1dPixelLayout::I420 => Layout::I420,
        Rav1dPixelLayout::I422 => Layout::I422,
        Rav1dPixelLayout::I444 => Layout::I444,
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self::with_threads(crate::media::default_threads())
    }

    pub fn with_threads(threads: usize) -> Self {
        Self {
            ctx: None,
            threads: threads.clamp(1, 256),
            time_base: TimeBase::new(1, 90000),
            apply_grain: true,
            sequence: None,
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub fn set_time_base(&mut self, time_base: TimeBase) {
        self.time_base = time_base;
    }

    pub fn set_film_grain(&mut self, on: bool) {
        if self.apply_grain != on {
            self.close();
            self.apply_grain = on;
        }
    }

    pub fn sequence(&self) -> Option<SequenceInfo> {
        self.sequence
    }

    pub fn configure(&mut self, av1c: &[u8]) -> Result<Option<SequenceInfo>> {
        if av1c.len() < 4 || av1c[0] & 0x7f != 1 {
            return Err(Error::Invalid("av1: av1C box is not version 1".into()));
        }
        let obus = &av1c[4..];
        if obus.is_empty() {
            return Ok(None);
        }
        let seq = rav1d_parse_sequence_header(obus).map_err(err)?;
        let info = SequenceInfo {
            profile: seq.profile as u8,
            max_width: seq.max_width as u32,
            max_height: seq.max_height as u32,
            bit_depth: 8 + 2 * seq.hbd,
            layout: layout_of(seq.layout),
            matrix: seq.mtrx as u8,
            full_range: seq.color_range as u8 != 0,
        };
        self.sequence = Some(info);
        Ok(Some(info))
    }

    fn context(&mut self) -> Result<Arc<Rav1dContext>> {
        if let Some(c) = &self.ctx {
            return Ok(Arc::clone(c));
        }
        let s = Rav1dSettings {
            n_threads: InRange::new(self.threads as u16).unwrap_or_default(),
            apply_grain: self.apply_grain,
            frame_size_limit: 16384 * 16384,
            logger: None,
            ..Default::default()
        };
        let c = rav1d_open(&s).map_err(err)?;
        self.ctx = Some(Arc::clone(&c));
        Ok(c)
    }

    pub fn decode_pictures(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let c = self.context()?;
        let mut out = Vec::new();
        if data.is_empty() {
            return Ok(out);
        }
        let mut input = Rav1dData::wrap(CRef::Box(data.into())).map_err(err)?;
        input.m.timestamp = pts;
        loop {
            match rav1d_send_data(&c, &mut input) {
                Ok(()) | Err(Rav1dError::TryAgain) => {}
                Err(e) => {
                    self.reset();
                    return Err(err(e));
                }
            }
            loop {
                let mut pic = Rav1dPicture::default();
                match rav1d_get_picture(&c, &mut pic) {
                    Ok(()) => out.push(picture_of(&pic)?),
                    Err(Rav1dError::TryAgain) => break,
                    Err(e) => {
                        self.reset();
                        return Err(err(e));
                    }
                }
            }
            if input.data.is_none() {
                break;
            }
        }
        Ok(out)
    }

    pub fn flush_pictures(&mut self) -> Result<Vec<Picture>> {
        let mut out = Vec::new();
        let Some(c) = self.ctx.clone() else {
            return Ok(out);
        };
        loop {
            let mut pic = Rav1dPicture::default();
            match rav1d_get_picture(&c, &mut pic) {
                Ok(()) => out.push(picture_of(&pic)?),
                Err(Rav1dError::TryAgain) => break,
                Err(e) => {
                    self.reset();
                    return Err(err(e));
                }
            }
        }
        rav1d_flush(&c);
        Ok(out)
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        let tb = self.time_base;
        self.decode_pictures(&packet.data, packet.pts)?
            .into_iter()
            .map(|p| frame_of(p, tb))
            .collect()
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        let tb = self.time_base;
        self.flush_pictures()?
            .into_iter()
            .map(|p| frame_of(p, tb))
            .collect()
    }

    pub fn reset(&mut self) {
        if let Some(c) = &self.ctx {
            rav1d_flush(c);
        }
    }

    fn close(&mut self) {
        if let Some(c) = self.ctx.take() {
            rav1d_close(&c);
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.close();
    }
}

fn picture_of(pic: &Rav1dPicture) -> Result<Picture> {
    let Some(data) = &pic.data else {
        return Err(Error::Invalid("av1: picture without data".into()));
    };
    let width = pic.p.w.max(0) as usize;
    let height = pic.p.h.max(0) as usize;
    let bit_depth = pic.p.bpc;
    let bps = if bit_depth > 8 { 2 } else { 1 };
    let layout = layout_of(pic.p.layout);
    let (cw, ch) = match layout {
        Layout::I400 => (0, 0),
        Layout::I420 => (width.div_ceil(2), height.div_ceil(2)),
        Layout::I422 => (width.div_ceil(2), height),
        Layout::I444 => (width, height),
    };
    let plane = |i: usize, w: usize, h: usize| -> Result<Plane> {
        let row = w * bps;
        let mut out = vec![0u8; row * h];
        if row > 0 && h > 0 {
            let stride = pic.stride[(i != 0) as usize];
            let comp = &data.data[i];
            let src = comp.slice::<BitDepth8, _>(..);
            let stride = usize::try_from(stride)
                .map_err(|_| Error::Invalid("av1: negative picture stride".into()))?;
            for (y, dst) in out.chunks_exact_mut(row).enumerate() {
                let s = src
                    .get(y * stride..y * stride + row)
                    .ok_or_else(|| Error::Invalid("av1: picture plane too small".into()))?;
                dst.copy_from_slice(s);
            }
        }
        Ok(Plane {
            data: out,
            stride: row,
            width: w,
            height: h,
        })
    };
    Ok(Picture {
        width: width as u32,
        height: height as u32,
        bit_depth: bit_depth as u8,
        layout,
        pts: pic.m.timestamp,
        planes: [
            plane(0, width, height)?,
            plane(1, cw, ch)?,
            plane(2, cw, ch)?,
        ],
    })
}

fn frame_of(p: Picture, time_base: TimeBase) -> Result<Frame> {
    let format = match (p.layout, p.bit_depth) {
        (Layout::I420, 8) => PixelFormat::Yuv420p8,
        (Layout::I420, 10) => PixelFormat::Yuv420p10,
        (l, b) => {
            return Err(Error::Unsupported(format!(
                "av1: {b}-bit {l:?} as a 4:2:0 frame"
            )));
        }
    };
    Ok(Frame {
        format,
        width: p.width,
        height: p.height,
        pts: p.pts,
        time_base,
        planes: p.planes,
    })
}
