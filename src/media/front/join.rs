use std::path::{Path, PathBuf};

use super::demux::Demux;
use super::export::{ExportOptions, Fit, Piece, Progress, Stage, WRITTEN, export, export_fitted};
use super::mux::{RATE, aac_encoder};
use super::route::Prefs;
use super::shape::{color_of, display};
use super::stream::native_stream;
use super::video::{Source, frame_at, source};
use crate::gpu::Color;
use crate::media::{AudioBuffer, Codec, Error, Packet, Result, Route, TimeBase};
use crate::mkv;
use crate::mp4::{self, AvcConfig, HevcConfig, SampleEntry, VpcConfig};

pub const SAME_RATE: f64 = 0.01;
pub const AAC_KBPS: u32 = 320;
pub const OPUS_KBPS: u32 = 128;
const PREROLL: f64 = 0.1;
const TELL_EVERY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMethod {
    Copy,
    Encode,
}

impl JoinMethod {
    pub fn name(self) -> &'static str {
        match self {
            JoinMethod::Copy => "copy",
            JoinMethod::Encode => "encode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAudio {
    Silent,
    Copied,
    Encoded { codec: Codec, kbps: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinOptions {
    pub encode: bool,
    pub size: Option<(u32, u32)>,
    pub fps: Option<(u32, u32)>,
    pub video: Codec,
    pub crf: u8,
    pub audio_kbps: Option<u32>,
    pub prefs: Prefs,
}

impl Default for JoinOptions {
    fn default() -> Self {
        Self {
            encode: false,
            size: None,
            fps: None,
            video: Codec::H264,
            crf: 18,
            audio_kbps: None,
            prefs: Prefs::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Joined {
    pub path: PathBuf,
    pub method: JoinMethod,
    pub reason: String,
    pub clips: usize,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub frames: u64,
    pub seconds: f64,
    pub video: Codec,
    pub route: Route,
    pub audio: JoinAudio,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mp4,
    Mov,
    Mkv,
    Webm,
}

impl Kind {
    fn of(out: &Path, what: &str) -> Result<Kind> {
        let ext = extension(out);
        match ext.as_str() {
            "mp4" | "m4v" => Ok(Kind::Mp4),
            "mov" => Ok(Kind::Mov),
            "mkv" => Ok(Kind::Mkv),
            "webm" => Ok(Kind::Webm),
            _ => Err(Error::Invalid(format!(
                "{what} writes mp4, mov, mkv or webm, not .{ext}"
            ))),
        }
    }

    fn video(self, codec: Option<Codec>) -> bool {
        matches!(
            (self, codec),
            (Kind::Webm, Some(Codec::Vp9))
                | (Kind::Mov, Some(Codec::H264 | Codec::Hevc))
                | (
                    Kind::Mp4 | Kind::Mkv,
                    Some(Codec::H264 | Codec::Hevc | Codec::Vp9)
                )
        )
    }

    fn audio(self, codec: &str, matroska: bool) -> bool {
        match self {
            Kind::Webm => matroska && matches!(codec, "opus" | "vorbis"),
            Kind::Mp4 | Kind::Mov => codec == "aac",
            Kind::Mkv => codec == "aac" || matroska,
        }
    }

    fn matroska(self) -> bool {
        matches!(self, Kind::Mkv | Kind::Webm)
    }
}

struct Sound {
    ordinal: usize,
    track: usize,
    codec: String,
    rate: u32,
    channels: u16,
    extradata: Vec<u8>,
}

struct Clip {
    path: PathBuf,
    name: String,
    src: Source,
    demux: Option<Demux>,
    video: usize,
    sar: (u16, u16),
    color: Color,
    extradata: Vec<u8>,
    entries: usize,
    audio: Option<Sound>,
}

impl Clip {
    fn load(path: &Path, prefs: &Prefs) -> Result<Clip> {
        let name = path.display().to_string();
        let src = source(path, prefs).map_err(|e| about(&name, e))?;
        let demux = src.demux.clone();
        let (sar, color) = color_of(&src);
        let (video, extradata, entries) = match &demux {
            Some(d) => {
                let v = d.video().map_err(|e| about(&name, e))?;
                let st = d.stream(v).map_err(|e| about(&name, e))?;
                let entries = d
                    .mp4()
                    .map_or(1, |r| r.tracks.get(v).map_or(1, |t| t.entries.len()));
                (v, st.extradata, entries)
            }
            None => (0, Vec::new(), 1),
        };
        let audio = src.probe.audio.first().map(|a| Sound {
            ordinal: a.index,
            track: a.stream,
            codec: a.codec.to_ascii_lowercase(),
            rate: a.rate,
            channels: a.channels,
            extradata: demux
                .as_ref()
                .and_then(|d| d.stream(a.stream).ok())
                .map(|s| s.extradata)
                .unwrap_or_default(),
        });
        Ok(Clip {
            path: path.to_path_buf(),
            name,
            src,
            demux,
            video,
            sar,
            color,
            extradata,
            entries,
            audio,
        })
    }

    fn codec(&self) -> &str {
        &self.src.probe.video_codec_name
    }

    fn matroska(&self) -> bool {
        matches!(self.demux, Some(Demux::Mkv(_)))
    }

    fn demux(&self) -> Result<&Demux> {
        self.demux.as_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "{}: our MP4 and MKV readers can't open it",
                self.name
            ))
        })
    }
}

fn about(name: &str, e: Error) -> Error {
    match e {
        Error::Invalid(s) => Error::Invalid(format!("{name}: {s}")),
        Error::Unsupported(s) => Error::Unsupported(format!("{name}: {s}")),
        Error::Backend(s) => Error::Backend(format!("{name}: {s}")),
        other => other,
    }
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|part| part.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => std::path::absolute(a).ok() == std::path::absolute(b).ok(),
    }
}

pub fn join(
    inputs: &[PathBuf],
    out: &Path,
    opts: JoinOptions,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Joined> {
    if inputs.len() < 2 {
        return Err(Error::Invalid("a join takes at least two clips".into()));
    }
    let kind = Kind::of(out, "join")?;
    if let Some(input) = inputs.iter().find(|input| same_file(input, out)) {
        return Err(Error::Invalid(format!(
            "{} is both a clip and the output",
            input.display()
        )));
    }
    if let Some(kbps) = opts.audio_kbps
        && !(6..=512).contains(&kbps)
    {
        return Err(Error::Invalid(format!(
            "an audio bitrate of {kbps} kbps, not 6 to 512"
        )));
    }
    let clips = inputs
        .iter()
        .map(|p| Clip::load(p, &opts.prefs))
        .collect::<Result<Vec<_>>>()?;
    if let (Some(kbps), false) = (opts.audio_kbps, clips.iter().any(|c| c.audio.is_some())) {
        return Err(Error::Invalid(format!(
            "the clips have no audio to encode at {kbps} kbps"
        )));
    }
    match copyable(&clips, kind, &opts) {
        Ok(reason) => copy(&clips, out, kind, &opts, reason, &mut progress),
        Err(reason) => encode(&clips, out, kind, &opts, reason, &mut progress),
    }
}

fn copyable(clips: &[Clip], kind: Kind, opts: &JoinOptions) -> std::result::Result<String, String> {
    let first = &clips[0];
    if opts.encode || opts.size.is_some() || opts.fps.is_some() {
        return Err("a video option asks for a re-encode".into());
    }
    if !kind.video(first.src.probe.video_codec) {
        return Err(format!(
            ".{} doesn't carry {}",
            kind_ext(kind),
            first.codec()
        ));
    }
    if let Some(c) = clips.iter().find(|c| c.demux.is_none()) {
        return Err(format!(
            "our MP4 and MKV readers can't index {} to copy it",
            c.name
        ));
    }
    if let Some(c) = clips.iter().find(|c| c.entries > 1) {
        return Err(format!(
            "{} changes its {} headers mid-stream",
            c.name,
            c.codec()
        ));
    }
    for other in &clips[1..] {
        if let Some(reason) = differs(first, other) {
            return Err(reason);
        }
    }
    Ok(alike(first))
}

fn kind_ext(kind: Kind) -> &'static str {
    match kind {
        Kind::Mp4 => "mp4",
        Kind::Mov => "mov",
        Kind::Mkv => "mkv",
        Kind::Webm => "webm",
    }
}

fn layout(channels: u16) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} channels"),
    }
}

