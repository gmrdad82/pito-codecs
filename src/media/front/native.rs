use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use rayon::prelude::*;

use crate::gpu::GpuFrame;
use crate::mp4::{CodecConfig, Reader, Track};

use super::demux::{self, Demux, Stream};
use super::feed::Feed;

use super::picture::{Color, fit_width, rotate, to_rgba_color};
use super::probe::{AudioTrack, Probe};
use super::sound::Sound;
use crate::media::{AudioBuffer, Codec, Direction, Error, Frame, Packet, Result, Rgba, Route};

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
    let why = found.is_empty().then(|| NO_GPU.to_string());
    (found, why)
}

pub fn cpu() -> Vec<(Codec, Direction)> {
    let mut caps = vec![
        (Codec::H264, Direction::Decode),
        (Codec::H264, Direction::Encode),
        (Codec::Hevc, Direction::Decode),
        (Codec::Vp9, Direction::Decode),
        (Codec::Vp9, Direction::Encode),
        (Codec::Aac, Direction::Decode),
        (Codec::Aac, Direction::Encode),
    ];
    if cfg!(feature = "mp3") {
        caps.push((Codec::Mp3, Direction::Decode));
    }
    if cfg!(feature = "ac3") {
        caps.push((Codec::Ac3, Direction::Decode));
        caps.push((Codec::Eac3, Direction::Decode));
    }
    caps
}

pub(super) type Key = (PathBuf, u64, Option<SystemTime>);

pub(super) fn key(path: &Path) -> Result<Key> {
    let meta = std::fs::metadata(path).map_err(|e| Error::at(path, e))?;
    Ok((path.to_path_buf(), meta.len(), meta.modified().ok()))
}

pub(super) type Readers<R> = LazyLock<Mutex<HashMap<Key, Arc<OnceLock<Option<Arc<R>>>>>>>;

pub(super) fn cached<R>(
    readers: &Readers<R>,
    path: &Path,
    open: impl FnOnce(&Path) -> Result<R>,
) -> Result<Arc<R>> {
    let key = key(path)?;
    let slot = {
        let mut map = readers.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= 64 && !map.contains_key(&key) {
            map.clear();
        }
        map.entry(key.clone()).or_default().clone()
    };
    let mut failed = None;
    let got = slot.get_or_init(|| match open(path) {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            failed = Some(e);
            None
        }
    });
    if let Some(r) = got {
        return Ok(r.clone());
    }
    let mut map = readers.lock().unwrap_or_else(PoisonError::into_inner);
    if map.get(&key).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
        map.remove(&key);
    }
    drop(map);
    match failed {
        Some(e) => Err(e),
        None => Err(Error::Invalid(format!(
            "{} could not be opened",
            path.display()
        ))),
    }
}

pub fn reader(path: &Path) -> Result<Arc<Reader>> {
    static READERS: Readers<Reader> = LazyLock::new(Default::default);
    cached(&READERS, path, |p| Reader::open(p))
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

pub(super) fn video_track(r: &Reader) -> Result<(usize, &Track)> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    Ok((v, &r.tracks[v]))
}

pub(super) fn gop_start(t: &Track, seconds: f64) -> usize {
    t.keyframe_before(t.sample_at(t.ticks(seconds)).unwrap_or(0))
}

pub(super) fn sorted(mut keys: Vec<f64>) -> Vec<f64> {
    keys.sort_by(f64::total_cmp);
    keys.dedup();
    keys
}

const GENERIC: [&str; 6] = [
    "handler",
    "gpac",
    "l-smash",
    "core media",
    "apple",
    "mainconcept",
];

