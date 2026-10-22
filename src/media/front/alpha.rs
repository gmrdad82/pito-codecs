use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;

use super::demux::{Demux, mkv_head};
use super::export::{ExportOptions, Exported, MAX_FPS, Progress, Stage, WRITTEN};
use super::frames::Canvas;
use super::native::{rational, threads};
use super::picture::{Color, rotate, scale_to_height, to_rgba_color};
use super::route::Picked;
use super::still::Seek;
use super::video::frame_at;
use crate::filter::{ColorSpec, Matrix, Range, rgba_to_yuv};
use crate::media::{
    Codec, Error, Frame, Packet, PixelFormat, Plane, Result, Rgba, Route, TimeBase,
};
use crate::mkv;
use crate::vp9;

const GOP: u32 = 240;

enum Enc {
    Plain(Box<vp9::Encoder>),
    Alpha(Box<vp9::AlphaEncoder>),
}

impl Enc {
    fn encode(&mut self, f: &Frame, a: Option<&Plane>) -> Result<Vec<(Packet, Option<Vec<u8>>)>> {
        Ok(match (self, a) {
            (Enc::Alpha(e), Some(a)) => e
                .encode(f, a)?
                .into_iter()
                .map(|(p, a)| (p, Some(a)))
                .collect(),
            (Enc::Plain(e), _) => e.encode(f)?.into_iter().map(|p| (p, None)).collect(),
            (Enc::Alpha(_), None) => {
                return Err(Error::Invalid("a frame without its alpha plane".into()));
            }
        })
    }

    fn flush(&mut self) -> Result<Vec<(Packet, Option<Vec<u8>>)>> {
        Ok(match self {
            Enc::Alpha(e) => e.flush()?.into_iter().map(|(p, a)| (p, Some(a))).collect(),
            Enc::Plain(e) => e.flush()?.into_iter().map(|p| (p, None)).collect(),
        })
    }
}

fn webm_of(out: &Path) -> Result<bool> {
    match out
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("webm") => Ok(true),
        Some("mkv") => Ok(false),
        _ => Err(Error::Unsupported(format!(
            "{}: RGBA frames export to .webm or .mkv",
            out.display()
        ))),
    }
}

fn alpha_into(img: &Rgba, plane: &mut Plane) {
    let (w, h) = (img.width as usize, img.height as usize);
    plane.width = w;
    plane.height = h;
    plane.stride = w;
    plane.data.resize(w * h, 0);
    for (d, px) in plane.data.iter_mut().zip(img.data.chunks_exact(4)) {
        *d = px[3];
    }
}

