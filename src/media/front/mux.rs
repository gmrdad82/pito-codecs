use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError, mpsc};

use crate::gpu::{Color, Device, EncoderConfig as GpuConfig, HwEncoder, Profile as GpuProfile};
use crate::h264::{Encoder, EncoderConfig, Preset, Profile};
use crate::media::{Bitstream, Rate};
use crate::mp4::{
    AvcConfig, HevcConfig, SampleEntry, TrackConfig, VpcConfig, Writer, rotation_matrix,
};

use super::audio::ffmpeg_pcm;
use super::demux::Demux;
use super::export::{ExportOptions, Job, Part, Progress, Stage, Tell, WRITTEN, reencode};
use super::ffmpeg::Ffmpeg;
use super::native::{threads, video_track};
use super::resident::Resident;
use super::route::{Picked, Prefs};
use super::video::frame_at;
use crate::media::{AudioBuffer, Codec, Error, Frame, Packet, PixelFormat, Result, Route};

pub(super) const RATE: u32 = 48_000;

struct Held {
    data: Vec<u8>,
    pts: i64,
    dts: i64,
    key: bool,
    entry: u32,
}

pub(super) struct Mux {
    w: Writer<File>,
    track: usize,
    entry: u32,
    held: Option<Held>,
    spare: Vec<u8>,
    last: Option<i64>,
    step: i64,
}

impl Mux {
    fn push(&mut self, data: &[u8], pts: i64, dts: i64, key: bool, entry: u32) -> Result<()> {
        let dts = match self.last {
            Some(l) if dts <= l => l.saturating_add(1),
            _ => dts,
        };
        if let Some(h) = self.held.take() {
            self.write(h, dts)?;
        }
        let mut buf = std::mem::take(&mut self.spare);
        buf.clear();
        buf.extend_from_slice(data);
        self.held = Some(Held {
            data: buf,
            pts,
            dts,
            key,
            entry,
        });
        self.last = Some(dts);
        Ok(())
    }

    fn write(&mut self, h: Held, next: i64) -> Result<()> {
        if h.entry != self.entry {
            self.w.use_entry(self.track, h.entry)?;
            self.entry = h.entry;
        }
        let duration = u32::try_from(next.saturating_sub(h.dts).max(1)).unwrap_or(u32::MAX);
        self.w
            .push_sample(self.track, &h.data, h.pts, h.dts, duration, h.key)?;
        self.spare = h.data;
        Ok(())
    }

