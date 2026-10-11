mod bits;
mod decode;
mod huffman;
mod transform;

use super::{Options, Rgba};
use crate::media::{Error, Result};
use decode::Decoder;

pub(super) fn header(data: &[u8]) -> Result<(u32, u32, bool)> {
    if data.len() < 5 {
        return Err(super::riff::invalid("VP8L header too short"));
    }
    Decoder::new(data).header()
}

pub(super) fn decode_rgba(data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut d = Decoder::new(data);
    let (w, h, _) = d.header()?;
    let argb = d.image(w as usize, h as usize)?;
    for (o, &p) in out.chunks_exact_mut(4).zip(&argb) {
        o.copy_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]);
    }
    Ok(())
}

pub(super) fn decode_stream(data: &[u8], width: u32, height: u32) -> Result<Vec<u32>> {
    Decoder::new(data).image(width as usize, height as usize)
}

pub(super) fn encode(_image: &Rgba, _options: &Options) -> Result<Vec<u8>> {
    Err(Error::Unsupported("webp: lossless encode".into()))
}
