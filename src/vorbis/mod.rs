pub mod audio;
pub mod bitpacking;
pub mod header;
pub mod header_cached;
pub mod huffman_tree;
pub mod imdct;
pub mod ogg;

#[cfg(test)]
mod decode_tests;
#[cfg(test)]
mod tests;

pub use audio::{AudioReadError, Decoder};
pub use header::{
    CommentHeader, HeaderReadError, HeaderSet, IdentHeader, SetupHeader, read_header_comment,
    read_header_ident, read_header_set, read_header_setup,
};
pub use ogg::{OggVorbis, decode, xiph_headers};

pub(crate) fn ilog(val: u64) -> u8 {
    64 - val.leading_zeros() as u8
}
