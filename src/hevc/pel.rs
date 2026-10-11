#[derive(Clone)]
pub enum Samples {
    U8(Box<[u8]>),
    U16(Box<[u16]>),
}

pub(crate) trait Pel:
    Copy + Default + PartialEq + Send + Sync + 'static + Into<i32> + std::fmt::Debug
{
    const BYTES: usize;
    fn from_i32(v: i32) -> Self;
    fn view(s: &Samples) -> &[Self];
    fn wrap(v: Vec<Self>) -> Samples;
    fn take(s: Samples) -> Option<Vec<Self>>;
    fn extend_bytes(out: &mut Vec<u8>, src: &[Self]);
}

impl Pel for u8 {
    const BYTES: usize = 1;

    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u8
    }

    fn view(s: &Samples) -> &[u8] {
        match s {
            Samples::U8(v) => v,
            Samples::U16(_) => &[],
        }
    }

    fn wrap(v: Vec<u8>) -> Samples {
        Samples::U8(v.into())
    }

    fn take(s: Samples) -> Option<Vec<u8>> {
        match s {
            Samples::U8(v) => Some(v.into_vec()),
            Samples::U16(_) => None,
        }
    }

    fn extend_bytes(out: &mut Vec<u8>, src: &[u8]) {
        out.extend_from_slice(src);
    }
}

impl Pel for u16 {
    const BYTES: usize = 2;

    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u16
    }

    fn view(s: &Samples) -> &[u16] {
        match s {
            Samples::U16(v) => v,
            Samples::U8(_) => &[],
        }
    }

    fn wrap(v: Vec<u16>) -> Samples {
        Samples::U16(v.into())
    }

    fn take(s: Samples) -> Option<Vec<u16>> {
        match s {
            Samples::U16(v) => Some(v.into_vec()),
            Samples::U8(_) => None,
        }
    }

    fn extend_bytes(out: &mut Vec<u8>, src: &[u16]) {
        out.extend(src.iter().flat_map(|s| s.to_le_bytes()));
    }
}
