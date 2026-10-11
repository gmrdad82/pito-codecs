use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use super::ebml::{self, Children, Lacing, Size, parse_lacing, read_id, read_size};
use super::ids::*;
use super::parse::{self, Cluster, CuePoint, Info, SeekEntry};
use super::track::{Sample, Track};
use crate::media::{Error, Packet, Result};
use crate::mp4::{ByteSource, TrackKind};

const WIN: usize = 64 * 1024;
const MAX_META: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub index: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { index: true }
    }
}

#[derive(Default, Clone, Debug)]
pub(crate) struct Cache {
    buf: Vec<u8>,
    start: u64,
}

pub(crate) struct Io<'a, S: ByteSource + ?Sized> {
    src: &'a S,
    pub(crate) len: u64,
    cache: &'a mut Cache,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Hdr {
    pub(crate) id: u32,
    pub(crate) pos: u64,
    pub(crate) size: Size,
    hlen: usize,
}

impl Hdr {
    pub(crate) fn data_start(&self) -> u64 {
        self.pos + self.hlen as u64
    }

    pub(crate) fn end(&self, limit: u64) -> u64 {
        match self.size {
            Size::Known(n) => self.data_start().saturating_add(n).min(limit),
            Size::Unknown => limit,
        }
    }

    pub(crate) fn overruns(&self, limit: u64) -> bool {
        matches!(self.size, Size::Known(n) if self.data_start().saturating_add(n) > limit)
    }
}

fn eof() -> Error {
    Error::Invalid("truncated Matroska data".into())
}

impl<'a, S: ByteSource + ?Sized> Io<'a, S> {
    pub(crate) fn new(src: &'a S, cache: &'a mut Cache) -> Self {
        let len = src.len();
        Self { src, len, cache }
    }

    pub(crate) fn peek(&mut self, pos: u64, n: usize) -> Result<&[u8]> {
        let n = n.min(WIN);
        if pos >= self.len {
            return Ok(&[]);
        }
        let want_end = (pos + n as u64).min(self.len);
        let c = &*self.cache;
        if !(pos >= c.start && want_end <= c.start + c.buf.len() as u64) {
            let fill = ((self.len - pos) as usize).min(WIN);
            let mut buf = std::mem::take(&mut self.cache.buf);
            buf.resize(fill, 0);
            self.src.read_at(pos, &mut buf)?;
            self.cache.buf = buf;
            self.cache.start = pos;
        }
        let o = (pos - self.cache.start) as usize;
        Ok(&self.cache.buf[o..o + (want_end - pos) as usize])
    }

    pub(crate) fn read_into(&mut self, pos: u64, n: u64, out: &mut Vec<u8>) -> Result<()> {
        if pos.checked_add(n).is_none_or(|e| e > self.len) {
            return Err(eof());
        }
        let at = out.len();
        if n as usize <= WIN / 2 {
            let b = self.peek(pos, n as usize)?;
            out.extend_from_slice(b);
            return Ok(());
        }
        out.resize(at + n as usize, 0);
        self.src.read_at(pos, &mut out[at..])?;
        Ok(())
    }

