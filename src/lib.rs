#[cfg(feature = "media")]
pub mod media;

#[cfg(feature = "h264")]
pub mod h264;

#[cfg(feature = "hevc")]
pub mod hevc;

#[cfg(feature = "aac")]
pub mod aac;

#[cfg(feature = "ac3")]
pub mod ac3;
#[cfg(feature = "mp3")]
pub mod mp3;

#[cfg(feature = "mp4")]
pub mod mp4;

#[cfg(feature = "gpu")]
pub mod gpu;

#[cfg(any(feature = "h264", feature = "hevc", feature = "gpu", feature = "aac"))]
#[cfg_attr(
    not(all(
        feature = "h264",
        feature = "hevc",
        feature = "gpu",
        target_os = "linux"
    )),
    allow(dead_code)
)]
mod syntax;

#[cfg(feature = "filter")]
pub mod filter;

#[cfg(feature = "mkv")]
pub mod mkv;

#[cfg(feature = "opus")]
pub mod opus;

#[cfg(feature = "vp9")]
pub mod vp9;

#[cfg(feature = "webp")]
pub mod webp;

#[cfg(feature = "gif")]
pub mod gif;

#[cfg(feature = "png")]
pub mod png;
