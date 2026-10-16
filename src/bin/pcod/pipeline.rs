use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};

use pito_codecs::filter::{
    Chroma, ColorSpec, Kernel, Matrix, Order, PackedMut, Range, Rotation, Scaler, convert_color,
    rgba_to_yuv, rotate, yuv_to_packed,
};
use pito_codecs::gpu::{self, Backend, HwEncoder};
use pito_codecs::media::{
    self, AudioBuffer, Bitstream, Codec, Frame, Packet, PixelFormat, Plane, Prefs, Rate, Rgba, TimeBase,
};
use pito_codecs::{aac, gif, h264, mkv, mp4, opus, png, vp9, webp};
use rayon::prelude::*;

use super::Fail;
use super::plan::{Container, SAMPLE_RATE};
use super::profile::{Encoder, Preset};
use super::progress::Progress;
use super::quality;
use super::source::{Kind, Media};

#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped before it finished")
    }
}

impl std::error::Error for Stopped {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audio {
    Copy { codec: String },
    Aac { kbps: u32 },
    Opus { kbps: u32 },
}

pub struct Job<'a> {
    pub media: &'a Media,
    pub container: Container,
    pub encoder: &'a Encoder,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub frames: Option<u64>,
    pub audio: Option<Audio>,
    pub partial: &'a Path,
    pub tune: Option<&'a str>,
    pub alpha: bool,
    pub stop: &'a AtomicBool,
    pub progress: &'a Progress,
}

pub struct Done {
    pub device: String,
    pub notes: Vec<String>,
}

enum Pic {
    Yuv(Frame),
    Rgba(Rgba),
    YuvAlpha(Frame, Plane),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Yuv,
    Rgba,
    YuvAlpha,
}

fn stopped(stop: &AtomicBool) -> Result<(), Fail> {
    if stop.load(Ordering::Relaxed) {
        Err(Box::new(Stopped))
    } else {
        Ok(())
    }
}

fn spec_of(color: gpu::Color) -> ColorSpec {
    let matrix = match color.matrix {
        5 | 6 => Matrix::Bt601,
        9 | 10 => Matrix::Bt2020,
        _ => Matrix::Bt709,
    };
    let range = if color.full_range {
        Range::Full
    } else {
        Range::Limited
    };
    ColorSpec::new(matrix, range)
}

fn rotation_of(degrees: u16) -> Option<Rotation> {
    match degrees % 360 {
        90 => Some(Rotation::R90),
        180 => Some(Rotation::R180),
        270 => Some(Rotation::R270),
        _ => None,
    }
}

struct YuvFix {
    rotation: Option<Rotation>,
    size: (u32, u32),
    scaler: Option<Scaler>,
    from: ColorSpec,
    rgba: bool,
}

impl YuvFix {
    fn apply(&mut self, frame: Frame) -> Result<Pic, Fail> {
        let mut frame = match self.rotation {
            Some(r) => {
                let mut out = Frame::new(frame.format, 0, 0, frame.pts, frame.time_base);
                rotate(&frame, r, &mut out)?;
                out
            }
            None => frame,
        };
        if (frame.width, frame.height) != self.size {
            if self.scaler.is_none() {
                self.scaler = Some(Scaler::new(
                    frame.width,
                    frame.height,
                    self.size.0,
                    self.size.1,
                    frame.format,
                    Kernel::Lanczos,
                )?);
            }
            if let Some(s) = &self.scaler {
                let mut out = Frame::new(
                    frame.format,
                    self.size.0,
                    self.size.1,
                    frame.pts,
                    frame.time_base,
                );
                s.run(&frame, &mut out)?;
                frame = out;
            }
        }
        if self.rgba {
            if frame.format != PixelFormat::Yuv420p8 {
                let mut out = Frame::new(
                    PixelFormat::Yuv420p8,
                    frame.width,
                    frame.height,
                    frame.pts,
                    frame.time_base,
                );
                convert_color(
                    &frame,
                    self.from,
                    self.from,
                    PixelFormat::Yuv420p8,
                    &mut out,
                )?;
                frame = out;
            }
            let mut rgba = Rgba::new(frame.width, frame.height);
            let (w, h) = (rgba.width, rgba.height);
            yuv_to_packed(
                &frame,
                self.from,
                Chroma::Bilinear,
                PackedMut::new(w, h, Order::Rgba, &mut rgba.data),
            )?;
            return Ok(Pic::Rgba(rgba));
        }
        if self.from != ColorSpec::BT709 || frame.format != PixelFormat::Yuv420p8 {
            let mut out = Frame::new(
                PixelFormat::Yuv420p8,
                frame.width,
                frame.height,
                frame.pts,
                frame.time_base,
            );
            convert_color(
                &frame,
                self.from,
                ColorSpec::BT709,
                PixelFormat::Yuv420p8,
                &mut out,
            )?;
            frame = out;
        }
        Ok(Pic::Yuv(frame))
    }
}

fn premultiply(img: &mut Rgba) {
    for px in img.data.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
}

