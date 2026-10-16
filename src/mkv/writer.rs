use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use super::ebml::{write_id, write_size, write_unknown_size};
use super::ids::*;
use super::track::Track;
use crate::media::{Error, Packet, Result, TimeBase};
use crate::mp4::{CodecConfig, TrackKind};

const SEEK_HEAD_SPACE: usize = 128;
const SCALE: i64 = 1_000_000;
const KEYFRAME_CLUSTER_NS: i64 = 500_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub webm: bool,
    pub cluster_seconds: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            webm: false,
            cluster_seconds: 5.0,
        }
    }
}

impl Options {
    pub fn webm() -> Self {
        Self {
            webm: true,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackConfig {
    pub kind: TrackKind,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub time_base: TimeBase,
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
    pub sample_rate: u32,
    pub channels: u32,
    pub default_duration: Option<u64>,
    pub codec_delay: u64,
    pub seek_pre_roll: u64,
    pub language: String,
    pub name: Option<String>,
}

impl TrackConfig {
    pub fn new(kind: TrackKind, codec_id: &str, time_base: TimeBase) -> Self {
        Self {
            kind,
            codec_id: codec_id.into(),
            codec_private: Vec::new(),
            time_base,
            width: 0,
            height: 0,
            rotation: 0,
            sample_rate: 0,
            channels: 0,
            default_duration: None,
            codec_delay: 0,
            seek_pre_roll: 0,
            language: "und".into(),
            name: None,
        }
    }

    fn video(
        codec_id: &str,
        private: Vec<u8>,
        width: u32,
        height: u32,
        time_base: TimeBase,
    ) -> Self {
        Self {
            codec_private: private,
            width,
            height,
            ..Self::new(TrackKind::Video, codec_id, time_base)
        }
    }

    pub fn h264(avcc: Vec<u8>, width: u32, height: u32, time_base: TimeBase) -> Self {
        Self::video("V_MPEG4/ISO/AVC", avcc, width, height, time_base)
    }

    pub fn hevc(hvcc: Vec<u8>, width: u32, height: u32, time_base: TimeBase) -> Self {
        Self::video("V_MPEGH/ISO/HEVC", hvcc, width, height, time_base)
    }

    pub fn vp9(width: u32, height: u32, time_base: TimeBase) -> Self {
        Self::video("V_VP9", Vec::new(), width, height, time_base)
    }

    pub fn av1(av1c: Vec<u8>, width: u32, height: u32, time_base: TimeBase) -> Self {
        Self::video("V_AV1", av1c, width, height, time_base)
    }

    pub fn opus(head: Vec<u8>) -> Self {
        let channels = head.get(9).copied().unwrap_or(2) as u32;
        let pre_skip = head
            .get(10..12)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u64);
        Self {
            codec_private: head,
            sample_rate: 48000,
            channels,
            codec_delay: pre_skip * 1_000_000_000 / 48000,
            seek_pre_roll: 80_000_000,
            ..Self::new(TrackKind::Audio, "A_OPUS", TimeBase::new(1, 48000))
        }
    }

    pub fn aac(asc: Vec<u8>, sample_rate: u32, channels: u32) -> Self {
        Self {
            codec_private: asc,
            sample_rate,
            channels,
            ..Self::new(
                TrackKind::Audio,
                "A_AAC",
                TimeBase::new(1, sample_rate.max(1)),
            )
        }
    }

    pub fn from_track(track: &Track) -> Self {
        Self {
            kind: track.kind,
            codec_id: track.codec_id.clone(),
            codec_private: track.extradata().unwrap_or_default(),
            time_base: track.time_base,
            width: track.width,
            height: track.height,
            rotation: track.rotation(),
            sample_rate: track.sample_rate(),
            channels: track.channels(),
            default_duration: track.default_duration,
            codec_delay: track.codec_delay,
            seek_pre_roll: track.seek_pre_roll,
            language: track.language.clone(),
            name: (!track.name.is_empty()).then(|| track.name.clone()),
        }
    }

    pub fn from_mp4(track: &crate::mp4::Track) -> Result<Self> {
        let time_base = TimeBase::new(1, track.timescale.max(1));
        let entry = track
            .entry()
            .ok_or_else(|| Error::Invalid(format!("track {} has no sample entry", track.id)))?;
        let (w, h) = entry.video.map_or((track.width, track.height), |v| {
            (v.width as u32, v.height as u32)
        });
        let mut cfg = match &entry.codec {
            CodecConfig::Avc(c) => Self::h264(c.to_bytes(), w, h, time_base),
            CodecConfig::Hevc(c) => Self::hevc(c.to_bytes(), w, h, time_base),
            CodecConfig::Av1(c) => Self::av1(c.to_bytes(), w, h, time_base),
            CodecConfig::Aac(c) => {
                let mut cfg = Self::aac(c.asc.clone(), track.sample_rate(), track.channels());
                cfg.time_base = time_base;
                cfg
            }
            other => {
                return Err(Error::Unsupported(format!(
                    "{} in Matroska from MP4",
                    other.name()
                )));
            }
        };
        cfg.rotation = track.rotation();
        cfg.language = track.language.clone();
        cfg.name = (!track.name.is_empty()).then(|| track.name.clone());
        Ok(cfg)
    }

    fn nanos(&self, ticks: i64) -> i64 {
        (ticks as i128 * self.time_base.num as i128 * 1_000_000_000
            / self.time_base.den.max(1) as i128) as i64
    }
}

fn round_ticks(ns: i64) -> i64 {
    if ns >= 0 {
        ns.saturating_add(SCALE / 2) / SCALE
    } else {
        ns.saturating_sub(SCALE / 2) / SCALE
    }
}

fn webm_codec(id: &str) -> bool {
    matches!(
        id,
        "V_VP8" | "V_VP9" | "V_AV1" | "A_OPUS" | "A_VORBIS" | "S_TEXT/WEBVTT"
    ) || id.starts_with("D_WEBVTT/")
}

pub(crate) fn el(out: &mut Vec<u8>, id: u32, data: &[u8]) {
    write_id(out, id);
    write_size(out, data.len() as u64, 1);
    out.extend_from_slice(data);
}

pub(crate) fn el_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let n = (8 - v.leading_zeros() as usize / 8).max(1);
    el(out, id, &v.to_be_bytes()[8 - n..]);
}

pub(crate) fn el_float(out: &mut Vec<u8>, id: u32, v: f64) {
    el(out, id, &v.to_be_bytes());
}

pub(crate) fn el_str(out: &mut Vec<u8>, id: u32, s: &str) {
    el(out, id, s.as_bytes());
}

fn void(out: &mut Vec<u8>, total: usize) {
    let sl = if total - 2 <= 126 { 1 } else { 8 };
    write_id(out, VOID);
    write_size(out, (total - 1 - sl) as u64, sl);
    out.resize(out.len() + total - 1 - sl, 0);
}

fn track_entry(t: &TrackConfig, number: u64) -> Vec<u8> {
    let mut e = Vec::new();
    el_uint(&mut e, TRACK_NUMBER, number);
    el_uint(&mut e, TRACK_UID, number);
    let kind = match t.kind {
        TrackKind::Video => 1,
        TrackKind::Audio => 2,
        TrackKind::Subtitle => 0x11,
        TrackKind::Other => 0x21,
    };
    el_uint(&mut e, TRACK_TYPE, kind);
    el_uint(&mut e, FLAG_LACING, 0);
    el_str(&mut e, LANGUAGE, &t.language);
    if let Some(n) = &t.name {
        el_str(&mut e, NAME, n);
    }
    el_str(&mut e, CODEC_ID, &t.codec_id);
    if !t.codec_private.is_empty() {
        el(&mut e, CODEC_PRIVATE, &t.codec_private);
    }
    if let Some(d) = t.default_duration {
        el_uint(&mut e, DEFAULT_DURATION, d);
    }
    if t.codec_delay > 0 {
        el_uint(&mut e, CODEC_DELAY, t.codec_delay);
    }
    if t.seek_pre_roll > 0 {
        el_uint(&mut e, SEEK_PRE_ROLL, t.seek_pre_roll);
    }
    if t.kind == TrackKind::Video {
        let mut v = Vec::new();
        el_uint(&mut v, PIXEL_WIDTH, t.width as u64);
        el_uint(&mut v, PIXEL_HEIGHT, t.height as u64);
        if !t.rotation.is_multiple_of(360) {
            let mut p = Vec::new();
            el_uint(&mut p, PROJECTION_TYPE, 0);
            el_float(&mut p, PROJECTION_POSE_ROLL, -((t.rotation % 360) as f64));
            el(&mut v, PROJECTION, &p);
        }
        el(&mut e, VIDEO, &v);
    }
    if t.kind == TrackKind::Audio {
        let mut a = Vec::new();
        el_float(&mut a, SAMPLING_FREQUENCY, t.sample_rate as f64);
        el_uint(&mut a, CHANNELS, t.channels.max(1) as u64);
        el(&mut e, AUDIO, &a);
    }
    e
}

struct WTrack {
    cfg: TrackConfig,
    last_ns: Option<i64>,
    delta_ns: i64,
}

struct Layout {
    segment_size: u64,
    segment_start: u64,
    duration: u64,
    info: u64,
    tracks: u64,
}

pub struct Writer<W: Write + Seek = File> {
    w: W,
    options: Options,
    tracks: Vec<WTrack>,
    layout: Option<Layout>,
    has_video: bool,
    cluster: Vec<u8>,
    cluster_ts: Option<i64>,
    cluster_pos: u64,
    cues: Vec<(u64, u64, u64)>,
    end_ns: i64,
}

impl Writer<File> {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with(path, Options::default())
    }

