use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::gpu::{GpuFrame, GpuScaler};
use crate::mp4::Reader;

use super::ffmpeg::{Child, Ffmpeg};
use super::demux::Demux;
use super::native;
use super::picture::{Color, resize};
use super::probe::{Probe, probe};
use super::route::{Caps, Choice, Prefs, pick_decode};
use super::types::{Error, Frame, PixelFormat, Result, Route, TimeBase};

#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    pub probe: Probe,
    pub route: Route,
    pub reasons: Vec<String>,
    pub ffmpeg: Option<Ffmpeg>,
    pub ffmpeg_codec: Option<String>,
    pub reader: Option<Arc<Reader>>,
    pub demux: Option<Demux>,
}

impl std::fmt::Debug for Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Video({} {}x{})", self.route, self.width, self.height)
    }
}

pub fn source(path: &Path, prefs: &Prefs) -> Result<Source> {
    let probe = probe(path, prefs)?;
    let caps = Caps::detect(prefs);
    let demux = Demux::open(path).ok();
    let reader = demux.as_ref().and_then(Demux::mp4).cloned();
    let (mut route, mut reasons, mut ffmpeg_codec) = match probe.video_codec {
        Some(codec) => {
            let p = pick_decode(codec, prefs, &caps)?;
            (p.route, p.reasons, p.ffmpeg_codec)
        }
        None => match (&prefs.decode, &caps.ffmpeg) {
            (Choice::Ffmpeg(u), Some(_)) => (Route::Ffmpeg, Vec::new(), u.codec.clone()),
            _ => {
                return Err(Error::Unsupported(format!(
                    "{} video decodes only through ffmpeg picked in the settings",
                    probe.video_codec_name
                )));
            }
        },
    };
    if route != Route::Ffmpeg && demux.is_none() {
        match (&caps.ffmpeg, &prefs.decode) {
            (Some(_), Choice::Ffmpeg(u)) => {
                reasons.push("our MP4 and MKV readers can't open this file".to_string());
                route = Route::Ffmpeg;
                ffmpeg_codec = u.codec.clone();
            }
            _ => {
                return Err(Error::Unsupported(
                    "our MP4 and MKV readers can't open this file; pick ffmpeg in the settings to read it"
                        .to_string(),
                ));
            }
        }
    }
    Ok(Source {
        path: path.to_path_buf(),
        probe,
        route,
        reasons,
        ffmpeg: caps.ffmpeg,
        ffmpeg_codec,
        reader,
        demux,
    })
}

impl Source {
    pub fn ffmpeg(&self) -> Result<&Ffmpeg> {
        self.ffmpeg
            .as_ref()
            .ok_or_else(|| Error::Unsupported("ffmpeg is not picked in the settings".to_string()))
    }

    pub fn native(&self) -> Error {
        Error::Unsupported(format!(
            "the {} route for {} is not in this build yet",
            self.route, self.probe.video_codec_name
        ))
    }

    pub fn demux(&self) -> Result<&Demux> {
        self.demux.as_ref().ok_or_else(|| {
            Error::Unsupported("our MP4 and MKV readers can't open this file".to_string())
        })
    }

    pub fn reader(&self) -> Result<&Arc<Reader>> {
        self.reader
            .as_ref()
            .ok_or_else(|| Error::Unsupported("our MP4 reader can't open this file".to_string()))
    }
}

enum Inner {
    Ffmpeg(Child),
    Native(Box<native::Frames>, (u32, u32)),
}

pub struct Video {
    inner: Inner,
    pub route: Route,
    pub reasons: Vec<String>,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub time_base: TimeBase,
    next: i64,
    left: Option<u64>,
    got: u64,
}

