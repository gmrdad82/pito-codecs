use crate::vp9::bits::BitError;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Bitstream(BitError),
    Invalid(String),
    Unsupported(String),
    MissingReference(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Bitstream(e) => write!(f, "bitstream error: {e}"),
            Error::Invalid(s) => write!(f, "invalid bitstream: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::MissingReference(s) => write!(f, "missing reference frame: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<BitError> for Error {
    fn from(e: BitError) -> Self {
        Error::Bitstream(e)
    }
}

impl From<Error> for crate::media::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Unsupported(s) => crate::media::Error::Unsupported(format!("vp9: {s}")),
            other => crate::media::Error::Invalid(format!("vp9: {other}")),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::vp9::error::Error::Invalid(format!($($arg)+)));
        }
    };
}
pub(crate) use ensure;
