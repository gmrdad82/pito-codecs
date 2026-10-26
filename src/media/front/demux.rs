use std::any::Any;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};

use crate::mkv;
use crate::mp4::{
    Av1Config, AvcConfig, CodecConfig, HevcConfig, Reader, Track, TrackKind, VpcConfig,
};

use super::base::{self as native, Readers, cached, rational, sorted};
use super::probe::{AudioTrack, Probe};
use crate::media::{Codec, Error, Result, TimeBase};

#[derive(Clone)]
pub enum Demux {
    Mp4(Arc<Reader>),
    Mkv(Arc<mkv::Reader>),
}

pub struct Stream {
    pub codec: Option<Codec>,
    pub name: String,
    pub extradata: Vec<u8>,
    pub avc: Option<AvcConfig>,
    pub time_base: TimeBase,
    pub color: Option<(u8, bool)>,
    #[cfg(feature = "gpu")]
    pub look: Option<((u16, u16), crate::gpu::Color)>,
}

#[cfg(feature = "gpu")]
fn tagged_look(
    look: Option<((u16, u16), crate::gpu::Color)>,
    tags: Option<&mkv::Colour>,
) -> Option<((u16, u16), crate::gpu::Color)> {
    let Some(c) = tags else {
        return look;
    };
    let (sar, mut color) = look.unwrap_or(((0, 0), crate::gpu::Color::UNKNOWN));
    let tag = |v: Option<u64>| v.and_then(|v| u8::try_from(v).ok()).filter(|&v| v != 2);
    if let Some(m) = tag(c.matrix) {
        color.matrix = m;
    }
    if let Some(p) = tag(c.primaries) {
        color.primaries = p;
    }
    if let Some(t) = tag(c.transfer) {
        color.transfer = t;
    }
    if matches!(c.range, Some(1 | 2)) {
        color.full_range = c.full_range();
    }
    Some((sar, color))
}

const KEY_LOOK: usize = 8;

static MKV_HEADS: Readers<mkv::Reader> = LazyLock::new(Default::default);

fn mkv_reader(path: &Path) -> Result<Arc<mkv::Reader>> {
    cached(&MKV_HEADS, path, |p| {
        mkv::Reader::open_with(p, mkv::ReadOptions { index: false })
    })
}

pub fn mkv_head(path: &Path) -> Result<Option<Arc<mkv::Reader>>> {
    if !is_matroska(path) {
        return Ok(None);
    }
    mkv_reader(path).map(Some)
}

fn is_matroska(path: &Path) -> bool {
    head(path, 4).starts_with(&MATROSKA)
}

const MATROSKA: [u8; 4] = [0x1a, 0x45, 0xdf, 0xa3];

const ASF: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];

const NOT_OURS: &str = "this file is not an MP4, MOV, MKV or WebM file";

fn head(path: &Path, len: u64) -> Vec<u8> {
    let mut head = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(len).read_to_end(&mut head);
    }
    head
}

fn foreign(head: &[u8], len: u64) -> Option<&'static str> {
    let at = |i: usize, magic: &[u8]| head.get(i..i + magic.len()) == Some(magic);
    let synced =
        |first: usize, step: usize| (0..3).all(|k| head.get(first + k * step) == Some(&0x47));
    if at(0, b"RIFF") && at(8, b"AVI ") {
        Some("AVI")
    } else if (synced(0, 188) || synced(4, 192)) && !boxed(head, len) {
        Some("MPEG-TS")
    } else if at(0, b"FLV\x01") {
        Some("FLV")
    } else if at(0, &[0, 0, 1, 0xba]) && !boxed(head, len) {
        Some("MPEG-PS")
    } else if at(0, &ASF) {
        Some("ASF (WMV or WMA)")
    } else {
        None
    }
}

fn boxed(head: &[u8], len: u64) -> bool {
    let size = head
        .get(..4)
        .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as u64);
    let sized = size <= 1 || (8..=len).contains(&size);
    sized
        && head
            .get(4..8)
            .is_some_and(|t| t.iter().all(|b| b.is_ascii_graphic() || *b == b' '))
}

fn readable(head: &[u8], len: u64) -> Result<()> {
    if let Some(name) = foreign(head, len) {
        return Err(Error::Unsupported(format!(
            "{name} files are not supported"
        )));
    }
    if head.len() >= 8 && !head.starts_with(&MATROSKA) && !boxed(head, len) {
        return Err(Error::Unsupported(NOT_OURS.to_string()));
    }
    Ok(())
}

fn mp4_track(r: &Reader, i: usize) -> Result<&Track> {
    r.tracks
        .get(i)
        .ok_or_else(|| Error::Invalid(format!("no track {i}")))
}

fn mkv_track(r: &mkv::Reader, i: usize) -> Result<&mkv::Track> {
    r.tracks
        .get(i)
        .ok_or_else(|| Error::Invalid(format!("no track {i}")))
}

fn mkv_video(r: &mkv::Reader) -> Result<(usize, &mkv::Track)> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    Ok((v, mkv_track(r, v)?))
}

#[derive(Debug, Clone, Default)]
pub struct Starts {
    pub file: f64,
    pub tracks: Vec<Option<f64>>,
    shift: Vec<i64>,
}

impl Starts {
    pub fn track(&self, i: usize) -> Option<f64> {
        self.tracks.get(i).copied().flatten()
    }

    pub fn shift(&self, i: usize) -> i64 {
        self.shift.get(i).copied().unwrap_or(0)
    }

    fn new(firsts: Vec<Option<f64>>, ticks: impl Fn(usize, f64) -> i64) -> Starts {
        let file = firsts
            .iter()
            .flatten()
            .copied()
            .filter(|s| s.is_finite())
            .fold(f64::INFINITY, f64::min);
        let file = if file.is_finite() { file.max(0.0) } else { 0.0 };
        let shift = (0..firsts.len())
            .map(|i| if file > 0.0 { ticks(i, file) } else { 0 })
            .collect();
        Starts {
            file,
            tracks: firsts,
            shift,
        }
    }
}

