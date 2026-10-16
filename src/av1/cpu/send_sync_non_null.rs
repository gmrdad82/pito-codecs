use std::ptr::NonNull;

#[derive(Debug)]
#[repr(transparent)]
pub struct SendSyncNonNull<T: ?Sized>(NonNull<T>);

impl<T: ?Sized> Clone for SendSyncNonNull<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for SendSyncNonNull<T> {}

unsafe impl<T: ?Sized> Send for SendSyncNonNull<T> {}

unsafe impl<T: ?Sized> Sync for SendSyncNonNull<T> {}

impl<T: ?Sized> SendSyncNonNull<T> {
    pub const fn cast<U>(self) -> SendSyncNonNull<U> {
        SendSyncNonNull(self.0.cast())
    }
}

impl<T: ?Sized + Send + Sync> SendSyncNonNull<T> {
    pub const unsafe fn new_unchecked(ptr: NonNull<T>) -> Self {
        Self(ptr)
    }

    pub fn from_ref(r#ref: &T) -> Self {
        let ptr = NonNull::from(r#ref);

        unsafe { Self::new_unchecked(ptr) }
    }

    pub fn from_box(r#box: Box<T>) -> Self {
        let ptr = NonNull::new(Box::into_raw(r#box)).unwrap();

        unsafe { Self::new_unchecked(ptr) }
    }

    pub const fn as_ptr(&self) -> NonNull<T> {
        self.0
    }

    pub const unsafe fn as_ref(&self) -> &T {
        unsafe { self.0.as_ref() }
    }

    pub unsafe fn into_box(self) -> Box<T> {
        let ptr = self.as_ptr().as_ptr();

        unsafe { Box::from_raw(ptr) }
    }
}
