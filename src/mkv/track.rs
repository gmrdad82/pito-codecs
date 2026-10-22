use crate::media::{Codec, TimeBase};
use crate::mp4::TrackKind;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mastering {
    pub primaries: [(f64, f64); 3],
    pub white_point: (f64, f64),
    pub luminance_max: f64,
    pub luminance_min: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Colour {
    pub matrix: Option<u64>,
    pub bits_per_channel: Option<u64>,
    pub chroma_siting: (Option<u64>, Option<u64>),
    pub range: Option<u64>,
    pub transfer: Option<u64>,
    pub primaries: Option<u64>,
    pub max_cll: Option<u64>,
    pub max_fall: Option<u64>,
    pub mastering: Option<Mastering>,
}

impl Colour {
    pub fn full_range(&self) -> bool {
        self.range == Some(2)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub kind: u64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub display_unit: u64,
    pub crop: (u32, u32, u32, u32),
    pub interlaced: u64,
    pub colour: Option<Colour>,
    pub projection: Option<Projection>,
    pub alpha_mode: u64,
}

impl VideoInfo {
    pub fn rotation(&self) -> Option<u16> {
        let Some(p) = &self.projection else {
            return Some(0);
        };
        if p.kind != 0 || p.yaw != 0.0 || p.pitch != 0.0 || p.roll % 90.0 != 0.0 {
            return None;
        }
        Some((-(p.roll / 90.0) as i64).rem_euclid(4) as u16 * 90)
    }

    pub fn pixel_aspect(&self) -> (u32, u32) {
        let pw = self
            .pixel_width
            .saturating_sub(self.crop.2.saturating_add(self.crop.3)) as u64;
        let ph = self
            .pixel_height
            .saturating_sub(self.crop.0.saturating_add(self.crop.1)) as u64;
        let (Some(dw), Some(dh)) = (self.display_width, self.display_height) else {
            return (1, 1);
        };
        if pw == 0 || ph == 0 || dw == 0 || dh == 0 || !matches!(self.display_unit, 0 | 3) {
            return (1, 1);
        }
        let (n, d) = (dw as u64 * ph, dh as u64 * pw);
        let g = gcd(n, d);
        ((n / g) as u32, (d / g) as u32)
    }
}

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioInfo {
    pub sampling_frequency: f64,
    pub output_sampling_frequency: Option<f64>,
    pub channels: u64,
    pub bit_depth: Option<u64>,
}

impl Default for AudioInfo {
    fn default() -> Self {
        Self {
            sampling_frequency: 8000.0,
            output_sampling_frequency: None,
            channels: 1,
            bit_depth: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentEncoding {
    HeaderStripping { bytes: Vec<u8>, scope: u64 },
    Compression { algo: u64, scope: u64 },
    Encryption { scope: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub pts: i64,
    pub duration: u32,
    pub keyframe: bool,
    pub cluster: u32,
    pub block: u64,
    pub lace: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub enabled: bool,
    pub default: bool,
    pub name: String,
    pub language: String,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub default_duration: Option<u64>,
    pub codec_delay: u64,
    pub seek_pre_roll: u64,
    pub width: u32,
    pub height: u32,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    pub encodings: Vec<ContentEncoding>,
    pub time_base: TimeBase,
    pub samples: Vec<Sample>,
    pub(crate) sync: Vec<u32>,
    pub(crate) pts_order: Vec<u32>,
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

pub fn codec_name(codec_id: &str) -> &str {
    match codec_id {
        "V_MPEG4/ISO/AVC" => "h264",
        "V_MPEGH/ISO/HEVC" => "hevc",
        "V_VP8" => "vp8",
        "V_VP9" => "vp9",
        "V_AV1" => "av1",
        "A_OPUS" => "opus",
        "A_VORBIS" => "vorbis",
        "A_FLAC" => "flac",
        "A_ALAC" => "alac",
        "A_AC3" => "ac3",
        "A_EAC3" => "eac3",
        "A_MPEG/L3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "A_MPEG/L1" => "mp1",
        "S_TEXT/UTF8" => "subrip",
        "S_TEXT/WEBVTT" => "webvtt",
        "S_TEXT/ASS" => "ass",
        id if id.starts_with("A_AAC") => "aac",
        id if id.starts_with("A_PCM") => "pcm",
        id => id,
    }
}

impl Track {
    pub fn codec(&self) -> Option<Codec> {
        match codec_name(&self.codec_id) {
            "h264" => Some(Codec::H264),
            "hevc" => Some(Codec::Hevc),
            "aac" => Some(Codec::Aac),
            "vp9" => Some(Codec::Vp9),
            "av1" => Some(Codec::Av1),
            "opus" => Some(Codec::Opus),
            "ac3" => Some(Codec::Ac3),
            "eac3" => Some(Codec::Eac3),
            "mp3" | "mp2" | "mp1" => Some(Codec::Mp3),
            _ => None,
        }
    }

    pub fn codec_name(&self) -> &str {
        codec_name(&self.codec_id)
    }

    pub fn extradata(&self) -> Option<Vec<u8>> {
        if !self.codec_private.is_empty() {
            return Some(self.codec_private.clone());
        }
        let id = self.codec_id.as_str();
        if !id.starts_with("A_AAC/") {
            return None;
        }
        let a = self.audio.clone().unwrap_or_default();
        let object_type: u8 = if id.ends_with("/MAIN") {
            1
        } else if id.ends_with("/SSR") {
            3
        } else if id.ends_with("/LTP") {
            4
        } else {
            2
        };
        let index = |r: f64| AAC_RATES.iter().position(|&x| x as f64 == r).unwrap_or(4) as u8;
        let ri = index(a.sampling_frequency);
        let ch = a.channels.min(7) as u8;
        let mut v = vec![(object_type << 3) | (ri >> 1), ((ri & 1) << 7) | (ch << 3)];
        if id.ends_with("/SBR") {
            let ext = index(
                a.output_sampling_frequency
                    .unwrap_or(a.sampling_frequency * 2.0),
            );
            v.extend_from_slice(&[0x56, 0xE5, 0x80 | (ext << 3)]);
        }
        Some(v)
    }

    pub fn channels(&self) -> u32 {
        self.audio.as_ref().map_or(0, |a| a.channels as u32)
    }

    pub fn sample_rate(&self) -> u32 {
        self.audio.as_ref().map_or(0, |a| {
            a.output_sampling_frequency
                .filter(|_| self.codec_name() == "aac")
                .unwrap_or(a.sampling_frequency)
                .round() as u32
        })
    }

    pub fn rotation(&self) -> u16 {
        self.video
            .as_ref()
            .and_then(VideoInfo::rotation)
            .unwrap_or(0)
    }

    pub fn alpha(&self) -> bool {
        self.kind == TrackKind::Video && self.video.as_ref().is_some_and(|v| v.alpha_mode == 1)
    }

    pub fn seconds(&self, ticks: i64) -> f64 {
        self.time_base.seconds(ticks)
    }

    pub fn ticks(&self, seconds: f64) -> i64 {
        (seconds * self.time_base.den as f64 / self.time_base.num as f64).round() as i64
    }

    pub fn nanos(&self, ticks: i64) -> i64 {
        (ticks as i128 * self.time_base.num as i128 * 1_000_000_000 / self.time_base.den as i128)
            as i64
    }

    pub fn end(&self) -> i64 {
        self.samples
            .iter()
            .map(|s| s.pts.saturating_add(s.duration as i64))
            .max()
            .unwrap_or(0)
    }

    pub fn duration_seconds(&self) -> f64 {
        let start = self.samples.iter().map(|s| s.pts).min().unwrap_or(0).min(0);
        self.seconds(self.end().saturating_sub(start))
    }

    pub fn fps(&self) -> f64 {
        if let Some(d) = self.default_duration.filter(|&d| d > 0) {
            return 1e9 / d as f64;
        }
        let n = self.samples.len();
        let span = self
            .end()
            .saturating_sub(self.samples.iter().map(|s| s.pts).min().unwrap_or(0));
        if n == 0 || span <= 0 {
            0.0
        } else {
            n as f64 / self.seconds(span)
        }
    }

    pub fn keyframes(&self) -> &[u32] {
        &self.sync
    }

    pub fn keyframe_before(&self, index: usize) -> usize {
        let k = self.sync.partition_point(|&i| i as usize <= index);
        if k == 0 { 0 } else { self.sync[k - 1] as usize }
    }

    pub fn sample_at(&self, pts: i64) -> Option<usize> {
        if self.pts_order.is_empty() {
            return None;
        }
        let k = self
            .pts_order
            .partition_point(|&i| self.samples[i as usize].pts <= pts);
        Some(self.pts_order[k.saturating_sub(1)] as usize)
    }

    pub fn keyframe_at(&self, pts: i64) -> Option<usize> {
        let mut best: Option<usize> = None;
        for &k in &self.sync {
            let s = &self.samples[k as usize];
            if s.pts <= pts && best.is_none_or(|b| s.pts >= self.samples[b].pts) {
                best = Some(k as usize);
            }
        }
        best.or_else(|| self.sync.first().map(|&k| k as usize))
    }

    pub fn stripped_header(&self) -> Option<&[u8]> {
        self.encodings.iter().find_map(|e| match e {
            ContentEncoding::HeaderStripping { bytes, scope } if scope & 1 != 0 => {
                Some(bytes.as_slice())
            }
            _ => None,
        })
    }

    pub fn frames_readable(&self) -> bool {
        self.encodings.iter().all(|e| match e {
            ContentEncoding::HeaderStripping { .. } => true,
            ContentEncoding::Compression { scope, .. } | ContentEncoding::Encryption { scope } => {
                scope & 1 == 0
            }
        })
    }

    pub(crate) fn finish_indexes(&mut self) {
        self.sync = self
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