const START_BLOCKS: usize = 16;
const START_SCAN: usize = 4096;
const START_SPAN: f64 = 5.0;

fn mp4_starts(r: &Reader) -> Starts {
    let firsts = r
        .tracks
        .iter()
        .map(|t| {
            matches!(t.kind, TrackKind::Video | TrackKind::Audio)
                .then(|| t.sample_at(i64::MIN).and_then(|k| t.presentation(k)))
                .flatten()
                .map(|p| t.seconds(p))
        })
        .collect();
    Starts::new(firsts, |i, s| r.tracks.get(i).map_or(0, |t| t.ticks(s)))
}

fn mkv_starts(r: &mkv::Reader) -> Starts {
    let wanted: Vec<bool> = r
        .tracks
        .iter()
        .map(|t| matches!(t.kind, mkv::TrackKind::Video | mkv::TrackKind::Audio))
        .collect();
    let mut first: Vec<Option<i64>> = vec![None; r.tracks.len()];
    let mut seen = vec![0usize; r.tracks.len()];
    let mut earliest = f64::INFINITY;
    let mut stream = r.stream();
    for _ in 0..START_SCAN {
        let Ok(Some(b)) = stream.next_block(None) else {
            break;
        };
        let (Some(true), Some(t)) = (wanted.get(b.track).copied(), r.tracks.get(b.track)) else {
            continue;
        };
        let at = t.seconds(b.pts);
        if at > earliest + START_SPAN {
            break;
        }
        earliest = earliest.min(at);
        if seen[b.track] < START_BLOCKS {
            seen[b.track] += 1;
            first[b.track] = Some(first[b.track].map_or(b.pts, |f| f.min(b.pts)));
        }
        if wanted
            .iter()
            .zip(&seen)
            .all(|(w, n)| !*w || *n >= START_BLOCKS)
        {
            break;
        }
    }
    let firsts = first
        .iter()
        .zip(&r.tracks)
        .map(|(f, t)| f.map(|p| t.seconds(p)))
        .collect();
    Starts::new(firsts, |i, s| r.tracks.get(i).map_or(0, |t| t.ticks(s)))
}

type Held = (Weak<dyn Any + Send + Sync>, Arc<Starts>);

fn remembered(
    at: usize,
    make: impl FnOnce() -> (Weak<dyn Any + Send + Sync>, Starts),
) -> Arc<Starts> {
    static STARTS: LazyLock<Mutex<HashMap<usize, Held>>> = LazyLock::new(Default::default);
    if let Some((w, s)) = STARTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&at)
        && w.strong_count() > 0
    {
        return s.clone();
    }
    let (w, s) = make();
    let s = Arc::new(s);
    let mut map = STARTS.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= 256 {
        map.retain(|_, (w, _)| w.strong_count() > 0);
    }
    map.insert(at, (w, s.clone()));
    s
}

impl Demux {
    pub fn starts(&self) -> Arc<Starts> {
        match self {
            Demux::Mp4(r) => remembered(Arc::as_ptr(r) as usize, || {
                let any: Arc<dyn Any + Send + Sync> = r.clone();
                (Arc::downgrade(&any), mp4_starts(r))
            }),
            Demux::Mkv(r) => remembered(Arc::as_ptr(r) as usize, || {
                let any: Arc<dyn Any + Send + Sync> = r.clone();
                (Arc::downgrade(&any), mkv_starts(r))
            }),
        }
    }

    pub fn start(&self) -> f64 {
        self.starts().file
    }

    pub fn shift(&self, track: usize) -> i64 {
        self.starts().shift(track)
    }

    pub fn open(path: &Path) -> Result<Demux> {
        let head = head(path, 512);
        let len = std::fs::metadata(path).map_or(u64::MAX, |m| m.len());
        readable(&head, len)?;
        if head.starts_with(&MATROSKA) {
            mkv_reader(path).map(Demux::Mkv)
        } else {
            native::reader(path).map(Demux::Mp4)
        }
    }

    pub fn mp4(&self) -> Option<&Arc<Reader>> {
        match self {
            Demux::Mp4(r) => Some(r),
            Demux::Mkv(_) => None,
        }
    }

    pub fn video(&self) -> Result<usize> {
        match self {
            Demux::Mp4(r) => r.video(),
            Demux::Mkv(r) => r.video(),
        }
        .ok_or_else(|| Error::Invalid("no video track".to_string()))
    }

    pub fn sample(&self, track: usize, i: usize) -> Option<(i64, i64, bool)> {
        let (pts, dts, key) = self.raw_sample(track, i)?;
        let shift = self.shift(track);
        Some((pts.saturating_sub(shift), dts.saturating_sub(shift), key))
    }

    fn raw_sample(&self, track: usize, i: usize) -> Option<(i64, i64, bool)> {
        match self {
            Demux::Mp4(r) => {
                let t = r.tracks.get(track)?;
                let s = t.samples.get(i)?;
                Some((
                    s.pts.saturating_add(t.edit_offset),
                    s.dts.saturating_add(t.edit_offset),
                    s.keyframe,
                ))
            }
            Demux::Mkv(r) => {
                let s = r
                    .indexed_for(track)
                    .ok()?
                    .tracks
                    .get(track)?
                    .samples
                    .get(i)?;
                Some((s.pts, s.pts, s.keyframe))
            }
        }
    }

    pub fn samples(&self, track: usize) -> usize {
        match self {
            Demux::Mp4(r) => r.tracks.get(track).map_or(0, |t| t.samples.len()),
            Demux::Mkv(r) => r
                .indexed_for(track)
                .ok()
                .and_then(|r| r.tracks.get(track))
                .map_or(0, |t| t.samples.len()),
        }
    }