    fn read_vec(&mut self, pos: u64, n: u64) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.read_into(pos, n, &mut v)?;
        Ok(v)
    }

    pub(crate) fn header(&mut self, pos: u64, limit: u64) -> Result<Option<Hdr>> {
        if pos >= limit {
            return Ok(None);
        }
        let b = self.peek(pos, 12)?;
        let Some((id, il)) = read_id(b) else {
            return Ok(None);
        };
        let Some((size, sl)) = read_size(&b[il..]) else {
            return Ok(None);
        };
        let hlen = il + sl;
        if pos + hlen as u64 > limit {
            return Ok(None);
        }
        Ok(Some(Hdr {
            id,
            pos,
            size,
            hlen,
        }))
    }

    pub(crate) fn uint(&mut self, h: &Hdr, end: u64) -> Result<u64> {
        let n = end.saturating_sub(h.data_start()).min(8) as usize;
        Ok(ebml::uint(self.peek(h.data_start(), n)?))
    }

    pub(crate) fn resync(&mut self, from: u64, limit: u64) -> Result<Option<u64>> {
        let mut pos = from;
        while pos + 4 <= limit {
            let chunk = self.peek(pos, WIN)?.to_vec();
            if chunk.len() < 4 {
                return Ok(None);
            }
            for i in 0..chunk.len() - 3 {
                if chunk[i] & 0xF0 != 0x10 {
                    continue;
                }
                let id = u32::from_be_bytes([chunk[i], chunk[i + 1], chunk[i + 2], chunk[i + 3]]);
                if !is_top_level(id) || id == EBML || id == SEGMENT {
                    continue;
                }
                let p = pos + i as u64;
                if self.plausible(p, id, limit)? {
                    return Ok(Some(p));
                }
            }
            pos += (chunk.len() - 3) as u64;
        }
        Ok(None)
    }

    fn plausible(&mut self, p: u64, id: u32, limit: u64) -> Result<bool> {
        let Some(h) = self.header(p, limit)? else {
            return Ok(false);
        };
        if h.id != id || h.overruns(limit) {
            return Ok(false);
        }
        if h.size == Size::Unknown && id != CLUSTER {
            return Ok(false);
        }
        let mut cp = h.data_start();
        let end = h.end(limit);
        for _ in 0..3 {
            let Some(c) = self.header(cp, end)? else {
                return Ok(false);
            };
            if c.overruns(end) {
                return Ok(false);
            }
            if id != CLUSTER {
                return Ok(true);
            }
            match c.id {
                TIMESTAMP => return Ok(true),
                CRC32 | VOID => cp = c.end(end),
                _ => return Ok(false),
            }
        }
        Ok(false)
    }

    pub(crate) fn unknown_end(&mut self, data_start: u64, limit: u64) -> Result<u64> {
        let mut pos = data_start;
        while pos < limit {
            let Some(h) = self.header(pos, limit)? else {
                return Ok(pos);
            };
            if is_top_level(h.id) || h.size == Size::Unknown {
                return Ok(pos);
            }
            pos = h.end(limit);
        }
        Ok(limit)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockRef {
    pub(crate) track: usize,
    pub(crate) offset: u64,
    pub(crate) ts: i64,
    pub(crate) duration: Option<u64>,
    pub(crate) keyframe: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameRef {
    pub(crate) track: usize,
    pub(crate) offset: u64,
    pub(crate) size: u32,
    pub(crate) pts: i64,
    pub(crate) duration: u32,
    pub(crate) keyframe: bool,
    pub(crate) block: u64,
    pub(crate) lace: u16,
}

pub(crate) fn track_index(tracks: &[Track], number: u64) -> Option<usize> {
    tracks.iter().position(|t| t.number == number)
}

fn parse_block<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    data_start: u64,
    len: u64,
    tracks: &[Track],
    frames: &mut Vec<(u64, u32)>,
) -> Result<Option<(usize, i16, u8)>> {
    frames.clear();
    let head = io.peek(data_start, 16.min(len as usize))?;
    let Some((number, tl, _)) = ebml::read_vint(head) else {
        return Ok(None);
    };
    if head.len() < tl + 3 {
        return Ok(None);
    }
    let rel = i16::from_be_bytes([head[tl], head[tl + 1]]);
    let flags = head[tl + 2];
    let Some(track) = track_index(tracks, number) else {
        return Ok(None);
    };
    let hdr = (tl + 3) as u64;
    let lacing = Lacing::from_flags(flags);
    if lacing == Lacing::None {
        frames.push((data_start + hdr, (len - hdr) as u32));
    } else {
        let body = io.read_vec(data_start + hdr, len - hdr)?;
        match parse_lacing(lacing, &body) {
            Ok(f) => frames.extend(
                f.into_iter()
                    .map(|(o, l)| (data_start + hdr + o as u64, l as u32)),
            ),
            Err(_) => return Ok(None),
        }
    }
    Ok(Some((track, rel, flags)))
}

pub(crate) fn parse_block_elem<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    h: &Hdr,
    end: u64,
    cluster_ts: i64,
    tracks: &[Track],
    frames: &mut Vec<(u64, u32)>,
) -> Result<Option<BlockRef>> {
    let ds = h.data_start();
    match h.id {
        SIMPLE_BLOCK => {
            let Some((track, rel, flags)) = parse_block(io, ds, end - ds, tracks, frames)? else {
                return Ok(None);
            };
            Ok(Some(BlockRef {
                track,
                offset: h.pos,
                ts: cluster_ts.saturating_add(rel as i64),
                duration: None,
                keyframe: flags & 0x80 != 0,
            }))
        }
        BLOCK_GROUP => {
            let mut pos = ds;
            let mut block = None;
            let (mut duration, mut has_ref) = (None, false);
            while pos < end {
                let Some(c) = io.header(pos, end)? else {
                    break;
                };
                let ce = c.end(end);
                match c.id {
                    BLOCK => {
                        block =
                            parse_block(io, c.data_start(), ce - c.data_start(), tracks, frames)?
                    }
                    BLOCK_DURATION => duration = Some(io.uint(&c, ce)?),
                    REFERENCE_BLOCK => has_ref = true,
                    _ => {}
                }
                pos = ce;
            }
            let Some((track, rel, _)) = block else {
                return Ok(None);
            };
            Ok(Some(BlockRef {
                track,
                offset: h.pos,
                ts: cluster_ts.saturating_add(rel as i64),
                duration,
                keyframe: !has_ref,
            }))
        }
        _ => Ok(None),
    }
}

