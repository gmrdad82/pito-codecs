use std::collections::VecDeque;
use std::sync::Arc;

use crate::gpu::GpuFrame;
use crate::mp4::Reader;

pub use super::base::rational;
pub(super) use super::base::{gop_start, video_track};
use super::demux::{Demux, Stream};

use super::picture::{Color, fit_width, rotate, to_rgba_color};
use crate::media::{Codec, Error, Frame, Packet, Result, Rgba, Route};

pub enum Decoder {
    H264 {
        dec: Box<crate::h264::Decoder>,
        length: usize,
        params: Vec<u8>,
        buf: Vec<u8>,
    },
    Hevc(Box<crate::hevc::Decoder>),
    Vp9(Box<crate::vp9::Decoder>),
    #[cfg(feature = "av1")]
    Av1(Box<crate::av1::cpu::Decoder>),
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

    pub fn reset(&mut self) {
        if let Decoder::Gpu(d) = self {
            d.reset();
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
            #[cfg(feature = "av1")]
            (Some(Codec::Av1), _) => {
                let mut dec = crate::av1::cpu::Decoder::with_threads(threads.max(1));
                if !st.extradata.is_empty() {
                    let _ = dec.configure(&st.extradata);
                }
                dec.set_time_base(tb);
                Ok(Decoder::Av1(Box::new(dec)))
            }
            _ => Err(Error::Unsupported(format!(
                "our decoders read H.264, HEVC, VP9 and AV1, not {}",
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
            #[cfg(feature = "av1")]
            Decoder::Av1(dec) => dec.decode(p),
            Decoder::Gpu(dec) => dec.decode(p),
        }
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        match self {
            Decoder::H264 { dec, .. } => dec.flush(),
            Decoder::Hevc(dec) => dec.flush(),
            Decoder::Vp9(dec) => dec.flush(),
            #[cfg(feature = "av1")]
            Decoder::Av1(dec) => dec.flush(),
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
            #[cfg(feature = "av1")]
            Decoder::Av1(dec) => dec.sequence().map(|s| (s.matrix, s.full_range)),
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
    skip: Option<Skip>,
}

const REORDER: usize = 16;
const SEEN: usize = 64;

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

#[derive(Clone)]
pub(super) enum Skip {
    Nal(Codec, usize, Option<u8>),
    #[cfg(feature = "av1")]
    Av1(Box<crate::av1::parser::Parser>),
}

impl Skip {
    fn disposable(&self, data: &[u8]) -> bool {
        match self {
            Skip::Nal(codec, length, top) => disposable(*codec, *length, *top, data),
            #[cfg(feature = "av1")]
            Skip::Av1(parser) => av1_disposable(parser, data),
        }
    }
}

#[cfg(feature = "av1")]
fn av1_disposable(parser: &crate::av1::parser::Parser, data: &[u8]) -> bool {
    use crate::av1::parser::{ObuAction, ObuType};
    let mut pos = 0usize;
    let mut frames = 0usize;
    while let Some(rest) = data.get(pos..).filter(|r| !r.is_empty()) {
        let used = match parser.read_obu(rest) {
            Ok(ObuAction::Drop(used)) => used,
            Ok(ObuAction::Process(obu)) => {
                match obu.header.obu_type {
                    ObuType::Frame | ObuType::FrameHeader => match parser.frame_refresh(&obu) {
                        Ok(Some(0)) => frames += 1,
                        _ => return false,
                    },
                    ObuType::TemporalDelimiter
                    | ObuType::TileGroup
                    | ObuType::Metadata
                    | ObuType::RedundantFrameHeader
                    | ObuType::Padding => {}
                    _ => return false,
                }
                obu.bytes_used
            }
            Err(_) => return false,
        };
        if used == 0 {
            return false;
        }
        pos += used;
    }
    frames > 0
}

fn disposable(codec: Codec, length: usize, top: Option<u8>, data: &[u8]) -> bool {
    let mut pos = 0usize;
    let mut slices = false;
    while length > 0 && pos.saturating_add(length) <= data.len() {
        let n = data[pos..pos + length]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length;
        let Some(nal) = data.get(pos..pos.saturating_add(n)) else {
            return false;
        };
        let Some(&head) = nal.first() else {
            return false;
        };
        match codec {
            Codec::H264 => match head & 0x1f {
                1 if head & 0x60 == 0 => slices = true,
                6 | 9 | 12 => {}
                _ => return false,
            },
            Codec::Hevc => {
                let (Some(top), Some(&low)) = (top, nal.get(1)) else {
                    return false;
                };
                let kind = (head >> 1) & 0x3f;
                let layer = ((head & 1) << 5) | (low >> 3);
                match kind {
                    0 | 2 | 4 | 6 | 8 if layer == 0 && (low & 7) == top + 1 => slices = true,
                    35 | 38..=40 => {}
                    _ => return false,
                }
            }
            _ => return false,
        }
        pos += n;
    }
    slices && pos == data.len()
}

fn top_layer(config: &crate::mp4::HevcConfig) -> Option<u8> {
    let mut top = None;
    for sps in config
        .arrays
        .iter()
        .filter(|a| a.nal_type == 33)
        .flat_map(|a| &a.nalus)
    {
        let t = (sps.get(2)? >> 1) & 7;
        top = Some(top.map_or(t, |m: u8| m.max(t)));
    }
    top
}

pub(super) fn skipper(st: &Stream) -> Option<Skip> {
    match st.codec? {
        Codec::H264 => Some(Skip::Nal(
            Codec::H264,
            st.avc.as_ref()?.length_size.clamp(1, 4) as usize,
            None,
        )),
        Codec::Hevc => {
            let config = crate::mp4::HevcConfig::parse(&st.extradata).ok()?;
            Some(Skip::Nal(
                Codec::Hevc,
                config.length_size.clamp(1, 4) as usize,
                top_layer(&config),
            ))
        }
        #[cfg(feature = "av1")]
        Codec::Av1 => av1_skipper(&st.extradata),
        _ => None,
    }
}

#[cfg(feature = "av1")]
fn av1_skipper(av1c: &[u8]) -> Option<Skip> {
    use crate::av1::parser::{ObuAction, ObuType, Parser};
    let mut parser = Parser::default();
    let mut rest = av1c.get(4..)?;
    while !rest.is_empty() {
        let used = match parser.read_obu(rest).ok()? {
            ObuAction::Drop(used) => used,
            ObuAction::Process(obu) => {
                if obu.header.obu_type == ObuType::SequenceHeader {
                    parser.parse_sequence_header_obu(&obu).ok()?;
                    return Some(Skip::Av1(Box::new(parser)));
                }
                obu.bytes_used
            }
        };
        rest = rest.get(used.max(1)..)?;
    }
    None
}

impl Frames {
    pub fn open(demux: Demux, from: f64, route: Route) -> Result<Frames> {
        let track = demux.video()?;
        let (dec, reason) = Decoder::for_route(&demux.stream(track)?, threads(), route)?;
        Self::start(demux, track, from, (dec, reason))
    }

    pub(super) fn with_decoder(demux: Demux, from: f64, dec: Decoder) -> Result<Frames> {
        let track = demux.video()?;
        Self::start(demux, track, from, (dec, None))
    }

    pub(super) fn into_decoder(self) -> Decoder {
        self.dec
    }

    pub(super) fn advance(&mut self, from: f64) -> Result<()> {
        let start = match &self.demux {
            Demux::Mkv(r) => {
                let starts = self.demux.starts();
                let shift = starts.shift(self.track);
                let (_, start) = mkv_start(r, self.track, from.max(0.0) + starts.file)?;
                start.saturating_sub(shift)
            }
            Demux::Mp4(_) => self.demux.seek(self.track, from)?.1,
        };
        self.start = self.start.max(start);
        Ok(())
    }

    fn start(
        demux: Demux,
        track: usize,
        from: f64,
        (dec, reason): (Decoder, Option<String>),
    ) -> Result<Frames> {
        let skip = skipper(&demux.stream(track)?);
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
            skip,
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
            if pts < self.start
                && !keyframe
                && self.skip.as_ref().is_some_and(|s| s.disposable(&self.buf))
            {
                self.next += 1;
                return Ok(true);
            }
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
    fn upcoming(&self) -> Option<i64> {
        match self {
            Packets::Indexed { d, track, next } => d.sample(*track, *next).map(|s| s.0),
            Packets::Mkv { .. } => None,
        }
    }

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
    skip: Option<&Skip>,
) -> Result<Vec<Rgba>> {
    let mut out: Vec<Rgba> = Vec::with_capacity(targets.len());
    let mut held: Option<GpuFrame> = None;
    let mut buf = Vec::new();
    let mut flushed = false;
    let mut seen: VecDeque<i64> = VecDeque::with_capacity(SEEN);
    while out.len() < targets.len() && !flushed {
        let frames = match src.next(&mut buf)? {
            Some((pts, dts, keyframe)) => {
                let target = targets.iter().copied().find(|&t| t >= pts);
                let later = |q: i64| q > pts && target.is_some_and(|t| q <= t);
                let hidden = !keyframe
                    && (seen.iter().any(|&q| later(q)) || src.upcoming().is_some_and(later))
                    && skip.is_some_and(|s| s.disposable(&buf));
                if seen.len() == SEEN {
                    seen.pop_front();
                }
                seen.push_back(pts);
                if hidden {
                    continue;
                }
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
