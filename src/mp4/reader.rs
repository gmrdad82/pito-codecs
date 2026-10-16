use crate::media::{Codec, Error, Packet, Result};
use crate::mp4::boxes::{Cur, FourCc, boxes, find, parse_box_header};
use crate::mp4::entry::{CodecConfig, SampleEntry, parse_sample_entry};
use crate::mp4::source::ByteSource;
use crate::mp4::{too_large, truncated};
use std::collections::HashMap;
use std::fs::File;
use std::num::NonZeroU64;
use std::path::Path;

const MAX_SAMPLES: usize = 1 << 26;
const MAX_EMPTY_SAMPLES: u64 = 1 << 16;
const MAX_HEADER_BOX: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub keyframe: bool,
    pub entry: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit {
    pub segment_duration: u64,
    pub media_time: i64,
    pub media_rate: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u32,
    pub kind: TrackKind,
    pub handler: FourCc,
    pub name: String,
    pub timescale: u32,
    pub duration: u64,
    pub language: String,
    pub width: u32,
    pub height: u32,
    pub enabled: bool,
    pub matrix: [i32; 9],
    pub entries: Vec<SampleEntry>,
    pub samples: Vec<Sample>,
    pub edits: Vec<Edit>,
    pub edit_offset: i64,
    sync_index: Vec<u32>,
    pts_order: Vec<u32>,
}

impl Track {
    pub fn entry(&self) -> Option<&SampleEntry> {
        self.entries.first()
    }

    pub fn config(&self) -> Option<&CodecConfig> {
        self.entry().map(|e| &e.codec)
    }

    pub fn codec(&self) -> Option<Codec> {
        match self.config()? {
            CodecConfig::Avc(_) => Some(Codec::H264),
            CodecConfig::Hevc(_) => Some(Codec::Hevc),
            CodecConfig::Vp9(_) => Some(Codec::Vp9),
            CodecConfig::Av1(_) => Some(Codec::Av1),
            CodecConfig::Aac(_) => Some(Codec::Aac),
            _ => None,
        }
    }

    pub fn extradata(&self) -> Option<Vec<u8>> {
        self.config()?.extradata()
    }

    pub fn rotation(&self) -> u16 {
        let [a, b, _, c, d, _, _, _, _] = self.matrix;
        match (a.signum(), b.signum(), c.signum(), d.signum()) {
            (0, 1, -1, 0) => 90,
            (-1, 0, 0, -1) => 180,
            (0, -1, 1, 0) => 270,
            _ => 0,
        }
    }

    pub fn channels(&self) -> u32 {
        match self.config() {
            Some(CodecConfig::Aac(a)) if a.channels() > 0 => a.channels(),
            _ => self.entry().map_or(0, |e| {
                e.ac3_channels()
                    .or(e.audio.as_ref().map(|a| a.channels))
                    .unwrap_or(0)
            }),
        }
    }

    pub fn sample_rate(&self) -> u32 {
        match self.config() {
            Some(CodecConfig::Aac(a)) if a.sample_rate > 0 => a.sample_rate,
            _ => self
                .entry()
                .and_then(|e| e.audio.as_ref())
                .map_or(0, |a| a.sample_rate.round() as u32),
        }
    }

    pub fn seconds(&self, ticks: i64) -> f64 {
        if self.timescale == 0 {
            0.0
        } else {
            ticks as f64 / self.timescale as f64
        }
    }

    pub fn ticks(&self, seconds: f64) -> i64 {
        (seconds * self.timescale as f64).round() as i64
    }

    pub fn duration_seconds(&self) -> f64 {
        let edited: u64 = self
            .edits
            .iter()
            .filter(|e| e.media_time >= 0)
            .fold(0u64, |a, e| a.saturating_add(e.segment_duration));
        let media = self.seconds(self.duration.min(i64::MAX as u64) as i64);
        if edited == 0 {
            media
        } else {
            media.min(self.seconds(self.media_span()))
        }
    }

    fn media_span(&self) -> i64 {
        let end = self
            .samples
            .iter()
            .map(|s| s.pts.saturating_add(s.duration as i64))
            .max()
            .unwrap_or(0);
        end.saturating_add(self.edit_offset).max(0)
    }

