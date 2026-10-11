pub mod deblock;
mod mc;
mod mc_avx2;

pub use mc::{mc_chroma2, mc_chroma2_avg, mc_luma, mc_luma_avg, weighted_store};

pub fn avx2() -> bool {
    use std::sync::LazyLock;
    static AVX2: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("PITO_CODECS_NO_SIMD").is_none() && is_x86_feature_detected!("avx2")
    });
    *AVX2
}
