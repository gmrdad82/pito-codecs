use crate::media::{Error, Result};

pub(super) fn invalid(what: &str) -> Error {
    Error::Invalid(format!("webp: {what}"))
}

pub(super) fn u24(b: &[u8]) -> u32 {
    b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16
}

pub(super) fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

#[derive(Clone, Copy)]
pub(super) struct Chunk<'a> {
    pub id: [u8; 4],
    pub data: &'a [u8],
}

pub(super) struct Chunks<'a> {
    rest: &'a [u8],
}

impl<'a> Chunks<'a> {
    pub fn new(rest: &'a [u8]) -> Self {
        Chunks { rest }
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < 8 {
            return None;
        }
        let id = [self.rest[0], self.rest[1], self.rest[2], self.rest[3]];
        let size = u32le(&self.rest[4..]) as usize;
        let body = &self.rest[8..];
        if size > body.len() {
            self.rest = &[];
            return Some(Err(invalid("chunk runs past the end")));
        }
        let data = &body[..size];
        let next = (size + (size & 1)).min(body.len());
        self.rest = &body[next..];
        Some(Ok(Chunk { id, data }))
    }
}

pub(super) fn body(file: &[u8]) -> Result<&[u8]> {
    if file.len() < 12 || &file[..4] != b"RIFF" || &file[8..12] != b"WEBP" {
        return Err(invalid("not a RIFF WEBP file"));
    }
    let size = u32le(&file[4..]) as usize;
    if size < 4 {
        return Err(invalid("RIFF size"));
    }
    let end = (8 + size).min(file.len());
    Ok(&file[12..end])
}

pub(super) struct Writer {
    pub out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF\0\0\0\0WEBP");
        Writer { out }
    }

    pub fn chunk(&mut self, id: &[u8; 4], data: &[u8]) {
        put_chunk(&mut self.out, id, data);
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        let size = self.out.len() - 8;
        let size = u32::try_from(size).map_err(|_| invalid("file over 4 GiB"))?;
        self.out[4..8].copy_from_slice(&size.to_le_bytes());
        Ok(self.out)
    }
}

pub(super) fn put_chunk(out: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() & 1 == 1 {
        out.push(0);
    }
}

pub(super) fn put_u24(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes()[..3]);
}
