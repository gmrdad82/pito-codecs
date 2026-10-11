#[cfg(target_arch = "x86_64")]
pub fn avx2() -> bool {
    use std::sync::LazyLock;
    static AVX2: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("PITO_CODECS_NO_SIMD").is_none()
            && is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("bmi1")
            && is_x86_feature_detected!("bmi2")
            && is_x86_feature_detected!("lzcnt")
            && is_x86_feature_detected!("popcnt")
            && is_x86_feature_detected!("fma")
    });
    *AVX2
}

macro_rules! dispatch {
    ($(#[$m:meta])* $vis:vis fn $name:ident ($($a:ident: $t:ty),* $(,)?) $(-> $r:ty)? $body:block) => {
        $(#[$m])*
        $vis fn $name($($a: $t),*) $(-> $r)? {
            #[inline(always)]
            fn body($($a: $t),*) $(-> $r)? $body
            #[cfg(target_arch = "x86_64")]
            #[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt,fma")]
            fn fast($($a: $t),*) $(-> $r)? {
                body($($a),*)
            }
            #[cfg(target_arch = "x86_64")]
            if crate::vp9::enc::pred::isa::avx2() {
                return unsafe { fast($($a),*) };
            }
            body($($a),*)
        }
    };
}
pub(crate) use dispatch;
