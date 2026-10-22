use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use rayon::prelude::*;

use crate::mp4::{CodecConfig, Reader, SampleEntry, Track};

use super::demux::{self, Demux};
use super::feed::Feed;
use super::probe::{AudioTrack, Probe};
use super::sound::Sound;
use crate::media::{AudioBuffer, Codec, Direction, Error, Result};

const NO_GPU: &str = if cfg!(target_os = "macos") {
    "no VideoToolbox hardware decoder or encoder"
} else if cfg!(target_os = "windows") {
    "no Media Foundation hardware decoder or NVENC encoder"
} else {
    "no VA-API device or driver"
};

#[cfg(feature = "gpu")]
pub fn gpu() -> (Vec<(Codec, Direction)>, Option<String>) {
    static GPU: OnceLock<Vec<(Codec, Direction)>> = OnceLock::new();
    let found = GPU
        .get_or_init(|| {
            crate::gpu::available()
                .into_iter()
                .map(|(c, d, _)| {
                    (
                        c,
                        match d {
                            crate::gpu::Direction::Decode => Direction::Decode,
                            crate::gpu::Direction::Encode => Direction::Encode,
                        },
                    )
                })
                .collect()
        })
        .clone();
    let why = found.is_empty().then(|| NO_GPU.to_string());
    (found, why)
}

#[cfg(not(feature = "gpu"))]
pub fn gpu() -> (Vec<(Codec, Direction)>, Option<String>) {
    (Vec::new(), Some(NO_GPU.to_string()))
}

pub fn cpu() -> Vec<(Codec, Direction)> {
    let mut caps = vec![
        (Codec::H264, Direction::Decode),
        (Codec::H264, Direction::Encode),
        (Codec::Hevc, Direction::Decode),
        (Codec::Vp9, Direction::Decode),
        (Codec::Vp9, Direction::Encode),
        (Codec::Aac, Direction::Decode),
        (Codec::Aac, Direction::Encode),
    ];
    if cfg!(feature = "av1") {
        caps.push((Codec::Av1, Direction::Decode));
    }
    if cfg!(feature = "mp3") {
        caps.push((Codec::Mp3, Direction::Decode));
    }
    if cfg!(feature = "ac3") {
        caps.push((Codec::Ac3, Direction::Decode));
        caps.push((Codec::Eac3, Direction::Decode));
    }
    caps
}

pub(super) type Key = (PathBuf, u64, Option<SystemTime>);

pub(super) fn key(path: &Path) -> Result<Key> {
    let meta = std::fs::metadata(path).map_err(|e| Error::at(path, e))?;
    Ok((path.to_path_buf(), meta.len(), meta.modified().ok()))
}

pub(super) type Readers<R> = LazyLock<Mutex<HashMap<Key, Arc<OnceLock<Option<Arc<R>>>>>>>;

pub(super) fn cached<R>(
    readers: &Readers<R>,
    path: &Path,
    open: impl FnOnce(&Path) -> Result<R>,
) -> Result<Arc<R>> {
    let key = key(path)?;
    let slot = {
        let mut map = readers.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= 64 && !map.contains_key(&key) {
            map.clear();
        }
        map.entry(key.clone()).or_default().clone()
    };
    let mut failed = None;
    let got = slot.get_or_init(|| match open(path) {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            failed = Some(e);
            None
        }
    });
    if let Some(r) = got {
        return Ok(r.clone());
    }
    let mut map = readers.lock().unwrap_or_else(PoisonError::into_inner);
    if map.get(&key).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
        map.remove(&key);
    }
    drop(map);
    match failed {
        Some(e) => Err(e),
        None => Err(Error::Invalid(format!(
            "{} could not be opened",
            path.display()
        ))),
    }
}

pub fn reader(path: &Path) -> Result<Arc<Reader>> {
    static READERS: Readers<Reader> = LazyLock::new(Default::default);
    cached(&READERS, path, |p| Reader::open(p))
}

pub fn rational(fps: f64) -> (u32, u32) {
    if !fps.is_finite() || fps <= 0.0 {
        return (30, 1);
    }
    let fps = fps.clamp(0.001, 1000.0);
    if fps.round() >= 1.0 && (fps - fps.round()).abs() < 0.01 {
        return (fps.round() as u32, 1);
    }
    let ntsc = fps * 1.001;
    if ntsc.round() >= 1.0 && (ntsc - ntsc.round()).abs() < 0.01 {
        return ((ntsc.round() as u32).saturating_mul(1000), 1001);
    }
    (((fps * 1000.0).round() as u32).max(1), 1000)
}

