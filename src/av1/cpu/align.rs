use std::hint::assert_unchecked;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::{Deref, DerefMut, Index, IndexMut};
use std::{mem, slice};

use crate::av1::cpu::disjoint_mut::AsMutPtr;

pub trait ArrayDefault {
    fn default() -> Self;
}

impl<T: ArrayDefault + Copy, const N: usize> ArrayDefault for [T; N] {
    fn default() -> Self {
        [T::default(); N]
    }
}

impl<T> ArrayDefault for Option<T> {
    fn default() -> Self {
        None
    }
}

macro_rules! impl_ArrayDefault {
    ($T:ty) => {
        impl ArrayDefault for $T {
            fn default() -> Self {
                <Self as Default>::default()
            }
        }
    };
}

impl_ArrayDefault!(u8);
impl_ArrayDefault!(i8);
impl_ArrayDefault!(i16);
impl_ArrayDefault!(i32);
impl_ArrayDefault!(u16);

pub trait AlignedByteChunk
where
    Self: Sized,
{
}

macro_rules! def_align {
    ($align:literal, $name:ident) => {
        #[derive(Clone, Copy)]
        #[repr(C, align($align))]
        pub struct $name<T>(pub T);

        impl<T> From<T> for $name<T> {
            fn from(from: T) -> Self {
                Self(from)
            }
        }

        impl<T: Index<usize>> Index<usize> for $name<T> {
            type Output = T::Output;

            fn index(&self, index: usize) -> &Self::Output {
                &self.0[index]
            }
        }

        impl<T: IndexMut<usize>> IndexMut<usize> for $name<T> {
            fn index_mut(&mut self, index: usize) -> &mut Self::Output {
                &mut self.0[index]
            }
        }

        impl<T: ArrayDefault> ArrayDefault for $name<T> {
            fn default() -> Self {
                Self(T::default())
            }
        }

        impl<T: ArrayDefault> Default for $name<T> {
            fn default() -> Self {
                <Self as ArrayDefault>::default()
            }
        }

        impl AlignedByteChunk for $name<[u8; $align]> {}

        unsafe impl<V, const N: usize> AsMutPtr for $name<[V; N]> {
            type Target = V;

            unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut V {
                unsafe { assert_unchecked(ptr.is_aligned()) }

                ptr.cast()
            }

            fn len(&self) -> usize {
                N
            }
        }
    };
}

def_align!(1, Align1);
def_align!(2, Align2);
def_align!(4, Align4);
def_align!(8, Align8);
def_align!(16, Align16);
def_align!(32, Align32);
def_align!(64, Align64);

pub struct AlignedVec<T: Copy, C: AlignedByteChunk> {
    inner: Vec<MaybeUninit<C>>,

    len: usize,
    _phantom: PhantomData<T>,
}

impl<T: Copy, C: AlignedByteChunk> AlignedVec<T, C> {
    const fn check_byte_chunk_type_is_aligned() {
        assert!(mem::size_of::<C>() == mem::align_of::<C>());
    }

    const fn check_inner_type_is_aligned() {
        assert!(mem::align_of::<T>() <= mem::align_of::<C>());
    }

    pub const fn new() -> Self {
        Self::check_byte_chunk_type_is_aligned();
        Self::check_inner_type_is_aligned();

        Self {
            inner: Vec::new(),
            len: 0,
            _phantom: PhantomData,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_ptr(&self) -> *const T {
        self.inner.as_ptr().cast()
    }

    pub fn as_mut_ptr(&mut self) -> *mut T {
        self.inner.as_mut_ptr().cast()
    }

    pub fn as_slice(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.as_ptr(), self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.as_mut_ptr(), self.len) }
    }

    pub fn resize(&mut self, new_len: usize, value: T) {
        let old_len = self.len();

        let new_bytes = mem::size_of::<T>()
            .checked_mul(new_len)
            .expect("Resizing would overflow the underlying aligned buffer");

        let chunk_size = mem::size_of::<C>();
        let new_chunks = if (new_bytes % chunk_size) == 0 {
            new_bytes / chunk_size
        } else {
            (new_bytes / chunk_size) + 1
        };

        self.inner.resize_with(new_chunks, MaybeUninit::uninit);

        for offset in old_len..new_len {
            unsafe { self.as_mut_ptr().add(offset).write(value) };
        }

        self.len = new_len;
    }
}

impl<T: Copy, C: AlignedByteChunk> AsRef<[T]> for AlignedVec<T, C> {
    fn as_ref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy, C: AlignedByteChunk> AsMut<[T]> for AlignedVec<T, C> {
    fn as_mut(&mut self) -> &mut [T] {
        self.as_mut_slice()
    }
}

impl<T: Copy, C: AlignedByteChunk> Deref for AlignedVec<T, C> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Copy, C: AlignedByteChunk> DerefMut for AlignedVec<T, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Copy, C: AlignedByteChunk> Default for AlignedVec<T, C> {
    fn default() -> Self {
        Self::new()
    }
}

pub type AlignedVec2<T> = AlignedVec<T, Align2<[u8; 2]>>;
pub type AlignedVec32<T> = AlignedVec<T, Align32<[u8; 32]>>;
pub type AlignedVec64<T> = AlignedVec<T, Align64<[u8; 64]>>;

unsafe impl<T: Copy, C: AlignedByteChunk> AsMutPtr for AlignedVec<T, C> {
    type Target = T;

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target {
        let ptr = unsafe { &mut *ptr }.as_mut_ptr();

        unsafe { assert_unchecked(ptr.cast::<C>().is_aligned()) };

        ptr
    }

    fn len(&self) -> usize {
        self.len()
    }
}
