mod bits;
mod dec;
mod dsp;
mod enc;
mod filter;
mod tables;
#[cfg(all(test, target_os = "linux"))]
mod tests;
mod tokens;
mod yuv;

use super::Options;
use crate::media::Result;

pub(crate) fn info(data: &[u8]) -> Result<(u32, u32)> {
    let (w, h) = dec::size(data)?;
    Ok((w as u32, h as u32))
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn decode_yuv(data: &[u8]) -> Result<dec::Frame> {
    dec::decode(data)
}

pub(crate) fn decode_into(data: &[u8], out: &mut [u8]) -> Result<()> {
    dec::decode_rgba(data, out)
}

pub(crate) fn encode(rgba: &[u8], width: u32, height: u32, options: &Options) -> Result<Vec<u8>> {
    enc::encode(
        rgba,
        width as usize,
        height as usize,
        &enc::Params {
            quality: options.quality,
            effort: options.effort,
            exact: options.exact,
        },
    )
}