fn unpremultiply(img: &mut Rgba) {
    for px in img.data.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

struct RgbaFix {
    size: (u32, u32),
    want: Want,
}

impl RgbaFix {
    fn apply(&self, mut img: Rgba) -> Result<Pic, Fail> {
        let resize = (img.width, img.height) != self.size;
        let yuv = self.want == Want::Yuv;
        if yuv || resize {
            premultiply(&mut img);
        }
        if resize {
            img = media::scale(&img, self.size.0, self.size.1);
        }
        if resize && !yuv {
            unpremultiply(&mut img);
        }
        if self.want == Want::YuvAlpha {
            let mut frame = Frame::new(
                PixelFormat::Yuv420p8,
                img.width,
                img.height,
                0,
                TimeBase::new(1, 1),
            );
            rgba_to_yuv(&img, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut frame)?;
            let (w, h) = (img.width as usize, img.height as usize);
            let plane = Plane {
                data: img.data.chunks_exact(4).map(|px| px[3]).collect(),
                stride: w,
                width: w,
                height: h,
            };
            return Ok(Pic::YuvAlpha(frame, plane));
        }
        if yuv {
            let mut frame = Frame::new(
                PixelFormat::Yuv420p8,
                img.width,
                img.height,
                0,
                TimeBase::new(1, 1),
            );
            rgba_to_yuv(&img, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut frame)?;
            return Ok(Pic::Yuv(frame));
        }
        Ok(Pic::Rgba(img))
    }
}

struct Retime {
    fps: (u32, u32),
    origin: Option<f64>,
    next: i64,
}

impl Retime {
    fn slot(&mut self, seconds: f64) -> i64 {
        let origin = *self.origin.get_or_insert(seconds);
        ((seconds - origin) * self.fps.0 as f64 / self.fps.1.max(1) as f64).round() as i64
    }

    fn span(&mut self, at: i64, until: i64) -> std::ops::Range<i64> {
        let first = self.next.max(at);
        if until <= first {
            return first..first;
        }
        self.next = until;
        first..until
    }
}

fn send_span(
    tx: &SyncSender<Result<Pic, Fail>>,
    pic: Pic,
    span: std::ops::Range<i64>,
) -> Result<(), Fail> {
    let count = span.end - span.start;
    for _ in 1..count {
        let copy = match &pic {
            Pic::Yuv(f) => Pic::Yuv(f.clone()),
            Pic::Rgba(r) => Pic::Rgba(r.clone()),
            Pic::YuvAlpha(f, a) => Pic::YuvAlpha(f.clone(), a.clone()),
        };
        tx.send(Ok(copy))
            .map_err(|_| Fail::from("the encoder stopped"))?;
    }
    if count > 0 {
        tx.send(Ok(pic))
            .map_err(|_| Fail::from("the encoder stopped"))?;
    }
    Ok(())
}

fn produce_file(
    job: &Job,
    rgba: bool,
    tx: &SyncSender<Result<Pic, Fail>>,
) -> Result<Vec<String>, Fail> {
    let media = job.media;
    let video = media::decode_video(&media.path, 0.0, None, &Prefs::default())?;
    let notes = video.reasons.clone();
    let mut fix = YuvFix {
        rotation: rotation_of(media.rotation),
        size: job.size,
        scaler: None,
        from: spec_of(media.color),
        rgba,
    };
    let mut retime = Retime {
        fps: job.fps,
        origin: None,
        next: 0,
    };
    let mut held: Option<(Frame, i64)> = None;
    for frame in video {
        stopped(job.stop)?;
        let frame = frame?;
        let slot = retime.slot(frame.time_base.seconds(frame.pts));
        if let Some((f, at)) = held.take() {
            let span = retime.span(at, slot);
            if !span.is_empty() {
                send_span(tx, fix.apply(f)?, span)?;
            }
        }
        held = Some((frame, slot));
    }
    if let Some((f, at)) = held.take() {
        let step = f64::from(media.fps.1.max(1)) / f64::from(media.fps.0.max(1));
        let end = retime.slot(f.time_base.seconds(f.pts) + step);
        let until = if retime.next == 0 {
            end.max(at + 1)
        } else {
            end
        };
        let span = retime.span(at, until);
        if !span.is_empty() {
            send_span(tx, fix.apply(f)?, span)?;
        }
    }
    Ok(notes)
}

fn produce_alpha(
    job: &Job,
    want: Want,
    tx: &SyncSender<Result<Pic, Fail>>,
) -> Result<Vec<String>, Fail> {
    let media = job.media;
    let mut video = media::decode_alpha(&media.path, 0.0)?;
    let fix = RgbaFix {
        size: job.size,
        want,
    };
    let color = video.color;
    let rotation = media.rotation;
    let image = move |f: Frame, a: Option<Plane>| -> Rgba {
        let img = media::to_rgba_alpha(&f, a.as_ref(), 0, color);
        if rotation.is_multiple_of(360) {
            img
        } else {
            media::rotate(&img, rotation)
        }
    };
    let mut retime = Retime {
        fps: job.fps,
        origin: None,
        next: 0,
    };
    let mut held: Option<(Frame, Option<Plane>, i64)> = None;
    while let Some((frame, alpha)) = video.next_frame()? {
        stopped(job.stop)?;
        let slot = retime.slot(frame.time_base.seconds(frame.pts));
        if let Some((f, a, at)) = held.take() {
            let span = retime.span(at, slot);
            if !span.is_empty() {
                send_span(tx, fix.apply(image(f, a))?, span)?;
            }
        }
        held = Some((frame, alpha, slot));
    }
    if let Some((f, a, at)) = held.take() {
        let step = f64::from(media.fps.1.max(1)) / f64::from(media.fps.0.max(1));
        let end = retime.slot(f.time_base.seconds(f.pts) + step);
        let until = if retime.next == 0 { end.max(at + 1) } else { end };
        let span = retime.span(at, until);
        if !span.is_empty() {
            send_span(tx, fix.apply(image(f, a))?, span)?;
        }
    }
    Ok(Vec::new())
}

fn produce_frames(
    job: &Job,
    files: &[PathBuf],
    want: Want,
    tx: &SyncSender<Result<Pic, Fail>>,
) -> Result<Vec<String>, Fail> {
    let media = job.media;
    let fix = RgbaFix {
        size: job.size,
        want,
    };
    let mut retime = Retime {
        fps: job.fps,
        origin: None,
        next: 0,
    };
    let rate = media.fps_value().max(1e-9);
    let mut slots: Vec<i64> = (0..=files.len())
        .map(|i| retime.slot(i as f64 / rate))
        .collect();
    if let Some(last) = slots.last_mut() {
        *last = (*last).max(1);
    }
    let batch = rayon::current_num_threads().clamp(2, 32) * 2;
    let first = (media.width, media.height);
    for (start, chunk) in files.chunks(batch).enumerate().map(|(k, c)| (k * batch, c)) {
        stopped(job.stop)?;
        let made: Vec<Result<Option<Pic>, Fail>> = chunk
            .par_iter()
            .enumerate()
            .map(|(k, path)| {
                let i = start + k;
                let until = slots[i + 1];
                if until <= slots[i] {
                    return Ok(None);
                }
                let at = |e: &dyn std::fmt::Display| -> Fail {
                    format!("{}: {e}", path.display()).into()
                };
                let data = std::fs::read(path).map_err(|e| at(&e))?;
                let img = png::decode(&data).map_err(|e| at(&e))?;
                if (img.width, img.height) != first {
                    return Err(at(&format_args!(
                        "the frame is {}x{}, the first is {}x{}",
                        img.width, img.height, first.0, first.1
                    )));
                }
                fix.apply(img).map(Some)
            })
            .collect();
        for (k, pic) in made.into_iter().enumerate() {
            let i = start + k;
            let until = slots[i + 1];
            let span = retime.span(slots[i], until);
            if let Some(pic) = pic? {
                send_span(tx, pic, span)?;
            }
        }
    }
    Ok(Vec::new())
}

enum VEnc {
    H264(Box<h264::Encoder>),
    Gpu(Box<HwEncoder>),
    Vp9(Box<vp9::Encoder>),
    Vp9Alpha(Box<vp9::AlphaEncoder>),
}

type Coded = Vec<(Packet, Option<Vec<u8>>)>;

fn plain(packets: Vec<Packet>) -> Coded {
    packets.into_iter().map(|p| (p, None)).collect()
}

fn paired(packets: Vec<(Packet, Vec<u8>)>) -> Coded {
    packets.into_iter().map(|(p, a)| (p, Some(a))).collect()
}

impl VEnc {
    fn encode(&mut self, frame: &Frame, alpha: Option<&Plane>) -> Result<Coded, Fail> {
        Ok(match (self, alpha) {
            (VEnc::H264(e), _) => plain(e.encode(frame)?),
            (VEnc::Gpu(e), _) => plain(e.encode(frame)?),
            (VEnc::Vp9(e), _) => plain(e.encode(frame)?),
            (VEnc::Vp9Alpha(e), Some(a)) => paired(e.encode(frame, a)?),
            (VEnc::Vp9Alpha(_), None) => {
                return Err("a frame without its alpha plane".into());
            }
        })
    }

    fn flush(&mut self) -> Result<Coded, Fail> {
        Ok(match self {
            VEnc::H264(e) => plain(e.flush()?),
            VEnc::Gpu(e) => plain(e.flush()?),
            VEnc::Vp9(e) => plain(e.flush()?),
            VEnc::Vp9Alpha(e) => paired(e.flush()?),
        })
    }

    fn extradata(&self) -> Option<Vec<u8>> {
        match self {
            VEnc::H264(e) => Some(e.avcc()),
            VEnc::Gpu(e) => e.extradata().map(<[u8]>::to_vec),
            VEnc::Vp9(_) | VEnc::Vp9Alpha(_) => Some(Vec::new()),
        }
    }
}

fn backend_name(backend: Backend) -> &'static str {
    match backend {
        Backend::VaApi => "VAAPI",
        Backend::Nvenc => "NVENC",
        Backend::VideoToolbox => "VideoToolbox",
        Backend::MediaFoundation => "Media Foundation",
    }
}

