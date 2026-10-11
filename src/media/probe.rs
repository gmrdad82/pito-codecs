use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use super::ffmpeg::{Ffmpeg, codec_of};
use super::route::Prefs;
use super::types::{Codec, Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub duration: f64,
    pub fps: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
    pub video_codec: Option<Codec>,
    pub video_codec_name: String,
    pub profile: String,
    pub bit_depth: u8,
    pub frames: Option<u64>,
    pub audio: Vec<AudioTrack>,
}

impl Probe {
    pub fn fps(&self) -> f64 {
        self.fps.0 as f64 / self.fps.1.max(1) as f64
    }

    pub fn shown_size(&self) -> (u32, u32) {
        if self.rotation % 180 == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    pub index: usize,
    pub stream: usize,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: String,
    pub channels: u16,
    pub rate: u32,
    pub bitrate: Option<u32>,
    pub default: bool,
}

type Key = (PathBuf, u64, Option<SystemTime>);

struct Known {
    probe: Option<Probe>,
    keys: Option<Vec<f64>>,
}

fn known() -> &'static Mutex<HashMap<Key, Known>> {
    static KNOWN: OnceLock<Mutex<HashMap<Key, Known>>> = OnceLock::new();
    KNOWN.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(path: &Path) -> Result<Key> {
    let meta = std::fs::metadata(path).map_err(|e| Error::at(path, e))?;
    Ok((path.to_path_buf(), meta.len(), meta.modified().ok()))
}

fn remember(k: Key, f: impl FnOnce(&mut Known)) {
    let mut map = known().lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() > 4096 {
        map.clear();
    }
    f(map.entry(k).or_insert(Known {
        probe: None,
        keys: None,
    }));
}

pub fn probe(path: &Path, prefs: &Prefs) -> Result<Probe> {
    let k = key(path)?;
    if let Some(p) = known()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&k)
        .and_then(|e| e.probe.clone())
    {
        return Ok(p);
    }
    let p = match super::native::probe(path) {
        Ok(p) => p,
        Err(native) => match container_ffmpeg(prefs) {
            Ok(f) => from_flat(&f.probe_flat(&["-show_format", "-show_streams"], path)?)?,
            Err(_) => return Err(native),
        },
    };
    remember(k, |e| e.probe = Some(p.clone()));
    Ok(p)
}

pub fn keyframes(path: &Path, prefs: &Prefs) -> Result<Vec<f64>> {
    let k = key(path)?;
    if let Some(keys) = known()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&k)
        .and_then(|e| e.keys.clone())
    {
        return Ok(keys);
    }
    let keys = match super::native::keyframes(path) {
        Ok(k) => k,
        Err(native) => match keyframes_of(path, prefs) {
            Ok(k) => k,
            Err(_) => return Err(native),
        },
    };
    remember(k, |e| e.keys = Some(keys.clone()));
    Ok(keys)
}

fn keyframes_of(path: &Path, prefs: &Prefs) -> Result<Vec<f64>> {
    let f = container_ffmpeg(prefs)?;
    let listing = f.probe_csv(
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,flags",
        ],
        path,
    )?;
    let mut keys: Vec<f64> = listing
        .lines()
        .filter_map(|l| {
            let (t, flags) = l.trim().split_once(',')?;
            flags.contains('K').then_some(())?;
            t.parse::<f64>().ok().filter(|t| *t >= 0.0)
        })
        .collect();
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    Ok(keys)
}

pub(crate) fn container_ffmpeg(prefs: &Prefs) -> Result<Ffmpeg> {
    match prefs.user_ffmpeg() {
        Some(u) => Ffmpeg::find(u.path.as_deref()),
        None => Err(Error::Unsupported(
            "reading containers needs the MP4 module, not in this build yet, or ffmpeg picked in the settings"
                .to_string(),
        )),
    }
}

fn rate(s: Option<&String>) -> Option<(u32, u32)> {
    let (n, d) = s?.split_once('/')?;
    let (n, d) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0 && d > 0).then_some((n, d))
}

fn from_flat(m: &BTreeMap<String, String>) -> Result<Probe> {
    let get = |s: usize, k: &str| m.get(&format!("streams.stream.{s}.{k}"));
    let count = (0..)
        .take_while(|s| get(*s, "codec_type").is_some())
        .count();
    let video = (0..count)
        .find(|&s| {
            get(s, "codec_type").map(String::as_str) == Some("video")
                && get(s, "disposition.attached_pic").map(String::as_str) != Some("1")
        })
        .ok_or_else(|| Error::Invalid("no video stream".to_string()))?;
    let num = |s: usize, k: &str| get(s, k).and_then(|v| v.parse::<f64>().ok());
    let fps = rate(get(video, "avg_frame_rate"))
        .or_else(|| rate(get(video, "r_frame_rate")))
        .ok_or_else(|| Error::Invalid("no frame rate".to_string()))?;
    let duration = num(video, "duration")
        .or_else(|| m.get("format.duration").and_then(|v| v.parse().ok()))
        .filter(|d: &f64| d.is_finite() && *d >= 0.0)
        .ok_or_else(|| Error::Invalid("no duration".to_string()))?;
    let side = (0..8)
        .find_map(|i| num(video, &format!("side_data_list.side_data.{i}.rotation")))
        .map(|r| -r);
    let tag = num(video, "tags.rotate");
    let rotation = side.or(tag).unwrap_or(0.0).round() as i64;
    let rotation = ((rotation.rem_euclid(360) + 45) / 90 * 90 % 360) as u16;
    let codec_name = get(video, "codec_name").cloned().unwrap_or_default();
    let pix = get(video, "pix_fmt").cloned().unwrap_or_default();
    let audio = (0..count)
        .filter(|&s| get(s, "codec_type").map(String::as_str) == Some("audio"))
        .enumerate()
        .map(|(index, s)| AudioTrack {
            index,
            stream: get(s, "index").and_then(|v| v.parse().ok()).unwrap_or(s),
            title: ["tags.title", "tags.name", "tags.handler_name"]
                .iter()
                .filter_map(|k| get(s, k))
                .map(|t| t.trim())
                .find(|t| {
                    let l = t.to_lowercase();
                    !t.is_empty()
                        && ![
                            "soundhandler",
                            "sound handler",
                            "gpac",
                            "l-smash",
                            "media handler",
                        ]
                        .iter()
                        .any(|g| l.contains(g))
                })
                .map(str::to_string),
            language: get(s, "tags.language")
                .filter(|l| !l.is_empty() && *l != "und")
                .cloned(),
            codec: get(s, "codec_name").cloned().unwrap_or_default(),
            channels: num(s, "channels").unwrap_or(0.0) as u16,
            rate: num(s, "sample_rate").unwrap_or(0.0) as u32,
            bitrate: num(s, "bit_rate").map(|b| b as u32),
            default: get(s, "disposition.default").map(String::as_str) == Some("1"),
        })
        .collect();
    Ok(Probe {
        duration,
        fps,
        width: num(video, "width").unwrap_or(0.0) as u32,
        height: num(video, "height").unwrap_or(0.0) as u32,
        rotation,
        video_codec: codec_of(&codec_name),
        video_codec_name: codec_name,
        profile: get(video, "profile").cloned().unwrap_or_default(),
        bit_depth: if pix.contains("10") { 10 } else { 8 },
        frames: num(video, "nb_frames").map(|n| n as u64),
        audio,
    })
}