fn rate_text(fps: Option<f64>) -> String {
    match fps {
        Some(fps) if (fps - fps.round()).abs() < 0.005 => format!("{fps:.0} fps"),
        Some(fps) => format!("{fps:.2} fps"),
        None => "an unknown rate".into(),
    }
}

fn fps_of(clip: &Clip) -> Option<f64> {
    let (n, d) = clip.src.probe.fps;
    (n > 0 && d > 0).then(|| n as f64 / d as f64)
}

fn alike(media: &Clip) -> String {
    let sound = match &media.audio {
        Some(a) => format!("with {} {} at {} Hz", a.codec, layout(a.channels), a.rate),
        None => "without audio".into(),
    };
    let p = &media.src.probe;
    format!(
        "every clip is {} {}x{} at {} {sound}",
        media.codec(),
        p.width,
        p.height,
        rate_text(fps_of(media))
    )
}

fn pix_fmt(depth: u8) -> String {
    if depth <= 8 {
        "yuv420p".into()
    } else {
        format!("yuv420p{depth}le")
    }
}

fn matrix_name(code: u8) -> String {
    match code {
        0 => "gbr".into(),
        1 => "bt709".into(),
        2 => "untagged".into(),
        4 => "fcc".into(),
        5 => "bt470bg".into(),
        6 => "smpte170m".into(),
        7 => "smpte240m".into(),
        8 => "ycgco".into(),
        9 => "bt2020nc".into(),
        10 => "bt2020c".into(),
        n => format!("matrix {n}"),
    }
}

fn primaries_name(code: u8) -> String {
    match code {
        1 => "bt709".into(),
        2 => "untagged".into(),
        4 => "bt470m".into(),
        5 => "bt470bg".into(),
        6 => "smpte170m".into(),
        7 => "smpte240m".into(),
        9 => "bt2020".into(),
        11 => "smpte431".into(),
        12 => "smpte432".into(),
        n => format!("primaries {n}"),
    }
}

fn transfer_name(code: u8) -> String {
    match code {
        1 => "bt709".into(),
        2 => "untagged".into(),
        4 => "gamma22".into(),
        5 => "gamma28".into(),
        6 => "smpte170m".into(),
        8 => "linear".into(),
        13 => "iec61966-2-1".into(),
        14 => "bt2020-10".into(),
        15 => "bt2020-12".into(),
        16 => "smpte2084".into(),
        18 => "arib-std-b67".into(),
        n => format!("transfer {n}"),
    }
}

fn tags(c: Color) -> String {
    let untagged = c.matrix == 2 && c.primaries == 2 && c.transfer == 2;
    let range = if c.full_range {
        "pc"
    } else if untagged {
        "untagged"
    } else {
        "tv"
    };
    [
        matrix_name(c.matrix),
        range.to_string(),
        primaries_name(c.primaries),
        transfer_name(c.transfer),
    ]
    .join("/")
}

fn differs(a: &Clip, b: &Clip) -> Option<String> {
    let (first, other) = (&a.name, &b.name);
    let (pa, pb) = (&a.src.probe, &b.src.probe);
    if b.codec() != a.codec() {
        return Some(format!(
            "{other} is {}, not {} like {first}",
            b.codec(),
            a.codec()
        ));
    }
    if pb.profile != pa.profile {
        let text = |s: &str| {
            if s.is_empty() {
                "untagged".to_string()
            } else {
                s.to_string()
            }
        };
        return Some(format!(
            "{other} is {} {}, not {} like {first}",
            b.codec(),
            text(&pb.profile),
            text(&pa.profile)
        ));
    }
    if (pb.width, pb.height) != (pa.width, pa.height) {
        return Some(format!(
            "{other} is {}x{}, not {}x{} like {first}",
            pb.width, pb.height, pa.width, pa.height
        ));
    }
    let square = |s: (u16, u16)| s.0 == s.1 || s.0 == 0 || s.1 == 0;
    if !(square(a.sar) && square(b.sar)) && a.sar != b.sar {
        let aspect = |s: (u16, u16)| {
            if square(s) {
                "square".to_string()
            } else {
                format!("{}:{}", s.0, s.1)
            }
        };
        return Some(format!(
            "{other}'s pixels are {}, not {} like {first}'s",
            aspect(b.sar),
            aspect(a.sar)
        ));
    }
    if pb.rotation != pa.rotation {
        return Some(format!(
            "{other} is turned {}°, not {}° like {first}",
            pb.rotation, pa.rotation
        ));
    }
    let same_rate = match (fps_of(a), fps_of(b)) {
        (Some(x), Some(y)) => (x - y).abs() < SAME_RATE,
        (None, None) => true,
        _ => false,
    };
    if !same_rate {
        return Some(format!(
            "{other} runs at {}, not {} like {first}",
            rate_text(fps_of(b)),
            rate_text(fps_of(a))
        ));
    }
    if pb.bit_depth != pa.bit_depth {
        return Some(format!(
            "{other} is {}, not {} like {first}",
            pix_fmt(pb.bit_depth),
            pix_fmt(pa.bit_depth)
        ));
    }
    if tags(b.color) != tags(a.color) {
        return Some(format!(
            "{other}'s colour tags are {}, not {} like {first}'s",
            tags(b.color),
            tags(a.color)
        ));
    }
    if b.extradata != a.extradata {
        return Some(format!(
            "{other}'s {} headers differ from {first}'s, so it was made by another encoder or with other settings",
            b.codec()
        ));
    }
    match (&a.audio, &b.audio) {
        (Some(_), None) => return Some(format!("{other} has no audio and {first} has")),
        (None, Some(_)) => return Some(format!("{other} has audio and {first} has none")),
        _ => {}
    }
    let order = |c: &Clip| (c.video, c.audio.as_ref().map(|s| s.track));
    if order(b) != order(a) {
        return Some(format!(
            "{other}'s streams are in another order than {first}'s"
        ));
    }
    let (Some(x), Some(y)) = (&a.audio, &b.audio) else {
        return None;
    };
    if y.codec != x.codec {
        return Some(format!(
            "{other}'s audio is {}, not {} like {first}'s",
            y.codec, x.codec
        ));
    }
    if y.rate != x.rate {
        return Some(format!(
            "{other}'s audio is {} Hz, not {} Hz like {first}'s",
            y.rate, x.rate
        ));
    }
    if y.channels != x.channels {
        return Some(format!(
            "{other}'s audio is {}, not {} like {first}'s",
            layout(y.channels),
            layout(x.channels)
        ));
    }
    if y.extradata != x.extradata {
        return Some(format!(
            "{other}'s {} audio headers differ from {first}'s",
            x.codec
        ));
    }
    None
}

