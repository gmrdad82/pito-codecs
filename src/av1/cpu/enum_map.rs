use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

pub trait EnumKey<const N: usize>: Sized + Copy {
    const VALUES: [Self; N];

    fn as_usize(self) -> usize;
}

pub trait DefaultValue {
    const DEFAULT: Self;
}

impl<T> DefaultValue for Option<T> {
    const DEFAULT: Self = None;
}

pub struct EnumMap<K, V, const N: usize>
where
    K: EnumKey<N>,
{
    array: [V; N],
    _phantom: PhantomData<K>,
}

macro_rules! enum_map_ty {
    ($K:ty, $V:ty) => {
        $crate::av1::cpu::enum_map::EnumMap<$K, $V, { <$K as $crate::av1::cpu::enums::EnumCount>::COUNT }>
    }
}

pub(crate) use enum_map_ty;

impl<K, V, const N: usize> EnumMap<K, V, N>
where
    K: EnumKey<N>,
{
    pub const fn new(array: [V; N]) -> Self {
        Self {
            array,
            _phantom: PhantomData,
        }
    }
}

impl<K, V, const N: usize> EnumMap<K, V, N>
where
    K: EnumKey<N>,
    V: DefaultValue,
{
    #[allow(dead_code)]
    const fn default() -> Self {
        Self {
            array: [V::DEFAULT; N],
            _phantom: PhantomData,
        }
    }
}

impl<K, V, const N: usize> Index<K> for EnumMap<K, V, N>
where
    K: EnumKey<N>,
{
    type Output = V;

    fn index(&self, index: K) -> &Self::Output {
        &self.array[index.as_usize()]
    }
}

impl<K, V, const N: usize> IndexMut<K> for EnumMap<K, V, N>
where
    K: EnumKey<N>,
{
    fn index_mut(&mut self, index: K) -> &mut Self::Output {
        &mut self.array[index.as_usize()]
    }
}

macro_rules! enum_map {
    ($K:ty => $V:ty; match key { $($t:tt)* }) => {{
        use $crate::av1::cpu::enum_map::EnumKey;
        use $crate::av1::cpu::enum_map::EnumMap;

        let mut a = [<$V>::DEFAULT; <$K>::VALUES.len()];
        let mut i = 0;
        while i < <$K>::VALUES.len() {
            let key = <$K>::VALUES[i];
            use $K::*;
            let value = match key { $($t)* };
            a[key as usize] = value;
            i += 1;
        }
        EnumMap::<$K, $V, { <$K>::VALUES.len() }>::new(a)
    }};
}

pub(crate) use enum_map;
