use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ffmpeg::{Child, Ffmpeg};
use super::picture::Blender;
use super::probe::keyframes;
use super::route::{Caps, Picked, Prefs, pick_encode};
use super::types::{Codec, Error, Frame, PixelFormat, Result, Rgba, Route};
use super::video::{Source, open, source};

#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub path: PathBuf,
    pub from: f64,
    pub to: f64,
}

const MAX_SECONDS: f64 = 1e7;
const MAX_FPS: u32 = 1000;

pub type Overlay = Box<dyn FnMut(f64, u32, u32) -> Option<Arc<Rgba>> + Send>;

pub struct ExportOptions {
    pub video: Codec,
    pub crf: u8,
    pub audio_kbps: u32,
    pub audio_gains: Vec<f32>,
    pub overlay: Option<Overlay>,
    pub prefs: Prefs,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            video: Codec::H264,
            crf: 18,
            audio_kbps: 192,
            audio_gains: Vec::new(),
            overlay: None,
            prefs: Prefs::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    Cutting { piece: usize, pieces: usize },
    Encoding,
    Mixing,
    Joining,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    pub done: f32,
    pub encoder: Route,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Exported {
    pub path: PathBuf,
    pub frames: u64,
    pub copied: u64,
    pub encoded: u64,
    pub smart: bool,
    pub video: Picked,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Copy { from: i64, frames: i64 },
    Encode { from: i64, frames: i64 },
}

pub fn parts(in_f: i64, out_f: i64, keys: &[i64]) -> Vec<Part> {
    if out_f <= in_f {
        return Vec::new();
    }
    let Some(a) = keys.iter().copied().find(|k| *k >= in_f && *k < out_f) else {
        return vec![Part::Encode {
            from: in_f,
            frames: out_f - in_f,
        }];
    };
    let b = keys
        .iter()
        .copied()
        .rev()
        .find(|k| *k <= out_f)
        .unwrap_or(a)
        .max(a);
    let mut out = Vec::new();
    if a > in_f {
        out.push(Part::Encode {
            from: in_f,
            frames: a - in_f,
        });
    }
    if b > a {
        out.push(Part::Copy {
            from: a,
            frames: b - a,
        });
    }
    if out_f > b {
        out.push(Part::Encode {
            from: b,
            frames: out_f - b,
        });
    }
    out
}

pub(super) struct Job<'a> {
    pub(super) pieces: &'a [Piece],
    pub(super) sources: Vec<Source>,
    pub(super) index: Vec<usize>,
    pub(super) fps: (u32, u32),
    pub(super) size: (u32, u32),
    pub(super) format: PixelFormat,
    pub(super) total: u64,
    pub(super) done: u64,
    pub(super) work: PathBuf,
    pub(super) route: Route,
}

pub(super) type Tell<'a> = &'a mut dyn FnMut(&Progress) -> bool;

impl Job<'_> {
    pub(super) fn frame_of(&self, seconds: f64) -> i64 {
        (seconds * self.fps.0 as f64 / self.fps.1 as f64).round() as i64
    }

    pub(super) fn seconds(&self, frames: i64) -> f64 {
        frames as f64 * self.fps.1 as f64 / self.fps.0 as f64
    }

    pub(super) fn tell(&self, progress: Tell, stage: Stage) -> Result<()> {
        let done = if self.total == 0 {
            0.0
        } else {
            self.done as f32 / self.total as f32 * 0.9
        };
        let done = match stage {
            Stage::Mixing => 0.9,
            Stage::Joining => 0.95,
            _ => done,
        };
        if progress(&Progress {
            stage,
            done,
            encoder: self.route,
        }) {
            Ok(())
        } else {
            Err(Error::Cancelled)
        }
    }
}