    pub fn read_into(&self, track: usize, i: usize, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            Demux::Mp4(r) => r.read_into(track, i, buf),
            Demux::Mkv(r) => r.indexed_for(track)?.read_into(track, i, buf),
        }
    }

    pub fn read_decodable(&self, track: usize, i: usize, buf: &mut Vec<u8>) -> Result<()> {
        self.read_into(track, i, buf)?;
        let Demux::Mp4(r) = self else {
            return Ok(());
        };
        let Some(t) = r.tracks.get(track).filter(|t| t.entries.len() > 1) else {
            return Ok(());
        };
        let Some(s) = t.samples.get(i).filter(|s| s.keyframe) else {
            return Ok(());
        };
        let own = t.entries.get(s.entry as usize).map(|e| &e.codec);
        let (n, sets): (u8, Vec<&[u8]>) = match (t.config(), own) {
            (Some(CodecConfig::Avc(first)), Some(CodecConfig::Avc(c))) => (
                first.length_size,
                c.sps.iter().chain(&c.pps).map(Vec::as_slice).collect(),
            ),
            (Some(CodecConfig::Hevc(first)), Some(CodecConfig::Hevc(c))) => (
                first.length_size,
                c.arrays
                    .iter()
                    .flat_map(|a| a.nalus.iter().map(Vec::as_slice))
                    .collect(),
            ),
            _ => return Ok(()),
        };
        let n = usize::from(n.clamp(1, 4));
        let mut head = Vec::new();
        for nal in sets {
            head.extend_from_slice(&(nal.len() as u64).to_be_bytes()[8 - n..]);
            head.extend_from_slice(nal);
        }
        buf.splice(0..0, head);
        Ok(())
    }

    pub fn seconds(&self, track: usize, ticks: i64) -> f64 {
        match self {
            Demux::Mp4(r) => r.tracks.get(track).map_or(0.0, |t| t.seconds(ticks)),
            Demux::Mkv(r) => r.tracks.get(track).map_or(0.0, |t| t.seconds(ticks)),
        }
    }

    pub fn seek(&self, track: usize, from: f64) -> Result<(usize, i64)> {
        let starts = self.starts();
        let from = from.max(0.0) + starts.file;
        let shift = starts.shift(track);
        let (key, pts) = match self {
            Demux::Mp4(r) => {
                let t = mp4_track(r, track)?;
                let at = t.sample_at(t.shown_at(from)).unwrap_or(0);
                (t.keyframe_before(at), t.presentation(at).unwrap_or(shift))
            }
            Demux::Mkv(r) => {
                let t = mkv_track(r.indexed_for(track)?, track)?;
                let at = t.sample_at(t.shown_at(from)).unwrap_or(0);
                (
                    t.keyframe_before(at),
                    t.samples.get(at).map_or(shift, |s| s.pts),
                )
            }
        };
        Ok((key, pts.saturating_sub(shift)))
    }

    fn keyframe_vpcc(&self, track: usize) -> Option<VpcConfig> {
        type Held = (Weak<dyn Any + Send + Sync>, Option<VpcConfig>);
        static KEYS: LazyLock<Mutex<HashMap<(usize, usize), Held>>> =
            LazyLock::new(Default::default);
        let (at, any): (usize, Arc<dyn Any + Send + Sync>) = match self {
            Demux::Mp4(r) => (Arc::as_ptr(r) as usize, r.clone()),
            Demux::Mkv(r) => (Arc::as_ptr(r) as usize, r.clone()),
        };
        if let Some((w, k)) = KEYS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(at, track))
            && w.strong_count() > 0
        {
            return *k;
        }
        let k = self.scan_keyframe(track);
        let mut map = KEYS.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= 256 {
            map.retain(|_, (w, _)| w.strong_count() > 0);
        }
        map.insert((at, track), (Arc::downgrade(&any), k));
        k
    }

    fn scan_keyframe(&self, track: usize) -> Option<VpcConfig> {
        let mut buf = Vec::new();
        match self {
            Demux::Mp4(_) => {
                for i in 0..self.samples(track).min(KEY_LOOK) {
                    self.read_into(track, i, &mut buf).ok()?;
                    if let Some(c) = keyframe_config(&buf) {
                        return Some(c);
                    }
                }
                None
            }
            Demux::Mkv(r) => {
                let mut blocks = r.stream();
                let mut seen = 0;
                for _ in 0..KEY_LOOK * r.tracks.len().max(1) {
                    let b = blocks.next_into(&mut buf).ok()??;
                    if b.track != track {
                        continue;
                    }
                    if let Some(c) = keyframe_config(&buf) {
                        return Some(c);
                    }
                    seen += 1;
                    if seen == KEY_LOOK {
                        break;
                    }
                }
                None
            }
        }
    }

    pub fn stream(&self, track: usize) -> Result<Stream> {
        match self {
            Demux::Mp4(r) => {
                let t = mp4_track(r, track)?;
                let codec = t.codec();
                let extradata = t.extradata().unwrap_or_default();
                let vpcc = match t.config() {
                    Some(CodecConfig::Vp9(c)) => Some(c),
                    _ => None,
                };
                let key = match codec {
                    Some(Codec::Vp9) if !vpcc.is_some_and(complete) => self.keyframe_vpcc(track),
                    _ => None,
                };
                let vp9 = match (vpcc, key) {
                    (Some(c), Some(k)) => Some(merged(c, &k)),
                    (c, k) => c.copied().or(k),
                };
                let color = match vp9 {
                    Some(c) => vpcc_color(&c),
                    None => codec.and_then(|c| color_of(c, &extradata)),
                };
                #[cfg(feature = "gpu")]
                let look = match vp9 {
                    Some(c) => vpcc_look(&c),
                    None => codec.and_then(|c| crate::gpu::look(c, &extradata)),
                };
                Ok(Stream {
                    codec,
                    name: t.config().map(|c| c.name()).unwrap_or_default(),
                    avc: match t.config() {
                        Some(CodecConfig::Avc(c)) => Some(c.clone()),
                        _ => None,
                    },
                    extradata,
                    time_base: TimeBase::new(1, t.timescale.max(1)),
                    color,
                    #[cfg(feature = "gpu")]
                    look,
                })
            }
            Demux::Mkv(r) => {
                let t = mkv_track(r, track)?;
                let codec = t.codec();
                let extradata = t.codec_private.clone();
                let avc = match codec {
                    Some(Codec::H264) => Some(AvcConfig::parse(&extradata)?),
                    _ => None,
                };
                let colour = t.video.as_ref().and_then(|v| v.colour.as_ref());
                let key = match codec {
                    Some(Codec::Vp9) if !colour.is_some_and(settled) => self.keyframe_vpcc(track),
                    _ => None,
                };
                let color = mkv_color(colour, key.as_ref())
                    .or_else(|| codec.and_then(|c| color_of(c, &extradata)));
                #[cfg(feature = "gpu")]
                let look = tagged_look(
                    key.as_ref()
                        .and_then(vpcc_look)
                        .or_else(|| codec.and_then(|c| crate::gpu::look(c, &extradata))),
                    colour,
                );
                Ok(Stream {
                    codec,
                    name: t.codec_name().to_string(),
                    extradata,
                    avc,
                    time_base: t.time_base,
                    color,
                    #[cfg(feature = "gpu")]
                    look,
                })
            }
        }
    }
}