fn device_of(backend: Backend) -> String {
    let name = backend_name(backend);
    match gpu::driver(backend) {
        Ok(d) => {
            let short = d
                .split_once(" for ")
                .map(|(_, rest)| rest.split(" (").next().unwrap_or(rest))
                .unwrap_or(&d)
                .trim();
            format!("{name}: {short}")
        }
        Err(_) => name.to_string(),
    }
}

fn gop_of(fps: (u32, u32)) -> u32 {
    let per_second = fps.0.div_ceil(fps.1.max(1)).max(1);
    (per_second * 10).clamp(1, 250)
}

fn cpu_h264(job: &Job, preset: h264::Preset, rate: Rate) -> Result<VEnc, Fail> {
    let e = h264::Encoder::new(h264::EncoderConfig {
        width: job.size.0,
        height: job.size.1,
        fps: job.fps,
        profile: h264::Profile::High,
        preset,
        rate,
        gop: gop_of(job.fps),
        b_frames: 2,
        threads: 0,
        bitstream: Bitstream::LengthPrefixed,
    })?;
    Ok(VEnc::H264(Box::new(e)))
}

fn video_encoder(job: &Job, notes: &mut Vec<String>) -> Result<(VEnc, String), Fail> {
    let animation = job.tune == Some("animation");
    match job.encoder {
        Encoder::H264 { preset, crf } => {
            let name = match preset {
                Preset::Balanced => "balanced",
                Preset::Quality => "best",
            };
            let p = match preset {
                Preset::Balanced => h264::Preset::Balanced,
                Preset::Quality => h264::Preset::Quality,
            };
            let rate = quality::h264_rate(name, Some(f32::from(*crf)), animation);
            Ok((cpu_h264(job, p, rate)?, "CPU".into()))
        }
        Encoder::H264Gpu { qp } => {
            let config = gpu::EncoderConfig {
                width: job.size.0,
                height: job.size.1,
                fps: job.fps,
                profile: gpu::Profile::High,
                rate: Rate::Qp(*qp),
                gop: gop_of(job.fps),
                b_frames: 0,
                bitstream: Bitstream::LengthPrefixed,
                sar: (1, 1),
                color: gpu::Color::BT709,
            };
            match HwEncoder::new(Codec::H264, &config) {
                Ok(e) => {
                    let device = device_of(e.backend());
                    Ok((VEnc::Gpu(Box::new(e)), device))
                }
                Err(e) => {
                    notes.push(format!(
                        "no GPU H.264 encoder ({e}); encoding on the CPU at the balanced preset"
                    ));
                    let rate = quality::h264_rate("balanced", None, animation);
                    Ok((cpu_h264(job, h264::Preset::Balanced, rate)?, "CPU".into()))
                }
            }
        }
        Encoder::Vp9 { crf } => {
            let name = if *crf <= 28 { "best" } else { "balanced" };
            let rate = match quality::vp9_rate(name, Some(f32::from(*crf))) {
                Rate::Crf(c) => vp9::Rate::Crf(c),
                Rate::Qp(q) => vp9::Rate::Q(q),
                Rate::Bitrate(k) | Rate::Vbr { kbps: k, .. } => vp9::Rate::Bitrate(k),
            };
            let config = vp9::EncoderConfig {
                width: job.size.0,
                height: job.size.1,
                fps: job.fps,
                rate,
                gop: gop_of(job.fps).max(120),
                threads: 0,
                aq: false,
            };
            if job.alpha {
                let mut e = vp9::AlphaEncoder::new(config)?;
                e.set_color(2, false)?;
                return Ok((VEnc::Vp9Alpha(Box::new(e)), "CPU".into()));
            }
            let mut e = vp9::Encoder::new(config)?;
            e.set_color(2, false)?;
            Ok((VEnc::Vp9(Box::new(e)), "CPU".into()))
        }
        _ => Err("this encoder writes no video track".into()),
    }
}