pub fn export(
    out: &Path,
    pieces: &[Piece],
    mut opts: ExportOptions,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    if pieces.is_empty() {
        return Err(Error::Invalid("nothing to export".to_string()));
    }
    let mut sources: Vec<Source> = Vec::new();
    let mut by_path: HashMap<PathBuf, usize> = HashMap::new();
    let mut index = Vec::new();
    for p in pieces {
        if p.to.partial_cmp(&p.from) != Some(std::cmp::Ordering::Greater)
            || !(-MAX_SECONDS..=MAX_SECONDS).contains(&p.from)
            || !(-MAX_SECONDS..=MAX_SECONDS).contains(&p.to)
        {
            return Err(Error::Invalid(format!(
                "a piece from {} to {}",
                p.from, p.to
            )));
        }
        let i = match by_path.get(&p.path) {
            Some(&i) => i,
            None => {
                sources.push(source(&p.path, &opts.prefs)?);
                by_path.insert(p.path.clone(), sources.len() - 1);
                sources.len() - 1
            }
        };
        index.push(i);
    }
    let caps = Caps::detect(&opts.prefs);
    let mut video = pick_encode(opts.video, &opts.prefs, &caps)?;
    let first = &sources[index[0]].probe;
    let fps = first.fps;
    if fps.0 == 0 || fps.1 == 0 || fps.0 / fps.1 > MAX_FPS {
        return Err(Error::Invalid(format!("a frame rate of {}/{}", fps.0, fps.1)));
    }
    let size = (first.width & !1, first.height & !1);
    if size.0 == 0 || size.1 == 0 {
        return Err(Error::Invalid(format!(
            "a picture of {}x{}",
            first.width, first.height
        )));
    }
    let format = if first.bit_depth > 8 && video.codec == Codec::Hevc {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    };
    let format = match video.route {
        Route::Gpu => match super::mux::gpu_check(video.codec, size, fps, format) {
            Ok(()) => format,
            Err(e) => {
                video.reasons.push(format!(
                    "the GPU encoder failed ({e}); encoding H.264 on the CPU instead"
                ));
                video.route = Route::Cpu;
                video.codec = Codec::H264;
                PixelFormat::Yuv420p8
            }
        },
        Route::Cpu => PixelFormat::Yuv420p8,
        Route::Ffmpeg => format,
    };
    let mut reasons: Vec<String> = video.reasons.clone();
    for s in &sources {
        for r in &s.reasons {
            if !reasons.contains(r) {
                reasons.push(r.clone());
            }
        }
    }
    let same = sources.iter().all(|s| {
        s.probe.video_codec == Some(video.codec)
            && s.probe.fps == fps
            && (s.probe.width, s.probe.height) == (first.width, first.height)
            && s.probe.bit_depth == first.bit_depth
            && s.probe.rotation == first.rotation
    });
    let smart = opts.overlay.is_none() && same && size == (first.width, first.height);
    let ffmpeg = match (&caps.ffmpeg, video.route) {
        (Some(f), Route::Ffmpeg) => Some(f.clone()),
        (_, Route::Cpu | Route::Gpu) => None,
        (None, Route::Ffmpeg) => {
            return Err(Error::Unsupported(
                "ffmpeg was picked but not found".to_string(),
            ));
        }
    };
    let smart = smart && (ffmpeg.is_some() || sources.iter().all(|s| s.reader.is_some()));
    let work = std::path::absolute(out.with_extension("parts"))?;
    if work.exists() {
        std::fs::remove_dir_all(&work)?;
    }
    std::fs::create_dir_all(&work)?;
    let mut job = Job {
        pieces,
        sources,
        index,
        fps,
        size,
        format,
        total: 0,
        done: 0,
        work: work.clone(),
        route: video.route,
    };
    job.total = pieces
        .iter()
        .map(|p| (job.frame_of(p.to) - job.frame_of(p.from)).max(0) as u64)
        .sum();
    let result = match &ffmpeg {
        Some(f) => run(&mut job, f, &video, &mut opts, smart, out, &mut progress),
        None => super::mux::run(
            &mut job,
            &video,
            &mut opts,
            smart,
            out,
            &mut progress,
            caps.ffmpeg.as_ref(),
        ),
    };
    let _ = std::fs::remove_dir_all(&work);
    let (copied, encoded) = result?;
    Ok(Exported {
        path: out.to_path_buf(),
        frames: copied + encoded,
        copied,
        encoded,
        smart,
        video,
        reasons,
    })
}