    pub(super) fn close(&mut self) -> Result<()> {
        if let Some(h) = self.held.take() {
            let next = h.dts.saturating_add(self.step);
            self.write(h, next)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Entries {
    ids: HashMap<Vec<u8>, u32>,
    first: Option<SampleEntry>,
    later: Vec<SampleEntry>,
}

impl Entries {
    fn id(&mut self, key: Vec<u8>, entry: SampleEntry) -> u32 {
        if let Some(&id) = self.ids.get(&key) {
            return id;
        }
        let id = self.ids.len() as u32;
        self.ids.insert(key, id);
        if self.first.is_none() {
            self.first = Some(entry);
        } else {
            self.later.push(entry);
        }
        id
    }
}

pub(super) enum VEnc {
    Cpu(Box<Encoder>),
    Gpu(Box<HwEncoder>),
    Vp9(Box<crate::vp9::Encoder>, Vec<u8>),
}

impl VEnc {
    pub(super) fn encode(&mut self, f: &Frame) -> Result<Vec<Packet>> {
        match self {
            VEnc::Cpu(e) => e.encode(f),
            VEnc::Gpu(e) => e.encode(f),
            VEnc::Vp9(e, _) => e.encode(f),
        }
    }

    pub(super) fn encode_gpu(&mut self, f: &crate::gpu::GpuFrame) -> Result<Vec<Packet>> {
        match self {
            VEnc::Cpu(e) => e.encode(&f.download()?),
            VEnc::Gpu(e) => e.encode_gpu(f),
            VEnc::Vp9(e, _) => e.encode(&f.download()?),
        }
    }

    pub(super) fn on_gpu(&self) -> bool {
        matches!(self, VEnc::Gpu(_))
    }

    pub(super) fn composes(&self) -> bool {
        matches!(self, VEnc::Gpu(e) if e.composes())
    }

    pub(super) fn encode_gpu_over(
        &mut self,
        f: &crate::gpu::GpuFrame,
        layer: &std::sync::Arc<crate::media::Rgba>,
    ) -> Result<Vec<Packet>> {
        match self {
            VEnc::Gpu(e) => e.encode_gpu_over(f, layer),
            _ => Err(Error::Unsupported(
                "this encoder can't draw an overlay on the GPU".to_string(),
            )),
        }
    }

    fn restart(&mut self, config: &GpuConfig) -> Result<()> {
        match self {
            VEnc::Gpu(e) => e.restart(config),
            _ => Err(Error::Unsupported(
                "only the GPU encoder restarts".to_string(),
            )),
        }
    }

    pub(super) fn flush(&mut self) -> Result<Vec<Packet>> {
        match self {
            VEnc::Cpu(e) => e.flush(),
            VEnc::Gpu(e) => e.flush(),
            VEnc::Vp9(e, _) => e.flush(),
        }
    }

    fn extradata(&self) -> Option<Vec<u8>> {
        match self {
            VEnc::Cpu(e) => Some(e.avcc()),
            VEnc::Gpu(e) => e.extradata().map(<[u8]>::to_vec),
            VEnc::Vp9(_, vpcc) => Some(vpcc.clone()),
        }
    }
}

fn vp9_config(color: (u8, bool), size: (u32, u32), fps: (u32, u32)) -> VpcConfig {
    VpcConfig::new((0, 8, 1), color, size, fps)
}

fn vp9_encoder(
    (size, fps): ((u32, u32), (u32, u32)),
    crf: u8,
    gop: u32,
    threads: usize,
) -> Result<crate::vp9::Encoder> {
    crate::vp9::Encoder::new(crate::vp9::EncoderConfig {
        width: size.0,
        height: size.1,
        fps,
        rate: crate::vp9::Rate::Crf((crf as u32 * 7 / 4).min(63) as u8),
        gop,
        threads,
        aq: false,
    })
}

fn vp9_keyframe(demux: &Demux) -> Result<crate::vp9::KeyframeInfo> {
    let track = demux.video()?;
    let mut buf = Vec::new();
    for k in 0..demux.samples(track) {
        if demux.sample(track, k).is_some_and(|s| s.2) {
            demux.read_into(track, k, &mut buf)?;
            if let Some(info) = crate::vp9::keyframe_info(&buf) {
                return Ok(info);
            }
        }
    }
    Err(Error::Invalid("the VP9 video has no keyframe".to_string()))
}

pub(super) type Look = ((u16, u16), Color);

fn gpu_config(
    codec: Codec,
    size: (u32, u32),
    fps: (u32, u32),
    format: PixelFormat,
    (crf, gop): (u8, u32),
    (sar, color): Look,
) -> GpuConfig {
    GpuConfig {
        width: size.0,
        height: size.1,
        fps,
        profile: match (codec, format) {
            (Codec::Hevc, PixelFormat::Yuv420p10) => GpuProfile::Main10,
            (Codec::Hevc, _) => GpuProfile::Main,
            _ => GpuProfile::High,
        },
        rate: Rate::Crf(crf),
        gop,
        b_frames: 0,
        bitstream: Bitstream::LengthPrefixed,
        sar,
        color,
    }
}

pub(super) fn gpu_check(
    device: &mut Option<Device>,
    codec: Codec,
    size: (u32, u32),
    fps: (u32, u32),
    format: PixelFormat,
    look: Look,
) -> Result<HwEncoder> {
    let config = gpu_config(codec, size, fps, format, (20, 60), look);
    if device.is_none() {
        *device = Device::open().ok();
    }
    match device {
        Some(d) => HwEncoder::on(d, codec, &config),
        None => HwEncoder::new(codec, &config),
    }
}

fn tuning(fps: (u32, u32), crf: u8, edge: bool, frames: i64) -> (u8, u32) {
    let gop = if edge {
        (frames.max(1) as u32).saturating_add(1)
    } else {
        (fps.0 / fps.1.max(1)).clamp(1, 120) * 2
    };
    (if edge { crf.saturating_sub(2) } else { crf }, gop)
}

fn job_config(job: &Job, codec: Codec, crf: u8, edge: bool, frames: i64) -> GpuConfig {
    let tuned = tuning(job.fps, crf, edge, frames);
    gpu_config(codec, job.size, job.fps, job.format, tuned, job.look)
}

fn encoder(
    job: &Job,
    video: &Picked,
    crf: u8,
    edge: bool,
    frames: i64,
    threads: usize,
) -> Result<VEnc> {
    if video.route == Route::Gpu {
        let config = job_config(job, video.codec, crf, edge, frames);
        let spare = job
            .spare
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut e) = spare
            && e.restart(&config).is_ok()
        {
            return Ok(VEnc::Gpu(Box::new(e)));
        }
        let e = match &job.device {
            Some(d) => HwEncoder::on(d, video.codec, &config)?,
            None => HwEncoder::new(video.codec, &config)?,
        };
        return Ok(VEnc::Gpu(Box::new(e)));
    }
    let shape = (job.size, job.fps, job.format);
    encoder_for(shape, video, crf, edge, frames, threads)
}

pub(super) type Shape = ((u32, u32), (u32, u32), PixelFormat);

pub(super) fn encoder_for(
    (size, fps, format): Shape,
    video: &Picked,
    crf: u8,
    edge: bool,
    frames: i64,
    threads: usize,
) -> Result<VEnc> {
    let (crf, gop) = tuning(fps, crf, edge, frames);
    if video.route == Route::Gpu {
        let look = ((1, 1), Color::BT709);
        let cfg = gpu_config(video.codec, size, fps, format, (crf, gop), look);
        return Ok(VEnc::Gpu(Box::new(HwEncoder::new(video.codec, &cfg)?)));
    }
    if video.codec == Codec::Vp9 {
        let enc = vp9_encoder((size, fps), crf, gop, threads)?;
        let vpcc = vp9_config(enc.color(), size, fps).to_bytes();
        return Ok(VEnc::Vp9(Box::new(enc), vpcc));
    }
    Ok(VEnc::Cpu(Box::new(Encoder::new(EncoderConfig {
        width: size.0,
        height: size.1,
        fps,
        profile: Profile::High,
        preset: if edge {
            Preset::Speed
        } else {
            Preset::Balanced
        },
        rate: Rate::Crf(crf),
        gop,
        b_frames: if edge { 0 } else { 2 },
        threads,
        bitstream: Bitstream::LengthPrefixed,
    })?)))
}

fn entry_for(codec: Codec, extradata: &[u8], w: u32, h: u32) -> Result<SampleEntry> {
    Ok(match codec {
        Codec::Hevc => SampleEntry::hevc(HevcConfig::parse(extradata)?, w as u16, h as u16),
        Codec::Vp9 => SampleEntry::vp9(VpcConfig::parse(extradata)?, w as u16, h as u16),
        _ => SampleEntry::h264(AvcConfig::parse(extradata)?, w as u16, h as u16),
    })
}

struct Edge {
    packets: Vec<Packet>,
    avcc: Vec<u8>,
    entry: SampleEntry,
    reasons: Vec<String>,
}

#[derive(Default)]
struct Worker {
    enc: Option<VEnc>,
    resident: Option<Resident>,
}

#[allow(clippy::too_many_arguments)]
fn encode_edge(
    job: &Job,
    video: &Picked,
    (si, from, frames): (usize, i64, i64),
    crf: u8,
    threads: usize,
    budget: &Budget,
    worker: &mut Worker,
    longest: i64,
) -> Result<Edge> {
    if video.codec == Codec::Vp9 && video.route == Route::Cpu {
        return vp9_edge(job, (si, from, frames), crf, threads, budget);
    }
    let gpu = video.route == Route::Gpu;
    let span = if gpu { longest.max(frames) } else { frames };
    let mut enc = match worker.enc.take() {
        Some(mut e) if gpu => {
            let config = job_config(job, video.codec, crf, true, span);
            match e.restart(&config) {
                Ok(()) => e,
                Err(_) => encoder(job, video, crf, true, span, threads)?,
            }
        }
        _ => encoder(job, video, crf, true, span, threads)?,
    };
    let mut packets = Vec::new();
    let mut quiet = job.quiet();
    let resident = worker.resident.get_or_insert_with(|| Resident::new(&enc));
    resident.renew(&enc);
    let on_gpu = resident.on
        && job.sources[si].route == Route::Gpu
        && resident.piece(
            &mut quiet,
            si,
            (from, frames),
            0,
            &mut enc,
            &mut |_, p| {
                packets.extend(p);
                Ok(())
            },
            &mut None,
            &mut |_| true,
        )?;
    if !on_gpu {
        reencode(
            &mut quiet,
            si,
            (from, frames),
            0,
            &mut |f: &Frame| {
                packets.extend(enc.encode(f)?);
                Ok(())
            },
            &mut None,
            &mut |_| true,
        )?;
    }
    packets.extend(enc.flush()?);
    let avcc = enc
        .extradata()
        .ok_or_else(|| Error::Backend("the encoder gave no parameter sets".to_string()))?;
    let entry = entry_for(video.codec, &avcc, job.size.0, job.size.1)?;
    let reasons = std::mem::take(&mut resident.reasons);
    if enc.on_gpu() {
        worker.enc = Some(enc);
    }
    Ok(Edge {
        packets,
        avcc,
        entry,
        reasons,
    })
}

const CHUNK: i64 = 20;
const BUFFERED: usize = 128 << 20;

struct Budget {
    left: Mutex<(usize, bool)>,
    moved: Condvar,
}

impl Budget {
    fn new(job: &Job) -> Budget {
        let picture = job.size.0 as usize * job.size.1 as usize * 3 / 2;
        Budget {
            left: Mutex::new(((BUFFERED / picture.max(1)).max(2), false)),
            moved: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (usize, bool)> {
        self.left.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn take(&self) -> Result<()> {
        let mut g = self.lock();
        while g.0 == 0 && !g.1 {
            g = self.moved.wait(g).unwrap_or_else(PoisonError::into_inner);
        }
        if g.1 {
            return Err(Error::Backend("an edge encoder stopped".to_string()));
        }
        g.0 -= 1;
        Ok(())
    }

    fn give(&self) {
        self.lock().0 += 1;
        self.moved.notify_one();
    }

    fn fail(&self) {
        self.lock().1 = true;
        self.moved.notify_all();
    }
}

fn vp9_edge(
    job: &Job,
    (si, from, frames): (usize, i64, i64),
    crf: u8,
    threads: usize,
    budget: &Budget,
) -> Result<Edge> {
    let key = vp9_keyframe(job.sources[si].demux()?)?;
    let color = (key.color.color_space, key.color.full_range);
    let frames = frames.max(1);
    let chunks = ((frames + CHUNK - 1) / CHUNK).clamp(1, threads.max(1) as i64);
    let len = (frames + chunks - 1) / chunks;
    let each = (threads / chunks as usize).clamp(1, 2);
    let crf = crf.saturating_sub(2);
    let packets = std::thread::scope(|s| -> Result<Vec<Packet>> {
        let mut senders = Vec::new();
        let mut workers = Vec::new();
        for c in 0..chunks {
            let n = len.min(frames - c * len);
            let (tx, rx) = mpsc::channel::<Frame>();
            senders.push(Some(tx));
            workers.push(s.spawn(move || {
                let run = || -> Result<Vec<Packet>> {
                    let Ok(first) = rx.recv() else {
                        return Ok(Vec::new());
                    };
                    let mut enc = vp9_encoder((job.size, job.fps), crf, n.max(1) as u32 + 1, each)?;
                    enc.set_color(color.0, color.1)?;
                    let mut out = Vec::new();
                    for f in std::iter::once(first).chain(rx) {
                        budget.give();
                        out.extend(enc.encode(&f)?);
                    }
                    out.extend(enc.flush()?);
                    Ok(out)
                };
                let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                    .unwrap_or_else(|_| Err(Error::Backend("an edge encoder failed".to_string())));
                if got.is_err() {
                    budget.fail();
                }
                got
            }));
        }
        let mut quiet = job.quiet();
        let mut n = 0i64;
        let fed = reencode(
            &mut quiet,
            si,
            (from, frames),
            0,
            &mut |f: &Frame| {
                let c = usize::try_from(n / len).unwrap_or(usize::MAX);
                n += 1;
                if let Some(done) = c.checked_sub(1).and_then(|p| senders.get_mut(p)) {
                    done.take();
                }
                budget.take()?;
                senders
                    .get(c)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| Error::Backend("an edge chunk is missing".to_string()))?
                    .send(f.clone())
                    .map_err(|_| Error::Backend("an edge encoder stopped".to_string()))
            },
            &mut None,
            &mut |_| true,
        );
        drop(senders);
        if fed.is_err() {
            budget.fail();
        }
        let mut packets = Vec::new();
        let mut failed = None;
        for w in workers {
            match w.join() {
                Ok(Ok(p)) => packets.extend(p),
                Ok(Err(e)) => {
                    failed.get_or_insert(e);
                }
                Err(_) => {
                    failed.get_or_insert(Error::Backend("an edge encoder failed".to_string()));
                }
            }
        }
        fed?;
        match failed {
            Some(e) => Err(e),
            None => Ok(packets),
        }
    })?;
    let config = vp9_config(color, job.size, job.fps);
    Ok(Edge {
        packets,
        avcc: config.to_bytes(),
        entry: SampleEntry::vp9(config, job.size.0 as u16, job.size.1 as u16),
        reasons: Vec::new(),
    })
}

enum Lane {
    Stream(super::stream::AudioStream, Vec<f32>),
    Whole(Vec<f32>, usize),
}

impl Lane {
    fn add(&mut self, out: &mut [f32], gain: f32) -> Result<()> {
        let want = out.len();
        let got: &[f32] = match self {
            Lane::Stream(s, pending) => {
                while pending.len() < want {
                    match s.next() {
                        Some(b) => pending.extend_from_slice(&b?.samples),
                        None => break,
                    }
                }
                pending.get(..want.min(pending.len())).unwrap_or_default()
            }
            Lane::Whole(all, at) => all
                .get(*at..(*at + want).min(all.len()))
                .unwrap_or_default(),
        };
        for (o, s) in out.iter_mut().zip(got) {
            *o += s * gain;
        }
        match self {
            Lane::Stream(_, pending) => {
                pending.drain(..want.min(pending.len()));
            }
            Lane::Whole(_, at) => *at += want,
        }
        Ok(())
    }
}

fn mix(
    job: &Job,
    gains: &[f32],
    ffmpeg: Option<&Ffmpeg>,
    enc: &mut crate::aac::Encoder,
    stop: &AtomicBool,
) -> Result<(Spill, u64)> {
    let mut packets = Spill::new(job, "mixed.aac")?;
    let mut at = 0i64;
    let mut total = 0u64;
    let mut out = AudioBuffer::new(RATE, 2);
    for (i, piece) in job.pieces.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let src = &job.sources[job.index[i]];
        let (a, b) = job.span(piece);
        let start = job.seconds(a);
        let dur = job.seconds(b - a);
        let first = (job.seconds(at) * RATE as f64).round() as usize;
        at += b - a;
        let n = ((job.seconds(at) * RATE as f64).round() as usize).saturating_sub(first);
        total = total.saturating_add(n as u64);
        let mut lanes: Vec<(Lane, f32)> = Vec::new();
        for t in &src.probe.audio {
            let g = gains.get(t.index).copied().unwrap_or(1.0);
            if g <= 0.0 {
                continue;
            }
            let ours = match &src.demux {
                Some(_) => super::stream::native_stream(&src.path, t.index, start, RATE, 2),
                None => Err(Error::Unsupported(
                    "this file's audio needs ffmpeg picked in the settings".to_string(),
                )),
            };
            let lane = match (ours, ffmpeg) {
                (Ok(s), _) => Lane::Stream(s, Vec::new()),
                (Err(_), Some(f)) => Lane::Whole(
                    ffmpeg_pcm(f, &src.path, t.index, Some(start), Some(dur), RATE, 2)?.samples,
                    0,
                ),
                (Err(e), None) => return Err(e),
            };
            lanes.push((lane, g));
        }
        let mut left = n;
        while left > 0 {
            if stop.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let step = left.min(RATE as usize);
            out.samples.clear();
            out.samples.resize(step * 2, 0.0);
            for (lane, g) in &mut lanes {
                lane.add(&mut out.samples, *g)?;
            }
            packets.extend(enc.encode(&out)?)?;
            left -= step;
        }
    }
    packets.extend(enc.flush())?;
    Ok((packets, total))
}

pub(super) type Premix<'a> = &'a mut (dyn FnMut(usize, usize) -> Result<AudioBuffer> + Send);

pub(super) enum Sound<'a> {
    Tracks(Option<&'a Ffmpeg>),
    Premixed(Premix<'a>),
}

pub(super) fn aac_encoder(kbps: u32) -> Result<crate::aac::Encoder> {
    crate::aac::Encoder::new(crate::aac::EncoderConfig {
        rate: RATE,
        channels: 2,
        bitrate: kbps.clamp(32, 512) * 1000,
    })
}

fn check(piece: usize, frames: usize, buf: &AudioBuffer) -> Result<()> {
    check_at(&format_args!("piece {piece}"), "the piece", frames, buf)
}

pub(super) fn check_at(
    at: &dyn std::fmt::Display,
    what: &str,
    frames: usize,
    buf: &AudioBuffer,
) -> Result<()> {
    if buf.rate != RATE || buf.channels != 2 {
        return Err(Error::Invalid(format!(
            "{at}: the audio is {} Hz with {} channels, the export takes {RATE} Hz stereo",
            buf.rate, buf.channels
        )));
    }
    if frames.checked_mul(2) != Some(buf.samples.len()) {
        return Err(Error::Invalid(format!(
            "{at}: {} audio samples per channel, {what} needs {frames}",
            buf.samples.len() / 2
        )));
    }
    if let Some(k) = buf.samples.iter().position(|s| !s.is_finite()) {
        return Err(Error::Invalid(format!(
            "{at}: audio sample {} is not a finite number",
            k / 2
        )));
    }
    Ok(())
}

fn premixed(
    job: &Job,
    audio: Premix,
    enc: &mut crate::aac::Encoder,
    stop: &AtomicBool,
) -> Result<(Spill, u64)> {
    let mut packets = Spill::new(job, "premixed.aac")?;
    let (mut at, mut first) = (0i64, 0usize);
    for (i, piece) in job.pieces.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let (a, b) = job.span(piece);
        at += b - a;
        let end = (job.seconds(at) * RATE as f64).round() as usize;
        let n = end.saturating_sub(first);
        first = end;
        let buf = audio(i, n)?;
        check(i, n, &buf)?;
        packets.extend(enc.encode(&buf)?)?;
    }
    packets.extend(enc.flush())?;
    Ok((packets, first as u64))
}

pub(super) struct Spill {
    file: std::io::BufWriter<File>,
    path: std::path::PathBuf,
    meta: Vec<(i64, u32)>,
}

impl Spill {
    fn new(job: &Job, name: &str) -> Result<Spill> {
        Spill::create(&job.work, name)
    }

    pub(super) fn create(dir: &Path, name: &str) -> Result<Spill> {
        let path = dir.join(name);
        let file = File::create(&path).map_err(|e| Error::at(&path, e))?;
        Ok(Spill {
            file: std::io::BufWriter::with_capacity(1 << 20, file),
            path,
            meta: Vec::new(),
        })
    }

    pub(super) fn extend(&mut self, packets: Vec<Packet>) -> Result<()> {
        use std::io::Write as _;
        for p in packets {
            let size = u32::try_from(p.data.len())
                .map_err(|_| Error::Invalid("an audio packet over 4 GiB".into()))?;
            self.file.write_all(&p.data)?;
            self.meta.push((p.pts, size));
        }
        Ok(())
    }

    fn replay(self, mut each: impl FnMut(&Packet, bool) -> Result<()>) -> Result<()> {
        use std::io::Read as _;
        let file = self
            .file
            .into_inner()
            .map_err(|e| Error::Io(e.into_error()))?;
        drop(file);
        let file = File::open(&self.path).map_err(|e| Error::at(&self.path, e))?;
        let mut r = std::io::BufReader::with_capacity(1 << 20, file);
        let mut p = Packet {
            data: Vec::new(),
            pts: 0,
            dts: 0,
            keyframe: true,
        };
        let n = self.meta.len();
        for (k, &(pts, size)) in self.meta.iter().enumerate() {
            p.data.resize(size as usize, 0);
            r.read_exact(&mut p.data)?;
            p.pts = pts;
            p.dts = pts;
            each(&p, k + 1 == n)?;
        }
        drop(r);
        let _ = std::fs::remove_file(&self.path);
        Ok(())
    }
}

fn audio_track(
    w: &mut Writer<File>,
    enc: &crate::aac::Encoder,
    kbps: u32,
    packets: Spill,
    total: Option<u64>,
) -> Result<()> {
    let priming = enc.priming() as i64;
    let mut cfg = TrackConfig::new(
        SampleEntry::aac(enc.extradata(), 2, RATE, kbps.clamp(32, 512) * 1000),
        RATE,
    );
    cfg.media_start = Some(priming);
    let a = w.add_track(cfg)?;
    let Some(total) = total else {
        return packets.replay(|p, _| w.push(a, p));
    };
    if !packets.meta.is_empty() {
        w.set_movie_timescale(RATE);
    }
    let end = priming.saturating_add(i64::try_from(total).unwrap_or(i64::MAX));
    packets.replay(|p, last| {
        if last {
            let duration = u32::try_from(end.saturating_sub(p.pts).max(1)).unwrap_or(u32::MAX);
            w.push_timed(a, p, duration)
        } else {
            w.push(a, p)
        }
    })
}

pub(super) fn premixed_m4a(
    job: &Job,
    audio: Premix,
    kbps: u32,
    stop: &AtomicBool,
    path: &Path,
) -> Result<()> {
    let mut enc = aac_encoder(kbps)?;
    let (packets, total) = premixed(job, audio, &mut enc, stop)?;
    let mut w = Writer::create(path)?;
    audio_track(&mut w, &enc, kbps, packets, Some(total))?;
    w.finish().map(|_| ())
}

pub(super) fn run(
    job: &mut Job,
    video: &Picked,
    opts: &mut ExportOptions,
    smart: bool,
    out: &Path,
    progress: Tell,
    sound: Sound,
) -> Result<(u64, u64, Vec<String>)> {
    let mut aac = aac_encoder(opts.audio_kbps)?;
    let fd = job.fps.1 as i64 * 64;
    let timescale = job.fps.0.saturating_mul(64);
    let part = job.work.join("joined.mp4");
    let stop = AtomicBool::new(false);
    let gains = opts.audio_gains.clone();
    let audio_job = job.quiet();
    let (copied, encoded, mux, reasons, audio) = std::thread::scope(|s| -> Result<_> {
        let (aj, gains, stop_ref, aac) = (&audio_job, &gains, &stop, &mut aac);
        let audio = s.spawn(move || match sound {
            Sound::Tracks(ffmpeg) => {
                mix(aj, gains, ffmpeg, aac, stop_ref).map(|(p, n)| (p, Some(n)))
            }
            Sound::Premixed(p) => premixed(aj, p, aac, stop_ref).map(|(p, n)| (p, Some(n))),
        });
        let video = if smart {
            smart_video(
                job,
                video,
                opts.crf,
                (timescale, fd),
                &part,
                progress,
                &stop,
            )
        } else {
            whole_video(job, video, opts, timescale, fd, &part, progress)
        };
        if video.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        let audio = audio
            .join()
            .unwrap_or_else(|_| Err(Error::Backend("the audio mix failed".to_string())));
        let (c, e, m, r) = video?;
        Ok((c, e, m, r, audio?))
    })?;
    job.tell(progress, Stage::Joining)?;
    let route = job.route;
    seal(mux, &aac, opts.audio_kbps, audio, (&part, out), &mut |f| {
        progress(&joining(f, route))
    })?;
    job.finish(progress)?;
    Ok((copied, encoded, reasons))
}

pub(super) fn joining(moved: f32, encoder: Route) -> Progress {
    Progress {
        stage: Stage::Joining,
        done: WRITTEN + (1.0 - WRITTEN) * moved.clamp(0.0, 1.0),
        encoder,
    }
}

pub(super) fn seal(
    mut mux: Mux,
    aac: &crate::aac::Encoder,
    kbps: u32,
    (packets, total): (Spill, Option<u64>),
    (part, out): (&Path, &Path),
    moved: &mut dyn FnMut(f32) -> bool,
) -> Result<()> {
    mux.close()?;
    audio_track(&mut mux.w, aac, kbps, packets, total)?;
    mux.w
        .finish_faststart_with(|n, of| moved(n as f32 / of.max(1) as f32))?;
    std::fs::rename(part, out).map_err(|e| Error::at(out, e))
}

pub(super) struct Sink<'a> {
    mux: Option<Mux>,
    waiting: Vec<Packet>,
    codec: Codec,
    size: (u32, u32),
    rotation: u16,
    timescale: u32,
    fd: i64,
    part: &'a Path,
}

impl<'a> Sink<'a> {
    pub(super) fn new(
        codec: Codec,
        size: (u32, u32),
        rotation: u16,
        (timescale, fd): (u32, i64),
        part: &'a Path,
    ) -> Self {
        Sink {
            mux: None,
            waiting: Vec::new(),
            codec,
            size,
            rotation,
            timescale,
            fd,
            part,
        }
    }

    pub(super) fn feed(&mut self, enc: &VEnc, packets: Vec<Packet>) -> Result<()> {
        self.waiting.extend(packets);
        if self.mux.is_none() {
            let Some(x) = enc.extradata() else {
                return Ok(());
            };
            let mut entries = Entries::default();
            entries.id(
                x.clone(),
                entry_for(self.codec, &x, self.size.0, self.size.1)?,
            );
            self.mux = Some(open_mux(
                self.rotation,
                entries,
                self.timescale,
                self.fd,
                self.part,
            )?);
        }
        if let Some(m) = self.mux.as_mut() {
            let fd = self.fd;
            for p in self.waiting.drain(..) {
                m.push(&p.data, p.pts * fd, p.dts * fd, p.keyframe, 0)?;
            }
        }
        Ok(())
    }

    pub(super) fn done(self) -> Result<Mux> {
        self.mux
            .ok_or_else(|| Error::Backend("the encoder gave no parameter sets".to_string()))
    }
}

fn open_mux(rotation: u16, entries: Entries, timescale: u32, fd: i64, part: &Path) -> Result<Mux> {
    let first = entries
        .first
        .ok_or_else(|| Error::Invalid("no video to write".to_string()))?;
    let mut w = Writer::create(part)?;
    let mut cfg = TrackConfig::new(first, timescale);
    cfg.matrix = rotation_matrix(rotation);
    let track = w.add_track(cfg)?;
    for e in entries.later {
        w.add_entry(track, e)?;
    }
    Ok(Mux {
        w,
        track,
        entry: 0,
        held: None,
        spare: Vec::new(),
        last: None,
        step: fd,
    })
}

fn whole_video(
    job: &mut Job,
    video: &Picked,
    opts: &mut ExportOptions,
    timescale: u32,
    fd: i64,
    part: &Path,
    progress: Tell,
) -> Result<(u64, u64, Mux, Vec<String>)> {
    job.tell(progress, Stage::Encoding)?;
    let mut enc = encoder(job, video, opts.crf, false, 0, 0)?;
    let rotation = job.sources[job.index[0]].probe.rotation;
    let mut out = Sink::new(video.codec, job.size, rotation, (timescale, fd), part);
    let mut resident = Resident::new(&enc);
    let mut at = 0i64;
    for n in 0..job.pieces.len() {
        let (a, b) = job.span(&job.pieces[n]);
        let si = job.index[n];
        let on_gpu = resident.on
            && job.sources[si].route == Route::Gpu
            && resident.piece(
                job,
                si,
                (a, b - a),
                at,
                &mut enc,
                &mut |e, p| out.feed(e, p),
                &mut opts.overlay,
                progress,
            )?;
        if !on_gpu {
            let mut sink = |f: &Frame| -> Result<()> {
                let packets = enc.encode(f)?;
                out.feed(&enc, packets)
            };
            reencode(
                job,
                si,
                (a, b - a),
                at,
                &mut sink,
                &mut opts.overlay,
                progress,
            )?;
        }
        at += b - a;
    }
    let packets = enc.flush()?;
    out.feed(&enc, packets)?;
    Ok((0, at as u64, out.done()?, resident.reasons))
}

struct Gop<'a> {
    demux: &'a Demux,
    track: usize,
    start: usize,
    count: usize,
    base: i64,
    fps: (u32, u32),
}

impl<'a> Gop<'a> {
    fn at(job: &'a Job<'_>, si: usize, from: i64) -> Result<Self> {
        let demux = job.sources[si].demux()?;
        let track = demux.video()?;
        let (start, _) = demux.seek(track, job.seconds(from) + job.seconds(1) / 2.0)?;
        Ok(Gop {
            demux,
            track,
            start,
            count: demux.samples(track),
            base: demux.sample(track, start).map_or(0, |s| s.0),
            fps: job.fps,
        })
    }

