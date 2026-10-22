use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use super::meta::{self, SeekPoint, StreamInfo};
use super::{Decoder, Fault, Frame};
use crate::media::{AudioBuffer, Error, Result};

const CHUNK: usize = 1 << 18;
const LIMIT: usize = 1 << 22;

pub struct Reader<R> {
    src: R,
    info: StreamInfo,
    seek: Vec<SeekPoint>,
    pub(super) first: u64,
    len: u64,
    buf: Vec<u8>,
    at: usize,
    end: usize,
    base: u64,
    eof: bool,
    want: usize,
    dec: Decoder,
    target: Option<u64>,
    total: Option<u64>,
    seen: bool,
}

struct Found {
    pos: u64,
    sample: u64,
    block: u64,
}

fn io(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        Error::Invalid("a FLAC stream cut short in its header".to_string())
    } else {
        Error::Io(e)
    }
}

struct Head {
    info: StreamInfo,
    seek: Vec<SeekPoint>,
    first: u64,
    len: u64,
}

fn head<R: Read + Seek>(src: &mut R) -> Result<Head> {
    let len = src.seek(SeekFrom::End(0)).map_err(io)?;
    src.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut tag = [0u8; 10];
    src.read_exact(&mut tag[..4]).map_err(io)?;
    if &tag[..3] == b"ID3" {
        src.read_exact(&mut tag[4..]).map_err(io)?;
        let size = tag[6..10]
            .iter()
            .fold(0u64, |a, &b| (a << 7) | (b & 0x7f) as u64);
        let footer = if tag[5] & 0x10 != 0 { 10 } else { 0 };
        src.seek(SeekFrom::Start(10 + size + footer)).map_err(io)?;
        src.read_exact(&mut tag[..4]).map_err(io)?;
    }
    if &tag[..4] != b"fLaC" {
        return Err(Error::Invalid("not a FLAC stream".to_string()));
    }
    let mut info = None;
    let mut seek = Vec::new();
    loop {
        let mut h = [0u8; 4];
        src.read_exact(&mut h).map_err(io)?;
        let kind = h[0] & 0x7f;
        let size = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        match kind {
            meta::STREAMINFO | meta::SEEKTABLE => {
                let mut body = vec![0u8; size];
                src.read_exact(&mut body).map_err(io)?;
                if kind == meta::STREAMINFO {
                    info = Some(StreamInfo::parse(&body)?);
                } else {
                    seek = meta::seek_table(&body);
                }
            }
            meta::INVALID => {
                return Err(Error::Invalid(
                    "a FLAC metadata block of the invalid type 127".to_string(),
                ));
            }
            _ => {
                src.seek(SeekFrom::Current(size as i64)).map_err(io)?;
            }
        }
        if h[0] & 0x80 != 0 {
            break;
        }
    }
    let info =
        info.ok_or_else(|| Error::Invalid("a FLAC stream without STREAMINFO".to_string()))?;
    let first = src.stream_position().map_err(io)?;
    if first > len {
        return Err(Error::Invalid(
            "a FLAC stream cut short in its header".to_string(),
        ));
    }
    Ok(Head {
        info,
        seek,
        first,
        len,
    })
}

pub fn read_info(path: &Path) -> Result<StreamInfo> {
    let mut file = File::open(path).map_err(|e| Error::at(path, e))?;
    Ok(head(&mut file)?.info)
}

impl Reader<File> {
    pub fn open(path: &Path) -> Result<Reader<File>> {
        let file = File::open(path).map_err(|e| Error::at(path, e))?;
        Reader::new(file)
    }
}