    pub fn bitrate(&self) -> u32 {
        let declared = match self.config() {
            Some(CodecConfig::Aac(a)) if a.avg_bitrate > 0 => a.avg_bitrate,
            _ => self
                .entry()
                .and_then(|e| e.bitrate)
                .map_or(0, |b| b.avg_bitrate),
        };
        if declared > 0 {
            return declared;
        }
        let bytes: u64 = self.samples.iter().map(|s| s.size as u64).sum();
        let ticks: u64 = self.samples.iter().map(|s| s.duration as u64).sum();
        if ticks == 0 || self.timescale == 0 {
            return 0;
        }
        (bytes as u128 * 8 * self.timescale as u128 / ticks as u128).min(u32::MAX as u128) as u32
    }

    pub fn fps(&self) -> f64 {
        let ticks: u64 = self.samples.iter().map(|s| s.duration as u64).sum();
        if ticks == 0 || self.timescale == 0 {
            0.0
        } else {
            self.samples.len() as f64 * self.timescale as f64 / ticks as f64
        }
    }

    pub fn presentation(&self, index: usize) -> Option<i64> {
        self.samples
            .get(index)
            .map(|s| s.pts.saturating_add(self.edit_offset))
    }

    pub fn sample_at(&self, presentation: i64) -> Option<usize> {
        let pts = presentation.saturating_sub(self.edit_offset);
        if self.pts_order.is_empty() {
            return None;
        }
        let k = self
            .pts_order
            .partition_point(|&i| self.samples[i as usize].pts <= pts);
        Some(self.pts_order[k.saturating_sub(1)] as usize)
    }

    pub fn keyframe_before(&self, index: usize) -> usize {
        let k = self.sync_index.partition_point(|&i| i as usize <= index);
        if k == 0 {
            0
        } else {
            self.sync_index[k - 1] as usize
        }
    }

    pub fn keyframes(&self) -> &[u32] {
        &self.sync_index
    }

    fn finish_indexes(&mut self) {
        self.sync_index = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.keyframe)
            .map(|(i, _)| i as u32)
            .collect();
        let mut order: Vec<u32> = (0..self.samples.len() as u32).collect();
        order.sort_by_key(|&i| (self.samples[i as usize].pts, i));
        self.pts_order = order;
    }
}

pub struct Reader<S: ByteSource = File> {
    source: S,
    len: u64,
    pub brand: FourCc,
    pub quicktime: bool,
    pub timescale: u32,
    pub duration: u64,
    pub tracks: Vec<Track>,
    pub fragmented: bool,
}

impl Reader<File> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        File::open(path)
            .map_err(Error::from)
            .and_then(Self::new)
            .map_err(|e| e.in_file(path))
    }
}

impl<S: ByteSource> Reader<S> {
    pub fn new(source: S) -> Result<Self> {
        open(source)
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn duration_seconds(&self) -> f64 {
        let movie = if self.timescale == 0 {
            0.0
        } else {
            self.duration as f64 / self.timescale as f64
        };
        let tracks = self
            .tracks
            .iter()
            .map(Track::duration_seconds)
            .fold(0.0, f64::max);
        if movie > 0.0 { movie } else { tracks }
    }

    pub fn video(&self) -> Option<usize> {
        self.tracks.iter().position(|t| t.kind == TrackKind::Video)
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
        read_range(&self.source, self.len, s.offset, s.size as u64, data)
    }

    pub fn read_span(&self, offset: u64, data: &mut Vec<u8>) -> Result<()> {
        read_range(&self.source, self.len, offset, data.len() as u64, data)
    }

    pub fn packet(&self, track: usize, index: usize) -> Result<Packet> {
        let data = self.read(track, index)?;
        let (t, s) = self
            .tracks
            .get(track)
            .and_then(|t| Some((t, *t.samples.get(index)?)))
            .ok_or_else(|| Error::Invalid(format!("no sample {index} in track {track}")))?;
        Ok(Packet {
            data,
            pts: s.pts.saturating_add(t.edit_offset),
            dts: s.dts.saturating_add(t.edit_offset),
            keyframe: s.keyframe,
        })
    }
}

fn times(c: &mut Cur, v: u8) -> Result<(u32, u64)> {
    c.skip(if v == 1 { 16 } else { 8 })?;
    let timescale = c.u32()?;
    let duration = if v == 1 { c.u64()? } else { c.u32()? as u64 };
    Ok((timescale, duration))
}

fn read_range<S: ByteSource + ?Sized>(
    src: &S,
    len: u64,
    offset: u64,
    size: u64,
    data: &mut Vec<u8>,
) -> Result<()> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| truncated("sample"))?;
    if end > len {
        return Err(truncated("sample data"));
    }
    data.resize(size as usize, 0);
    src.read_at(offset, data)?;
    Ok(())
}

