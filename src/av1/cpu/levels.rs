#![deny(unsafe_code)]

use std::fmt::{Display, Formatter};
use std::ops::Neg;
use std::{fmt, mem};

use crate::av1::cpu::enums::EnumCount;
use crate::av1::cpu::flags::bitflags;
use crate::av1::cpu::pod::{AsBytes, FromBytes};

use crate::av1::cpu::align::ArrayDefault;
use crate::av1::cpu::enum_map::{DefaultValue, EnumKey};
use crate::av1::cpu::in_range::InRange;
use crate::av1::cpu::include::dav1d::headers::Rav1dFilterMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObuMetaType {
    HdrCll = 1,
    HdrMdcv = 2,
    Scalability = 3,
    ItutT35 = 4,
    Timecode = 5,
}

impl ObuMetaType {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::HdrCll as usize {
            return Some(Self::HdrCll);
        }
        if v == Self::HdrMdcv as usize {
            return Some(Self::HdrMdcv);
        }
        if v == Self::Scalability as usize {
            return Some(Self::Scalability);
        }
        if v == Self::ItutT35 as usize {
            return Some(Self::ItutT35);
        }
        if v == Self::Timecode as usize {
            return Some(Self::Timecode);
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug)]
pub enum TxfmSize {
    #[default]
    S4x4 = 0,
    S8x8 = 1,
    S16x16 = 2,
    S32x32 = 3,
    S64x64 = 4,

    R4x8 = 5,
    R8x4 = 6,
    R8x16 = 7,
    R16x8 = 8,
    R16x32 = 9,
    R32x16 = 10,
    R32x64 = 11,
    R64x32 = 12,
    R4x16 = 13,
    R16x4 = 14,
    R8x32 = 15,
    R32x8 = 16,
    R16x64 = 17,
    R64x16 = 18,
}

impl crate::av1::cpu::enums::EnumCount for TxfmSize {
    const COUNT: usize = 19;
}

impl TxfmSize {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::S4x4 as usize {
            return Some(Self::S4x4);
        }
        if v == Self::S8x8 as usize {
            return Some(Self::S8x8);
        }
        if v == Self::S16x16 as usize {
            return Some(Self::S16x16);
        }
        if v == Self::S32x32 as usize {
            return Some(Self::S32x32);
        }
        if v == Self::S64x64 as usize {
            return Some(Self::S64x64);
        }
        if v == Self::R4x8 as usize {
            return Some(Self::R4x8);
        }
        if v == Self::R8x4 as usize {
            return Some(Self::R8x4);
        }
        if v == Self::R8x16 as usize {
            return Some(Self::R8x16);
        }
        if v == Self::R16x8 as usize {
            return Some(Self::R16x8);
        }
        if v == Self::R16x32 as usize {
            return Some(Self::R16x32);
        }
        if v == Self::R32x16 as usize {
            return Some(Self::R32x16);
        }
        if v == Self::R32x64 as usize {
            return Some(Self::R32x64);
        }
        if v == Self::R64x32 as usize {
            return Some(Self::R64x32);
        }
        if v == Self::R4x16 as usize {
            return Some(Self::R4x16);
        }
        if v == Self::R16x4 as usize {
            return Some(Self::R16x4);
        }
        if v == Self::R8x32 as usize {
            return Some(Self::R8x32);
        }
        if v == Self::R32x8 as usize {
            return Some(Self::R32x8);
        }
        if v == Self::R16x64 as usize {
            return Some(Self::R16x64);
        }
        if v == Self::R64x16 as usize {
            return Some(Self::R64x16);
        }
        None
    }
}

impl TxfmSize {
    pub const NUM_SQUARE: usize = Self::S64x64 as usize + 1;
    pub const _NUM_RECT: usize = Self::COUNT;
}

impl DefaultValue for TxfmSize {
    const DEFAULT: Self = Self::S4x4;
}

impl ArrayDefault for TxfmSize {
    fn default() -> Self {
        Default::default()
    }
}