fn run(
    job: &mut Job,
    f: &Ffmpeg,
    video: &Picked,
    opts: &mut ExportOptions,
    smart: bool,
    out: &Path,
    progress: Tell,
) -> Result<(u64, u64)> {
    let (mut copied, mut encoded) = (0u64, 0u64);
    let mut list = String::new();
    if smart {
        let mut keys: HashMap<usize, Vec<i64>> = HashMap::new();
        let mut plan: Vec<(usize, Part)> = Vec::new();
        for (n, piece) in job.pieces.iter().enumerate() {
            let si = job.index[n];
            if let std::collections::hash_map::Entry::Vacant(slot) = keys.entry(si) {
                slot.insert(
                    keyframes(&job.sources[si].path, &opts.prefs)?
                        .into_iter()
                        .map(|t| job.frame_of(t))
                        .collect(),
                );
            }
            let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
            for part in parts(a, b, &keys[&si]) {
                let file = match plan.iter().position(|q| *q == (si, part)) {
                    Some(i) => job.work.join(format!("part-{i:05}.ts")),
                    None => {
                        plan.push((si, part));
                        job.work.join(format!("part-{:05}.ts", plan.len() - 1))
                    }
                };
                list.push_str(&format!("file '{}'\n", file.display()));
                match part {
                    Part::Copy { frames, .. } => copied += frames as u64,
                    Part::Encode { frames, .. } => encoded += frames as u64,
                }
            }
        }
        let tail = edge_tail(f, job, video, opts.crf)?;
        let mix = job.work.join("mix.m4a");
        let mix_args = mix_args(job, opts, &mix);
        let total: i64 = plan
            .iter()
            .map(|(_, p)| match p {
                Part::Copy { frames, .. } | Part::Encode { frames, .. } => *frames,
            })
            .sum::<i64>()
            .max(1);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel::<Result<i64>>();
        let workers = plan.len().clamp(1, 6);
        let jobref = &*job;
        let outcome = std::thread::scope(|s| {
            {
                let tx = tx.clone();
                let mix_args = &mix_args;
                s.spawn(move || {
                    let _ = tx.send(f.run_args(mix_args).map(|_| 0));
                });
            }
            for _ in 0..workers {
                let tx = tx.clone();
                let (plan, next, stop, tail) = (&plan, &next, &stop, &tail);
                s.spawn(move || {
                    loop {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(&(si, part)) = plan.get(i) else {
                            break;
                        };
                        let file = jobref.work.join(format!("part-{i:05}.ts"));
                        let r = match part {
                            Part::Copy { from, frames } => {
                                copy(f, jobref, si, from, frames, &file).map(|_| frames)
                            }
                            Part::Encode { from, frames } => {
                                edge(f, jobref, si, from, frames, tail, &file).map(|_| frames)
                            }
                        };
                        let _ = tx.send(r);
                    }
                });
            }
            drop(tx);
            let mut done = 0i64;
            let mut first_err = None;
            for (k, r) in rx.iter().enumerate() {
                match r {
                    Ok(frames) => done += frames,
                    Err(e) => {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        first_err.get_or_insert(e);
                        continue;
                    }
                }
                let keep = progress(&Progress {
                    stage: Stage::Cutting {
                        piece: k + 1,
                        pieces: plan.len() + 1,
                    },
                    done: done as f32 / total as f32 * 0.9,
                    encoder: Route::Ffmpeg,
                });
                if !keep {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    first_err.get_or_insert(Error::Cancelled);
                }
            }
            match first_err {
                Some(e) => Err(e),
                None => Ok(()),
            }
        });
        outcome?;
        return join(job, f, video, &list, &mix, out, progress).map(|_| (copied, encoded));
    } else {
        job.tell(progress, Stage::Encoding)?;
        let file = job.work.join("whole.ts");
        let mut enc = Encoder::open(f, job, video, opts.crf, false, &file)?;
        let mut at = 0i64;
        for (n, piece) in job.pieces.iter().enumerate() {
            let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
            reencode(
                job,
                job.index[n],
                (a, b - a),
                at,
                &mut |f| enc.push(f),
                &mut opts.overlay,
                progress,
            )?;
            at += b - a;
        }
        enc.finish()?;
        encoded = at as u64;
        list.push_str(&format!("file '{}'\n", file.display()));
    }
    job.tell(progress, Stage::Mixing)?;
    let mix = job.work.join("mix.m4a");
    f.run_args(&mix_args(job, opts, &mix))?;
    join(job, f, video, &list, &mix, out, progress)?;
    Ok((copied, encoded))
}

