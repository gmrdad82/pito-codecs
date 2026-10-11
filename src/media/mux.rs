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

use super::export::{ExportOptions, Job, Part, Progress, Stage, Tell, parts, reencode};
use super::ffmpeg::Ffmpeg;
use super::native::{decode_audio, threads};
use super::probe::keyframes;
use super::route::Picked;
use super::types::{AudioBuffer, Codec, Error, Frame, Packet, PixelFormat, Result, Route};

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
    let mut quiet = Job {
        pieces: job.pieces,
        sources: job.sources.clone(),
        index: job.index.clone(),
        fps: job.fps,
        size: job.size,
        format: job.format,
        total: 0,
        done: 0,
        work: job.work.clone(),
        route: job.route,
    };
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

fn pcm_ffmpeg(f: &Ffmpeg, path: &Path, track: usize, start: f64, dur: f64) -> Result<AudioBuffer> {
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{start:.6}"),
        "-t".into(),
        format!("{dur:.6}"),
        "-i".into(),
        path.display().to_string(),
        "-map".into(),
        format!("0:a:{track}"),
        "-vn".into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        RATE.to_string(),
        "-f".into(),
        "f32le".into(),
        "-".into(),
    ];
    let raw = f.run_args(&args)?;
    let mut buf = AudioBuffer::new(RATE, 2);
    buf.samples = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(buf)
}

fn mix(
    job: &Job,
    gains: &[f32],
    ffmpeg: Option<&Ffmpeg>,
    enc: &mut crate::aac::Encoder,
    stop: &AtomicBool,
) -> Result<Vec<Packet>> {
    let mut packets = Vec::new();
    let mut at = 0i64;
    for (i, piece) in job.pieces.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let src = &job.sources[job.index[i]];
        let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
        let start = job.seconds(a);
        let dur = job.seconds(b - a);
        let first = (job.seconds(at) * RATE as f64).round() as usize;
        at += b - a;
        let n = ((job.seconds(at) * RATE as f64).round() as usize).saturating_sub(first);
        let mut out = AudioBuffer::new(RATE, 2);
        out.samples = vec![0.0; n * 2];
        for t in &src.probe.audio {
            let g = gains.get(t.index).copied().unwrap_or(1.0);
            if g <= 0.0 {
                continue;
            }
            let buf = match (&src.reader, ffmpeg) {
                (Some(r), _) => {
                    let ids = r.audio();
                    let id = *ids
                        .get(t.index)
                        .ok_or_else(|| Error::Invalid(format!("no audio track {}", t.index)))?;
                    let raw = decode_audio(r, id, start, start + dur)?;
                    let stereo = crate::aac::downmix_stereo(&raw);
                    if stereo.rate == RATE {
                        stereo
                    } else {
                        crate::aac::resample(&stereo, RATE)
                    }
                }
                (None, Some(f)) => pcm_ffmpeg(f, &src.path, t.index, start, dur)?,
                (None, None) => {
                    return Err(Error::Unsupported(
                        "this file's audio needs ffmpeg picked in the settings".to_string(),
                    ));
                }
            };
            for (o, s) in out.samples.iter_mut().zip(&buf.samples) {
                *o += s * g;
            }
        }
        packets.extend(enc.encode(&out)?);
    }
    packets.extend(enc.flush());
    Ok(packets)
}