enum Sink {
    Mp4(Box<mp4::Writer>),
    Mkv(Box<mkv::Writer>),
}

struct Mux {
    sink: Sink,
    video: usize,
    audio: Option<usize>,
    step: i64,
    last_dts: Option<i64>,
    video_tb: TimeBase,
}

impl Mux {
    fn push_video(&mut self, p: &Packet, alpha: Option<&[u8]>) -> Result<(), Fail> {
        match &mut self.sink {
            Sink::Mp4(w) => {
                let mut dts = p.dts.saturating_mul(self.step);
                if let Some(l) = self.last_dts
                    && dts <= l
                {
                    dts = l.saturating_add(1);
                }
                self.last_dts = Some(dts);
                w.push(
                    self.video,
                    &Packet {
                        data: p.data.clone(),
                        pts: p.pts.saturating_mul(self.step),
                        dts,
                        keyframe: p.keyframe,
                    },
                )?;
            }
            Sink::Mkv(w) if alpha.is_some() => w.push_alpha(self.video, p, alpha)?,
            Sink::Mkv(w) => w.push(self.video, p)?,
        }
        Ok(())
    }

    fn push_audio(&mut self, p: &Packet) -> Result<(), Fail> {
        let Some(a) = self.audio else {
            return Ok(());
        };
        match &mut self.sink {
            Sink::Mp4(w) => w.push(a, p)?,
            Sink::Mkv(w) => w.push(a, p)?,
        }
        Ok(())
    }

    fn finish(self) -> Result<(), Fail> {
        match self.sink {
            Sink::Mp4(w) => {
                w.finish_faststart()?;
            }
            Sink::Mkv(w) => {
                w.finish()?;
            }
        }
        Ok(())
    }
}

enum AudioTrackConfig {
    Mp4(mp4::TrackConfig),
    Mkv(mkv::TrackConfig),
}

struct AudioPlan {
    config: AudioTrackConfig,
    ticks: f64,
    work: AudioWork,
}