pub(crate) fn expand(
    b: &BlockRef,
    frames: &[(u64, u32)],
    t: &Track,
    scale: u64,
    skip: u16,
    mut out: impl FnMut(FrameRef),
) {
    let n = frames.len() as u64;
    let block_ns: Option<u128> = match (b.duration, t.default_duration) {
        (Some(d), _) => Some(d as u128 * scale as u128),
        (None, Some(dd)) => Some(dd as u128 * n as u128),
        _ => None,
    };
    let scale = scale as i128;
    let ticks = |ns: i128| -> i64 {
        (if ns >= 0 {
            (ns + scale / 2) / scale
        } else {
            (ns - scale / 2) / scale
        })
        .clamp(i64::MIN as i128, i64::MAX as i128) as i64
    };
    let delay = ticks(t.codec_delay as i128);
    let base = b.ts as i128 * scale;
    for (i, &(offset, size)) in frames.iter().enumerate().skip(skip as usize) {
        let (start, end) = match block_ns {
            Some(total) => (
                (total * i as u128 / n as u128) as i128,
                Some((total * (i as u128 + 1) / n as u128) as i128),
            ),
            None => (0, None),
        };
        let duration = end.map_or(0, |e| {
            ticks(e)
                .saturating_sub(ticks(start))
                .clamp(0, u32::MAX as i64)
        });
        out(FrameRef {
            track: b.track,
            offset,
            size,
            pts: ticks(base + start).saturating_sub(delay),
            duration: duration as u32,
            keyframe: b.keyframe,
            block: b.offset,
            lace: i as u16,
        });
    }
}

pub(crate) fn scan_cluster<S: ByteSource + ?Sized>(
    io: &mut Io<'_, S>,
    h: &Hdr,
    limit: u64,
    tracks: &[Track],
    warnings: &mut Vec<String>,
    frames: &mut Vec<(u64, u32)>,
    mut each: impl FnMut(&BlockRef, &[(u64, u32)]),
) -> Result<Cluster> {
    let ds = h.data_start();
    let known_end = match h.size {
        Size::Known(_) => Some(h.end(limit)),
        Size::Unknown => None,
    };
    let lim = known_end.unwrap_or(limit);
    let mut ts: i64 = 0;
    let mut pos = ds;
    let end = loop {
        if pos >= lim {
            break lim;
        }
        let Some(c) = io.header(pos, lim)? else {
            warnings.push(format!(
                "damaged cluster at {}: undecodable element at {pos}",
                h.pos
            ));
            break io.resync(pos + 1, limit)?.unwrap_or(limit);
        };
        if known_end.is_none() && is_top_level(c.id) {
            break pos;
        }
        if c.size == Size::Unknown || c.overruns(lim) {
            warnings.push(format!(
                "damaged cluster at {}: element 0x{:X} at {pos} overruns",
                h.pos, c.id
            ));
            if c.overruns(lim)
                && matches!(c.id, SIMPLE_BLOCK | BLOCK_GROUP)
                && io.len <= c.data_start() + c.size.known().unwrap_or(0)
            {
                break lim;
            }
            break io.resync(pos + 1, limit)?.unwrap_or(limit);
        }
        let ce = c.end(lim);
        match c.id {
            TIMESTAMP => ts = io.uint(&c, ce)? as i64,
            SIMPLE_BLOCK | BLOCK_GROUP => {
                if let Some(b) = parse_block_elem(io, &c, ce, ts, tracks, frames)? {
                    each(&b, frames);
                }
            }
            _ => {}
        }
        pos = ce;
    };
    Ok(Cluster {
        offset: h.pos,
        data_start: ds,
        end,
        timestamp: ts.max(0) as u64,
    })
}