fn join(
    job: &Job,
    f: &Ffmpeg,
    video: &Picked,
    list: &str,
    mix: &Path,
    out: &Path,
    progress: Tell,
) -> Result<()> {
    job.tell(progress, Stage::Joining)?;
    let list_path = job.work.join("pieces.txt");
    std::fs::write(&list_path, list)?;
    let part = job.work.join("joined.mp4");
    let mut join: Vec<String> = vec![
        "-f".into(),
        "concat".into(),
        "-safe".into(),
        "0".into(),
        "-i".into(),
        list_path.display().to_string(),
        "-i".into(),
        mix.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "1:a:0".into(),
        "-c".into(),
        "copy".into(),
    ];
    if video.codec == Codec::Hevc {
        join.extend(["-tag:v".into(), "hvc1".into()]);
    }
    let rotation = job.sources[job.index[0]].probe.rotation;
    if rotation != 0 {
        join.extend([
            "-display_rotation:v:0".into(),
            format!("{}", -(rotation as i32)),
        ]);
    }
    join.extend([
        "-movflags".into(),
        "+faststart".into(),
        "-f".into(),
        "mp4".into(),
        part.display().to_string(),
    ]);
    f.run_args(&join)?;
    std::fs::rename(&part, out)?;
    if !progress(&Progress {
        stage: Stage::Joining,
        done: 1.0,
        encoder: Route::Ffmpeg,
    }) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn copy(f: &Ffmpeg, job: &Job, si: usize, from: i64, frames: i64, file: &Path) -> Result<()> {
    let seek = job.seconds(from) + job.seconds(1) / 2.0;
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{seek:.6}"),
        "-i".into(),
        job.sources[si].path.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-frames:v".into(),
        frames.to_string(),
        "-c:v".into(),
        "copy".into(),
        "-f".into(),
        "mpegts".into(),
        file.display().to_string(),
    ];
    f.run_args(&args).map(|_| ())
}

pub(super) fn reencode(
    job: &mut Job,
    si: usize,
    (from, frames): (i64, i64),
    at: i64,
    push: &mut dyn FnMut(&Frame) -> Result<()>,
    overlay: &mut Option<Overlay>,
    progress: Tell,
) -> Result<()> {
    if frames <= 0 {
        return Ok(());
    }
    let src = &job.sources[si];
    let size = (src.probe.width, src.probe.height) != job.size;
    let mut frames_in = open(
        src,
        job.seconds(from),
        Some(frames as u64),
        size.then_some(job.size),
        Some(job.fps),
    )?;
    let mut n = 0i64;
    let mut last: Option<Frame> = None;
    let mut blender = Blender::new();
    while n < frames {
        let mut frame = match frames_in.next() {
            Some(f) => f?,
            None => match last.clone() {
                Some(f) => f,
                None => return Err(Error::Invalid("a piece with no frames".to_string())),
            },
        };
        if frame.format != job.format {
            frame = convert_depth(&frame, job.format);
        }
        frame.pts = at + n;
        let layer = overlay
            .as_mut()
            .and_then(|o| o(job.seconds(at + n), frame.width, frame.height));
        let keep = match layer {
            Some(layer) => {
                let clean = frame.clone();
                blender.draw(&mut frame, &layer);
                push(&frame)?;
                frames_in.recycle(frame);
                clean
            }
            None => {
                push(&frame)?;
                frame
            }
        };
        if let Some(old) = last.replace(keep) {
            frames_in.recycle(old);
        }
        n += 1;
        job.done += 1;
        if n % 15 == 0 {
            job.tell(progress, Stage::Encoding)?;
        }
    }
    Ok(())
}

fn convert_depth(frame: &Frame, format: PixelFormat) -> Frame {
    let mut out = Frame::new(
        format,
        frame.width,
        frame.height,
        frame.pts,
        frame.time_base,
    );
    for (o, i) in out.planes.iter_mut().zip(&frame.planes) {
        for y in 0..i.height {
            for x in 0..i.width {
                let v = match frame.format {
                    PixelFormat::Yuv420p8 => i.data[y * i.stride + x] as u16 * 4,
                    PixelFormat::Yuv420p10 => {
                        let k = y * i.stride + x * 2;
                        u16::from_le_bytes([i.data[k], i.data[k + 1]])
                    }
                };
                match format {
                    PixelFormat::Yuv420p8 => {
                        o.data[y * o.stride + x] = ((v + 2) / 4).min(255) as u8;
                    }
                    PixelFormat::Yuv420p10 => {
                        let k = y * o.stride + x * 2;
                        o.data[k..k + 2].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
    }
    out
}

struct Encoder {
    child: Child,
}

struct Tail {
    pre: Vec<String>,
    post: Vec<String>,
}

fn tail(f: &Ffmpeg, job: &Job, video: &Picked, crf: u8, edge: bool) -> Result<Tail> {
    let name = f
        .encoder_for(video.codec, video.ffmpeg_codec.as_deref())
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "{} has no {} encoder",
                f.path.display(),
                video.codec
            ))
        })?;
    let mut pre: Vec<String> = Vec::new();
    let mut post = encoder_args(&name, video.codec, crf, job, edge);
    if name.ends_with("_vaapi") {
        pre.extend([
            "-init_hw_device".into(),
            "vaapi=va:/dev/dri/renderD128".into(),
            "-filter_hw_device".into(),
            "va".into(),
        ]);
        let up = if job.format == PixelFormat::Yuv420p10 {
            "format=p010,hwupload"
        } else {
            "format=nv12,hwupload"
        };
        post.extend(["-vf".into(), up.into()]);
    }
    if edge {
        post.extend(["-bf".into(), "0".into(), "-g".into(), "100000".into()]);
    }
    Ok(Tail { pre, post })
}