impl TxfmSize {
    pub const fn from_wh(w: usize, h: usize) -> Self {
        use TxfmSize::*;
        match (w, h) {
            (4, 4) => S4x4,
            (8, 8) => S8x8,
            (16, 16) => S16x16,
            (32, 32) => S32x32,
            (64, 64) => S64x64,

            (4, 8) => R4x8,
            (8, 4) => R8x4,
            (8, 16) => R8x16,
            (16, 8) => R16x8,
            (16, 32) => R16x32,
            (32, 16) => R32x16,
            (32, 64) => R32x64,
            (64, 32) => R64x32,
            (4, 16) => R4x16,
            (16, 4) => R16x4,
            (8, 32) => R8x32,
            (32, 8) => R32x8,
            (16, 64) => R16x64,
            (64, 16) => R64x16,
            _ => {
                debug_assert!(false);
                DefaultValue::DEFAULT
            }
        }
    }

    pub const fn is_rect(self) -> bool {
        self as u8 >= Self::R4x8 as u8
    }
}

#[repr(u8)]
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockLevel {
    #[default]
    Bl128x128 = 0,
    Bl64x64 = 1,
    Bl32x32 = 2,
    Bl16x16 = 3,
    Bl8x8 = 4,
}

impl crate::av1::cpu::enums::EnumCount for BlockLevel {
    const COUNT: usize = 5;
}

impl BlockLevel {
    pub const fn decrease(self) -> Option<Self> {
        match self {
            BlockLevel::Bl8x8 => None,
            BlockLevel::Bl16x16 => Some(BlockLevel::Bl8x8),
            BlockLevel::Bl32x32 => Some(BlockLevel::Bl16x16),
            BlockLevel::Bl64x64 => Some(BlockLevel::Bl32x32),
            BlockLevel::Bl128x128 => Some(BlockLevel::Bl64x64),
        }
    }
}

pub type TxfmType = u8;
pub const N_TX_TYPES_PLUS_LL: usize = 17;
pub const WHT_WHT: TxfmType = 16;
pub const _N_TX_TYPES: usize = 16;
pub const H_FLIPADST: TxfmType = 15;
pub const V_FLIPADST: TxfmType = 14;
pub const H_ADST: TxfmType = 13;
pub const V_ADST: TxfmType = 12;
pub const H_DCT: TxfmType = 11;
pub const V_DCT: TxfmType = 10;
pub const IDTX: TxfmType = 9;
pub const FLIPADST_ADST: TxfmType = 8;
pub const ADST_FLIPADST: TxfmType = 7;
pub const FLIPADST_FLIPADST: TxfmType = 6;
pub const DCT_FLIPADST: TxfmType = 5;
pub const FLIPADST_DCT: TxfmType = 4;
pub const ADST_ADST: TxfmType = 3;
pub const DCT_ADST: TxfmType = 2;
pub const ADST_DCT: TxfmType = 1;
pub const DCT_DCT: TxfmType = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TxClass {
    TwoD,
    H,
    V,
}

impl TxClass {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::TwoD as usize {
            return Some(Self::TwoD);
        }
        if v == Self::H as usize {
            return Some(Self::H);
        }
        if v == Self::V as usize {
            return Some(Self::V);
        }
        None
    }
}

pub type IntraPredMode = u8;
pub const FILTER_PRED: IntraPredMode = 13;
pub const Z3_PRED: IntraPredMode = 8;
pub const Z2_PRED: IntraPredMode = 7;
pub const Z1_PRED: IntraPredMode = 6;
pub const DC_128_PRED: IntraPredMode = 5;
pub const TOP_DC_PRED: IntraPredMode = 4;
pub const LEFT_DC_PRED: IntraPredMode = 3;
pub const N_IMPL_INTRA_PRED_MODES: usize = 14;
pub const N_UV_INTRA_PRED_MODES: usize = 14;
pub const CFL_PRED: IntraPredMode = 13;
pub const N_INTRA_PRED_MODES: usize = 13;
pub const PAETH_PRED: IntraPredMode = 12;
pub const SMOOTH_H_PRED: IntraPredMode = 11;
pub const SMOOTH_V_PRED: IntraPredMode = 10;
pub const SMOOTH_PRED: IntraPredMode = 9;
pub const VERT_LEFT_PRED: IntraPredMode = 8;
pub const HOR_UP_PRED: IntraPredMode = 7;
pub const HOR_DOWN_PRED: IntraPredMode = 6;
pub const VERT_RIGHT_PRED: IntraPredMode = 5;
pub const DIAG_DOWN_RIGHT_PRED: IntraPredMode = 4;
pub const DIAG_DOWN_LEFT_PRED: IntraPredMode = 3;
pub const HOR_PRED: IntraPredMode = 2;
pub const VERT_PRED: IntraPredMode = 1;
pub const DC_PRED: IntraPredMode = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterIntraPredMode {
    #[default]
    Dc = 0,
    Vert = 1,
    Hor = 2,
    Smooth = 3,
}

