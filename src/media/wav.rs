use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
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
    matches!(head.get(..4), Some(b"RIFF" | b"RF64" | b"BW64")) && head.get(8..12) == Some(b"WAVE")
}

fn le64(b: &[u8], at: usize) -> Result<u64> {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| invalid("the ds64 chunk is too short"))
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
    let mut wide = None;
    let mut at = 12u64;
    while at + 8 <= len {
        let h = chunk(at, 8)?;
        let mut size = u64::from(le32(&h, 4)?);
        if &h[..4] == b"data" && size == u64::from(u32::MAX) {
            size = wide.unwrap_or(len.saturating_sub(at + 8));
        }
        let start = at + 8;
        let end = start.saturating_add(size).min(len);
        match &h[..4] {
            b"ds64" => {
                let body = chunk(start, (end - start).min(28) as usize)?;
                wide = Some(le64(&body, 8)?);
            }
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

pub struct Reader<R = File> {
    file: R,
    pub layout: Layout,
    next: u64,
    bytes: Vec<u8>,
}

fn fetch<R: Read + Seek>(file: &mut R, pos: u64, n: usize) -> std::io::Result<Vec<u8>> {
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
}

impl Reader<File> {
    pub fn open(path: &Path) -> Result<Reader<File>> {
        let at = |e: std::io::Error| Error::at(path, e);
        let file = File::open(path).map_err(at)?;
        Reader::new(file)
    }
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut file: R) -> Result<Reader<R>> {
        let len = file.seek(SeekFrom::End(0))?;
        let layout = layout_of(len, |pos, n| Ok(fetch(&mut file, pos, n)?))?;
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

    pub fn position(&self) -> u64 {
        self.next
    }

    fn fill(&mut self, frames: usize) -> Result<usize> {
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
        self.next += n as u64;
        Ok(n)
    }

    pub fn read(&mut self, frames: usize, out: &mut Vec<f32>) -> Result<usize> {
        let n = self.fill(frames)?;
        if n > 0 {
            convert(&self.layout, &self.bytes, out);
        }
        Ok(n)
    }

    pub fn read_ints(&mut self, frames: usize, out: &mut Vec<i32>) -> Result<usize> {
        if self.layout.float() {
            return Err(invalid("a float file has no integer samples"));
        }
        let n = self.fill(frames)?;
        if n == 0 {
            return Ok(0);
        }
        let bytes = &self.bytes;
        match self.layout.bits {
            8 => out.extend(bytes.iter().map(|&b| i32::from(b) - 128)),
            16 => out.extend(
                bytes
                    .chunks_exact(2)
                    .map(|s| i32::from(i16::from_le_bytes([s[0], s[1]]))),
            ),
            24 => out.extend(
                bytes
                    .chunks_exact(3)
                    .map(|s| i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8),
            ),
            _ => out.extend(
                bytes
                    .chunks_exact(4)
                    .map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]])),
            ),
        }
        Ok(n)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Int8,
    Int16,
    Int24,
    Int32,
    Float32,
}

impl Format {
    pub fn bits(self) -> u16 {
        match self {
            Format::Int8 => 8,
            Format::Int16 => 16,
            Format::Int24 => 24,
            Format::Int32 | Format::Float32 => 32,
        }
    }

    pub fn float(self) -> bool {
        self == Format::Float32
    }
}

pub struct Writer<W: Write + Seek> {
    out: W,
    format: Format,
    channels: u16,
    bytes: u64,
    start: u64,
    scratch: Vec<u8>,
}

impl Writer<std::io::BufWriter<File>> {
    pub fn create(
        path: &Path,
        rate: u32,
        channels: u16,
        format: Format,
    ) -> Result<Writer<std::io::BufWriter<File>>> {
        let file = File::create(path).map_err(|e| Error::at(path, e))?;
        Writer::new(std::io::BufWriter::new(file), rate, channels, format)
    }
}