fn edge_tail(f: &Ffmpeg, job: &Job, video: &Picked, crf: u8) -> Result<Tail> {
    tail(f, job, video, crf, true)
}

fn edge(
    f: &Ffmpeg,
    job: &Job,
    si: usize,
    from: i64,
    frames: i64,
    tail: &Tail,
    file: &Path,
) -> Result<()> {
    let src = &job.sources[si];
    let mut args = tail.pre.clone();
    args.extend(f.decoder_args(src.ffmpeg_codec.as_deref()));
    args.extend([
        "-ss".into(),
        format!("{:.6}", (job.seconds(from) - job.seconds(1) / 2.0).max(0.0)),
        "-i".into(),
        src.path.display().to_string(),
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-frames:v".into(),
        frames.to_string(),
    ]);
    args.extend(tail.post.iter().cloned());
    args.extend(["-f".into(), "mpegts".into(), file.display().to_string()]);
    f.run_args(&args).map(|_| ())
}

impl Encoder {
    fn open(
        f: &Ffmpeg,
        job: &Job,
        video: &Picked,
        crf: u8,
        edge: bool,
        file: &Path,
    ) -> Result<Encoder> {
        let t = tail(f, job, video, crf, edge)?;
        let pix = match job.format {
            PixelFormat::Yuv420p8 => "yuv420p",
            PixelFormat::Yuv420p10 => "yuv420p10le",
        };
        let mut args = t.pre;
        args.extend([
            "-f".into(),
            "rawvideo".into(),
            "-pix_fmt".into(),
            pix.into(),
            "-s".into(),
            format!("{}x{}", job.size.0, job.size.1),
            "-r".into(),
            format!("{}/{}", job.fps.0, job.fps.1),
            "-i".into(),
            "-".into(),
        ]);
        args.extend(t.post);
        args.extend(["-f".into(), "mpegts".into(), file.display().to_string()]);
        Ok(Encoder {
            child: f.spawn(&args, true, false)?,
        })
    }