pub(super) fn video_track(r: &Reader) -> Result<(usize, &Track)> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    Ok((v, &r.tracks[v]))
}

pub(super) fn gop_start(t: &Track, seconds: f64) -> usize {
    t.keyframe_before(t.sample_at(t.ticks(seconds)).unwrap_or(0))
}

pub(super) fn sorted(mut keys: Vec<f64>) -> Vec<f64> {
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    keys
}

const GENERIC: [&str; 6] = [
    "handler",
    "gpac",
    "l-smash",
    "core media",
    "apple",
    "mainconcept",
];

pub fn probe(path: &Path) -> Result<Probe> {
    if let Some(kind) = super::bare::sniff(path) {
        return super::bare::probe(path, kind);
    }
    if let Ok(Some(head)) = demux::mkv_head(path)
        && let Ok(Some(p)) = demux::probe_head(&head)
    {
        return Ok(p);
    }
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::probe(&r),
    };
    let starts = Demux::Mp4(r.clone()).starts();
    let audio = r
        .audio()
        .into_iter()
        .enumerate()
        .map(|(index, i)| {
            let a = &r.tracks[i];
            let name = a.name.trim();
            let lower = name.to_lowercase();
            let (codec, rate, refused) = super::probe::mp4_audio_codec(a);
            AudioTrack {
                index,
                stream: i,
                title: (!name.is_empty() && !GENERIC.iter().any(|g| lower.contains(g)))
                    .then(|| name.to_string()),
                language: (!a.language.is_empty() && a.language != "und")
                    .then(|| a.language.clone()),
                codec,
                channels: a.channels() as u16,
                rate,
                bitrate: Some(a.bitrate()).filter(|b| *b > 0),
                default: a.enabled,
                start: starts.track(i),
                bits: a
                    .entry()
                    .filter(|e| e.codec.name() == "flac")
                    .and_then(|e| super::sound::child(e, b"dfLa"))
                    .map_or(0, super::probe::flac_bits),
                refused,
            }
        })
        .collect();
    let Ok((v, t)) = video_track(&r) else {
        let duration = r
            .audio()
            .into_iter()
            .map(|i| r.tracks[i].duration_seconds())
            .fold(0.0f64, f64::max);
        return audio_only(audio, duration, starts.file);
    };
    let (width, height) = t
        .entry()
        .and_then(|e| e.video)
        .map(|v| (v.width as u32, v.height as u32))
        .unwrap_or((t.width, t.height));
    let (profile, bit_depth) = match t.config() {
        Some(CodecConfig::Avc(c)) => demux::avc_profile(c.profile),
        Some(CodecConfig::Hevc(c)) => demux::hevc_profile(c),
        Some(CodecConfig::Vp9(c)) => (format!("Profile {}", c.profile), c.bit_depth.max(8)),
        Some(CodecConfig::Av1(c)) => (c.profile_name(), c.bit_depth()),
        _ => (String::new(), 8),
    };
    let config = t.extradata().unwrap_or_default();
    let keyframe = super::variant::needs_keyframe(t.codec(), &config)
        .then(|| {
            let k = t.keyframes().first().copied().unwrap_or(0);
            r.read(v, k as usize).ok()
        })
        .flatten();
    let shape = super::variant::of(t.codec(), &config, keyframe.as_deref()).unwrap_or_default();
    let lead = starts.track(v).map_or(0.0, |s| (s - starts.file).max(0.0));
    Ok(Probe {
        duration: lead + t.duration_seconds(),
        start: starts.file,
        video_start: starts.track(v),
        fps: rational(t.fps()),
        width,
        height,
        rotation: t.rotation(),
        video_codec: t.codec(),
        video_codec_name: t.entry().map(video_name).unwrap_or_default(),
        profile,
        bit_depth: if shape.bit_depth > 0 {
            shape.bit_depth
        } else {
            bit_depth
        },
        chroma: shape.chroma,
        interlaced: shape.interlaced,
        tools: shape.tools,
        frames: Some(t.samples.len() as u64),
        audio,
        alpha: false,
    })
}

