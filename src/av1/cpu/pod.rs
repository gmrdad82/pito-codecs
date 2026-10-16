use std::mem::{align_of, size_of};
use std::slice;

pub unsafe trait FromZeroes {
    fn new_zeroed() -> Self
    where
        Self: Sized,
    {
        unsafe { std::mem::zeroed() }
    }
}

pub unsafe trait FromBytes: FromZeroes {
    fn ref_from(bytes: &[u8]) -> Option<&Self>
    where
        Self: Sized,
    {
        if bytes.len() != size_of::<Self>() {
            return None;
        }
        Self::ref_from_prefix(bytes)
    }

    fn mut_from(bytes: &mut [u8]) -> Option<&mut Self>
    where
        Self: Sized + AsBytes,
    {
        if bytes.len() != size_of::<Self>() {
            return None;
        }
        Self::mut_from_prefix(bytes)
    }

    fn ref_from_prefix(bytes: &[u8]) -> Option<&Self>
    where
        Self: Sized,
    {
        if bytes.len() < size_of::<Self>()
            || !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self>())
        {
            return None;
        }
        Some(unsafe { &*(bytes.as_ptr() as *const Self) })
    }

    fn mut_from_prefix(bytes: &mut [u8]) -> Option<&mut Self>
    where
        Self: Sized + AsBytes,
    {
        if bytes.len() < size_of::<Self>()
            || !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self>())
        {
            return None;
        }
        Some(unsafe { &mut *(bytes.as_mut_ptr() as *mut Self) })
    }

    fn slice_from(bytes: &[u8]) -> Option<&[Self]>
    where
        Self: Sized,
    {
        let size = size_of::<Self>();
        if size == 0
            || !bytes.len().is_multiple_of(size)
            || !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self>())
        {
            return None;
        }
        Some(unsafe { slice::from_raw_parts(bytes.as_ptr() as *const Self, bytes.len() / size) })
    }

    fn mut_slice_from(bytes: &mut [u8]) -> Option<&mut [Self]>
    where
        Self: Sized + AsBytes,
    {
        let size = size_of::<Self>();
        if size == 0
            || !bytes.len().is_multiple_of(size)
            || !(bytes.as_ptr() as usize).is_multiple_of(align_of::<Self>())
        {
            return None;
        }
        Some(unsafe {
            slice::from_raw_parts_mut(bytes.as_mut_ptr() as *mut Self, bytes.len() / size)
        })
    }
}

pub unsafe trait AsBytes {
    fn as_bytes(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self as *const Self as *const u8, size_of_val(self)) }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8]
    where
        Self: FromBytes,
    {
        unsafe { slice::from_raw_parts_mut(self as *mut Self as *mut u8, size_of_val(self)) }
    }
}

macro_rules! pod {
    ($($t:ty),*) => {$(
        unsafe impl FromZeroes for $t {}
        unsafe impl FromBytes for $t {}
        unsafe impl AsBytes for $t {}
    )*};
}

pod!(u8, i8, u16, i16, u32, i32, u64, i64, usize, isize);

unsafe impl<T: FromZeroes, const N: usize> FromZeroes for [T; N] {}
unsafe impl<T: FromBytes, const N: usize> FromBytes for [T; N] {}
unsafe impl<T: AsBytes, const N: usize> AsBytes for [T; N] {}
unsafe impl<T: FromZeroes> FromZeroes for [T] {}
unsafe impl<T: FromBytes> FromBytes for [T] {}
unsafe impl<T: AsBytes> AsBytes for [T] {}
unsafe impl FromZeroes for bool {}
unsafe impl AsBytes for bool {}