enum AudioWork {
    CopyMp4(Box<mp4::Reader>, usize, i64),
    CopyMkv(Box<mkv::Reader>, usize, Option<u32>),
    Aac(media::AudioStream, Box<aac::Encoder>),
    Opus(media::AudioStream, Box<opus::enc::Encoder>),
}

pub fn mkv_family(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|mut f| {
            let mut head = [0u8; 4];
            std::io::Read::read_exact(&mut f, &mut head)?;
            Ok(head == [0x1a, 0x45, 0xdf, 0xa3])
        })
        .unwrap_or(false)
}

fn audio_plan(job: &Job, audio: &Audio) -> Result<AudioPlan, Fail> {
    let media = job.media;
    let sound = media
        .audio
        .as_ref()
        .ok_or_else(|| Fail::from("the source has no audio"))?;
    let mp4_out = matches!(job.container, Container::Mp4 | Container::Mov);
    match audio {
        Audio::Copy { .. } if !mkv_family(&media.path) => {
            let reader = mp4::Reader::open(&media.path)?;
            let track = &reader.tracks[sound.stream];
            let ticks = f64::from(track.timescale.max(1));
            let first = if track.samples.is_empty() {
                0
            } else {
                reader.packet(sound.stream, 0)?.pts
            };
            let shift = (-first).max(0);
            let config = if mp4_out {
                let mut cfg = mp4::TrackConfig::from_track(track)?;
                cfg.edits = Vec::new();
                AudioTrackConfig::Mp4(cfg)
            } else {
                let mut cfg = mkv::TrackConfig::from_mp4(track)?;
                cfg.codec_delay = u64::try_from(shift).unwrap_or(0) * 1_000_000_000
                    / u64::from(track.timescale.max(1));
                AudioTrackConfig::Mkv(cfg)
            };
            let shift = if mp4_out { 0 } else { shift };
            Ok(AudioPlan {
                config,
                ticks,
                work: AudioWork::CopyMp4(Box::new(reader), sound.stream, shift),
            })
        }
        Audio::Copy { codec } => {
            let reader = mkv::Reader::open(&media.path)?;
            let track = &reader.tracks[sound.stream];
            if mp4_out {
                let asc = track
                    .extradata()
                    .ok_or_else(|| Fail::from("the AAC track has no AudioSpecificConfig"))?;
                if codec != "aac" {
                    return Err(format!("{codec} audio can't be copied into this container").into());
                }
                let rate = sound.rate.max(1);
                let entry = mp4::SampleEntry::aac(asc, u32::from(sound.channels.max(1)), rate, 0);
                Ok(AudioPlan {
                    config: AudioTrackConfig::Mp4(mp4::TrackConfig::new(entry, rate)),
                    ticks: f64::from(rate),
                    work: AudioWork::CopyMkv(Box::new(reader), sound.stream, Some(rate)),
                })
            } else {
                let tb = track.time_base;
                let cfg = mkv::TrackConfig::from_track(track);
                Ok(AudioPlan {
                    config: AudioTrackConfig::Mkv(cfg),
                    ticks: f64::from(tb.den) / f64::from(tb.num.max(1)),
                    work: AudioWork::CopyMkv(Box::new(reader), sound.stream, None),
                })
            }
        }
        Audio::Aac { kbps } => {
            let stream = media::audio_stream(
                &media.path,
                sound.index,
                0.0,
                SAMPLE_RATE,
                2,
                &Prefs::default(),
            )?;
            let enc = aac::Encoder::new(aac::EncoderConfig {
                rate: SAMPLE_RATE,
                channels: 2,
                bitrate: kbps * 1000,
            })?;
            let priming = i64::from(enc.priming());
            let config = if mp4_out {
                let entry = mp4::SampleEntry::aac(enc.extradata(), 2, SAMPLE_RATE, kbps * 1000);
                let mut cfg = mp4::TrackConfig::new(entry, SAMPLE_RATE);
                cfg.media_start = Some(priming);
                AudioTrackConfig::Mp4(cfg)
            } else {
                let mut cfg = mkv::TrackConfig::aac(enc.extradata(), SAMPLE_RATE, 2);
                cfg.codec_delay = (priming.max(0) as u64) * 1_000_000_000 / u64::from(SAMPLE_RATE);
                AudioTrackConfig::Mkv(cfg)
            };
            Ok(AudioPlan {
                config,
                ticks: f64::from(SAMPLE_RATE),
                work: AudioWork::Aac(stream, Box::new(enc)),
            })
        }
        Audio::Opus { kbps } => {
            let stream = media::audio_stream(
                &media.path,
                sound.index,
                0.0,
                SAMPLE_RATE,
                2,
                &Prefs::default(),
            )?;
            let enc = opus::enc::Encoder::new(opus::enc::EncoderConfig::new(2, kbps * 1000))?;
            let cfg = mkv::TrackConfig::opus(enc.codec_private());
            Ok(AudioPlan {
                config: AudioTrackConfig::Mkv(cfg),
                ticks: f64::from(SAMPLE_RATE),
                work: AudioWork::Opus(stream, Box::new(enc)),
            })
        }
    }
}