pub fn avc_profile(profile: u8) -> (String, u8) {
    let name = match profile {
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4 Predictive",
        _ => "",
    };
    (name.to_string(), if profile >= 110 { 10 } else { 8 })
}

pub fn hevc_profile(c: &HevcConfig) -> (String, u8) {
    let name = match c.profile {
        1 => "Main",
        2 => "Main 10",
        3 => "Main Still Picture",
        4 => "Rext",
        _ => "",
    };
    (name.to_string(), c.bit_depth_luma.max(8))
}

const TAIL_SPAN: u64 = 16 << 20;
const MAX_TAIL_SPAN: u64 = 256 << 20;

fn video_end(stream: &mut mkv::Stream<'_, std::fs::File>, v: usize) -> Result<Option<i64>> {
    let mut end: Option<i64> = None;
    while let Some(b) = stream.next_block(Some(v))? {
        end = end.max(Some(b.pts.saturating_add(b.duration as i64)));
    }
    Ok(end)
}

fn variant_config(t: &mkv::Track) -> &[u8] {
    match t.codec() {
        Some(Codec::Vp9) => &[],
        _ => &t.codec_private,
    }
}

pub fn probe_head(r: &Arc<mkv::Reader>) -> Result<Option<Probe>> {
    let (v, t) = mkv_video(r)?;
    let Some(ns) = t.default_duration.filter(|d| *d > 0) else {
        return Ok(None);
    };
    let last = r.cues.iter().rev().find(|c| c.track == t.number);
    let wants = super::variant::needs_keyframe(t.codec(), variant_config(t));
    let mut stream = r.stream();
    let mut data = Vec::new();
    let mut first: Option<Vec<u8>> = None;
    let mut start = 0i64;
    let mut seen = 0;
    while seen < 16 {
        let Some(b) = stream.next_into(&mut data)? else {
            break;
        };
        if b.track != v {
            continue;
        }
        if first.is_none() && b.keyframe && wants {
            first = Some(data.clone());
        }
        start = start.min(b.pts);
        seen += 1;
    }
    if seen == 0 {
        return Ok(None);
    }
    let end = match last {
        Some(last) => {
            stream.seek(v, t.seconds(last.time as i64))?;
            video_end(&mut stream, v)?
        }
        None => {
            let mut span = TAIL_SPAN;
            loop {
                if stream.tail(span)?
                    && let Some(end) = video_end(&mut stream, v)?
                {
                    break Some(end);
                }
                if span >= MAX_TAIL_SPAN || span >= r.len() {
                    break None;
                }
                span *= 4;
            }
        }
    };
    let Some(end) = end else {
        return Ok(None);
    };
    let starts = Demux::Mkv(r.clone()).starts();
    let duration = (t.seconds(end.saturating_sub(start)) - starts.file).max(0.0);
    describe(r, t, (duration, 1e9 / ns as f64, None), first, &starts).map(Some)
}

pub fn probe(head: &Arc<mkv::Reader>) -> Result<Probe> {
    let starts = Demux::Mkv(head.clone()).starts();
    let r = head.full()?;
    if r.video().is_none() {
        let duration = (r.duration_seconds() - starts.file).max(0.0);
        return super::base::audio_only(mkv_audio(r, &starts), duration, starts.file);
    }
    let (v, t) = mkv_video(r)?;
    let duration = (t.duration_seconds() - starts.file).max(0.0);
    let fps = t.fps();
    let keyframe = super::variant::needs_keyframe(t.codec(), variant_config(t))
        .then(|| {
            t.samples
                .iter()
                .position(|s| s.keyframe)
                .and_then(|i| r.read(v, i).ok())
        })
        .flatten();
    let frames = Some(t.samples.len() as u64);
    describe(r, t, (duration, fps, frames), keyframe, &starts)
}

fn mkv_audio(r: &mkv::Reader, starts: &Starts) -> Vec<AudioTrack> {
    r.audio()
        .into_iter()
        .enumerate()
        .filter_map(|(index, i)| {
            let a = r.tracks.get(i)?;
            let name = a.name.trim();
            let (codec, rate, refused) = super::probe::mkv_audio_codec(a);
            Some(AudioTrack {
                index,
                stream: i,
                title: (!name.is_empty()).then(|| name.to_string()),
                language: (!a.language.is_empty() && a.language != "und")
                    .then(|| a.language.clone()),
                codec,
                channels: super::probe::mkv_channels(a),
                rate,
                bitrate: None,
                default: a.default,
                start: starts.track(i),
                bits: if a.codec_name() == "flac" {
                    super::probe::flac_bits(&a.codec_private)
                } else {
                    0
                },
                refused,
            })
        })
        .collect()
}