    pub fn create_with(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let path = path.as_ref();
        let f = File::create(path).map_err(|e| Error::at(path, e))?;
        Ok(Self::new(f, options))
    }
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(w: W, options: Options) -> Self {
        Self {
            w,
            options,
            tracks: Vec::new(),
            layout: None,
            has_video: false,
            cluster: Vec::with_capacity(1 << 20),
            cluster_ts: None,
            cluster_pos: 0,
            cues: Vec::new(),
            end_ns: 0,
        }
    }

    pub fn add_track(&mut self, cfg: TrackConfig) -> Result<usize> {
        if self.layout.is_some() {
            return Err(Error::Invalid(
                "tracks must be added before the first packet".into(),
            ));
        }
        if self.options.webm && !webm_codec(&cfg.codec_id) {
            return Err(Error::Unsupported(format!(
                "WebM carries VP8, VP9, AV1, Opus and Vorbis, not {}; write Matroska",
                super::track::codec_name(&cfg.codec_id)
            )));
        }
        if cfg.time_base.num == 0 || cfg.time_base.den == 0 {
            return Err(Error::Invalid("track time base is zero".into()));
        }
        if self.tracks.len() >= 126 {
            return Err(Error::Unsupported("more than 126 tracks".into()));
        }
        self.has_video |= cfg.kind == TrackKind::Video;
        self.tracks.push(WTrack {
            cfg,
            last_ns: None,
            delta_ns: 0,
        });
        Ok(self.tracks.len() - 1)
    }

