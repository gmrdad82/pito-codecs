mod nal;
mod types;
pub mod wav;

pub use nal::length_prefixed_to_annexb;
pub use types::*;

pub fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
}

#[cfg(any(
    feature = "hevc",
    feature = "vp9",
    feature = "gif",
    feature = "webp",
    feature = "opus"
))]
pub(crate) mod isa;

#[cfg(any(feature = "hevc", feature = "vp9"))]
pub(crate) fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16)
}

#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod front;

#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
pub use front::*;