fn mask(channels: u16) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        4 => 0x33,
        6 => 0x3F,
        8 => 0x63F,
        _ => 0,
    }
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut out: W, rate: u32, channels: u16, format: Format) -> Result<Writer<W>> {
        if rate == 0 || channels == 0 {
            return Err(invalid(format!("{channels} channels at {rate} Hz")));
        }
        let bits = format.bits();
        let align = u32::from(channels) * u32::from(bits / 8);
        let Ok(block) = u16::try_from(align) else {
            return Err(invalid(format!("{channels} channels are too many")));
        };
        let wide = channels > 2 || bits > 16 && !format.float();
        let tag: u16 = if format.float() { 3 } else { 1 };
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&(if wide { 0xFFFEu16 } else { tag }).to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&rate.to_le_bytes());
        fmt.extend_from_slice(&(rate.saturating_mul(align)).to_le_bytes());
        fmt.extend_from_slice(&block.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        if wide {
            fmt.extend_from_slice(&22u16.to_le_bytes());
            fmt.extend_from_slice(&bits.to_le_bytes());
            fmt.extend_from_slice(&mask(channels).to_le_bytes());
            fmt.extend_from_slice(&tag.to_le_bytes());
            fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]);
        } else if format.float() {
            fmt.extend_from_slice(&0u16.to_le_bytes());
        }
        let mut head = Vec::new();
        head.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        head.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
        head.extend_from_slice(&fmt);
        if format.float() {
            head.extend_from_slice(b"fact\x04\0\0\0\0\0\0\0");
        }
        head.extend_from_slice(b"data\0\0\0\0");
        let start = out.stream_position()?;
        out.write_all(&head)?;
        Ok(Writer {
            out,
            format,
            channels,
            bytes: 0,
            start,
            scratch: Vec::new(),
        })
    }

    pub fn frames(&self) -> u64 {
        self.bytes / (u64::from(self.format.bits() / 8) * u64::from(self.channels))
    }

    fn put(&mut self) -> Result<()> {
        self.bytes += self.scratch.len() as u64;
        if self.bytes > u64::from(u32::MAX) - 1024 {
            return Err(Error::Unsupported(
                "WAV: more than 4 GiB of samples does not fit a RIFF file".into(),
            ));
        }
        self.out.write_all(&self.scratch)?;
        Ok(())
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        if !samples.len().is_multiple_of(usize::from(self.channels)) {
            return Err(invalid("the samples do not fill whole frames"));
        }
        self.scratch.clear();
        for &raw in samples {
            let v = if raw.is_nan() {
                0.0
            } else {
                raw.clamp(-1.0, 1.0)
            };
            match self.format {
                Format::Int8 => self.scratch.push(((v * 127.0).round() as i32 + 128) as u8),
                Format::Int16 => self
                    .scratch
                    .extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes()),
                Format::Int24 => self
                    .scratch
                    .extend_from_slice(&((v * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
                Format::Int32 => self.scratch.extend_from_slice(
                    &((f64::from(v) * 2_147_483_647.0).round() as i32).to_le_bytes(),
                ),
                Format::Float32 => self.scratch.extend_from_slice(&raw.to_le_bytes()),
            }
        }
        self.put()
    }

    pub fn write_ints(&mut self, samples: &[i32]) -> Result<()> {
        if self.format.float() {
            return Err(invalid("integer samples into a float file"));
        }
        if !samples.len().is_multiple_of(usize::from(self.channels)) {
            return Err(invalid("the samples do not fill whole frames"));
        }
        let bits = self.format.bits();
        let (lo, hi) = (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1);
        self.scratch.clear();
        for &v in samples {
            let v = i64::from(v).clamp(lo, hi) as i32;
            match self.format {
                Format::Int8 => self.scratch.push((v + 128) as u8),
                Format::Int16 => self.scratch.extend_from_slice(&(v as i16).to_le_bytes()),
                Format::Int24 => self.scratch.extend_from_slice(&v.to_le_bytes()[..3]),
                _ => self.scratch.extend_from_slice(&v.to_le_bytes()),
            }
        }
        self.put()
    }

    pub fn finish(mut self) -> Result<W> {
        if self.bytes % 2 == 1 {
            self.out.write_all(&[0])?;
        }
        let end = self.out.stream_position()?;
        let riff = (end - self.start - 8) as u32;
        let data = self.bytes as u32;
        self.out.seek(SeekFrom::Start(self.start + 4))?;
        self.out.write_all(&riff.to_le_bytes())?;
        let at = end - self.bytes - self.bytes % 2 - 4;
        self.out.seek(SeekFrom::Start(at))?;
        self.out.write_all(&data.to_le_bytes())?;
        if self.format.float() {
            let fact = at - 4 - 4;
            self.out.seek(SeekFrom::Start(fact))?;
            self.out.write_all(&(self.frames() as u32).to_le_bytes())?;
        }
        self.out.seek(SeekFrom::Start(end))?;
        self.out.flush()?;
        Ok(self.out)
    }
}

pub fn encode(buffer: &AudioBuffer, format: Format) -> Result<Vec<u8>> {
    let mut w = Writer::new(
        std::io::Cursor::new(Vec::new()),
        buffer.rate,
        buffer.channels,
        format,
    )?;
    w.write(&buffer.samples)?;
    Ok(w.finish()?.into_inner())
}