    fn start(&mut self) -> Result<()> {
        if self.layout.is_some() {
            return Ok(());
        }
        if self.tracks.is_empty() {
            return Err(Error::Invalid("no tracks".into()));
        }
        let mut out = Vec::new();
        let mut h = Vec::new();
        el_uint(&mut h, EBML_VERSION, 1);
        el_uint(&mut h, EBML_READ_VERSION, 1);
        el_uint(&mut h, EBML_MAX_ID_LENGTH, 4);
        el_uint(&mut h, EBML_MAX_SIZE_LENGTH, 8);
        el_str(
            &mut h,
            DOC_TYPE,
            if self.options.webm {
                "webm"
            } else {
                "matroska"
            },
        );
        el_uint(&mut h, DOC_TYPE_VERSION, 4);
        el_uint(&mut h, DOC_TYPE_READ_VERSION, 2);
        el(&mut out, EBML, &h);
        write_id(&mut out, SEGMENT);
        let base = self.w.stream_position()?;
        let segment_size = base + out.len() as u64;
        write_unknown_size(&mut out, 8);
        let segment_start = base + out.len() as u64;
        void(&mut out, SEEK_HEAD_SPACE);
        let info_pos = base + out.len() as u64;
        let mut info = Vec::new();
        el_uint(&mut info, TIMESTAMP_SCALE, SCALE as u64);
        el_str(&mut info, MUXING_APP, "pito-codecs");
        el_str(&mut info, WRITING_APP, "pito-codecs");
        let duration_in_info = info.len() + 3;
        el_float(&mut info, DURATION, 0.0);
        write_id(&mut out, INFO);
        write_size(&mut out, info.len() as u64, 1);
        let duration = base + out.len() as u64 + duration_in_info as u64;
        out.extend_from_slice(&info);
        let tracks_pos = base + out.len() as u64;
        let mut tr = Vec::new();
        for (i, t) in self.tracks.iter().enumerate() {
            el(&mut tr, TRACK_ENTRY, &track_entry(&t.cfg, i as u64 + 1));
        }
        el(&mut out, TRACKS, &tr);
        self.w.write_all(&out)?;
        self.layout = Some(Layout {
            segment_size,
            segment_start,
            duration,
            info: info_pos,
            tracks: tracks_pos,
        });
        Ok(())
    }

