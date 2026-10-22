use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::gpu::{GpuFrame, GpuScaler};
use crate::mp4::Reader;

use super::demux::Demux;
use super::native;
use super::picture::{Color, even, fit_width, resize};
use super::probe::{Probe, probe};
use super::route::{Caps, Prefs, pick_decode};
use crate::media::{Error, Frame, PixelFormat, Result, Route, TimeBase};

#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    pub probe: Probe,
    pub route: Route,
    pub reasons: Vec<String>,
    pub demux: Demux,
}

impl std::fmt::Debug for Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Video({} {}x{})", self.route, self.width, self.height)
    }
}

pub fn source(path: &Path, prefs: &Prefs) -> Result<Source> {
    let src = copied(path, prefs)?;
    src.probe.decodable()?;
    Ok(src)
}

pub(super) fn copied(path: &Path, prefs: &Prefs) -> Result<Source> {
    let probe = probe(path, prefs)?;
    let codec = probe.known_video()?;
    let picked = pick_decode(codec, prefs, &Caps::for_decode(prefs))?;
    let demux = Demux::open(path)?;
    Ok(Source {
        path: path.to_path_buf(),
        probe,
        route: picked.route,
        reasons: picked.reasons,
        demux,
    })
}

impl Source {
    pub fn native(&self) -> Error {
        Error::Unsupported(format!(
            "the {} route for {} is not in this build",
            self.route, self.probe.video_codec_name
        ))
    }

    pub fn demux(&self) -> &Demux {
        &self.demux
    }

    pub fn mp4(&self) -> Option<&Arc<Reader>> {
        self.demux.mp4()
    }
}

struct Inner(Box<native::Frames>, (u32, u32));

pub struct Video {
    inner: Inner,
    pub route: Route,
    pub reasons: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub time_base: TimeBase,
    left: Option<u64>,
}

pub fn decode_video(path: &Path, from: f64, height: Option<u32>, prefs: &Prefs) -> Result<Video> {
    let src = source(path, prefs)?;
    let p = &src.probe;
    let size = height
        .filter(|h| *h > 0 && p.height > 0 && *h != p.height)
        .map(|h| (fit_width(p.width, p.height, h), h));
    open(&src, from, None, size, None)
}

pub fn open(
    src: &Source,
    from: f64,
    frames: Option<u64>,
    size: Option<(u32, u32)>,
    fps: Option<(u32, u32)>,
) -> Result<Video> {
    let p = &src.probe;
    let decoded = native::Frames::open(src.demux.clone(), from, src.route)?;
    let mut reasons = src.reasons.clone();
    reasons.extend(decoded.reason.clone());
    Ok(native_video(
        decoded,
        reasons,
        size.unwrap_or((p.width, p.height)),
        fps.unwrap_or(p.fps),
        format_of(p),
        frames,
    ))
}

impl Video {
    pub(super) fn advance(&mut self, from: f64, frames: u64) -> Result<()> {
        self.inner.0.advance(from)?;
        self.left = Some(frames);
        Ok(())
    }

    pub fn recycle(&mut self, frame: Frame) {
        self.inner.0.recycle(frame);
    }

    fn read(&mut self) -> Result<Option<Frame>> {
        if self.left == Some(0) {
            return Ok(None);
        }
        let Inner(frames, fps) = &mut self.inner;
        let Some(mut frame) = frames.next_frame()? else {
            return Ok(None);
        };
        frame.pts = frame_at(frames.seconds(frame.pts), *fps);
        frame.time_base = self.time_base;
        if (frame.width, frame.height) != (self.width, self.height) {
            frame = resize(&frame, self.width, self.height);
        }
        if let Some(n) = self.left.as_mut() {
            *n -= 1;
        }
        Ok(Some(frame))
    }
}

impl Iterator for Video {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Result<Frame>> {
        self.read().transpose()
    }
}

pub const IN_FLIGHT: usize = 8;

pub enum VideoFrame {
    Gpu(GpuFrame),
    Cpu(Frame),
}

impl VideoFrame {
    pub fn pts(&self) -> i64 {
        match self {
            VideoFrame::Gpu(f) => f.pts(),
            VideoFrame::Cpu(f) => f.pts,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            VideoFrame::Gpu(f) => f.size(),
            VideoFrame::Cpu(f) => (f.width, f.height),
        }
    }

    pub fn on_gpu(&self) -> bool {
        matches!(self, VideoFrame::Gpu(_))
    }
}

enum GpuRoute {
    Gpu {
        scaler: Option<GpuScaler>,
        frames: Box<native::Frames>,
        fps: (u32, u32),
    },
    Cpu(Video),
}

pub struct GpuVideo {
    inner: GpuRoute,
    pub route: Route,
    pub reasons: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub time_base: TimeBase,
    pub color: Color,
}

impl std::fmt::Debug for GpuVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "GpuVideo({} {}x{}, {} on the GPU)",
            self.route,
            self.width,
            self.height,
            if self.on_gpu() { "frames" } else { "no frames" }
        )
    }
}

fn format_of(p: &Probe) -> PixelFormat {
    if p.bit_depth > 8 {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    }
}

