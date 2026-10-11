use super::ebml::{Children, float, string, uint};
use super::ids::*;
use super::track::{
    AudioInfo, Colour, ContentEncoding, Mastering, Projection, Track, VideoInfo, gcd,
};
use crate::media::TimeBase;
use crate::mp4::TrackKind;

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub timestamp_scale: u64,
    pub duration: Option<f64>,
    pub title: Option<String>,
    pub muxing_app: String,
    pub writing_app: String,
}

impl Default for Info {
    fn default() -> Self {
        Self {
            timestamp_scale: 1_000_000,
            duration: None,
            title: None,
            muxing_app: String::new(),
            writing_app: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekEntry {
    pub id: u32,
    pub position: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CuePoint {
    pub time: u64,
    pub track: u64,
    pub cluster: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cluster {
    pub offset: u64,
    pub data_start: u64,
    pub end: u64,
    pub timestamp: u64,
}

pub(crate) fn time_base(scale: u64) -> TimeBase {
    let g = gcd(scale, 1_000_000_000);
    let (num, den) = (scale / g, 1_000_000_000 / g);
    if num <= u32::MAX as u64 && den <= u32::MAX as u64 {
        TimeBase::new(num as u32, den as u32)
    } else {
        TimeBase::new(1, 1000)
    }
}

pub(crate) fn info(d: &[u8]) -> Info {
    let mut info = Info::default();
    for e in Children::new(d) {
        match e.id {
            TIMESTAMP_SCALE => {
                let v = uint(e.data);
                if v > 0 && v <= u32::MAX as u64 {
                    info.timestamp_scale = v;
                }
            }
            DURATION => info.duration = Some(float(e.data)).filter(|v| v.is_finite() && *v >= 0.0),
            TITLE => info.title = Some(string(e.data)),
            MUXING_APP => info.muxing_app = string(e.data),
            WRITING_APP => info.writing_app = string(e.data),
            _ => {}
        }
    }
    info
}

pub(crate) fn seek_head(d: &[u8]) -> Vec<SeekEntry> {
    let mut out = Vec::new();
    for s in Children::new(d).filter(|e| e.id == SEEK) {
        let (mut id, mut pos) = (None, None);
        for e in Children::new(s.data) {
            match e.id {
                SEEK_ID => id = Some(uint(e.data) as u32),
                SEEK_POSITION => pos = Some(uint(e.data)),
                _ => {}
            }
        }
        if let (Some(id), Some(position)) = (id, pos) {
            out.push(SeekEntry { id, position });
        }
    }
    out
}

pub(crate) fn cues(d: &[u8], out: &mut Vec<CuePoint>) {
    for cp in Children::new(d).filter(|e| e.id == CUE_POINT) {
        let mut time = None;
        let start = out.len();
        for e in Children::new(cp.data) {
            match e.id {
                CUE_TIME => time = Some(uint(e.data)),
                CUE_TRACK_POSITIONS => {
                    let (mut track, mut cluster) = (0, u64::MAX);
                    for c in Children::new(e.data) {
                        match c.id {
                            CUE_TRACK => track = uint(c.data),
                            CUE_CLUSTER_POSITION => cluster = uint(c.data),
                            _ => {}
                        }
                    }
                    if track != 0 && cluster != u64::MAX {
                        out.push(CuePoint {
                            time: 0,
                            track,
                            cluster,
                        });
                    }
                }
                _ => {}
            }
        }
        match time {
            Some(t) => out[start..].iter_mut().for_each(|c| c.time = t),
            None => out.truncate(start),
        }
    }
}

fn colour(d: &[u8]) -> Colour {
    let mut c = Colour::default();
    for e in Children::new(d) {
        let v = Some(uint(e.data));
        match e.id {
            MATRIX_COEFFICIENTS => c.matrix = v,
            BITS_PER_CHANNEL => c.bits_per_channel = v,
            CHROMA_SITING_HORZ => c.chroma_siting.0 = v,
            CHROMA_SITING_VERT => c.chroma_siting.1 = v,
            RANGE => c.range = v,
            TRANSFER_CHARACTERISTICS => c.transfer = v,
            PRIMARIES => c.primaries = v,
            MAX_CLL => c.max_cll = v,
            MAX_FALL => c.max_fall = v,
            MASTERING_METADATA => {
                let mut m = Mastering::default();
                for x in Children::new(e.data) {
                    let f = float(x.data);
                    match x.id {
                        PRIMARY_R_X => m.primaries[0].0 = f,
                        PRIMARY_R_Y => m.primaries[0].1 = f,
                        PRIMARY_G_X => m.primaries[1].0 = f,
                        PRIMARY_G_Y => m.primaries[1].1 = f,
                        PRIMARY_B_X => m.primaries[2].0 = f,
                        PRIMARY_B_Y => m.primaries[2].1 = f,
                        WHITE_POINT_X => m.white_point.0 = f,
                        WHITE_POINT_Y => m.white_point.1 = f,
                        LUMINANCE_MAX => m.luminance_max = f,
                        LUMINANCE_MIN => m.luminance_min = f,
                        _ => {}
                    }
                }
                c.mastering = Some(m);
            }
            _ => {}
        }
    }
    c
}

fn video(d: &[u8]) -> VideoInfo {
    let mut v = VideoInfo::default();
    for e in Children::new(d) {
        let u = uint(e.data);
        match e.id {
            PIXEL_WIDTH => v.pixel_width = u as u32,
            PIXEL_HEIGHT => v.pixel_height = u as u32,
            DISPLAY_WIDTH => v.display_width = Some(u as u32),
            DISPLAY_HEIGHT => v.display_height = Some(u as u32),
            DISPLAY_UNIT => v.display_unit = u,
            PIXEL_CROP_TOP => v.crop.0 = u as u32,
            PIXEL_CROP_BOTTOM => v.crop.1 = u as u32,
            PIXEL_CROP_LEFT => v.crop.2 = u as u32,
            PIXEL_CROP_RIGHT => v.crop.3 = u as u32,
            FLAG_INTERLACED => v.interlaced = u,
            COLOUR => v.colour = Some(colour(e.data)),
            PROJECTION => {
                let mut p = Projection::default();
                for x in Children::new(e.data) {
                    match x.id {
                        PROJECTION_TYPE => p.kind = uint(x.data),
                        PROJECTION_POSE_YAW => p.yaw = float(x.data),
                        PROJECTION_POSE_PITCH => p.pitch = float(x.data),
                        PROJECTION_POSE_ROLL => p.roll = float(x.data),
                        _ => {}
                    }
                }
                v.projection = Some(p);
            }
            _ => {}
        }
    }
    v
}

fn audio(d: &[u8]) -> AudioInfo {
    let mut a = AudioInfo::default();
    for e in Children::new(d) {
        match e.id {
            SAMPLING_FREQUENCY => a.sampling_frequency = float(e.data),
            OUTPUT_SAMPLING_FREQUENCY => a.output_sampling_frequency = Some(float(e.data)),
            CHANNELS => a.channels = uint(e.data),
            BIT_DEPTH => a.bit_depth = Some(uint(e.data)),
            _ => {}
        }
    }
    a
}

fn encodings(d: &[u8]) -> Vec<ContentEncoding> {
    let mut list: Vec<(u64, ContentEncoding)> = Vec::new();
    for ce in Children::new(d).filter(|e| e.id == CONTENT_ENCODING) {
        let (mut order, mut scope, mut kind) = (0u64, 1u64, 0u64);
        let mut comp: Option<(u64, Vec<u8>)> = None;
        for e in Children::new(ce.data) {
            match e.id {
                CONTENT_ENCODING_ORDER => order = uint(e.data),
                CONTENT_ENCODING_SCOPE => scope = uint(e.data),
                CONTENT_ENCODING_TYPE => kind = uint(e.data),
                CONTENT_COMPRESSION => {
                    let (mut algo, mut settings) = (0u64, Vec::new());
                    for x in Children::new(e.data) {
                        match x.id {
                            CONTENT_COMP_ALGO => algo = uint(x.data),
                            CONTENT_COMP_SETTINGS => settings = x.data.to_vec(),
                            _ => {}
                        }
                    }
                    comp = Some((algo, settings));
                }
                _ => {}
            }
        }
        let enc = if kind == 1 {
            ContentEncoding::Encryption { scope }
        } else {
            match comp.unwrap_or((0, Vec::new())) {
                (3, bytes) => ContentEncoding::HeaderStripping { bytes, scope },
                (algo, _) => ContentEncoding::Compression { algo, scope },
            }
        };
        list.push((order, enc));
    }
    list.sort_by_key(|e| std::cmp::Reverse(e.0));
    list.into_iter().map(|e| e.1).collect()
}

pub(crate) fn tracks(d: &[u8], timestamp_scale: u64) -> Vec<Track> {
    let time_base = time_base(timestamp_scale);
    let mut out = Vec::new();
    for te in Children::new(d).filter(|e| e.id == TRACK_ENTRY) {
        let mut t = Track {
            number: 0,
            uid: 0,
            kind: TrackKind::Other,
            enabled: true,
            default: true,
            name: String::new(),
            language: "eng".into(),
            codec_id: String::new(),
            codec_private: Vec::new(),
            default_duration: None,
            codec_delay: 0,
            seek_pre_roll: 0,
            width: 0,
            height: 0,
            video: None,
            audio: None,
            encodings: Vec::new(),
            time_base,
            samples: Vec::new(),
            sync: Vec::new(),
            pts_order: Vec::new(),
        };
        for e in Children::new(te.data) {
            match e.id {
                TRACK_NUMBER => t.number = uint(e.data),
                TRACK_UID => t.uid = uint(e.data),
                TRACK_TYPE => {
                    t.kind = match uint(e.data) {
                        1 => TrackKind::Video,
                        2 => TrackKind::Audio,
                        0x11 => TrackKind::Subtitle,
                        _ => TrackKind::Other,
                    }
                }
                FLAG_ENABLED => t.enabled = uint(e.data) != 0,
                FLAG_DEFAULT => t.default = uint(e.data) != 0,
                DEFAULT_DURATION => t.default_duration = Some(uint(e.data)).filter(|&v| v > 0),
                NAME => t.name = string(e.data),
                LANGUAGE => t.language = string(e.data),
                CODEC_ID => t.codec_id = string(e.data),
                CODEC_PRIVATE => t.codec_private = e.data.to_vec(),
                CODEC_DELAY => t.codec_delay = uint(e.data),
                SEEK_PRE_ROLL => t.seek_pre_roll = uint(e.data),
                VIDEO => t.video = Some(video(e.data)),
                AUDIO => t.audio = Some(audio(e.data)),
                CONTENT_ENCODINGS => t.encodings = encodings(e.data),
                _ => {}
            }
        }
        if t.number == 0 {
            continue;
        }
        if t.kind == TrackKind::Audio && t.audio.is_none() {
            t.audio = Some(AudioInfo::default());
        }
        if let Some(v) = &t.video {
            t.width = v.pixel_width;
            t.height = v.pixel_height;
        }
        for enc in &t.encodings {
            if let ContentEncoding::HeaderStripping { bytes, scope } = enc
                && scope & 2 != 0
            {
                let mut p = bytes.clone();
                p.extend_from_slice(&t.codec_private);
                t.codec_private = p;
            }
        }
        out.push(t);
    }
    out
}
