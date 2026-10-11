#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Mv {
    pub x: i16,
    pub y: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { x: 0, y: 0 };
    #[inline]
    pub const fn new(x: i32, y: i32) -> Mv {
        Mv {
            x: x as i16,
            y: y as i16,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MbKind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    PSkip,
    PInter,
    BSkip,
    BDirect,
    BInter,
}

impl MbKind {
    #[inline]
    pub fn is_intra(self) -> bool {
        matches!(self, MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16)
    }
    #[inline]
    pub fn is_skip(self) -> bool {
        matches!(self, MbKind::PSkip | MbKind::BSkip)
    }
    #[inline]
    pub fn is_inxn(self) -> bool {
        matches!(self, MbKind::I4x4 | MbKind::I8x8)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Part {
    #[default]
    P16x16,
    P16x8,
    P8x16,
    P8x8,
}

impl Part {
    pub fn count(self) -> usize {
        match self {
            Part::P16x16 => 1,
            Part::P16x8 | Part::P8x16 => 2,
            Part::P8x8 => 4,
        }
    }
    pub fn rect(self, i: usize) -> (usize, usize, usize, usize) {
        match self {
            Part::P16x16 => (0, 0, 16, 16),
            Part::P16x8 => (0, 8 * i, 16, 8),
            Part::P8x16 => (8 * i, 0, 8, 16),
            Part::P8x8 => (8 * (i & 1), 8 * (i >> 1), 8, 8),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MbInfo {
    pub kind: MbKind,
    pub slice: u16,
    pub qp: u8,
    pub cbp: u8,
    pub t8x8: bool,
    pub chroma_mode: u8,
    pub nnz: [u8; 16],
    pub nnz_c: [u8; 8],
    pub dc_cbf: u8,
    pub imodes: [i8; 16],
    pub ref_idx: [[i8; 4]; 2],
    pub mv: [[Mv; 16]; 2],
    pub mvd: [[[u8; 2]; 16]; 2],
}

impl Default for MbInfo {
    fn default() -> Self {
        MbInfo {
            kind: MbKind::None,
            slice: 0,
            qp: 0,
            cbp: 0,
            t8x8: false,
            chroma_mode: 0,
            nnz: [0; 16],
            nnz_c: [0; 8],
            dc_cbf: 0,
            imodes: [-1; 16],
            ref_idx: [[-1; 4]; 2],
            mv: [[Mv::ZERO; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
        }
    }
}

impl MbInfo {
    #[inline]
    pub fn ref_at(&self, list: usize, blk4_raster: usize) -> i8 {
        let (x, y) = (blk4_raster & 3, blk4_raster >> 2);
        self.ref_idx[list][(y >> 1) * 2 + (x >> 1)]
    }
}
