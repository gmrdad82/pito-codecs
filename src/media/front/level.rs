use std::fs::File;
use std::path::{Path, PathBuf};

use crate::filter::{Limiter, Loudness, LoudnessMeter};
use crate::media::{AudioBuffer, Codec, Error, Packet, Result};
use crate::mkv;
use crate::mp4::{self, AvcConfig, Brand, HevcConfig, SampleEntry};

use super::demux::Demux;
use super::probe::probe;
use super::route::Prefs;
use super::stream::{AudioSpan, decode_audio_span};

pub const LEVEL_RATE: u32 = 48_000;
pub const YOUTUBE_LUFS: f64 = -14.0;
pub const YOUTUBE_TRUE_PEAK: f64 = -2.0;
pub const DEFAULT_TRUE_PEAK: f64 = -1.0;
pub const AAC_KBPS: u32 = 320;
pub const OPUS_KBPS: u32 = 128;
pub const QUIETEST_LUFS: f64 = -70.0;
pub const LOUDEST_LUFS: f64 = -5.0;
pub const LOWEST_TRUE_PEAK: f64 = -9.0;
pub const HIGHEST_TRUE_PEAK: f64 = 0.0;
const CLOSE: f64 = 0.1;
const TRIES: usize = 3;
const RETRIES: usize = 8;
const NARROWS: usize = 4;
const MARGIN: f64 = 0.05;
const SLACK: f64 = 0.05;
const HEADROOM: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelTarget {
    pub integrated: f64,
    pub true_peak: f64,
}

impl LevelTarget {
    pub fn new(integrated: f64) -> LevelTarget {
        LevelTarget {
            integrated,
            true_peak: DEFAULT_TRUE_PEAK,
        }
    }

    pub fn youtube() -> LevelTarget {
        LevelTarget {
            integrated: YOUTUBE_LUFS,
            true_peak: YOUTUBE_TRUE_PEAK,
        }
    }

