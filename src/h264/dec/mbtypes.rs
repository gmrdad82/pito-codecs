use crate::h264::dec::picture::MbKind;

pub const PRED_L0: u8 = 1;
pub const PRED_L1: u8 = 2;
pub const PRED_BI: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Part {
    #[default]
    P16x16,
    P16x8,
    P8x16,
    P8x8,
}

impl Part {
    pub fn num_parts(self) -> usize {
        match self {
            Part::P16x16 => 1,
            Part::P16x8 | Part::P8x16 => 2,
            Part::P8x8 => 4,
        }
    }
    pub fn rect(self, i: usize) -> (usize, usize, usize, usize) {
        match self {
            Part::P16x16 => (0, 0, 4, 4),
            Part::P16x8 => (0, 2 * i, 4, 2),
            Part::P8x16 => (2 * i, 0, 2, 4),
            Part::P8x8 => ((i & 1) * 2, (i >> 1) * 2, 2, 2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubPart {
    #[default]
    S8x8,
    S8x4,
    S4x8,
    S4x4,
}

impl SubPart {
    pub fn num_parts(self) -> usize {
        match self {
            SubPart::S8x8 => 1,
            SubPart::S8x4 | SubPart::S4x8 => 2,
            SubPart::S4x4 => 4,
        }
    }
    pub fn rect(self, j: usize) -> (usize, usize, usize, usize) {
        match self {
            SubPart::S8x8 => (0, 0, 2, 2),
            SubPart::S8x4 => (0, j, 2, 1),
            SubPart::S4x8 => (j, 0, 1, 2),
            SubPart::S4x4 => (j & 1, j >> 1, 1, 1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MbTypeInfo {
    pub kind: MbKind,
    pub i16_mode: u8,
    pub i16_cbp: u8,
    pub part: Part,
    pub pred: [u8; 2],
    pub ref0: bool,
}

pub fn intra_mb_type(t: u32) -> Option<MbTypeInfo> {
    let mut m = MbTypeInfo::default();
    match t {
        0 => m.kind = MbKind::I4x4,
        1..=24 => {
            m.kind = MbKind::I16x16;
            m.i16_mode = ((t - 1) % 4) as u8;
            let chroma = ((t - 1) / 4) % 3;
            let luma = if t >= 13 { 15 } else { 0 };
            m.i16_cbp = (luma | (chroma << 4)) as u8;
        }
        25 => m.kind = MbKind::IPcm,
        _ => return None,
    }
    Some(m)
}

pub fn p_mb_type(t: u32) -> Option<MbTypeInfo> {
    let mut m = MbTypeInfo {
        kind: MbKind::Inter,
        pred: [PRED_L0; 2],
        ..Default::default()
    };
    m.part = match t {
        0 => Part::P16x16,
        1 => Part::P16x8,
        2 => Part::P8x16,
        3 => Part::P8x8,
        4 => {
            m.ref0 = true;
            Part::P8x8
        }
        _ => return None,
    };
    Some(m)
}

pub fn b_mb_type(t: u32) -> Option<MbTypeInfo> {
    let mut m = MbTypeInfo {
        kind: MbKind::Inter,
        ..Default::default()
    };
    match t {
        0 => {
            m.kind = MbKind::BDirect16x16;
            m.pred = [PRED_BI; 2];
        }
        1..=3 => {
            m.part = Part::P16x16;
            m.pred = [t as u8; 2];
        }
        4..=21 => {
            const PAIRS: [(u8, u8); 9] = [
                (PRED_L0, PRED_L0),
                (PRED_L1, PRED_L1),
                (PRED_L0, PRED_L1),
                (PRED_L1, PRED_L0),
                (PRED_L0, PRED_BI),
                (PRED_L1, PRED_BI),
                (PRED_BI, PRED_L0),
                (PRED_BI, PRED_L1),
                (PRED_BI, PRED_BI),
            ];
            let (a, b) = PAIRS[((t - 4) / 2) as usize];
            m.pred = [a, b];
            m.part = if t.is_multiple_of(2) {
                Part::P16x8
            } else {
                Part::P8x16
            };
        }
        22 => m.part = Part::P8x8,
        _ => return None,
    }
    Some(m)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubMbInfo {
    pub shape: SubPart,
    pub pred: u8,
    pub direct: bool,
}

pub fn p_sub_mb_type(t: u32) -> Option<SubMbInfo> {
    let shape = match t {
        0 => SubPart::S8x8,
        1 => SubPart::S8x4,
        2 => SubPart::S4x8,
        3 => SubPart::S4x4,
        _ => return None,
    };
    Some(SubMbInfo {
        shape,
        pred: PRED_L0,
        direct: false,
    })
}

pub fn b_sub_mb_type(t: u32) -> Option<SubMbInfo> {
    let (shape, pred, direct) = match t {
        0 => (SubPart::S8x8, PRED_BI, true),
        1 => (SubPart::S8x8, PRED_L0, false),
        2 => (SubPart::S8x8, PRED_L1, false),
        3 => (SubPart::S8x8, PRED_BI, false),
        4 => (SubPart::S8x4, PRED_L0, false),
        5 => (SubPart::S4x8, PRED_L0, false),
        6 => (SubPart::S8x4, PRED_L1, false),
        7 => (SubPart::S4x8, PRED_L1, false),
        8 => (SubPart::S8x4, PRED_BI, false),
        9 => (SubPart::S4x8, PRED_BI, false),
        10 => (SubPart::S4x4, PRED_L0, false),
        11 => (SubPart::S4x4, PRED_L1, false),
        12 => (SubPart::S4x4, PRED_BI, false),
        _ => return None,
    };
    Some(SubMbInfo {
        shape,
        pred,
        direct,
    })
}

#[rustfmt::skip]
pub const CBP_ME: [(u8, u8); 48] = [
    (47, 0), (31, 16), (15, 1), (0, 2), (23, 4), (27, 8), (29, 32), (30, 3),
    (7, 5), (11, 10), (13, 12), (14, 15), (39, 47), (43, 7), (45, 11), (46, 13),
    (16, 14), (3, 6), (5, 9), (10, 31), (12, 35), (19, 37), (21, 42), (26, 44),
    (28, 33), (35, 34), (37, 36), (42, 40), (44, 39), (1, 43), (2, 45), (4, 46),
    (8, 17), (17, 18), (18, 20), (20, 24), (24, 19), (6, 21), (9, 26), (22, 28),
    (25, 23), (32, 27), (33, 29), (34, 30), (36, 22), (40, 25), (38, 38), (41, 41),
];
