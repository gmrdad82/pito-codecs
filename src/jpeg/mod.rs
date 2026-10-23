#[cfg(target_arch = "x86_64")]
mod avx2;
mod color;
mod dct;
mod huffman;
mod quant;
#[cfg(test)]
mod tests;
mod writer;

use crate::media::Rgba;
use huffman::HuffmanTable;
use quant::QuantTable;
use std::fmt;
use writer::Writer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Copy, Clone)]
#[repr(C, align(32))]
struct Block([i16; 64]);

trait Ops: Copy {
    fn ycbcr_row(self, rgba: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]);
    fn fdct(self, block: &mut Block);
}

#[derive(Clone, Copy)]
struct Scalar;

impl Ops for Scalar {
    fn ycbcr_row(self, rgba: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
        color::ycbcr_row(rgba, y, cb, cr);
    }

    fn fdct(self, block: &mut Block) {
        dct::fdct(block);
    }
}

const MAX_SIDE: u32 = u16::MAX as u32;

pub fn encode(image: &Rgba, quality: u8) -> Result<Vec<u8>> {
    #[cfg(target_arch = "x86_64")]
    if let Some(ops) = avx2::Avx2::detect() {
        return encode_with(image, quality, ops);
    }
    encode_with(image, quality, Scalar)
}

fn check(image: &Rgba, quality: u8) -> Result<(usize, usize)> {
    if !(1..=100).contains(&quality) {
        return Err(Error(format!(
            "JPEG: quality {quality} is outside 1 to 100"
        )));
    }
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 {
        return Err(Error(format!(
            "JPEG: the image is {w}x{h}, nothing to encode"
        )));
    }
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err(Error(format!(
            "JPEG: {w}x{h} is larger than JPEG's {MAX_SIDE}x{MAX_SIDE}"
        )));
    }
    let need = u64::from(w) * u64::from(h) * 4;
    if (image.data.len() as u64) < need {
        return Err(Error(format!(
            "JPEG: the image holds {} bytes, {w}x{h} RGBA needs {need}",
            image.data.len()
        )));
    }
    Ok((w as usize, h as usize))
}

struct Component<'a> {
    quant: &'a QuantTable,
    dc: &'a HuffmanTable,
    ac: &'a HuffmanTable,
}

fn encode_with<O: Ops>(image: &Rgba, quality: u8, ops: O) -> Result<Vec<u8>> {
    let (width, height) = check(image, quality)?;

    let luma_q = QuantTable::new(&quant::LUMA, quality);
    let chroma_q = QuantTable::new(&quant::CHROMA, quality);
    let luma_dc = HuffmanTable::luma_dc();
    let luma_ac = HuffmanTable::luma_ac();
    let chroma_dc = HuffmanTable::chroma_dc();
    let chroma_ac = HuffmanTable::chroma_ac();

    let mut w = Writer::with_capacity(width * height / 4 + 1024);
    w.marker(writer::SOI);
    w.app0();
    w.quant_segment(0, &luma_q);
    w.quant_segment(1, &chroma_q);
    w.frame_header(width as u16, height as u16);
    w.huffman_segment(0, 0, &luma_dc);
    w.huffman_segment(1, 0, &luma_ac);
    w.huffman_segment(0, 1, &chroma_dc);
    w.huffman_segment(1, 1, &chroma_ac);
    w.scan_header();

    let luma = Component {
        quant: &luma_q,
        dc: &luma_dc,
        ac: &luma_ac,
    };
    let chroma = Component {
        quant: &chroma_q,
        dc: &chroma_dc,
        ac: &chroma_ac,
    };

    let cols = width.div_ceil(16);
    let rows = height.div_ceil(16);
    let stride = cols * 16;
    let mut planes = [
        vec![0u8; stride * 16],
        vec![0u8; stride * 16],
        vec![0u8; stride * 16],
    ];
    let mut prev_dc = [0i16; 3];

    for mcu_y in 0..rows {
        fill_rows(ops, image, width, height, mcu_y * 16, stride, &mut planes);
        let [py, pcb, pcr] = &planes;
        for mcu_x in 0..cols {
            let x = mcu_x * 16;
            for (bx, by) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                let block = color::block(py, x + bx, by, stride);
                encode_block(ops, &mut w, block, &luma, &mut prev_dc[0]);
            }
            let block = color::block_averaged(pcb, x, 0, stride);
            encode_block(ops, &mut w, block, &chroma, &mut prev_dc[1]);
            let block = color::block_averaged(pcr, x, 0, stride);
            encode_block(ops, &mut w, block, &chroma, &mut prev_dc[2]);
        }
    }

    w.finalize_bits();
    w.marker(writer::EOI);
    Ok(w.into_inner())
}

fn fill_rows<O: Ops>(
    ops: O,
    image: &Rgba,
    width: usize,
    height: usize,
    top: usize,
    stride: usize,
    planes: &mut [Vec<u8>; 3],
) {
    let [py, pcb, pcr] = planes;
    for r in 0..16 {
        let at = r * stride;
        if top + r >= height {
            for plane in [&mut *py, &mut *pcb, &mut *pcr] {
                plane.copy_within(at - stride..at, at);
            }
            continue;
        }
        let src_at = (top + r) * width * 4;
        let src = &image.data[src_at..src_at + width * 4];
        ops.ycbcr_row(
            src,
            &mut py[at..at + width],
            &mut pcb[at..at + width],
            &mut pcr[at..at + width],
        );
        for plane in [&mut *py, &mut *pcb, &mut *pcr] {
            let last = plane[at + width - 1];
            plane[at + width..at + stride].fill(last);
        }
    }
}

fn encode_block<O: Ops>(
    ops: O,
    w: &mut Writer,
    mut block: Block,
    component: &Component,
    prev_dc: &mut i16,
) {
    ops.fdct(&mut block);
    let q = component.quant.quantize_block(&block);
    w.block(&q, *prev_dc, component.dc, component.ac);
    *prev_dc = q[0];
}
