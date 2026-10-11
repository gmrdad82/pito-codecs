#![allow(
    clippy::precedence,
    clippy::int_plus_one,
    clippy::needless_range_loop,
    clippy::too_many_arguments
)]

pub(crate) mod celt;
mod decoder;
pub mod enc;
mod header;
mod multistream;
pub mod ogg;
pub mod packet;
pub(crate) mod range;
pub(crate) mod silk;

pub use decoder::{SAMPLE_RATES, StreamDecoder};
pub use header::OpusHead;
pub use multistream::Decoder;
pub use packet::{Bandwidth, Mode, Packet, Toc};

#[cfg(test)]
mod tests;
