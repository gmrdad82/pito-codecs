macro_rules! wrap_fn_ptr {
    ($vis:vis unsafe extern "C" fn $name:ident(
            $($arg_name:ident: $arg_ty:ty),*$(,)?
    ) -> $return_ty:ty) => {
        $vis mod $name {
            use $crate::av1::cpu::enum_map::DefaultValue;
            use super::*;

            pub type FnPtr = unsafe extern "C" fn($($arg_name: $arg_ty),*) -> $return_ty;

            #[derive(Clone, Copy)]
            #[repr(transparent)]
            pub struct Fn(FnPtr);

            impl Fn {
                pub(super) const fn new(fn_ptr: FnPtr) -> Self {
                    Self(fn_ptr)
                }

                pub(super) const fn get(&self) -> &FnPtr {
                    &self.0
                }
            }

            impl DefaultValue for Fn {
                const DEFAULT: Self = {
                    extern "C" fn default_unimplemented(
                        $($arg_name: $arg_ty,)*
                    ) -> $return_ty {
                        $(let _ = $arg_name;)*
                        unimplemented!()
                    }
                    Self::new(default_unimplemented)
                };
            }


        }
    };
}

pub(crate) use wrap_fn_ptr;