fn run_audio(
    work: AudioWork,
    stop: &AtomicBool,
    tx: &mpsc::Sender<Result<Packet, Fail>>,
) -> Result<(), Fail> {
    let send = |p: Packet| tx.send(Ok(p)).map_err(|_| Fail::from("the muxer stopped"));
    match work {
        AudioWork::CopyMp4(reader, track, shift) => {
            for i in 0..reader.tracks[track].samples.len() {
                stopped(stop)?;
                let mut p = reader.packet(track, i)?;
                p.pts += shift;
                p.dts += shift;
                send(p)?;
            }
        }
        AudioWork::CopyMkv(reader, track, rate) => {
            let t = &reader.tracks[track];
            for i in 0..t.samples.len() {
                stopped(stop)?;
                let mut p = reader.packet(track, i)?;
                if let Some(rate) = rate {
                    let at = (t.seconds(p.pts) * f64::from(rate)).round() as i64;
                    p.pts = at;
                    p.dts = at;
                }
                send(p)?;
            }
        }
        AudioWork::Aac(stream, mut enc) => {
            for buf in stream {
                stopped(stop)?;
                for p in enc.encode(&buf?)? {
                    send(p)?;
                }
            }
            for p in enc.flush() {
                send(p)?;
            }
        }
        AudioWork::Opus(stream, mut enc) => {
            for buf in stream {
                stopped(stop)?;
                let buf: AudioBuffer = buf?;
                for p in enc.encode(&buf)? {
                    send(p)?;
                }
            }
            for p in enc.flush()? {
                send(p)?;
            }
        }
    }
    Ok(())
}

enum VideoTrack {
    Encoded(Vec<u8>),
    Mp4(mp4::TrackConfig, TimeBase),
    Mkv(mkv::TrackConfig, TimeBase),
}

fn open_mux(job: &Job, track: VideoTrack, audio: Option<AudioTrackConfig>) -> Result<Mux, Fail> {
    let (w, h) = job.size;
    let (fw, fh) = (
        u16::try_from(w).map_err(|_| Fail::from("the picture is too wide for MP4"))?,
        u16::try_from(h).map_err(|_| Fail::from("the picture is too tall for MP4"))?,
    );
    match job.container {
        Container::Mp4 | Container::Mov => {
            let brand = if job.container == Container::Mov {
                mp4::Brand::Mov
            } else {
                mp4::Brand::Mp4
            };
            let mut writer = mp4::Writer::create_with(
                job.partial,
                mp4::Options {
                    brand,
                    ..Default::default()
                },
            )?;
            let (cfg, step, video_tb) = match track {
                VideoTrack::Encoded(x) => {
                    let entry = mp4::SampleEntry::h264(mp4::AvcConfig::parse(&x)?, fw, fh);
                    let timescale = job
                        .fps
                        .0
                        .checked_mul(64)
                        .ok_or_else(|| Fail::from("the frame rate is out of range"))?;
                    (
                        mp4::TrackConfig::new(entry, timescale),
                        i64::from(job.fps.1) * 64,
                        TimeBase::new(job.fps.1, job.fps.0),
                    )
                }
                VideoTrack::Mp4(cfg, tb) => (cfg, 1, tb),
                VideoTrack::Mkv(..) => {
                    return Err("a Matroska video track can't be copied into MP4".into());
                }
            };
            let video = writer.add_track(cfg)?;
            let audio = match audio {
                Some(AudioTrackConfig::Mp4(cfg)) => Some(writer.add_track(cfg)?),
                _ => None,
            };
            Ok(Mux {
                sink: Sink::Mp4(Box::new(writer)),
                video,
                audio,
                step,
                last_dts: None,
                video_tb,
            })
        }
        _ => {
            let options = if job.container == Container::Webm {
                mkv::Options::webm()
            } else {
                mkv::Options::default()
            };
            let mut writer = mkv::Writer::create_with(job.partial, options)?;
            let encoded_tb = TimeBase::new(job.fps.1, job.fps.0);
            let (cfg, tb) = match track {
                VideoTrack::Encoded(_) if matches!(job.encoder, Encoder::Vp9 { .. }) => {
                    let mut cfg = mkv::TrackConfig::vp9(w, h, encoded_tb);
                    cfg.alpha = job.alpha;
                    (cfg, encoded_tb)
                }
                VideoTrack::Encoded(x) => (mkv::TrackConfig::h264(x, w, h, encoded_tb), encoded_tb),
                VideoTrack::Mkv(cfg, tb) => (cfg, tb),
                VideoTrack::Mp4(..) => return Err("an MP4 track config for Matroska".into()),
            };
            let video = writer.add_track(cfg)?;
            let audio = match audio {
                Some(AudioTrackConfig::Mkv(cfg)) => Some(writer.add_track(cfg)?),
                _ => None,
            };
            Ok(Mux {
                sink: Sink::Mkv(Box::new(writer)),
                video,
                audio,
                step: 1,
                last_dts: None,
                video_tb: tb,
            })
        }
    }
}

struct Merge {
    rx: Option<Receiver<Result<Packet, Fail>>>,
    pending: Option<Packet>,
    ticks: f64,
}

