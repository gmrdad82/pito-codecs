use crate::media::{Error, Result, Rgba};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Png,
    PngRgba,
    Jpeg(u8),
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else {
        None
    }
}

fn jpeg_error(e: crate::jpeg::Error) -> Error {
    Error::Invalid(e.0)
}

fn unknown() -> Error {
    Error::Unsupported("the bytes are neither a PNG nor a JPEG file".into())
}

pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    match sniff(bytes).ok_or_else(unknown)? {
        Format::Png => crate::png::info(bytes).map(|i| (i.width, i.height)),
        Format::Jpeg => crate::jpeg::decode::info(bytes)
            .map(|i| (i.width, i.height))
            .map_err(jpeg_error),
    }
}

pub fn decode(bytes: &[u8]) -> Result<Rgba> {
    match sniff(bytes).ok_or_else(unknown)? {
        Format::Png => crate::png::decode(bytes),
        Format::Jpeg => crate::jpeg::decode(bytes).map_err(jpeg_error),
    }
}

pub fn open(path: &Path) -> Result<Rgba> {
    let bytes = std::fs::read(path).map_err(|e| Error::at(path, e))?;
    decode(&bytes)
}

pub fn encode(image: &Rgba, output: Output) -> Result<Vec<u8>> {
    match output {
        Output::Png => crate::png::encode(image),
        Output::PngRgba => crate::png::encode_rgba(image),
        Output::Jpeg(quality) => crate::jpeg::encode(image, quality).map_err(jpeg_error),
    }
}

pub fn save(path: &Path, image: &Rgba, output: Output) -> Result<()> {
    let bytes = encode(image, output)?;
    std::fs::write(path, bytes).map_err(|e| Error::at(path, e))
}
