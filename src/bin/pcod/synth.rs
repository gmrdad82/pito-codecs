use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pito_codecs::media::{self, AudioBuffer, Bitstream, Frame, Packet, PixelFormat, TimeBase};
use pito_codecs::{aac, h264, mkv, mp4, opus, vp9};

use super::Fail;
use super::convert;
use super::plan::{AAC_KBPS, OPUS_KBPS, partial};
use super::signals;

pub const BITS: u32 = 24;
pub const AMPLITUDE: f64 = 0.125;
pub const MIN_SIZE: (u32, u32) = (128, 96);
pub const MAX_SIDE: u32 = 8192;
pub const MAX_FPS: f64 = 240.0;
pub const MAX_SECONDS: f64 = 6.0 * 3600.0;
pub const H264_MAX_MBS: u64 = 139_264;
pub const H264_MAX_MBPS: u64 = 16_711_680;
const AUDIO_BLOCK: u64 = 4096;
const THREADS: usize = 4;
const H264_QP: u8 = 20;
const STOPPED: &str = "stopped before it finished";
const VP9_Q: u8 = 60;
const _: () = assert!(MAX_SECONDS * MAX_FPS <= (1u64 << BITS) as f64);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tone {
    Sine(f64),
    Silence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Out {
    Mp4,
    Mov,
    Mkv,
    Webm,
}

pub struct Job {
    pub output: PathBuf,
    pub size: (u32, u32),
    pub fps: (u32, u32),
    pub seconds: f64,
    pub audio: Vec<String>,
    pub rate: u32,
    pub force: bool,
}

pub fn size_of(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("'{text}' is not a size such as 1280x720");
    let (w, h) = text.split_once(['x', 'X']).ok_or_else(bad)?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(w) || !digits(h) {
        return Err(bad());
    }
    Ok((w.parse().map_err(|_| bad())?, h.parse().map_err(|_| bad())?))
}

pub fn tone_of(text: &str, rate: u32) -> Result<Tone, String> {
    let bad = || format!("unknown --audio entry '{text}': use sine:<hz> or silence");
    if text == "silence" {
        return Ok(Tone::Silence);
    }
    let hz: f64 = text
        .strip_prefix("sine:")
        .ok_or_else(bad)?
        .parse()
        .map_err(|_| bad())?;
    if !hz.is_finite() || hz <= 0.0 || hz >= f64::from(rate) / 2.0 {
        return Err(format!(
            "--audio {text}: the frequency must be above 0 and below {} Hz, half the rate",
            rate / 2
        ));
    }
    Ok(Tone::Sine(hz))
}

pub fn container_of(path: &Path) -> Result<Out, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("mp4") => Ok(Out::Mp4),
        Some("mov") => Ok(Out::Mov),
        Some("mkv") => Ok(Out::Mkv),
        Some("webm") => Ok(Out::Webm),
        _ => Err(format!(
            "{}: synth writes mp4, mov, mkv or webm",
            path.display()
        )),
    }
}

pub fn frame_count(seconds: f64, fps: (u32, u32)) -> u64 {
    (seconds * f64::from(fps.0) / f64::from(fps.1.max(1))).round() as u64
}

pub fn samples_at(frames: u64, fps: (u32, u32), rate: u32) -> u64 {
    let num = u128::from(frames) * u128::from(rate) * u128::from(fps.1);
    let den = u128::from(fps.0.max(1));
    u64::try_from((2 * num + den) / (2 * den)).unwrap_or(u64::MAX)
}

