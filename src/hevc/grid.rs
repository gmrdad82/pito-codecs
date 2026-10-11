use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Range;

pub struct Grid<T> {
    ptr: *mut T,
    len: usize,
    cap: usize,
    owned: PhantomData<T>,
}

unsafe impl<T: Send> Send for Grid<T> {}
unsafe impl<T: Send> Sync for Grid<T> {}

impl<T> Default for Grid<T> {
    fn default() -> Self {
        Grid::from(Vec::new())
    }
}

impl<T> From<Vec<T>> for Grid<T> {
    fn from(v: Vec<T>) -> Self {
        let mut v = ManuallyDrop::new(v);
        Grid {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            cap: v.capacity(),
            owned: PhantomData,
        }
    }
}

impl<T> Drop for Grid<T> {
    fn drop(&mut self) {
        drop(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) });
    }
}

impl<T> Grid<T> {
    pub fn into_vec(self) -> Vec<T> {
        let me = ManuallyDrop::new(self);
        unsafe { Vec::from_raw_parts(me.ptr, me.len, me.cap) }
    }

    fn check(&self, r: &Range<usize>) {
        assert!(
            r.start <= r.end && r.end <= self.len,
            "grid range {r:?} out of {}",
            self.len
        );
    }

    pub fn slice(&self, r: Range<usize>) -> &[T] {
        self.check(&r);
        unsafe { std::slice::from_raw_parts(self.ptr.add(r.start), r.end - r.start) }
    }

    #[allow(clippy::mut_from_ref)]
    pub fn slice_mut(&self, r: Range<usize>) -> &mut [T] {
        self.check(&r);
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(r.start), r.end - r.start) }
    }
}

impl<T: Copy> Grid<T> {
    #[inline(always)]
    pub fn get(&self, i: usize) -> T {
        assert!(i < self.len, "grid index {i} out of {}", self.len);
        unsafe { self.ptr.add(i).read() }
    }

    #[inline(always)]
    pub fn set(&self, i: usize, v: T) {
        assert!(i < self.len, "grid index {i} out of {}", self.len);
        unsafe { self.ptr.add(i).write(v) }
    }
}
