use crate::hevc::bits::BitError;
use std::fmt;

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

impl From<BitError> for Error {
    fn from(e: BitError) -> Self {
        Error::Bitstream(e)
    }
}

impl From<Error> for crate::media::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Unsupported(s) => crate::media::Error::Unsupported(format!("hevc: {s}")),
            other => crate::media::Error::Invalid(format!("hevc: {other}")),
        }
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
            return Err($crate::hevc::error::Error::Invalid(format!($($arg)+)));
        }
    };
}
pub(crate) use ensure;
