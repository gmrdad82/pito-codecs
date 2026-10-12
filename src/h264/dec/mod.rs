mod cabac;
mod cabac_mb;
#[rustfmt::skip]
pub(super) mod cabac_tables;
mod cavlc;
#[rustfmt::skip]
pub(super) mod cavlc_tables;
mod deblock;
mod decoder;
mod inter;
mod intra;
pub(super) mod mbtypes;
mod picture;
#[cfg(target_arch = "x86_64")]
mod simd;
mod slicedec;

pub(super) mod tables;
mod transform;

pub use crate::syntax::error::Error;
pub use decoder::Decoder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
    pub pts: i64,
    pub color: ColorInfo,
}
