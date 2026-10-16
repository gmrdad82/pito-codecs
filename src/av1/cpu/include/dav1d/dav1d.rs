use std::ffi::{c_int, c_uint};

use crate::av1::cpu::flags::bitflags;

use crate::av1::cpu::error::Rav1dError;
use crate::av1::cpu::in_range::InRange;
use crate::av1::cpu::include::dav1d::picture::{Dav1dPicAllocator, Rav1dPicAllocator};
pub use crate::av1::cpu::log::Dav1dLogger;
use crate::av1::cpu::log::Rav1dLogger;
use crate::av1::cpu::validate_input;

pub const RAV1D_MAX_THREADS: usize = 256;
pub const RAV1D_MAX_FRAME_DELAY: usize = 256;

pub const DAV1D_MAX_THREADS: c_int = RAV1D_MAX_THREADS as _;
pub const DAV1D_MAX_FRAME_DELAY: c_int = RAV1D_MAX_FRAME_DELAY as _;

pub type Dav1dInloopFilterType = c_uint;

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Rav1dInloopFilterType: u8 {
        const DEBLOCK = 1 << 1;
        const CDEF = 1 << 2;
        const RESTORATION = 1 << 3;
    }
}

impl From<Rav1dInloopFilterType> for Dav1dInloopFilterType {
    fn from(value: Rav1dInloopFilterType) -> Self {
        value.bits().into()
    }
}

impl From<Dav1dInloopFilterType> for Rav1dInloopFilterType {
    fn from(value: Dav1dInloopFilterType) -> Self {
        Self::from_bits_retain(value as u8)
    }
}

pub type Dav1dDecodeFrameType = c_uint;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Rav1dDecodeFrameType {
    #[default]
    All = 0,

    Reference = 1,

    Intra = 2,

    Key = 3,
}

impl Rav1dDecodeFrameType {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::All as usize {
            return Some(Self::All);
        }
        if v == Self::Reference as usize {
            return Some(Self::Reference);
        }
        if v == Self::Intra as usize {
            return Some(Self::Intra);
        }
        if v == Self::Key as usize {
            return Some(Self::Key);
        }
        None
    }
}

impl From<Rav1dDecodeFrameType> for Dav1dDecodeFrameType {
    fn from(value: Rav1dDecodeFrameType) -> Self {
        value as Self
    }
}

impl TryFrom<Dav1dDecodeFrameType> for Rav1dDecodeFrameType {
    type Error = Rav1dError;

    fn try_from(value: Dav1dDecodeFrameType) -> Result<Self, Self::Error> {
        Self::from_repr(value as usize).ok_or(Rav1dError::InvalidArgument)
    }
}

pub type Dav1dEventFlags = c_uint;

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub(crate) struct Rav1dEventFlags: u8 {

        const NEW_SEQUENCE = 1 << 0;

        const NEW_OP_PARAMS_INFO = 1 << 1;
    }
}

impl From<Rav1dEventFlags> for Dav1dEventFlags {
    fn from(value: Rav1dEventFlags) -> Self {
        value.bits().into()
    }
}

impl From<Dav1dEventFlags> for Rav1dEventFlags {
    fn from(value: Dav1dEventFlags) -> Self {
        Self::from_bits_retain(value as u8)
    }
}

#[repr(C)]
pub struct Dav1dSettings {
    pub n_threads: c_int,
    pub max_frame_delay: c_int,
    pub apply_grain: c_int,
    pub operating_point: c_int,
    pub all_layers: c_int,
    pub frame_size_limit: c_uint,
    pub allocator: Dav1dPicAllocator,
    pub logger: Dav1dLogger,
    pub strict_std_compliance: c_int,
    pub output_invisible_frames: c_int,
    pub inloop_filters: Dav1dInloopFilterType,
    pub decode_frame_type: Dav1dDecodeFrameType,
    pub reserved: [u8; 16],
}

#[repr(C)]
pub(crate) struct Rav1dSettings {
    pub n_threads: InRange<u16, 0, { RAV1D_MAX_THREADS as _ }>,
    pub max_frame_delay: InRange<u16, 0, { RAV1D_MAX_FRAME_DELAY as _ }>,
    pub apply_grain: bool,
    pub operating_point: InRange<u8, 0, 31>,
    pub all_layers: bool,
    pub frame_size_limit: c_uint,
    pub allocator: Rav1dPicAllocator,
    pub logger: Option<Rav1dLogger>,
    pub strict_std_compliance: bool,
    pub output_invisible_frames: bool,
    pub inloop_filters: Rav1dInloopFilterType,
    pub decode_frame_type: Rav1dDecodeFrameType,
}

impl TryFrom<Dav1dSettings> for Rav1dSettings {
    type Error = Rav1dError;

    fn try_from(value: Dav1dSettings) -> Result<Self, Self::Error> {
        let Dav1dSettings {
            n_threads,
            max_frame_delay,
            apply_grain,
            operating_point,
            all_layers,
            frame_size_limit,
            allocator,
            logger,
            strict_std_compliance,
            output_invisible_frames,
            inloop_filters,
            decode_frame_type,
            reserved: _,
        } = value;
        validate_input!((
            (0..=DAV1D_MAX_THREADS).contains(&n_threads),
            Rav1dError::InvalidArgument
        ))?;
        validate_input!((
            (0..=DAV1D_MAX_FRAME_DELAY).contains(&max_frame_delay),
            Rav1dError::InvalidArgument
        ))?;
        validate_input!((
            (0..32).contains(&operating_point),
            Rav1dError::InvalidArgument
        ))?;
        Ok(Self {
            n_threads: InRange::new(n_threads.try_into().unwrap()).unwrap(),
            max_frame_delay: InRange::new(max_frame_delay.try_into().unwrap()).unwrap(),
            apply_grain: apply_grain != 0,
            operating_point: InRange::new(operating_point.try_into().unwrap()).unwrap(),
            all_layers: all_layers != 0,
            frame_size_limit,
            allocator: allocator.try_into()?,
            logger: logger.into(),
            strict_std_compliance: strict_std_compliance != 0,
            output_invisible_frames: output_invisible_frames != 0,
            inloop_filters: inloop_filters.into(),
            decode_frame_type: decode_frame_type.try_into()?,
        })
    }
}

impl From<Rav1dSettings> for Dav1dSettings {
    fn from(value: Rav1dSettings) -> Self {
        let Rav1dSettings {
            n_threads,
            max_frame_delay,
            apply_grain,
            operating_point,
            all_layers,
            frame_size_limit,
            allocator,
            logger,
            strict_std_compliance,
            output_invisible_frames,
            inloop_filters,
            decode_frame_type,
        } = value;
        Self {
            n_threads: n_threads.get().into(),
            max_frame_delay: max_frame_delay.get().into(),
            apply_grain: apply_grain as c_int,
            operating_point: operating_point.get().into(),
            all_layers: all_layers as c_int,
            frame_size_limit,
            allocator: allocator.into(),
            logger: logger.into(),
            strict_std_compliance: strict_std_compliance as c_int,
            output_invisible_frames: output_invisible_frames as c_int,
            inloop_filters: inloop_filters.into(),
            decode_frame_type: decode_frame_type.into(),
            reserved: Default::default(),
        }
    }
}
