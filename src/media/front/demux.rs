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
    let (sar, mut color) = look.unwrap_or(((1, 1), crate::gpu::Color::UNKNOWN));
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
                let s = r.full().ok()?.tracks.get(track)?.samples.get(i)?;
                Some((s.pts, s.pts, s.keyframe))
            }
        }
    }

    pub fn samples(&self, track: usize) -> usize {
        match self {
            Demux::Mp4(r) => r.tracks.get(track).map_or(0, |t| t.samples.len()),
            Demux::Mkv(r) => r
                .full()
                .ok()
                .and_then(|r| r.tracks.get(track))
                .map_or(0, |t| t.samples.len()),
        }
    }

    pub fn read_into(&self, track: usize, i: usize, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            Demux::Mp4(r) => r.read_into(track, i, buf),
            Demux::Mkv(r) => r.full()?.read_into(track, i, buf),
        }
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
                let at = t.sample_at(t.ticks(from)).unwrap_or(0);
                (t.keyframe_before(at), t.presentation(at).unwrap_or(shift))
            }
            Demux::Mkv(r) => {
                let t = mkv_track(r.full()?, track)?;
                let at = t.sample_at(t.ticks(from)).unwrap_or(0);
                (
                    t.keyframe_before(at),
                    t.samples.get(at).map_or(shift, |s| s.pts),
                )
            }
        };
        Ok((key, pts.saturating_sub(shift)))
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
                let color = match vpcc {
                    Some(c) => vpcc_color(c),
                    None => codec.and_then(|c| color_of(c, &extradata)),
                };
                #[cfg(feature = "gpu")]
                let look = match vpcc {
                    Some(c) => vpcc_look(c),
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
                let tagged = t
                    .video
                    .as_ref()
                    .and_then(|v| v.colour.as_ref())
                    .and_then(|c| Some((u8::try_from(c.matrix?).ok()?, c.full_range())))
                    .filter(|(m, _)| *m != 2);
                let color = tagged.or_else(|| codec.and_then(|c| color_of(c, &extradata)));
                #[cfg(feature = "gpu")]
                let look = tagged_look(
                    codec.and_then(|c| crate::gpu::look(c, &extradata)),
                    t.video.as_ref().and_then(|v| v.colour.as_ref()),
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
    let fps = match t.default_duration.filter(|d| *d > 0) {
        Some(ns) => 1e9 / ns as f64,
        None if duration > 0.0 => t.samples.len() as f64 / duration,
        None => 0.0,
    };
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
                channels: a.channels() as u16,
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
}