pub fn export_rgba(
    out: &Path,
    canvas: Canvas,
    alpha: bool,
    opts: ExportOptions,
    mut frames: impl FnMut(u64, Option<Rgba>) -> Result<Option<Rgba>>,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    let size = (canvas.width, canvas.height);
    let fps = canvas.fps;
    if size.0 == 0 || size.1 == 0 || size.0 > 16384 || size.1 > 16384 {
        return Err(Error::Invalid(format!(
            "a picture of {}x{}",
            size.0, size.1
        )));
    }
    if fps.0 == 0 || fps.1 == 0 || fps.0 / fps.1 > MAX_FPS {
        return Err(Error::Invalid(format!(
            "a frame rate of {}/{}",
            fps.0, fps.1
        )));
    }
    if opts.video != Codec::Vp9 {
        return Err(Error::Unsupported(format!(
            "RGBA frames export to VP9, not {}",
            opts.video
        )));
    }
    let webm = webm_of(out)?;
    let mut reasons = Vec::new();
    if opts.overlay.is_some() {
        reasons.push("the overlay is not drawn on RGBA frames".into());
    }
    let cfg = vp9::EncoderConfig {
        width: size.0,
        height: size.1,
        fps,
        rate: vp9::Rate::Crf((opts.crf as u32 * 7 / 4).min(63) as u8),
        gop: GOP,
        threads: 0,
        aq: false,
    };
    let mut enc = if alpha {
        let mut e = vp9::AlphaEncoder::new(cfg)?;
        e.set_color(2, false)?;
        Enc::Alpha(Box::new(e))
    } else {
        let mut e = vp9::Encoder::new(cfg)?;
        e.set_color(2, false)?;
        Enc::Plain(Box::new(e))
    };
    let time_base = TimeBase::new(fps.1, fps.0);
    let mut track = mkv::TrackConfig::vp9(size.0, size.1, time_base);
    track.alpha = alpha;
    track.default_duration =
        Some(((fps.1 as u128 * 1_000_000_000 + fps.0 as u128 / 2) / fps.0 as u128) as u64);
    let mut writer = mkv::Writer::create_with(
        out,
        mkv::Options {
            webm,
            ..mkv::Options::default()
        },
    )?;
    let made = (|| -> Result<u64> {
        let v = writer.add_track(track)?;
        let spec = ColorSpec::new(Matrix::Bt709, Range::Limited);
        let (tx, rx) = mpsc::sync_channel::<(Frame, Option<Plane>)>(2);
        let (back, spare) = mpsc::channel::<(Frame, Option<Plane>)>();
        std::thread::scope(|s| -> Result<u64> {
            let writer = &mut writer;
            let enc = &mut enc;
            let worker = s.spawn(move || -> Result<()> {
                for (f, a) in rx {
                    for (p, add) in enc.encode(&f, a.as_ref())? {
                        writer.push_alpha(v, &p, add.as_deref())?;
                    }
                    let _ = back.send((f, a));
                }
                for (p, add) in enc.flush()? {
                    writer.push_alpha(v, &p, add.as_deref())?;
                }
                Ok(())
            });
            let mut feed = || -> Result<u64> {
                let mut spent: Option<Rgba> = None;
                let mut n = 0u64;
                let tell = |n: u64, progress: &mut dyn FnMut(&Progress) -> bool| {
                    let done = match canvas.frames {
                        Some(t) if t > 0 => (n as f32 / t as f32).min(1.0) * WRITTEN,
                        _ => 0.0,
                    };
                    let keep = progress(&Progress {
                        stage: Stage::Encoding,
                        done,
                        encoder: Route::Cpu,
                    });
                    if keep { Ok(()) } else { Err(Error::Cancelled) }
                };
                tell(0, &mut progress)?;
                while canvas.frames.is_none_or(|t| n < t)
                    && let Some(img) = frames(n, spent.take())?
                {
                    if (img.width, img.height) != size
                        || img.data.len() < size.0 as usize * size.1 as usize * 4
                    {
                        return Err(Error::Invalid(format!(
                            "frame {n} is {}x{}, the export is {}x{}",
                            img.width, img.height, size.0, size.1
                        )));
                    }
                    let pts = i64::try_from(n).unwrap_or(i64::MAX);
                    let (mut f, mut a) = match spare.try_recv() {
                        Ok(old) => old,
                        Err(_) => (
                            Frame::new(PixelFormat::Yuv420p8, size.0, size.1, pts, time_base),
                            None,
                        ),
                    };
                    f.pts = pts;
                    f.time_base = time_base;
                    rgba_to_yuv(&img, spec, PixelFormat::Yuv420p8, &mut f)
                        .map_err(|e| Error::Invalid(format!("frame {n}: {e}")))?;
                    if alpha {
                        let mut plane = a.take().unwrap_or(Plane {
                            data: Vec::new(),
                            stride: 0,
                            width: 0,
                            height: 0,
                        });
                        alpha_into(&img, &mut plane);
                        a = Some(plane);
                    }
                    if tx.send((f, a)).is_err() {
                        return Err(Error::Backend("the encoder stopped".into()));
                    }
                    spent = Some(img);
                    n += 1;
                    if n.is_multiple_of(15) {
                        tell(n, &mut progress)?;
                    }
                }
                if n == 0 {
                    return Err(Error::Invalid("no frames to export".to_string()));
                }
                Ok(n)
            };
            let fed = feed();
            drop(tx);
            let done = worker
                .join()
                .unwrap_or_else(|_| Err(Error::Backend("the encoder failed".into())));
            match (fed, done) {
                (Ok(n), Ok(())) => Ok(n),
                (Err(Error::Backend(m)), Err(e)) if m == "the encoder stopped" => Err(e),
                (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            }
        })
    })();
    let count = match made.and_then(|n| writer.finish().map(|_| n)) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(out);
            return Err(e);
        }
    };
    if !progress(&Progress {
        stage: Stage::Joining,
        done: 1.0,
        encoder: Route::Cpu,
    }) {
        return Err(Error::Cancelled);
    }
    Ok(Exported {
        path: out.to_path_buf(),
        frames: count,
        copied: 0,
        encoded: count,
        smart: false,
        video: Picked {
            route: Route::Cpu,
            codec: Codec::Vp9,
            reasons: Vec::new(),
        },
        reasons,
        sessions: Default::default(),
    })
}

