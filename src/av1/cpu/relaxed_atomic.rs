use std::sync::atomic::Ordering;

use crate::av1::cpu::atom::{Atom, Atomic};

#[derive(Default)]
pub struct RelaxedAtomic<T: Atom> {
    inner: Atomic<T>,
}

impl<T: Atom> RelaxedAtomic<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Atomic::new(value),
        }
    }

    pub fn get(&self) -> T {
        self.inner.load(Ordering::Relaxed)
    }

    pub fn set(&self, value: T) {
        self.inner.store(value, Ordering::Relaxed);
    }

    pub fn update(&self, f: impl Fn(T) -> T) {
        self.set(f(self.get()));
    }

    pub fn inner(&self) -> &Atomic<T> {
        &self.inner
    }
}

impl<T: Atom + Copy> RelaxedAtomic<T> {
    pub fn get_update(&self, f: impl Fn(T) -> T) -> T {
        let old = self.get();
        self.set(f(old));
        old
    }
}

impl<T: Atom> Clone for RelaxedAtomic<T> {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl<T: Atom> From<T> for RelaxedAtomic<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
