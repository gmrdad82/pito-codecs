use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use rayon::prelude::*;

use crate::gpu::GpuFrame;
use crate::mp4::{CodecConfig, Reader, Track};

use super::demux::{self, Demux, Stream};

use super::picture::{Color, rotate, to_rgba_color};
use super::probe::{AudioTrack, Probe};
use super::sound::Sound;
use super::types::{
    AudioBuffer, Codec, Direction, Error, Frame, Packet, Result, Rgba, Route,
};

const NO_GPU: &str = if cfg!(target_os = "macos") {
    "no VideoToolbox hardware decoder or encoder"
} else if cfg!(target_os = "windows") {
    "no Media Foundation hardware decoder or NVENC encoder"
} else {
    "no VA-API device or driver"
};

pub fn gpu() -> (Vec<(Codec, Direction)>, Option<String>) {
    static GPU: OnceLock<Vec<(Codec, Direction)>> = OnceLock::new();
    let found = GPU
        .get_or_init(|| {
            crate::gpu::available()
                .into_iter()
                .map(|(c, d, _)| {
                    (
                        c,
                        match d {
                            crate::gpu::Direction::Decode => Direction::Decode,
                            crate::gpu::Direction::Encode => Direction::Encode,
                        },
                    )
                })
                .collect()
        })
        .clone();
    let why = found
        .is_empty()
        .then(|| NO_GPU.to_string());
    (found, why)
}

pub fn cpu() -> Vec<(Codec, Direction)> {
    vec![
        (Codec::H264, Direction::Decode),
        (Codec::H264, Direction::Encode),
        (Codec::Hevc, Direction::Decode),
        (Codec::Vp9, Direction::Decode),
        (Codec::Aac, Direction::Decode),
        (Codec::Aac, Direction::Encode),
    ]
}

type Key = (PathBuf, u64, Option<SystemTime>);

fn readers() -> &'static Mutex<HashMap<Key, Arc<Reader>>> {
    static READERS: OnceLock<Mutex<HashMap<Key, Arc<Reader>>>> = OnceLock::new();
    READERS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn reader(path: &Path) -> Result<Arc<Reader>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::at(path, e))?;
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if let Some(r) = readers()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
    {
        return Ok(r.clone());
    }
    let r = Arc::new(Reader::open(path)?);
    let mut map = readers().lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= 64 {
        map.clear();
    }
    map.insert(key, r.clone());
    Ok(r)
}

pub fn rational(fps: f64) -> (u32, u32) {
    if !fps.is_finite() || fps <= 0.0 {
        return (30, 1);
    }
    let fps = fps.clamp(0.001, 1000.0);
    if fps.round() >= 1.0 && (fps - fps.round()).abs() < 0.01 {
        return (fps.round() as u32, 1);
    }
    let ntsc = fps * 1.001;
    if ntsc.round() >= 1.0 && (ntsc - ntsc.round()).abs() < 0.01 {
        return ((ntsc.round() as u32).saturating_mul(1000), 1001);
    }
    (((fps * 1000.0).round() as u32).max(1), 1000)
}

fn video_track(r: &Reader) -> Result<(usize, &Track)> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    Ok((v, &r.tracks[v]))
}

const GENERIC: [&str; 8] = [
    "handler",
    "soundhandler",
    "videohandler",
    "gpac",
    "l-smash",
    "core media",
    "apple",
    "mainconcept",
];

