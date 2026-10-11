mod boxes;
mod entry;
mod reader;
mod remux;
mod source;
mod writer;

pub use boxes::FourCc;
pub use entry::{
    AacConfig, AudioParams, AvcConfig, BitRate, CodecConfig, HevcConfig, NalArray, PcmConfig,
    SampleEntry, VideoParams,
};
pub use reader::{Edit, Reader, Sample, Track, TrackKind};
pub use remux::{Cut, Snap, Span, copy_samples, plan, remux};
pub use source::ByteSource;
pub use writer::{Brand, FragmentedWriter, Options, TrackConfig, Writer, rotation_matrix};

use crate::media::Error;

pub(crate) fn truncated(what: &str) -> Error {
    Error::Invalid(format!("truncated {what}"))
}

pub(crate) fn too_large(what: &str) -> Error {
    Error::Invalid(format!("{what} exceeds the size limits"))
}

#[cfg(test)]
mod malformed;
#[cfg(test)]
mod tests;