impl<R: Read + Seek> Reader<R> {
    pub fn new(mut src: R) -> Result<Reader<R>> {
        let Head {
            info,
            seek,
            first,
            len,
        } = head(&mut src)?;
        if info.bits > 31 {
            return Err(Error::Unsupported("32-bit FLAC".to_string()));
        }
        let want = (info.max_frame as usize + 64).clamp(1 << 16, LIMIT);
        Ok(Reader {
            src,
            info,
            seek,
            first,
            len,
            buf: Vec::new(),
            at: 0,
            end: 0,
            base: first,
            eof: false,
            want,
            dec: Decoder::new(Some(info)),
            target: None,
            total: (info.samples > 0).then_some(info.samples),
            seen: false,
        })
    }

    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn seek_table(&self) -> &[SeekPoint] {
        &self.seek
    }

    fn restart(&mut self, pos: u64) -> Result<()> {
        self.src.seek(SeekFrom::Start(pos)).map_err(io)?;
        self.base = pos;
        self.at = 0;
        self.end = 0;
        self.eof = pos >= self.len;
        Ok(())
    }

    fn fill(&mut self, need: usize) -> Result<()> {
        if self.at > 0 {
            self.buf.copy_within(self.at..self.end, 0);
            self.end -= self.at;
            self.base += self.at as u64;
            self.at = 0;
        }
        let size = (2 * need).max(CHUNK);
        if self.buf.len() < size {
            self.buf.resize(size, 0);
        }
        while self.end < self.buf.len() && !self.eof {
            match self.src.read(&mut self.buf[self.end..]) {
                Ok(0) => self.eof = true,
                Ok(n) => self.end += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Io(e)),
            }
        }
        Ok(())
    }

    fn frame(&mut self) -> Result<Option<Frame>> {
        let mut need = self.want;
        loop {
            if self.end - self.at < need && !self.eof {
                self.fill(need)?;
            }
            if self.at == self.end {
                return Ok(None);
            }
            match self.dec.frame(&self.buf[self.at..self.end]) {
                Ok(f) => {
                    self.at += f.bytes;
                    self.seen = true;
                    return Ok(Some(f));
                }
                Err(Fault::Short) if !self.eof && need < LIMIT => need = (need * 2).min(LIMIT),
                Err(Fault::Short) if !self.eof => {
                    return Err(Error::Invalid("a FLAC frame larger than 4 MiB".to_string()));
                }
                Err(e) => {
                    let rest = &self.buf[self.at..self.end];
                    let synced = rest.len() >= 2 && rest[0] == 0xff && rest[1] & 0xfe == 0xf8;
                    let tail = e == Fault::Short || (self.seen && !synced);
                    if self.eof && tail && !self.later_frame() {
                        self.at = self.end;
                        return Ok(None);
                    }
                    if e == Fault::Short {
                        return Err(Error::Invalid(
                            "FLAC: a damaged frame that runs into the next one".to_string(),
                        ));
                    }
                    if let Fault::Bad(why) = e {
                        return Err(Error::Invalid(format!("FLAC: a damaged frame: {why}")));
                    }
                    return Err(e.into());
                }
            }
        }
    }

    fn later_frame(&mut self) -> bool {
        let mut i = self.at + 1;
        while i + 1 < self.end {
            if self.buf[i] == 0xff
                && self.buf[i + 1] & 0xfe == 0xf8
                && self.dec.frame(&self.buf[i..self.end]).is_ok()
            {
                return true;
            }
            i += 1;
        }
        false
    }

    fn next(&mut self) -> Result<Option<(usize, usize)>> {
        loop {
            let Some(f) = self.frame()? else {
                return Ok(None);
            };
            let n = f.block_size as u64;
            let from = self.target.map_or(0, |t| t.saturating_sub(f.sample).min(n));
            let to = match self.total {
                Some(t) if f.sample >= t => return Ok(None),
                Some(t) => (t - f.sample).min(n),
                None => n,
            };
            if from >= to {
                continue;
            }
            self.target = None;
            if self.total.is_some_and(|t| f.sample + to >= t) {
                self.at = self.end;
                self.eof = true;
            }
            return Ok(Some((from as usize, to as usize)));
        }
    }

    pub fn read_f32(&mut self, out: &mut Vec<f32>) -> Result<usize> {
        let Some((from, to)) = self.next()? else {
            return Ok(0);
        };
        self.dec.put_f32(from, to, out);
        Ok(to - from)
    }

    pub fn read_i32(&mut self, out: &mut Vec<i32>) -> Result<usize> {
        let Some((from, to)) = self.next()? else {
            return Ok(0);
        };
        self.dec.put_i32(from, to, out);
        Ok(to - from)
    }

    fn sync(&mut self, pos: u64, limit: u64) -> Result<Option<Found>> {
        self.restart(pos)?;
        self.fill(self.want * 2)?;
        let mut i = 0usize;
        while i + 1 < self.end && self.base + (i as u64) < limit {
            if self.buf[i] == 0xff && self.buf[i + 1] & 0xfe == 0xf8 {
                let base = self.base;
                if let Ok(f) = self.dec.frame(&self.buf[i..self.end]) {
                    return Ok(Some(Found {
                        pos: base + i as u64,
                        sample: f.sample,
                        block: f.block_size as u64,
                    }));
                }
            }
            i += 1;
        }
        Ok(None)
    }

    pub fn seek(&mut self, sample: u64) -> Result<()> {
        let mut lo = self.first;
        let mut hi = self.len;
        for p in &self.seek {
            let pos = self.first.saturating_add(p.offset);
            if pos >= self.len {
                continue;
            }
            if p.sample <= sample {
                lo = lo.max(pos);
            } else if pos > lo {
                hi = hi.min(pos);
            }
        }
        let found = match self.bisect(lo, hi, sample)? {
            None if lo > self.first || hi < self.len => {
                self.bisect(self.first, self.len, sample)?
            }
            found => found,
        };
        match found.and_then(|at| at.checked_sub(self.base)) {
            Some(i) if i as usize <= self.end => self.at = i as usize,
            _ => self.restart(self.first)?,
        }
        self.target = Some(sample);
        Ok(())
    }

    fn bisect(&mut self, mut lo: u64, mut hi: u64, sample: u64) -> Result<Option<u64>> {
        let span = 2 * self.want as u64;
        while hi > lo && hi - lo > 2 * span {
            let mid = lo + (hi - lo) / 2;
            match self.sync(mid, hi)? {
                Some(f) if f.sample <= sample => {
                    lo = f.pos;
                    if sample < f.sample + f.block {
                        break;
                    }
                }
                _ => hi = mid,
            }
        }
        Ok(self
            .sync(lo, self.len)?
            .filter(|f| f.sample <= sample)
            .map(|f| f.pos))
    }

    pub fn total(&mut self) -> Result<u64> {
        if let Some(t) = self.total {
            return Ok(t);
        }
        let pos = self.base + self.at as u64;
        let (eof, target) = (self.eof && self.at == self.end, self.target);
        let mut back = (4 * self.want as u64).max(1 << 20);
        let mut end = 0u64;
        loop {
            let start = self.len.saturating_sub(back).max(self.first);
            if let Some(f) = self.sync(start, self.len)? {
                end = f.sample + f.block;
                self.restart(f.pos)?;
                while let Ok(Some(f)) = self.frame() {
                    end = end.max(f.sample + f.block_size as u64);
                }
                break;
            }
            if start == self.first {
                break;
            }
            back *= 2;
        }
        self.restart(pos)?;
        if eof {
            self.eof = true;
        }
        self.target = target;
        self.total = Some(end);
        Ok(end)
    }
}

pub fn decode_file(data: &[u8]) -> Result<AudioBuffer> {
    let mut r = Reader::new(Cursor::new(data))?;
    let mut b = AudioBuffer::new(r.info.rate, r.info.channels as u16);
    if let Some(t) = r.total {
        let want = (t as usize).saturating_mul(r.info.channels as usize);
        b.samples.reserve(want.min(data.len().saturating_mul(4)));
    }
    while r.read_f32(&mut b.samples)? > 0 {}
    Ok(b)
}