impl crate::av1::cpu::enums::EnumCount for InterIntraPredMode {
    const COUNT: usize = 4;
}

impl InterIntraPredMode {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::Dc as usize {
            return Some(Self::Dc);
        }
        if v == Self::Vert as usize {
            return Some(Self::Vert);
        }
        if v == Self::Hor as usize {
            return Some(Self::Hor);
        }
        if v == Self::Smooth as usize {
            return Some(Self::Smooth);
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockPartition {
    #[default]
    None = 0,
    H = 1,
    V = 2,
    Split = 3,
    TopSplit = 4,
    BottomSplit = 5,
    LeftSplit = 6,
    RightSplit = 7,
    H4 = 8,
    V4 = 9,
}

impl crate::av1::cpu::enums::EnumCount for BlockPartition {
    const COUNT: usize = 10;
}

impl BlockPartition {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::None as usize {
            return Some(Self::None);
        }
        if v == Self::H as usize {
            return Some(Self::H);
        }
        if v == Self::V as usize {
            return Some(Self::V);
        }
        if v == Self::Split as usize {
            return Some(Self::Split);
        }
        if v == Self::TopSplit as usize {
            return Some(Self::TopSplit);
        }
        if v == Self::BottomSplit as usize {
            return Some(Self::BottomSplit);
        }
        if v == Self::LeftSplit as usize {
            return Some(Self::LeftSplit);
        }
        if v == Self::RightSplit as usize {
            return Some(Self::RightSplit);
        }
        if v == Self::H4 as usize {
            return Some(Self::H4);
        }
        if v == Self::V4 as usize {
            return Some(Self::V4);
        }
        None
    }
}

impl BlockPartition {
    pub const N_SUB8X8_PARTITIONS: usize = 4;
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BlockSize {
    Bs128x128 = 0,
    Bs128x64 = 1,
    Bs64x128 = 2,
    Bs64x64 = 3,
    Bs64x32 = 4,
    Bs64x16 = 5,
    Bs32x64 = 6,
    Bs32x32 = 7,
    Bs32x16 = 8,
    Bs32x8 = 9,
    Bs16x64 = 10,
    Bs16x32 = 11,
    Bs16x16 = 12,
    Bs16x8 = 13,
    Bs16x4 = 14,
    Bs8x32 = 15,
    Bs8x16 = 16,
    Bs8x8 = 17,
    Bs8x4 = 18,
    Bs4x16 = 19,
    Bs4x8 = 20,
    Bs4x4 = 21,
}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromZeroes for BlockSize {}

impl crate::av1::cpu::enums::EnumCount for BlockSize {
    const COUNT: usize = 22;
}

impl BlockSize {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::Bs128x128 as usize {
            return Some(Self::Bs128x128);
        }
        if v == Self::Bs128x64 as usize {
            return Some(Self::Bs128x64);
        }
        if v == Self::Bs64x128 as usize {
            return Some(Self::Bs64x128);
        }
        if v == Self::Bs64x64 as usize {
            return Some(Self::Bs64x64);
        }
        if v == Self::Bs64x32 as usize {
            return Some(Self::Bs64x32);
        }
        if v == Self::Bs64x16 as usize {
            return Some(Self::Bs64x16);
        }
        if v == Self::Bs32x64 as usize {
            return Some(Self::Bs32x64);
        }
        if v == Self::Bs32x32 as usize {
            return Some(Self::Bs32x32);
        }
        if v == Self::Bs32x16 as usize {
            return Some(Self::Bs32x16);
        }
        if v == Self::Bs32x8 as usize {
            return Some(Self::Bs32x8);
        }
        if v == Self::Bs16x64 as usize {
            return Some(Self::Bs16x64);
        }
        if v == Self::Bs16x32 as usize {
            return Some(Self::Bs16x32);
        }
        if v == Self::Bs16x16 as usize {
            return Some(Self::Bs16x16);
        }
        if v == Self::Bs16x8 as usize {
            return Some(Self::Bs16x8);
        }
        if v == Self::Bs16x4 as usize {
            return Some(Self::Bs16x4);
        }
        if v == Self::Bs8x32 as usize {
            return Some(Self::Bs8x32);
        }
        if v == Self::Bs8x16 as usize {
            return Some(Self::Bs8x16);
        }
        if v == Self::Bs8x8 as usize {
            return Some(Self::Bs8x8);
        }
        if v == Self::Bs8x4 as usize {
            return Some(Self::Bs8x4);
        }
        if v == Self::Bs4x16 as usize {
            return Some(Self::Bs4x16);
        }
        if v == Self::Bs4x8 as usize {
            return Some(Self::Bs4x8);
        }
        if v == Self::Bs4x4 as usize {
            return Some(Self::Bs4x4);
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter2d {
    #[default]
    Regular8Tap = 0,
    RegularSmooth8Tap = 1,
    RegularSharp8Tap = 2,
    SharpRegular8Tap = 3,
    SharpSmooth8Tap = 4,
    Sharp8Tap = 5,
    SmoothRegular8Tap = 6,
    Smooth8Tap = 7,
    SmoothSharp8Tap = 8,
    Bilinear = 9,
}

impl crate::av1::cpu::enums::EnumCount for Filter2d {
    const COUNT: usize = 10;
}

impl Filter2d {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::Regular8Tap as usize {
            return Some(Self::Regular8Tap);
        }
        if v == Self::RegularSmooth8Tap as usize {
            return Some(Self::RegularSmooth8Tap);
        }
        if v == Self::RegularSharp8Tap as usize {
            return Some(Self::RegularSharp8Tap);
        }
        if v == Self::SharpRegular8Tap as usize {
            return Some(Self::SharpRegular8Tap);
        }
        if v == Self::SharpSmooth8Tap as usize {
            return Some(Self::SharpSmooth8Tap);
        }
        if v == Self::Sharp8Tap as usize {
            return Some(Self::Sharp8Tap);
        }
        if v == Self::SmoothRegular8Tap as usize {
            return Some(Self::SmoothRegular8Tap);
        }
        if v == Self::Smooth8Tap as usize {
            return Some(Self::Smooth8Tap);
        }
        if v == Self::SmoothSharp8Tap as usize {
            return Some(Self::SmoothSharp8Tap);
        }
        if v == Self::Bilinear as usize {
            return Some(Self::Bilinear);
        }
        None
    }
}

impl EnumKey<{ Self::COUNT }> for Filter2d {
    const VALUES: [Self; Self::COUNT] = [
        Self::Regular8Tap,
        Self::RegularSmooth8Tap,
        Self::RegularSharp8Tap,
        Self::SharpRegular8Tap,
        Self::SharpSmooth8Tap,
        Self::Sharp8Tap,
        Self::SmoothRegular8Tap,
        Self::Smooth8Tap,
        Self::SmoothSharp8Tap,
        Self::Bilinear,
    ];