fn video_name(e: &SampleEntry) -> String {
    match (&e.format.0, e.object_type()) {
        (b"mp4v", Some(0x60..=0x65)) => "mpeg2video".into(),
        (b"mp4v", Some(0x6a)) => "mpeg1video".into(),
        (b"mp4v", Some(0x6c)) => "mjpeg".into(),
        _ => e.codec.name(),
    }
}

pub(super) fn audio_only(audio: Vec<AudioTrack>, duration: f64, start: f64) -> Result<Probe> {
    if audio.is_empty() {
        return Err(Error::Invalid("no video or audio track".to_string()));
    }
    Ok(Probe {
        duration,
        start,
        video_start: None,
        fps: (0, 1),
        width: 0,
        height: 0,
        rotation: 0,
        video_codec: None,
        video_codec_name: String::new(),
        profile: String::new(),
        bit_depth: 0,
        chroma: None,
        interlaced: false,
        tools: None,
        frames: None,
        audio,
        alpha: false,
    })
}

pub fn keyframes(path: &Path) -> Result<Vec<f64>> {
    if let Some(head) = demux::mkv_head(path)? {
        return demux::keyframes(&head);
    }
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::keyframes(&r),
    };
    let start = Demux::Mp4(r.clone()).start();
    let (_, t) = video_track(&r)?;
    Ok(sorted(
        t.keyframes()
            .iter()
            .filter_map(|&k| t.presentation(k as usize))
            .map(|p| (t.seconds(p) - start).max(0.0))
            .collect(),
    ))
}

pub const PAD: f64 = 0.02;
const CHUNK: f64 = 4.0;

fn audio_tracks(d: &Demux) -> Vec<usize> {
    match d {
        Demux::Mp4(r) => r.audio(),
        Demux::Mkv(r) => r.audio(),
    }
}

fn pcm_span(t: &crate::mkv::Track, sound: &Sound) -> Option<(f64, u64)> {
    let first = t.samples.first()?;
    let mut frames = 0u64;
    for s in &t.samples {
        frames = frames.saturating_add(sound.pcm_frames(s.size)?);
    }
    Some((t.seconds(first.pts), frames))
}

fn audio_length(d: &Demux, i: usize, sound: &Sound) -> Result<f64> {
    let starts = d.starts();
    let base = starts.file;
    Ok(match d {
        Demux::Mp4(r) => r.tracks.get(i).map_or(0.0, |t| {
            starts.track(i).map_or(0.0, |s| (s - base).max(0.0)) + t.duration_seconds()
        }),
        Demux::Mkv(r) => match r.tracks.get(i) {
            Some(t) if t.samples.is_empty() => match super::feed::mkv_end(r, i)? {
                e if e > 0.0 => (e - base).max(0.0),
                _ => 0.0,
            },
            Some(t) => match pcm_span(t, sound) {
                Some((start, frames)) => {
                    (start + frames as f64 / sound.rate().max(1) as f64 - base).max(0.0)
                }
                None => (t.seconds(t.end()) - base).max(0.0),
            },
            None => 0.0,
        },
    })
}

pub fn audio_demux(path: &Path) -> Result<Demux> {
    match demux::mkv_head(path)? {
        Some(r) => Ok(Demux::Mkv(r)),
        None => Demux::open(path),
    }
}

