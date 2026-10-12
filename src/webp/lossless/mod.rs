mod bits;
mod cluster;
mod decode;
mod encode;
mod huffman;
mod lz77;
mod transform;
mod writer;

use super::Options;
use crate::media::Result;
use crate::media::Rgba;
use decode::{Decoder, Out};
pub(super) use encode::Policy;
use encode::encode_image;
use writer::BitWriter;

pub(super) fn header(data: &[u8]) -> Result<(u32, u32, bool)> {
    if data.len() < 5 {
        return Err(super::riff::invalid("VP8L header too short"));
    }
    Decoder::new(data).header()
}

pub(super) fn decode_rgba(data: &[u8], out: &mut [u8]) -> Result<()> {
    let mut d = Decoder::new(data);
    let (w, h, _) = d.header()?;
    d.image(w as usize, h as usize, Out::Rgba(out))
}

pub(super) fn decode_alpha(data: &[u8], width: u32, height: u32, plane: &mut [u8]) -> Result<()> {
    Decoder::new(data).image(width as usize, height as usize, Out::Green(plane))
}

pub(super) fn encode(image: &Rgba, options: &Options) -> Result<Vec<u8>> {
    let w = image.width as usize;
    let h = image.height as usize;
    let mut alpha = false;
    let px: Vec<u32> = image
        .data
        .chunks_exact(4)
        .map(|p| {
            alpha |= p[3] != 255;
            if p[3] == 0 && !options.exact {
                0
            } else {
                (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
            }
        })
        .collect();
    let mut bw = BitWriter::with_capacity(5);
    bw.put(0x2f, 8);
    bw.put(image.width - 1, 14);
    bw.put(image.height - 1, 14);
    bw.put(alpha as u32, 1);
    bw.put(0, 3);
    let policy = Policy {
        effort: options.effort.min(9),
        palette: true,
        predict: true,
    };
    let mut out = bw.finish();
    out.extend_from_slice(&encode_image(px, w, h, &policy));
    Ok(out)
}

pub(super) fn encode_stream(px: Vec<u32>, width: u32, height: u32, policy: &Policy) -> Vec<u8> {
    encode_image(px, width as usize, height as usize, policy)
}