fn color_of(cs: u8, full_range: bool) -> Color {
    let matrix = match cs {
        1 | 3 => 6,
        2 => 1,
        4 => 7,
        5 => 9,
        _ => 0,
    };
    Color { matrix, full_range }
}

pub fn to_rgba_alpha(frame: &Frame, alpha: Option<&Plane>, height: u32, color: Color) -> Rgba {
    let mut img = to_rgba_color(frame, 0, color);
    if let Some(a) = alpha {
        let (w, h) = (img.width as usize, img.height as usize);
        if a.width >= w && a.height >= h && a.stride >= w && a.data.len() >= a.stride * h {
            for y in 0..h {
                let src = &a.data[y * a.stride..y * a.stride + w];
                let row = &mut img.data[y * w * 4..(y + 1) * w * 4];
                for (px, &v) in row.chunks_exact_mut(4).zip(src) {
                    px[3] = v;
                }
            }
        }
    }
    if height == 0 || height >= img.height {
        img
    } else {
        scale_to_height(&img, height)
    }
}

pub struct AlphaVideo {
    head: Arc<mkv::Reader>,
    track: usize,
    next: usize,
    from: i64,
    dec: vp9::AlphaDecoder,
    ready: VecDeque<(Frame, Option<Plane>)>,
    flushed: bool,
    data: Vec<u8>,
    extra: Vec<u8>,
    start: f64,
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub time_base: TimeBase,
    pub rotation: u16,
    pub alpha: bool,
    pub color: Color,
}

pub fn decode_alpha(path: &Path, from: f64) -> Result<AlphaVideo> {
    let head = mkv_head(path)?.ok_or_else(|| {
        Error::Unsupported(format!(
            "{}: VP9 with alpha is read from WebM or Matroska",
            path.display()
        ))
    })?;
    AlphaVideo::open(head, from, true)
}

impl AlphaVideo {
    fn open(head: Arc<mkv::Reader>, from: f64, exact: bool) -> Result<AlphaVideo> {
        let start = Demux::Mkv(head.clone()).start();
        let r: &mkv::Reader<File> = head.full()?;
        let track = r
            .video()
            .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
        let t = r
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
        if t.codec() != Some(Codec::Vp9) {
            return Err(Error::Unsupported(format!(
                "alpha in {}; it is read from VP9",
                t.codec_name()
            )));
        }
        let at = t.ticks(from.max(0.0) + start);
        let last = t.samples.partition_point(|s| s.pts <= at);
        let first = t.keyframe_before(last.saturating_sub(1));
        let (width, height) = t
            .video
            .as_ref()
            .map(|i| (i.pixel_width, i.pixel_height))
            .unwrap_or((t.width, t.height));
        let fps = rational(t.fps());
        let mut data = Vec::new();
        let color = match t.samples.get(first) {
            Some(_) => {
                r.read_into(track, first, &mut data)?;
                vp9::keyframe_info(&data)
                    .map(|k| color_of(k.color.color_space, k.color.full_range))
                    .unwrap_or_default()
            }
            None => Color::default(),
        };
        let mut dec = vp9::AlphaDecoder::with_threads(threads());
        dec.set_time_base(t.time_base);
        let from_pts = if exact {
            last.checked_sub(1)
                .and_then(|i| t.samples.get(i))
                .map_or(at, |s| s.pts)
        } else {
            t.samples.get(first).map_or(at, |s| s.pts)
        };
        Ok(AlphaVideo {
            track,
            next: first,
            from: from_pts,
            dec,
            ready: VecDeque::new(),
            flushed: false,
            data,
            extra: Vec::new(),
            start,
            width,
            height,
            fps,
            time_base: TimeBase::new(fps.1, fps.0),
            rotation: t.rotation(),
            alpha: t.alpha(),
            color,
            head,
        })
    }

