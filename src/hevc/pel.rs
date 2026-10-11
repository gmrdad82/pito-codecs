use crate::hevc::grid::Grid;

use std::sync::OnceLock;

pub enum Store {
    U8([Grid<u8>; 3]),
    U16([Grid<u16>; 3]),
}

#[derive(Default)]
pub struct Slots {
    pub u8: OnceLock<[Grid<u8>; 3]>,
    pub u16: OnceLock<[Grid<u16>; 3]>,
}

pub(crate) trait Pel:
    Copy + Default + PartialEq + Send + Sync + 'static + Into<i32> + std::fmt::Debug
{
    const BYTES: usize;
    fn from_i32(v: i32) -> Self;
    fn slot(s: &Slots) -> &OnceLock<[Grid<Self>; 3]>;
    fn unstore(s: Store) -> Option<[Grid<Self>; 3]>;
    fn extend_bytes(out: &mut Vec<u8>, src: &[Self]);
    fn write_bytes(out: &mut [u8], src: &[Self]);
}

impl Pel for u8 {
    const BYTES: usize = 1;

    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u8
    }

    fn slot(s: &Slots) -> &OnceLock<[Grid<u8>; 3]> {
        &s.u8
    }

    fn unstore(s: Store) -> Option<[Grid<u8>; 3]> {
        match s {
            Store::U8(g) => Some(g),
            Store::U16(_) => None,
        }
    }

    fn extend_bytes(out: &mut Vec<u8>, src: &[u8]) {
        out.extend_from_slice(src);
    }

    fn write_bytes(out: &mut [u8], src: &[u8]) {
        out.copy_from_slice(src);
    }
}

impl Pel for u16 {
    const BYTES: usize = 2;

    #[inline(always)]
    fn from_i32(v: i32) -> Self {
        v as u16
    }

    fn slot(s: &Slots) -> &OnceLock<[Grid<u16>; 3]> {
        &s.u16
    }

    fn unstore(s: Store) -> Option<[Grid<u16>; 3]> {
        match s {
            Store::U16(g) => Some(g),
            Store::U8(_) => None,
        }
    }

    fn extend_bytes(out: &mut Vec<u8>, src: &[u16]) {
        let at = out.len();
        out.resize(at + src.len() * 2, 0);
        Self::write_bytes(&mut out[at..], src);
    }

    fn write_bytes(out: &mut [u8], src: &[u16]) {
        #[cfg(target_endian = "little")]
        {
            let bytes =
                unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, src.len() * 2) };
            let n = out.len().min(bytes.len());
            out[..n].copy_from_slice(&bytes[..n]);
        }
        #[cfg(not(target_endian = "little"))]
        for (d, s) in out.chunks_exact_mut(2).zip(src) {
            d.copy_from_slice(&s.to_le_bytes());
        }
    }
}
