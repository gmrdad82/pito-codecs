use std::error::Error;
use std::ffi::{c_int, c_uint};
use std::fmt::{self, Display, Formatter};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[non_exhaustive]
pub enum Rav1dError {
    Other = 1,

    NoEntity = 2 as u8,

    TryAgain = 11 as u8,

    OutOfMemory = 12 as u8,

    InvalidArgument = 22 as u8,

    OutOfRange = 34 as u8,

    UnsupportedBitstream = 92 as u8,
}

impl Rav1dError {
    pub const fn from_repr(v: u8) -> Option<Self> {
        if v == Self::Other as u8 {
            return Some(Self::Other);
        }
        if v == Self::NoEntity as u8 {
            return Some(Self::NoEntity);
        }
        if v == Self::TryAgain as u8 {
            return Some(Self::TryAgain);
        }
        if v == Self::OutOfMemory as u8 {
            return Some(Self::OutOfMemory);
        }
        if v == Self::InvalidArgument as u8 {
            return Some(Self::InvalidArgument);
        }
        if v == Self::OutOfRange as u8 {
            return Some(Self::OutOfRange);
        }
        if v == Self::UnsupportedBitstream as u8 {
            return Some(Self::UnsupportedBitstream);
        }
        None
    }
}

impl Rav1dError {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Rav1dError::TryAgain => "Try again",
            Rav1dError::InvalidArgument => "Invalid argument",
            Rav1dError::OutOfMemory => "Not enough memory available",
            Rav1dError::UnsupportedBitstream => "Unsupported bitstream",
            Rav1dError::Other => "Other error",
            Rav1dError::NoEntity => "No sequence header found",
            Rav1dError::OutOfRange => "Out of range",
        }
    }
}

impl Display for Rav1dError {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        write!(fmt, "{}", self.as_str())
    }
}

impl Error for Rav1dError {}

pub type Rav1dResult<T = ()> = Result<T, Rav1dError>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct Dav1dResult(pub c_int);

impl From<Rav1dResult> for Dav1dResult {
    #[inline]
    fn from(value: Rav1dResult) -> Self {
        Dav1dResult(
            -(match value {
                Ok(()) => 0,
                Err(e) => e as c_int,
            }),
        )
    }
}

impl From<Rav1dResult<c_uint>> for Dav1dResult {
    #[inline]
    fn from(value: Rav1dResult<c_uint>) -> Self {
        Dav1dResult(match value {
            Ok(value) => value as c_int,
            Err(e) => e as c_int,
        })
    }
}

impl TryFrom<Dav1dResult> for Rav1dResult {
    type Error = Dav1dResult;

    #[inline]
    fn try_from(value: Dav1dResult) -> Result<Self, Self::Error> {
        match value.0 {
            0 => Ok(Ok(())),
            e => {
                let e = (-e).try_into().map_err(|_| value)?;
                let e = Rav1dError::from_repr(e).ok_or(value)?;
                Ok(Err(e))
            }
        }
    }
}