#[derive(Clone, Copy)]
struct Timed {
    pts: i64,
    dts: i64,
    dur: i64,
    key: bool,
}

fn timed(demux: &Demux, track: usize, video: bool) -> Result<(TimeBase, Vec<Timed>)> {
    let shift = demux.shift(track);
    match demux {
        Demux::Mp4(r) => {
            let t = r
                .tracks
                .get(track)
                .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
            let tb = TimeBase::new(1, t.timescale.max(1));
            let out = t
                .samples
                .iter()
                .map(|s| Timed {
                    pts: s.pts.saturating_add(t.edit_offset).saturating_sub(shift),
                    dts: s.dts.saturating_add(t.edit_offset).saturating_sub(shift),
                    dur: s.duration as i64,
                    key: s.keyframe,
                })
                .collect();
            Ok((tb, out))
        }
        Demux::Mkv(r) => {
            let full = r.full()?;
            let t = full
                .tracks
                .get(track)
                .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
            let mut out: Vec<Timed> = t
                .samples
                .iter()
                .map(|s| Timed {
                    pts: s.pts.saturating_sub(shift),
                    dts: s.pts.saturating_sub(shift),
                    dur: s.duration as i64,
                    key: s.keyframe,
                })
                .collect();
            if video {
                let mut order: Vec<i64> = out.iter().map(|t| t.pts).collect();
                order.sort_unstable();
                let lag = order
                    .iter()
                    .zip(&out)
                    .map(|(o, t)| o - t.pts)
                    .max()
                    .unwrap_or(0)
                    .max(0);
                for (t, o) in out.iter_mut().zip(&order) {
                    t.dts = o - lag;
                }
            }
            Ok((t.time_base, out))
        }
    }
}

