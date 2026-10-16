mod decode;
mod encode;
mod lzw;
mod quant;

pub use decode::{Animation, Decoder, Disposal, Frame, decode};
pub use encode::{
    BEST_TOLERANCE, EncodeOptions, Encoder, PaletteMode, REDRAW_PERCENT, TOLERANCE, delay_cs,
    encode,
};
pub use quant::{Dither, Histogram, Palette};

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotGif,
    Truncated,
    Invalid(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotGif => f.write_str("not a GIF file"),
            Error::Truncated => f.write_str("truncated GIF"),
            Error::Invalid(s) => write!(f, "invalid GIF: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for crate::media::Error {
    fn from(e: Error) -> Self {
        crate::media::Error::Invalid(format!("gif: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests;
