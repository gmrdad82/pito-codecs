use std::ops::{Deref, DerefMut};
use std::sync::{self as s, PoisonError, TryLockError};

pub use std::sync::{RwLockReadGuard, RwLockWriteGuard};

#[derive(Default)]
pub struct Mutex<T: ?Sized>(s::Mutex<T>);

pub struct MutexGuard<'a, T: ?Sized>(Option<s::MutexGuard<'a, T>>);

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self(s::Mutex::new(value))
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard(Some(self.0.lock().unwrap_or_else(PoisonError::into_inner)))
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(g) => Some(MutexGuard(Some(g))),
            Err(TryLockError::Poisoned(e)) => Some(MutexGuard(Some(e.into_inner()))),
            Err(TryLockError::WouldBlock) => None,
        }
    }
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match &self.0 {
            Some(g) => g,
            None => unreachable!(),
        }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        match &mut self.0 {
            Some(g) => g,
            None => unreachable!(),
        }
    }
}

#[derive(Default)]
pub struct RwLock<T: ?Sized>(s::RwLock<T>);

impl<T> RwLock<T> {}

impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.0.try_read() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        match self.0.try_write() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Default)]
pub struct Condvar(s::Condvar);

impl Condvar {
    pub const fn new() -> Self {
        Self(s::Condvar::new())
    }

    pub fn wait<T>(&self, guard: &mut MutexGuard<'_, T>) {
        if let Some(g) = guard.0.take() {
            guard.0 = Some(self.0.wait(g).unwrap_or_else(PoisonError::into_inner));
        }
    }

    pub fn notify_one(&self) -> bool {
        self.0.notify_one();
        true
    }

    pub fn notify_all(&self) -> usize {
        self.0.notify_all();
        0
    }
}
