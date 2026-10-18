use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{AudioBuffer, Error, Result};

fn invalid(what: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("WAV: {what}"))
}

fn le16(b: &[u8], at: usize) -> Result<u16> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| invalid("the fmt chunk is too short"))
}

fn le32(b: &[u8], at: usize) -> Result<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| invalid("the file ends inside a chunk header"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sample {
    Int,
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    kind: Sample,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub data: u64,
    pub bytes: u64,
}

impl Layout {
    pub fn frame_bytes(&self) -> usize {
        usize::from(self.bits / 8) * usize::from(self.channels)
    }

    pub fn frames(&self) -> u64 {
        self.bytes / self.frame_bytes() as u64
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate)
    }

    pub fn float(&self) -> bool {
        self.kind == Sample::Float
    }
}

pub fn is_wav(head: &[u8]) -> bool {
    head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WAVE")
}

fn format(body: &[u8]) -> Result<(Sample, u16, u32, u16)> {
    let tag = le16(body, 0)?;
    let channels = le16(body, 2)?;
    let rate = le32(body, 4).map_err(|_| invalid("the fmt chunk is too short"))?;
    let align = le16(body, 12)?;
    let bits = le16(body, 14)?;
    let tag = if tag == 0xfffe { le16(body, 24)? } else { tag };
    let kind = match (tag, bits) {
        (1, 8 | 16 | 24 | 32) => Sample::Int,
        (3, 32 | 64) => Sample::Float,
        _ => {
            return Err(Error::Unsupported(format!(
                "WAV format {tag} with {bits}-bit samples; PCM 8, 16, 24 or 32-bit and float 32 or 64-bit are read"
            )));
        }
    };
    if channels == 0 || rate == 0 {
        return Err(invalid(format!("{channels} channels at {rate} Hz")));
    }
    if usize::from(align) != usize::from(channels) * usize::from(bits / 8) {
        return Err(invalid(format!(
            "a block of {align} bytes for {channels} channels of {bits} bits"
        )));
    }
    Ok((kind, channels, rate, bits))
}

fn layout_of(len: u64, mut chunk: impl FnMut(u64, usize) -> Result<Vec<u8>>) -> Result<Layout> {
    let head = chunk(0, 12)?;
    if !is_wav(&head) {
        return Err(invalid("not a RIFF WAVE file"));
    }
    let mut format_of = None;
    let mut data = None;
    let mut at = 12u64;
    while at + 8 <= len {
        let h = chunk(at, 8)?;
        let size = u64::from(le32(&h, 4)?);
        let start = at + 8;
        let end = start.saturating_add(size).min(len);
        match &h[..4] {
            b"fmt " => {
                let body = chunk(start, (end - start).min(64) as usize)?;
                format_of = Some(format(&body)?);
            }
            b"data" => {
                data = Some((start, end - start));
                if format_of.is_some() {
                    break;
                }
            }
            _ => {}
        }
        at = start.saturating_add(size).saturating_add(size & 1);
    }
    let (kind, channels, rate, bits) = format_of.ok_or_else(|| invalid("no fmt chunk"))?;
    let (data, bytes) = data.ok_or_else(|| invalid("no data chunk"))?;
    let frame = u64::from(bits / 8) * u64::from(channels);
    Ok(Layout {
        kind,
        channels,
        rate,
        bits,
        data,
        bytes: bytes - bytes % frame,
    })
}

pub fn layout(data: &[u8]) -> Result<Layout> {
    layout_of(data.len() as u64, |at, n| {
        let at = at as usize;
        Ok(data
            .get(at..at.saturating_add(n).min(data.len()))
            .unwrap_or_default()
            .to_vec())
    })
}

pub fn decode(data: &[u8]) -> Result<AudioBuffer> {
    let l = layout(data)?;
    let body = &data[l.data as usize..(l.data + l.bytes) as usize];
    let mut samples = Vec::new();
    convert(&l, body, &mut samples);
    Ok(AudioBuffer {
        samples,
        rate: l.rate,
        channels: l.channels,
    })
}

pub fn convert(l: &Layout, body: &[u8], out: &mut Vec<f32>) {
    let body = &body[..body.len() - body.len() % l.frame_bytes()];
    match (l.kind, l.bits) {
        (Sample::Int, 8) => out.extend(body.iter().map(|&b| (f32::from(b) - 128.0) / 128.0)),
        (Sample::Int, 16) => out.extend(
            body.chunks_exact(2)
                .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0),
        ),
        (Sample::Int, 24) => out.extend(
            body.chunks_exact(3)
                .map(|s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0),
        ),
        (Sample::Int, _) => out.extend(
            body.chunks_exact(4)
                .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0),
        ),
        (Sample::Float, 32) => out.extend(
            body.chunks_exact(4)
                .map(|s| clean(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))),
        ),
        (Sample::Float, _) => out.extend(body.chunks_exact(8).map(|s| {
            let mut b = [0u8; 8];
            b.copy_from_slice(s);
            clean(f64::from_le_bytes(b) as f32)
        })),
    }
}

fn clean(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

pub struct Reader {
    file: File,
    pub layout: Layout,
    next: u64,
    bytes: Vec<u8>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        let at = |e: std::io::Error| Error::at(path, e);
        let mut file = File::open(path).map_err(at)?;
        let len = file.metadata().map_err(at)?.len();
        let layout = layout_of(len, |pos, n| {
            let mut b = vec![0u8; n];
            file.seek(SeekFrom::Start(pos))?;
            let mut got = 0;
            while got < n {
                match file.read(&mut b[got..])? {
                    0 => break,
                    k => got += k,
                }
            }
            b.truncate(got);
            Ok(b)
        })?;
        Ok(Reader {
            file,
            layout,
            next: 0,
            bytes: Vec::new(),
        })
    }

    pub fn seek(&mut self, frame: u64) {
        self.next = frame.min(self.layout.frames());
    }

    pub fn read(&mut self, frames: usize, out: &mut Vec<f32>) -> Result<usize> {
        let left = self.layout.frames() - self.next;
        let n = (frames as u64).min(left) as usize;
        if n == 0 {
            return Ok(0);
        }
        let fb = self.layout.frame_bytes();
        self.bytes.resize(n * fb, 0);
        self.file
            .seek(SeekFrom::Start(self.layout.data + self.next * fb as u64))?;
        self.file.read_exact(&mut self.bytes)?;
        convert(&self.layout, &self.bytes, out);
        self.next += n as u64;
        Ok(n)
    }
}