pub struct Reader<S: ByteSource = File> {
    source: S,
    len: u64,
    pub doc_type: String,
    pub info: Info,
    pub tracks: Vec<Track>,
    pub cues: Vec<CuePoint>,
    pub clusters: Vec<Cluster>,
    pub warnings: Vec<String>,
    pub indexed: bool,
    pub(crate) segment_start: u64,
    pub(crate) segment_end: u64,
    pub(crate) first_cluster: Option<u64>,
}

struct Head {
    doc_type: String,
    info: Info,
    tracks: Vec<Track>,
    cues: Vec<CuePoint>,
    clusters: Vec<Cluster>,
    warnings: Vec<String>,
    segment_start: u64,
    segment_end: u64,
    first_cluster: Option<u64>,
    seek_head: Vec<SeekEntry>,
}

impl Reader<File> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::new(File::open(path).map_err(|e| Error::at(path, e))?)
    }

    pub fn open_with(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let path = path.as_ref();
        Self::with_options(File::open(path).map_err(|e| Error::at(path, e))?, options)
    }
}

impl<S: ByteSource> Reader<S> {
    pub fn new(source: S) -> Result<Self> {
        Self::with_options(source, Options::default())
    }

    pub fn with_options(source: S, options: Options) -> Result<Self> {
        let mut cache = Cache::default();
        let mut io = Io::new(&source, &mut cache);
        let len = io.len;
        let start = {
            let b = io.peek(0, WIN)?;
            b.windows(4)
                .position(|w| w == [0x1A, 0x45, 0xDF, 0xA3])
                .ok_or_else(|| Error::Invalid("not a Matroska file: no EBML header".into()))?
                as u64
        };
        let eh = io
            .header(start, len)?
            .ok_or_else(|| Error::Invalid("not a Matroska file: bad EBML header".into()))?;
        let hdr_end = eh.end(len);
        let hdr = io.read_vec(eh.data_start(), hdr_end - eh.data_start())?;
        let mut doc_type = "matroska".to_string();
        for e in Children::new(&hdr) {
            match e.id {
                DOC_TYPE => doc_type = ebml::string(e.data),
                EBML_MAX_ID_LENGTH if ebml::uint(e.data) > 4 => {
                    return Err(Error::Unsupported("EBMLMaxIDLength above 4".into()));
                }
                EBML_MAX_SIZE_LENGTH if ebml::uint(e.data) > 8 => {
                    return Err(Error::Unsupported("EBMLMaxSizeLength above 8".into()));
                }
                _ => {}
            }
        }
        if doc_type != "matroska" && doc_type != "webm" {
            return Err(Error::Invalid(format!(
                "not a Matroska file: DocType {doc_type:?}"
            )));
        }
        let mut pos = hdr_end;
        let seg = loop {
            let h = io
                .header(pos, len)?
                .ok_or_else(|| Error::Invalid("no Matroska Segment".into()))?;
            if h.id == SEGMENT {
                break h;
            }
            if h.size == Size::Unknown {
                return Err(Error::Invalid(
                    "unknown-size element before the Segment".into(),
                ));
            }
            pos = h.end(len);
        };
        let mut head = Head {
            doc_type,
            info: Info::default(),
            tracks: Vec::new(),
            cues: Vec::new(),
            clusters: Vec::new(),
            warnings: Vec::new(),
            segment_start: seg.data_start(),
            segment_end: seg.end(len),
            first_cluster: None,
            seek_head: Vec::new(),
        };
        if seg.overruns(len) {
            head.warnings
                .push("Segment extends past the end of the file".into());
        }
        let mut w = Walker {
            index: options.index,
            parsed: HashSet::new(),
            frames: Vec::new(),
        };
        w.walk(&mut io, &mut head)?;
        for t in &mut head.tracks {
            fill_durations(t);
            t.finish_indexes();
        }
        Ok(Reader {
            source,
            len,
            doc_type: head.doc_type,
            info: head.info,
            tracks: head.tracks,
            cues: head.cues,
            clusters: head.clusters,
            warnings: head.warnings,
            indexed: options.index,
            segment_start: head.segment_start,
            segment_end: head.segment_end,
            first_cluster: head.first_cluster,
        })
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn is_webm(&self) -> bool {
        self.doc_type == "webm"
    }

    pub fn duration_seconds(&self) -> f64 {
        if let Some(d) = self.info.duration.filter(|&d| d > 0.0) {
            return d * self.info.timestamp_scale as f64 / 1e9;
        }
        self.tracks
            .iter()
            .map(Track::duration_seconds)
            .fold(0.0, f64::max)
    }

    pub fn video(&self) -> Option<usize> {
        let mut it = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Video);
        let first = it.clone().next().map(|x| x.0);
        it.find(|(_, t)| t.default).map(|x| x.0).or(first)
    }