fn parse_name(udta: &[u8]) -> Option<String> {
    for b in boxes(udta).map_while(|b| b.ok()) {
        let text = if b.kind == b"name" {
            b.payload
        } else if b.kind == [0xA9, b'n', b'a', b'm'] && b.payload.len() >= 4 {
            let n = u16::from_be_bytes([b.payload[0], b.payload[1]]) as usize;
            b.payload.get(4..4 + n).unwrap_or(&[])
        } else {
            continue;
        };
        let text = text.split(|&c| c == 0).next().unwrap_or(&[]);
        if !text.is_empty() {
            return Some(String::from_utf8_lossy(text).into_owned());
        }
    }
    None
}

struct Movie {
    brand: FourCc,
    quicktime: bool,
    timescale: u32,
    duration: u64,
    tracks: Vec<Track>,
    fragmented: bool,
    trex: HashMap<u32, Trex>,
    next_dts: Vec<i64>,
    room: u64,
}

fn movie(ftyp: Option<&[u8]>, moov: &[u8], len: u64) -> Result<Movie> {
    let mut m = Movie {
        brand: FourCc::default(),
        quicktime: true,
        timescale: 0,
        duration: 0,
        tracks: Vec::new(),
        fragmented: false,
        trex: HashMap::new(),
        next_dts: Vec::new(),
        room: len.saturating_mul(2),
    };
    if let Some(f) = ftyp {
        let mut c = Cur::new(f, "ftyp");
        m.brand = c.fourcc()?;
        m.quicktime = m.brand == b"qt  ";
    }
    let mut trak_payloads = Vec::new();
    for b in boxes(moov) {
        let b = b?;
        match &b.kind.0 {
            b"cmov" => return Err(Error::Unsupported("compressed movie header (cmov)".into())),
            b"mvhd" => {
                let mut c = Cur::new(b.payload, "mvhd");
                let (v, _) = c.full_header()?;
                (m.timescale, m.duration) = times(&mut c, v)?;
            }
            b"trak" => trak_payloads.push(b.payload),
            b"mvex" => {
                m.fragmented = true;
                for x in boxes(b.payload).map_while(|b| b.ok()) {
                    if x.kind == b"trex" {
                        let mut c = Cur::new(x.payload, "trex");
                        c.full_header()?;
                        let id = c.u32()?;
                        m.trex.insert(
                            id,
                            Trex {
                                description_index: c.u32()?,
                                duration: c.u32()?,
                                size: c.u32()?,
                                flags: c.u32()?,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
    for p in trak_payloads {
        let t = parse_trak(p, m.timescale, m.quicktime, len, &mut m.room)?;
        m.tracks.push(t);
    }
    m.next_dts = m
        .tracks
        .iter()
        .map(|t| {
            t.samples
                .last()
                .map(|s| s.dts.saturating_add(s.duration as i64))
                .unwrap_or(0)
        })
        .collect();
    Ok(m)
}

fn open<S: ByteSource>(src: S) -> Result<Reader<S>> {
    let len = src.len();
    let mut offset = 0u64;
    let mut ftyp: Option<Vec<u8>> = None;
    let mut moov: Option<Vec<u8>> = None;
    let mut built: Option<Movie> = None;
    let mut later: Vec<(u64, u64, u64)> = Vec::new();
    let mut any_moof = false;
    while len.saturating_sub(offset) >= 8 {
        let n = (len - offset).min(32) as usize;
        let mut hb = [0u8; 32];
        src.read_at(offset, &mut hb[..n])?;
        let h = parse_box_header(&hb[..n])?;
        let size = h.size.unwrap_or(len - offset);
        let end = offset.saturating_add(size).min(len);
        let at = offset + h.header_len;
        let body_len = end.saturating_sub(at);
        let body = |max: u64| -> Result<Vec<u8>> {
            if body_len > max {
                return Err(too_large("header box"));
            }
            let mut data = Vec::new();
            read_range(&src, len, at, body_len, &mut data)?;
            Ok(data)
        };
        match &h.kind.0 {
            b"ftyp" if ftyp.is_none() => ftyp = Some(body(1 << 16)?),
            b"moov" if moov.is_none() && built.is_none() => moov = Some(body(MAX_HEADER_BOX)?),
            b"moof" => {
                if body_len > MAX_HEADER_BOX {
                    return Err(too_large("header box"));
                }
                any_moof = true;
                if built.is_none()
                    && later.is_empty()
                    && let Some(m) = moov.take()
                {
                    built = Some(movie(ftyp.as_deref(), &m, len)?);
                }
                match &mut built {
                    Some(m) if later.is_empty() => {
                        let moof = body(MAX_HEADER_BOX)?;
                        parse_moof(
                            offset,
                            &moof,
                            &mut m.tracks,
                            &m.trex,
                            &mut m.next_dts,
                            (len, &mut m.room),
                        )?;
                    }
                    _ => later.push((offset, at, body_len)),
                }
            }
            _ => {}
        }
        if h.size.is_none() {
            break;
        }
        offset = offset.saturating_add(size);
    }
    let mut m = match built {
        Some(m) => m,
        None => {
            let moov = moov.ok_or_else(|| Error::Invalid("no moov box".into()))?;
            movie(ftyp.as_deref(), &moov, len)?
        }
    };
    let mut moof = Vec::new();
    for (moof_off, at, body_len) in later {
        read_range(&src, len, at, body_len, &mut moof)?;
        parse_moof(
            moof_off,
            &moof,
            &mut m.tracks,
            &m.trex,
            &mut m.next_dts,
            (len, &mut m.room),
        )?;
    }
    let mut file = Reader {
        source: src,
        len,
        brand: m.brand,
        quicktime: m.quicktime,
        timescale: m.timescale,
        duration: m.duration,
        tracks: m.tracks,
        fragmented: m.fragmented,
    };
    for t in &mut file.tracks {
        if file.fragmented && any_moof {
            let d: u64 = t.samples.iter().map(|s| s.duration as u64).sum();
            t.duration = t.duration.max(d);
            if t.timescale > 0 && file.timescale > 0 {
                let md = (t.duration as u128 * file.timescale as u128 / t.timescale as u128) as u64;
                file.duration = file.duration.max(md);
            }
        }
        t.finish_indexes();
    }
    Ok(file)
}

#[derive(Clone, Copy, Debug, Default)]
struct Trex {
    description_index: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

fn parse_language(code: u16) -> String {
    if code < 0x400 {
        return if code == 0 {
            "eng".into()
        } else {
            "und".into()
        };
    }
    let c = |s: u16| (((code >> s) & 0x1F) as u8 + 0x60) as char;
    let s: String = [c(10), c(5), c(0)].into_iter().collect();
    if s.chars().all(|ch| ch.is_ascii_lowercase()) {
        s
    } else {
        "und".into()
    }
}

fn rescale(v: i128, from: u32, to: u32) -> i64 {
    if from == 0 {
        return 0;
    }
    (v * to as i128 / from as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn parse_trak(
    p: &[u8],
    movie_timescale: u32,
    quicktime: bool,
    file_len: u64,
    room: &mut u64,
) -> Result<Track> {
    let mut t = Track {
        id: 0,
        kind: TrackKind::Other,
        handler: FourCc::default(),
        name: String::new(),
        timescale: 0,
        duration: 0,
        language: "und".into(),
        width: 0,
        height: 0,
        enabled: true,
        matrix: [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000],
        entries: Vec::new(),
        samples: Vec::new(),
        edits: Vec::new(),
        edit_offset: 0,
        sync_index: Vec::new(),
        pts_order: Vec::new(),
    };
    let mut mdia = None;
    let mut name = None;
    for b in boxes(p) {
        let b = b?;
        match &b.kind.0 {
            b"tkhd" => {
                let mut c = Cur::new(b.payload, "tkhd");
                let (v, flags) = c.full_header()?;
                t.enabled = flags & 1 != 0;
                c.skip(if v == 1 { 16 } else { 8 })?;
                t.id = c.u32()?;
                c.skip(if v == 1 { 28 } else { 24 })?;
                for m in t.matrix.iter_mut() {
                    *m = c.i32()?;
                }
                t.width = c.u32()? >> 16;
                t.height = c.u32()? >> 16;
            }
            b"edts" => {
                if let Some(e) = find(b.payload, b"elst") {
                    let mut c = Cur::new(e, "elst");
                    let (v, _) = c.full_header()?;
                    let n = c.count(if v == 1 { 20 } else { 12 })?;
                    for _ in 0..n {
                        let (segment_duration, media_time) = if v == 1 {
                            (c.u64()?, c.i64()?)
                        } else {
                            (c.u32()? as u64, c.i32()? as i64)
                        };
                        t.edits.push(Edit {
                            segment_duration,
                            media_time,
                            media_rate: c.i32()?,
                        });
                    }
                }
            }
            b"udta" => {
                if let Some(n) = parse_name(b.payload) {
                    name = Some(n);
                }
            }
            b"mdia" => mdia = Some(b.payload),
            _ => {}
        }
    }
    let mdia = mdia.ok_or_else(|| Error::Invalid("trak without mdia".into()))?;
    let mut minf = None;
    for b in boxes(mdia) {
        let b = b?;
        match &b.kind.0 {
            b"mdhd" => {
                let mut c = Cur::new(b.payload, "mdhd");
                let (v, _) = c.full_header()?;
                (t.timescale, t.duration) = times(&mut c, v)?;
                t.language = parse_language(c.u16()?);
            }
            b"hdlr" => {
                let mut c = Cur::new(b.payload, "hdlr");
                c.full_header()?;
                let _pre = c.u32()?;
                t.handler = c.fourcc()?;
                c.skip(12)?;
                let name = c.rest();
                let s = if !name.is_empty() && name[0] as usize == name.len() - 1 && name.len() > 1
                {
                    &name[1..]
                } else {
                    name.split(|&b| b == 0).next().unwrap_or(&[])
                };
                t.name = String::from_utf8_lossy(s).into_owned();
            }
            b"minf" => minf = Some(b.payload),
            _ => {}
        }
    }
    t.kind = match &t.handler.0 {
        b"vide" => TrackKind::Video,
        b"soun" => TrackKind::Audio,
        b"text" | b"sbtl" | b"subt" | b"clcp" => TrackKind::Subtitle,
        _ => TrackKind::Other,
    };
    let mut empty = 0i128;
    for e in &t.edits {
        if e.media_time == -1 {
            empty += e.segment_duration as i128;
        } else {
            t.edit_offset =
                rescale(empty, movie_timescale, t.timescale).saturating_sub(e.media_time);
            break;
        }
    }
    if let Some(stbl) = minf.and_then(|m| find(m, b"stbl")) {
        parse_stbl(stbl, &mut t, quicktime, file_len, room)?;
    }
    if let Some(n) = name {
        t.name = n;
    }
    Ok(t)
}

struct SttsCursor<'a> {
    runs: &'a [(u32, u32)],
    run: usize,
    used: u32,
}

impl SttsCursor<'_> {
    fn take(&mut self, mut n: u64) -> u64 {
        let mut total = 0u64;
        while n > 0 && self.run < self.runs.len() {
            let (count, delta) = self.runs[self.run];
            let avail = (count - self.used) as u64;
            let k = avail.min(n);
            total = total.saturating_add(k.saturating_mul(delta as u64));
            n -= k;
            self.used += k as u32;
            if self.used >= count {
                self.run += 1;
                self.used = 0;
            }
        }
        total
    }
    fn next_value(&mut self) -> u32 {
        while self.run < self.runs.len() && self.runs[self.run].0 == 0 {
            self.run += 1;
        }
        let Some(&(_, d)) = self.runs.get(self.run) else {
            return 0;
        };
        self.take(1);
        d
    }
}

fn chunk_capacity(stsc: &[(u32, u32, u32)], chunks: usize) -> u64 {
    let end = chunks as u64 + 1;
    stsc.iter()
        .enumerate()
        .map(|(i, &(first, spc, _))| {
            let next = stsc.get(i + 1).map_or(end, |n| (n.0 as u64).min(end));
            next.saturating_sub(first as u64).saturating_mul(spc as u64)
        })
        .fold(0u64, u64::saturating_add)
}

fn parse_stbl(
    stbl: &[u8],
    t: &mut Track,
    is_qt: bool,
    file_len: u64,
    room: &mut u64,
) -> Result<()> {
    let mut stts: Vec<(u32, u32)> = Vec::new();
    let mut ctts: Vec<(u32, u32)> = Vec::new();
    let mut stss: Option<Vec<u32>> = None;
    let mut stsc: Vec<(u32, u32, u32)> = Vec::new();
    let mut const_size = 0u32;
    let mut sample_count = 0usize;
    let mut sizes: Vec<u32> = Vec::new();
    let mut chunks: Vec<u64> = Vec::new();
    let mut entry_half = 8u64;
    let mut chunk_half = 8u64;
    for b in boxes(stbl) {
        let b = b?;
        let mut c = Cur::new(b.payload, "stbl child");
        match &b.kind.0 {
            b"stsd" => {
                c.full_header()?;
                let n = c.count(8)?;
                for e in boxes(c.rest()).take(n) {
                    let e = e?;
                    t.entries
                        .push(parse_sample_entry(e.kind, e.payload, t.handler, is_qt)?);
                }
            }
            b"stts" => {
                c.full_header()?;
                let n = c.count(8)?;
                stts = (0..n)
                    .map(|_| Ok((c.u32()?, c.u32()?)))
                    .collect::<Result<_>>()?;
            }
            b"ctts" => {
                c.full_header()?;
                let n = c.count(8)?;
                ctts = (0..n)
                    .map(|_| Ok((c.u32()?, c.u32()?)))
                    .collect::<Result<_>>()?;
            }
            b"stss" => {
                c.full_header()?;
                let n = c.count(4)?;
                let mut v: Vec<u32> = (0..n).map(|_| c.u32()).collect::<Result<_>>()?;
                v.sort_unstable();
                stss = Some(v);
            }
            b"stsc" => {
                c.full_header()?;
                let n = c.count(12)?;
                stsc = (0..n)
                    .map(|_| Ok((c.u32()?, c.u32()?, c.u32()?)))
                    .collect::<Result<_>>()?;
            }
            b"stsz" => {
                c.full_header()?;
                const_size = c.u32()?;
                let n = c.u32()? as usize;
                match NonZeroU64::new(const_size as u64) {
                    None => {
                        c.check_count(n, 4)?;
                        sizes = (0..n).map(|_| c.u32()).collect::<Result<_>>()?;
                        sample_count = n;
                    }
                    Some(size) => {
                        sample_count =
                            usize::try_from((n as u64).min(file_len / size)).unwrap_or(usize::MAX);
                    }
                }
            }
            b"stz2" => {
                c.full_header()?;
                let field = c.u32()? & 0xFF;
                let n = c.u32()? as usize;
                let bits = match field {
                    4 | 8 | 16 => {
                        entry_half = field as u64 / 4;
                        field as usize
                    }
                    _ => return Err(Error::Invalid(format!("stz2 field size {field}"))),
                };
                if n.checked_mul(bits)
                    .is_none_or(|b| b.div_ceil(8) > c.remaining())
                {
                    return Err(truncated("stz2"));
                }
                let d = c.rest();
                sizes = (0..n)
                    .map(|i| match bits {
                        4 => ((d[i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 0xF) as u32,
                        8 => d[i] as u32,
                        _ => u16::from_be_bytes([d[2 * i], d[2 * i + 1]]) as u32,
                    })
                    .collect();
                sample_count = n;
            }
            b"stco" => {
                c.full_header()?;
                let n = c.count(4)?;
                chunks = (0..n)
                    .map(|_| c.u32().map(|v| v as u64))
                    .collect::<Result<_>>()?;
            }
            b"co64" => {
                c.full_header()?;
                let n = c.count(8)?;
                chunk_half = 16;
                chunks = (0..n).map(|_| c.u64()).collect::<Result<_>>()?;
            }
            _ => {}
        }
    }
    if sample_count == 0 || chunks.is_empty() || stsc.is_empty() {
        return Ok(());
    }
    if stsc[0].0 == 0 || stsc.windows(2).any(|w| w[1].0 <= w[0].0) {
        return Err(Error::Invalid(
            "stsc first_chunk values are not increasing from 1".into(),
        ));
    }
    let pcm = match t.entries.first().map(|e| &e.codec) {
        Some(CodecConfig::Pcm(p)) if const_size != 0 => Some(*p),
        _ => None,
    };
    let mut stts_c = SttsCursor {
        runs: &stts,
        run: 0,
        used: 0,
    };
    let mut ctts_c = SttsCursor {
        runs: &ctts,
        run: 0,
        used: 0,
    };
    let mut samples = Vec::new();
    let mut sidx = 0usize;
    let mut dts = 0i64;
    let mut stsc_i = 0usize;
    if let Some(p) = pcm {
        let a = t.entry().and_then(|e| e.audio.clone()).unwrap_or_default();
        let frame_bytes = |frames: u64| -> u64 {
            if a.qt_version == 1 && a.bytes_per_frame > 0 && a.samples_per_packet > 0 {
                frames / a.samples_per_packet as u64 * a.bytes_per_frame as u64
            } else if const_size > 1 {
                frames * const_size as u64
            } else {
                frames * p.bytes_per_frame() as u64
            }
        };
        if chunks.len() > MAX_SAMPLES {
            return Err(too_large("chunk count"));
        }
        let fit = usize::try_from(*room / chunk_half).unwrap_or(usize::MAX);
        let chunks = chunks.get(..chunks.len().min(fit)).unwrap_or(&[]);
        *room = room.saturating_sub((chunks.len() as u64).saturating_mul(chunk_half));
        samples.reserve(chunks.len());
        for (ci, &off) in chunks.iter().enumerate() {
            while stsc_i + 1 < stsc.len() && stsc[stsc_i + 1].0 as usize <= ci + 1 {
                stsc_i += 1;
            }
            let spc = (stsc[stsc_i].1 as usize).min(sample_count - sidx);
            if spc == 0 {
                continue;
            }
            let dur = stts_c.take(spc as u64);
            let size = frame_bytes(spc as u64).min(u32::MAX as u64) as u32;
            samples.push(Sample {
                offset: off,
                size,
                dts,
                pts: dts,
                duration: dur.min(u32::MAX as u64) as u32,
                keyframe: true,
                entry: stsc[stsc_i].2.saturating_sub(1),
            });
            dts = dts.saturating_add(dur.min(i64::MAX as u64) as i64);
            sidx += spc;
            if sidx >= sample_count {
                break;
            }
        }
    } else {
        let capacity = chunk_capacity(&stsc, chunks.len());
        let mut sample_count = sample_count.min(usize::try_from(capacity).unwrap_or(usize::MAX));
        if const_size != 0 {
            let cost = const_size as u64 * 2;
            let fit = usize::try_from(*room / cost).unwrap_or(usize::MAX);
            sample_count = sample_count.min(fit);
            *room = room.saturating_sub((sample_count as u64).saturating_mul(cost));
        } else {
            let mut fit = 0usize;
            for &size in sizes.iter().take(sample_count) {
                let cost = size as u64 * 2 + entry_half;
                if cost > *room {
                    break;
                }
                *room -= cost;
                fit += 1;
            }
            sample_count = fit;
        }
        if sample_count > MAX_SAMPLES {
            return Err(too_large("sample count"));
        }
        samples.reserve(sample_count);
        let mut stss_i = 0usize;
        'chunks: for (ci, &off) in chunks.iter().enumerate() {
            while stsc_i + 1 < stsc.len() && stsc[stsc_i + 1].0 as usize <= ci + 1 {
                stsc_i += 1;
            }
            let (_, spc, sdi) = stsc[stsc_i];
            let mut pos = off;
            for _ in 0..spc {
                if sidx >= sample_count {
                    break 'chunks;
                }
                let size = if const_size != 0 {
                    const_size
                } else {
                    *sizes.get(sidx).ok_or_else(|| truncated("stsz"))?
                };
                let duration = stts_c.next_value();
                let cts = ctts_c.next_value() as i32 as i64;
                let keyframe = match &stss {
                    None => true,
                    Some(v) => {
                        while stss_i < v.len() && (v[stss_i] as usize) < sidx + 1 {
                            stss_i += 1;
                        }
                        stss_i < v.len() && v[stss_i] as usize == sidx + 1
                    }
                };
                samples.push(Sample {
                    offset: pos,
                    size,
                    dts,
                    pts: dts.saturating_add(cts),
                    duration,
                    keyframe,
                    entry: sdi.saturating_sub(1),
                });
                pos = pos.saturating_add(size as u64);
                dts = dts.saturating_add(duration as i64);
                sidx += 1;
            }
        }
    }
    t.samples = samples;
    Ok(())
}

fn parse_moof(
    moof_off: u64,
    moof: &[u8],
    tracks: &mut [Track],
    trex: &HashMap<u32, Trex>,
    next_dts: &mut [i64],
    (file_len, room): (u64, &mut u64),
) -> Result<()> {
    let mut prev_end: Option<u64> = None;
    for b in boxes(moof) {
        let b = b?;
        if b.kind != b"traf" {
            continue;
        }
        let mut tfhd = None;
        let mut tfdt = None;
        let mut truns = Vec::new();
        for c in boxes(b.payload) {
            let c = c?;
            match &c.kind.0 {
                b"tfhd" => tfhd = Some(c.payload),
                b"tfdt" => tfdt = Some(c.payload),
                b"trun" => truns.push(c.payload),
                _ => {}
            }
        }
        let tfhd = tfhd.ok_or_else(|| Error::Invalid("traf without tfhd".into()))?;
        let mut c = Cur::new(tfhd, "tfhd");
        let (_, fl) = c.full_header()?;
        let id = c.u32()?;
        let Some(ti) = tracks.iter().position(|t| t.id == id) else {
            continue;
        };
        let d = trex.get(&id).copied().unwrap_or_default();
        let base = if fl & 1 != 0 {
            c.u64()?
        } else if fl & 0x20000 != 0 {
            moof_off
        } else {
            prev_end.unwrap_or(moof_off)
        };
        let sdi = if fl & 2 != 0 {
            c.u32()?
        } else {
            d.description_index
        };
        let def_dur = if fl & 8 != 0 { c.u32()? } else { d.duration };
        let def_size = if fl & 0x10 != 0 { c.u32()? } else { d.size };
        let def_flags = if fl & 0x20 != 0 { c.u32()? } else { d.flags };
        if let Some(p) = tfdt {
            let mut c = Cur::new(p, "tfdt");
            let (v, _) = c.full_header()?;
            let t = if v == 1 { c.u64()? } else { c.u32()? as u64 };
            next_dts[ti] = t.min(i64::MAX as u64) as i64;
        }
        let mut data_pos = base;
        for tr in truns {
            let mut c = Cur::new(tr, "trun");
            let (v, tf) = c.full_header()?;
            let per = [0x100u32, 0x200, 0x400, 0x800]
                .iter()
                .filter(|&&f| tf & f != 0)
                .count()
                * 4;
            let n = c.u32()? as usize;
            let total = (tracks[ti].samples.len() as u64).saturating_add(n as u64);
            if per > 0 {
                c.check_count(n, per)?;
            } else if def_size == 0 && total > (file_len / 64).max(MAX_EMPTY_SAMPLES) {
                return Err(too_large("trun sample count"));
            }
            let data = if tf & 0x200 == 0 { def_size as u64 } else { 0 };
            let cost = (per as u64 + data)
                .saturating_mul(2)
                .saturating_mul(n as u64);
            if cost > *room {
                return Err(too_large("trun sample count"));
            }
            *room -= cost;
            if tracks[ti].samples.len() + n > MAX_SAMPLES {
                return Err(too_large("sample count"));
            }
            if tf & 1 != 0 {
                data_pos = base.wrapping_add_signed(c.i32()? as i64);
            }
            let first_flags = if tf & 4 != 0 { Some(c.u32()?) } else { None };
            let t = &mut tracks[ti];
            t.samples.reserve(n);
            for i in 0..n {
                let dur = if tf & 0x100 != 0 { c.u32()? } else { def_dur };
                let size = if tf & 0x200 != 0 { c.u32()? } else { def_size };
                let flags = if tf & 0x400 != 0 { c.u32()? } else { def_flags };
                let flags = if i == 0 {
                    first_flags.unwrap_or(flags)
                } else {
                    flags
                };
                let cto = if tf & 0x800 != 0 {
                    let raw = c.u32()?;
                    if v == 0 {
                        raw as i64
                    } else {
                        raw as i32 as i64
                    }
                } else {
                    0
                };
                let dts = next_dts[ti];
                t.samples.push(Sample {
                    offset: data_pos,
                    size,
                    dts,
                    pts: dts.saturating_add(cto),
                    duration: dur,
                    keyframe: flags & 0x1_0000 == 0,
                    entry: sdi.saturating_sub(1),
                });
                data_pos = data_pos.saturating_add(size as u64);
                next_dts[ti] = dts.saturating_add(dur as i64);
            }
        }
        prev_end = Some(data_pos);
    }
    Ok(())
}