pub fn checked(job: &Job) -> Result<(Out, Vec<Tone>, u64), String> {
    let kind = container_of(&job.output)?;
    let (w, h) = job.size;
    if w == 0 || h == 0 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
        return Err(format!(
            "a size of {w}x{h}; 4:2:0 video needs an even width and height above 0"
        ));
    }
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!(
            "a size of {w}x{h}; synth draws from {}x{} to {MAX_SIDE}x{MAX_SIDE}",
            MIN_SIZE.0, MIN_SIZE.1
        ));
    }
    let fps = f64::from(job.fps.0) / f64::from(job.fps.1.max(1));
    if job.fps.0 == 0 || job.fps.1 == 0 || fps > MAX_FPS {
        return Err(format!(
            "a frame rate of {fps}; synth takes up to {MAX_FPS}"
        ));
    }
    if kind != Out::Webm {
        let mbs = u64::from(w.div_ceil(16)) * u64::from(h.div_ceil(16));
        if mbs > H264_MAX_MBS {
            return Err(format!(
                "a size of {w}x{h} is {mbs} macroblocks; H.264 level 6.2 holds {H264_MAX_MBS} (8192x4352), webm holds 8192x8192"
            ));
        }
        let per_second = u128::from(mbs) * u128::from(job.fps.0);
        if per_second > u128::from(H264_MAX_MBPS) * u128::from(job.fps.1) {
            return Err(format!(
                "{w}x{h} at {fps} fps is past H.264 level 6.2's {H264_MAX_MBPS} macroblocks a second; lower the size or the rate, or write webm"
            ));
        }
    }
    if !job.seconds.is_finite() || job.seconds <= 0.0 || job.seconds > MAX_SECONDS {
        return Err(format!(
            "a duration of {} s; synth takes more than 0 and up to {MAX_SECONDS} s",
            job.seconds
        ));
    }
    let frames = frame_count(job.seconds, job.fps);
    if frames == 0 {
        return Err(format!(
            "a duration of {} s is shorter than one frame",
            job.seconds
        ));
    }
    if job.audio.is_empty() {
        return Ok((kind, Vec::new(), frames));
    }
    if kind == Out::Webm && job.rate != 48_000 {
        return Err(format!(
            "a rate of {} Hz; WebM's Opus runs at 48000 Hz",
            job.rate
        ));
    }
    if kind != Out::Webm && !aac::RATES.contains(&job.rate) {
        return Err(format!(
            "a rate of {} Hz; AAC takes {}",
            job.rate,
            aac::RATES
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let tones = job
        .audio
        .iter()
        .map(|t| tone_of(t.trim(), job.rate))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((kind, tones, frames))
}

fn yuv(r: f64, g: f64, b: f64) -> [u8; 3] {
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
    [
        q(16.0 + 219.0 * y),
        q(128.0 + 224.0 * (b - y) / 1.8556),
        q(128.0 + 224.0 * (r - y) / 1.5748),
    ]
}

pub const WHITE: [u8; 3] = [235, 128, 128];
pub const BLACK: [u8; 3] = [16, 128, 128];

pub fn bars() -> [[u8; 3]; 7] {
    let l = 0.75;
    [
        yuv(l, l, l),
        yuv(l, l, 0.0),
        yuv(0.0, l, l),
        yuv(0.0, l, 0.0),
        yuv(l, 0.0, l),
        yuv(l, 0.0, 0.0),
        yuv(0.0, 0.0, l),
    ]
}

fn fill(f: &mut Frame, x0: u32, y0: u32, x1: u32, y1: u32, c: [u8; 3]) {
    let (x1, y1) = (x1.min(f.width), y1.min(f.height));
    let (x0, y0) = (x0.min(x1), y0.min(y1));
    let luma = &mut f.planes[0];
    for y in y0 as usize..y1 as usize {
        let row = y * luma.stride;
        luma.data[row + x0 as usize..row + x1 as usize].fill(c[0]);
    }
    for (p, v) in [(1, c[1]), (2, c[2])] {
        let plane = &mut f.planes[p];
        let (cx0, cx1) = (
            (x0 / 2) as usize,
            (x1.div_ceil(2) as usize).min(plane.width),
        );
        for y in (y0 / 2) as usize..(y1.div_ceil(2) as usize).min(plane.height) {
            let row = y * plane.stride;
            plane.data[row + cx0..row + cx1].fill(v);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub top: u32,
    pub cell: u32,
    pub bits: (u32, u32),
    pub digits: (u32, u32),
    pub mover: u32,
    pub step: u32,
}

impl Layout {
    pub fn new(w: u32, h: u32) -> Layout {
        let top = (h * 2 / 3) & !1;
        let band = h - top;
        Layout {
            top,
            cell: (w / (BITS + 2)) & !1,
            bits: (top + band / 10, top + band / 2),
            digits: (top + band / 2 + band / 10, h - band / 10),
            mover: ((top / 3).max(8) & !1).min(w / 4),
            step: (w / 48).max(2) & !1,
        }
    }

    #[cfg(test)]
    pub fn bit_centre(&self, k: u32) -> (u32, u32) {
        (
            self.cell * (k + 1) + self.cell / 2,
            (self.bits.0 + self.bits.1) / 2,
        )
    }

    pub fn mover_x(&self, w: u32, n: u64) -> u32 {
        let travel = u64::from(w - self.mover);
        let pos = (n * u64::from(self.step)) % (2 * travel);
        (if pos > travel { 2 * travel - pos } else { pos }) as u32
    }
}

const SEGMENTS: [u8; 10] = [
    0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110, 0b1101101, 0b1111101, 0b0000111,
    0b1111111, 0b1101111,
];

fn digit(f: &mut Frame, x: u32, (y0, y1): (u32, u32), w: u32, d: usize) {
    let t = ((y1 - y0) / 10).max(2).min((w / 3).max(1));
    let ym = (y0 + y1) / 2;
    let on = SEGMENTS[d % 10];
    let xr = x + w - t;
    let rects = [
        (x, y0, x + w, y0 + t),
        (xr, y0, x + w, ym),
        (xr, ym, x + w, y1),
        (x, y1 - t, x + w, y1),
        (x, ym, x + t, y1),
        (x, y0, x + t, ym),
        (x, ym - t / 2, x + w, ym - t / 2 + t),
    ];
    for (i, r) in rects.iter().enumerate() {
        if on & (1 << i) != 0 {
            fill(f, r.0, r.1, r.2, r.3, WHITE);
        }
    }
}

pub struct Pattern {
    base: Frame,
    layout: Layout,
    places: usize,
}

impl Pattern {
    pub fn new(w: u32, h: u32, frames: u64) -> Pattern {
        let layout = Layout::new(w, h);
        let mut base = Frame::new(PixelFormat::Yuv420p8, w, h, 0, TimeBase::new(1, 1));
        for (i, c) in bars().iter().enumerate() {
            let x0 = (i as u32 * w / 7) & !1;
            let x1 = if i == 6 {
                w
            } else {
                ((i as u32 + 1) * w / 7) & !1
            };
            fill(&mut base, x0, 0, x1, layout.top, *c);
        }
        fill(&mut base, 0, layout.top, w, h, BLACK);
        Pattern {
            base,
            layout,
            places: frames.saturating_sub(1).max(1).to_string().len(),
        }
    }

    pub fn draw(&self, n: u64, into: &mut Frame) {
        for (to, from) in into.planes.iter_mut().zip(&self.base.planes) {
            to.data.copy_from_slice(&from.data);
        }
        let (w, l) = (self.base.width, &self.layout);
        let x = l.mover_x(w, n);
        let y = ((l.top - l.mover) / 2) & !1;
        fill(into, x, y, x + l.mover, y + l.mover, BLACK);
        let inset = (l.mover / 4) & !1;
        fill(
            into,
            x + inset,
            y + inset,
            x + l.mover - inset,
            y + l.mover - inset,
            WHITE,
        );
        let code = n % (1 << BITS);
        for k in 0..BITS {
            if (code >> (BITS - 1 - k)) & 1 == 1 {
                let x0 = l.cell * (k + 1);
                fill(into, x0, l.bits.0, x0 + l.cell, l.bits.1, WHITE);
            }
        }
        let dh = l.digits.1.saturating_sub(l.digits.0);
        let places = self.places as u32;
        let room = w.saturating_sub(2 * l.cell) * 2 / (3 * places);
        let dw = (dh / 2).min(room).max(2);
        let mut rest = n;
        for i in (0..places).rev() {
            let d = (rest % 10) as usize;
            rest /= 10;
            let x0 = l.cell + i * (dw + dw / 2);
            digit(into, x0, l.digits, dw, d);
        }
    }

    pub fn frame(&self) -> Frame {
        self.base.clone()
    }
}

#[cfg(test)]
pub fn read_counter(luma: &[u8], stride: usize, w: u32, h: u32) -> u64 {
    let l = Layout::new(w, h);
    (0..BITS).fold(0u64, |acc, k| {
        let (x, y) = l.bit_centre(k);
        let on = luma
            .get(y as usize * stride + x as usize)
            .is_some_and(|&v| v > 125);
        (acc << 1) | u64::from(on)
    })
}

pub struct Tones {
    tone: Tone,
    at: u64,
    buf: AudioBuffer,
}

impl Tones {
    pub fn new(tone: Tone, rate: u32) -> Tones {
        Tones {
            tone,
            at: 0,
            buf: AudioBuffer {
                samples: Vec::new(),
                rate,
                channels: 2,
            },
        }
    }

    pub fn next(&mut self, n: usize) -> &AudioBuffer {
        let samples = &mut self.buf.samples;
        samples.clear();
        samples.resize(n * 2, 0.0);
        if let Tone::Sine(hz) = self.tone {
            let w = std::f64::consts::TAU * hz / f64::from(self.buf.rate);
            for (k, pair) in samples.chunks_exact_mut(2).enumerate() {
                let i = (self.at + k as u64) as f64;
                let v = (AMPLITUDE * (w * i).sin()) as f32;
                pair.fill(v);
            }
        }
        self.at += n as u64;
        &self.buf
    }
}

enum AEnc {
    Aac(Box<aac::Encoder>),
    Opus(Box<opus::enc::Encoder>),
}

impl AEnc {
    fn encode(&mut self, buf: &AudioBuffer) -> Result<Vec<Packet>, Fail> {
        Ok(match self {
            AEnc::Aac(e) => e.encode(buf)?,
            AEnc::Opus(e) => e.encode(buf)?,
        })
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Fail> {
        Ok(match self {
            AEnc::Aac(e) => e.flush(),
            AEnc::Opus(e) => e.flush()?,
        })
    }
}

struct Lane {
    tones: Tones,
    enc: AEnc,
    track: usize,
    shift: i64,
    held: Option<Packet>,
    end: i64,
}

enum VEnc {
    H264(Box<h264::Encoder>),
    Vp9(Box<vp9::Encoder>),
}

enum Sink {
    Mp4(Box<mp4::Writer>),
    Mkv(Box<mkv::Writer>),
}

struct Mux {
    sink: Sink,
    video: usize,
    step: i64,
}

impl Mux {
    fn video(&mut self, p: Packet) -> Result<(), Fail> {
        match &mut self.sink {
            Sink::Mp4(w) => w.push(
                self.video,
                &Packet {
                    pts: p.pts.saturating_mul(self.step),
                    dts: p.dts.saturating_mul(self.step),
                    ..p
                },
            )?,
            Sink::Mkv(w) => w.push(self.video, &p)?,
        }
        Ok(())
    }

    fn audio(&mut self, lane: &mut Lane, packets: Vec<Packet>) -> Result<(), Fail> {
        for mut p in packets {
            p.pts += lane.shift;
            p.dts += lane.shift;
            if let Some(prev) = lane.held.replace(p) {
                match &mut self.sink {
                    Sink::Mp4(w) => w.push(lane.track, &prev)?,
                    Sink::Mkv(w) => w.push(lane.track, &prev)?,
                }
            }
        }
        Ok(())
    }

    fn last_audio(&mut self, lane: &mut Lane) -> Result<(), Fail> {
        let (track, end) = (lane.track, lane.end);
        lane.held.take().into_iter().try_for_each(|p| {
            let left = u32::try_from(end.saturating_sub(p.pts).max(1)).unwrap_or(u32::MAX);
            match &mut self.sink {
                Sink::Mp4(w) => w.push_timed(track, &p, left)?,
                Sink::Mkv(w) => w.push_timed(track, &p, left)?,
            }
            Ok(())
        })
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

fn h264_config(w: u32, h: u32, fps: (u32, u32), gop: u32) -> h264::EncoderConfig {
    h264::EncoderConfig {
        width: w,
        height: h,
        fps,
        profile: h264::Profile::High,
        preset: h264::Preset::Balanced,
        rate: media::Rate::Qp(H264_QP),
        gop,
        b_frames: 2,
        threads: THREADS,
        bitstream: Bitstream::LengthPrefixed,
    }
}

fn go_on(stop: &AtomicBool) -> Result<(), Fail> {
    if stop.load(Ordering::Relaxed) {
        return Err(STOPPED.into());
    }
    Ok(())
}

fn write(
    path: &Path,
    kind: Out,
    job: &Job,
    tones: &[Tone],
    frames: u64,
    stop: &AtomicBool,
) -> Result<&'static str, Fail> {
    let (w, h) = job.size;
    let fps = job.fps;
    let gop = (fps.0.div_ceil(fps.1) * 2).clamp(2, 240);
    let frame_tb = TimeBase::new(fps.1, fps.0);
    let mut venc = if kind == Out::Webm {
        let mut e = vp9::Encoder::new(vp9::EncoderConfig {
            width: w,
            height: h,
            fps,
            rate: vp9::Rate::Q(VP9_Q),
            gop,
            threads: THREADS,
            aq: false,
        })?;
        e.set_color(2, false)?;
        VEnc::Vp9(Box::new(e))
    } else {
        VEnc::H264(Box::new(h264::Encoder::new(h264_config(w, h, fps, gop))?))
    };
    let total = i64::try_from(samples_at(frames, fps, job.rate)).unwrap_or(i64::MAX);
    let mut mux = match (kind, &venc) {
        (Out::Mp4 | Out::Mov, VEnc::H264(e)) => {
            let brand = if kind == Out::Mov {
                mp4::Brand::Mov
            } else {
                mp4::Brand::Mp4
            };
            let mut writer = mp4::Writer::create_with(
                path,
                mp4::Options {
                    brand,
                    ..Default::default()
                },
            )?;
            writer.set_movie_timescale(if tones.is_empty() { 1000 } else { job.rate });
            let entry =
                mp4::SampleEntry::h264(mp4::AvcConfig::parse(&e.avcc())?, w as u16, h as u16);
            let timescale = fps
                .0
                .checked_mul(64)
                .ok_or("the frame rate is out of range")?;
            let video = writer.add_track(mp4::TrackConfig::new(entry, timescale))?;
            Mux {
                sink: Sink::Mp4(Box::new(writer)),
                video,
                step: i64::from(fps.1) * 64,
            }
        }
        (_, venc) => {
            let options = if kind == Out::Webm {
                mkv::Options::webm()
            } else {
                mkv::Options::default()
            };
            let mut writer = mkv::Writer::create_with(path, options)?;
            let cfg = match venc {
                VEnc::H264(e) => mkv::TrackConfig::h264(e.avcc(), w, h, frame_tb),
                VEnc::Vp9(_) => mkv::TrackConfig::vp9(w, h, frame_tb),
            };
            let video = writer.add_track(cfg)?;
            Mux {
                sink: Sink::Mkv(Box::new(writer)),
                video,
                step: 1,
            }
        }
    };
    let mut lanes = Vec::with_capacity(tones.len());
    for tone in tones {
        let lane = match &mut mux.sink {
            Sink::Mp4(writer) => {
                let e = aac::Encoder::new(aac::EncoderConfig {
                    rate: job.rate,
                    channels: 2,
                    bitrate: AAC_KBPS * 1000,
                })?;
                let priming = i64::from(e.priming());
                let entry = mp4::SampleEntry::aac(e.extradata(), 2, job.rate, AAC_KBPS * 1000);
                let mut cfg = mp4::TrackConfig::new(entry, job.rate);
                cfg.media_start = Some(priming);
                let track = writer.add_track(cfg)?;
                (AEnc::Aac(Box::new(e)), track, 0, priming + total)
            }
            Sink::Mkv(writer) if kind == Out::Webm => {
                let e =
                    opus::enc::Encoder::new(opus::enc::EncoderConfig::new(2, OPUS_KBPS * 1000))?;
                let track = writer.add_track(mkv::TrackConfig::opus(e.codec_private()))?;
                let shift = -i64::from(e.pre_skip());
                (AEnc::Opus(Box::new(e)), track, shift, total)
            }
            Sink::Mkv(writer) => {
                let e = aac::Encoder::new(aac::EncoderConfig {
                    rate: job.rate,
                    channels: 2,
                    bitrate: AAC_KBPS * 1000,
                })?;
                let priming = i64::from(e.priming());
                let mut cfg = mkv::TrackConfig::aac(e.extradata(), job.rate, 2);
                cfg.codec_delay =
                    u64::try_from(priming).unwrap_or(0) * 1_000_000_000 / u64::from(job.rate);
                let track = writer.add_track(cfg)?;
                (AEnc::Aac(Box::new(e)), track, -priming, total)
            }
        };
        lanes.push(Lane {
            tones: Tones::new(*tone, job.rate),
            enc: lane.0,
            track: lane.1,
            shift: lane.2,
            held: None,
            end: lane.3,
        });
    }
    let pattern = Pattern::new(w, h, frames);
    let mut frame = pattern.frame();
    let mut done_samples = 0u64;
    for n in 0..frames {
        go_on(stop)?;
        pattern.draw(n, &mut frame);
        frame.pts = i64::try_from(n).unwrap_or(i64::MAX);
        frame.time_base = frame_tb;
        let packets = match &mut venc {
            VEnc::H264(e) => e.encode(&frame)?,
            VEnc::Vp9(e) => e.encode(&frame)?,
        };
        for p in packets {
            mux.video(p)?;
        }
        let upto = samples_at(n + 1, fps, job.rate);
        while !lanes.is_empty() && done_samples < upto {
            go_on(stop)?;
            let count = (upto - done_samples).min(AUDIO_BLOCK) as usize;
            done_samples += count as u64;
            for lane in &mut lanes {
                let buf = lane.tones.next(count);
                let packets = lane.enc.encode(buf)?;
                mux.audio(lane, packets)?;
            }
        }
    }
    let tail = match &mut venc {
        VEnc::H264(e) => e.flush()?,
        VEnc::Vp9(e) => e.flush()?,
    };
    for p in tail {
        mux.video(p)?;
    }
    for lane in &mut lanes {
        let packets = lane.enc.flush()?;
        mux.audio(lane, packets)?;
        mux.last_audio(lane)?;
    }
    mux.finish()?;
    Ok(match kind {
        Out::Webm => "VP9 and Opus",
        _ => "H.264 and AAC",
    })
}

pub fn exit(job: &Job) -> ExitCode {
    signals::catch();
    match run(&mut std::io::stdout().lock(), job, &signals::STOP) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if let Some((code, message)) = signals::caught() {
                let _ = writeln!(std::io::stderr(), "pcod: {message}");
                return ExitCode::from(code);
            }
            convert::refuse(&e.to_string())
        }
    }
}

pub fn run(out: &mut impl Write, job: &Job, stop: &AtomicBool) -> Result<(), Fail> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", job.output.display());
    let (kind, tones, frames) = checked(job).map_err(|e| at(&e))?;
    if !job.force && job.output.exists() {
        return Err(at(&"already exists; use --force to replace it").into());
    }
    let temp = partial(&job.output).map_err(|e| at(&e))?;
    let start = Instant::now();
    let made = write(&temp, kind, job, &tones, frames, stop).and_then(|codecs| {
        std::fs::rename(&temp, &job.output)
            .map(|_| codecs)
            .map_err(Fail::from)
    });
    let codecs = match made {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            return Err(at(&e).into());
        }
    };
    let took = start.elapsed().as_secs_f64().max(1e-9);
    writeln!(
        out,
        "synth   {}x{} at {}/{} fps, {frames} frames, {} audio track{} at {} Hz, {codecs}",
        job.size.0,
        job.size.1,
        job.fps.0,
        job.fps.1,
        tones.len(),
        if tones.len() == 1 { "" } else { "s" },
        job.rate
    )?;
    writeln!(
        out,
        "wrote   {} ({:.1} MB) in {took:.2} s",
        job.output.display(),
        std::fs::metadata(&job.output)?.len() as f64 / 1e6
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(out: &str, size: (u32, u32), seconds: f64, audio: &[&str]) -> Job {
        Job {
            output: PathBuf::from(out),
            size,
            fps: (30, 1),
            seconds,
            audio: audio.iter().map(|s| s.to_string()).collect(),
            rate: 48_000,
            force: false,
        }
    }

    #[test]
    fn sizes_parse_and_bad_ones_are_refused() {
        assert_eq!(size_of("320x240"), Ok((320, 240)));
        assert_eq!(size_of("1280X720"), Ok((1280, 720)));
        for bad in ["320", "x240", "320x", "-2x4", "3.5x4", "axb", ""] {
            assert!(size_of(bad).is_err(), "{bad}");
        }
        for size in [
            (321, 240),
            (320, 241),
            (0, 240),
            (320, 0),
            (64, 48),
            (9000, 240),
        ] {
            let e = checked(&job("a.mp4", size, 1.0, &[])).unwrap_err();
            assert!(!e.contains('\n') && e.contains("a size of"), "{e}");
        }
    }

    #[test]
    fn durations_rates_and_containers_are_checked() {
        for s in [0.0, -1.0, f64::NAN, f64::INFINITY, MAX_SECONDS + 1.0, 0.001] {
            let e = checked(&job("a.mp4", (320, 240), s, &[])).unwrap_err();
            assert!(e.contains("duration"), "{e}");
        }
        assert!(checked(&job("a.avi", (320, 240), 1.0, &[])).is_err());
        let mut j = job("a.webm", (320, 240), 1.0, &["silence"]);
        j.rate = 44_100;
        assert!(checked(&j).unwrap_err().contains("48000"));
        let mut j = job("a.mp4", (320, 240), 1.0, &["silence"]);
        j.rate = 12_345;
        assert!(checked(&j).is_err());
        j.rate = 44_100;
        assert!(checked(&j).is_ok());
        let mut j = job("a.mp4", (320, 240), 1.0, &[]);
        j.fps = (241, 1);
        assert!(checked(&j).unwrap_err().contains("frame rate"));
        let (kind, tones, frames) =
            checked(&job("A.MKV", (320, 240), 2.0, &["sine:440", "silence"])).unwrap();
        assert_eq!((kind, frames), (Out::Mkv, 60));
        assert_eq!(tones, vec![Tone::Sine(440.0), Tone::Silence]);
    }

    #[test]
    fn audio_entries_are_sines_or_silence() {
        assert_eq!(tone_of("sine:880", 48_000), Ok(Tone::Sine(880.0)));
        assert_eq!(tone_of("sine:1000.5", 48_000), Ok(Tone::Sine(1000.5)));
        assert_eq!(tone_of("silence", 48_000), Ok(Tone::Silence));
        for bad in [
            "noise",
            "sine",
            "sine:",
            "sine:abc",
            "Sine:440",
            "sine:0",
            "sine:-5",
            "sine:24000",
            "sine:inf",
        ] {
            let e = tone_of(bad, 48_000).unwrap_err();
            assert!(!e.contains('\n'), "{e}");
        }
        assert!(
            tone_of("noise", 48_000)
                .unwrap_err()
                .contains("unknown --audio entry 'noise'")
        );
    }

    #[test]
    fn the_audio_matches_the_video_length() {
        assert_eq!(samples_at(30, (30, 1), 48_000), 48_000);
        assert_eq!(samples_at(30_000, (30_000, 1001), 48_000), 48_048_000);
        assert_eq!(frame_count(1.0, (30_000, 1001)), 30);
        assert_eq!(frame_count(0.5, (25, 1)), 13);
    }

    #[test]
    fn every_frame_carries_its_own_counter() {
        let (w, h) = (320, 240);
        let p = Pattern::new(w, h, 2000);
        let mut f = p.frame();
        let mut seen = Vec::new();
        for n in [0u64, 1, 2, 7, 255, 1023, 1999, (1 << BITS) - 1] {
            p.draw(n, &mut f);
            assert_eq!(read_counter(&f.planes[0].data, f.planes[0].stride, w, h), n);
            seen.push(f.planes[0].data.clone());
        }
        for (i, a) in seen.iter().enumerate() {
            for b in &seen[i + 1..] {
                assert_ne!(a, b);
            }
        }
        p.draw(1 << BITS, &mut f);
        assert_eq!(read_counter(&f.planes[0].data, f.planes[0].stride, w, h), 0);
    }

    #[test]
    fn the_smallest_size_still_reads_its_counter() {
        let (w, h) = MIN_SIZE;
        let p = Pattern::new(w, h, 100);
        let mut f = p.frame();
        for n in [0u64, 5, 99] {
            p.draw(n, &mut f);
            assert_eq!(read_counter(&f.planes[0].data, f.planes[0].stride, w, h), n);
        }
    }

    #[test]
    fn h264_sizes_stay_inside_level_6_2_and_webm_takes_the_largest() {
        let e = checked(&job("a.mp4", (8192, 8192), 1.0, &[])).unwrap_err();
        assert!(e.contains("262144 macroblocks") && !e.contains('\n'), "{e}");
        assert!(checked(&job("a.mkv", (8192, 4368), 1.0, &[])).is_err());
        assert!(checked(&job("a.webm", (8192, 8192), 1.0, &[])).is_ok());
        assert!(checked(&job("a.mov", (8192, 4352), 1.0, &[])).is_ok());
        let mut j = job("a.mp4", (8192, 4352), 1.0, &[]);
        j.fps = (240, 1);
        assert!(checked(&j).unwrap_err().contains("a second"));
        j.fps = (120, 1);
        assert!(checked(&j).is_ok());
        j.output = PathBuf::from("a.webm");
        j.fps = (240, 1);
        assert!(checked(&j).is_ok());
        let mut j = job("a.mp4", (3840, 2160), 1.0, &[]);
        j.fps = (240, 1);
        assert!(checked(&j).is_ok());
    }

    #[test]
    fn tall_narrow_frames_still_draw_every_digit() {
        let (w, h) = (128, 8192);
        let p = Pattern::new(w, h, 300);
        let mut f = p.frame();
        p.draw(288, &mut f);
        assert_eq!(
            read_counter(&f.planes[0].data, f.planes[0].stride, w, h),
            288
        );
        let l = Layout::new(w, h);
        let stride = f.planes[0].stride;
        let y = (l.digits.0 + l.digits.1) / 2;
        let lit: Vec<u32> = (0..w)
            .filter(|&x| f.planes[0].data[y as usize * stride + x as usize] == WHITE[0])
            .collect();
        assert!(lit.len() >= 6, "{lit:?}");
        assert!(lit.iter().all(|&x| x < w));
        let last = *lit.last().unwrap();
        assert!(last > w / 2, "the third digit is drawn too: {lit:?}");
    }

    #[test]
    fn the_mover_moves_and_stays_inside() {
        let l = Layout::new(320, 240);
        let xs: Vec<u32> = (0..200).map(|n| l.mover_x(320, n)).collect();
        assert!(xs.iter().all(|&x| x + l.mover <= 320));
        assert!(xs.windows(2).all(|p| p[0] != p[1]));
        assert_eq!(xs[0], 0);
    }

    #[test]
    fn the_bars_are_bt709_and_the_first_is_grey() {
        let b = bars();
        assert_eq!(b[0][1..], [128, 128]);
        assert_eq!(b[0][0], 180);
        assert!(b[6][1] > 200 && b[5][2] > 200);
    }

    #[test]
    fn a_sine_plays_at_its_level_and_silence_is_zero() {
        let mut t = Tones::new(Tone::Sine(1000.0), 48_000);
        let a = t.next(480).clone();
        let b = t.next(480).clone();
        let peak = a
            .samples
            .iter()
            .chain(&b.samples)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((f64::from(peak) - AMPLITUDE).abs() < 1e-6);
        assert_eq!(a.samples[0], 0.0);
        assert!((b.samples[0] - a.samples[0]).abs() < 1e-6);
        assert_eq!(a.samples[2], a.samples[3]);
        let mut s = Tones::new(Tone::Silence, 48_000);
        assert!(s.next(100).samples.iter().all(|&v| v == 0.0));
        assert_eq!(s.next(7).samples.len(), 14);
        let mut t = Tones::new(Tone::Sine(440.0), 44_100);
        let first = t.next(64).clone();
        assert_eq!(
            (first.rate, first.channels, first.samples.len()),
            (44_100, 2, 128)
        );
        let w = std::f64::consts::TAU * 440.0 / 44_100.0;
        let again = t.next(4);
        assert!((f64::from(again.samples[0]) - AMPLITUDE * (w * 64.0).sin()).abs() < 1e-6);
    }

    #[test]
    fn the_rate_is_checked_only_when_there_is_audio() {
        let mut j = job("a.mp4", (320, 240), 1.0, &[]);
        j.rate = 22_000;
        assert!(checked(&j).is_ok());
        j.audio = vec!["sine:440".into()];
        let e = checked(&j).unwrap_err();
        assert!(e.contains("AAC takes 96000") && e.contains("7350"), "{e}");
        j.rate = 32_000;
        assert!(checked(&j).is_ok());
        let mut j = job("a.webm", (320, 240), 1.0, &[]);
        j.rate = 44_100;
        assert!(checked(&j).is_ok());
        j.audio = vec!["silence".into()];
        assert!(checked(&j).unwrap_err().contains("48000"));
    }

    #[test]
    fn the_longest_job_still_numbers_every_frame() {
        assert!(frame_count(MAX_SECONDS, (240, 1)) <= 1 << BITS);
        let mut j = job("a.mp4", (320, 240), MAX_SECONDS, &[]);
        j.fps = (240, 1);
        assert_eq!(checked(&j).unwrap().2, 5_184_000);
        let (w, h) = MIN_SIZE;
        let p = Pattern::new(w, h, 5_184_000);
        let mut f = p.frame();
        for n in [1u64 << 20, 5_183_999] {
            p.draw(n, &mut f);
            assert_eq!(read_counter(&f.planes[0].data, f.planes[0].stride, w, h), n);
        }
    }

    #[test]
    fn a_stop_leaves_no_partial_file_behind() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/synth-unit/stop");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut j = job("x.mp4", (320, 240), 1.0, &["sine:440"]);
        j.output = d.join("x.mp4");
        let stop = AtomicBool::new(true);
        let e = run(&mut Vec::new(), &j, &stop).unwrap_err().to_string();
        assert!(e.contains(STOPPED), "{e}");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    }

    fn scratch(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/synth-unit")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    struct Lines {
        allowed: usize,
        seen: usize,
    }

    impl Write for Lines {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.seen >= self.allowed {
                return Err(std::io::Error::other("the reader went away"));
            }
            self.seen += buf.iter().filter(|&&b| b == b'\n').count();
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_report_line_that_fails_to_write_is_an_error_and_the_file_stays() {
        for allowed in [0, 1] {
            let d = scratch(&format!("report-{allowed}"));
            let mut j = job("x.mp4", MIN_SIZE, 1.0 / 30.0, &[]);
            j.output = d.join("x.mp4");
            let mut out = Lines { allowed, seen: 0 };
            let e = run(&mut out, &j, &AtomicBool::new(false))
                .unwrap_err()
                .to_string();
            assert!(e.contains("the reader went away"), "{e}");
            assert_eq!(out.seen, allowed);
            assert!(j.output.is_file());
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        }
    }

    #[test]
    fn an_output_folder_that_does_not_exist_fails_in_one_line_for_every_container() {
        let d = scratch("no-folder");
        for name in ["x.mp4", "x.mov", "x.mkv", "x.webm"] {
            let mut j = job(name, MIN_SIZE, 1.0 / 30.0, &["sine:440"]);
            j.output = d.join("missing").join(name);
            let e = run(&mut Vec::new(), &j, &AtomicBool::new(false))
                .unwrap_err()
                .to_string();
            assert!(e.starts_with(&j.output.display().to_string()), "{e}");
            assert!(!e.contains('\n'), "{e}");
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    }

    #[test]
    fn the_encoder_gives_rising_decode_times_and_an_mp4_refuses_any_other_order() {
        let (w, h) = MIN_SIZE;
        let mut enc = h264::Encoder::new(h264_config(w, h, (30, 1), 60)).unwrap();
        let pattern = Pattern::new(w, h, 40);
        let mut frame = pattern.frame();
        frame.time_base = TimeBase::new(1, 30);
        let mut packets = Vec::new();
        for n in 0..40 {
            pattern.draw(n, &mut frame);
            frame.pts = n as i64;
            packets.extend(enc.encode(&frame).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        assert_eq!(packets.len(), 40);
        assert!(packets.windows(2).all(|p| p[0].dts < p[1].dts));
        assert!(packets.iter().any(|p| p.pts != p.dts));

        let d = scratch("order");
        let mut writer = mp4::Writer::create(d.join("x.mp4")).unwrap();
        let entry = mp4::SampleEntry::h264(
            mp4::AvcConfig::parse(&enc.avcc()).unwrap(),
            w as u16,
            h as u16,
        );
        let video = writer
            .add_track(mp4::TrackConfig::new(entry, 30 * 64))
            .unwrap();
        let mut mux = Mux {
            sink: Sink::Mp4(Box::new(writer)),
            video,
            step: 64,
        };
        let mut late = packets[1].clone();
        mux.video(late.clone()).unwrap();
        late.dts -= 1;
        let e = mux.video(late).unwrap_err().to_string();
        assert!(e.contains("decode order"), "{e}");
    }
}
