#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::UnsafeCell;
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::{
    Deref, DerefMut, Index, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive,
};
use std::ptr::addr_of_mut;
use std::sync::Arc;
use std::{fmt, mem, ptr};

use crate::av1::cpu::pod::{AsBytes, FromBytes};

use crate::av1::cpu::align::{AlignedByteChunk, AlignedVec};

#[derive(Default)]
#[cfg_attr(not(debug_assertions), repr(transparent))]
pub struct DisjointMut<T: ?Sized + AsMutPtr> {
    #[cfg(debug_assertions)]
    bounds: debug::DisjointMutAllBounds,

    inner: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + AsMutPtr + Send> Send for DisjointMut<T> {}

unsafe impl<T: ?Sized + AsMutPtr + Sync> Sync for DisjointMut<T> {}

impl<T: AsMutPtr> DisjointMut<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
            #[cfg(debug_assertions)]
            bounds: debug::DisjointMutAllBounds::new(),
        }
    }

    pub const fn inner(&self) -> *mut T {
        self.inner.get()
    }

    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

#[cfg_attr(not(debug_assertions), repr(transparent))]
pub struct DisjointMutGuard<'a, T: ?Sized + AsMutPtr, V: ?Sized> {
    slice: &'a mut V,

    phantom: PhantomData<&'a DisjointMut<T>>,

    #[cfg(debug_assertions)]
    parent: &'a DisjointMut<T>,
    #[cfg(debug_assertions)]
    bounds: debug::DisjointMutBounds,
}

impl<'a, T: AsMutPtr> DisjointMutGuard<'a, T, [u8]> {
    #[inline]
    fn cast_slice<V: AsBytes + FromBytes>(self) -> DisjointMutGuard<'a, T, [V]> {
        let mut old_guard = ManuallyDrop::new(self);
        let bytes = mem::take(&mut old_guard.slice);
        DisjointMutGuard {
            slice: V::mut_slice_from(bytes).unwrap(),
            phantom: old_guard.phantom,
            #[cfg(debug_assertions)]
            parent: old_guard.parent,
            #[cfg(debug_assertions)]
            bounds: mem::take(&mut old_guard.bounds),
        }
    }

    #[inline]
    fn cast<V: AsBytes + FromBytes>(self) -> DisjointMutGuard<'a, T, V> {
        let mut old_guard = ManuallyDrop::new(self);
        let bytes = mem::take(&mut old_guard.slice);
        DisjointMutGuard {
            slice: V::mut_from(bytes).unwrap(),
            phantom: old_guard.phantom,
            #[cfg(debug_assertions)]
            parent: old_guard.parent,
            #[cfg(debug_assertions)]
            bounds: mem::take(&mut old_guard.bounds),
        }
    }
}

impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> Deref for DisjointMutGuard<'a, T, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DerefMut for DisjointMutGuard<'a, T, V> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.slice
    }
}

#[cfg_attr(not(debug_assertions), repr(transparent))]
pub struct DisjointImmutGuard<'a, T: ?Sized + AsMutPtr, V: ?Sized> {
    slice: &'a V,

    phantom: PhantomData<&'a DisjointMut<T>>,

    #[cfg(debug_assertions)]
    parent: &'a DisjointMut<T>,
    #[cfg(debug_assertions)]
    bounds: debug::DisjointMutBounds,
}

impl<'a, T: AsMutPtr> DisjointImmutGuard<'a, T, [u8]> {
    #[inline]
    fn cast_slice<V: FromBytes>(self) -> DisjointImmutGuard<'a, T, [V]> {
        let mut old_guard = ManuallyDrop::new(self);
        let bytes = mem::take(&mut old_guard.slice);
        DisjointImmutGuard {
            slice: V::slice_from(bytes).unwrap(),
            phantom: old_guard.phantom,
            #[cfg(debug_assertions)]
            parent: old_guard.parent,
            #[cfg(debug_assertions)]
            bounds: mem::take(&mut old_guard.bounds),
        }
    }

    #[inline]
    fn cast<V: FromBytes>(self) -> DisjointImmutGuard<'a, T, V> {
        let mut old_guard = ManuallyDrop::new(self);
        let bytes = mem::take(&mut old_guard.slice);
        DisjointImmutGuard {
            slice: V::ref_from(bytes).unwrap(),
            phantom: old_guard.phantom,
            #[cfg(debug_assertions)]
            parent: old_guard.parent,
            #[cfg(debug_assertions)]
            bounds: mem::take(&mut old_guard.bounds),
        }
    }
}

impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> Deref for DisjointImmutGuard<'a, T, V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.slice
    }
}

impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DisjointImmutGuard<'a, T, V> {
    pub unsafe fn get_unchecked(&self) -> &'a V {
        self.slice
    }
}

pub unsafe trait AsMutPtr {
    type Target;

    unsafe fn as_mut_slice(ptr: *mut Self) -> *mut [Self::Target] {
        let len = unsafe { (*ptr).len() };

        let data = unsafe { Self::as_mut_ptr(ptr) };
        ptr::slice_from_raw_parts_mut(data, len)
    }

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target;

    fn len(&self) -> usize;
}

impl<T: ?Sized + AsMutPtr> DisjointMut<T> {
    pub fn len(&self) -> usize {
        unsafe { (*self.inner.get()).len() }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn as_mut_slice(&self) -> *mut [<T as AsMutPtr>::Target] {
        unsafe { AsMutPtr::as_mut_slice(self.inner.get()) }
    }

    pub fn as_mut_ptr(&self) -> *mut <T as AsMutPtr>::Target {
        unsafe { AsMutPtr::as_mut_ptr(self.inner.get()) }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index_mut<'a, I>(&'a self, index: I) -> DisjointMutGuard<'a, T, I::Output>
    where
        I: Into<Bounds> + Clone,
        I: DisjointMutIndex<[<T as AsMutPtr>::Target]>,
    {
        let bounds = index.clone().into();

        let slice = unsafe { &mut *index.get_mut(self.as_mut_slice()) };
        DisjointMutGuard::new(self, slice, bounds)
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn index<'a, I>(&'a self, index: I) -> DisjointImmutGuard<'a, T, I::Output>
    where
        I: Into<Bounds> + Clone,
        I: DisjointMutIndex<[<T as AsMutPtr>::Target]>,
    {
        let bounds = index.clone().into();

        let slice = unsafe { &*index.get_mut(self.as_mut_slice()).cast_const() };
        DisjointImmutGuard::new(self, slice, bounds)
    }
}

impl<T: AsMutPtr<Target = u8>> DisjointMut<T> {
    #[inline]
    fn check_cast_slice_len<I, V>(&self, index: I, slice: &[V])
    where
        I: SliceBounds,
    {
        let range = index.to_range(self.len() / mem::size_of::<V>());
        let range_len = range.end - range.start;
        assert!(slice.len() == range_len);
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn mut_slice_as<'a, I, V>(&'a self, index: I) -> DisjointMutGuard<'a, T, [V]>
    where
        I: SliceBounds,
        V: AsBytes + FromBytes,
    {
        let slice = self.index_mut(index.mul(mem::size_of::<V>())).cast_slice();
        self.check_cast_slice_len(index, &slice);
        slice
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn mut_element_as<'a, V>(&'a self, index: usize) -> DisjointMutGuard<'a, T, V>
    where
        V: AsBytes + FromBytes,
    {
        self.index_mut((index..index + 1).mul(mem::size_of::<V>()))
            .cast()
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn slice_as<'a, I, V>(&'a self, index: I) -> DisjointImmutGuard<'a, T, [V]>
    where
        I: SliceBounds,
        V: FromBytes,
    {
        let slice = self.index(index.mul(mem::size_of::<V>())).cast_slice();
        self.check_cast_slice_len(index, &slice);
        slice
    }

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    pub fn element_as<'a, V>(&'a self, index: usize) -> DisjointImmutGuard<'a, T, V>
    where
        V: FromBytes,
    {
        self.index((index..index + 1).mul(mem::size_of::<V>()))
            .cast()
    }
}

pub trait DisjointMutIndex<T: ?Sized> {
    type Output: ?Sized;

    unsafe fn get_mut(self, slice: *mut T) -> *mut Self::Output;
}

pub trait TranslateRange {
    fn mul(&self, by: usize) -> Self;
}

impl TranslateRange for usize {
    fn mul(&self, by: usize) -> Self {
        *self * by
    }
}

impl TranslateRange for Range<usize> {
    fn mul(&self, by: usize) -> Self {
        self.start * by..self.end * by
    }
}

impl TranslateRange for RangeFrom<usize> {
    fn mul(&self, by: usize) -> Self {
        self.start * by..
    }
}