pub fn decode_video(path: &Path, from: f64, height: Option<u32>, prefs: &Prefs) -> Result<Video> {
    let src = source(path, prefs)?;
    let p = &src.probe;
    let size = height
        .filter(|h| *h > 0 && p.height > 0 && *h != p.height)
        .map(|h| {
            (
                ((p.width as u64 * h as u64 / p.height as u64) as u32).max(2) & !1,
                h,
            )
        });
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
    match src.route {
        Route::Cpu | Route::Gpu => {
            let decoded =
                native::Frames::open(src.demux()?.clone(), from, native::threads(), src.route)?;
            let mut reasons = src.reasons.clone();
            reasons.extend(decoded.reason.clone());
            let format = if p.bit_depth <= 8 {
                PixelFormat::Yuv420p8
            } else {
                PixelFormat::Yuv420p10
            };
            return Ok(native_video(
                decoded,
                reasons,
                size.unwrap_or((p.width, p.height)),
                fps.unwrap_or(p.fps),
                format,
                frames,
            ));
        }
        Route::Ffmpeg => {}
    }
    let f = src.ffmpeg()?;
    let format = if p.bit_depth > 8 {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    };
    let (width, out_height) = size.unwrap_or((p.width, p.height));
    if width == 0 || out_height == 0 {
        return Err(Error::Invalid(format!(
            "a picture of {width}x{out_height}"
        )));
    }
    let fps = fps.unwrap_or(p.fps);
    let mut args: Vec<String> = f.decoder_args(src.ffmpeg_codec.as_deref());
    args.extend([
        "-noautorotate".into(),
        "-ss".into(),
        format!("{:.6}", from.max(0.0)),
        "-i".into(),
        src.path.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-fps_mode".into(),
        "cfr".into(),
        "-r".into(),
        format!("{}/{}", fps.0, fps.1),
    ]);
    if let Some(n) = frames {
        args.extend(["-frames:v".into(), n.to_string()]);
    }
    if (width, out_height) != (p.width, p.height) {
        args.extend([
            "-vf".into(),
            format!("scale={width}:{out_height}:flags=area"),
        ]);
    }
    args.extend([
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        match format {
            PixelFormat::Yuv420p8 => "yuv420p",
            PixelFormat::Yuv420p10 => "yuv420p10le",
        }
        .into(),
        "-".into(),
    ]);
    let child = f.spawn(&args, false, true)?;
    Ok(Video {
        inner: Inner::Ffmpeg(child),
        route: Route::Ffmpeg,
        reasons: src.reasons.clone(),
        width,
        height: out_height,
        format,
        time_base: TimeBase::new(fps.1, fps.0),
        next: (from.max(0.0) * fps.0 as f64 / fps.1.max(1) as f64).round() as i64,
        left: frames,
        got: 0,
    })
}

impl Video {
    pub fn recycle(&mut self, frame: Frame) {
        if let Inner::Native(frames, _) = &mut self.inner {
            frames.recycle(frame);
        }
    }

    fn read(&mut self) -> Result<Option<Frame>> {
        if self.left == Some(0) {
            return Ok(None);
        }
        let child = match &mut self.inner {
            Inner::Ffmpeg(child) => child,
            Inner::Native(frames, fps) => {
                let Some(mut frame) = frames.next_frame()? else {
                    return Ok(None);
                };
                let seconds = frames.seconds(frame.pts);
                frame.pts = (seconds * fps.0 as f64 / fps.1.max(1) as f64).round() as i64;
                frame.time_base = self.time_base;
                if (frame.width, frame.height) != (self.width, self.height) {
                    frame = resize(&frame, self.width, self.height);
                }
                self.got += 1;
                if let Some(n) = self.left.as_mut() {
                    *n -= 1;
                }
                return Ok(Some(frame));
            }
        };
        let mut frame = Frame::new(
            self.format,
            self.width,
            self.height,
            self.next,
            self.time_base,
        );
        let Some(out) = child.stdout.as_mut() else {
            return Ok(None);
        };
        for (i, plane) in frame.planes.iter_mut().enumerate() {
            match out.read_exact(&mut plane.data) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && i == 0 => {
                    self.left = Some(0);
                    let err = child.stderr_now();
                    if !err.is_empty() && self.got == 0 {
                        return Err(Error::Backend(format!("ffmpeg: {err}")));
                    }
                    return Ok(None);
                }
                Err(e) => return Err(Error::Io(e)),
            }
        }
        self.next += 1;
        self.got += 1;
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

fn even(v: u32) -> u32 {
    v.max(2) & !1
}

fn stream_color(src: &Source) -> Color {
    let found = src
        .demux
        .as_ref()
        .and_then(|d| d.stream(d.video().ok()?).ok()?.color);
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
    open_gpu(&src, from, size, None)
}

pub fn open_gpu(
    src: &Source,
    from: f64,
    size: Option<(u32, u32)>,
    fps: Option<(u32, u32)>,
) -> Result<GpuVideo> {
    let p = &src.probe;
    let size = size
        .filter(|&(w, h)| w > 0 && h > 0)
        .map(|(w, h)| (even(w), even(h)))
        .unwrap_or((p.width, p.height));
    let fps = fps.unwrap_or(p.fps);
    let color = stream_color(src);
    let format = if p.bit_depth <= 8 {
        PixelFormat::Yuv420p8
    } else {
        PixelFormat::Yuv420p10
    };
    if src.route == Route::Gpu {
        let frames = native::Frames::open(src.demux()?.clone(), from, native::threads(), Route::Gpu)?;
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
                    reasons.push(format!(
                        "no GPU scaler ({e}); frames are scaled on the CPU"
                    ));
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
    let video = open(src, from, None, Some(size), Some(fps))?;
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
        inner: Inner::Native(Box::new(frames), fps),
        reasons,
        width: size.0,
        height: size.1,
        format,
        time_base: TimeBase::new(fps.1, fps.0),
        next: 0,
        left,
        got: 0,
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
        let seconds = frames.seconds(frame.pts());
        let pts = (seconds * fps.0 as f64 / fps.1.max(1) as f64).round() as i64;
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
