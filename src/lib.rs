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
#[cfg(feature = "alac")]
pub mod alac;
#[cfg(feature = "mp3")]
pub mod mp3;

#[cfg(feature = "flac")]
pub mod flac;

#[cfg(feature = "mp4")]
pub mod mp4;

#[cfg(feature = "gpu")]
pub mod gpu;

#[cfg(any(
    feature = "h264",
    feature = "hevc",
    feature = "gpu",
    feature = "aac",
    feature = "av1"
))]
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

#[cfg(feature = "vorbis")]
pub mod vorbis;

#[cfg(feature = "vp9")]
pub mod vp9;

#[cfg(feature = "av1")]
pub mod av1;

#[cfg(feature = "webp")]
pub mod webp;

#[cfg(feature = "gif")]
pub mod gif;

#[cfg(feature = "png")]
pub mod png;

#[cfg(feature = "jpeg")]
pub mod jpeg;

#[cfg(test)]
mod fixture;

#[cfg(test)]
mod fixture_tests;