    fn as_usize(self) -> usize {
        self as usize
    }
}

impl Filter2d {
    pub const fn h(&self) -> Rav1dFilterMode {
        use Filter2d::*;
        match *self {
            Regular8Tap | RegularSmooth8Tap | RegularSharp8Tap => Rav1dFilterMode::Regular8Tap,
            SharpRegular8Tap | SharpSmooth8Tap | Sharp8Tap => Rav1dFilterMode::Sharp8Tap,
            SmoothRegular8Tap | Smooth8Tap | SmoothSharp8Tap => Rav1dFilterMode::Smooth8Tap,
            Bilinear => Rav1dFilterMode::Bilinear,
        }
    }

    pub const fn v(&self) -> Rav1dFilterMode {
        use Filter2d::*;
        match *self {
            Regular8Tap | SharpRegular8Tap | SmoothRegular8Tap => Rav1dFilterMode::Regular8Tap,
            RegularSharp8Tap | Sharp8Tap | SmoothSharp8Tap => Rav1dFilterMode::Sharp8Tap,
            RegularSmooth8Tap | SharpSmooth8Tap | Smooth8Tap => Rav1dFilterMode::Smooth8Tap,
            Bilinear => Rav1dFilterMode::Bilinear,
        }
    }

    pub const fn hv(&self) -> (Rav1dFilterMode, Rav1dFilterMode) {
        (self.h(), self.v())
    }
}

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy)]
    pub struct MVJoint: u8 {
        const H = 1 << 0;
        const V = 1 << 1;
    }
}

