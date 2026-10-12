use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;

use crate::gpu::{
    Bitstream as GpuBitstream, EncoderConfig as GpuConfig, HwEncoder, Profile as GpuProfile,
    Rate as GpuRate,
};
use crate::h264::{Bitstream, Encoder, EncoderConfig, Preset, Profile, Rate};
use crate::mp4::{AvcConfig, HevcConfig, SampleEntry, TrackConfig, Writer, rotation_matrix};

use super::audio::ffmpeg_pcm;
use super::demux::Demux;
use super::export::{ExportOptions, Job, Part, Progress, Stage, Tell, reencode};
use super::ffmpeg::Ffmpeg;
use super::native::{at_rate, audio_range, decode_audio, shape, threads, video_track};
use super::route::{Picked, Prefs};
use super::video::frame_at;
use crate::media::{AudioBuffer, Codec, Error, Frame, Packet, PixelFormat, Result, Route};

const RATE: u32 = 48_000;

struct Held {
    data: Vec<u8>,
    pts: i64,
    dts: i64,
    key: bool,
    entry: u32,
}

struct Mux {
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

    fn close(&mut self) -> Result<()> {
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

enum VEnc {
    Cpu(Box<Encoder>),
    Gpu(Box<HwEncoder>),
}

impl VEnc {
    fn encode(&mut self, f: &Frame) -> Result<Vec<Packet>> {
        match self {
            VEnc::Cpu(e) => e.encode(f),
            VEnc::Gpu(e) => e.encode(f),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        match self {
            VEnc::Cpu(e) => e.flush(),
            VEnc::Gpu(e) => e.flush(),
        }
    }

    fn extradata(&self) -> Option<Vec<u8>> {
        match self {
            VEnc::Cpu(e) => Some(e.avcc()),
            VEnc::Gpu(e) => e.extradata().map(<[u8]>::to_vec),
        }
    }
}

fn gpu_config(
    codec: Codec,
    size: (u32, u32),
    fps: (u32, u32),
    format: PixelFormat,
    crf: u8,
    gop: u32,
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
        rate: GpuRate::Crf(crf),
        gop,
        b_frames: 0,
        bitstream: GpuBitstream::LengthPrefixed,
    }
}

pub(super) fn gpu_check(
    codec: Codec,
    size: (u32, u32),
    fps: (u32, u32),
    format: PixelFormat,
) -> Result<()> {
    HwEncoder::new(codec, &gpu_config(codec, size, fps, format, 20, 60)).map(|_| ())
}

fn encoder(
    job: &Job,
    video: &Picked,
    crf: u8,
    edge: bool,
    frames: i64,
    threads: usize,
) -> Result<VEnc> {
    let gop = if edge {
        (frames.max(1) as u32).saturating_add(1)
    } else {
        (job.fps.0 / job.fps.1.max(1)).clamp(1, 120) * 2
    };
    let crf = if edge { crf.saturating_sub(2) } else { crf };
    if video.route == Route::Gpu {
        let cfg = gpu_config(video.codec, job.size, job.fps, job.format, crf, gop);
        return Ok(VEnc::Gpu(Box::new(HwEncoder::new(video.codec, &cfg)?)));
    }
    Ok(VEnc::Cpu(Box::new(Encoder::new(EncoderConfig {
        width: job.size.0,
        height: job.size.1,
        fps: job.fps,
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
        _ => SampleEntry::h264(AvcConfig::parse(extradata)?, w as u16, h as u16),
    })
}

struct Edge {
    packets: Vec<Packet>,
    avcc: Vec<u8>,
    entry: SampleEntry,
}

fn encode_edge(
    job: &Job,
    video: &Picked,
    (si, from, frames): (usize, i64, i64),
    crf: u8,
    threads: usize,
) -> Result<Edge> {
    let mut enc = encoder(job, video, crf, true, frames, threads)?;
    let mut packets = Vec::new();
    let mut quiet = job.quiet();
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
    packets.extend(enc.flush()?);
    let avcc = enc
        .extradata()
        .ok_or_else(|| Error::Backend("the encoder gave no parameter sets".to_string()))?;
    let entry = entry_for(video.codec, &avcc, job.size.0, job.size.1)?;
    Ok(Edge {
        packets,
        avcc,
        entry,
    })
}

fn mix(
    job: &Job,
    gains: &[f32],
    ffmpeg: Option<&Ffmpeg>,
    enc: &mut crate::aac::Encoder,
    stop: &AtomicBool,
) -> Result<(Vec<Packet>, u64)> {
    let mut packets = Vec::new();
    let mut at = 0i64;
    let mut total = 0u64;
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
        let mut out = AudioBuffer::new(RATE, 2);
        out.samples = vec![0.0; n * 2];
        for t in &src.probe.audio {
            let g = gains.get(t.index).copied().unwrap_or(1.0);
            if g <= 0.0 {
                continue;
            }
            let buf = match (src.mp4(), ffmpeg) {
                (Some(r), _) => {
                    let ids = r.audio();
                    let id = *ids
                        .get(t.index)
                        .ok_or_else(|| Error::Invalid(format!("no audio track {}", t.index)))?;
                    at_rate(shape(decode_audio(r, id, start, start + dur)?, 2)?, RATE)
                }
                (None, f) => {
                    let ours = match &src.demux {
                        Some(_) => audio_range(&src.path, t.index, start, start + dur, RATE, 2),
                        None => Err(Error::Unsupported(
                            "this file's audio needs ffmpeg picked in the settings".to_string(),
                        )),
                    };
                    match (ours, f) {
                        (Ok(b), _) => b,
                        (Err(_), Some(f)) => {
                            ffmpeg_pcm(f, &src.path, t.index, Some(start), Some(dur), RATE, 2)?
                        }
                        (Err(e), None) => return Err(e),
                    }
                }
            };
            for (o, s) in out.samples.iter_mut().zip(&buf.samples) {
                *o += s * g;
            }
        }
        packets.extend(enc.encode(&out)?);
    }
    packets.extend(enc.flush());
    Ok((packets, total))
}

pub(super) type Premix<'a> = &'a mut (dyn FnMut(usize, usize) -> Result<AudioBuffer> + Send);

pub(super) enum Sound<'a> {
    Tracks(Option<&'a Ffmpeg>),
    Premixed(Premix<'a>),
}

fn aac_encoder(kbps: u32) -> Result<crate::aac::Encoder> {
    crate::aac::Encoder::new(crate::aac::EncoderConfig {
        rate: RATE,
        channels: 2,
        bitrate: kbps.clamp(32, 512) * 1000,
    })
}

fn check(piece: usize, frames: usize, buf: &AudioBuffer) -> Result<()> {
    if buf.rate != RATE || buf.channels != 2 {
        return Err(Error::Invalid(format!(
            "piece {piece}: the audio is {} Hz with {} channels, the export takes {RATE} Hz stereo",
            buf.rate, buf.channels
        )));
    }
    if frames.checked_mul(2) != Some(buf.samples.len()) {
        return Err(Error::Invalid(format!(
            "piece {piece}: {} audio samples per channel, the piece needs {frames}",
            buf.samples.len() / 2
        )));
    }
    if let Some(k) = buf.samples.iter().position(|s| !s.is_finite()) {
        return Err(Error::Invalid(format!(
            "piece {piece}: audio sample {} is not a finite number",
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
) -> Result<(Vec<Packet>, u64)> {
    let mut packets = Vec::new();
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
        packets.extend(enc.encode(&buf)?);
    }
    packets.extend(enc.flush());
    Ok((packets, first as u64))
}

fn audio_track(
    w: &mut Writer<File>,
    enc: &crate::aac::Encoder,
    kbps: u32,
    packets: &[Packet],
    total: Option<u64>,
) -> Result<()> {
    let priming = enc.priming() as i64;
    let mut cfg = TrackConfig::new(
        SampleEntry::aac(enc.extradata(), 2, RATE, kbps.clamp(32, 512) * 1000),
        RATE,
    );
    cfg.media_start = Some(priming);
    let a = w.add_track(cfg)?;
    let (Some(total), Some((last, rest))) = (total, packets.split_last()) else {
        for p in packets {
            w.push(a, p)?;
        }
        return Ok(());
    };
    w.set_movie_timescale(RATE);
    for p in rest {
        w.push(a, p)?;
    }
    let end = priming.saturating_add(i64::try_from(total).unwrap_or(i64::MAX));
    let duration = u32::try_from(end.saturating_sub(last.pts).max(1)).unwrap_or(u32::MAX);
    w.push_timed(a, last, duration)
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
    audio_track(&mut w, &enc, kbps, &packets, Some(total))?;
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
    let (copied, encoded, mut mux, reasons, audio) = std::thread::scope(|s| -> Result<_> {
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
                .map(|(c, e, m)| (c, e, m, Vec::new()))
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
    mux.close()?;
    let (packets, total) = audio;
    audio_track(&mut mux.w, &aac, opts.audio_kbps, &packets, total)?;
    mux.w.finish_faststart()?;
    std::fs::rename(&part, out).map_err(|e| Error::at(out, e))?;
    job.finish(progress)?;
    Ok((copied, encoded, reasons))
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
) -> Result<(u64, u64, Mux)> {
    job.tell(progress, Stage::Encoding)?;
    let mut enc = encoder(job, video, opts.crf, false, 0, 0)?;
    let mut mux: Option<Mux> = None;
    let mut waiting: Vec<Packet> = Vec::new();
    let codec = video.codec;
    let (w, h) = job.size;
    let rotation = job.sources[job.index[0]].probe.rotation;
    let mut feed = |enc: &VEnc, mux: &mut Option<Mux>, packets: Vec<Packet>| -> Result<()> {
        waiting.extend(packets);
        if mux.is_none() {
            let Some(x) = enc.extradata() else {
                return Ok(());
            };
            let mut entries = Entries::default();
            entries.id(x.clone(), entry_for(codec, &x, w, h)?);
            *mux = Some(open_mux(rotation, entries, timescale, fd, part)?);
        }
        if let Some(m) = mux.as_mut() {
            for p in waiting.drain(..) {
                m.push(&p.data, p.pts * fd, p.dts * fd, p.keyframe, 0)?;
            }
        }
        Ok(())
    };
    let mut at = 0i64;
    for n in 0..job.pieces.len() {
        let (a, b) = job.span(&job.pieces[n]);
        let si = job.index[n];
        let mut sink = |f: &Frame| -> Result<()> {
            let packets = enc.encode(f)?;
            feed(&enc, &mut mux, packets)
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
        at += b - a;
    }
    let packets = enc.flush()?;
    feed(&enc, &mut mux, packets)?;
    let mux =
        mux.ok_or_else(|| Error::Backend("the encoder gave no parameter sets".to_string()))?;
    Ok((0, at as u64, mux))
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
    let edges: Vec<usize> = (0..plan.len())
        .filter(|&i| matches!(plan[i].1, Part::Encode { .. }))
        .collect();
    let workers = edges.len().clamp(1, 4);
    let per = (threads() * 2 / workers).max(2);
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Result<Edge>)>();
    let total: i64 = plan.iter().map(|(_, p)| p.frames()).sum::<i64>().max(1);
    let mut done_frames = 0i64;
    let mut encoded_edges: HashMap<usize, Edge> = HashMap::new();
    std::thread::scope(|s| -> Result<()> {
        for _ in 0..workers {
            let tx = tx.clone();
            let (plan, next, edges) = (&plan, &next, &edges);
            s.spawn(move || {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = edges.get(k) else { break };
                    let (si, p) = plan[i];
                    let Part::Encode { from, frames } = p else {
                        continue;
                    };
                    let _ = tx.send((i, encode_edge(job, video, (si, from, frames), crf, per)));
                }
            });
        }
        drop(tx);
        for (i, r) in rx.iter() {
            match r {
                Ok(e) => {
                    if let Part::Encode { frames, .. } = plan[i].1 {
                        done_frames += frames;
                    }
                    encoded_edges.insert(i, e);
                    let keep = progress(&Progress {
                        stage: Stage::Cutting {
                            piece: encoded_edges.len(),
                            pieces: edges.len(),
                        },
                        done: done_frames as f32 / total as f32 * 0.8,
                        encoder: job.route,
                    });
                    if !keep {
                        stop.store(true, Ordering::Relaxed);
                        return Err(Error::Cancelled);
                    }
                }
                Err(e) => {
                    stop.store(true, Ordering::Relaxed);
                    return Err(e);
                }
            }
        }
        Ok(())
    })?;
    let mut entries = Entries::default();
    let mut entry_of: HashMap<usize, u32> = HashMap::new();
    let mut source_entry: HashMap<usize, u32> = HashMap::new();
    for &i in &order {
        if entry_of.contains_key(&i) {
            continue;
        }
        let (si, p) = plan[i];
        let id = match p {
            Part::Copy { .. } => match source_entry.get(&si) {
                Some(&id) => id,
                None => {
                    let (key, entry) = copy_entry(job, si)?;
                    let id = entries.id(key, entry);
                    source_entry.insert(si, id);
                    id
                }
            },
            Part::Encode { .. } => {
                let e = encoded_edges
                    .get(&i)
                    .ok_or_else(|| Error::Backend("an edge was not encoded".into()))?;
                entries.id(e.avcc.clone(), e.entry.clone())
            }
        };
        entry_of.insert(i, id);
    }
    let mut mux = open_mux(
        job.sources[job.index[0]].probe.rotation,
        entries,
        timescale,
        fd,
        part,
    )?;
    let (mut copied, mut encoded) = (0u64, 0u64);
    let mut at = 0i64;
    let mut buf = Vec::new();
    let mut times: Vec<(i64, i64, bool)> = Vec::new();
    let mut order_pts: Vec<i64> = Vec::new();
    for &i in &order {
        let (si, p) = plan[i];
        let entry = entry_of.get(&i).copied().unwrap_or(0);
        match p {
            Part::Copy { from, frames } => {
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
                }
                copied += frames as u64;
            }
            Part::Encode { frames, .. } => {
                let e = encoded_edges
                    .get(&i)
                    .ok_or_else(|| Error::Backend("an edge was not encoded".into()))?;
                for p in &e.packets {
                    mux.push(
                        &p.data,
                        (at + p.pts) * fd,
                        (at + p.dts) * fd,
                        p.keyframe,
                        entry,
                    )?;
                }
                encoded += frames as u64;
            }
        }
        at += p.frames();
    }
    Ok((copied, encoded, mux, planner.reasons))
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