fn describe(
    r: &mkv::Reader,
    t: &mkv::Track,
    (duration, fps, frames): (f64, f64, Option<u64>),
    keyframe: Option<Vec<u8>>,
    starts: &Starts,
) -> Result<Probe> {
    let (width, height) = t
        .video
        .as_ref()
        .map(|i| (i.pixel_width, i.pixel_height))
        .unwrap_or((t.width, t.height));
    let (profile, bit_depth) = match t.codec() {
        Some(Codec::H264) => AvcConfig::parse(&t.codec_private)
            .map(|c| avc_profile(c.profile))
            .unwrap_or((String::new(), 8)),
        Some(Codec::Hevc) => HevcConfig::parse(&t.codec_private)
            .map(|c| hevc_profile(&c))
            .unwrap_or((String::new(), 8)),
        Some(Codec::Vp9) => keyframe
            .as_deref()
            .and_then(vp9_profile)
            .unwrap_or((String::new(), 8)),
        Some(Codec::Av1) => Av1Config::parse(&t.codec_private)
            .map(|c| (c.profile_name(), c.bit_depth()))
            .unwrap_or((String::new(), 8)),
        _ => (String::new(), 8),
    };
    let shape =
        super::variant::of(t.codec(), variant_config(t), keyframe.as_deref()).unwrap_or_default();
    let audio = mkv_audio(r, starts);
    Ok(Probe {
        duration,
        start: starts.file,
        video_start: r.video().and_then(|v| starts.track(v)),
        fps: rational(fps),
        width,
        height,
        rotation: t.rotation(),
        video_codec: t.codec(),
        video_codec_name: video_name(t),
        profile,
        bit_depth: if shape.bit_depth > 0 {
            shape.bit_depth
        } else {
            bit_depth
        },
        chroma: shape.chroma,
        interlaced: shape.interlaced,
        tools: shape.tools,
        frames,
        audio,
        alpha: t.alpha(),
    })
}

fn video_name(t: &mkv::Track) -> String {
    let vfw = t
        .codec_private
        .get(16..20)
        .filter(|f| t.codec_id == "V_MS/VFW/FOURCC" && f.iter().all(u8::is_ascii_graphic));
    match vfw {
        Some(f) => String::from_utf8_lossy(f).into_owned(),
        None => t.codec_name().to_string(),
    }
}

pub fn frame_count(path: &Path) -> Result<u64> {
    if let Some(r) = mkv_head(path)? {
        return Ok(mkv_video(r.full()?)?.1.samples.len() as u64);
    }
    match Demux::open(path)? {
        Demux::Mp4(r) => Ok(r
            .video()
            .and_then(|v| r.tracks.get(v))
            .map_or(0, |t| t.samples.len()) as u64),
        Demux::Mkv(r) => Ok(mkv_video(r.full()?)?.1.samples.len() as u64),
    }
}

pub fn keyframes(r: &Arc<mkv::Reader>) -> Result<Vec<f64>> {
    let start = Demux::Mkv(r.clone()).start();
    if let Some(keys) = r.cue_keys() {
        let (_, t) = mkv_video(r)?;
        return Ok(sorted(
            keys.iter()
                .map(|&k| (t.seconds(k) - start).max(0.0))
                .collect(),
        ));
    }
    let (_, t) = mkv_video(r.full()?)?;
    Ok(sorted(
        t.samples
            .iter()
            .filter(|s| s.keyframe)
            .map(|s| (t.seconds(s.pts) - start).max(0.0))
            .collect(),
    ))
}

#[cfg(feature = "gpu")]
fn color_of(codec: Codec, extradata: &[u8]) -> Option<(u8, bool)> {
    crate::gpu::color(codec, extradata)
}

fn tagged_color(tags: Option<&mkv::Colour>) -> Option<(u8, bool)> {
    tags.and_then(|c| Some((u8::try_from(c.matrix?).ok()?, c.full_range())))
        .filter(|(m, _)| *m != 2)
}

fn settled(c: &mkv::Colour) -> bool {
    let set = |v: Option<u64>| v.and_then(|v| u8::try_from(v).ok()).is_some_and(|v| v != 2);
    matches!(c.range, Some(1 | 2)) && set(c.matrix) && set(c.primaries) && set(c.transfer)
}

fn mkv_color(tags: Option<&mkv::Colour>, key: Option<&VpcConfig>) -> Option<(u8, bool)> {
    let Some(k) = key else {
        return tagged_color(tags);
    };
    let full_range = match tags {
        Some(c) if matches!(c.range, Some(1 | 2)) => c.full_range(),
        _ => k.full_range,
    };
    let matrix_coefficients = tagged_color(tags).map_or(k.matrix_coefficients, |(m, _)| m);
    vpcc_color(&VpcConfig {
        full_range,
        matrix_coefficients,
        ..*k
    })
}

fn complete(c: &VpcConfig) -> bool {
    [
        c.colour_primaries,
        c.transfer_characteristics,
        c.matrix_coefficients,
    ]
    .iter()
    .all(|&v| v != 2)
}

fn merged(c: &VpcConfig, k: &VpcConfig) -> VpcConfig {
    let pick = |v: u8, k: u8| if v != 2 { v } else { k };
    VpcConfig {
        colour_primaries: pick(c.colour_primaries, k.colour_primaries),
        transfer_characteristics: pick(c.transfer_characteristics, k.transfer_characteristics),
        matrix_coefficients: pick(c.matrix_coefficients, k.matrix_coefficients),
        full_range: if c.full_range || c.matrix_coefficients != 2 {
            c.full_range
        } else {
            k.full_range
        },
        ..*c
    }
}

fn vpcc_color(c: &VpcConfig) -> Option<(u8, bool)> {
    (c.matrix_coefficients != 2 || c.full_range).then_some((c.matrix_coefficients, c.full_range))
}

#[cfg(feature = "gpu")]
fn vpcc_look(c: &VpcConfig) -> Option<((u16, u16), crate::gpu::Color)> {
    let described = [
        c.colour_primaries,
        c.transfer_characteristics,
        c.matrix_coefficients,
    ]
    .iter()
    .any(|&v| v != 2);
    (described || c.full_range).then_some((
        (1, 1),
        crate::gpu::Color {
            primaries: c.colour_primaries,
            transfer: c.transfer_characteristics,
            matrix: c.matrix_coefficients,
            full_range: c.full_range,
        },
    ))
}

#[cfg(not(feature = "gpu"))]
fn color_of(_: Codec, _: &[u8]) -> Option<(u8, bool)> {
    None
}