    pub fn push(&mut self, track: usize, packet: &Packet) -> Result<()> {
        self.push_sample(track, &packet.data, packet.pts, None, packet.keyframe)
    }

    pub fn push_timed(&mut self, track: usize, packet: &Packet, duration: u32) -> Result<()> {
        self.push_sample(
            track,
            &packet.data,
            packet.pts,
            Some(duration),
            packet.keyframe,
        )
    }

    pub fn push_sample(
        &mut self,
        track: usize,
        data: &[u8],
        pts: i64,
        duration: Option<u32>,
        keyframe: bool,
    ) -> Result<()> {
        self.start()?;
        let t = self
            .tracks
            .get_mut(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let pts_ns = t.cfg.nanos(pts);
        let ts = round_ticks(pts_ns) + round_ticks(t.cfg.codec_delay as i64);
        if ts < 0 {
            return Err(Error::Invalid(format!(
                "negative timestamp {pts} in track {track}"
            )));
        }
        let duration_ns = duration.map(|d| t.cfg.nanos(d as i64));
        if let Some(last) = t.last_ns
            && pts_ns > last
        {
            t.delta_ns = pts_ns.saturating_sub(last);
        }
        t.last_ns = Some(t.last_ns.map_or(pts_ns, |l| l.max(pts_ns)));
        let length = duration_ns
            .or(t.cfg.default_duration.map(|d| d as i64))
            .unwrap_or(t.delta_ns);
        self.end_ns = self.end_ns.max(pts_ns.saturating_add(length));
        let video = t.cfg.kind == TrackKind::Video;
        let explicit = duration_ns.filter(|&d| Some(d as u64) != t.cfg.default_duration);
        let start_new = match self.cluster_ts {
            None => true,
            Some(cts) => {
                let age = (ts - cts).saturating_mul(SCALE);
                let limit = (self.options.cluster_seconds * 1e9) as i64;
                let rel = ts - cts;
                !(-32768..=32767).contains(&rel)
                    || (video && keyframe && age >= KEYFRAME_CLUSTER_NS)
                    || (!self.has_video && age >= limit)
            }
        };
        if start_new {
            self.flush_cluster()?;
            self.cluster_ts = Some(ts);
            self.cluster_pos = self.w.stream_position()?;
            el_uint(&mut self.cluster, TIMESTAMP, ts as u64);
            if !self.has_video {
                self.cue(ts, track)?;
            }
        }
        if video && keyframe {
            self.cue(ts, track)?;
        }
        let rel = (ts - self.cluster_ts.unwrap_or(ts)) as i16;
        let number = track as u8 + 1;
        match explicit {
            None => {
                write_id(&mut self.cluster, SIMPLE_BLOCK);
                write_size(&mut self.cluster, data.len() as u64 + 4, 1);
                self.cluster.push(0x80 | number);
                self.cluster.extend_from_slice(&rel.to_be_bytes());
                self.cluster.push(if keyframe { 0x80 } else { 0 });
                self.cluster.extend_from_slice(data);
            }
            Some(d) => {
                let mut tail = Vec::with_capacity(16);
                el_uint(
                    &mut tail,
                    BLOCK_DURATION,
                    (d.saturating_add(SCALE / 2) / SCALE) as u64,
                );
                if !keyframe {
                    el(&mut tail, REFERENCE_BLOCK, &[0xFF]);
                }
                let block_len = data.len() as u64 + 4;
                let mut head = Vec::with_capacity(16);
                write_id(&mut head, BLOCK);
                write_size(&mut head, block_len, 1);
                write_id(&mut self.cluster, BLOCK_GROUP);
                write_size(
                    &mut self.cluster,
                    (head.len() as u64 + block_len) + tail.len() as u64,
                    1,
                );
                self.cluster.extend_from_slice(&head);
                self.cluster.push(0x80 | number);
                self.cluster.extend_from_slice(&rel.to_be_bytes());
                self.cluster.push(0);
                self.cluster.extend_from_slice(data);
                self.cluster.extend_from_slice(&tail);
            }
        }
        Ok(())
    }

    fn cue(&mut self, ts: i64, track: usize) -> Result<()> {
        let Some(l) = &self.layout else {
            return Ok(());
        };
        let pos = self.cluster_pos - l.segment_start;
        let number = track as u64 + 1;
        if self.cues.last().is_none_or(|c| c.2 != pos || c.1 != number) {
            self.cues.push((ts as u64, number, pos));
        }
        Ok(())
    }

    fn flush_cluster(&mut self) -> Result<()> {
        if self.cluster_ts.is_some() {
            let mut head = Vec::with_capacity(12);
            write_id(&mut head, CLUSTER);
            write_size(&mut head, self.cluster.len() as u64, 8);
            self.w.write_all(&head)?;
            self.w.write_all(&self.cluster)?;
        }
        self.cluster.clear();
        self.cluster_ts = None;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        self.start()?;
        self.flush_cluster()?;
        let Some(l) = self.layout.take() else {
            return Err(Error::Invalid("no tracks".into()));
        };
        let cues_pos = self.w.stream_position()?;
        let mut cues = Vec::new();
        for &(time, track, pos) in &self.cues {
            let mut ctp = Vec::new();
            el_uint(&mut ctp, CUE_TRACK, track);
            el_uint(&mut ctp, CUE_CLUSTER_POSITION, pos);
            let mut cp = Vec::new();
            el_uint(&mut cp, CUE_TIME, time);
            el(&mut cp, CUE_TRACK_POSITIONS, &ctp);
            el(&mut cues, CUE_POINT, &cp);
        }
        if !cues.is_empty() {
            let mut out = Vec::new();
            el(&mut out, CUES, &cues);
            self.w.write_all(&out)?;
        }
        let end = self.w.stream_position()?;
        let mut sh = Vec::new();
        let mut entries = vec![(INFO, l.info), (TRACKS, l.tracks)];
        if !cues.is_empty() {
            entries.push((CUES, cues_pos));
        }
        for (id, pos) in entries {
            let mut s = Vec::new();
            let mut idb = Vec::new();
            write_id(&mut idb, id);
            el(&mut s, SEEK_ID, &idb);
            el_uint(&mut s, SEEK_POSITION, pos - l.segment_start);
            el(&mut sh, SEEK, &s);
        }
        let mut out = Vec::new();
        el(&mut out, SEEK_HEAD, &sh);
        let rest = SEEK_HEAD_SPACE - out.len();
        void(&mut out, rest);
        self.w.seek(SeekFrom::Start(l.segment_start))?;
        self.w.write_all(&out)?;
        let duration = self.end_ns.max(0) as f64 / SCALE as f64;
        self.w.seek(SeekFrom::Start(l.duration))?;
        self.w.write_all(&duration.to_be_bytes())?;
        let mut size = Vec::new();
        write_size(&mut size, end - l.segment_start, 8);
        self.w.seek(SeekFrom::Start(l.segment_size))?;
        self.w.write_all(&size)?;
        self.w.seek(SeekFrom::Start(end))?;
        self.w.flush()?;
        Ok(self.w)
    }
}