pub fn audio_range(
    path: &Path,
    track: usize,
    from: f64,
    to: f64,
    rate: u32,
    channels: u16,
) -> Result<AudioBuffer> {
    if let Some(kind) = super::bare::sniff(path) {
        return bare_range(path, kind, track, from, to, rate, channels);
    }
    let d = audio_demux(path)?;
    let i = audio_track(&d, track)?;
    let sound = Sound::open(&d, i)?;
    let pad = sound.pre_roll().max(PAD);
    let at = |t: f64| (t * rate as f64).round() as usize;
    let start = at(from.max(0.0));
    let whole = matches!(&d, Demux::Mkv(_)) && sound.pcm_frames(0).is_some();
    let end = if to.is_finite() {
        Some(at(to))
    } else if whole {
        None
    } else {
        let length = audio_length(&d, i, &sound)?;
        (length > 0.0).then(|| at(length).max(start))
    };
    let Some(end) = end.filter(|_| !whole) else {
        return piece(&d, i, start, end, rate, channels, pad);
    };
    let chunk = at(CHUNK).max(1);
    let n = (end - start).div_ceil(chunk).max(1);
    if n == 1 {
        return piece(&d, i, start, Some(end), rate, channels, pad);
    }
    let pieces = (0..n)
        .into_par_iter()
        .map(|k| {
            let s = start + k * chunk;
            piece(&d, i, s, Some((s + chunk).min(end)), rate, channels, pad)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out = AudioBuffer::new(rate, channels);
    out.samples.reserve((end - start) * channels as usize);
    for p in pieces {
        out.samples.extend_from_slice(&p.samples);
    }
    Ok(out)
}

fn bare_range(
    path: &Path,
    kind: super::bare::Bare,
    track: usize,
    from: f64,
    to: f64,
    rate: u32,
    channels: u16,
) -> Result<AudioBuffer> {
    let mut feed = super::bare::BareFeed::open(path, kind, track, from)?;
    let feed_rate = feed.rate as f64;
    let at = |t: f64| (t * feed_rate).round() as usize;
    let want = to
        .is_finite()
        .then(|| at(to).saturating_sub(at(from.max(0.0))));
    let mut raw = AudioBuffer::new(feed.rate, feed.channels);
    while want.is_none_or(|w| raw.frames() < w) && feed.next(&mut raw.samples)? {}
    if let Some(w) = want {
        raw.samples.truncate(w * raw.channels as usize);
    }
    if channels > raw.channels {
        shape(at_rate(raw, rate), channels)
    } else {
        Ok(at_rate(shape(raw, channels)?, rate))
    }
}

fn piece(
    d: &Demux,
    i: usize,
    start: usize,
    end: Option<usize>,
    rate: u32,
    channels: u16,
    pad: f64,
) -> Result<AudioBuffer> {
    let pad = start.min((pad * rate as f64).round() as usize);
    let t0 = (start - pad) as f64 / rate as f64;
    let t1 = end.map_or(f64::INFINITY, |e| e as f64 / rate as f64 + PAD);
    let (raw, length) = decode_track(d, i, t0, t1)?;
    let last = (length > 0.0).then(|| ((length * rate as f64).round() as usize).max(start));
    let end = match (end, last) {
        (Some(e), Some(l)) => Some(e.min(l)),
        (e, l) => e.or(l),
    };
    let buf = if channels > raw.channels {
        shape(at_rate(raw, rate), channels)?
    } else {
        at_rate(shape(raw, channels)?, rate)
    };
    Ok(cut(buf, pad, end.map(|e| e - start)))
}

pub fn audio_track(d: &Demux, track: usize) -> Result<usize> {
    audio_tracks(d)
        .get(track)
        .copied()
        .ok_or_else(|| Error::Invalid(format!("no audio track {track}")))
}

pub(super) fn at_rate(buf: AudioBuffer, rate: u32) -> AudioBuffer {
    if buf.rate == rate {
        buf
    } else {
        crate::aac::resample(&buf, rate)
    }
}

pub fn shape(buf: AudioBuffer, channels: u16) -> Result<AudioBuffer> {
    Ok(match channels {
        c if c == buf.channels => buf,
        1 => crate::aac::mono(&buf),
        2 => crate::aac::downmix_stereo(&buf),
        c => {
            return Err(Error::Invalid(format!(
                "{c} channels from a {}-channel track: ask for 1, 2 or {}",
                buf.channels, buf.channels
            )));
        }
    })
}

pub fn cut(mut buf: AudioBuffer, skip: usize, want: Option<usize>) -> AudioBuffer {
    let ch = buf.channels.max(1) as usize;
    buf.samples.drain(..(skip * ch).min(buf.samples.len()));
    if let Some(w) = want {
        buf.samples.resize(w * ch, 0.0);
    }
    buf
}

pub fn on_grid(start: f64, origin: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return start;
    }
    let snapped = origin + ((start - origin) / step).round() * step;
    if (snapped - start).abs() <= (step * 0.45).min(0.0011) {
        snapped
    } else {
        start
    }
}

fn decode_track(d: &Demux, i: usize, from: f64, to: f64) -> Result<(AudioBuffer, f64)> {
    let mut feed = Feed::open(d, i, from)?;
    let mut out = AudioBuffer::new(feed.rate, feed.channels);
    loop {
        if let Some(w) = feed.want(to)
            && out.frames() >= w
        {
            break;
        }
        if !feed.next(&mut out.samples)? {
            break;
        }
    }
    if let Some(w) = feed.want(to) {
        out.samples.resize(w * feed.channels as usize, 0.0);
    }
    Ok((out, feed.end()))
}