#[cfg(feature = "vp9")]
fn keyframe_config(frame: &[u8]) -> Option<VpcConfig> {
    let k = crate::vp9::keyframe_info(frame)?;
    let chroma = match (k.subsampling_x, k.subsampling_y) {
        (true, false) => 2,
        (false, false) => 3,
        _ => 1,
    };
    Some(VpcConfig::new(
        (k.profile, k.bit_depth as u8, chroma),
        (k.color.color_space, k.color.full_range),
        (k.width, k.height),
        (30, 1),
    ))
}

#[cfg(not(feature = "vp9"))]
fn keyframe_config(_: &[u8]) -> Option<VpcConfig> {
    None
}

#[cfg(feature = "vp9")]
fn vp9_profile(frame: &[u8]) -> Option<(String, u8)> {
    crate::vp9::keyframe_info(frame)
        .map(|k| (format!("Profile {}", k.profile), k.bit_depth.max(8) as u8))
}

#[cfg(not(feature = "vp9"))]
fn vp9_profile(_: &[u8]) -> Option<(String, u8)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(head: &[u8]) -> String {
        readable(head, u64::MAX).unwrap_err().to_string()
    }

    fn ts(first: usize, step: usize) -> Vec<u8> {
        let mut head = vec![0u8; 512];
        for k in 0..3 {
            head[first + k * step] = 0x47;
        }
        head
    }

    #[test]
    fn containers_no_reader_takes_are_named_in_one_line() {
        let mut avi = b"RIFF\x24\x10\x00\x00AVI LIST".to_vec();
        avi.resize(64, 0);
        let mut asf = ASF.to_vec();
        asf.resize(64, 0);
        let cases: [(Vec<u8>, &str); 6] = [
            (avi, "AVI"),
            (ts(0, 188), "MPEG-TS"),
            (ts(4, 192), "MPEG-TS"),
            (b"FLV\x01\x05\x00\x00\x00\x09".to_vec(), "FLV"),
            (vec![0, 0, 1, 0xba, 0x44, 0, 4, 0, 4, 1], "MPEG-PS"),
            (asf, "ASF"),
        ];
        for (head, name) in cases {
            let found = foreign(&head, u64::MAX).unwrap_or_default();
            assert!(found.starts_with(name), "{found}");
            let e = refusal(&head);
            assert_eq!(e, format!("unsupported: {found} files are not supported"));
            assert!(!e.to_lowercase().contains("ffmpeg"), "{e}");
        }
    }

    #[test]
    fn a_file_that_is_no_box_stream_is_refused_plainly() {
        let mut ogg = b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        ogg.resize(64, 0);
        let mut wav = b"RIFF\x24\x10\x00\x00WAVEfmt ".to_vec();
        wav.resize(64, 0);
        for head in [
            ogg,
            wav,
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0x1e],
            vec![0xff; 64],
        ] {
            assert_eq!(foreign(&head, u64::MAX), None);
            assert_eq!(refusal(&head), format!("unsupported: {NOT_OURS}"));
        }
    }

    #[test]
    fn mp4_mov_and_matroska_heads_pass_and_short_files_reach_the_reader() {
        let heads: [&[u8]; 6] = [
            b"\x00\x00\x00\x20ftypisom\x00\x00\x02\x00",
            b"\x00\x00\x00\x08free\x00\x00\x00\x08mdat",
            b"\x00\x00\x00\x08wide\x00\x00\x00\x08mdat",
            b"\x00\x00\x00\x01mdat\x00\x00\x00\x00\x00\x00\x00\x10",
            &[0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81, 0x01],
            b"\x00\x00\x00",
        ];
        for head in heads {
            assert!(readable(head, u64::MAX).is_ok(), "{head:?}");
        }
        assert!(readable(&[], 0).is_ok());
        let ftyp = b"\x00\x00\x00\x20ftypisom\x00\x00\x02\x00";
        assert!(readable(ftyp, 32).is_ok());
        assert_eq!(
            readable(ftyp, 31).unwrap_err().to_string(),
            format!("unsupported: {NOT_OURS}")
        );
        assert_eq!(
            readable(b"\x00\x00\x00\x04ftypisom", u64::MAX)
                .unwrap_err()
                .to_string(),
            format!("unsupported: {NOT_OURS}")
        );
        let words = b"these words are not a movie";
        assert_eq!(
            readable(words, words.len() as u64).unwrap_err().to_string(),
            format!("unsupported: {NOT_OURS}")
        );
    }

    #[test]
    fn a_single_sync_byte_is_not_mpeg_ts() {
        let mut head = vec![0u8; 512];
        head[0] = 0x47;
        head[188] = 0x47;
        assert_eq!(foreign(&head, u64::MAX), None);
    }

    #[test]
    fn a_box_whose_size_reads_as_mpeg_ps_or_ts_magic_is_still_a_box() {
        let mut ps = vec![0, 0, 1, 0xba];
        ps.extend_from_slice(b"free");
        ps.resize(442, 0);
        ps.extend_from_slice(b"\x00\x00\x00\x08mdat");
        let len = ps.len() as u64;
        assert_eq!(foreign(&ps, len), None);
        assert!(readable(&ps, len).is_ok());
        assert_eq!(foreign(&ps[..100], 100), Some("MPEG-PS"));
        let pack = [0, 0, 1, 0xba, 0x44, 0, 4, 0, 4, 1];
        assert_eq!(foreign(&pack, u64::MAX), Some("MPEG-PS"));
        let mut ts = ts(0, 188);
        ts[4..8].copy_from_slice(b"skip");
        assert_eq!(foreign(&ts, u64::MAX), None);
        assert!(readable(&ts, u64::MAX).is_ok());
        assert_eq!(foreign(&ts, 1000), Some("MPEG-TS"));
    }

    #[cfg(all(feature = "vp9", feature = "gpu"))]
    mod vp9_colour {
        use super::*;
        use crate::media::{Frame, PixelFormat};
        use crate::mp4::SampleEntry;
        use std::path::PathBuf;

        const SIDE: u32 = 64;

        fn scratch(name: &str) -> PathBuf {
            let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/vp9color");
            std::fs::create_dir_all(&d).unwrap();
            d.join(format!("{}-{name}", std::process::id()))
        }

        fn keyframe(space: u8, full_range: bool) -> Vec<u8> {
            let mut enc = crate::vp9::Encoder::new(crate::vp9::EncoderConfig {
                width: SIDE,
                height: SIDE,
                gop: 1,
                threads: 1,
                ..Default::default()
            })
            .unwrap();
            enc.set_color(space, full_range).unwrap();
            let mut f = Frame::new(PixelFormat::Yuv420p8, SIDE, SIDE, 0, TimeBase::new(1, 30));
            for p in &mut f.planes {
                p.data.fill(128);
            }
            let mut out = enc.encode(&f).unwrap();
            out.extend(enc.flush().unwrap());
            out.into_iter().next().unwrap().data
        }

        fn mkv_of(name: &str, samples: &[(&[u8], bool)]) -> PathBuf {
            let path = scratch(name);
            let mut w = mkv::Writer::create(&path).unwrap();
            let t = w
                .add_track(mkv::TrackConfig::vp9(SIDE, SIDE, TimeBase::new(1, 1000)))
                .unwrap();
            for (i, (data, key)) in samples.iter().enumerate() {
                w.push_sample(t, data, i as i64 * 33, Some(33), *key)
                    .unwrap();
            }
            w.finish().unwrap();
            path
        }

        fn vpcc(space: u8, full_range: bool) -> VpcConfig {
            VpcConfig::new((0, 8, 1), (space, full_range), (SIDE, SIDE), (30, 1))
        }

        fn mp4_of(name: &str, space: (u8, bool), frame: &[u8]) -> PathBuf {
            mp4_with(name, vpcc(space.0, space.1), frame)
        }

        fn mp4_with(name: &str, config: VpcConfig, frame: &[u8]) -> PathBuf {
            let path = scratch(name);
            let mut w = crate::mp4::Writer::create(&path).unwrap();
            let t = w
                .add_track(crate::mp4::TrackConfig::new(
                    SampleEntry::vp9(config, SIDE as u16, SIDE as u16),
                    1000,
                ))
                .unwrap();
            w.push_sample(t, frame, 0, 0, 33, true).unwrap();
            w.finish().unwrap();
            path
        }

        fn colours(path: &Path) -> (Option<(u8, bool)>, Option<crate::gpu::Color>) {
            let d = Demux::open(path).unwrap();
            let s = d.stream(d.video().unwrap()).unwrap();
            let _ = std::fs::remove_file(path);
            (s.color, s.look.map(|(_, c)| c))
        }

        fn look(matrix: u8, primaries: u8, transfer: u8, full_range: bool) -> crate::gpu::Color {
            crate::gpu::Color {
                primaries,
                transfer,
                matrix,
                full_range,
            }
        }

        #[test]
        fn an_untagged_matroska_vp9_takes_the_keyframe_colour() {
            let bt709 = keyframe(2, false);
            let (color, l) = colours(&mkv_of("709.mkv", &[(&bt709, true)]));
            assert_eq!(color, Some((1, false)));
            assert_eq!(l, Some(look(1, 1, 1, false)));
            let bt601 = keyframe(1, true);
            let (color, l) = colours(&mkv_of("601.mkv", &[(&bt601, true)]));
            assert_eq!(color, Some((5, true)));
            assert_eq!(l, Some(look(5, 5, 6, true)));
        }

        #[test]
        fn an_unspecified_vpcc_falls_back_to_the_keyframe_colour() {
            let (color, l) = colours(&mp4_of("open.mp4", (0, false), &keyframe(2, true)));
            assert_eq!(color, Some((1, true)));
            assert_eq!(l, Some(look(1, 1, 1, true)));
        }

        #[test]
        fn a_vpcc_that_disagrees_with_the_keyframe_wins() {
            let (color, l) = colours(&mp4_of("wins.mp4", (1, false), &keyframe(2, true)));
            assert_eq!(color, Some((5, false)));
            assert_eq!(l, Some(look(5, 5, 6, false)));
        }

        #[test]
        fn unknown_and_reserved_keyframe_spaces_leave_the_size_default() {
            for space in [0, 6] {
                let frame = keyframe(space, false);
                let (color, l) = colours(&mkv_of(&format!("cs{space}.mkv"), &[(&frame, true)]));
                assert_eq!((color, l), (None, None), "color space {space}");
            }
            let frame = keyframe(6, true);
            let (color, _) = colours(&mkv_of("cs6-full.mkv", &[(&frame, true)]));
            assert_eq!(color, Some((2, true)));
        }

        #[test]
        fn a_stream_without_a_readable_keyframe_still_opens_untagged() {
            let junk = [0xffu8; 32];
            let (color, l) = colours(&mkv_of("junk.mkv", &[(&junk, true), (&junk, false)]));
            assert_eq!((color, l), (None, None));
            let late = keyframe(2, false);
            let (color, _) = colours(&mkv_of("late.mkv", &[(&junk, false), (&late, true)]));
            assert_eq!(color, Some((1, false)));
        }

        #[test]
        fn the_keyframe_is_looked_for_only_in_the_first_samples() {
            let junk = [0xffu8; 32];
            let late = keyframe(2, false);
            let mut samples: Vec<(&[u8], bool)> = vec![(&junk, false); KEY_LOOK];
            samples.push((&late, true));
            let (color, _) = colours(&mkv_of("too-late.mkv", &samples));
            assert_eq!(color, None);
        }

        #[test]
        fn a_matroska_range_tag_overrides_the_keyframe_range_only() {
            let key = VpcConfig::new((0, 8, 1), (2, false), (SIDE, SIDE), (30, 1));
            let full = mkv::Colour {
                range: Some(2),
                ..Default::default()
            };
            assert_eq!(mkv_color(Some(&full), Some(&key)), Some((1, true)));
            let open = mkv::Colour {
                range: Some(0),
                ..Default::default()
            };
            assert_eq!(mkv_color(Some(&open), Some(&key)), Some((1, false)));
            assert_eq!(mkv_color(None, Some(&key)), Some((1, false)));
        }

        #[test]
        fn a_range_only_vpcc_takes_the_matrix_from_the_keyframe() {
            let path = mp4_of("range.mp4", (0, true), &keyframe(2, false));
            let v = crate::media::front::look::video_look(&path).unwrap();
            assert_eq!(v.color, look(1, 1, 1, true));
            let (color, l) = colours(&path);
            assert_eq!(color, Some((1, true)));
            assert_eq!(l, Some(look(1, 1, 1, true)));
        }

        #[test]
        fn vpcc_primaries_and_transfer_survive_the_keyframe_matrix() {
            let pq = VpcConfig {
                colour_primaries: 9,
                transfer_characteristics: 16,
                ..vpcc(0, false)
            };
            let (color, l) = colours(&mp4_with("pq.mp4", pq, &keyframe(2, true)));
            assert_eq!(color, Some((1, true)));
            assert_eq!(l, Some(look(1, 9, 16, true)));
        }

        #[test]
        fn merged_fills_only_what_the_vpcc_leaves_unspecified() {
            let key = vpcc(1, true);
            assert_eq!(merged(&vpcc(2, false), &key), vpcc(2, false));
            let open = merged(&vpcc(0, false), &key);
            assert_eq!(open, key);
            let full = merged(&vpcc(0, true), &vpcc(2, false));
            assert_eq!((full.matrix_coefficients, full.full_range), (1, true));
            assert!(complete(&vpcc(2, false)));
            assert!(!complete(&vpcc(0, false)));
        }

        #[test]
        fn a_matroska_matrix_tag_wins_over_the_keyframe_on_both_routes() {
            let key = keyframe_config(&keyframe(2, true)).unwrap();
            let tags = mkv::Colour {
                matrix: Some(5),
                ..Default::default()
            };
            assert_eq!(mkv_color(Some(&tags), Some(&key)), Some((5, true)));
            assert_eq!(
                tagged_look(vpcc_look(&key), Some(&tags)),
                Some(((1, 1), look(5, 1, 1, true)))
            );
            let limited = mkv::Colour {
                matrix: Some(5),
                range: Some(1),
                ..Default::default()
            };
            assert_eq!(mkv_color(Some(&limited), Some(&key)), Some((5, false)));
            assert_eq!(
                tagged_look(vpcc_look(&key), Some(&limited)),
                Some(((1, 1), look(5, 1, 1, false)))
            );
            assert_eq!(mkv_color(Some(&limited), None), Some((5, false)));
        }

        #[test]
        fn fully_tagged_matroska_colour_skips_the_keyframe() {
            let all = mkv::Colour {
                matrix: Some(1),
                primaries: Some(1),
                transfer: Some(1),
                range: Some(1),
                ..Default::default()
            };
            assert!(settled(&all));
            for open in [
                mkv::Colour {
                    range: Some(0),
                    ..all.clone()
                },
                mkv::Colour {
                    range: None,
                    ..all.clone()
                },
                mkv::Colour {
                    matrix: Some(2),
                    ..all.clone()
                },
                mkv::Colour {
                    primaries: None,
                    ..all.clone()
                },
                mkv::Colour {
                    transfer: Some(300),
                    ..all.clone()
                },
            ] {
                assert!(!settled(&open), "{open:?}");
            }
            let key = keyframe_config(&keyframe(1, true)).unwrap();
            assert_eq!(
                mkv_color(Some(&all), Some(&key)),
                mkv_color(Some(&all), None)
            );
            assert_eq!(
                tagged_look(vpcc_look(&key), Some(&all)).map(|(_, c)| c),
                tagged_look(None, Some(&all)).map(|(_, c)| c)
            );
            assert_eq!(
                tagged_look(None, Some(&all)).map(|(sar, _)| sar),
                Some((0, 0))
            );
        }

        #[test]
        fn the_keyframe_colour_is_read_once_per_reader() {
            let path = mkv_of("once.mkv", &[(&keyframe(2, false), true)]);
            let d = Demux::open(&path).unwrap();
            let track = d.video().unwrap();
            assert_eq!(d.stream(track).unwrap().color, Some((1, false)));
            std::fs::write(&path, b"").unwrap();
            assert_eq!(d.stream(track).unwrap().color, Some((1, false)));
            let _ = std::fs::remove_file(&path);
        }

        fn rgb(profile: u8) -> Vec<u8> {
            let mut frame = keyframe(2, false);
            frame[0] |= (profile & 1) << 5;
            frame[4] = (frame[4] & 0x0f) | 0xe0;
            frame
        }

        #[test]
        fn rgb_keyframes_map_to_identity_full_range_or_are_refused() {
            assert_eq!(keyframe_config(&rgb(0)), None);
            let c = keyframe_config(&rgb(1)).unwrap();
            assert_eq!(
                (
                    c.profile,
                    c.chroma_subsampling,
                    c.matrix_coefficients,
                    c.full_range
                ),
                (1, 3, 0, true)
            );
            let (color, l) = colours(&mkv_of("rgb.mkv", &[(&rgb(1), true)]));
            assert_eq!(color, Some((0, true)));
            assert_eq!(l, Some(look(0, 1, 13, true)));
            let (color, l) = colours(&mkv_of("rgb0.mkv", &[(&rgb(0), true)]));
            assert_eq!((color, l), (None, None));
        }

        #[test]
        fn keyframe_config_maps_libvpx_spaces_and_refuses_junk() {
            assert_eq!(keyframe_config(&[0xff; 32]), None);
            assert_eq!(keyframe_config(&[]), None);
            let c = keyframe_config(&keyframe(5, false)).unwrap();
            assert_eq!(
                (
                    c.colour_primaries,
                    c.transfer_characteristics,
                    c.matrix_coefficients
                ),
                (9, 2, 9)
            );
            let c = keyframe_config(&keyframe(3, true)).unwrap();
            assert_eq!((c.matrix_coefficients, c.full_range), (6, true));
        }
    }
}