pub type InterPredMode = u8;
pub const _N_INTER_PRED_MODES: usize = 4;
pub const NEWMV: InterPredMode = 3;
pub const GLOBALMV: InterPredMode = 2;
pub const NEARMV: InterPredMode = 1;
pub const NEARESTMV: InterPredMode = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum DrlProximity {
    #[default]
    Nearest,
    Nearer,
    Near,
    Nearish,
}

pub type CompInterPredMode = u8;
pub const N_COMP_INTER_PRED_MODES: usize = 8;
pub const NEWMV_NEWMV: CompInterPredMode = 7;
pub const GLOBALMV_GLOBALMV: CompInterPredMode = 6;
pub const NEWMV_NEARMV: CompInterPredMode = 5;
pub const NEARMV_NEWMV: CompInterPredMode = 4;
pub const NEWMV_NEARESTMV: CompInterPredMode = 3;
pub const NEARESTMV_NEWMV: CompInterPredMode = 2;
pub const NEARMV_NEARMV: CompInterPredMode = 1;
pub const NEARESTMV_NEARESTMV: CompInterPredMode = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompInterType {
    WeightedAvg = 1,
    Avg = 2,
    Seg = 3,
    Wedge = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterIntraType {
    Blend,
    Wedge,
}

#[derive(Clone, Copy, Eq, Default)]
#[repr(C, align(4))]
pub struct Mv {
    pub y: i16,
    pub x: i16,
}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromZeroes for Mv {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromBytes for Mv {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::AsBytes for Mv {}

impl Mv {
    pub const ZERO: Self = Self { y: 0, x: 0 };

    pub const INVALID: Self = Self {
        y: i16::MIN,
        x: i16::MIN,
    };

    pub fn is_invalid(self) -> bool {
        self == Self::INVALID
    }

    #[allow(dead_code)]
    pub fn is_valid(self) -> bool {
        !self.is_invalid()
    }

    pub fn into_unaligned(self) -> UnalignedMv {
        let Self { y, x } = self;
        UnalignedMv { y, x }
    }
}

impl Neg for Mv {
    type Output = Self;

    fn neg(self) -> Self::Output {
        Self {
            y: -self.y,
            x: -self.x,
        }
    }
}

impl PartialEq for Mv {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[repr(C)]
pub struct UnalignedMv {
    pub y: i16,
    pub x: i16,
}

impl UnalignedMv {
    pub fn into_aligned(self) -> Mv {
        let Self { y, x } = self;
        Mv { y, x }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MotionMode {
    #[default]
    Translation = 0,
    Obmc = 1,
    Warp = 2,
}

impl MotionMode {
    pub const fn from_repr(v: usize) -> Option<Self> {
        if v == Self::Translation as usize {
            return Some(Self::Translation);
        }
        if v == Self::Obmc as usize {
            return Some(Self::Obmc);
        }
        if v == Self::Warp as usize {
            return Some(Self::Warp);
        }
        None
    }
}

#[derive(Clone, Default)]
#[repr(C)]
pub struct Av1BlockIntra {
    pub y_mode: u8,
    pub uv_mode: u8,
    pub tx: TxfmSize,
    pub pal_sz: [u8; 2],
    pub y_angle: i8,
    pub uv_angle: i8,
    pub cfl_alpha: [i8; 2],
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct MaskedInterIntraPredMode(u8);

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromZeroes for MaskedInterIntraPredMode {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromBytes for MaskedInterIntraPredMode {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::AsBytes for MaskedInterIntraPredMode {}

impl MaskedInterIntraPredMode {
    pub const fn get(self) -> InterIntraPredMode {
        match InterIntraPredMode::from_repr(self.0 as usize % InterIntraPredMode::COUNT) {
            Some(it) => it,
            None => unreachable!(),
        }
    }

    pub const fn new(this: InterIntraPredMode) -> Self {
        Self(this as u8)
    }
}

impl Default for MaskedInterIntraPredMode {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl From<InterIntraPredMode> for MaskedInterIntraPredMode {
    fn from(value: InterIntraPredMode) -> Self {
        MaskedInterIntraPredMode::new(value)
    }
}

impl From<MaskedInterIntraPredMode> for InterIntraPredMode {
    fn from(value: MaskedInterIntraPredMode) -> Self {
        value.get()
    }
}

#[derive(Clone, Default)]
#[repr(C)]
pub struct Av1BlockInter1d {
    pub mv: [Mv; 2],
    pub wedge_idx: u8,

    pub mask_sign: u8,

    pub interintra_mode: MaskedInterIntraPredMode,

    pub _padding: u8,
}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromZeroes for Av1BlockInter1d {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromBytes for Av1BlockInter1d {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::AsBytes for Av1BlockInter1d {}

impl Av1BlockInter1d {
    pub fn mask_sign(&self) -> bool {
        self.mask_sign != 0
    }
}

#[derive(Clone)]
#[repr(C)]
pub struct Av1BlockInter2d {
    pub mv2d: Mv,

    pub matrix: [i16; 4],
}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromZeroes for Av1BlockInter2d {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::FromBytes for Av1BlockInter2d {}

#[allow(unsafe_code)]
unsafe impl crate::av1::cpu::pod::AsBytes for Av1BlockInter2d {}

#[derive(Clone)]
#[repr(C)]
pub struct Av1BlockInterNd {
    pub one_d: Av1BlockInter1d,
}

impl Av1BlockInterNd {
    pub fn two_d(&self) -> &Av1BlockInter2d {
        const _: () =
            assert!(mem::size_of::<Av1BlockInter1d>() == mem::size_of::<Av1BlockInter2d>());
        const _: () =
            assert!(mem::align_of::<Av1BlockInter1d>() == mem::align_of::<Av1BlockInter2d>());
        FromBytes::ref_from(AsBytes::as_bytes(&self.one_d)).unwrap()
    }
}

impl From<Av1BlockInter1d> for Av1BlockInterNd {
    fn from(one_d: Av1BlockInter1d) -> Self {
        Self { one_d }
    }
}

impl From<Av1BlockInter2d> for Av1BlockInterNd {
    fn from(two_d: Av1BlockInter2d) -> Self {
        let one_d = <Av1BlockInter1d as FromBytes>::ref_from(AsBytes::as_bytes(&two_d))
            .unwrap()
            .clone();
        Self { one_d }
    }
}

#[derive(Clone)]
#[repr(C)]
pub struct Av1BlockInter {
    pub nd: Av1BlockInterNd,
    pub comp_type: Option<CompInterType>,
    pub inter_mode: u8,
    pub motion_mode: MotionMode,
    pub drl_idx: DrlProximity,
    pub r#ref: [i8; 2],
    pub max_ytx: TxfmSize,
    pub filter2d: Filter2d,
    pub interintra_type: Option<InterIntraType>,
    pub tx_split0: u8,
    pub tx_split1: u16,
}

pub enum Av1BlockIntraInter {
    Intra(Av1BlockIntra),
    Inter(Av1BlockInter),
}

impl Av1BlockIntraInter {
    pub fn filter2d(&self) -> Filter2d {
        match self {
            Self::Inter(inter) => Some(inter),
            _ => None,
        }
        .map(|inter| inter.filter2d)
        .unwrap_or_default()
    }
}

impl Default for Av1BlockIntraInter {
    fn default() -> Self {
        Self::Intra(Default::default())
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SegmentId {
    id: InRange<u8, 0, { Self::COUNT as u128 - 1 }>,
}

impl SegmentId {
    pub const COUNT: usize = 8;

    pub fn new(id: u8) -> Option<Self> {
        Some(Self {
            id: InRange::new(id)?,
        })
    }

    pub fn get(&self) -> usize {
        self.id.get() as usize
    }

    pub fn min() -> Self {
        Self::new(0).unwrap()
    }

    pub fn max() -> Self {
        Self::new(Self::COUNT as u8 - 1).unwrap()
    }
}

impl Display for SegmentId {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self.id)
    }
}

#[derive(Default)]
#[repr(C)]
pub struct Av1Block {
    pub bl: BlockLevel,
    pub bs: u8,
    pub bp: BlockPartition,
    pub seg_id: SegmentId,
    pub skip_mode: u8,
    pub skip: u8,
    pub uvtx: TxfmSize,
    pub ii: Av1BlockIntraInter,
}