pub fn probe(path: &Path) -> Result<Probe> {
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::probe(&r),
    };
    let (_, t) = video_track(&r)?;
    let (width, height) = t
        .entry()
        .and_then(|e| e.video)
        .map(|v| (v.width as u32, v.height as u32))
        .unwrap_or((t.width, t.height));
    let (profile, bit_depth) = match t.config() {
        Some(CodecConfig::Avc(c)) => demux::avc_profile(c.profile),
        Some(CodecConfig::Hevc(c)) => demux::hevc_profile(c),
        _ => (String::new(), 8),
    };
    let audio = r
        .audio()
        .into_iter()
        .enumerate()
        .map(|(index, i)| {
            let a = &r.tracks[i];
            let name = a.name.trim();
            AudioTrack {
                index,
                stream: i,
                title: (!name.is_empty()
                    && !GENERIC.iter().any(|g| name.to_lowercase().contains(g)))
                .then(|| name.to_string()),
                language: (!a.language.is_empty() && a.language != "und")
                    .then(|| a.language.clone()),
                codec: a.config().map(|c| c.name()).unwrap_or_default(),
                channels: a.channels() as u16,
                rate: a.sample_rate(),
                bitrate: Some(a.bitrate()).filter(|b| *b > 0),
                default: a.enabled,
            }
        })
        .collect();
    Ok(Probe {
        duration: t.duration_seconds(),
        fps: rational(t.fps()),
        width,
        height,
        rotation: t.rotation(),
        video_codec: t.codec(),
        video_codec_name: t.config().map(|c| c.name()).unwrap_or_default(),
        profile,
        bit_depth,
        frames: Some(t.samples.len() as u64),
        audio,
    })
}

pub fn keyframes(path: &Path) -> Result<Vec<f64>> {
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::keyframes(&r),
    };
    let (_, t) = video_track(&r)?;
    let mut keys: Vec<f64> = t
        .keyframes()
        .iter()
        .filter_map(|&k| t.presentation(k as usize))
        .map(|p| t.seconds(p).max(0.0))
        .collect();
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    Ok(keys)
}

pub enum Decoder {
    H264 {
        dec: Box<crate::h264::Decoder>,
        length: usize,
        params: Vec<u8>,
        buf: Vec<u8>,
    },
    Hevc(Box<crate::hevc::Decoder>),
    Vp9(Box<crate::vp9::Decoder>),
    Gpu(Box<crate::gpu::HwDecoder>),
}

fn annexb_into(out: &mut Vec<u8>, data: &[u8], length: usize) {
    let mut pos = 0;
    while length > 0 && pos + length <= data.len() {
        let n = data[pos..pos + length]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length;
        let Some(nal) = data.get(pos..pos + n) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        pos += n;
    }
}

impl Decoder {
    pub fn for_route(
        st: &Stream,
        threads: usize,
        route: Route,
    ) -> Result<(Decoder, Option<String>)> {
        if route == Route::Gpu {
            let codec = st
                .codec
                .ok_or_else(|| Error::Unsupported("an unknown video codec".to_string()))?;
            match crate::gpu::HwDecoder::new(codec, &st.extradata) {
                Ok(d) => {
                    let d = d.with_time_base(st.time_base);
                    return Ok((Decoder::Gpu(Box::new(d)), None));
                }
                Err(e) => {
                    let why = format!("the GPU decoder failed ({e}); decoding on the CPU");
                    return Ok((Decoder::new(st, threads)?, Some(why)));
                }
            }
        }
        Ok((Decoder::new(st, threads)?, None))
    }

    pub fn route(&self) -> Route {
        match self {
            Decoder::Gpu(_) => Route::Gpu,
            _ => Route::Cpu,
        }
    }

