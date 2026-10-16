#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ops::Deref;
use std::pin::Pin;
use std::ptr::{NonNull, drop_in_place};
use std::sync::Arc;

use crate::av1::cpu::send_sync_non_null::SendSyncNonNull;

pub type FnFree = unsafe extern "C" fn(ptr: *const u8, cookie: Option<SendSyncNonNull<c_void>>);

#[derive(Debug)]
struct Free {
    pub free: FnFree,

    pub cookie: Option<SendSyncNonNull<c_void>>,
}

impl Free {
    pub unsafe fn free(&self, ptr: *mut c_void) {
        unsafe { (self.free)(ptr as *const u8, self.cookie) }
    }
}

#[derive(Debug)]
pub struct Unique<T: ?Sized> {
    pointer: NonNull<T>,

    _marker: PhantomData<T>,
}

unsafe impl<T: Send + ?Sized> Send for Unique<T> {}

unsafe impl<T: Sync + ?Sized> Sync for Unique<T> {}

pub struct CBox<T: ?Sized> {
    data: Unique<T>,
    free: Free,
}

impl<T: ?Sized> AsRef<T> for CBox<T> {
    fn as_ref(&self) -> &T {
        unsafe { self.data.pointer.as_ref() }
    }
}

impl<T: ?Sized> Drop for CBox<T> {
    fn drop(&mut self) {
        let Self { data, free } = self;
        let ptr = data.pointer.as_ptr();

        unsafe { drop_in_place(ptr) };
        let ptr = ptr.cast();

        unsafe { free.free(ptr) }
    }
}

impl<T: ?Sized> Deref for CBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T: ?Sized> CBox<T> {
    pub unsafe fn new(
        data: NonNull<T>,
        free: FnFree,
        cookie: Option<SendSyncNonNull<c_void>>,
    ) -> Self {
        Self {
            data: Unique {
                pointer: data,
                _marker: PhantomData,
            },
            free: Free { free, cookie },
        }
    }
}

pub enum CRef<T: ?Sized + 'static> {
    Ref(&'static T),
    Box(Box<T>),
    Arc(Arc<T>),

    C(CBox<T>),
}

impl<T: ?Sized> AsRef<T> for CRef<T> {
    fn as_ref(&self) -> &T {
        match self {
            Self::Ref(data) => data,
            Self::Box(data) => data.as_ref(),
            Self::Arc(data) => data.as_ref(),
            Self::C(data) => data.as_ref(),
        }
    }
}

impl<T: ?Sized> Deref for CRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T: ?Sized> CRef<T> {
    pub fn into_pin(self) -> Pin<Self> {
        unsafe { Pin::new_unchecked(self) }
    }
}

impl<T: ?Sized> From<CRef<T>> for Pin<CRef<T>> {
    fn from(value: CRef<T>) -> Self {
        value.into_pin()
    }
}
