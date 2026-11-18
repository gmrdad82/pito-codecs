use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use super::audio::Decoder;
use super::header::{CommentHeader, read_header_set};
use super::ogg::page_crc;
use crate::media::{Error, Result};

const MAX_PACKET: usize = 1 << 22;
const TAIL: u64 = 65_536 + 27 + 255;

fn invalid(what: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("Ogg Vorbis: {what}"))
}

struct Packet {
    data: Vec<u8>,
    page: Option<(i64, bool)>,
}

struct Pages<R> {
    source: R,
    serial: Option<u32>,
    last_seq: Option<u32>,
    queue: VecDeque<Packet>,
    partial: Vec<u8>,
    open: bool,
    ended: bool,
}

impl<R: Read> Pages<R> {
    fn reset(&mut self) {
        self.serial = None;
        self.last_seq = None;
        self.queue.clear();
        self.partial.clear();
        self.open = false;
        self.ended = false;
    }

    fn new(source: R) -> Self {
        Pages {
            source,
            serial: None,
            last_seq: None,
            queue: VecDeque::new(),
            partial: Vec::new(),
            open: false,
            ended: false,
        }
    }

    fn next(&mut self) -> Result<Option<Packet>> {
        loop {
            if let Some(p) = self.queue.pop_front() {
                return Ok(Some(p));
            }
            if self.ended {
                return Ok(None);
            }
            self.page()?;
        }
    }

    fn page(&mut self) -> Result<()> {
        let mut head = [0u8; 27];
        let mut got = 0;
        while got < head.len() {
            match self.source.read(&mut head[got..])? {
                0 => break,
                n => got += n,
            }
        }
        if got == 0 {
            self.ended = true;
            return Ok(());
        }
        if got < head.len() {
            return Err(invalid("the stream ends inside a page header"));
        }
        if &head[..4] != b"OggS" || head[4] != 0 {
            return Err(invalid("lost sync, no page where one must start"));
        }
        let mut lacing = vec![0u8; usize::from(head[26])];
        self.source
            .read_exact(&mut lacing)
            .map_err(|_| invalid("the stream ends inside a page"))?;
        let size: usize = lacing.iter().map(|&b| usize::from(b)).sum();
        let mut page = Vec::with_capacity(27 + lacing.len() + size);
        page.extend_from_slice(&head);
        page.extend_from_slice(&lacing);
        page.resize(27 + lacing.len() + size, 0);
        self.source
            .read_exact(&mut page[27 + lacing.len()..])
            .map_err(|_| invalid("the stream ends inside a page"))?;
        if page_crc(&page) != u32::from_le_bytes([page[22], page[23], page[24], page[25]]) {
            return Err(invalid("a page fails its checksum"));
        }
        let flags = page[5];
        let granule = i64::from_le_bytes(page[6..14].try_into().unwrap_or([0; 8]));
        let serial = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
        let seq = u32::from_le_bytes([page[18], page[19], page[20], page[21]]);
        let body = &page[27 + lacing.len()..];
        match self.serial {
            None => {
                if flags & 0x02 == 0 || !body.starts_with(b"\x01vorbis") {
                    return Err(invalid("the first page does not open a Vorbis stream"));
                }
                self.serial = Some(serial);
            }
            Some(s) if s == serial => {
                if flags & 0x02 != 0 {
                    return Err(Error::Unsupported(
                        "chained Ogg Vorbis files are not supported".into(),
                    ));
                }
            }
            Some(_) => return Ok(()),
        }
        if self.last_seq.is_some_and(|l| l.wrapping_add(1) != seq) {
            self.partial.clear();
            self.open = false;
        }
        self.last_seq = Some(seq);
        let continued = flags & 0x01 != 0;
        if !continued && self.open {
            self.partial.clear();
            self.open = false;
        }
        let mut skip = continued && !self.open;
        let mut at = 0usize;
        let first = self.queue.len();
        for &l in &lacing {
            let seg = &body[at..at + usize::from(l)];
            at += usize::from(l);
            if skip {
                skip = l == 255;
                continue;
            }
            if self.partial.len() + seg.len() > MAX_PACKET {
                return Err(invalid("a packet is larger than any Vorbis packet"));
            }
            self.open = true;
            self.partial.extend_from_slice(seg);
            if l < 255 {
                self.queue.push_back(Packet {
                    data: std::mem::take(&mut self.partial),
                    page: None,
                });
                self.open = false;
            }
        }
        if self.queue.len() > first
            && granule != -1
            && let Some(last) = self.queue.back_mut()
        {
            last.page = Some((granule, flags & 0x04 != 0));
        }
        if flags & 0x04 != 0 {
            self.ended = true;
        }
        Ok(())
    }
}

