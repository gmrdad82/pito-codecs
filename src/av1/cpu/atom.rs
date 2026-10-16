use std::sync::atomic::{
    AtomicBool, AtomicI8, AtomicI16, AtomicI32, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering,
};

pub trait Prim: Copy {
    type Cell: Send + Sync;
    fn cell(v: Self) -> Self::Cell;
    fn load(c: &Self::Cell, o: Ordering) -> Self;
    fn store(c: &Self::Cell, v: Self, o: Ordering);
    fn compare_exchange(
        c: &Self::Cell,
        current: Self,
        new: Self,
        success: Ordering,
        failure: Ordering,
    ) -> Result<Self, Self>;
}

macro_rules! prim {
    ($($t:ty => $a:ty),*) => {$(
        impl Prim for $t {
            type Cell = $a;
            fn cell(v: Self) -> $a {
                <$a>::new(v)
            }
            fn load(c: &$a, o: Ordering) -> Self {
                c.load(o)
            }
            fn store(c: &$a, v: Self, o: Ordering) {
                c.store(v, o)
            }
            fn compare_exchange(
                c: &$a,
                current: Self,
                new: Self,
                success: Ordering,
                failure: Ordering,
            ) -> Result<Self, Self> {
                c.compare_exchange(current, new, success, failure)
            }
        }
    )*};
}

prim!(bool => AtomicBool, u8 => AtomicU8, i8 => AtomicI8, u16 => AtomicU16, i16 => AtomicI16, u32 => AtomicU32, i32 => AtomicI32, u64 => AtomicU64);

pub trait Atom: Copy {
    type Repr: Prim;
    fn pack(self) -> Self::Repr;
    fn unpack(src: Self::Repr) -> Self;
}

macro_rules! atom_prim {
    ($($t:ty),*) => {$(
        impl Atom for $t {
            type Repr = $t;
            fn pack(self) -> $t {
                self
            }
            fn unpack(src: $t) -> $t {
                src
            }
        }
    )*};
}

atom_prim!(bool, u8, i8, u16, i16, u32, i32, u64);

impl Atom for [i8; 4] {
    type Repr = u32;
    fn pack(self) -> u32 {
        u32::from_ne_bytes(self.map(|b| b as u8))
    }
    fn unpack(src: u32) -> Self {
        src.to_ne_bytes().map(|b| b as i8)
    }
}

impl Atom for [i16; 4] {
    type Repr = u64;
    fn pack(self) -> u64 {
        let mut b = [0u8; 8];
        for (i, v) in self.iter().enumerate() {
            b[i * 2..i * 2 + 2].copy_from_slice(&v.to_ne_bytes());
        }
        u64::from_ne_bytes(b)
    }
    fn unpack(src: u64) -> Self {
        let b = src.to_ne_bytes();
        std::array::from_fn(|i| i16::from_ne_bytes([b[i * 2], b[i * 2 + 1]]))
    }
}

pub struct Atomic<T: Atom>(<T::Repr as Prim>::Cell);

impl<T: Atom> Atomic<T> {
    pub fn new(v: T) -> Self {
        Self(<T::Repr as Prim>::cell(v.pack()))
    }

    pub fn load(&self, o: Ordering) -> T {
        T::unpack(<T::Repr as Prim>::load(&self.0, o))
    }

    pub fn store(&self, v: T, o: Ordering) {
        <T::Repr as Prim>::store(&self.0, v.pack(), o)
    }

    pub fn compare_exchange(
        &self,
        current: T,
        new: T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<T, T> {
        <T::Repr as Prim>::compare_exchange(&self.0, current.pack(), new.pack(), success, failure)
            .map(T::unpack)
            .map_err(T::unpack)
    }
}

impl<T: Atom + Default> Default for Atomic<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}
