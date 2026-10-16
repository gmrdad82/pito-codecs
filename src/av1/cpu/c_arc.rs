#![deny(unsafe_op_in_unsafe_fn)]

use std::marker::PhantomData;
use std::ops::Deref;
use std::pin::Pin;
use std::ptr::NonNull;
use std::slice::SliceIndex;
use std::sync::Arc;

use crate::av1::cpu::c_box::CRef;
use crate::av1::cpu::error::Rav1dResult;

pub fn arc_into_raw<T: ?Sized>(arc: Arc<T>) -> NonNull<T> {
    let raw = Arc::into_raw(arc).cast_mut();

    unsafe { NonNull::new_unchecked(raw) }
}

pub struct CArc<T: ?Sized + 'static> {
    owner: Arc<Pin<CRef<T>>>,

    #[cfg(debug_assertions)]
    base_stable_ref: StableRef<T>,

    stable_ref: StableRef<T>,
}

#[derive(Debug)]
struct StableRef<T: ?Sized>(NonNull<T>);

impl<T: ?Sized> Clone for StableRef<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for StableRef<T> {}

unsafe impl<T: Send + ?Sized> Send for StableRef<T> {}

unsafe impl<T: Send + ?Sized> Sync for StableRef<T> {}

impl<T: ?Sized> AsRef<T> for CArc<T> {
    fn as_ref(&self) -> &T {
        #[cfg(debug_assertions)]
        {
            use std::{mem, ptr};

            use crate::av1::cpu::extensions::To;

            let real_ref = (*self.owner).as_ref().get_ref();
            assert_eq!(real_ref.to::<NonNull<T>>(), self.base_stable_ref.0);

            let real_ptr = ptr::from_ref(real_ref);
            let stable_ptr = self.stable_ref.0.as_ptr().cast_const();

            let [real_address, stable_address] =
                [real_ptr, stable_ptr].map(|ptr| ptr.cast::<()>() as isize);
            let offset = stable_address - real_address;
            let len = mem::size_of_val(real_ref);
            if offset < 0 || offset > len as isize {
                panic!(
                    "CArc::stable_ref is out of bounds:
    real_ref: {real_ptr:?}
    stable_ref: {stable_ptr:?}
    offset: {offset}
    len: {len}"
                );
            }
        }

        unsafe { self.stable_ref.0.as_ref() }
    }
}

impl<T: ?Sized> Deref for CArc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T: ?Sized> Clone for CArc<T> {
    fn clone(&self) -> Self {
        let Self {
            owner,
            #[cfg(debug_assertions)]
            base_stable_ref,
            stable_ref,
        } = self;
        Self {
            owner: owner.clone(),
            #[cfg(debug_assertions)]
            base_stable_ref: base_stable_ref.clone(),
            stable_ref: stable_ref.clone(),
        }
    }
}

impl<T: ?Sized> From<Arc<Pin<CRef<T>>>> for CArc<T> {
    fn from(owner: Arc<Pin<CRef<T>>>) -> Self {
        let stable_ref = StableRef((*owner).as_ref().get_ref().into());
        Self {
            owner,
            #[cfg(debug_assertions)]
            base_stable_ref: stable_ref,
            stable_ref,
        }
    }
}

impl<T: ?Sized> CArc<T> {
    pub fn wrap(owner: CRef<T>) -> Rav1dResult<Self> {
        let owner = Arc::new(owner.into_pin());
        Ok(owner.into())
    }
}

#[repr(transparent)]
pub struct RawArc<T>(NonNull<PhantomData<T>>);

impl<T> Clone for RawArc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for RawArc<T> {}

impl<T> RawArc<T> {
    pub fn from_arc(arc: Arc<T>) -> Self {
        Self(arc_into_raw(arc).cast())
    }

    pub unsafe fn into_arc(self) -> Arc<T> {
        let raw = self.0.cast().as_ptr();

        unsafe { Arc::from_raw(raw) }
    }
}

#[repr(transparent)]
pub struct RawCArc<T: ?Sized + 'static>(RawArc<Pin<CRef<T>>>);

impl<T: ?Sized> CArc<T> {
    pub fn into_raw(self) -> RawCArc<T> {
        RawCArc(RawArc::from_arc(self.owner))
    }

    pub unsafe fn from_raw(raw: RawCArc<T>) -> Self {
        let owner = unsafe { raw.0.into_arc() };
        owner.into()
    }
}

impl<T> CArc<[T]> {
    pub fn slice_in_place<I>(&mut self, range: I)
    where
        I: SliceIndex<[T], Output = [T]>,
    {
        self.stable_ref = StableRef(self.as_ref()[range].into());
    }

    pub fn split_at(this: Self, mid: usize) -> (Self, Self) {
        let mut first = this.clone();
        let mut second = this;
        first.slice_in_place(..mid);
        second.slice_in_place(mid..);
        (first, second)
    }
}

impl<T> CArc<[T]>
where
    T: Default + 'static,
{
    pub fn zeroed_slice(size: usize) -> Rav1dResult<Self> {
        let owned_slice = (0..size).map(|_| Default::default()).collect::<Box<[_]>>();
        Self::wrap(CRef::Box(owned_slice))
    }
}