impl TranslateRange for RangeInclusive<usize> {
    fn mul(&self, by: usize) -> Self {
        *self.start() * by..=*self.end() * by
    }
}

impl TranslateRange for RangeTo<usize> {
    fn mul(&self, by: usize) -> Self {
        ..self.end * by
    }
}

impl TranslateRange for RangeToInclusive<usize> {
    fn mul(&self, by: usize) -> Self {
        ..=self.end * by
    }
}

impl TranslateRange for RangeFull {
    fn mul(&self, _by: usize) -> Self {
        *self
    }
}

impl TranslateRange for (RangeFrom<usize>, RangeTo<usize>) {
    fn mul(&self, by: usize) -> Self {
        (self.0.start * by.., ..self.1.end * by)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Bounds {
    range: Range<usize>,
}

impl Display for Bounds {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Range { start, end } = self.range;
        if start != 0 {
            write!(f, "{start}")?;
        }
        write!(f, "..")?;
        if end != usize::MAX {
            write!(f, "{end}")?;
        }
        Ok(())
    }
}

impl Debug for Bounds {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "{}", self)
    }
}

impl Bounds {
    #[cfg(debug_assertions)]
    fn overlaps(&self, other: &Bounds) -> bool {
        let a = &self.range;
        let b = &other.range;
        a.start < b.end && b.start < a.end
    }
}

impl From<usize> for Bounds {
    fn from(index: usize) -> Self {
        Self {
            range: index..index + 1,
        }
    }
}

impl<T: SliceBounds> From<T> for Bounds {
    fn from(range: T) -> Self {
        Self {
            range: range.to_range(usize::MAX),
        }
    }
}

pub trait SliceBounds: TranslateRange + Clone {
    fn to_range(&self, len: usize) -> Range<usize>;
}

impl SliceBounds for Range<usize> {
    fn to_range(&self, _len: usize) -> Range<usize> {
        let Self { start, end } = *self;
        start..end
    }
}

impl SliceBounds for RangeFrom<usize> {
    fn to_range(&self, len: usize) -> Range<usize> {
        let Self { start } = *self;
        start..len
    }
}

impl SliceBounds for RangeInclusive<usize> {
    fn to_range(&self, _len: usize) -> Range<usize> {
        *self.start()..*self.end() + 1
    }
}

impl SliceBounds for RangeTo<usize> {
    fn to_range(&self, _len: usize) -> Range<usize> {
        let Self { end } = *self;
        0..end
    }
}

impl SliceBounds for RangeToInclusive<usize> {
    fn to_range(&self, _len: usize) -> Range<usize> {
        let Self { end } = *self;
        0..end + 1
    }
}

impl SliceBounds for RangeFull {
    fn to_range(&self, len: usize) -> Range<usize> {
        0..len
    }
}

impl SliceBounds for (RangeFrom<usize>, RangeTo<usize>) {
    fn to_range(&self, _len: usize) -> Range<usize> {
        let (RangeFrom { start }, RangeTo { end: range_len }) = *self;
        start..start + range_len
    }
}

impl<T> DisjointMutIndex<[T]> for usize {
    type Output = <[T] as Index<usize>>::Output;

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    unsafe fn get_mut(self, slice: *mut [T]) -> *mut Self::Output {
        let index = self;
        let len = slice.len();
        if index < len {
            unsafe { (slice as *mut T).add(index) }
        } else {
            #[inline(never)]
            #[cfg_attr(debug_assertions, track_caller)]
            fn out_of_bounds(index: usize, len: usize) -> ! {
                panic!("index out of bounds: the len is {len} but the index is {index}")
            }
            out_of_bounds(index, len);
        }
    }
}

impl<T, I> DisjointMutIndex<[T]> for I
where
    I: SliceBounds,
{
    type Output = <[T] as Index<Range<usize>>>::Output;

    #[inline]
    #[cfg_attr(debug_assertions, track_caller)]
    unsafe fn get_mut(self, slice: *mut [T]) -> *mut Self::Output {
        let len = slice.len();
        let Range { start, end } = self.to_range(len);
        if start <= end && end <= len {
            let data = unsafe { (slice as *mut T).add(start) };
            ptr::slice_from_raw_parts_mut(data, end - start)
        } else {
            #[inline(never)]
            #[cfg_attr(debug_assertions, track_caller)]
            fn out_of_bounds(start: usize, end: usize, len: usize) -> ! {
                if start > end {
                    panic!("slice index starts at {start} but ends at {end}");
                }
                if end > len {
                    panic!("range end index {end} out of range for slice of length {len}");
                }
                unreachable!();
            }
            out_of_bounds(start, end, len);
        }
    }
}

