use std::mem;
use std::ops::Deref;

use crate::av1::cpu::disjoint_mut::{AsMutPtr, DisjointMut};
use crate::av1::cpu::include::common::bitdepth::BitDepth;
use crate::av1::cpu::strided::WithStride;

pub trait Pixels {
    fn byte_len(&self) -> usize;

    fn as_byte_mut_ptr(&self) -> *mut u8;

    fn pixel_len<BD: BitDepth>(&self) -> usize {
        self.byte_len() / mem::size_of::<BD::Pixel>()
    }

    fn as_mut_ptr<BD: BitDepth>(&self) -> *mut BD::Pixel {
        self.as_byte_mut_ptr().cast()
    }

    fn as_ptr<BD: BitDepth>(&self) -> *const BD::Pixel {
        self.as_mut_ptr::<BD>().cast_const()
    }

    #[cfg_attr(debug_assertions, track_caller)]
    fn as_mut_ptr_at<BD: BitDepth>(&self, pixel_offset: usize) -> *mut BD::Pixel {
        #[inline(never)]
        #[cfg_attr(debug_assertions, track_caller)]
        fn out_of_bounds(pixel_offset: usize, pixel_len: usize) -> ! {
            panic!(
                "pixel offset {pixel_offset} out of range for slice of pixel length {pixel_len}"
            );
        }

        let pixel_len = self.pixel_len::<BD>();
        if pixel_offset > pixel_len {
            out_of_bounds(pixel_offset, pixel_len);
        }

        unsafe { self.as_mut_ptr::<BD>().add(pixel_offset) }
    }

    #[cfg_attr(debug_assertions, track_caller)]
    fn as_ptr_at<BD: BitDepth>(&self, pixel_offset: usize) -> *const BD::Pixel {
        self.as_mut_ptr_at::<BD>(pixel_offset).cast_const()
    }

    fn wrapping_as_mut_ptr_at<BD: BitDepth>(&self, pixel_offset: usize) -> *mut BD::Pixel {
        self.as_mut_ptr::<BD>().wrapping_add(pixel_offset)
    }

    fn wrapping_as_ptr_at<BD: BitDepth>(&self, pixel_offset: usize) -> *const BD::Pixel {
        self.as_ptr::<BD>().wrapping_add(pixel_offset)
    }

    fn ref_eq(&self, other: &Self) -> bool {
        self.as_byte_mut_ptr() == other.as_byte_mut_ptr()
    }
}

impl<'a, P: Pixels> Pixels for &'a P {
    fn byte_len(&self) -> usize {
        (*self).byte_len()
    }

    fn as_byte_mut_ptr(&self) -> *mut u8 {
        (*self).as_byte_mut_ptr()
    }
}

impl<P: Pixels> Pixels for WithStride<P> {
    fn byte_len(&self) -> usize {
        self.deref().byte_len()
    }

    fn as_byte_mut_ptr(&self) -> *mut u8 {
        self.deref().as_byte_mut_ptr()
    }
}

impl<T: AsMutPtr<Target = u8>> Pixels for DisjointMut<T> {
    fn byte_len(&self) -> usize {
        self.len()
    }

    fn as_byte_mut_ptr(&self) -> *mut u8 {
        self.as_mut_ptr()
    }
}