    fn push(&mut self, frame: &Frame) -> Result<()> {
        let Some(input) = self.child.stdin.as_mut() else {
            return Err(Error::Backend("ffmpeg closed its input".to_string()));
        };
        for p in &frame.planes {
            let row = p.width
                * if frame.format == PixelFormat::Yuv420p10 {
                    2
                } else {
                    1
                };
            let ok = if p.stride == row {
                input.write_all(&p.data)
            } else {
                p.data
                    .chunks(p.stride)
                    .take(p.height)
                    .try_for_each(|r| input.write_all(&r[..row.min(r.len())]))
            };
            if ok.is_err() {
                let err = self.child.stderr_now();
                return Err(Error::Backend(format!("ffmpeg: {err}")));
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<()> {
        self.child.finish()
    }
}

fn encoder_args(name: &str, codec: Codec, crf: u8, job: &Job, edge: bool) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let crf_edge = if edge { crf.saturating_sub(2) } else { crf };
    let q = crf_edge.to_string();
    let ten = job.format == PixelFormat::Yuv420p10;
    let mut a = s(&["-c:v", name]);
    match name {
        "libx264" => a.extend(s(&[
            "-preset",
            if edge { "veryfast" } else { "medium" },
            "-crf",
            &q,
            "-pix_fmt",
            "yuv420p",
        ])),
        "libx265" => a.extend(s(&[
            "-preset",
            if edge { "veryfast" } else { "medium" },
            "-crf",
            &q,
            "-pix_fmt",
            if ten { "yuv420p10le" } else { "yuv420p" },
            "-x265-params",
            "log-level=error",
        ])),
        n if n.ends_with("_vaapi") => a.extend(s(&[
            "-rc_mode",
            "CQP",
            "-qp",
            &(crf_edge as u32 + 1).to_string(),
        ])),
        n if n.ends_with("_nvenc") => {
            a.extend(s(&["-preset", "p5", "-rc", "vbr", "-cq", &q, "-b:v", "0"]))
        }
        n if n.ends_with("_qsv") => a.extend(s(&["-global_quality", &q])),
        n if n.ends_with("_videotoolbox") => a.extend(s(&[
            "-q:v",
            &(100u32.saturating_sub(crf_edge as u32 * 2)).to_string(),
        ])),
        _ => {
            let pixels = job.size.0 as f64 * job.size.1 as f64;
            let rate = pixels * job.fps.0 as f64 / job.fps.1 as f64;
            let bits = rate * 0.15 * 2f64.powf((18.0 - crf_edge as f64) / 6.0);
            a.extend(s(&["-b:v", &format!("{}", bits.round() as u64)]));
        }
    }
    if codec == Codec::Hevc && !name.ends_with("_vaapi") && !name.starts_with("lib") {
        a.extend(s(&["-tag:v", "hvc1"]));
    }
    a
}

fn mix_args(job: &Job, opts: &ExportOptions, out: &Path) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut graph = String::new();
    let mut joined = String::new();
    for (i, piece) in job.pieces.iter().enumerate() {
        let src = &job.sources[job.index[i]];
        let (a, b) = (job.frame_of(piece.from), job.frame_of(piece.to));
        let start = job.seconds(a);
        let dur = job.seconds(b - a);
        args.extend([
            "-ss".into(),
            format!("{start:.6}"),
            "-t".into(),
            format!("{:.6}", dur + 0.5),
            "-i".into(),
            src.path.display().to_string(),
        ]);
        let tracks: Vec<(usize, f32)> = src
            .probe
            .audio
            .iter()
            .map(|t| {
                (
                    t.index,
                    opts.audio_gains.get(t.index).copied().unwrap_or(1.0),
                )
            })
            .filter(|(_, g)| *g > 0.0)
            .collect();
        if tracks.is_empty() {
            graph.push_str(&format!(
                "anullsrc=r=48000:cl=stereo,atrim=end={dur:.6}[p{i}];"
            ));
        } else {
            let mut labels = String::new();
            for (t, g) in &tracks {
                graph.push_str(&format!(
                    "[{i}:a:{t}]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,volume={g:.4}[p{i}t{t}];"
                ));
                labels.push_str(&format!("[p{i}t{t}]"));
            }
            graph.push_str(&format!(
                "{labels}amix=inputs={}:normalize=0:duration=longest,apad=whole_dur={dur:.6},atrim=end={dur:.6},asetpts=N/SR/TB[p{i}];",
                tracks.len()
            ));
        }
        joined.push_str(&format!("[p{i}]"));
    }
    graph.push_str(&format!(
        "{joined}concat=n={}:v=0:a=1[mix]",
        job.pieces.len()
    ));
    let script = job.work.join("mix.graph");
    let _ = std::fs::write(&script, &graph);
    args.extend([
        "-/filter_complex".into(),
        script.display().to_string(),
        "-map".into(),
        "[mix]".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        format!("{}k", opts.audio_kbps.max(32)),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "2".into(),
        out.display().to_string(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_between_keyframes_copies_its_middle_and_encodes_its_edges() {
        let keys = [0, 120, 240, 360];
        assert_eq!(
            parts(100, 300, &keys),
            [
                Part::Encode {
                    from: 100,
                    frames: 20
                },
                Part::Copy {
                    from: 120,
                    frames: 120
                },
                Part::Encode {
                    from: 240,
                    frames: 60
                },
            ]
        );
        assert_eq!(
            parts(130, 200, &keys),
            [Part::Encode {
                from: 130,
                frames: 70
            }]
        );
        assert_eq!(
            parts(100, 200, &keys),
            [
                Part::Encode {
                    from: 100,
                    frames: 20
                },
                Part::Encode {
                    from: 120,
                    frames: 80
                },
            ]
        );
    }
}