    fn frame(&self, ticks: i64) -> i64 {
        frame_at(
            self.demux
                .seconds(self.track, ticks.saturating_sub(self.base)),
            self.fps,
        )
    }

    fn first(&self) -> i64 {
        frame_at(self.demux.seconds(self.track, self.base), self.fps)
    }

    fn sample(&self, k: usize) -> Result<(i64, i64, bool)> {
        self.demux
            .sample(self.track, k)
            .ok_or_else(|| Error::Invalid(format!("no video sample {k}")))
    }
}

fn random_access(codec: Codec, length: usize, data: &[u8]) -> bool {
    if codec == Codec::Vp9 {
        return crate::vp9::keyframe_info(data).is_some_and(|k| k.profile == 0);
    }
    let mut pos = 0usize;
    while length > 0 && pos.saturating_add(length) <= data.len() {
        let n = data[pos..pos + length]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length;
        let Some(&head) = data.get(pos) else {
            return false;
        };
        match codec {
            Codec::H264 => match head & 0x1f {
                5 => return true,
                1..=4 => return false,
                _ => {}
            },
            Codec::Hevc => match (head >> 1) & 0x3f {
                16..=21 => return true,
                0..=15 | 22..=31 => return false,
                _ => {}
            },
            _ => return true,
        }
        pos = pos.saturating_add(n);
    }
    false
}

const NOT_IDR: &str = "some keyframes are not IDR pictures, so the spans that start on them are re-encoded instead of copied";
const OPEN_GOP: &str =
    "some GOPs are open or off the frame grid, so those spans are re-encoded instead of copied";

#[derive(Default)]
pub(super) struct Planner {
    lengths: HashMap<usize, Option<(Codec, usize)>>,
    buf: Vec<u8>,
    seen: Vec<bool>,
    pub(super) reasons: Vec<String>,
}

fn nal_length(g: &Gop) -> Option<(Codec, usize)> {
    let st = g.demux.stream(g.track).ok()?;
    let codec = st.codec?;
    let length = match codec {
        Codec::H264 => st.avc.as_ref()?.length_size,
        Codec::Hevc => HevcConfig::parse(&st.extradata).ok()?.length_size,
        Codec::Vp9 => return Some((codec, 0)),
        _ => return None,
    };
    Some((codec, length.clamp(1, 4) as usize))
}

impl Planner {
    fn note(&mut self, why: &str) {
        if !self.reasons.iter().any(|r| r == why) {
            self.reasons.push(why.to_string());
        }
    }

