pub mod deblock;
mod mc;
mod mc_avx2;

pub use mc::{Avx2, Sse2};

pub fn avx2() -> bool {
    use std::sync::LazyLock;
    static AVX2: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("PITO_CODECS_NO_SIMD").is_none() && is_x86_feature_detected!("avx2")
    });
    *AVX2
}
