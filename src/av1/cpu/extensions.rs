pub trait OptionError<E> {
    fn err_or<T>(self, ok: T) -> Result<T, E>;
}

impl<E> OptionError<E> for Option<E> {
    #[inline]
    fn err_or<T>(self, ok: T) -> Result<T, E> {
        match self {
            Some(e) => Err(e),
            None => Ok(ok),
        }
    }
}

pub trait To {
    fn to<T>(self) -> T
    where
        Self: Into<T>,
    {
        self.into()
    }

    fn try_to<T>(self) -> Result<T, <Self as TryInto<T>>::Error>
    where
        Self: TryInto<T>,
    {
        self.try_into()
    }
}

impl<T> To for T {}