pub(super) fn frame_at(seconds: f64, fps: (u32, u32)) -> i64 {
    (seconds * fps.0 as f64 / fps.1.max(1) as f64).round() as i64
}

fn stream_color(src: &Source) -> Color {
    let d = &src.demux;
    let found = d.video().ok().and_then(|v| d.stream(v).ok()?.color);
    let (matrix, full_range) = found.unwrap_or((2, false));
    Color { matrix, full_range }
}

pub fn decode_video_gpu(
    path: &Path,
    from: f64,
    size: Option<(u32, u32)>,
    prefs: &Prefs,
) -> Result<GpuVideo> {
    let src = source(path, prefs)?;
    let p = &src.probe;
    let size = size
        .filter(|&(w, h)| w > 0 && h > 0)
        .map(|(w, h)| (even(w), even(h)))
        .unwrap_or((p.width, p.height));
    let fps = p.fps;
    let color = stream_color(&src);
    let format = format_of(p);
    if src.route == Route::Gpu {
        let frames = native::Frames::open(src.demux.clone(), from, Route::Gpu)?;
        let mut reasons = src.reasons.clone();
        reasons.extend(frames.reason.clone());
        if frames.route() == Route::Gpu {
            let ten_bit = format == PixelFormat::Yuv420p10;
            let scaler = match GpuScaler::with_frames(size.0, size.1, ten_bit, IN_FLIGHT) {
                Ok(s) => Some(s),
                Err(e) if size == (p.width, p.height) => {
                    reasons.push(format!(
                        "no GPU scaler ({e}); frames are the decoder's own surfaces"
                    ));
                    None
                }
                Err(e) => {
                    reasons.push(format!("no GPU scaler ({e}); frames are scaled on the CPU"));
                    None
                }
            };
            return Ok(GpuVideo {
                inner: GpuRoute::Gpu {
                    scaler,
                    frames: Box::new(frames),
                    fps,
                },
                route: Route::Gpu,
                reasons,
                width: size.0,
                height: size.1,
                format,
                time_base: TimeBase::new(fps.1, fps.0),
                color,
            });
        }
        let video = native_video(frames, reasons, size, fps, format, None);
        return Ok(GpuVideo::cpu(video, color));
    }
    let video = open(&src, from, None, Some(size), Some(fps))?;
    Ok(GpuVideo::cpu(video, color))
}

fn native_video(
    frames: native::Frames,
    reasons: Vec<String>,
    size: (u32, u32),
    fps: (u32, u32),
    format: PixelFormat,
    left: Option<u64>,
) -> Video {
    Video {
        route: frames.route(),
        inner: Inner(Box::new(frames), fps),
        reasons,
        width: size.0,
        height: size.1,
        format,
        time_base: TimeBase::new(fps.1, fps.0),
        left,
    }
}

impl GpuVideo {
    fn cpu(video: Video, color: Color) -> Self {
        Self {
            route: video.route,
            reasons: video.reasons.clone(),
            width: video.width,
            height: video.height,
            format: video.format,
            time_base: video.time_base,
            color,
            inner: GpuRoute::Cpu(video),
        }
    }

    pub fn on_gpu(&self) -> bool {
        matches!(self.inner, GpuRoute::Gpu { .. })
    }

    pub fn recycle(&mut self, frame: VideoFrame) {
        let VideoFrame::Cpu(frame) = frame else {
            return;
        };
        match &mut self.inner {
            GpuRoute::Gpu { frames, .. } => frames.recycle(frame),
            GpuRoute::Cpu(video) => video.recycle(frame),
        }
    }

    fn read(&mut self) -> Result<Option<VideoFrame>> {
        let size = (self.width, self.height);
        let time_base = self.time_base;
        let (frames, scaler, fps) = match &mut self.inner {
            GpuRoute::Cpu(video) => return Ok(video.read()?.map(VideoFrame::Cpu)),
            GpuRoute::Gpu {
                frames,
                scaler,
                fps,
            } => (frames, scaler, *fps),
        };
        let Some(mut frame) = frames.next_gpu()? else {
            return Ok(None);
        };
        let pts = frame_at(frames.seconds(frame.pts()), fps);
        if !frame.on_gpu() {
            let Some(mut cpu) = frame.into_cpu() else {
                return Ok(None);
            };
            cpu.pts = pts;
            cpu.time_base = time_base;
            if (cpu.width, cpu.height) != size {
                cpu = resize(&cpu, size.0, size.1);
            }
            return Ok(Some(VideoFrame::Cpu(cpu)));
        }
        frame.retime(pts, time_base);
        match scaler {
            Some(s) => Ok(Some(VideoFrame::Gpu(s.scale(&frame)?))),
            None if frame.size() == size => Ok(Some(VideoFrame::Gpu(frame))),
            None => {
                let cpu = frame.download()?;
                Ok(Some(VideoFrame::Cpu(resize(&cpu, size.0, size.1))))
            }
        }
    }
}

impl Iterator for GpuVideo {
    type Item = Result<VideoFrame>;

    fn next(&mut self) -> Option<Result<VideoFrame>> {
        self.read().transpose()
    }
}
