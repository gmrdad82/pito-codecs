use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::mp4::Reader;

use super::ffmpeg::{Child, Ffmpeg};
use super::native;
use super::picture::resize;
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
}

impl std::fmt::Debug for Video {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Video({} {}x{})", self.route, self.width, self.height)
    }
}

pub fn source(path: &Path, prefs: &Prefs) -> Result<Source> {
    let probe = probe(path, prefs)?;
    let caps = Caps::detect(prefs);
    let reader = native::reader(path).ok();
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
    if route != Route::Ffmpeg && reader.is_none() {
        match (&caps.ffmpeg, &prefs.decode) {
            (Some(_), Choice::Ffmpeg(u)) => {
                reasons.push("our MP4 reader can't open this file".to_string());
                route = Route::Ffmpeg;
                ffmpeg_codec = u.codec.clone();
            }
            _ => {
                return Err(Error::Unsupported(
                    "our MP4 reader can't open this file; pick ffmpeg in the settings to read it"
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
                native::Frames::open(src.reader()?.clone(), from, native::threads(), src.route)?;
            let mut reasons = src.reasons.clone();
            reasons.extend(decoded.reason.clone());
            let route = decoded.route();
            let (width, height) = size.unwrap_or((p.width, p.height));
            let fps = fps.unwrap_or(p.fps);
            let format = if p.bit_depth <= 8 {
                PixelFormat::Yuv420p8
            } else {
                PixelFormat::Yuv420p10
            };
            return Ok(Video {
                inner: Inner::Native(Box::new(decoded), fps),
                route,
                reasons,
                width,
                height,
                format,
                time_base: TimeBase::new(fps.1, fps.0),
                next: 0,
                left: frames,
                got: 0,
            });
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
                let t = frames.track();
                let seconds = t.seconds(frame.pts);
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