#[cfg(not(debug_assertions))]
mod release {
    use super::*;

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DisjointMutGuard<'a, T, V> {
        pub fn new(_parent: &'a DisjointMut<T>, slice: &'a mut V, _bounds: Bounds) -> Self {
            Self {
                slice,
                phantom: PhantomData,
            }
        }
    }

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DisjointImmutGuard<'a, T, V> {
        pub fn new(_parent: &'a DisjointMut<T>, slice: &'a V, _bounds: Bounds) -> Self {
            Self {
                slice,
                phantom: PhantomData,
            }
        }
    }
}

#[cfg(debug_assertions)]
mod debug {
    use std::backtrace::{Backtrace, BacktraceStatus};
    use std::fmt::Debug;
    use std::panic::Location;
    use std::thread;
    use std::thread::ThreadId;

    use crate::av1::cpu::sync::Mutex;

    use super::*;

    #[derive(Debug)]
    pub(super) struct DisjointMutBounds {
        bounds: Bounds,
        mutable: bool,
        location: &'static Location<'static>,
        backtrace: Backtrace,
        thread: ThreadId,
    }

    impl Default for DisjointMutBounds {
        fn default() -> Self {
            Self {
                bounds: Default::default(),
                mutable: Default::default(),
                location: Location::caller(),
                backtrace: Backtrace::disabled(),
                thread: thread::current().id(),
            }
        }
    }

    impl PartialEq for DisjointMutBounds {
        fn eq(&self, other: &Self) -> bool {
            self.bounds == other.bounds
                && self.mutable == other.mutable
                && self.location == other.location
                && self.thread == other.thread
        }
    }

    impl DisjointMutBounds {
        #[track_caller]
        pub fn new(bounds: Bounds, mutable: bool) -> Self {
            Self {
                bounds,
                mutable,
                location: Location::caller(),
                backtrace: Backtrace::capture(),
                thread: thread::current().id(),
            }
        }

        pub fn check_overlaps(&self, existing: &Self) {
            if !self.bounds.overlaps(&existing.bounds) {
                return;
            }

            panic!("\toverlapping DisjointMut:\n current: {self}\nexisting: {existing}");
        }
    }

    impl Display for DisjointMutBounds {
        fn fmt(&self, f: &mut Formatter) -> fmt::Result {
            let Self {
                bounds,
                mutable,
                location,
                backtrace,
                thread,
            } = self;
            let mutable = if *mutable { "&mut" } else { "   &" };
            write!(f, "{mutable} _[{bounds}] on {thread:?} at {location}")?;
            if backtrace.status() == BacktraceStatus::Captured {
                write!(f, ":\nstack backtrace:\n{backtrace}")?;
            }
            Ok(())
        }
    }

    #[derive(Default)]
    pub struct DisjointMutAllBounds {
        mutable: Mutex<Vec<DisjointMutBounds>>,

        immutable: Mutex<Vec<DisjointMutBounds>>,
    }

    impl DisjointMutAllBounds {
        pub const fn new() -> Self {
            Self {
                mutable: Mutex::new(Vec::new()),
                immutable: Mutex::new(Vec::new()),
            }
        }
    }

    impl<T: ?Sized + AsMutPtr> DisjointMut<T> {
        #[track_caller]
        fn add_mut_bounds(&self, current: DisjointMutBounds) {
            for existing in self.bounds.immutable.lock().iter() {
                current.check_overlaps(existing);
            }
            let mut mut_bounds = self.bounds.mutable.lock();
            for existing in mut_bounds.iter() {
                current.check_overlaps(existing);
            }
            mut_bounds.push(current);
        }

        #[track_caller]
        fn add_immut_bounds(&self, current: DisjointMutBounds) {
            let mut_bounds = self.bounds.mutable.lock();
            for existing in mut_bounds.iter() {
                current.check_overlaps(existing);
            }
            self.bounds.immutable.lock().push(current);
        }

