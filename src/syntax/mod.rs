#![allow(clippy::needless_range_loop)]

pub mod bits;
pub mod error;
#[cfg(any(feature = "h264", feature = "gpu"))]
pub mod h264;
#[cfg(any(feature = "hevc", feature = "gpu"))]
pub mod hevc;