    fn reader(&self) -> Result<&mkv::Reader<File>> {
        self.head.full()
    }

    pub fn next_frame(&mut self) -> Result<Option<(Frame, Option<Plane>)>> {
        loop {
            while let Some((f, a)) = self.ready.pop_front() {
                if f.pts < self.from {
                    continue;
                }
                let r = self.reader()?;
                let t = r
                    .tracks
                    .get(self.track)
                    .ok_or_else(|| Error::Invalid("no video track".to_string()))?;
                let mut f = f;
                f.pts = frame_at(t.seconds(f.pts) - self.start, self.fps);
                f.time_base = self.time_base;
                return Ok(Some((f, a)));
            }
            if self.flushed {
                return Ok(None);
            }
            let head = self.head.clone();
            let r = head.full()?;
            let Some(s) = r
                .tracks
                .get(self.track)
                .and_then(|t| t.samples.get(self.next))
            else {
                self.flushed = true;
                let out = self.dec.flush()?;
                self.ready.extend(out);
                continue;
            };
            let s = *s;
            r.read_into(self.track, self.next, &mut self.data)?;
            let has = self.alpha && r.read_alpha(self.track, self.next, &mut self.extra)?;
            self.next += 1;
            let packet = Packet {
                data: std::mem::take(&mut self.data),
                pts: s.pts,
                dts: s.pts,
                keyframe: s.keyframe,
            };
            let out = self.dec.decode(&packet, has.then_some(&self.extra[..]));
            self.data = packet.data;
            self.ready.extend(out?);
        }
    }

    pub fn next_rgba(&mut self, height: u32) -> Result<Option<Rgba>> {
        let Some((f, a)) = self.next_frame()? else {
            return Ok(None);
        };
        let pre = if self.rotation % 180 == 90 && f.height > 0 && height > 0 {
            super::picture::fit_width(f.height, f.width.max(1), height)
        } else {
            height
        };
        let img = to_rgba_alpha(&f, a.as_ref(), pre, self.color);
        Ok(Some(if self.rotation.is_multiple_of(360) {
            img
        } else {
            rotate(&img, self.rotation)
        }))
    }
}

impl Iterator for AlphaVideo {
    type Item = Result<(Frame, Option<Plane>)>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_frame().transpose()
    }
}

pub(super) fn has_alpha(head: &mkv::Reader) -> bool {
    head.video()
        .and_then(|v| head.tracks.get(v))
        .is_some_and(|t| t.alpha() && t.codec() == Some(Codec::Vp9))
}

pub(super) fn stills(
    head: &Arc<mkv::Reader>,
    height: u32,
    times: &[f64],
    seek: Seek,
    emit: &(dyn Fn(usize, Result<Rgba>) + Sync),
) -> Result<()> {
    for (i, &at) in times.iter().enumerate() {
        let got = AlphaVideo::open(head.clone(), at, seek == Seek::Exact).and_then(|mut v| {
            v.next_rgba(height)?
                .ok_or_else(|| Error::Invalid(format!("no picture at {at:.3} s")))
        });
        emit(i, got);
    }
    Ok(())
}