    pub fn new(st: &Stream, threads: usize) -> Result<Decoder> {
        let tb = st.time_base;
        match (st.codec, &st.avc) {
            (Some(Codec::H264), Some(c)) => {
                let mut dec = crate::h264::Decoder::with_threads(threads.max(1));
                dec.set_time_base(tb);
                let mut params = Vec::new();
                for nal in c.sps.iter().chain(&c.pps) {
                    params.extend_from_slice(&[0, 0, 0, 1]);
                    params.extend_from_slice(nal);
                }
                Ok(Decoder::H264 {
                    dec: Box::new(dec),
                    length: c.length_size.clamp(1, 4) as usize,
                    params,
                    buf: Vec::new(),
                })
            }
            (Some(Codec::Hevc), _) => {
                if st.extradata.is_empty() {
                    return Err(Error::Invalid("HEVC track without hvcC".to_string()));
                }
                let mut dec = crate::hevc::Decoder::with_threads(threads.max(1));
                dec.configure_hvcc(&st.extradata)?;
                dec.set_time_base(tb);
                Ok(Decoder::Hevc(Box::new(dec)))
            }
            (Some(Codec::Vp9), _) => {
                let mut dec = crate::vp9::Decoder::with_threads(threads.max(1));
                dec.set_time_base(tb);
                Ok(Decoder::Vp9(Box::new(dec)))
            }
            _ => Err(Error::Unsupported(format!(
                "our decoders read H.264, HEVC and VP9, not {}",
                st.name
            ))),
        }
    }

    pub fn decode(&mut self, p: &Packet) -> Result<Vec<Frame>> {
        match self {
            Decoder::H264 {
                dec,
                length,
                params,
                buf,
            } => {
                buf.clear();
                if p.keyframe {
                    buf.extend_from_slice(params);
                }
                annexb_into(buf, &p.data, *length);
                let packet = Packet {
                    data: std::mem::take(buf),
                    pts: p.pts,
                    dts: p.dts,
                    keyframe: p.keyframe,
                };
                let out = dec.decode(&packet);
                *buf = packet.data;
                out
            }
            Decoder::Hevc(dec) => dec.decode(p),
            Decoder::Vp9(dec) => dec.decode(p),
            Decoder::Gpu(dec) => dec.decode(p),
        }
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        match self {
            Decoder::H264 { dec, .. } => dec.flush(),
            Decoder::Hevc(dec) => dec.flush(),
            Decoder::Vp9(dec) => dec.flush(),
            Decoder::Gpu(dec) => dec.flush(),
        }
    }

    pub fn recycle(&mut self, frame: Frame) {
        if let Decoder::H264 { dec, .. } = self {
            dec.recycle(frame);
        }
    }

    pub fn color(&self) -> Color {
        let (matrix, full_range) = match self {
            Decoder::H264 { dec, .. } => dec.color().map(|c| (c.matrix, c.full_range)),
            Decoder::Hevc(dec) => dec.color().map(|c| (c.matrix, c.full_range)),
            Decoder::Vp9(_) | Decoder::Gpu(_) => None,
        }
        .unwrap_or((2, false));
        Color { matrix, full_range }
    }
}

pub fn threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16)
}


const BEHIND: usize = 6;

pub struct Frames {
    pub reason: Option<String>,
    demux: Demux,
    track: usize,
    dec: Decoder,
    next: usize,
    start: i64,
    queue: VecDeque<Frame>,
    gpu: VecDeque<GpuFrame>,
    flushed: bool,
    buf: Vec<u8>,
}

impl Frames {
    pub fn open(demux: Demux, from: f64, threads: usize, route: Route) -> Result<Frames> {
        let track = demux.video()?;
        let (dec, reason) = Decoder::for_route(&demux.stream(track)?, threads, route)?;
        let (next, start) = demux.seek(track, from)?;
        Ok(Frames {
            reason,
            demux,
            track,
            dec,
            next,
            start,
            queue: VecDeque::new(),
            gpu: VecDeque::new(),
            flushed: false,
            buf: Vec::new(),
        })
    }

    pub fn route(&self) -> Route {
        self.dec.route()
    }

    pub fn recycle(&mut self, frame: Frame) {
        self.dec.recycle(frame);
    }