    fn opens_cleanly(&mut self, si: usize, g: &Gop, from: i64) -> Result<bool> {
        if g.first() != from {
            self.note(OPEN_GOP);
            return Ok(false);
        }
        let Some((codec, length)) = *self.lengths.entry(si).or_insert_with(|| nal_length(g)) else {
            self.note(NOT_IDR);
            return Ok(false);
        };
        g.demux.read_into(g.track, g.start, &mut self.buf)?;
        if !random_access(codec, length, &self.buf) {
            self.note(NOT_IDR);
            return Ok(false);
        }
        Ok(true)
    }

    fn copyable(&mut self, job: &Job, si: usize, from: i64, frames: i64) -> Result<bool> {
        if job.sources[si].demux.is_none() {
            return Ok(true);
        }
        let g = Gop::at(job, si, from)?;
        if !self.opens_cleanly(si, &g, from)? {
            return Ok(false);
        }
        let n = usize::try_from(frames).unwrap_or(0);
        if n == 0 || g.start.saturating_add(n) > g.count {
            self.note(OPEN_GOP);
            return Ok(false);
        }
        self.seen.clear();
        self.seen.resize(n, false);
        for k in g.start..g.start + n {
            let rel = g.frame(g.sample(k)?.0);
            match usize::try_from(rel).ok().and_then(|r| self.seen.get_mut(r)) {
                Some(slot) if !*slot => *slot = true,
                _ => {
                    self.note(OPEN_GOP);
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn closed_prefix(&mut self, job: &Job, si: usize, from: i64, frames: i64) -> Result<i64> {
        let g = Gop::at(job, si, from)?;
        if !self.opens_cleanly(si, &g, from)? {
            return Ok(0);
        }
        let (mut best, mut most) = (0i64, -1i64);
        for (n, k) in (g.start..g.count).enumerate() {
            let (pts, _, key) = g.sample(k)?;
            if n > 0 && key {
                break;
            }
            let rel = g.frame(pts);
            if rel >= frames || rel < 0 {
                break;
            }
            most = most.max(rel);
            if most == n as i64 {
                best = n as i64 + 1;
            }
        }
        Ok(best)
    }

    pub(super) fn split(
        &mut self,
        job: &Job,
        (si, part): (usize, Part),
        keys: &[i64],
        prefix: bool,
    ) -> Result<Vec<Part>> {
        Ok(match part {
            Part::Copy { from, frames } => {
                if self.copyable(job, si, from, frames)? {
                    vec![part]
                } else {
                    vec![Part::Encode { from, frames }]
                }
            }
            Part::Encode { from, frames } if prefix && keys.contains(&from) => {
                let j = if job.sources[si].demux.is_some() {
                    self.closed_prefix(job, si, from, frames)?
                } else {
                    0
                };
                let mut split = Vec::new();
                if j > 0 {
                    split.push(Part::Copy { from, frames: j });
                }
                if frames > j {
                    split.push(Part::Encode {
                        from: from + j,
                        frames: frames - j,
                    });
                }
                split
            }
            p => vec![p],
        })
    }
}

const AHEAD: usize = 4;

struct Writing {
    mux: Option<Mux>,
    ids: HashMap<Vec<u8>, u32>,
    sources: HashMap<usize, u32>,
    rotation: u16,
    timescale: u32,
    fd: i64,
}

impl Writing {
    fn entry(&mut self, key: Vec<u8>, entry: SampleEntry, part: &Path) -> Result<u32> {
        if let Some(&id) = self.ids.get(&key) {
            return Ok(id);
        }
        let id = match self.mux.as_mut() {
            Some(m) => m.w.add_entry(m.track, entry)?,
            None => {
                let mut entries = Entries::default();
                entries.id(key.clone(), entry);
                self.mux = Some(open_mux(
                    self.rotation,
                    entries,
                    self.timescale,
                    self.fd,
                    part,
                )?);
                0
            }
        };
        self.ids.insert(key, id);
        Ok(id)
    }

    fn source(&mut self, job: &Job, si: usize, part: &Path) -> Result<u32> {
        if let Some(&id) = self.sources.get(&si) {
            return Ok(id);
        }
        let (key, entry) = copy_entry(job, si)?;
        let id = self.entry(key, entry, part)?;
        self.sources.insert(si, id);
        Ok(id)
    }

    fn mux(&mut self) -> Result<&mut Mux> {
        self.mux
            .as_mut()
            .ok_or_else(|| Error::Invalid("no video to write".to_string()))
    }
}

fn smart_video(
    job: &Job,
    video: &Picked,
    crf: u8,
    (timescale, fd): (u32, i64),
    part: &Path,
    progress: Tell,
    stop: &AtomicBool,
) -> Result<(u64, u64, Mux, Vec<String>)> {
    let mut planner = Planner::default();
    let (plan, order) = job.plan(&Prefs::default(), |si, part, keys| {
        planner.split(job, (si, part), keys, true)
    })?;
    let mut edge_reasons: Vec<String> = Vec::new();
    let edges: Vec<usize> = (0..plan.len())
        .filter(|&i| matches!(plan[i].1, Part::Encode { .. }))
        .collect();
    let rank: HashMap<usize, usize> = edges.iter().enumerate().map(|(r, &i)| (i, r)).collect();
    let mut last_use: HashMap<usize, usize> = HashMap::new();
    for (n, &i) in order.iter().enumerate() {
        last_use.insert(i, n);
    }
    let longest = edges
        .iter()
        .filter_map(|&i| match plan[i].1 {
            Part::Encode { frames, .. } => Some(frames),
            Part::Copy { .. } => None,
        })
        .max()
        .unwrap_or(1);
    let workers = edges.len().clamp(1, 4);
    let per = (threads() * 2 / workers).max(2);
    let next = AtomicUsize::new(0);
    let wanted = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Result<Edge>)>();
    let budget = Budget::new(job);
    let total: i64 = order
        .iter()
        .filter_map(|&i| plan.get(i))
        .map(|(_, p)| p.frames())
        .sum::<i64>()
        .max(1);
    let mut out = Writing {
        mux: None,
        ids: HashMap::new(),
        sources: HashMap::new(),
        rotation: job.sources[job.index[0]].probe.rotation,
        timescale,
        fd,
    };
    let written = std::thread::scope(|s| -> Result<(u64, u64)> {
        for _ in 0..workers {
            let tx = tx.clone();
            let (plan, next, edges, wanted, budget) = (&plan, &next, &edges, &wanted, &budget);
            s.spawn(move || {
                let mut worker = Worker::default();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = edges.get(k) else { break };
                    while k >= wanted.load(Ordering::Relaxed) + AHEAD + workers {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    let (si, p) = plan[i];
                    let Part::Encode { from, frames } = p else {
                        continue;
                    };
                    if tx
                        .send((
                            i,
                            encode_edge(
                                job,
                                video,
                                (si, from, frames),
                                crf,
                                per,
                                budget,
                                &mut worker,
                                longest,
                            ),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut ready: HashMap<usize, Edge> = HashMap::new();
        let mut received = 0usize;
        let mut ahead = vec![false; plan.len()];
        let (mut written, mut early) = (0i64, 0i64);
        let pieces = edges.len();
        let mut tell = |received: usize, done: i64| {
            let keep = progress(&Progress {
                stage: Stage::Cutting {
                    piece: received,
                    pieces,
                },
                done: done as f32 / total as f32 * WRITTEN,
                encoder: job.route,
            });
            if keep { Ok(()) } else { Err(Error::Cancelled) }
        };
        let got = (|| -> Result<(u64, u64)> {
            let (mut copied, mut encoded) = (0u64, 0u64);
            let mut at = 0i64;
            let mut buf = Vec::new();
            let mut times: Vec<(i64, i64, bool)> = Vec::new();
            let mut order_pts: Vec<i64> = Vec::new();
            for (n, &i) in order.iter().enumerate() {
                let (si, p) = plan[i];
                match p {
                    Part::Copy { from, frames } => {
                        let entry = out.source(job, si, part)?;
                        let mux = out.mux()?;
                        let g = Gop::at(job, si, from)?;
                        copy_times(&g, frames, &mut times, &mut order_pts)?;
                        for (k, &(pts, dts, key)) in (g.start..).zip(&times) {
                            g.demux.read_into(g.track, k, &mut buf)?;
                            mux.push(
                                &buf,
                                at.saturating_add(pts).saturating_mul(fd),
                                at.saturating_add(dts).saturating_mul(fd),
                                key,
                                entry,
                            )?;
                            written += 1;
                            if written % 64 == 0 {
                                tell(received, written + early)?;
                            }
                        }
                        copied += frames as u64;
                        tell(received, written + early)?;
                    }
                    Part::Encode { frames, .. } => {
                        if let Some(&r) = rank.get(&i) {
                            wanted.fetch_max(r, Ordering::Relaxed);
                        }
                        while !ready.contains_key(&i) {
                            let Ok((j, r)) = rx.recv() else {
                                return Err(Error::Backend("an edge was not encoded".into()));
                            };
                            let e = r?;
                            received += 1;
                            if let (Some(slot), Some((_, Part::Encode { frames, .. }))) =
                                (ahead.get_mut(j), plan.get(j))
                            {
                                *slot = true;
                                early += frames;
                            }
                            ready.insert(j, e);
                            tell(received, written + early)?;
                        }
                        let e = ready
                            .remove(&i)
                            .ok_or_else(|| Error::Backend("an edge was not encoded".into()))?;
                        for r in &e.reasons {
                            if !edge_reasons.contains(r) {
                                edge_reasons.push(r.clone());
                            }
                        }
                        let entry = if video.codec == Codec::Vp9 {
                            out.source(job, si, part)?
                        } else {
                            out.entry(e.avcc.clone(), e.entry.clone(), part)?
                        };
                        let mux = out.mux()?;
                        for p in &e.packets {
                            mux.push(
                                &p.data,
                                (at + p.pts) * fd,
                                (at + p.dts) * fd,
                                p.keyframe,
                                entry,
                            )?;
                        }
                        if last_use.get(&i).is_some_and(|&l| l > n) {
                            ready.insert(i, e);
                        }
                        if let Some(slot) = ahead.get_mut(i).filter(|s| **s) {
                            *slot = false;
                            early -= frames;
                        }
                        written += frames;
                        tell(received, written + early)?;
                        encoded += frames as u64;
                    }
                }
                at += p.frames();
            }
            Ok((copied, encoded))
        })();
        if got.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        drop(rx);
        got
    })?;
    let mux = out
        .mux
        .ok_or_else(|| Error::Invalid("no video to write".to_string()))?;
    let mut reasons = planner.reasons;
    reasons.extend(edge_reasons);
    Ok((written.0, written.1, mux, reasons))
}

fn copy_entry(job: &Job, si: usize) -> Result<(Vec<u8>, SampleEntry)> {
    let src = &job.sources[si];
    if let Some(r) = src.mp4() {
        let (_, t) = video_track(r)?;
        let entry = t
            .entry()
            .cloned()
            .ok_or_else(|| Error::Invalid("video track without an entry".into()))?;
        return Ok((t.extradata().unwrap_or_default(), entry));
    }
    let demux = src.demux()?;
    let st = demux.stream(demux.video()?)?;
    let codec = st
        .codec
        .ok_or_else(|| Error::Unsupported(format!("copying {} video", st.name)))?;
    if codec == Codec::Vp9 {
        let k = vp9_keyframe(demux)?;
        let chroma = match (k.subsampling_x, k.subsampling_y) {
            (true, false) => 2,
            (false, false) => 3,
            _ => 1,
        };
        let config = VpcConfig::new(
            (k.profile, k.bit_depth as u8, chroma),
            (k.color.color_space, k.color.full_range),
            (src.probe.width, src.probe.height),
            job.fps,
        );
        let entry = SampleEntry::vp9(config, src.probe.width as u16, src.probe.height as u16);
        return Ok((config.to_bytes(), entry));
    }
    let entry = entry_for(codec, &st.extradata, src.probe.width, src.probe.height)?;
    Ok((st.extradata, entry))
}

fn copy_times(
    g: &Gop,
    frames: i64,
    times: &mut Vec<(i64, i64, bool)>,
    order_pts: &mut Vec<i64>,
) -> Result<()> {
    times.clear();
    let end = g
        .start
        .saturating_add(usize::try_from(frames).unwrap_or(0))
        .min(g.count);
    for k in g.start..end {
        let (pts, dts, key) = g.sample(k)?;
        times.push((g.frame(pts), g.frame(dts), key));
    }
    if g.demux.mp4().is_some() {
        return Ok(());
    }
    order_pts.clear();
    order_pts.extend(times.iter().map(|t| t.0));
    order_pts.sort_unstable();
    let shift = order_pts
        .iter()
        .zip(times.iter())
        .map(|(o, t)| o - t.0)
        .max()
        .unwrap_or(0)
        .max(0);
    for (t, o) in times.iter_mut().zip(order_pts.iter()) {
        t.1 = o - shift;
    }
    Ok(())
}

pub(super) fn gop_mp4(job: &Job, si: usize, from: i64, frames: i64, path: &Path) -> Result<()> {
    let fd = job.fps.1 as i64 * 64;
    let mut entries = Entries::default();
    let (key, entry) = copy_entry(job, si)?;
    entries.id(key, entry);
    let mut mux = open_mux(0, entries, job.fps.0.saturating_mul(64), fd, path)?;
    let g = Gop::at(job, si, from)?;
    let (mut times, mut order_pts) = (Vec::new(), Vec::new());
    copy_times(&g, frames, &mut times, &mut order_pts)?;
    let mut buf = Vec::new();
    for (k, &(pts, dts, key)) in (g.start..).zip(&times) {
        g.demux.read_into(g.track, k, &mut buf)?;
        mux.push(&buf, pts.saturating_mul(fd), dts.saturating_mul(fd), key, 0)?;
    }
    mux.close()?;
    mux.w.finish().map(|_| ())
}