fn rescale(v: i64, from: TimeBase, to: TimeBase) -> i64 {
    if from == to {
        return v;
    }
    let n = v as i128 * from.num as i128 * to.den as i128;
    let d = (from.den as i128 * to.num as i128).max(1);
    let q = if n >= 0 {
        (n + d / 2) / d
    } else {
        (n - d / 2) / d
    };
    q.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn ticks_of(seconds: f64, tb: TimeBase) -> i64 {
    (seconds * tb.den as f64 / tb.num.max(1) as f64).round() as i64
}

#[derive(Clone, Copy)]
enum From {
    Clip {
        clip: usize,
        track: usize,
        index: usize,
    },
    Encoded {
        at: usize,
        len: usize,
    },
}

#[derive(Clone, Copy)]
struct Planned {
    pts: i64,
    dts: i64,
    key: bool,
    from: From,
}

struct Lane {
    tb: TimeBase,
    packets: Vec<Planned>,
}

struct VideoPlan {
    lane: Lane,
    starts: Vec<(i64, TimeBase)>,
    offsets: Vec<i64>,
    end: i64,
}

fn out_tb(tb: TimeBase, kind: Kind) -> TimeBase {
    if kind.matroska() || tb.num == 1 {
        tb
    } else {
        TimeBase::new(1, 90_000)
    }
}

fn plan_video(clips: &[Clip], kind: Kind) -> Result<VideoPlan> {
    let mut packets = Vec::new();
    let mut starts = Vec::new();
    let mut offsets = Vec::new();
    let mut offset = 0i64;
    let mut last: Option<i64> = None;
    let mut tb_out = None;
    for (c, clip) in clips.iter().enumerate() {
        let demux = clip.demux()?;
        let (tb, times) = timed(demux, clip.video, true).map_err(|e| about(&clip.name, e))?;
        let out = *tb_out.get_or_insert(out_tb(tb, kind));
        let Some(start) = times.iter().map(|t| t.pts).min() else {
            return Err(Error::Invalid(format!("{} has no video frames", clip.name)));
        };
        let (n, d) = clip.src.probe.fps;
        let step = if n > 0 && d > 0 {
            ticks_of(d as f64 / n as f64, tb).max(1)
        } else {
            1
        };
        let end = times
            .iter()
            .map(|t| t.pts.saturating_add(if t.dur > 0 { t.dur } else { step }))
            .max()
            .unwrap_or(start);
        for (i, t) in times.iter().enumerate() {
            let pts = rescale(t.pts - start, tb, out) + offset;
            let mut dts = rescale(t.dts - start, tb, out) + offset;
            if let Some(l) = last
                && dts <= l
            {
                dts = l + 1;
            }
            last = Some(dts);
            packets.push(Planned {
                pts,
                dts,
                key: t.key,
                from: From::Clip {
                    clip: c,
                    track: clip.video,
                    index: i,
                },
            });
        }
        starts.push((start, tb));
        offsets.push(offset);
        offset += rescale(end - start, tb, out);
    }
    Ok(VideoPlan {
        lane: Lane {
            tb: tb_out.unwrap_or(TimeBase::new(1, 90_000)),
            packets,
        },
        starts,
        offsets,
        end: offset,
    })
}

fn plan_audio(clips: &[Clip], video: &VideoPlan, kind: Kind) -> Result<Lane> {
    let mut per_clip: Vec<Vec<(Planned, i64)>> = Vec::new();
    let mut tb_out = None;
    for (c, clip) in clips.iter().enumerate() {
        let Some(sound) = &clip.audio else {
            per_clip.push(Vec::new());
            continue;
        };
        let demux = clip.demux()?;
        let (tb, times) = timed(demux, sound.track, false).map_err(|e| about(&clip.name, e))?;
        let out = *tb_out.get_or_insert(match (kind.matroska(), clip.matroska()) {
            (false, true) => TimeBase::new(1, sound.rate.max(1)),
            _ => out_tb(tb, kind),
        });
        let (vstart, vtb) = video.starts[c];
        let start = rescale(vstart, vtb, tb);
        let offset = rescale(video.offsets[c], video.lane.tb, out);
        let preroll = ticks_of(PREROLL, tb);
        let mut list = Vec::with_capacity(times.len());
        for (i, t) in times.iter().enumerate() {
            let rel = t.pts - start;
            if rel < -preroll {
                continue;
            }
            let at = rescale(rel, tb, out) + offset;
            let planned = Planned {
                pts: at,
                dts: at,
                key: t.key,
                from: From::Clip {
                    clip: c,
                    track: sound.track,
                    index: i,
                },
            };
            list.push((planned, rescale(t.dur, tb, out)));
        }
        list.sort_by_key(|p| p.0.dts);
        let mut last = 0;
        for k in 0..list.len() {
            if list[k].1 <= 0 {
                list[k].1 = match list.get(k + 1) {
                    Some(n) => n.0.pts - list[k].0.pts,
                    None => last,
                };
            }
            last = list[k].1;
        }
        per_clip.push(list);
    }
    let tb = tb_out.unwrap_or(TimeBase::new(1, RATE));
    let mut packets = Vec::new();
    let mut at: Option<i64> = None;
    for c in 0..per_clip.len() {
        let next = video.offsets.get(c + 1).copied().unwrap_or(video.end);
        let mut cut = rescale(next, video.lane.tb, tb);
        if let Some(first) = per_clip.get(c + 1).and_then(|l| l.first()) {
            cut = cut.min(first.0.pts);
        }
        for &(p, d) in &per_clip[c] {
            let d = d.max(1);
            let y = *at.get_or_insert(p.pts);
            if p.pts + d / 2 < y {
                continue;
            }
            if y + d > cut + d / 2 {
                break;
            }
            let y = if p.pts > y + d / 2 { p.pts } else { y };
            packets.push(Planned {
                pts: y,
                dts: y,
                ..p
            });
            at = Some(y + d);
        }
    }
    Ok(Lane { tb, packets })
}

struct Encoded {
    data: Vec<u8>,
    lane: Lane,
    codec: Codec,
    kbps: u32,
    extradata: Vec<u8>,
}

fn encode_audio(
    sources: &[(&Path, Option<usize>, f64)],
    lengths: &[f64],
    codec: Codec,
    kbps: u32,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<Encoded> {
    enum Enc {
        Aac(Box<crate::aac::Encoder>),
        Opus(Box<crate::opus::enc::Encoder>),
    }
    let mut enc = match codec {
        Codec::Opus => Enc::Opus(Box::new(crate::opus::enc::Encoder::new(
            crate::opus::enc::EncoderConfig::new(2, kbps * 1000),
        )?)),
        _ => Enc::Aac(Box::new(aac_encoder(kbps)?)),
    };
    let (delay, extradata) = match &enc {
        Enc::Aac(e) => (e.priming() as i64, e.extradata()),
        Enc::Opus(e) => (e.pre_skip() as i64, e.codec_private()),
    };
    let mut data = Vec::new();
    let mut packets = Vec::new();
    let keep = |got: Vec<Packet>, data: &mut Vec<u8>, packets: &mut Vec<Planned>| {
        for p in got {
            let at = data.len();
            data.extend_from_slice(&p.data);
            packets.push(Planned {
                pts: p.pts - delay,
                dts: p.pts - delay,
                key: true,
                from: From::Encoded {
                    at,
                    len: p.data.len(),
                },
            });
        }
    };
    let total: f64 = lengths.iter().sum();
    let mut done = 0.0;
    let mut at = 0usize;
    let mut buf = AudioBuffer::new(RATE, 2);
    for (&(path, ordinal, start), &length) in sources.iter().zip(lengths) {
        let end = ((done + length) * RATE as f64).round().max(0.0) as usize;
        let mut left = end.saturating_sub(at);
        at = end;
        done += length;
        let mut stream = match ordinal {
            Some(o) => Some(
                native_stream(path, o, start, RATE, 2)
                    .map_err(|e| about(&path.display().to_string(), e))?,
            ),
            None => None,
        };
        let mut pending: Vec<f32> = Vec::new();
        while left > 0 {
            let step = left.min(RATE as usize);
            buf.samples.clear();
            if let Some(s) = stream.as_mut() {
                while pending.len() < step * 2 {
                    match s.next() {
                        Some(b) => pending.extend_from_slice(&b?.samples),
                        None => {
                            stream = None;
                            break;
                        }
                    }
                }
                if stream.is_none() {
                    pending.resize(pending.len().max(step * 2), 0.0);
                }
                buf.samples.extend(pending.drain(..step * 2));
            } else {
                let n = pending.len().min(step * 2);
                buf.samples.extend(pending.drain(..n));
                buf.samples.resize(step * 2, 0.0);
            }
            let got = match &mut enc {
                Enc::Aac(e) => e.encode(&buf)?,
                Enc::Opus(e) => e.encode(&buf)?,
            };
            keep(got, &mut data, &mut packets);
            left -= step;
            if total > 0.0
                && !progress((done - left as f64 / RATE as f64).max(0.0) as f32 / total as f32)
            {
                return Err(Error::Cancelled);
            }
        }
    }
    let got = match &mut enc {
        Enc::Aac(e) => e.flush(),
        Enc::Opus(e) => e.flush()?,
    };
    keep(got, &mut data, &mut packets);
    let end = at as i64;
    packets.retain(|p| p.pts < end);
    Ok(Encoded {
        data,
        lane: Lane {
            tb: TimeBase::new(1, RATE),
            packets,
        },
        codec,
        kbps,
        extradata,
    })
}

enum Writer {
    Mp4(mp4::Writer),
    Mkv(mkv::Writer),
}

fn vp9_entry(clip: &Clip) -> Result<SampleEntry> {
    let demux = clip.demux()?;
    let mut buf = Vec::new();
    let mut info = None;
    for k in 0..demux.samples(clip.video) {
        if demux.sample(clip.video, k).is_some_and(|s| s.2) {
            demux.read_into(clip.video, k, &mut buf)?;
            if let Some(i) = crate::vp9::keyframe_info(&buf) {
                info = Some(i);
                break;
            }
        }
    }
    let k = info
        .ok_or_else(|| Error::Invalid(format!("{}: the VP9 video has no keyframe", clip.name)))?;
    let chroma = match (k.subsampling_x, k.subsampling_y) {
        (true, false) => 2,
        (false, false) => 3,
        _ => 1,
    };
    let p = &clip.src.probe;
    let config = VpcConfig::new(
        (k.profile, k.bit_depth as u8, chroma),
        (k.color.color_space, k.color.full_range),
        (p.width, p.height),
        p.fps,
    );
    Ok(SampleEntry::vp9(config, p.width as u16, p.height as u16))
}

fn mp4_video(clip: &Clip, tb: TimeBase) -> Result<mp4::TrackConfig> {
    if let Some(Demux::Mp4(r)) = &clip.demux {
        let t = r
            .tracks
            .get(clip.video)
            .ok_or_else(|| Error::Invalid(format!("{}: no video track", clip.name)))?;
        let mut cfg = mp4::TrackConfig::from_track(t)?;
        cfg.timescale = tb.den;
        return Ok(cfg);
    }
    let p = &clip.src.probe;
    let (w, h) = (p.width as u16, p.height as u16);
    let entry = match p.video_codec {
        Some(Codec::H264) => SampleEntry::h264(AvcConfig::parse(&clip.extradata)?, w, h),
        Some(Codec::Hevc) => SampleEntry::hevc(HevcConfig::parse(&clip.extradata)?, w, h),
        Some(Codec::Vp9) => vp9_entry(clip)?,
        _ => {
            return Err(Error::Unsupported(format!(
                "copying {} video into MP4",
                clip.codec()
            )));
        }
    };
    let mut cfg = mp4::TrackConfig::new(entry, tb.den);
    cfg.matrix = mp4::rotation_matrix(p.rotation);
    Ok(cfg)
}

fn mkv_video(clip: &Clip, tb: TimeBase) -> Result<mkv::TrackConfig> {
    match &clip.demux {
        Some(Demux::Mkv(r)) => {
            let t = r
                .tracks
                .get(clip.video)
                .ok_or_else(|| Error::Invalid(format!("{}: no video track", clip.name)))?;
            Ok(mkv::TrackConfig::from_track(t))
        }
        Some(Demux::Mp4(r)) => {
            let t = r
                .tracks
                .get(clip.video)
                .ok_or_else(|| Error::Invalid(format!("{}: no video track", clip.name)))?;
            let mut cfg = match clip.src.probe.video_codec {
                Some(Codec::Vp9) => {
                    mkv::TrackConfig::vp9(clip.src.probe.width, clip.src.probe.height, tb)
                }
                _ => mkv::TrackConfig::from_mp4(t)?,
            };
            cfg.rotation = clip.src.probe.rotation;
            cfg.time_base = tb;
            Ok(cfg)
        }
        None => Err(Error::Unsupported(format!(
            "{}: our MP4 and MKV readers can't open it",
            clip.name
        ))),
    }
}

fn mp4_audio(clip: &Clip, tb: TimeBase) -> Result<mp4::TrackConfig> {
    let sound = clip
        .audio
        .as_ref()
        .ok_or_else(|| Error::Invalid(format!("{} has no audio", clip.name)))?;
    if let Some(Demux::Mp4(r)) = &clip.demux {
        let t = r
            .tracks
            .get(sound.track)
            .ok_or_else(|| Error::Invalid(format!("{}: no audio track", clip.name)))?;
        let mut cfg = mp4::TrackConfig::from_track(t)?;
        cfg.timescale = tb.den;
        return Ok(cfg);
    }
    let entry = SampleEntry::aac(
        sound.extradata.clone(),
        sound.channels as u32,
        sound.rate,
        0,
    );
    Ok(mp4::TrackConfig::new(entry, tb.den))
}

fn mkv_audio(clip: &Clip, tb: TimeBase) -> Result<mkv::TrackConfig> {
    let sound = clip
        .audio
        .as_ref()
        .ok_or_else(|| Error::Invalid(format!("{} has no audio", clip.name)))?;
    match &clip.demux {
        Some(Demux::Mkv(r)) => {
            let t = r
                .tracks
                .get(sound.track)
                .ok_or_else(|| Error::Invalid(format!("{}: no audio track", clip.name)))?;
            Ok(mkv::TrackConfig::from_track(t))
        }
        Some(Demux::Mp4(r)) => {
            let t = r
                .tracks
                .get(sound.track)
                .ok_or_else(|| Error::Invalid(format!("{}: no audio track", clip.name)))?;
            let mut cfg = mkv::TrackConfig::from_mp4(t)?;
            cfg.time_base = tb;
            Ok(cfg)
        }
        None => Err(Error::Unsupported(format!(
            "{}: our MP4 and MKV readers can't open it",
            clip.name
        ))),
    }
}

enum Audio<'a> {
    None,
    Copy(Lane),
    Encoded(&'a Encoded),
}

fn nanos(ticks: i64, tb: TimeBase) -> u64 {
    (ticks.max(0) as i128 * tb.num as i128 * 1_000_000_000 / tb.den.max(1) as i128)
        .clamp(0, u64::MAX as i128) as u64
}

#[allow(clippy::too_many_arguments)]
fn write(
    clips: &[Clip],
    kind: Kind,
    part: &Path,
    video: &Lane,
    audio: &Audio,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<()> {
    let first = &clips[0];
    let alane: Option<&Lane> = match audio {
        Audio::None => None,
        Audio::Copy(l) => Some(l),
        Audio::Encoded(e) => Some(&e.lane),
    };
    let lead = |l: &Lane| l.packets.first().map_or(0, |p| p.dts.min(0));
    let mut w = if kind.matroska() {
        let options = if kind == Kind::Webm {
            mkv::Options::webm()
        } else {
            mkv::Options::default()
        };
        let mut w = mkv::Writer::create_with(part, options)?;
        w.add_track(mkv_video(first, video.tb)?)?;
        if let Some(l) = alane {
            let mut cfg = match audio {
                Audio::Encoded(e) if e.codec == Codec::Opus => {
                    mkv::TrackConfig::opus(e.extradata.clone())
                }
                Audio::Encoded(e) => mkv::TrackConfig::aac(e.extradata.clone(), RATE, 2),
                _ => {
                    let source = clips.iter().find(|c| c.audio.is_some()).unwrap_or(first);
                    mkv_audio(source, l.tb)?
                }
            };
            cfg.time_base = l.tb;
            let need = nanos(-lead(l), l.tb);
            cfg.codec_delay = cfg.codec_delay.max(need);
            w.add_track(cfg)?;
        }
        Writer::Mkv(w)
    } else {
        let brand = if kind == Kind::Mov {
            mp4::Brand::Mov
        } else {
            mp4::Brand::Mp4
        };
        let mut w = mp4::Writer::create_with(part, mp4::Options { brand, reserve: 0 })?;
        w.add_track(mp4_video(first, video.tb)?)?;
        if let Some(l) = alane {
            let mut cfg = match audio {
                Audio::Encoded(e) => mp4::TrackConfig::new(
                    SampleEntry::aac(e.extradata.clone(), 2, RATE, e.kbps * 1000),
                    RATE,
                ),
                _ => {
                    let source = clips.iter().find(|c| c.audio.is_some()).unwrap_or(first);
                    mp4_audio(source, l.tb)?
                }
            };
            cfg.edits.clear();
            let lead = -lead(l);
            cfg.media_start = (lead > 0).then_some(lead);
            w.add_track(cfg)?;
        }
        Writer::Mp4(w)
    };
    let empty = Lane {
        tb: video.tb,
        packets: Vec::new(),
    };
    let a = alane.unwrap_or(&empty);
    let total = (video.packets.len() + a.packets.len()).max(1);
    let (mut i, mut j) = (0usize, 0usize);
    let mut pk = Packet {
        data: Vec::new(),
        pts: 0,
        dts: 0,
        keyframe: false,
    };
    while i < video.packets.len() || j < a.packets.len() {
        let take_video = j >= a.packets.len()
            || (i < video.packets.len()
                && video.tb.seconds(video.packets[i].dts) <= a.tb.seconds(a.packets[j].dts));
        let (p, track) = if take_video {
            i += 1;
            (&video.packets[i - 1], 0usize)
        } else {
            j += 1;
            (&a.packets[j - 1], 1usize)
        };
        match p.from {
            From::Clip { clip, track, index } => {
                clips[clip].demux()?.read_into(track, index, &mut pk.data)?;
            }
            From::Encoded { at, len } => {
                let Audio::Encoded(e) = audio else {
                    return Err(Error::Backend("encoded audio went missing".into()));
                };
                let bytes = e
                    .data
                    .get(at..at + len)
                    .ok_or_else(|| Error::Backend("encoded audio went missing".into()))?;
                pk.data.clear();
                pk.data.extend_from_slice(bytes);
            }
        }
        pk.pts = p.pts;
        pk.dts = p.dts;
        pk.keyframe = p.key;
        match &mut w {
            Writer::Mp4(w) => w.push(track, &pk)?,
            Writer::Mkv(w) => w.push_sample(track, &pk.data, p.pts, None, p.key)?,
        }
        let n = i + j;
        if n % TELL_EVERY == 0 && !progress(n as f32 / total as f32 * WRITTEN) {
            return Err(Error::Cancelled);
        }
    }
    match w {
        Writer::Mp4(w) => {
            let mut stop = false;
            w.finish_faststart_with(|n, of| {
                let go = progress(WRITTEN + (1.0 - WRITTEN) * n as f32 / of.max(1) as f32);
                stop |= !go;
                go
            })?;
            if stop {
                return Err(Error::Cancelled);
            }
        }
        Writer::Mkv(w) => {
            w.finish()?;
        }
    }
    Ok(())
}

fn partial(out: &Path) -> PathBuf {
    let ext = extension(out);
    out.with_extension(format!("partial.{ext}"))
}

fn finish(part: &Path, out: &Path, done: Result<()>) -> Result<()> {
    if let Err(e) = done {
        let _ = std::fs::remove_file(part);
        return Err(e);
    }
    std::fs::rename(part, out).map_err(|e| Error::at(out, e))
}

fn joining(done: f32, encoder: Route) -> Progress {
    Progress {
        stage: Stage::Joining,
        done: done.clamp(0.0, 1.0),
        encoder,
    }
}

fn copy(
    clips: &[Clip],
    out: &Path,
    kind: Kind,
    opts: &JoinOptions,
    reason: String,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<Joined> {
    let first = &clips[0];
    let plan = plan_video(clips, kind)?;
    let heard = first.audio.is_some();
    let carried = first
        .audio
        .as_ref()
        .is_some_and(|a| kind.audio(&a.codec, first.matroska()));
    let seconds = plan.lane.tb.seconds(plan.end);
    let part = partial(out);
    let (encoded, audio_report) = if heard && (!carried || opts.audio_kbps.is_some()) {
        let codec = if kind == Kind::Webm {
            Codec::Opus
        } else {
            Codec::Aac
        };
        let kbps = opts.audio_kbps.unwrap_or(if codec == Codec::Opus {
            OPUS_KBPS
        } else {
            AAC_KBPS
        });
        let sources: Vec<(&Path, Option<usize>, f64)> = clips
            .iter()
            .map(|c| (c.path.as_path(), c.audio.as_ref().map(|a| a.ordinal), 0.0))
            .collect();
        let lengths: Vec<f64> = (0..clips.len())
            .map(|c| {
                let next = plan.offsets.get(c + 1).copied().unwrap_or(plan.end);
                plan.lane.tb.seconds(next - plan.offsets[c])
            })
            .collect();
        let e = encode_audio(&sources, &lengths, codec, kbps, &mut |f| {
            progress(&joining(f * 0.5 * WRITTEN, Route::Cpu))
        })?;
        (Some(e), JoinAudio::Encoded { codec, kbps })
    } else if heard {
        (None, JoinAudio::Copied)
    } else {
        (None, JoinAudio::Silent)
    };
    let audio = match (&encoded, audio_report) {
        (Some(e), _) => Audio::Encoded(e),
        (None, JoinAudio::Copied) => Audio::Copy(plan_audio(clips, &plan, kind)?),
        _ => Audio::None,
    };
    let base = if encoded.is_some() {
        0.5 * WRITTEN
    } else {
        0.0
    };
    let done = write(clips, kind, &part, &plan.lane, &audio, &mut |f| {
        progress(&joining(base + (1.0 - base) * f, Route::Cpu))
    });
    finish(&part, out, done)?;
    let video = first.src.probe.video_codec.unwrap_or(Codec::H264);
    Ok(Joined {
        path: out.to_path_buf(),
        method: JoinMethod::Copy,
        reason,
        clips: clips.len(),
        size: (first.src.probe.width, first.src.probe.height),
        fps: first.src.probe.fps,
        frames: plan.lane.packets.len() as u64,
        seconds,
        video,
        route: Route::Cpu,
        audio: audio_report,
        reasons: Vec::new(),
    })
}

fn readable(clip: &Clip) -> Result<()> {
    let c = clip.color;
    if matches!(c.transfer, 16 | 18) || matches!(c.primaries, 9 | 11 | 12) {
        return Err(Error::Unsupported(format!(
            "{}: the source is HDR or wide gamut ({}, {}): the join writes SDR BT.709 and does not tone-map",
            clip.name,
            primaries_name(c.primaries),
            transfer_name(c.transfer)
        )));
    }
    if !matches!(c.matrix, 0 | 1 | 2 | 5 | 6) {
        return Err(Error::Unsupported(format!(
            "{}: the source's colour matrix is {}: the join reads BT.709 and BT.601 sources",
            clip.name,
            matrix_name(c.matrix)
        )));
    }
    Ok(())
}

fn encode(
    clips: &[Clip],
    out: &Path,
    kind: Kind,
    opts: &JoinOptions,
    reason: String,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<Joined> {
    for clip in clips {
        readable(clip)?;
    }
    let first = &clips[0];
    let shown = display(&first.src, first.sar);
    let size = match opts.size {
        Some((w, h)) => (w & !1, h & !1),
        None => (shown.0 & !1, shown.1 & !1),
    };
    if size.0 == 0 || size.1 == 0 {
        return Err(Error::Invalid(format!(
            "{}'s shape {}x{} cannot make an even output",
            first.name, shown.0, shown.1
        )));
    }
    let fps = opts.fps.unwrap_or(first.src.probe.fps);
    if fps.0 == 0 || fps.1 == 0 {
        return Err(Error::Invalid(format!(
            "a frame rate of {}/{}",
            fps.0, fps.1
        )));
    }
    let mut reasons = Vec::new();
    let video = if kind == Kind::Webm && opts.video != Codec::Vp9 {
        reasons.push(format!(
            "WebM carries VP9, not {}, so the join encodes VP9",
            opts.video
        ));
        Codec::Vp9
    } else if kind == Kind::Mov && opts.video == Codec::Vp9 {
        reasons.push("MOV doesn't carry VP9, so the join encodes H.264".to_string());
        Codec::H264
    } else {
        opts.video
    };
    let mut pieces = Vec::with_capacity(clips.len());
    for clip in clips {
        let d = clip.src.probe.duration;
        if !d.is_finite() || d <= 0.0 {
            return Err(Error::Invalid(format!(
                "{} has no known length to join",
                clip.name
            )));
        }
        pieces.push(Piece {
            path: clip.path.clone(),
            from: 0.0,
            to: d,
        });
    }
    let lengths: Vec<f64> = pieces
        .iter()
        .map(|p| {
            let frames = frame_at(p.to, fps) - frame_at(p.from, fps);
            frames.max(0) as f64 * fps.1 as f64 / fps.0 as f64
        })
        .collect();
    let tracks = clips
        .iter()
        .map(|c| c.src.probe.audio.len())
        .max()
        .unwrap_or(0);
    let mut gains = vec![0.0f32; tracks.max(1)];
    gains[0] = 1.0;
    let aac = opts.audio_kbps.unwrap_or(AAC_KBPS).clamp(32, 512);
    let after = kind != Kind::Mp4;
    let temp = out.with_extension(format!("joining.{}.mp4", extension(out)));
    let scale = if after { 0.9 } else { 1.0 };
    let exported = export_fitted(
        &temp,
        &pieces,
        ExportOptions {
            video,
            crf: opts.crf,
            audio_kbps: aac,
            audio_gains: gains,
            overlay: None,
            prefs: opts.prefs.clone(),
        },
        Fit { size, fps },
        |p| {
            progress(&Progress {
                stage: p.stage,
                done: p.done * scale,
                encoder: p.encoder,
            })
        },
    );
    let exported = match exported {
        Ok(e) => e,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    };
    let route = exported.video.route;
    reasons.extend(exported.reasons.iter().cloned());
    let audio = if kind == Kind::Webm {
        JoinAudio::Encoded {
            codec: Codec::Opus,
            kbps: opts.audio_kbps.unwrap_or(OPUS_KBPS),
        }
    } else {
        JoinAudio::Encoded {
            codec: Codec::Aac,
            kbps: aac,
        }
    };
    let done = if after {
        rewrap(
            clips,
            &temp,
            out,
            kind,
            opts,
            &lengths,
            &vec![0.0; clips.len()],
            false,
            route,
            progress,
        )
    } else {
        std::fs::rename(&temp, out).map_err(|e| Error::at(out, e))
    };
    let _ = std::fs::remove_file(&temp);
    done?;
    Ok(Joined {
        path: out.to_path_buf(),
        method: JoinMethod::Encode,
        reason,
        clips: clips.len(),
        size,
        fps,
        frames: exported.frames,
        seconds: lengths.iter().sum(),
        video: exported.video.codec,
        route,
        audio,
        reasons,
    })
}

#[allow(clippy::too_many_arguments)]
fn rewrap(
    clips: &[Clip],
    temp: &Path,
    out: &Path,
    kind: Kind,
    opts: &JoinOptions,
    lengths: &[f64],
    starts: &[f64],
    silent: bool,
    route: Route,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<()> {
    let made = Clip::load(temp, &opts.prefs)?;
    let one = std::slice::from_ref(&made);
    let plan = plan_video(one, kind)?;
    let part = partial(out);
    let mut tell = |f: f32| progress(&joining(0.9 + 0.1 * f, route));
    let encoded = if kind == Kind::Webm && !silent {
        let sources: Vec<(&Path, Option<usize>, f64)> = clips
            .iter()
            .zip(starts)
            .map(|(c, s)| (c.path.as_path(), c.audio.as_ref().map(|a| a.ordinal), *s))
            .collect();
        let kbps = opts.audio_kbps.unwrap_or(OPUS_KBPS);
        Some(encode_audio(
            &sources,
            lengths,
            Codec::Opus,
            kbps,
            &mut |f| tell(f * 0.5),
        )?)
    } else {
        None
    };
    let audio = match &encoded {
        Some(e) => Audio::Encoded(e),
        None if made.audio.is_some() && !silent => Audio::Copy(plan_audio(one, &plan, kind)?),
        None => Audio::None,
    };
    let base = if encoded.is_some() { 0.5 } else { 0.0 };
    let done = write(one, kind, &part, &plan.lane, &audio, &mut |f| {
        tell(base + (1.0 - base) * f)
    });
    finish(&part, out, done)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cut {
    pub path: PathBuf,
    pub from: f64,
    pub to: f64,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub frames: u64,
    pub copied: u64,
    pub encoded: u64,
    pub smart: bool,
    pub video: Codec,
    pub route: Route,
    pub audio: JoinAudio,
    pub reasons: Vec<String>,
}

pub fn track_length(seconds: Option<f64>, frames: Option<u64>, fps: (u32, u32)) -> Option<f64> {
    let (n, d) = fps;
    let own = frames
        .filter(|&f| f > 0 && n > 0 && d > 0)
        .map(|f| f as f64 * f64::from(d) / f64::from(n));
    match (seconds.filter(|s| s.is_finite() && *s > 0.0), own) {
        (Some(s), Some(o)) => Some(s.min(o)),
        (s, o) => s.or(o),
    }
}

fn video_length(probe: &super::probe::Probe) -> f64 {
    track_length(Some(probe.duration), probe.frames, probe.fps).unwrap_or(probe.duration)
}

pub fn first_frame_at(seconds: f64, fps: (u32, u32)) -> i64 {
    let frames = seconds * fps.0 as f64 / fps.1.max(1) as f64;
    (frames - 1e-6).ceil().clamp(-9e15, 9e15) as i64
}

fn unfit_copy(kind: Kind, source: Option<Codec>, matroska: bool) -> Option<String> {
    if !matches!(kind, Kind::Mp4 | Kind::Mov) {
        return None;
    }
    match source {
        Some(Codec::Vp9) => Some("MP4 and MOV get H.264 or HEVC, not VP9, so the cut encodes the whole window".to_string()),
        Some(_) if matroska => Some("a Matroska source is re-encoded into MP4 or MOV, as a plain convert does, so the cut encodes the whole window".to_string()),
        _ => None,
    }
}

pub fn cut(
    input: &Path,
    from: f64,
    to: f64,
    out: &Path,
    opts: JoinOptions,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Cut> {
    let kind = Kind::of(out, "a cut")?;
    if same_file(input, out) {
        return Err(Error::Invalid(format!(
            "{} is both the source and the output",
            input.display()
        )));
    }
    if !from.is_finite() || from < 0.0 || to.is_nan() || to <= from {
        return Err(Error::Invalid(format!("a range from {from} to {to}")));
    }
    if let Some(kbps) = opts.audio_kbps
        && !(6..=512).contains(&kbps)
    {
        return Err(Error::Invalid(format!(
            "an audio bitrate of {kbps} kbps, not 6 to 512"
        )));
    }
    let clip = Clip::load(input, &opts.prefs)?;
    if let (Some(kbps), None) = (opts.audio_kbps, &clip.audio) {
        return Err(Error::Invalid(format!(
            "{}: the source has no audio to encode at {kbps} kbps",
            clip.name
        )));
    }
    let length = video_length(&clip.src.probe);
    let known = length.is_finite() && length > 0.0;
    if known && from >= length {
        return Err(Error::Invalid(format!(
            "{}: {from:.3} s is at or past the end of the video ({length:.3} s)",
            clip.name
        )));
    }
    let to = if known { to.min(length) } else { to };
    if !to.is_finite() {
        return Err(Error::Invalid(format!(
            "{} has no known length to cut to",
            clip.name
        )));
    }
    let source = clip.src.probe.video_codec;
    let mut reasons = Vec::new();
    let foreign = unfit_copy(kind, source, clip.matroska());
    let fit = opts.encode || opts.size.is_some() || opts.fps.is_some() || foreign.is_some();
    reasons.extend(foreign);
    let fps = opts.fps.unwrap_or(clip.src.probe.fps);
    if fps.0 == 0 || fps.1 == 0 {
        return Err(Error::Invalid(format!(
            "a frame rate of {}/{}",
            fps.0, fps.1
        )));
    }
    let (a, b) = (first_frame_at(from, fps), first_frame_at(to, fps));
    if b <= a {
        return Err(Error::Invalid(format!(
            "{}: no frame starts between {from:.3} s and {to:.3} s",
            clip.name
        )));
    }
    let at = |f: i64| f as f64 * fps.1 as f64 / fps.0 as f64;
    let piece = Piece {
        path: input.to_path_buf(),
        from: at(a),
        to: at(b),
    };
    let video = match source {
        Some(c @ (Codec::H264 | Codec::Hevc | Codec::Vp9)) if !fit && kind.video(source) => c,
        _ if kind == Kind::Webm && opts.video != Codec::Vp9 => {
            if fit {
                reasons.push(format!(
                    "WebM carries VP9, not {}, so the cut encodes VP9",
                    source.unwrap_or(opts.video)
                ));
            }
            Codec::Vp9
        }
        _ if kind == Kind::Mov && opts.video == Codec::Vp9 => {
            if fit {
                reasons.push("MOV doesn't carry VP9, so the cut encodes H.264".to_string());
            }
            Codec::H264
        }
        _ => opts.video,
    };
    let shown = display(&clip.src, clip.sar);
    let size = match (fit, opts.size) {
        (true, Some((w, h))) => (w & !1, h & !1),
        (true, None) => (shown.0 & !1, shown.1 & !1),
        (false, _) => (clip.src.probe.width & !1, clip.src.probe.height & !1),
    };
    if size.0 == 0 || size.1 == 0 {
        return Err(Error::Invalid(format!(
            "{}'s shape {}x{} cannot make an even output",
            clip.name, shown.0, shown.1
        )));
    }
    if fit {
        readable(&clip)?;
    }
    let tracks = clip.src.probe.audio.len();
    let mut gains = vec![0.0f32; tracks.max(1)];
    gains[0] = 1.0;
    let aac = opts.audio_kbps.unwrap_or(AAC_KBPS).clamp(32, 512);
    let silent = clip.audio.is_none();
    let after = kind != Kind::Mp4 || silent;
    let temp = out.with_extension(format!("cutting.{}.mp4", extension(out)));
    let scale = if after { 0.9 } else { 1.0 };
    let options = ExportOptions {
        video,
        crf: opts.crf,
        audio_kbps: aac,
        audio_gains: gains,
        overlay: None,
        prefs: opts.prefs.clone(),
    };
    let pieces = std::slice::from_ref(&piece);
    let tell = |p: &Progress| {
        progress(&Progress {
            stage: p.stage,
            done: p.done * scale,
            encoder: p.encoder,
        })
    };
    let exported = if fit {
        export_fitted(&temp, pieces, options, Fit { size, fps }, tell)
    } else {
        export(&temp, pieces, options, tell)
    };
    let exported = match exported {
        Ok(e) => e,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    };
    let route = exported.video.route;
    reasons.extend(exported.reasons.iter().cloned());
    let audio = if silent {
        JoinAudio::Silent
    } else if kind == Kind::Webm {
        JoinAudio::Encoded {
            codec: Codec::Opus,
            kbps: opts.audio_kbps.unwrap_or(OPUS_KBPS),
        }
    } else {
        JoinAudio::Encoded {
            codec: Codec::Aac,
            kbps: aac,
        }
    };
    let done = if after {
        rewrap(
            std::slice::from_ref(&clip),
            &temp,
            out,
            kind,
            &opts,
            &[at(b - a)],
            &[piece.from],
            silent,
            route,
            &mut progress,
        )
    } else {
        std::fs::rename(&temp, out).map_err(|e| Error::at(out, e))
    };
    let _ = std::fs::remove_file(&temp);
    done?;
    Ok(Cut {
        path: out.to_path_buf(),
        from: piece.from,
        to: piece.to,
        size,
        fps,
        frames: exported.frames,
        copied: exported.copied,
        encoded: exported.encoded,
        smart: exported.smart,
        video: exported.video.codec,
        route,
        audio,
        reasons,
    })
}

#[cfg(test)]
mod tests {
    use super::super::probe::Probe;
    use super::*;

    fn probe(duration: f64, fps: (u32, u32), frames: Option<u64>) -> Probe {
        Probe {
            duration,
            start: 0.0,
            video_start: Some(0.0),
            fps,
            width: 352,
            height: 240,
            rotation: 0,
            video_codec: Some(Codec::H264),
            video_codec_name: "h264".into(),
            profile: String::new(),
            bit_depth: 8,
            frames,
            audio: Vec::new(),
            alpha: false,
        }
    }

    #[test]
    fn first_frame_at_snaps_up_to_the_next_frame() {
        assert_eq!(first_frame_at(0.0, (24, 1)), 0);
        assert_eq!(first_frame_at(1.0, (24, 1)), 24);
        assert_eq!(first_frame_at(1.01, (24, 1)), 25);
        assert_eq!(first_frame_at(1.0 / 24.0 * 3.0, (24, 1)), 3);
        assert_eq!(first_frame_at(2.0, (30000, 1001)), 60);
        assert_eq!(first_frame_at(2.0 * 1001.0 / 30000.0, (30000, 1001)), 2);
        assert_eq!(first_frame_at(3.5, (25, 1)), 88);
    }

    #[test]
    fn first_frame_at_survives_a_zero_denominator_and_huge_times() {
        assert_eq!(first_frame_at(1.0, (24, 0)), 24);
        assert_eq!(first_frame_at(f64::MAX, (24, 1)), 9e15 as i64);
        assert_eq!(first_frame_at(-f64::MAX, (24, 1)), -9e15 as i64);
    }

    #[test]
    fn a_cut_copies_into_mp4_and_mov_only_what_a_plain_convert_copies() {
        for kind in [Kind::Mp4, Kind::Mov] {
            assert_eq!(unfit_copy(kind, Some(Codec::H264), false), None);
            assert_eq!(unfit_copy(kind, Some(Codec::Hevc), false), None);
            assert!(unfit_copy(kind, Some(Codec::Vp9), false).is_some());
            assert!(unfit_copy(kind, Some(Codec::Vp9), true).is_some());
            assert!(unfit_copy(kind, Some(Codec::H264), true).is_some());
        }
        for kind in [Kind::Mkv, Kind::Webm] {
            for codec in [Codec::H264, Codec::Hevc, Codec::Vp9] {
                assert_eq!(unfit_copy(kind, Some(codec), true), None);
                assert_eq!(unfit_copy(kind, Some(codec), false), None);
            }
        }
    }

    #[test]
    fn video_length_takes_the_video_track_not_a_longer_audio_track() {
        assert_eq!(video_length(&probe(5.5, (24, 1), Some(120))), 5.0);
        assert_eq!(video_length(&probe(4.0, (24, 1), Some(120))), 4.0);
        assert_eq!(video_length(&probe(5.5, (24, 1), None)), 5.5);
        assert_eq!(video_length(&probe(5.5, (24, 1), Some(0))), 5.5);
        assert_eq!(video_length(&probe(f64::NAN, (24, 1), Some(48))), 2.0);
        assert_eq!(video_length(&probe(5.5, (0, 1), Some(120))), 5.5);
    }

    #[test]
    fn a_cut_refuses_a_backwards_or_empty_range_before_reading_the_source() {
        let missing = Path::new("tmp/does-not-exist.mp4");
        let out = Path::new("tmp/out.mp4");
        for (from, to) in [
            (2.0, 2.0),
            (3.0, 2.0),
            (-1.0, 2.0),
            (f64::NAN, 2.0),
            (1.0, f64::NAN),
        ] {
            let e = cut(missing, from, to, out, JoinOptions::default(), |_| true).unwrap_err();
            assert!(matches!(e, Error::Invalid(_)), "{from} {to}: {e}");
        }
    }

    #[test]
    fn a_cut_refuses_an_output_it_cannot_write() {
        let e = cut(
            Path::new("tmp/a.mp4"),
            0.0,
            1.0,
            Path::new("tmp/a.gif"),
            JoinOptions::default(),
            |_| true,
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("a cut writes mp4, mov, mkv or webm"),
            "{e}"
        );
    }
}
