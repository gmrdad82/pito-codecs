use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use crate::mkv;
use crate::mp4::{AvcConfig, CodecConfig, HevcConfig, Reader};

use super::native::{self, rational};
use super::probe::{AudioTrack, Probe};
use super::types::{Codec, Error, Result, TimeBase};

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
}

type Key = (PathBuf, u64, Option<SystemTime>);

fn mkv_readers() -> &'static Mutex<HashMap<Key, Arc<mkv::Reader>>> {
    static READERS: OnceLock<Mutex<HashMap<Key, Arc<mkv::Reader>>>> = OnceLock::new();
    READERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mkv_reader(path: &Path) -> Result<Arc<mkv::Reader>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::at(path, e))?;
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some(r) = mkv_readers()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Ok(r.clone());
    }
    let r = Arc::new(mkv::Reader::open(path)?);
    let mut map = mkv_readers().lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= 64 {
        map.clear();
    }
    map.insert(key, r.clone());
    Ok(r)
}

fn is_matroska(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == [0x1a, 0x45, 0xdf, 0xa3]
}

fn mkv_track(r: &mkv::Reader, i: usize) -> Result<&mkv::Track> {
    r.tracks
        .get(i)
        .ok_or_else(|| Error::Invalid(format!("no track {i}")))
}

impl Demux {
    pub fn open(path: &Path) -> Result<Demux> {
        if is_matroska(path) {
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
                let s = r.tracks.get(track)?.samples.get(i)?;
                Some((s.pts, s.pts, s.keyframe))
            }
        }
    }

    pub fn read_into(&self, track: usize, i: usize, buf: &mut Vec<u8>) -> Result<()> {
        match self {
            Demux::Mp4(r) => r.read_into(track, i, buf),
            Demux::Mkv(r) => r.read_into(track, i, buf),
        }
    }

    pub fn seconds(&self, track: usize, ticks: i64) -> f64 {
        match self {
            Demux::Mp4(r) => r.tracks.get(track).map_or(0.0, |t| t.seconds(ticks)),
            Demux::Mkv(r) => r.tracks.get(track).map_or(0.0, |t| t.seconds(ticks)),
        }
    }

    pub fn seek(&self, track: usize, from: f64) -> Result<(usize, i64)> {
        match self {
            Demux::Mp4(r) => {
                let t = r
                    .tracks
                    .get(track)
                    .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
                let at = t.sample_at(t.ticks(from.max(0.0))).unwrap_or(0);
                Ok((t.keyframe_before(at), t.presentation(at).unwrap_or(0)))
            }
            Demux::Mkv(r) => {
                let t = mkv_track(r, track)?;
                let want = t.ticks(from.max(0.0));
                let first = t
                    .samples
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, s)| s.pts)
                    .map_or(0, |(i, _)| i);
                let at = t
                    .samples
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.pts <= want)
                    .max_by_key(|(_, s)| s.pts)
                    .map_or(first, |(i, _)| i);
                let key = (0..=at)
                    .rev()
                    .find(|&i| t.samples.get(i).is_some_and(|s| s.keyframe))
                    .unwrap_or(0);
                let start = t.samples.get(at).map_or(0, |s| s.pts);
                Ok((key, start))
            }
        }
    }

    pub fn stream(&self, track: usize) -> Result<Stream> {
        match self {
            Demux::Mp4(r) => {
                let t = r
                    .tracks
                    .get(track)
                    .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
                let codec = t.codec();
                let extradata = t.extradata().unwrap_or_default();
                let color = codec.and_then(|c| crate::gpu::color(c, &extradata));
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
                let color = tagged.or_else(|| codec.and_then(|c| crate::gpu::color(c, &extradata)));
                Ok(Stream {
                    codec,
                    name: t.codec_name().to_string(),
                    extradata,
                    avc,
                    time_base: t.time_base,
                    color,
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

pub fn probe(r: &mkv::Reader) -> Result<Probe> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    let t = mkv_track(r, v)?;
    let (width, height) = t
        .video
        .as_ref()
        .map(|i| (i.pixel_width, i.pixel_height))
        .unwrap_or((t.width, t.height));
    let duration = t.duration_seconds();
    let fps = match t.default_duration.filter(|d| *d > 0) {
        Some(ns) => 1e9 / ns as f64,
        None if duration > 0.0 => t.samples.len() as f64 / duration,
        None => 0.0,
    };
    let (profile, bit_depth) = match t.codec() {
        Some(Codec::H264) => AvcConfig::parse(&t.codec_private)
            .map(|c| avc_profile(c.profile))
            .unwrap_or((String::new(), 8)),
        Some(Codec::Hevc) => HevcConfig::parse(&t.codec_private)
            .map(|c| hevc_profile(&c))
            .unwrap_or((String::new(), 8)),
        Some(Codec::Vp9) => t
            .samples
            .iter()
            .position(|s| s.keyframe)
            .and_then(|i| r.read(v, i).ok())
            .and_then(|data| crate::vp9::keyframe_info(&data))
            .map(|k| (format!("Profile {}", k.profile), k.bit_depth.max(8) as u8))
            .unwrap_or((String::new(), 8)),
        _ => (String::new(), 8),
    };
    let audio = r
        .audio()
        .into_iter()
        .enumerate()
        .filter_map(|(index, i)| {
            let a = r.tracks.get(i)?;
            let name = a.name.trim();
            Some(AudioTrack {
                index,
                stream: i,
                title: (!name.is_empty()).then(|| name.to_string()),
                language: (!a.language.is_empty() && a.language != "und")
                    .then(|| a.language.clone()),
                codec: a.codec_name().to_string(),
                channels: a.channels() as u16,
                rate: a.sample_rate(),
                bitrate: None,
                default: a.default,
            })
        })
        .collect();
    Ok(Probe {
        duration,
        fps: rational(fps),
        width,
        height,
        rotation: t.rotation(),
        video_codec: t.codec(),
        video_codec_name: t.codec_name().to_string(),
        profile,
        bit_depth,
        frames: Some(t.samples.len() as u64),
        audio,
    })
}

pub fn keyframes(r: &mkv::Reader) -> Result<Vec<f64>> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    let t = mkv_track(r, v)?;
    let mut keys: Vec<f64> = t
        .samples
        .iter()
        .filter(|s| s.keyframe)
        .map(|s| t.seconds(s.pts).max(0.0))
        .collect();
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    Ok(keys)
}
