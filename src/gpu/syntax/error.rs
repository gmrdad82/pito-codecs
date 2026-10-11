use std::fmt;

use crate::gpu::syntax::bits::BitError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Bitstream(BitError),
    Invalid(String),
    Unsupported(String),
    MissingParameterSet(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Bitstream(e) => write!(f, "bitstream error: {e}"),
            Error::Invalid(s) => write!(f, "invalid bitstream: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::MissingParameterSet(s) => write!(f, "missing parameter set: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<BitError> for Error {
    fn from(e: BitError) -> Self {
        Error::Bitstream(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

pub(crate) fn unsupported<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Unsupported(msg.into()))
}

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::gpu::syntax::error::Error::Invalid(format!($($arg)+)));
        }
    };
}
pub(crate) use ensure;