    pub fn check(&self) -> Result<()> {
        if !(QUIETEST_LUFS..=LOUDEST_LUFS).contains(&self.integrated) {
            return Err(Error::Invalid(format!(
                "loudness {} LUFS is outside {QUIETEST_LUFS} to {LOUDEST_LUFS}",
                self.integrated
            )));
        }
        if !(LOWEST_TRUE_PEAK..=HIGHEST_TRUE_PEAK).contains(&self.true_peak) {
            return Err(Error::Invalid(format!(
                "true peak {} dBTP is outside {LOWEST_TRUE_PEAK} to {HIGHEST_TRUE_PEAK}",
                self.true_peak
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessStats {
    pub integrated: f64,
    pub range: f64,
    pub true_peak: f64,
    pub threshold: f64,
}

impl LoudnessStats {
    fn of(meter: &LoudnessMeter) -> LoudnessStats {
        LoudnessStats {
            integrated: meter.integrated(),
            range: meter.range(),
            true_peak: meter.true_peak(),
            threshold: meter.threshold(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelMode {
    Linear,
    Limited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelStage {
    Measuring,
    Calibrating,
    Levelling,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelProgress {
    pub stage: LevelStage,
    pub done: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LevelStyle {
    pub linear: bool,
    pub encoded: Option<(Codec, u32)>,
}

#[derive(Debug, Clone)]
pub struct LevelOptions {
    pub target: LevelTarget,
    pub audio_kbps: Option<u32>,
    pub track: usize,
    pub prefs: Prefs,
}

impl Default for LevelOptions {
    fn default() -> Self {
        LevelOptions {
            target: LevelTarget::youtube(),
            audio_kbps: None,
            track: 0,
            prefs: Prefs::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Levelled {
    pub path: PathBuf,
    pub before: LoudnessStats,
    pub after: LoudnessStats,
    pub mode: LevelMode,
    pub gain_db: f64,
    pub audio: Codec,
    pub audio_kbps: u32,
    pub video: Option<Codec>,
    pub video_packets: u64,
}

#[derive(Debug, Clone)]
pub struct Leveler {
    gain: f32,
    gain_db: f64,
    limiter: Option<Limiter>,
    skip: usize,
    channels: usize,
    out: AudioBuffer,
}

impl Leveler {
    pub fn new(
        rate: u32,
        channels: u16,
        measured: &LoudnessStats,
        target: &LevelTarget,
    ) -> Result<Leveler> {
        let gain_db = target.integrated - measured.integrated;
        if !gain_db.is_finite() {
            return Err(silent(target));
        }
        let top = target.true_peak - HEADROOM;
        let ceiling = (measured.true_peak + gain_db > top).then_some(top);
        Leveler::with_gain(rate, channels, gain_db, ceiling)
    }

    pub fn with_gain(
        rate: u32,
        channels: u16,
        gain_db: f64,
        ceiling_db: Option<f64>,
    ) -> Result<Leveler> {
        if rate == 0 || channels == 0 || !gain_db.is_finite() {
            return Err(Error::Invalid(format!(
                "levelling {channels} channels at {rate} Hz by {gain_db} dB"
            )));
        }
        let limiter = ceiling_db
            .map(|c| Limiter::new(rate, channels, c))
            .transpose()?;
        let skip = limiter.as_ref().map_or(0, Limiter::latency);
        Ok(Leveler {
            gain: 10f64.powf(gain_db / 20.0) as f32,
            gain_db,
            limiter,
            skip,
            channels: channels as usize,
            out: AudioBuffer::new(rate, channels),
        })
    }

    pub fn mode(&self) -> LevelMode {
        if self.limiter.is_some() {
            LevelMode::Limited
        } else {
            LevelMode::Linear
        }
    }

    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    pub fn latency(&self) -> usize {
        self.limiter.as_ref().map_or(0, Limiter::latency)
    }

    pub fn process(&mut self, buffer: &mut AudioBuffer) -> Result<()> {
        if buffer.channels as usize != self.channels {
            return Err(Error::Invalid(format!(
                "the leveler takes {} channels, got {}",
                self.channels, buffer.channels
            )));
        }
        let g = self.gain;
        buffer.samples.iter_mut().for_each(|s| *s *= g);
        let Some(limiter) = self.limiter.as_mut() else {
            return Ok(());
        };
        limiter.process(buffer, &mut self.out)?;
        self.take(buffer);
        Ok(())
    }

    pub fn flush(&mut self, buffer: &mut AudioBuffer) -> Result<()> {
        buffer.samples.clear();
        buffer.channels = self.channels as u16;
        buffer.rate = self.out.rate;
        let Some(limiter) = self.limiter.as_mut() else {
            return Ok(());
        };
        limiter.flush(&mut self.out);
        self.take(buffer);
        Ok(())
    }

    fn take(&mut self, buffer: &mut AudioBuffer) {
        let frames = self.out.samples.len() / self.channels;
        let drop = self.skip.min(frames);
        self.skip -= drop;
        buffer.samples.clear();
        buffer
            .samples
            .extend_from_slice(&self.out.samples[drop * self.channels..]);
    }
}

fn silent(target: &LevelTarget) -> Error {
    Error::Invalid(format!(
        "the source's audio is silent (below {QUIETEST_LUFS} LUFS), so there is nothing to bring to {} LUFS",
        target.integrated
    ))
}

type Tell<'a> = dyn FnMut(&LevelProgress) -> bool + 'a;

fn tell(progress: &mut Tell, stage: LevelStage, frames: u64, total: u64) -> Result<()> {
    let done = if total == 0 {
        0.0
    } else {
        (frames as f64 / total as f64).min(1.0) as f32
    };
    if progress(&LevelProgress { stage, done }) {
        Ok(())
    } else {
        Err(Error::Cancelled)
    }
}

fn decoded(source: &Pass, each: impl FnMut(AudioBuffer) -> Result<()>) -> Result<()> {
    decode_audio_span(source.path, source.track, source.prefs, &source.span, each)
}

fn meter_pass(
    source: &Pass,
    mut leveler: Option<Leveler>,
    stage: LevelStage,
    progress: &mut Tell,
) -> Result<LoudnessStats> {
    let span = source.span;
    let total = source.total;
    let mut meter = LoudnessMeter::new(span.rate, span.channels)?;
    let mut frames = 0u64;
    tell(progress, stage, 0, total)?;
    decoded(source, |mut b| {
        frames += b.frames() as u64;
        if let Some(l) = leveler.as_mut() {
            l.process(&mut b)?;
        }
        meter.process(&b)?;
        tell(progress, stage, frames, total)
    })?;
    if let Some(l) = leveler.as_mut() {
        let mut tail = AudioBuffer::new(span.rate, span.channels);
        l.flush(&mut tail)?;
        meter.process(&tail)?;
    }
    Ok(LoudnessStats::of(&meter))
}

pub fn measure_loudness(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    mut progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<LoudnessStats> {
    let source = Pass::open(path, track, prefs, AudioSpan::WHOLE)?;
    meter_pass(&source, None, LevelStage::Measuring, &mut progress)
}

pub fn meter_loudness(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    mut progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<Loudness> {
    let p = probe(path, prefs)?;
    let t = p
        .audio
        .get(track)
        .ok_or_else(|| Error::Invalid(format!("audio track {track} of {}", p.audio.len())))?;
    if t.rate == 0 || t.channels == 0 {
        return Err(Error::Unsupported(format!(
            "audio track {track} reads as {} channels at {} Hz",
            t.channels, t.rate
        )));
    }
    let span = AudioSpan {
        rate: t.rate,
        channels: t.channels,
        ..AudioSpan::WHOLE
    };
    let source = Pass::open(path, track, prefs, span)?;
    let mut meter = LoudnessMeter::new(span.rate, span.channels)?;
    let mut frames = 0u64;
    let progress: &mut Tell = &mut progress;
    tell(progress, LevelStage::Measuring, 0, source.total)?;
    decoded(&source, |b| {
        frames += b.frames() as u64;
        meter.process(&b)?;
        tell(progress, LevelStage::Measuring, frames, source.total)
    })?;
    Ok(meter.summary())
}

fn format_for(codec: Codec) -> Format {
    if codec == Codec::Opus {
        Format::Webm
    } else {
        Format::Mp4(Brand::Mp4)
    }
}

fn encoded_pass(
    source: &Pass,
    mut leveler: Leveler,
    codec: Codec,
    kbps: u32,
    progress: &mut Tell,
) -> Result<LoudnessStats> {
    let mut enc = Encoder::new(format_for(codec), kbps)?;
    let mut checker = enc.checker()?;
    let mut meter = LoudnessMeter::new(LEVEL_RATE, 2)?;
    let stage = LevelStage::Calibrating;
    let total = source.total;
    let mut frames = 0u64;
    let pass = Pass {
        span: AudioSpan {
            rate: LEVEL_RATE,
            channels: 2,
            ..source.span
        },
        ..*source
    };
    tell(progress, stage, 0, total)?;
    let mut emit = |packets: Vec<Packet>| -> Result<()> {
        for p in packets {
            meter.process(&checker.decode(&p)?)?;
        }
        Ok(())
    };
    decoded(&pass, |mut b| {
        leveler.process(&mut b)?;
        frames += b.frames() as u64;
        emit(enc.encode(&b)?)?;
        tell(progress, stage, frames, total)
    })?;
    let mut tail = AudioBuffer::new(LEVEL_RATE, 2);
    leveler.flush(&mut tail)?;
    let mut packets = enc.encode(&tail)?;
    packets.extend(enc.flush()?);
    emit(packets)?;
    Ok(LoudnessStats::of(&meter))
}

#[derive(Debug, Clone, PartialEq)]
pub struct LevelledPcm {
    pub buffer: AudioBuffer,
    pub before: LoudnessStats,
    pub after: LoudnessStats,
    pub encoded: Option<LoudnessStats>,
    pub mode: LevelMode,
    pub gain_db: f64,
    pub ceiling_db: Option<f64>,
}

fn measured(b: &AudioBuffer) -> Result<LoudnessStats> {
    let mut meter = LoudnessMeter::new(b.rate, b.channels)?;
    meter.process(b)?;
    Ok(LoudnessStats::of(&meter))
}

fn levelled(buffer: &AudioBuffer, gain_db: f64, ceiling_db: Option<f64>) -> Result<AudioBuffer> {
    let mut leveler = Leveler::with_gain(buffer.rate, buffer.channels, gain_db, ceiling_db)?;
    let mut out = buffer.clone();
    leveler.process(&mut out)?;
    let mut tail = AudioBuffer::new(buffer.rate, buffer.channels);
    leveler.flush(&mut tail)?;
    out.samples.extend_from_slice(&tail.samples);
    Ok(out)
}

pub fn level_pcm(
    buffer: &AudioBuffer,
    target: &LevelTarget,
    style: LevelStyle,
) -> Result<LevelledPcm> {
    target.check()?;
    let before = measured(buffer)?;
    if !before.integrated.is_finite() || before.integrated <= QUIETEST_LUFS {
        return Err(silent(target));
    }
    let linear = target.integrated - before.integrated;
    let mut gain_db = linear;
    settle(|headroom| {
        let ceiling = target.true_peak - headroom;
        let ceiling_db = (before.true_peak + linear > ceiling).then_some(ceiling);
        let mut out = levelled(buffer, gain_db, ceiling_db)?;
        let mut after = measured(&out)?;
        if ceiling_db.is_some() && !style.linear {
            for _ in 0..TRIES {
                let short = target.integrated - after.integrated;
                if !short.is_finite() || short.abs() <= CLOSE {
                    break;
                }
                gain_db += short;
                out = levelled(buffer, gain_db, ceiling_db)?;
                after = measured(&out)?;
            }
        }
        let encoded = match style.encoded {
            Some((codec, kbps)) => Some(encoded_pcm(&out, codec, kbps)?),
            None => None,
        };
        let over = match encoded {
            Some(e) => e.true_peak - (target.true_peak - MARGIN),
            None => after.true_peak - target.true_peak,
        };
        let mode = if ceiling_db.is_some() {
            LevelMode::Limited
        } else {
            LevelMode::Linear
        };
        let levelled = LevelledPcm {
            buffer: out,
            before,
            after,
            encoded,
            mode,
            gain_db,
            ceiling_db,
        };
        let limited = ceiling_db.is_some() && encoded.is_some();
        Ok((levelled, over, limited))
    })
}

fn settle<T>(mut trial: impl FnMut(f64) -> Result<(T, f64, bool)>) -> Result<T> {
    let mut headroom = HEADROOM;
    let mut over_at: Option<(f64, f64)> = None;
    let mut under_at = (HEADROOM, 0.0);
    let mut kept: Option<(f64, T)> = None;
    let mut raised = 0;
    let mut narrowed = 0;
    loop {
        let (value, over, limited) = trial(headroom)?;
        if over.is_nan() {
            return Ok(value);
        }
        if over > 0.0 {
            over_at = Some((headroom, over));
            if kept.is_none() {
                if raised == RETRIES {
                    return Ok(value);
                }
                raised += 1;
                headroom += over + CLOSE * raised as f64;
                continue;
            }
        } else {
            if kept.as_ref().is_none_or(|(slack, _)| -over < *slack) {
                kept = Some((-over, value));
            }
            under_at = (headroom, over);
            if -over <= SLACK || (!limited && over_at.is_none()) {
                break;
            }
        }
        let next = match over_at {
            Some(lo) => between(lo, under_at),
            None => (under_at.0 + under_at.1 + SLACK / 2.0).max(0.0),
        };
        if narrowed == NARROWS || next >= under_at.0 - 0.01 {
            break;
        }
        narrowed += 1;
        headroom = next;
    }
    kept.map(|(_, v)| v)
        .ok_or_else(|| Error::Invalid("a levelling search kept no trial".into()))
}

fn between((h1, o1): (f64, f64), (h2, o2): (f64, f64)) -> f64 {
    let aim = -SLACK / 2.0;
    let t = ((o1 - aim) / (o1 - o2)).clamp(0.2, 0.8);
    h1 + (h2 - h1) * t
}

fn encoded_pcm(buffer: &AudioBuffer, codec: Codec, kbps: u32) -> Result<LoudnessStats> {
    if buffer.rate != LEVEL_RATE || buffer.channels != 2 {
        return Err(Error::Invalid(format!(
            "an encode check takes {LEVEL_RATE} Hz stereo, not {} Hz with {} channels",
            buffer.rate, buffer.channels
        )));
    }
    let mut enc = Encoder::new(format_for(codec), kbps)?;
    let mut checker = enc.checker()?;
    let mut meter = LoudnessMeter::new(LEVEL_RATE, 2)?;
    let mut packets = Vec::new();
    for part in buffer.samples.chunks(LEVEL_RATE as usize * 2) {
        let mut b = AudioBuffer::new(LEVEL_RATE, 2);
        b.samples.extend_from_slice(part);
        packets.extend(enc.encode(&b)?);
    }
    packets.extend(enc.flush()?);
    for p in &packets {
        meter.process(&checker.decode(p)?)?;
    }
    Ok(LoudnessStats::of(&meter))
}

fn audio_frames(path: &Path, track: usize, prefs: &Prefs, span: &AudioSpan) -> Result<u64> {
    let p = probe(path, prefs)?;
    if p.audio.is_empty() {
        return Err(Error::Unsupported("no audio to level".into()));
    }
    if track >= p.audio.len() {
        return Err(Error::Invalid(format!(
            "audio track {track} of {}",
            p.audio.len()
        )));
    }
    let end = p.duration.max(0.0).min(span.to);
    Ok(((end - span.from).max(0.0) * span.rate as f64).round() as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Mp4(Brand),
    Mkv,
    Webm,
}

fn format(path: &Path) -> Result<Format> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    Ok(match ext.as_str() {
        "mp4" | "m4v" | "m4a" => Format::Mp4(Brand::Mp4),
        "mov" => Format::Mp4(Brand::Mov),
        "mkv" | "mka" => Format::Mkv,
        "webm" => Format::Webm,
        _ => {
            return Err(Error::Unsupported(format!(
                "levelling into {}: use mp4, mov, mkv or webm",
                path.display()
            )));
        }
    })
}

enum Encoder {
    Aac(Box<crate::aac::Encoder>),
    #[cfg(feature = "opus")]
    Opus(Box<crate::opus::enc::Encoder>),
}

impl Encoder {
    fn new(format: Format, kbps: u32) -> Result<Encoder> {
        if format == Format::Webm {
            #[cfg(feature = "opus")]
            return Ok(Encoder::Opus(Box::new(crate::opus::enc::Encoder::new(
                crate::opus::enc::EncoderConfig::new(2, kbps * 1000),
            )?)));
            #[cfg(not(feature = "opus"))]
            return Err(Error::Unsupported(
                "Opus audio for WebM (built without the opus feature)".into(),
            ));
        }
        Ok(Encoder::Aac(Box::new(crate::aac::Encoder::new(
            crate::aac::EncoderConfig {
                rate: LEVEL_RATE,
                channels: 2,
                bitrate: kbps * 1000,
            },
        )?)))
    }

    fn codec(&self) -> Codec {
        match self {
            Encoder::Aac(_) => Codec::Aac,
            #[cfg(feature = "opus")]
            Encoder::Opus(_) => Codec::Opus,
        }
    }

    fn encode(&mut self, b: &AudioBuffer) -> Result<Vec<Packet>> {
        match self {
            Encoder::Aac(e) => e.encode(b),
            #[cfg(feature = "opus")]
            Encoder::Opus(e) => e.encode(b),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        match self {
            Encoder::Aac(e) => Ok(e.flush()),
            #[cfg(feature = "opus")]
            Encoder::Opus(e) => e.flush(),
        }
    }

    fn checker(&self) -> Result<Checker> {
        Ok(match self {
            Encoder::Aac(e) => Checker::Aac(Box::new(crate::aac::Decoder::new(&e.extradata())?)),
            #[cfg(feature = "opus")]
            Encoder::Opus(_) => Checker::Opus(
                Box::new(crate::opus::StreamDecoder::new(LEVEL_RATE, 2)?),
                Vec::new(),
            ),
        })
    }
}

enum Checker {
    Aac(Box<crate::aac::Decoder>),
    #[cfg(feature = "opus")]
    Opus(Box<crate::opus::StreamDecoder>, Vec<f32>),
}

impl Checker {
    fn decode(&mut self, p: &Packet) -> Result<AudioBuffer> {
        match self {
            Checker::Aac(d) => d.decode(p),
            #[cfg(feature = "opus")]
            Checker::Opus(d, out) => {
                out.clear();
                d.decode_interleaved(Some(&p.data), out)?;
                let mut b = AudioBuffer::new(LEVEL_RATE, 2);
                b.samples = std::mem::take(out);
                Ok(b)
            }
        }
    }
}

enum Sink {
    Mp4(mp4::Writer<File>),
    Mkv(mkv::Writer<File>),
}

enum Source<'a> {
    Mp4(&'a mp4::Reader),
    Mkv(&'a mkv::Reader),
}

struct VideoCopy<'a> {
    source: Source<'a>,
    track: usize,
    dst: usize,
    next: usize,
    entry: Option<u32>,
    shift: i64,
    times: Vec<(i64, i64, u32)>,
    buf: Vec<u8>,
    codec: Option<Codec>,
}

fn copyable_into_webm(codec_id: &str) -> bool {
    matches!(codec_id, "V_VP8" | "V_VP9" | "V_AV1")
}

impl<'a> VideoCopy<'a> {
    fn open(demux: &'a Demux, sink: &mut Sink, format: Format) -> Result<Option<VideoCopy<'a>>> {
        let source = match demux {
            Demux::Mp4(r) => Source::Mp4(r),
            Demux::Mkv(r) => Source::Mkv(r.full()?),
        };
        let track = match &source {
            Source::Mp4(r) => r.video(),
            Source::Mkv(r) => r.video(),
        };
        let Some(track) = track else {
            return Ok(None);
        };
        let mut copy = VideoCopy {
            source,
            track,
            dst: 0,
            next: 0,
            entry: None,
            shift: 0,
            times: Vec::new(),
            buf: Vec::new(),
            codec: None,
        };
        copy.dst = match (&copy.source, sink) {
            (Source::Mp4(r), Sink::Mp4(w)) => {
                let t = &r.tracks[track];
                copy.codec = t.codec();
                let mut cfg = mp4::TrackConfig::from_track(t)?;
                cfg.edits = t.edits.clone();
                let dst = w.add_track(cfg)?;
                for e in t.entries.iter().skip(1) {
                    w.add_entry(dst, e.clone())?;
                }
                dst
            }
            (Source::Mp4(r), Sink::Mkv(w)) => {
                let t = &r.tracks[track];
                copy.codec = t.codec();
                let cfg = mkv::TrackConfig::from_mp4(t)?;
                if format == Format::Webm && !copyable_into_webm(&cfg.codec_id) {
                    return Err(webm_refusal(t.codec()));
                }
                copy.shift = (0..t.samples.len())
                    .filter_map(|n| t.presentation(n))
                    .min()
                    .unwrap_or(0)
                    .min(0);
                w.add_track(cfg)?
            }
            (Source::Mkv(r), Sink::Mkv(w)) => {
                let t = &r.tracks[track];
                copy.codec = t.codec();
                if format == Format::Webm && !copyable_into_webm(&t.codec_id) {
                    return Err(webm_refusal(t.codec()));
                }
                copy.shift = t
                    .samples
                    .iter()
                    .map(|s| s.pts)
                    .min()
                    .unwrap_or(0)
                    .min(demux.shift(track));
                w.add_track(mkv::TrackConfig::from_track(t))?
            }
            (Source::Mkv(r), Sink::Mp4(w)) => {
                let t = &r.tracks[track];
                copy.codec = t.codec();
                let extradata = t.extradata().unwrap_or_default();
                let (wd, ht) = (t.width as u16, t.height as u16);
                let entry = match t.codec() {
                    Some(Codec::H264) => SampleEntry::h264(AvcConfig::parse(&extradata)?, wd, ht),
                    Some(Codec::Hevc) => SampleEntry::hevc(HevcConfig::parse(&extradata)?, wd, ht),
                    _ => {
                        return Err(Error::Unsupported(format!(
                            "copying {} video from Matroska into MP4",
                            t.codec_name()
                        )));
                    }
                };
                if t.time_base.num != 1 || t.time_base.den == 0 {
                    return Err(Error::Unsupported(format!(
                        "copying video with a time base of {}/{} into MP4",
                        t.time_base.num, t.time_base.den
                    )));
                }
                let shift = demux.shift(track);
                copy.times = mkv_times(&t.samples, shift, t.default_duration, t.time_base.den);
                w.add_track(mp4::TrackConfig::new(entry, t.time_base.den))?
            }
        };
        Ok(Some(copy))
    }

    fn len(&self) -> usize {
        match &self.source {
            Source::Mp4(r) => r.tracks[self.track].samples.len(),
            Source::Mkv(r) => r.tracks[self.track].samples.len(),
        }
    }

    fn seconds(&self, i: usize) -> f64 {
        match &self.source {
            Source::Mp4(r) => {
                let t = &r.tracks[self.track];
                t.seconds(t.samples[i].dts.saturating_sub(t.edit_offset))
            }
            Source::Mkv(r) => {
                let t = &r.tracks[self.track];
                t.seconds(t.samples[i].pts.saturating_sub(self.shift))
            }
        }
    }

    fn until(&mut self, sink: &mut Sink, seconds: f64) -> Result<()> {
        while self.next < self.len() && self.seconds(self.next) < seconds {
            self.push(sink, self.next)?;
            self.next += 1;
        }
        Ok(())
    }

    fn push(&mut self, sink: &mut Sink, i: usize) -> Result<()> {
        match (&self.source, sink) {
            (Source::Mp4(r), Sink::Mp4(w)) => {
                let s = r.tracks[self.track].samples[i];
                r.read_into(self.track, i, &mut self.buf)?;
                if self.entry != Some(s.entry) {
                    w.use_entry(self.dst, s.entry)?;
                    self.entry = Some(s.entry);
                }
                w.push_sample(self.dst, &self.buf, s.pts, s.dts, s.duration, s.keyframe)
            }
            (Source::Mp4(r), Sink::Mkv(w)) => {
                let t = &r.tracks[self.track];
                r.read_into(self.track, i, &mut self.buf)?;
                let pts = t.presentation(i).unwrap_or(0).saturating_sub(self.shift);
                w.push_sample(self.dst, &self.buf, pts, None, t.samples[i].keyframe)
            }
            (Source::Mkv(r), Sink::Mkv(w)) => {
                let s = r.tracks[self.track].samples[i];
                r.read_into(self.track, i, &mut self.buf)?;
                w.push_sample(
                    self.dst,
                    &self.buf,
                    s.pts.saturating_sub(self.shift),
                    None,
                    s.keyframe,
                )
            }
            (Source::Mkv(r), Sink::Mp4(w)) => {
                let s = r.tracks[self.track].samples[i];
                r.read_into(self.track, i, &mut self.buf)?;
                let (pts, dts, duration) = self.times[i];
                w.push_sample(self.dst, &self.buf, pts, dts, duration, s.keyframe)
            }
        }
    }
}

fn webm_refusal(codec: Option<Codec>) -> Error {
    Error::Unsupported(format!(
        "copying {} video into WebM, which takes VP8, VP9 or AV1",
        codec.map_or_else(|| "this".to_string(), |c| format!("{c:?}"))
    ))
}

fn mkv_times(
    samples: &[mkv::Sample],
    shift: i64,
    default_duration: Option<u64>,
    den: u32,
) -> Vec<(i64, i64, u32)> {
    let pts: Vec<i64> = samples
        .iter()
        .map(|s| s.pts.saturating_sub(shift))
        .collect();
    let mut order = pts.clone();
    order.sort_unstable();
    let lead = order
        .iter()
        .zip(&pts)
        .map(|(o, p)| o - p)
        .max()
        .unwrap_or(0)
        .max(0);
    let dts: Vec<i64> = order.iter().map(|o| o - lead).collect();
    let last = default_duration
        .map(|ns| (ns as u128 * den as u128 / 1_000_000_000) as i64)
        .filter(|&d| d > 0)
        .or_else(|| dts.windows(2).last().map(|w| w[1] - w[0]))
        .unwrap_or(1);
    (0..samples.len())
        .map(|i| {
            let d = dts.get(i + 1).map_or(last, |n| n - dts[i]).max(1);
            (pts[i], dts[i], u32::try_from(d).unwrap_or(u32::MAX))
        })
        .collect()
}

struct Audio {
    dst: usize,
    priming: i64,
    held: Option<Packet>,
    end: i64,
}

impl Audio {
    fn open(sink: &mut Sink, enc: &Encoder, kbps: u32) -> Result<Audio> {
        let (dst, priming) = match (sink, enc) {
            (Sink::Mp4(w), Encoder::Aac(e)) => {
                let mut cfg = mp4::TrackConfig::new(
                    SampleEntry::aac(e.extradata(), 2, LEVEL_RATE, kbps * 1000),
                    LEVEL_RATE,
                );
                cfg.media_start = Some(e.priming() as i64);
                (w.add_track(cfg)?, e.priming() as i64)
            }
            (Sink::Mkv(w), Encoder::Aac(e)) => {
                let mut cfg = mkv::TrackConfig::aac(e.extradata(), LEVEL_RATE, 2);
                cfg.codec_delay = e.priming() as u64 * 1_000_000_000 / u64::from(LEVEL_RATE);
                (w.add_track(cfg)?, e.priming() as i64)
            }
            #[cfg(feature = "opus")]
            (Sink::Mkv(w), Encoder::Opus(e)) => (
                w.add_track(mkv::TrackConfig::opus(e.codec_private()))?,
                e.pre_skip() as i64,
            ),
            #[cfg(feature = "opus")]
            (Sink::Mp4(_), Encoder::Opus(_)) => {
                return Err(Error::Unsupported("Opus audio in MP4".into()));
            }
        };
        Ok(Audio {
            dst,
            priming,
            held: None,
            end: 0,
        })
    }

    fn push(&mut self, sink: &mut Sink, p: Packet) -> Result<()> {
        if let Some(h) = self.held.replace(p) {
            self.write(sink, &h, None)?;
        }
        Ok(())
    }

    fn close(&mut self, sink: &mut Sink, frames: u64) -> Result<()> {
        let Some(h) = self.held.take() else {
            return Ok(());
        };
        self.end = self
            .priming
            .saturating_add(i64::try_from(frames).unwrap_or(i64::MAX));
        let duration = u32::try_from(self.end.saturating_sub(h.pts).max(1)).unwrap_or(u32::MAX);
        self.write(sink, &h, Some(duration))
    }

    fn write(&mut self, sink: &mut Sink, p: &Packet, duration: Option<u32>) -> Result<()> {
        match (sink, duration) {
            (Sink::Mp4(w), Some(d)) => w.push_timed(self.dst, p, d),
            (Sink::Mp4(w), None) => w.push(self.dst, p),
            (Sink::Mkv(w), _) => {
                w.push_sample(self.dst, &p.data, p.pts - self.priming, None, p.keyframe)
            }
        }
    }
}

fn partial(out: &Path) -> PathBuf {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    out.with_file_name(format!(".{name}.level"))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelPlan {
    pub before: LoudnessStats,
    pub gain_db: f64,
    pub ceiling_db: Option<f64>,
}

impl LevelPlan {
    pub fn mode(&self) -> LevelMode {
        if self.ceiling_db.is_some() {
            LevelMode::Limited
        } else {
            LevelMode::Linear
        }
    }

    pub fn leveler(&self, rate: u32, channels: u16) -> Result<Leveler> {
        Leveler::with_gain(rate, channels, self.gain_db, self.ceiling_db)
    }
}

pub fn plan_level(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    target: &LevelTarget,
    progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<LevelPlan> {
    plan_level_span(path, track, prefs, &AudioSpan::WHOLE, target, progress)
}

pub fn plan_level_span(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    span: &AudioSpan,
    target: &LevelTarget,
    mut progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<LevelPlan> {
    plan_level_styled(
        path,
        track,
        prefs,
        span,
        target,
        LevelStyle::default(),
        &mut progress,
    )
}

pub fn plan_level_styled(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    span: &AudioSpan,
    target: &LevelTarget,
    style: LevelStyle,
    mut progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<LevelPlan> {
    target.check()?;
    let progress: &mut Tell = &mut progress;
    let source = Pass::open(path, track, prefs, *span)?;
    let before = meter_pass(&source, None, LevelStage::Measuring, progress)?;
    let mut gain = target.integrated - before.integrated;
    let Some((codec, kbps)) = style.encoded else {
        return planned(
            &source,
            before,
            target,
            HEADROOM,
            gain,
            style.linear,
            progress,
        );
    };
    settle(|headroom| {
        let plan = planned(
            &source,
            before,
            target,
            headroom,
            gain,
            style.linear,
            progress,
        )?;
        gain = plan.gain_db;
        let trial = plan.leveler(LEVEL_RATE, 2)?;
        let got = encoded_pass(&source, trial, codec, kbps, progress)?;
        let over = got.true_peak - (target.true_peak - MARGIN);
        Ok((plan, over, plan.ceiling_db.is_some()))
    })
}

#[derive(Clone, Copy)]
struct Pass<'a> {
    path: &'a Path,
    track: usize,
    prefs: &'a Prefs,
    span: AudioSpan,
    total: u64,
}

impl<'a> Pass<'a> {
    fn open(path: &'a Path, track: usize, prefs: &'a Prefs, span: AudioSpan) -> Result<Pass<'a>> {
        Ok(Pass {
            path,
            track,
            prefs,
            span,
            total: audio_frames(path, track, prefs, &span)?,
        })
    }
}

fn planned(
    source: &Pass,
    before: LoudnessStats,
    target: &LevelTarget,
    headroom: f64,
    mut gain_db: f64,
    linear_only: bool,
    progress: &mut Tell,
) -> Result<LevelPlan> {
    if !before.integrated.is_finite() || before.integrated <= QUIETEST_LUFS {
        return Err(silent(target));
    }
    let linear = target.integrated - before.integrated;
    let ceiling = target.true_peak - headroom;
    if before.true_peak + linear <= ceiling {
        return Ok(LevelPlan {
            before,
            gain_db: linear,
            ceiling_db: None,
        });
    }
    if linear_only {
        return Ok(LevelPlan {
            before,
            gain_db: linear,
            ceiling_db: Some(ceiling),
        });
    }
    for _ in 0..TRIES {
        let span = source.span;
        let trial = Leveler::with_gain(span.rate, span.channels, gain_db, Some(ceiling))?;
        let got = meter_pass(source, Some(trial), LevelStage::Calibrating, progress)?;
        let short = target.integrated - got.integrated;
        if !short.is_finite() || short.abs() <= CLOSE {
            break;
        }
        gain_db += short;
    }
    Ok(LevelPlan {
        before,
        gain_db,
        ceiling_db: Some(ceiling),
    })
}

pub fn level(
    input: &Path,
    output: &Path,
    opts: &LevelOptions,
    progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<Levelled> {
    level_with(input, output, opts, LevelStyle::default(), progress)
}

pub fn level_with(
    input: &Path,
    output: &Path,
    opts: &LevelOptions,
    style: LevelStyle,
    mut progress: impl FnMut(&LevelProgress) -> bool,
) -> Result<Levelled> {
    let progress: &mut Tell = &mut progress;
    let target = opts.target;
    target.check()?;
    let format = format(output)?;
    let kbps = opts.audio_kbps.unwrap_or(if format == Format::Webm {
        OPUS_KBPS
    } else {
        AAC_KBPS
    });
    if !(32..=512).contains(&kbps) {
        return Err(Error::Invalid(format!(
            "an audio bitrate of {kbps} kbps, outside 32 to 512"
        )));
    }
    let source = Pass::open(input, opts.track, &opts.prefs, AudioSpan::WHOLE)?;
    let demux = Demux::open(input)?;
    let before = meter_pass(&source, None, LevelStage::Measuring, progress)?;
    let part = partial(output);
    let mut gain = target.integrated - before.integrated;
    let mut n = 0;
    let (trial, levelled) = settle(|headroom| {
        let plan = planned(
            &source,
            before,
            &target,
            headroom,
            gain,
            style.linear,
            progress,
        )?;
        gain = plan.gain_db;
        n += 1;
        let trial = Trial(part.with_extension(format!("level{n}")));
        let leveler = plan.leveler(LEVEL_RATE, 2)?;
        let (after, video, video_packets, audio) =
            write(&source, &demux, &trial.0, format, kbps, leveler, progress)?;
        let over = after.true_peak - (target.true_peak - MARGIN);
        let limited = plan.ceiling_db.is_some();
        let levelled = Levelled {
            path: output.to_path_buf(),
            before,
            after,
            mode: plan.mode(),
            gain_db: plan.gain_db,
            audio,
            audio_kbps: kbps,
            video,
            video_packets,
        };
        Ok(((trial, levelled), over, limited))
    })?;
    std::fs::rename(&trial.0, output).map_err(|e| Error::at(output, e))?;
    Ok(levelled)
}

struct Trial(PathBuf);

impl Drop for Trial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write(
    source: &Pass,
    demux: &Demux,
    part: &Path,
    format: Format,
    kbps: u32,
    mut leveler: Leveler,
    progress: &mut Tell,
) -> Result<(LoudnessStats, Option<Codec>, u64, Codec)> {
    let mut enc = Encoder::new(format, kbps)?;
    let mut sink = match format {
        Format::Mp4(brand) => Sink::Mp4(mp4::Writer::create_with(
            part,
            mp4::Options { brand, reserve: 0 },
        )?),
        Format::Mkv => Sink::Mkv(mkv::Writer::create(part)?),
        Format::Webm => Sink::Mkv(mkv::Writer::create_with(part, mkv::Options::webm())?),
    };
    let mut video = VideoCopy::open(demux, &mut sink, format)?;
    let mut audio = Audio::open(&mut sink, &enc, kbps)?;
    let mut checker = enc.checker()?;
    let mut meter = LoudnessMeter::new(LEVEL_RATE, 2)?;
    let stage = LevelStage::Levelling;
    let total = source.total;
    let mut frames = 0u64;
    tell(progress, stage, 0, total)?;
    let mut emit = |packets: Vec<Packet>,
                    sink: &mut Sink,
                    audio: &mut Audio,
                    video: &mut Option<VideoCopy>|
     -> Result<()> {
        for p in packets {
            let b = checker.decode(&p)?;
            meter.process(&b)?;
            let at = (p.pts - audio.priming) as f64 / LEVEL_RATE as f64;
            if let Some(v) = video.as_mut() {
                v.until(sink, at)?;
            }
            audio.push(sink, p)?;
        }
        Ok(())
    };
    decoded(source, |mut b| {
        leveler.process(&mut b)?;
        frames += b.frames() as u64;
        let packets = enc.encode(&b)?;
        emit(packets, &mut sink, &mut audio, &mut video)?;
        tell(progress, stage, frames, total)
    })?;
    let mut tail = AudioBuffer::new(LEVEL_RATE, 2);
    leveler.flush(&mut tail)?;
    frames += tail.frames() as u64;
    let mut packets = enc.encode(&tail)?;
    packets.extend(enc.flush()?);
    emit(packets, &mut sink, &mut audio, &mut video)?;
    audio.close(&mut sink, frames)?;
    let (codec, copied) = match video.as_mut() {
        Some(v) => {
            v.until(&mut sink, f64::INFINITY)?;
            (v.codec, v.len() as u64)
        }
        None => (None, 0),
    };
    match sink {
        Sink::Mp4(w) => {
            w.finish_faststart()?;
        }
        Sink::Mkv(w) => {
            w.finish()?;
        }
    }
    tell(progress, stage, total, total)?;
    Ok((LoudnessStats::of(&meter), codec, copied, enc.codec()))
}