pub(super) fn run(
    job: &mut Job,
    video: &Picked,
    opts: &mut ExportOptions,
    smart: bool,
    out: &Path,
    progress: Tell,
    ffmpeg: Option<&Ffmpeg>,
) -> Result<(u64, u64)> {
    let mut aac = crate::aac::Encoder::new(crate::aac::EncoderConfig {
        rate: RATE,
        channels: 2,
        bitrate: opts.audio_kbps.clamp(32, 512) * 1000,
    })?;
    let asc = aac.extradata();
    let priming = aac.priming();
    let fd = job.fps.1 as i64 * 64;
    let timescale = job.fps.0.saturating_mul(64);
    let part = job.work.join("joined.mp4");
    let stop = AtomicBool::new(false);
    let gains = opts.audio_gains.clone();
    let audio_job = Job {
        pieces: job.pieces,
        sources: job.sources.clone(),
        index: job.index.clone(),
        fps: job.fps,
        size: job.size,
        format: job.format,
        total: 0,
        done: 0,
        work: job.work.clone(),
        route: job.route,
    };
    let (copied, encoded, mut mux, audio) = std::thread::scope(|s| -> Result<_> {
        let (aj, gains, stop_ref, aac) = (&audio_job, &gains, &stop, &mut aac);
        let audio = s.spawn(move || mix(aj, gains, ffmpeg, aac, stop_ref));
        let video = if smart {
            smart_video(job, video, opts.crf, (timescale, fd), &part, progress, &stop)
        } else {
            whole_video(job, video, opts, timescale, fd, &part, progress)
        };
        if video.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
        let audio = audio
            .join()
            .unwrap_or_else(|_| Err(Error::Backend("the audio mix failed".to_string())));
        let (c, e, m) = video?;
        Ok((c, e, m, audio?))
    })?;
    job.tell(progress, Stage::Joining)?;
    mux.close()?;
    let mut cfg = TrackConfig::new(
        SampleEntry::aac(asc, 2, RATE, opts.audio_kbps.clamp(32, 512) * 1000),
        RATE,
    );
    cfg.media_start = Some(priming as i64);
    let a = mux.w.add_track(cfg)?;
    for p in &audio {
        mux.w.push(a, p)?;
    }
    mux.w.finish_faststart()?;
    std::fs::rename(&part, out)?;
    if !progress(&Progress {
        stage: Stage::Joining,
        done: 1.0,
        encoder: job.route,
    }) {
        return Err(Error::Cancelled);
    }
    Ok((copied, encoded))
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
            let mut entries = Entries {
                ids: HashMap::new(),
                first: None,
                later: Vec::new(),
            };
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
        let piece = &job.pieces[n];
        let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
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

fn closed_prefix(job: &Job, si: usize, from: i64, frames: i64) -> Result<i64> {
    let r = job.sources[si].reader()?;
    let v = r.video().ok_or_else(|| Error::Invalid("no video".into()))?;
    let t = &r.tracks[v];
    let ka = t.keyframe_before(t.sample_at(t.ticks(job.seconds(from))).unwrap_or(0));
    let base = t.presentation(ka).unwrap_or(0);
    let ft = t.timescale as f64 * job.fps.1 as f64 / job.fps.0 as f64;
    let (mut best, mut most) = (0i64, -1i64);
    for (n, k) in (ka..t.samples.len()).enumerate() {
        if n > 0 && t.samples[k].keyframe {
            break;
        }
        let rel = (t.presentation(k).unwrap_or(0).saturating_sub(base) as f64 / ft).round() as i64;
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

fn smart_video(
    job: &Job,
    video: &Picked,
    crf: u8,
    (timescale, fd): (u32, i64),
    part: &Path,
    progress: Tell,
    stop: &AtomicBool,
) -> Result<(u64, u64, Mux)> {
    let mut keys: HashMap<usize, Vec<i64>> = HashMap::new();
    let mut plan: Vec<(usize, Part)> = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    for (n, piece) in job.pieces.iter().enumerate() {
        let si = job.index[n];
        if let std::collections::hash_map::Entry::Vacant(slot) = keys.entry(si) {
            slot.insert(
                keyframes(&job.sources[si].path, &super::route::Prefs::default())?
                    .into_iter()
                    .map(|t| job.frame_of(t))
                    .collect(),
            );
        }
        let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
        let mut split = Vec::new();
        for p in parts(a, b, &keys[&si]) {
            match p {
                Part::Encode { from, frames } if keys[&si].contains(&from) => {
                    let j = closed_prefix(job, si, from, frames)?;
                    if j > 0 {
                        split.push(Part::Copy { from, frames: j });
                    }
                    if frames > j {
                        split.push(Part::Encode {
                            from: from + j,
                            frames: frames - j,
                        });
                    }
                }
                p => split.push(p),
            }
        }
        for p in split {
            let i = match plan.iter().position(|q| *q == (si, p)) {
                Some(i) => i,
                None => {
                    plan.push((si, p));
                    plan.len() - 1
                }
            };
            order.push(i);
        }
    }
    let edges: Vec<usize> = (0..plan.len())
        .filter(|&i| matches!(plan[i].1, Part::Encode { .. }))
        .collect();
    let workers = edges.len().clamp(1, 4);
    let per = (threads() * 2 / workers).max(2);
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, Result<Edge>)>();
    let total: i64 = plan
        .iter()
        .map(|(_, p)| match p {
            Part::Copy { frames, .. } | Part::Encode { frames, .. } => *frames,
        })
        .sum::<i64>()
        .max(1);
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
    let mut entries = Entries {
        ids: HashMap::new(),
        first: None,
        later: Vec::new(),
    };
    let mut entry_of: HashMap<usize, u32> = HashMap::new();
    for &i in &order {
        if entry_of.contains_key(&i) {
            continue;
        }
        let (si, p) = plan[i];
        let id = match p {
            Part::Copy { .. } => {
                let r = job.sources[si].reader()?;
                let t = &r.tracks[r.video().ok_or_else(|| Error::Invalid("no video".into()))?];
                let entry = t
                    .entry()
                    .cloned()
                    .ok_or_else(|| Error::Invalid("video track without an entry".into()))?;
                entries.id(t.extradata().unwrap_or_default(), entry)
            }
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
    for &i in &order {
        let (si, p) = plan[i];
        let entry = entry_of.get(&i).copied().unwrap_or(0);
        match p {
            Part::Copy { from, frames } => {
                let r = job.sources[si].reader()?;
                let v = r.video().ok_or_else(|| Error::Invalid("no video".into()))?;
                let t = &r.tracks[v];
                let ka = t.keyframe_before(t.sample_at(t.ticks(job.seconds(from))).unwrap_or(0));
                let kb = ka.saturating_add(frames as usize).min(t.samples.len());
                let base = t.presentation(ka).unwrap_or(0);
                let ft = t.timescale as f64 * job.fps.1 as f64 / job.fps.0 as f64;
                for k in ka..kb {
                    let smp = t.samples[k];
                    let pts = smp.pts.saturating_add(t.edit_offset);
                    let dts = smp.dts.saturating_add(t.edit_offset);
                    let rp = (pts.saturating_sub(base) as f64 / ft).round() as i64;
                    let rd = (dts.saturating_sub(base) as f64 / ft).round() as i64;
                    r.read_into(v, k, &mut buf)?;
                    mux.push(
                        &buf,
                        at.saturating_add(rp).saturating_mul(fd),
                        at.saturating_add(rd).saturating_mul(fd),
                        smp.keyframe,
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
        let frames = match p {
            Part::Copy { frames, .. } | Part::Encode { frames, .. } => frames,
        };
        at += frames;
    }
    Ok((copied, encoded, mux))
}