fn last_granule<R: Read + Seek>(source: &mut R, serial: u32) -> Result<Option<i64>> {
    let len = source.seek(SeekFrom::End(0))?;
    let from = len.saturating_sub(TAIL * 4);
    source.seek(SeekFrom::Start(from))?;
    let mut tail = Vec::new();
    source.take(len - from).read_to_end(&mut tail)?;
    let mut best = None;
    let mut at = 0usize;
    while at + 27 <= tail.len() {
        if &tail[at..at + 4] == b"OggS" && tail[at + 4] == 0 {
            let nseg = usize::from(tail[at + 26]);
            if let Some(lacing) = tail.get(at + 27..at + 27 + nseg) {
                let end = at + 27 + nseg + lacing.iter().map(|&b| usize::from(b)).sum::<usize>();
                if let Some(page) = tail.get(at..end)
                    && page_crc(page)
                        == u32::from_le_bytes([page[22], page[23], page[24], page[25]])
                {
                    let s = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
                    let g = i64::from_le_bytes(page[6..14].try_into().unwrap_or([0; 8]));
                    if s == serial && g >= 0 {
                        best = Some(g);
                    }
                    at = end;
                    continue;
                }
            }
        }
        at += 1;
    }
    Ok(best)
}

pub struct Stream<R: Read + Seek> {
    pages: Pages<R>,
    decoder: Decoder,
    pub comments: CommentHeader,
    lead: u64,
    skip: u64,
    total: Option<u64>,
    left: Option<u64>,
    block: Vec<f32>,
    sent: u64,
    finished: bool,
}

impl Stream<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|e| Error::at(path, e))?;
        Stream::new(BufReader::new(file))
    }
}

impl<'a> Stream<Cursor<&'a [u8]>> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self> {
        Stream::new(Cursor::new(bytes))
    }
}

fn audio<T>(r: std::result::Result<T, super::AudioReadError>) -> Result<T> {
    r.map_err(|e| invalid(format!("{e}")))
}

impl<R: Read + Seek> Stream<R> {
    pub fn new(source: R) -> Result<Self> {
        let mut pages = Pages::new(source);
        let mut heads = Vec::with_capacity(3);
        while heads.len() < 3 {
            let Some(p) = pages.next()? else {
                return Err(invalid("the stream ends before its three headers"));
            };
            heads.push(p.data);
        }
        let serial = pages.serial.unwrap_or(0);
        let (ident, comments, setup) = read_header_set(&heads[0], &heads[1], &heads[2])
            .map_err(|e| invalid(format!("{e}")))?;
        let tail = last_granule(&mut pages.source, serial)?;
        let mut stream = Stream {
            pages,
            decoder: Decoder::new(ident, setup),
            comments,
            lead: 0,
            skip: 0,
            total: None,
            left: None,
            block: Vec::new(),
            sent: 0,
            finished: false,
        };
        stream.measure(tail)?;
        stream.rewind()?;
        Ok(stream)
    }

    fn measure(&mut self, tail: Option<i64>) -> Result<()> {
        self.rewind_pages()?;
        let mut upto = 0u64;
        let mut prev: Option<usize> = None;
        let mut first = None;
        while let Some(p) = self.pages.next()? {
            if let Ok(n) = self.decoder.blocksize(&p.data) {
                upto += prev.map_or(0, |q| (q / 4 + n / 4) as u64);
                prev = Some(n);
            }
            if let Some(mark) = p.page {
                first = Some((mark, upto));
                break;
            }
        }
        let start: i128 = match first {
            Some(((g, last), upto)) if !last => i128::from(g) - i128::from(upto),
            _ => 0,
        };
        self.lead = (-start).max(0) as u64;
        self.total = match (tail, first) {
            (Some(g), Some(_)) => {
                let end = (i128::from(g) - start).max(i128::from(self.lead));
                Some((end - i128::from(self.lead)) as u64)
            }
            _ => None,
        };
        Ok(())
    }

    fn rewind_pages(&mut self) -> Result<()> {
        self.pages.source.seek(SeekFrom::Start(0))?;
        self.pages.reset();
        for _ in 0..3 {
            if self.pages.next()?.is_none() {
                return Err(invalid("the stream ends before its three headers"));
            }
        }
        Ok(())
    }

    pub fn rewind(&mut self) -> Result<()> {
        self.rewind_pages()?;
        self.decoder.reset();
        self.skip = self.lead;
        self.left = self.total;
        self.sent = 0;
        self.finished = false;
        Ok(())
    }

    pub fn rate(&self) -> u32 {
        self.decoder.rate()
    }

    pub fn channels(&self) -> u16 {
        self.decoder.channels()
    }

    pub fn total_frames(&self) -> Option<u64> {
        self.total
    }

    pub fn position(&self) -> u64 {
        self.sent
    }

    pub fn next_block(&mut self) -> Result<Option<&[f32]>> {
        let ch = usize::from(self.decoder.channels().max(1));
        loop {
            if self.finished || self.left == Some(0) {
                self.finished = true;
                return Ok(None);
            }
            let Some(p) = self.pages.next()? else {
                self.finished = true;
                return Ok(None);
            };
            if p.data.is_empty() {
                continue;
            }
            self.block.clear();
            audio(self.decoder.decode_into(&p.data, &mut self.block))?;
            let mut frames = (self.block.len() / ch) as u64;
            if self.skip > 0 {
                let d = self.skip.min(frames);
                self.block.drain(..d as usize * ch);
                self.skip -= d;
                frames -= d;
            }
            if let Some(left) = &mut self.left {
                if frames > *left {
                    self.block.truncate(*left as usize * ch);
                    frames = *left;
                }
                *left -= frames;
            }
            if frames == 0 {
                continue;
            }
            self.sent += frames;
            return Ok(Some(&self.block));
        }
    }
}