impl Merge {
    fn upto(&mut self, mux: &mut Mux, seconds: Option<f64>) -> Result<(), Fail> {
        loop {
            if self.pending.is_none() {
                let Some(rx) = &self.rx else {
                    return Ok(());
                };
                match rx.recv() {
                    Ok(p) => self.pending = Some(p?),
                    Err(_) => {
                        self.rx = None;
                        return Ok(());
                    }
                }
            }
            let Some(p) = self.pending.take() else {
                return Ok(());
            };
            let at = p.pts as f64 / self.ticks;
            if seconds.is_some_and(|s| at > s) {
                self.pending = Some(p);
                return Ok(());
            }
            mux.push_audio(&p)?;
        }
    }
}

pub fn run(job: &Job) -> Result<Done, Fail> {
    let animated = job.container.animated();
    let want = if animated {
        Want::Rgba
    } else if job.alpha {
        Want::YuvAlpha
    } else {
        Want::Yuv
    };
    let (tx, rx) = mpsc::sync_channel::<Result<Pic, Fail>>(6);
    let mut notes = Vec::new();
    let mut audio_plan = match &job.audio {
        Some(a) if !animated => Some(audio_plan(job, a)?),
        _ => None,
    };
    let (atx, arx) = mpsc::channel::<Result<Packet, Fail>>();
    let audio_ticks = audio_plan.as_ref().map_or(1.0, |a| a.ticks);
    let audio_config = audio_plan.as_mut().map(|a| {
        std::mem::replace(
            &mut a.config,
            AudioTrackConfig::Mkv(mkv::TrackConfig::opus(Vec::new())),
        )
    });
    let audio_work = audio_plan.map(|a| a.work);
    job.progress.step("encode", 1, 1, job.frames);
    std::thread::scope(|s| -> Result<Done, Fail> {
        let copy = matches!(job.encoder, Encoder::Copy { .. });
        let producer = s.spawn(move || {
            if copy {
                return Ok(Vec::new());
            }
            let made = match &job.media.kind {
                Kind::File if job.media.alpha => produce_alpha(job, want, &tx),
                Kind::File => produce_file(job, animated, &tx),
                Kind::Frames(files) => produce_frames(job, files, want, &tx),
            };
            if let Err(e) = &made {
                let _ = tx.send(Err(e.to_string().into()));
            }
            drop(tx);
            made
        });
        let has_audio = audio_work.is_some();
        let audio = s.spawn(move || -> Result<(), Fail> {
            match audio_work {
                Some(w) => {
                    let r = run_audio(w, job.stop, &atx);
                    if let Err(e) = &r {
                        let _ = atx.send(Err(e.to_string().into()));
                    }
                    r
                }
                None => Ok(()),
            }
        });
        let merge = Merge {
            rx: has_audio.then_some(arx),
            pending: None,
            ticks: audio_ticks,
        };
        let consumed = if copy {
            consume_copy(job, audio_config, merge)
        } else if animated {
            consume_animation(job, &rx)
        } else {
            consume_video(job, &rx, audio_config, merge, &mut notes)
        };
        if consumed.is_err() {
            job.stop.store(true, Ordering::Relaxed);
        }
        drop(rx);
        let produced = producer
            .join()
            .unwrap_or_else(|_| Err("the decoder failed".into()));
        let sounded = audio
            .join()
            .unwrap_or_else(|_| Err("the audio failed".into()));
        let (device, _) = consumed?;
        produced?;
        sounded?;
        Ok(Done { device, notes })
    })
}

fn consume_copy(
    job: &Job,
    audio: Option<AudioTrackConfig>,
    mut merge: Merge,
) -> Result<(String, u64), Fail> {
    let path = &job.media.path;
    let mp4_out = matches!(job.container, Container::Mp4 | Container::Mov);
    let mut n = 0u64;
    let mut pump = |mux: &mut Mux,
                    count: usize,
                    read: &dyn Fn(usize) -> Result<Packet, Fail>|
     -> Result<(), Fail> {
        for i in 0..count {
            stopped(job.stop)?;
            let p = read(i)?;
            let at = mux.video_tb.seconds(p.dts.max(0));
            merge.upto(mux, Some(at))?;
            mux.push_video(&p, None)?;
            n += 1;
            job.progress.frame(n);
        }
        merge.upto(mux, None)
    };
    if mkv_family(path) {
        let reader = mkv::Reader::open(path)?;
        let t = reader
            .video()
            .ok_or_else(|| Fail::from("the source has no video track"))?;
        let track = &reader.tracks[t];
        if mp4_out {
            return Err("a Matroska video track can't be copied into MP4".into());
        }
        let cfg = mkv::TrackConfig::from_track(track);
        let mut mux = open_mux(job, VideoTrack::Mkv(cfg, track.time_base), audio)?;
        pump(&mut mux, track.samples.len(), &|i| Ok(reader.packet(t, i)?))?;
        job.progress.step("mux", 1, 1, None);
        mux.finish()?;
    } else {
        let reader = mp4::Reader::open(path)?;
        let t = reader
            .video()
            .ok_or_else(|| Fail::from("the source has no video track"))?;
        let track = &reader.tracks[t];
        let tb = TimeBase::new(1, track.timescale.max(1));
        let video = if mp4_out {
            let mut cfg = mp4::TrackConfig::from_track(track)?;
            cfg.edits = Vec::new();
            VideoTrack::Mp4(cfg, tb)
        } else {
            VideoTrack::Mkv(mkv::TrackConfig::from_mp4(track)?, tb)
        };
        let mut mux = open_mux(job, video, audio)?;
        pump(&mut mux, track.samples.len(), &|i| Ok(reader.packet(t, i)?))?;
        job.progress.step("mux", 1, 1, None);
        mux.finish()?;
    }
    if n == 0 {
        return Err("the source gave no frames".into());
    }
    Ok(("CPU".into(), n))
}

