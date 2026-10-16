use std::marker::PhantomData;
use std::ptr;

use crate::av1::cpu::with_offset::WithOffset;

#[repr(C)]
pub struct FFISafe<'a, T> {
    phantom: PhantomData<&'a T>,
    non_zst: bool,
}

impl<'a, T> FFISafe<'a, T> {
    pub fn new(this: &'a T) -> *const Self {
        ptr::from_ref(this).cast()
    }

    pub fn _new_mut(this: &'a mut T) -> *mut Self {
        ptr::from_mut(this).cast()
    }

    pub unsafe fn get(this: *const Self) -> &'a T {
        unsafe { &*this.cast() }
    }

    pub unsafe fn _get_mut(this: *mut Self) -> &'a mut T {
        unsafe { &mut *this.cast() }
    }

    pub unsafe fn from_with_offset(this: WithOffset<*const FFISafe<'a, T>>) -> WithOffset<&'a T> {
        this.map(|data| unsafe { FFISafe::get(data) })
    }
}

impl<'a, T> WithOffset<&'a T> {
    pub fn into_ffi_safe(self) -> WithOffset<*const FFISafe<'a, T>> {
        self.map(FFISafe::new)
    }
}