        fn remove_bound(&self, bounds: &DisjointMutBounds) {
            let mut all_bounds = if bounds.mutable {
                self.bounds.mutable.lock()
            } else {
                self.bounds.immutable.lock()
            };
            let idx = all_bounds
                .iter()
                .position(|r| r == bounds)
                .expect("Expected range {range:?} to be in the active ranges");
            all_bounds.remove(idx);
        }
    }

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DisjointMutGuard<'a, T, V> {
        #[track_caller]
        pub fn new(parent: &'a DisjointMut<T>, slice: &'a mut V, bounds: Bounds) -> Self {
            parent.add_mut_bounds(DisjointMutBounds::new(bounds.clone(), true));
            let bounds = DisjointMutBounds::new(bounds, true);
            Self {
                parent,
                slice,
                bounds,
                phantom: PhantomData,
            }
        }
    }

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> Drop for DisjointMutGuard<'a, T, V> {
        fn drop(&mut self) {
            self.parent.remove_bound(&self.bounds);
        }
    }

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> DisjointImmutGuard<'a, T, V> {
        #[track_caller]
        pub fn new(parent: &'a DisjointMut<T>, slice: &'a V, bounds: Bounds) -> Self {
            parent.add_immut_bounds(DisjointMutBounds::new(bounds.clone(), false));
            let bounds = DisjointMutBounds::new(bounds, false);
            Self {
                parent,
                slice,
                bounds,
                phantom: PhantomData,
            }
        }
    }

    impl<'a, T: ?Sized + AsMutPtr, V: ?Sized> Drop for DisjointImmutGuard<'a, T, V> {
        fn drop(&mut self) {
            self.parent.remove_bound(&self.bounds);
        }
    }
}

impl<V: Clone> DisjointMut<Vec<V>> {
    pub fn resize(&mut self, new_len: usize, value: V) {
        self.inner.get_mut().resize(new_len, value)
    }
}

impl<V> DisjointMut<Vec<V>> {
    pub fn clear(&mut self) {
        self.inner.get_mut().clear()
    }

    pub fn resize_with<F>(&mut self, new_len: usize, f: F)
    where
        F: FnMut() -> V,
    {
        self.inner.get_mut().resize_with(new_len, f)
    }
}

unsafe impl<V> AsMutPtr for Vec<V> {
    type Target = V;

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target {
        unsafe { (*ptr).as_mut_ptr() }
    }

    fn len(&self) -> usize {
        self.len()
    }
}

unsafe impl<V, const N: usize> AsMutPtr for [V; N] {
    type Target = V;

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut V {
        ptr.cast()
    }

    fn len(&self) -> usize {
        N
    }
}

unsafe impl<V> AsMutPtr for [V] {
    type Target = V;

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target {
        ptr.cast()
    }

    fn len(&self) -> usize {
        self.len()
    }
}

unsafe impl<V> AsMutPtr for Box<[V]> {
    type Target = V;

    unsafe fn as_mut_ptr(ptr: *mut Self) -> *mut Self::Target {
        unsafe { addr_of_mut!(**ptr) }.cast()
    }

    fn len(&self) -> usize {
        (**self).len()
    }
}

impl<V: Copy, C: AlignedByteChunk> DisjointMut<AlignedVec<V, C>> {
    pub fn resize(&mut self, new_len: usize, value: V) {
        self.inner.get_mut().resize(new_len, value)
    }
}

#[cfg(debug_assertions)]
pub type DisjointMutSlice<T> = DisjointMut<Box<[T]>>;

#[cfg(not(debug_assertions))]
pub type DisjointMutSlice<T> = DisjointMut<[T]>;

#[derive(Clone)]
pub struct DisjointMutArcSlice<T> {
    pub inner: Arc<DisjointMutSlice<T>>,
}

impl<T> FromIterator<T> for DisjointMutArcSlice<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        #[cfg(debug_assertions)]
        let inner = {
            let box_slice = iter.into_iter().collect::<Box<[_]>>();
            Arc::new(DisjointMut::new(box_slice))
        };
        #[cfg(not(debug_assertions))]
        let inner = {
            use std::mem;

            let arc_slice = iter.into_iter().collect::<Arc<[_]>>();

            const {
                type A = Vec<u8>;
                assert!(mem::size_of::<DisjointMut<A>>() == mem::size_of::<A>());
                assert!(mem::align_of::<DisjointMut<A>>() == mem::align_of::<A>());
            }

            unsafe { Arc::from_raw(Arc::into_raw(arc_slice) as *const DisjointMut<[_]>) }
        };
        Self { inner }
    }
}

impl<T> Default for DisjointMutArcSlice<T> {
    fn default() -> Self {
        [].into_iter().collect()
    }
}