fn next_pic(rx: &Receiver<Result<Pic, Fail>>, stop: &AtomicBool) -> Result<Option<Pic>, Fail> {
    stopped(stop)?;
    match rx.recv() {
        Ok(p) => p.map(Some),
        Err(_) => Ok(None),
    }
}

fn consume_video(
    job: &Job,
    rx: &Receiver<Result<Pic, Fail>>,
    audio: Option<AudioTrackConfig>,
    mut merge: Merge,
    notes: &mut Vec<String>,
) -> Result<(String, u64), Fail> {
    let (mut enc, device) = video_encoder(job, notes)?;
    let mut audio = audio;
    let mut mux: Option<Mux> = None;
    let mut waiting: Coded = Vec::new();
    let mut n = 0u64;
    let time_base = TimeBase::new(job.fps.1, job.fps.0);
    let mut feed = |enc: &VEnc,
                    mux: &mut Option<Mux>,
                    packets: Coded,
                    merge: &mut Merge|
     -> Result<(), Fail> {
        waiting.extend(packets);
        if mux.is_none() {
            let Some(x) = enc.extradata() else {
                return Ok(());
            };
            *mux = Some(open_mux(job, VideoTrack::Encoded(x), audio.take())?);
        }
        if let Some(m) = mux.as_mut() {
            for (p, a) in waiting.drain(..) {
                let at = m.video_tb.seconds(p.dts.max(0));
                merge.upto(m, Some(at))?;
                m.push_video(&p, a.as_deref())?;
            }
        }
        Ok(())
    };
    while let Some(pic) = next_pic(rx, job.stop)? {
        let (mut frame, alpha) = match pic {
            Pic::Yuv(f) => (f, None),
            Pic::YuvAlpha(f, a) => (f, Some(a)),
            Pic::Rgba(_) => {
                return Err("a picture in the wrong form for the video encoder".into());
            }
        };
        frame.pts = i64::try_from(n).unwrap_or(i64::MAX);
        frame.time_base = time_base;
        let packets = enc.encode(&frame, alpha.as_ref())?;
        feed(&enc, &mut mux, packets, &mut merge)?;
        n += 1;
        job.progress.frame(n);
    }
    stopped(job.stop)?;
    if n == 0 {
        return Err("the source gave no frames".into());
    }
    let packets = enc.flush()?;
    feed(&enc, &mut mux, packets, &mut merge)?;
    let mut mux = mux.ok_or_else(|| Fail::from("the encoder gave no parameter sets"))?;
    merge.upto(&mut mux, None)?;
    job.progress.step("mux", 1, 1, None);
    mux.finish()?;
    Ok((device, n))
}

fn consume_animation(job: &Job, rx: &Receiver<Result<Pic, Fail>>) -> Result<(String, u64), Fail> {
    let (w, h) = job.size;
    let fps = (f64::from(job.fps.0), f64::from(job.fps.1.max(1)));
    let at = |n: u64, unit: f64| (n as f64 * unit * fps.1 / fps.0).round() as u64;
    let mut n = 0u64;
    match job.encoder {
        Encoder::Gif { .. } => {
            let mut enc = gif::Encoder::new(w, h, gif::EncodeOptions::default())?;
            while let Some(pic) = next_pic(rx, job.stop)? {
                let Pic::Rgba(img) = pic else {
                    return Err("a picture in the wrong form for the GIF encoder".into());
                };
                let delay = (at(n + 1, 100.0) - at(n, 100.0)).clamp(2, u64::from(u16::MAX));
                enc.push(img, delay as u16)?;
                n += 1;
                job.progress.frame(n);
            }
            stopped(job.stop)?;
            if n == 0 {
                return Err("the source gave no frames".into());
            }
            job.progress.step("mux", 1, 1, None);
            write_all(job.partial, &enc.finish())?;
        }
        Encoder::Webp { quality, effort } => {
            let options = webp::Options {
                quality: f32::from(*quality),
                effort: *effort,
                ..webp::Options::default()
            };
            let mut enc = webp::AnimEncoder::new(w, h, options, 0)?;
            while let Some(pic) = next_pic(rx, job.stop)? {
                let Pic::Rgba(img) = pic else {
                    return Err("a picture in the wrong form for the WebP encoder".into());
                };
                let duration = (at(n + 1, 1000.0) - at(n, 1000.0)).max(1);
                enc.add(&img, u32::try_from(duration).unwrap_or(u32::MAX))?;
                n += 1;
                job.progress.frame(n);
            }
            stopped(job.stop)?;
            if n == 0 {
                return Err("the source gave no frames".into());
            }
            job.progress.step("mux", 1, 1, None);
            write_all(job.partial, &enc.finish()?)?;
        }
        _ => return Err("this encoder writes no animation".into()),
    }
    Ok(("CPU".into(), n))
}

fn write_all(path: &Path, data: &[u8]) -> Result<(), Fail> {
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()).into())
}
