mod ebml;
mod ids;
mod parse;
mod reader;
mod remux;
mod stream;
mod track;
mod writer;

#[cfg(test)]
mod malformed;
#[cfg(test)]
mod tests;

pub use crate::mp4::{ByteSource, Cut, Snap, Span, TrackKind};
pub use parse::{Cluster, CuePoint, Info};
pub use reader::{Options as ReadOptions, Reader};
pub use remux::{copy, plan, remux, remux_mp4};
pub use stream::{Block, Stream};
pub use track::{
    AudioInfo, Colour, ContentEncoding, Mastering, Projection, Sample, Track, VideoInfo, codec_name,
};
pub use writer::{Options, TrackConfig, Writer};
