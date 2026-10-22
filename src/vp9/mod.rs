#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod alpha;
#[cfg(test)]
mod alpha_edges;
pub(crate) mod bits;
pub(crate) mod boolcoder;
pub(crate) mod decoder;
pub mod enc;
pub(crate) mod error;
pub(crate) mod frame;
pub(crate) mod header;
pub(crate) mod inter;
pub(crate) mod intra;
pub(crate) mod loopfilter;
pub(crate) mod probs;
#[rustfmt::skip]
#[allow(dead_code)]
pub(crate) mod spec_tables;
pub(crate) mod tables;
#[cfg(test)]
mod tests;
pub(crate) mod tile;
pub(crate) mod transform;
#[rustfmt::skip]
#[allow(dead_code, clippy::all)]
pub(crate) mod transform_gen;

pub use error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyframeInfo {
    pub profile: u8,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub bit_depth: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub color: ColorInfo,
}

pub fn keyframe_info(chunk: &[u8]) -> Option<KeyframeInfo> {
    let first = header::split_superframe(chunk)
        .into_iter()
        .find(|f| !f.is_empty())?;
    let mut st = header::HeaderState::default();
    let h = header::parse_uncompressed(first, &mut st, &[None; 8]).ok()?;
    if h.show_existing_frame || h.frame_type != header::KEY_FRAME {
        return None;
    }
    Some(KeyframeInfo {
        profile: h.profile,
        width: h.width,
        height: h.height,
        render_width: h.render_width,
        render_height: h.render_height,
        bit_depth: h.color.bit_depth as u32,
        subsampling_x: h.color.subsampling_x,
        subsampling_y: h.color.subsampling_y,
        color: ColorInfo {
            color_space: h.color.color_space,
            full_range: h.color.color_range,
        },
    })
}

pub fn is_keyframe(chunk: &[u8]) -> bool {
    keyframe_info(chunk).is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub color_space: u8,
    pub full_range: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plane {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl Plane {
    pub fn len(&self) -> usize {
        match self {
            Plane::U8(v) => v.len(),
            Plane::U16(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn as_u8(&self) -> Option<&[u8]> {
        match self {
            Plane::U8(v) => Some(v),
            Plane::U16(_) => None,
        }
    }
    pub fn as_u16(&self) -> Option<&[u16]> {
        match self {
            Plane::U8(_) => None,
            Plane::U16(v) => Some(v),
        }
    }
    pub fn get(&self, i: usize) -> u16 {
        match self {
            Plane::U8(v) => v[i] as u16,
            Plane::U16(v) => v[i],
        }
    }
    pub fn to_le_bytes(&self) -> Vec<u8> {
        match self {
            Plane::U8(v) => v.clone(),
            Plane::U16(v) => v.iter().flat_map(|s| s.to_le_bytes()).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub bit_depth: u32,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub pts: i64,
    pub key: bool,
    pub intra_only: bool,
    pub color: ColorInfo,
    pub render_width: u32,
    pub render_height: u32,
    pub draft: bool,
}

pub use alpha::{AlphaDecoder, AlphaEncoder};
pub use decoder::{DecodeStats, Decoder};
pub use enc::{Encoder, EncoderConfig, Rate};
