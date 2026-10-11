mod bits;
mod dec;
mod dsp;
mod filter;
mod tables;
#[cfg(all(test, target_os = "linux"))]
mod tests;
mod yuv;

use super::Options;
use crate::media::{Error, Result};

pub(crate) fn info(data: &[u8]) -> Result<(u32, u32)> {
    let (w, h) = dec::size(data)?;
    Ok((w as u32, h as u32))
}

#[cfg(test)]
pub(crate) fn decode_yuv(data: &[u8]) -> Result<dec::Frame> {
    dec::decode(data)
}

pub(crate) fn decode_into(data: &[u8], out: &mut [u8]) -> Result<()> {
    dec::decode_rgba(data, out)
}

pub(crate) fn encode(
    _rgba: &[u8],
    _width: u32,
    _height: u32,
    _options: &Options,
) -> Result<Vec<u8>> {
    Err(Error::Unsupported("webp: lossy encode".into()))
}
