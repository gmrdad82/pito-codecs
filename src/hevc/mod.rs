#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod cabac;
mod decoder;
pub(crate) mod filter;
mod grid;
mod hvcc;
mod inter;
mod intra;
mod mvpred;
mod pel;
mod picture;
mod simd;
mod slicedec;
#[rustfmt::skip]
#[allow(dead_code)]
mod spec_tables;
#[cfg(test)]
mod synth_tests;
mod tables;
#[cfg(test)]
mod tests;
mod transform;

pub use decoder::Decoder;
pub use hvcc::Hvcc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorInfo {
    pub full_range: bool,
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub sar: (u16, u16),
}