    pub fn seconds(&self, ticks: i64) -> f64 {
        self.demux.seconds(self.track, ticks)
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            if let Some(f) = self.next_queued() {
                return Ok(Some(f));
            }
            if !self.pump(false)? {
                return Ok(None);
            }
        }
    }

    pub fn next_gpu(&mut self) -> Result<Option<GpuFrame>> {
        loop {
            while self.gpu.len() > BEHIND || (self.flushed && !self.gpu.is_empty()) {
                let Some(f) = self.gpu.pop_front() else { break };
                if f.pts() >= self.start {
                    return Ok(Some(f));
                }
            }
            if let Some(f) = self.next_queued() {
                return Ok(Some(GpuFrame::cpu(f)));
            }
            if !self.pump(true)? {
                return Ok(None);
            }
        }
    }

    fn next_queued(&mut self) -> Option<Frame> {
        while let Some(f) = self.queue.pop_front() {
            if f.pts >= self.start {
                return Some(f);
            }
            self.dec.recycle(f);
        }
        None
    }

    fn pump(&mut self, gpu: bool) -> Result<bool> {
        if let Some((pts, dts, keyframe)) = self.demux.sample(self.track, self.next) {
            self.demux
                .read_into(self.track, self.next, &mut self.buf)?;
            let packet = Packet {
                data: std::mem::take(&mut self.buf),
                pts,
                dts,
                keyframe,
            };
            self.next += 1;
            let out = match &mut self.dec {
                Decoder::Gpu(dec) if gpu => dec.decode_gpu(&packet).map(|f| self.gpu.extend(f)),
                dec => dec.decode(&packet).map(|f| self.queue.extend(f)),
            };
            self.buf = packet.data;
            out?;
        } else if !self.flushed {
            self.flushed = true;
            match &mut self.dec {
                Decoder::Gpu(dec) if gpu => self.gpu.extend(dec.flush_gpu()?),
                dec => self.queue.extend(dec.flush()?),
            }
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}

pub fn still(reader: &Reader, dec: &mut Decoder, at: f64, height: u32) -> Result<Rgba> {
    let (track, t) = video_track(reader)?;
    let i = t.keyframe_before(t.sample_at(t.ticks(at.max(0.0))).unwrap_or(0));
    let packet = reader.packet(track, i)?;
    let mut frames = dec.decode(&packet)?;
    frames.extend(dec.flush()?);
    let mut frames = frames.into_iter();
    let frame = frames
        .next()
        .ok_or_else(|| Error::Invalid(format!("no picture at {at:.3} s")))?;
    for f in frames {
        dec.recycle(f);
    }
    let rotation = t.rotation();
    let pre = if rotation % 180 == 90 && frame.height > 0 {
        let w = (height as u64 * frame.height as u64 / frame.width.max(1) as u64) as u32;
        w.max(2) & !1
    } else {
        height
    };
    let img = to_rgba_color(&frame, pre, dec.color());
    dec.recycle(frame);
    Ok(rotate(&img, rotation))
}

pub fn still_decoder(reader: &Arc<Reader>, route: Route) -> Result<Decoder> {
    let demux = Demux::Mp4(reader.clone());
    let track = demux.video()?;
    Ok(Decoder::for_route(&demux.stream(track)?, 1, route)?.0)
}

const PAD: f64 = 0.02;
const CHUNK: f64 = 4.0;

fn audio_tracks(d: &Demux) -> Vec<usize> {
    match d {
        Demux::Mp4(r) => r.audio(),
        Demux::Mkv(r) => r.audio(),
    }
}

fn pcm_span(t: &crate::mkv::Track, sound: &Sound) -> Option<(f64, u64)> {
    let first = t.samples.first()?;
    let mut frames = 0u64;
    for s in &t.samples {
        frames = frames.saturating_add(sound.pcm_frames(s.size)?);
    }
    Some((t.seconds(first.pts), frames))
}

fn audio_length(d: &Demux, i: usize, sound: &Sound) -> f64 {
    match d {
        Demux::Mp4(r) => r.tracks.get(i).map_or(0.0, Track::duration_seconds),
        Demux::Mkv(r) => r.tracks.get(i).map_or(0.0, |t| match pcm_span(t, sound) {
            Some((start, frames)) => (start + frames as f64 / sound.rate().max(1) as f64).max(0.0),
            None => t.seconds(t.end()).max(0.0),
        }),
    }
}

pub fn audio_range(
    path: &Path,
    track: usize,
    from: f64,
    to: f64,
    rate: u32,
    channels: u16,
) -> Result<AudioBuffer> {
    let d = Demux::open(path)?;
    let ids = audio_tracks(&d);
    let &i = ids
        .get(track)
        .ok_or_else(|| Error::Invalid(format!("no audio track {track}")))?;
    let sound = Sound::open(&d, i)?;
    let pad = sound.pre_roll().max(PAD);
    let at = |t: f64| (t * rate as f64).round() as usize;
    let start = at(from.max(0.0));
    let length = audio_length(&d, i, &sound);
    let end = if to.is_finite() {
        Some(at(to))
    } else if length > 0.0 {
        Some(at(length).max(start))
    } else {
        None
    };
    let whole = matches!(&d, Demux::Mkv(_)) && sound.pcm_frames(0).is_some();
    let Some(end) = end.filter(|_| !whole) else {
        return piece(&d, i, start, end, rate, channels, pad);
    };
    let chunk = at(CHUNK).max(1);
    let n = (end - start).div_ceil(chunk).max(1);
    if n == 1 {
        return piece(&d, i, start, Some(end), rate, channels, pad);
    }
    let pieces = (0..n)
        .into_par_iter()
        .map(|k| {
            let s = start + k * chunk;
            piece(&d, i, s, Some((s + chunk).min(end)), rate, channels, pad)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut out = AudioBuffer::new(rate, channels);
    out.samples.reserve((end - start) * channels as usize);
    for p in pieces {
        out.samples.extend_from_slice(&p.samples);
    }
    Ok(out)
}

fn piece(
    d: &Demux,
    i: usize,
    start: usize,
    end: Option<usize>,
    rate: u32,
    channels: u16,
    pad: f64,
) -> Result<AudioBuffer> {
    let pad = start.min((pad * rate as f64).round() as usize);
    let t0 = (start - pad) as f64 / rate as f64;
    let t1 = end.map_or(f64::INFINITY, |e| e as f64 / rate as f64 + PAD);
    let raw = decode_track(d, i, t0, t1)?;
    let buf = if channels > raw.channels {
        shape(at_rate(raw, rate), channels)?
    } else {
        at_rate(shape(raw, channels)?, rate)
    };
    Ok(cut(buf, pad, end.map(|e| e - start)))
}

fn at_rate(buf: AudioBuffer, rate: u32) -> AudioBuffer {
    if buf.rate == rate {
        buf
    } else {
        crate::aac::resample(&buf, rate)
    }
}

pub fn shape(buf: AudioBuffer, channels: u16) -> Result<AudioBuffer> {
    Ok(match channels {
        c if c == buf.channels => buf,
        1 => crate::aac::mono(&buf),
        2 => crate::aac::downmix_stereo(&buf),
        c => {
            return Err(Error::Invalid(format!(
                "{c} channels from a {}-channel track: ask for 1, 2 or {}",
                buf.channels, buf.channels
            )));
        }
    })
}

pub fn cut(mut buf: AudioBuffer, skip: usize, want: Option<usize>) -> AudioBuffer {
    let ch = buf.channels.max(1) as usize;
    buf.samples.drain(..(skip * ch).min(buf.samples.len()));
    if let Some(w) = want {
        buf.samples.resize(w * ch, 0.0);
    }
    buf
}

pub fn decode_audio(r: &Arc<Reader>, i: usize, from: f64, to: f64) -> Result<AudioBuffer> {
    decode_track(&Demux::Mp4(r.clone()), i, from, to)
}

type First = (usize, usize, f64, Option<f64>);

fn first_sample(d: &Demux, i: usize, from: f64, sound: &Sound) -> Result<First> {
    let missing = || Error::Invalid(format!("no track {i}"));
    Ok(match d {
        Demux::Mp4(r) => {
            let t = r.tracks.get(i).ok_or_else(missing)?;
            let from_t = t.ticks(from);
            let first = t
                .samples
                .partition_point(|s| {
                    s.pts.saturating_add(t.edit_offset) + s.duration as i64 <= from_t
                })
                .saturating_sub(1);
            let start = t.seconds(t.presentation(first).unwrap_or(0));
            (first, t.samples.len(), start, None)
        }
        Demux::Mkv(r) => {
            let t = r.tracks.get(i).ok_or_else(missing)?;
            if let Some((start, _)) = pcm_span(t, sound) {
                let rate = sound.rate().max(1) as f64;
                let mut at = 0u64;
                for (k, s) in t.samples.iter().enumerate() {
                    let next = at.saturating_add(sound.pcm_frames(s.size).unwrap_or(0));
                    if start + next as f64 / rate > from {
                        return Ok((k, t.samples.len(), start + at as f64 / rate, None));
                    }
                    at = next;
                }
                return Ok((
                    t.samples.len(),
                    t.samples.len(),
                    start + at as f64 / rate,
                    None,
                ));
            }
            let from_t = t.ticks(from);
            let first = t
                .samples
                .partition_point(|s| s.pts.saturating_add(s.duration as i64) <= from_t)
                .saturating_sub(1);
            let delay = t.codec_delay as f64 / 1e9;
            let fix = t.seconds(t.ticks(delay)) - delay;
            let start = t.seconds(t.samples.get(first).map_or(0, |s| s.pts)) + fix;
            let origin = t.samples.first().map(|s| t.seconds(s.pts) + fix);
            (first, t.samples.len(), start, origin)
        }
    })
}

fn on_grid(start: f64, origin: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return start;
    }
    let snapped = origin + ((start - origin) / step).round() * step;
    if (snapped - start).abs() <= (step * 0.45).min(0.0011) {
        snapped
    } else {
        start
    }
}

fn decode_track(d: &Demux, i: usize, from: f64, to: f64) -> Result<AudioBuffer> {
    let mut dec = Sound::open(d, i)?;
    let rate = dec.rate().max(1);
    let channels = dec.channels().max(1);
    let mut out = AudioBuffer::new(rate, channels);
    let (first, count, start, origin) = first_sample(d, i, from.max(0.0), &dec)?;
    let skip_from = |start: f64| {
        if from > 0.0 || start < 0.0 {
            ((from.max(0.0) - start) * rate as f64).round().max(0.0) as usize
        } else {
            0
        }
    };
    let mut skip = origin.is_none().then(|| skip_from(start));
    let end = audio_length(d, i, &dec);
    let to = if end > 0.0 { to.min(end) } else { to };
    let want = to
        .is_finite()
        .then(|| ((to - from.max(0.0)) * rate as f64).round().max(0.0) as usize);
    let mut buf = Vec::new();
    let mut pcm = Vec::new();
    let mut skipped = 0usize;
    for k in first..count {
        if let Some(w) = want
            && out.frames() >= w
        {
            break;
        }
        d.read_into(i, k, &mut buf)?;
        let got = dec.decode(&buf, &mut pcm)?;
        let ch = got.max(1) as usize;
        let frames = pcm.len() / ch;
        let skip = *skip.get_or_insert_with(|| {
            skip_from(origin.map_or(start, |o| on_grid(start, o, frames as f64 / rate as f64)))
        });
        let drop = (skip - skipped).min(frames);
        skipped += drop;
        if got == channels {
            out.samples
                .extend_from_slice(pcm.get(drop * ch..frames * ch).unwrap_or_default());
        }
    }
    if let Some(w) = want {
        out.samples.resize(w * channels as usize, 0.0);
    }
    Ok(out)
}
