macro_rules! bitflags {
    (
        $(#[$m:meta])*
        $vis:vis struct $name:ident: $t:ty {
            $(const $f:ident = $e:expr;)*
        }
    ) => {
        $(#[$m])*
        $vis struct $name($t);

        $crate::av1::cpu::flags::bitflags! {
            impl $name: $t {
                $(const $f = $e;)*
            }
        }
    };
    (
        impl $name:ident: $t:ty {
            $(const $f:ident = $e:expr;)*
        }
    ) => {
        #[allow(dead_code)]
        impl $name {
            $(pub const $f: Self = Self($e);)*

            pub const fn bits(&self) -> $t {
                self.0
            }

            pub const fn empty() -> Self {
                Self(0)
            }

            pub const fn all() -> Self {
                Self(0 $(| Self::$f.0)*)
            }

            pub const fn from_bits_retain(bits: $t) -> Self {
                Self(bits)
            }

            pub const fn from_bits_truncate(bits: $t) -> Self {
                Self(bits & Self::all().0)
            }

            pub const fn is_empty(&self) -> bool {
                self.0 == 0
            }

            pub const fn contains(&self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            pub const fn intersects(&self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            pub const fn union(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }

            pub const fn intersection(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }

            pub const fn difference(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }

            pub fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }

            pub fn remove(&mut self, other: Self) {
                self.0 &= !other.0;
            }

            pub fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }
        }

        impl ::std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, other: Self) -> Self {
                self.union(other)
            }
        }

        impl ::std::ops::BitOrAssign for $name {
            fn bitor_assign(&mut self, other: Self) {
                self.insert(other);
            }
        }

        impl ::std::ops::BitAnd for $name {
            type Output = Self;
            fn bitand(self, other: Self) -> Self {
                self.intersection(other)
            }
        }

        impl ::std::ops::BitAndAssign for $name {
            fn bitand_assign(&mut self, other: Self) {
                self.0 &= other.0;
            }
        }

        impl ::std::ops::BitXor for $name {
            type Output = Self;
            fn bitxor(self, other: Self) -> Self {
                Self(self.0 ^ other.0)
            }
        }

        impl ::std::ops::Sub for $name {
            type Output = Self;
            fn sub(self, other: Self) -> Self {
                self.difference(other)
            }
        }

        impl ::std::ops::Not for $name {
            type Output = Self;
            fn not(self) -> Self {
                Self::from_bits_truncate(!self.0)
            }
        }
    };
}

pub(crate) use bitflags;