pub fn probe(path: &Path) -> Result<Probe> {
    if let Ok(Some(head)) = demux::mkv_head(path)
        && let Ok(Some(p)) = demux::probe_head(&head)
    {
        return Ok(p);
    }
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::probe(&r),
    };
    let (v, t) = video_track(&r)?;
    let starts = Demux::Mp4(r.clone()).starts();
    let (width, height) = t
        .entry()
        .and_then(|e| e.video)
        .map(|v| (v.width as u32, v.height as u32))
        .unwrap_or((t.width, t.height));
    let (profile, bit_depth) = match t.config() {
        Some(CodecConfig::Avc(c)) => demux::avc_profile(c.profile),
        Some(CodecConfig::Hevc(c)) => demux::hevc_profile(c),
        Some(CodecConfig::Vp9(c)) => (format!("Profile {}", c.profile), c.bit_depth.max(8)),
        _ => (String::new(), 8),
    };
    let audio = r
        .audio()
        .into_iter()
        .enumerate()
        .map(|(index, i)| {
            let a = &r.tracks[i];
            let name = a.name.trim();
            let lower = name.to_lowercase();
            AudioTrack {
                index,
                stream: i,
                title: (!name.is_empty() && !GENERIC.iter().any(|g| lower.contains(g)))
                    .then(|| name.to_string()),
                language: (!a.language.is_empty() && a.language != "und")
                    .then(|| a.language.clone()),
                codec: a.config().map(|c| c.name()).unwrap_or_default(),
                channels: a.channels() as u16,
                rate: a.sample_rate(),
                bitrate: Some(a.bitrate()).filter(|b| *b > 0),
                default: a.enabled,
                start: starts.track(i),
            }
        })
        .collect();
    let lead = starts.track(v).map_or(0.0, |s| (s - starts.file).max(0.0));
    Ok(Probe {
        duration: lead + t.duration_seconds(),
        start: starts.file,
        video_start: starts.track(v),
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
    if let Some(head) = demux::mkv_head(path)? {
        return demux::keyframes(&head);
    }
    let r = match Demux::open(path)? {
        Demux::Mp4(r) => r,
        Demux::Mkv(r) => return demux::keyframes(&r),
    };
    let start = Demux::Mp4(r.clone()).start();
    let (_, t) = video_track(&r)?;
    Ok(sorted(
        t.keyframes()
            .iter()
            .filter_map(|&k| t.presentation(k as usize))
            .map(|p| (t.seconds(p) - start).max(0.0))
            .collect(),
    ))
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

    pub fn stills(&mut self) {
        if let Decoder::Gpu(d) = self {
            d.stills();
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
        match self {
            Decoder::H264 { dec, .. } => dec.recycle(frame),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Decoder::Gpu(_) => crate::gpu::biplanar::recycle(frame),
            _ => {}
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
    shown: usize,
    flushed: bool,
    buf: Vec<u8>,
    cur: Option<(Box<crate::mkv::Cursor>, i64)>,
}

const REORDER: usize = 16;

fn mkv_start(r: &crate::mkv::Reader, track: usize, from: f64) -> Result<(crate::mkv::Cursor, i64)> {
    let t = r
        .tracks
        .get(track)
        .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
    let target = t.ticks(from.max(0.0));
    let mut cur = crate::mkv::Cursor::new(r);
    cur.seek(r, track, from.max(0.0))?;
    let mut scan = cur.clone();
    let (mut best, mut low) = (None::<i64>, None::<i64>);
    let mut after = None::<usize>;
    let mut first = true;
    while let Some(b) = scan.next_block(r, Some(track))? {
        if b.pts <= target {
            best = best.max(Some(b.pts));
        }
        low = Some(low.map_or(b.pts, |l| l.min(b.pts)));
        if !first && b.keyframe && b.pts > target && after.is_none() {
            after = Some(0);
        }
        if let Some(n) = after.as_mut() {
            *n += 1;
            if *n > REORDER {
                break;
            }
        }
        first = false;
    }
    Ok((cur, best.or(low).unwrap_or(0)))
}

impl Frames {
    pub fn open(demux: Demux, from: f64, route: Route) -> Result<Frames> {
        let track = demux.video()?;
        let (dec, reason) = Decoder::for_route(&demux.stream(track)?, threads(), route)?;
        let (next, start, cur) = match &demux {
            Demux::Mkv(r) => {
                let starts = demux.starts();
                let shift = starts.shift(track);
                let (cur, start) = mkv_start(r, track, from.max(0.0) + starts.file)?;
                (0, start.saturating_sub(shift), Some((Box::new(cur), shift)))
            }
            Demux::Mp4(_) => {
                let (next, start) = demux.seek(track, from)?;
                (next, start, None)
            }
        };
        Ok(Frames {
            reason,
            demux,
            track,
            dec,
            next,
            start,
            queue: VecDeque::new(),
            gpu: VecDeque::new(),
            shown: 0,
            flushed: false,
            buf: Vec::new(),
            cur,
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
            while self.gpu.len() > self.shown.min(BEHIND) || (self.flushed && !self.gpu.is_empty())
            {
                let Some(f) = self.gpu.pop_front() else { break };
                if f.pts() >= self.start {
                    self.shown = self.shown.saturating_add(1);
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

    fn packet(&mut self) -> Result<Option<(i64, i64, bool)>> {
        if let (Some((cur, shift)), Demux::Mkv(r)) = (self.cur.as_mut(), &self.demux) {
            return Ok(cur.next_of(r, Some(self.track), &mut self.buf)?.map(|b| {
                let pts = b.pts.saturating_sub(*shift);
                (pts, pts, b.keyframe)
            }));
        }
        let Some(s) = self.demux.sample(self.track, self.next) else {
            return Ok(None);
        };
        self.demux.read_into(self.track, self.next, &mut self.buf)?;
        Ok(Some(s))
    }

    fn pump(&mut self, gpu: bool) -> Result<bool> {
        if let Some((pts, dts, keyframe)) = self.packet()? {
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

pub fn still(reader: &Arc<Reader>, dec: &mut Decoder, at: f64, height: u32) -> Result<Rgba> {
    let start = Demux::Mp4(reader.clone()).start();
    let (track, t) = video_track(reader)?;
    let packet = reader.packet(track, gop_start(t, at.max(0.0) + start))?;
    picture(dec, &packet, t.rotation(), at, height)
}

pub fn still_mkv(
    r: &Arc<crate::mkv::Reader>,
    cur: &mut crate::mkv::Cursor,
    dec: &mut Decoder,
    at: f64,
    height: u32,
) -> Result<Rgba> {
    let v = r
        .video()
        .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
    let rotation = r.tracks.get(v).map_or(0, |t| t.rotation());
    cur.seek(r, v, at.max(0.0) + Demux::Mkv(r.clone()).start())?;
    let mut data = Vec::new();
    let b = cur
        .next_of(r, Some(v), &mut data)?
        .ok_or_else(|| Error::Invalid(format!("no picture at {at:.3} s")))?;
    let packet = Packet {
        data,
        pts: b.pts,
        dts: b.pts,
        keyframe: b.keyframe,
    };
    picture(dec, &packet, rotation, at, height)
}

fn picture(
    dec: &mut Decoder,
    packet: &Packet,
    rotation: u16,
    at: f64,
    height: u32,
) -> Result<Rgba> {
    let mut frames = dec.decode(packet)?;
    frames.extend(dec.flush()?);
    let mut frames = frames.into_iter();
    let frame = frames
        .next()
        .ok_or_else(|| Error::Invalid(format!("no picture at {at:.3} s")))?;
    for f in frames {
        dec.recycle(f);
    }
    let color = dec.color();
    let img = rgba(&frame, rotation, height, color);
    dec.recycle(frame);
    Ok(img)
}

fn rgba(frame: &Frame, rotation: u16, height: u32, color: Color) -> Rgba {
    let pre = if rotation % 180 == 90 && frame.height > 0 {
        fit_width(frame.height, frame.width.max(1), height)
    } else {
        height
    };
    rotate(&to_rgba_color(frame, pre, color), rotation)
}

pub enum Packets<'a> {
    Indexed {
        d: &'a Demux,
        track: usize,
        next: usize,
    },
    Mkv {
        r: &'a crate::mkv::Reader,
        cur: &'a mut crate::mkv::Cursor,
        track: usize,
        shift: i64,
    },
}

impl Packets<'_> {
    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<(i64, i64, bool)>> {
        match self {
            Packets::Indexed { d, track, next } => {
                let Some(s) = d.sample(*track, *next) else {
                    return Ok(None);
                };
                d.read_into(*track, *next, buf)?;
                *next += 1;
                Ok(Some(s))
            }
            Packets::Mkv {
                r,
                cur,
                track,
                shift,
            } => Ok(cur.next_of(r, Some(*track), buf)?.map(|b| {
                let pts = b.pts.saturating_sub(*shift);
                (pts, pts, b.keyframe)
            })),
        }
    }
}

fn decode_any(dec: &mut Decoder, p: Option<&Packet>) -> Result<Vec<GpuFrame>> {
    match (dec, p) {
        (Decoder::Gpu(d), Some(p)) => d.decode_gpu(p),
        (Decoder::Gpu(d), None) => d.flush_gpu(),
        (d, Some(p)) => Ok(d.decode(p)?.into_iter().map(GpuFrame::cpu).collect()),
        (d, None) => Ok(d.flush()?.into_iter().map(GpuFrame::cpu).collect()),
    }
}

fn release(dec: &mut Decoder, f: GpuFrame) {
    if !f.on_gpu()
        && let Some(c) = f.into_cpu()
    {
        dec.recycle(c);
    }
}

fn shown(dec: &Decoder, f: &GpuFrame, rotation: u16, height: u32, tagged: Color) -> Result<Rgba> {
    let frame = f.download()?;
    let color = match dec {
        Decoder::Gpu(_) => tagged,
        d => d.color(),
    };
    Ok(rgba(&frame, rotation, height, color))
}

pub fn exact(
    src: &mut Packets,
    dec: &mut Decoder,
    targets: &[i64],
    rotation: u16,
    height: u32,
    tagged: Color,
) -> Result<Vec<Rgba>> {
    let mut out: Vec<Rgba> = Vec::with_capacity(targets.len());
    let mut held: Option<GpuFrame> = None;
    let mut buf = Vec::new();
    let mut flushed = false;
    while out.len() < targets.len() && !flushed {
        let frames = match src.next(&mut buf)? {
            Some((pts, dts, keyframe)) => {
                let p = Packet {
                    data: std::mem::take(&mut buf),
                    pts,
                    dts,
                    keyframe,
                };
                let got = decode_any(dec, Some(&p));
                buf = p.data;
                got?
            }
            None => {
                flushed = true;
                decode_any(dec, None)?
            }
        };
        for f in frames {
            while let Some(&t) = targets.get(out.len())
                && f.pts() > t
            {
                let pick = held.as_ref().unwrap_or(&f);
                out.push(shown(dec, pick, rotation, height, tagged)?);
            }
            if out.len() == targets.len() {
                release(dec, f);
            } else if let Some(old) = held.replace(f) {
                release(dec, old);
            }
        }
    }
    while out.len() < targets.len() {
        let pick = held
            .as_ref()
            .ok_or_else(|| Error::Invalid("no picture at that time".to_string()))?;
        out.push(shown(dec, pick, rotation, height, tagged)?);
    }
    if let Some(h) = held.take() {
        release(dec, h);
    }
    if !flushed {
        for f in decode_any(dec, None)? {
            release(dec, f);
        }
    }
    Ok(out)
}

pub fn still_decoder(reader: &Arc<Reader>, route: Route) -> Result<Decoder> {
    let mut dec = video_decoder(&Demux::Mp4(reader.clone()), route, 1)?;
    dec.stills();
    Ok(dec)
}

pub fn video_decoder(demux: &Demux, route: Route, threads: usize) -> Result<Decoder> {
    let track = demux.video()?;
    Ok(Decoder::for_route(&demux.stream(track)?, threads.max(1), route)?.0)
}

pub const PAD: f64 = 0.02;
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

fn audio_length(d: &Demux, i: usize, sound: &Sound) -> Result<f64> {
    let starts = d.starts();
    let base = starts.file;
    Ok(match d {
        Demux::Mp4(r) => r.tracks.get(i).map_or(0.0, |t| {
            starts.track(i).map_or(0.0, |s| (s - base).max(0.0)) + t.duration_seconds()
        }),
        Demux::Mkv(r) => match r.tracks.get(i) {
            Some(t) if t.samples.is_empty() => match super::feed::mkv_end(r, i)? {
                e if e > 0.0 => (e - base).max(0.0),
                _ => 0.0,
            },
            Some(t) => match pcm_span(t, sound) {
                Some((start, frames)) => {
                    (start + frames as f64 / sound.rate().max(1) as f64 - base).max(0.0)
                }
                None => (t.seconds(t.end()) - base).max(0.0),
            },
            None => 0.0,
        },
    })
}

pub fn audio_demux(path: &Path) -> Result<Demux> {
    match demux::mkv_head(path)? {
        Some(r) => Ok(Demux::Mkv(r)),
        None => Demux::open(path),
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
    let d = audio_demux(path)?;
    let i = audio_track(&d, track)?;
    let sound = Sound::open(&d, i)?;
    let pad = sound.pre_roll().max(PAD);
    let at = |t: f64| (t * rate as f64).round() as usize;
    let start = at(from.max(0.0));
    let whole = matches!(&d, Demux::Mkv(_)) && sound.pcm_frames(0).is_some();
    let end = if to.is_finite() {
        Some(at(to))
    } else if whole {
        None
    } else {
        let length = audio_length(&d, i, &sound)?;
        (length > 0.0).then(|| at(length).max(start))
    };
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
    let (raw, length) = decode_track(d, i, t0, t1)?;
    let last = (length > 0.0).then(|| ((length * rate as f64).round() as usize).max(start));
    let end = match (end, last) {
        (Some(e), Some(l)) => Some(e.min(l)),
        (e, l) => e.or(l),
    };
    let buf = if channels > raw.channels {
        shape(at_rate(raw, rate), channels)?
    } else {
        at_rate(shape(raw, channels)?, rate)
    };
    Ok(cut(buf, pad, end.map(|e| e - start)))
}

pub fn audio_track(d: &Demux, track: usize) -> Result<usize> {
    audio_tracks(d)
        .get(track)
        .copied()
        .ok_or_else(|| Error::Invalid(format!("no audio track {track}")))
}

pub(super) fn at_rate(buf: AudioBuffer, rate: u32) -> AudioBuffer {
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

pub fn on_grid(start: f64, origin: f64, step: f64) -> f64 {
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

fn decode_track(d: &Demux, i: usize, from: f64, to: f64) -> Result<(AudioBuffer, f64)> {
    let mut feed = Feed::open(d, i, from)?;
    let mut out = AudioBuffer::new(feed.rate, feed.channels);
    loop {
        if let Some(w) = feed.want(to)
            && out.frames() >= w
        {
            break;
        }
        if !feed.next(&mut out.samples)? {
            break;
        }
    }
    if let Some(w) = feed.want(to) {
        out.samples.resize(w * feed.channels as usize, 0.0);
    }
    Ok((out, feed.end()))
}