    pub fn audio(&self) -> Vec<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Audio)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn read(&self, track: usize, index: usize) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        self.read_into(track, index, &mut data)?;
        Ok(data)
    }

    pub fn read_into(&self, track: usize, index: usize, data: &mut Vec<u8>) -> Result<()> {
        let t = self
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let s = t
            .samples
            .get(index)
            .ok_or_else(|| Error::Invalid(format!("no sample {index} in track {track}")))?;
        read_frame(&self.source, self.len, t, s.offset, s.size, data)
    }

    pub fn packet(&self, track: usize, index: usize) -> Result<Packet> {
        let data = self.read(track, index)?;
        let s = self.tracks[track].samples[index];
        Ok(Packet {
            data,
            pts: s.pts,
            dts: s.pts,
            keyframe: s.keyframe,
        })
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

pub(crate) fn read_frame<S: ByteSource + ?Sized>(
    src: &S,
    len: u64,
    t: &Track,
    offset: u64,
    size: u32,
    data: &mut Vec<u8>,
) -> Result<()> {
    if !t.frames_readable() {
        return Err(Error::Unsupported(format!(
            "compressed or encrypted frames in track {}",
            t.number
        )));
    }
    if offset.checked_add(size as u64).is_none_or(|e| e > len) {
        return Err(eof());
    }
    let prefix = t.stripped_header().unwrap_or(&[]);
    data.clear();
    data.reserve(prefix.len() + size as usize);
    data.extend_from_slice(prefix);
    data.resize(prefix.len() + size as usize, 0);
    src.read_at(offset, &mut data[prefix.len()..])?;
    Ok(())
}

fn fill_durations(t: &mut Track) {
    if t.samples.iter().all(|s| s.duration > 0) {
        return;
    }
    let mut order: Vec<usize> = (0..t.samples.len()).collect();
    order.sort_by_key(|&i| (t.samples[i].pts, i));
    let mut last = 0u32;
    for w in 0..order.len() {
        let i = order[w];
        if t.samples[i].duration > 0 {
            last = t.samples[i].duration;
            continue;
        }
        let d = match order.get(w + 1) {
            Some(&j) => t.samples[j]
                .pts
                .saturating_sub(t.samples[i].pts)
                .clamp(0, u32::MAX as i64) as u32,
            None => last,
        };
        t.samples[i].duration = d;
        if d > 0 {
            last = d;
        }
    }
}

struct Walker {
    index: bool,
    parsed: HashSet<u64>,
    frames: Vec<(u64, u32)>,
}

impl Walker {
    fn walk<S: ByteSource + ?Sized>(&mut self, io: &mut Io<'_, S>, r: &mut Head) -> Result<()> {
        let seg_end = r.segment_end;
        let mut pos = r.segment_start;
        while pos < seg_end {
            let Some(h) = io.header(pos, seg_end)? else {
                match io.resync(pos + 1, seg_end)? {
                    Some(p) => {
                        r.warnings
                            .push(format!("undecodable element at {pos}; resynced at {p}"));
                        pos = p;
                        continue;
                    }
                    None => break,
                }
            };
            match h.id {
                CLUSTER => {
                    if r.first_cluster.is_none() {
                        r.first_cluster = Some(h.pos);
                        self.follow_seek_head(io, r, &[INFO, TRACKS])?;
                        if !self.index {
                            break;
                        }
                    }
                    let cluster = r.clusters.len() as u32;
                    let scale = r.info.timestamp_scale;
                    let mut samples: Vec<Vec<Sample>> = r
                        .tracks
                        .iter_mut()
                        .map(|t| std::mem::take(&mut t.samples))
                        .collect();
                    let tracks = &r.tracks;
                    let ci = scan_cluster(
                        io,
                        &h,
                        seg_end,
                        tracks,
                        &mut r.warnings,
                        &mut self.frames,
                        |b, frames| {
                            expand(b, frames, &tracks[b.track], scale, 0, |f| {
                                samples[f.track].push(Sample {
                                    offset: f.offset,
                                    size: f.size,
                                    pts: f.pts,
                                    duration: f.duration,
                                    keyframe: f.keyframe,
                                    cluster,
                                    block: f.block,
                                    lace: f.lace,
                                })
                            })
                        },
                    )?;
                    for (t, s) in r.tracks.iter_mut().zip(samples) {
                        t.samples = s;
                    }
                    r.clusters.push(ci);
                    pos = ci.end.max(h.data_start());
                }
                SEEK_HEAD | INFO | TRACKS | CUES | CHAPTERS | TAGS | ATTACHMENTS => {
                    pos = self.level1(io, r, &h, seg_end)?;
                }
                EBML | SEGMENT => break,
                _ => {
                    if h.size == Size::Unknown {
                        pos = io.unknown_end(h.data_start(), seg_end)?;
                    } else if h.overruns(seg_end) && h.id != VOID {
                        match io.resync(pos + 1, seg_end)? {
                            Some(p) => {
                                r.warnings.push(format!(
                                    "oversized element 0x{:X} at {pos}; resynced at {p}",
                                    h.id
                                ));
                                pos = p;
                            }
                            None => break,
                        }
                    } else {
                        pos = h.end(seg_end);
                    }
                }
            }
        }
        self.follow_seek_head(io, r, &[INFO, TRACKS, CUES, SEEK_HEAD])?;
        self.follow_seek_head(io, r, &[CUES])?;
        r.cues.sort_by_key(|c| c.time);
        Ok(())
    }

    fn follow_seek_head<S: ByteSource + ?Sized>(
        &mut self,
        io: &mut Io<'_, S>,
        r: &mut Head,
        ids: &[u32],
    ) -> Result<()> {
        let entries = r.seek_head.clone();
        for e in entries {
            if !ids.contains(&e.id) {
                continue;
            }
            let Some(abs) = r.segment_start.checked_add(e.position) else {
                continue;
            };
            if self.parsed.contains(&abs) || abs >= r.segment_end {
                continue;
            }
            match io.header(abs, r.segment_end)? {
                Some(h) if h.id == e.id => {
                    self.level1(io, r, &h, r.segment_end)?;
                }
                _ => r.warnings.push(format!(
                    "SeekHead entry 0x{:X} -> {abs} does not point at that element",
                    e.id
                )),
            }
        }
        Ok(())
    }

    fn level1<S: ByteSource + ?Sized>(
        &mut self,
        io: &mut Io<'_, S>,
        r: &mut Head,
        h: &Hdr,
        limit: u64,
    ) -> Result<u64> {
        let end = if h.size == Size::Unknown {
            io.unknown_end(h.data_start(), limit)?
        } else {
            h.end(limit)
        };
        if h.overruns(limit) {
            r.warnings
                .push(format!("element 0x{:X} at {} truncated", h.id, h.pos));
        }
        if !self.parsed.insert(h.pos) || !matches!(h.id, SEEK_HEAD | INFO | TRACKS | CUES) {
            return Ok(end);
        }
        let n = end - h.data_start();
        if n > MAX_META {
            r.warnings.push(format!(
                "element 0x{:X} at {} too large ({n} bytes); skipped",
                h.id, h.pos
            ));
            return Ok(end);
        }
        let data = io.read_vec(h.data_start(), n)?;
        match h.id {
            SEEK_HEAD => r.seek_head.extend(parse::seek_head(&data)),
            INFO => r.info = parse::info(&data),
            TRACKS if r.tracks.is_empty() => {
                r.tracks = parse::tracks(&data, r.info.timestamp_scale);
            }
            CUES => parse::cues(&data, &mut r.cues),
            _ => {}
        }
        Ok(end)
    }
}
