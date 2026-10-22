use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::mux::{Premix, Sound};
use super::picture::Blender;
use super::probe::keyframes;
use super::route::{Caps, Picked, Prefs, pick_encode};
use super::shape::Pull;
use super::video::{Source, Video, copied, frame_at, open};
use crate::media::{AudioBuffer, Codec, Error, Frame, PixelFormat, Result, Rgba, Route};

#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub path: PathBuf,
    pub from: f64,
    pub to: f64,
}

const MAX_SECONDS: f64 = 1e7;
pub(super) const MAX_FPS: u32 = 1000;
pub(super) const WRITTEN: f32 = 0.95;

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
    pub sessions: Sessions,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sessions {
    pub edges: u32,
    pub workers: u32,
    pub opened: u32,
    pub reused: u32,
    pub refreshed: u32,
    pub setup: Duration,
}

#[derive(Debug, Default)]
pub(super) struct Tally {
    edges: AtomicU32,
    workers: AtomicU32,
    opened: AtomicU32,
    reused: AtomicU32,
    refreshed: AtomicU32,
    nanos: AtomicU64,
}

impl Tally {
    pub(super) fn open<T>(&self, make: impl FnOnce() -> Result<T>) -> Result<T> {
        let start = Instant::now();
        let made = make();
        self.spent(start.elapsed());
        if made.is_ok() {
            self.opened.fetch_add(1, Ordering::Relaxed);
        }
        made
    }

    pub(super) fn spent(&self, d: Duration) {
        let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_add(nanos, Ordering::Relaxed);
    }

    pub(super) fn opened(&self) {
        self.opened.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn workers(&self, n: usize) {
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        self.workers.fetch_max(n, Ordering::Relaxed);
    }

    pub(super) fn edge(&self) {
        self.edges.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn reused(&self) {
        self.reused.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn refreshed(&self) {
        self.refreshed.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn sessions(&self) -> Sessions {
        Sessions {
            edges: self.edges.load(Ordering::Relaxed),
            workers: self.workers.load(Ordering::Relaxed),
            opened: self.opened.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            refreshed: self.refreshed.load(Ordering::Relaxed),
            setup: Duration::from_nanos(self.nanos.load(Ordering::Relaxed)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Copy { from: i64, frames: i64 },
    Encode { from: i64, frames: i64 },
}

impl Part {
    pub(super) fn frames(self) -> i64 {
        match self {
            Part::Copy { frames, .. } | Part::Encode { frames, .. } => frames,
        }
    }
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

#[derive(Clone)]
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
    pub(super) device: Option<crate::gpu::Device>,
    pub(super) look: super::mux::Look,
    pub(super) spare: Arc<Mutex<Option<crate::gpu::HwEncoder>>>,
    pub(super) fit: bool,
    pub(super) tally: Arc<Tally>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fit {
    pub(super) size: (u32, u32),
    pub(super) fps: (u32, u32),
}

pub(super) type Tell<'a> = &'a mut dyn FnMut(&Progress) -> bool;

pub(super) type Plan = (Vec<(usize, Part)>, Vec<usize>);

impl Job<'_> {
    pub(super) fn decodable(&self, si: usize) -> Result<()> {
        match self.sources.get(si) {
            Some(src) => named(&src.path, self.sources.len() > 1, src.probe.decodable()),
            None => Ok(()),
        }
    }

    pub(super) fn frame_of(&self, seconds: f64) -> i64 {
        frame_at(seconds, self.fps)
    }

    pub(super) fn span(&self, piece: &Piece) -> (i64, i64) {
        (self.frame_of(piece.from), self.frame_of(piece.to))
    }

    pub(super) fn quiet(&self) -> Self {
        Job {
            total: 0,
            done: 0,
            ..self.clone()
        }
    }

    pub(super) fn plan(
        &self,
        prefs: &Prefs,
        mut split: impl FnMut(usize, Part, &[i64]) -> Result<Vec<Part>>,
    ) -> Result<Plan> {
        let mut keys: HashMap<usize, Vec<i64>> = HashMap::new();
        let mut plan: Vec<(usize, Part)> = Vec::new();
        let mut order = Vec::new();
        for (n, piece) in self.pieces.iter().enumerate() {
            let si = self.index[n];
            if let std::collections::hash_map::Entry::Vacant(slot) = keys.entry(si) {
                slot.insert(
                    keyframes(&self.sources[si].path, prefs)?
                        .into_iter()
                        .map(|t| self.frame_of(t))
                        .collect(),
                );
            }
            let (a, b) = self.span(piece);
            for part in parts(a, b, &keys[&si]) {
                for p in split(si, part, &keys[&si])? {
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
        }
        Ok((plan, order))
    }

    pub(super) fn finish(&self, progress: Tell) -> Result<()> {
        if progress(&Progress {
            stage: Stage::Joining,
            done: 1.0,
            encoder: self.route,
        }) {
            Ok(())
        } else {
            Err(Error::Cancelled)
        }
    }

    pub(super) fn seconds(&self, frames: i64) -> f64 {
        frames as f64 * self.fps.1 as f64 / self.fps.0 as f64
    }

    pub(super) fn tell(&self, progress: Tell, stage: Stage) -> Result<()> {
        let done = if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).min(1.0) * WRITTEN
        };
        let done = match stage {
            Stage::Mixing | Stage::Joining => WRITTEN,
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
    opts: ExportOptions,
    progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    export_to(out, pieces, opts, None, None, progress)
}

pub(super) fn export_fitted(
    out: &Path,
    pieces: &[Piece],
    opts: ExportOptions,
    fit: Fit,
    progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    export_to(out, pieces, opts, None, Some(fit), progress)
}

pub fn export_with_audio(
    out: &Path,
    pieces: &[Piece],
    opts: ExportOptions,
    mut audio: impl FnMut(usize, usize) -> Result<AudioBuffer> + Send,
    progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    export_to(out, pieces, opts, Some(&mut audio), None, progress)
}

fn export_to(
    out: &Path,
    pieces: &[Piece],
    mut opts: ExportOptions,
    premix: Option<Premix>,
    fit: Option<Fit>,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    if pieces.is_empty() {
        return Err(Error::Invalid("nothing to export".to_string()));
    }
    let mut sources: Vec<Source> = Vec::new();
    let mut by_path: HashMap<PathBuf, usize> = HashMap::new();
    let mut index = Vec::new();
    let many = pieces.iter().any(|p| p.path != pieces[0].path);
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
                sources.push(named(&p.path, many, copied(&p.path, &opts.prefs))?);
                by_path.insert(p.path.clone(), sources.len() - 1);
                sources.len() - 1
            }
        };
        index.push(i);
    }
    let caps = Caps::detect(&opts.prefs);
    let mut video = pick_encode(opts.video, &opts.prefs, &caps)?;
    let first = sources[index[0]].probe.clone();
    let fps = fit.map_or(first.fps, |f| f.fps);
    if fps.0 == 0 || fps.1 == 0 || fps.0 / fps.1 > MAX_FPS {
        return Err(Error::Invalid(format!(
            "a frame rate of {}/{}",
            fps.0, fps.1
        )));
    }
    let size = fit.map_or((first.width & !1, first.height & !1), |f| f.size);
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
    let look = match fit {
        Some(_) => ((1, 1), crate::gpu::Color::BT709),
        None => {
            let d = &sources[index[0]].demux;
            d.video()
                .ok()
                .and_then(|v| d.stream(v).ok()?.look)
                .unwrap_or(((0, 0), crate::gpu::Color::UNKNOWN))
        }
    };
    let mut device = None;
    let mut spare = None;
    let tally = Arc::new(Tally::default());
    let format = match video.route {
        Route::Gpu => {
            let start = Instant::now();
            let crf = if fit.is_some() {
                opts.crf
            } else {
                opts.crf.saturating_sub(2)
            };
            let checked =
                super::mux::gpu_check(&mut device, video.codec, size, fps, format, look, crf);
            tally.spent(start.elapsed());
            match checked {
                Ok(e) => {
                    tally.opened();
                    spare = Some(e);
                    format
                }
                Err(e) => {
                    video.reasons.push(format!(
                        "the GPU encoder failed ({e}); encoding H.264 on the CPU instead"
                    ));
                    video.route = Route::Cpu;
                    video.codec = Codec::H264;
                    PixelFormat::Yuv420p8
                }
            }
        }
        Route::Cpu => PixelFormat::Yuv420p8,
    };
    let same = sources.iter().all(|s| {
        s.probe.video_codec == Some(video.codec)
            && s.probe.fps == fps
            && (s.probe.width, s.probe.height) == (first.width, first.height)
            && s.probe.bit_depth == first.bit_depth
            && s.probe.rotation == first.rotation
    });
    let spliceless = unspliced(video.codec, &first.profile, first.bit_depth);
    let mut why = if fit.is_some() {
        None
    } else if opts.overlay.is_some() {
        Some("an overlay is drawn on every frame, so the whole export is re-encoded".to_string())
    } else if !same {
        Some(match sources.iter().find(|s| s.probe.video_codec != Some(video.codec)) {
            Some(s) => format!(
                "the source video is {}, the export {}, so the whole export is re-encoded",
                s.probe.video_codec_name, video.codec
            ),
            None => "the sources differ in frame rate, size, depth or rotation, so the whole export is re-encoded".to_string(),
        })
    } else if spliceless.is_some() {
        spliceless.clone()
    } else if size != (first.width, first.height) {
        Some(format!(
            "the picture's odd size {}x{} is evened, so the whole export is re-encoded",
            first.width, first.height
        ))
    } else {
        None
    };
    if premix.is_none() {
        for s in &sources {
            for t in &s.probe.audio {
                if opts.audio_gains.get(t.index).copied().unwrap_or(1.0) > 0.0 {
                    named(&s.path, many, t.decodable())?;
                }
            }
        }
    }
    let work = std::path::absolute(out.with_extension("parts"))?;
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
        device,
        look,
        spare: Arc::new(Mutex::new(spare)),
        fit: fit.is_some(),
        tally,
    };
    if why.is_some()
        && why == spliceless
        && size == (first.width, first.height)
        && super::mux::copied_whole(&job)?
    {
        why = None;
    }
    let smart = why.is_none() && fit.is_none();
    if !smart {
        for s in &job.sources {
            named(&s.path, many, s.probe.decodable())?;
        }
    }
    let mut reasons: Vec<String> = video.reasons.clone();
    for s in &job.sources {
        for r in &s.reasons {
            if !reasons.contains(r) {
                reasons.push(r.clone());
            }
        }
    }
    reasons.extend(why);
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| Error::at(&work, e))?;
    }
    std::fs::create_dir_all(&work).map_err(|e| Error::at(&work, e))?;
    job.total = pieces
        .iter()
        .map(|p| (job.frame_of(p.to) - job.frame_of(p.from)).max(0) as u64)
        .sum();
    let result = super::mux::run(
        &mut job,
        &video,
        &mut opts,
        smart,
        out,
        &mut progress,
        match premix {
            Some(p) => Sound::Premixed(p),
            None => Sound::Tracks,
        },
    );
    let _ = std::fs::remove_dir_all(&work);
    let (copied, encoded, planned) = result?;
    for r in planned {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    }
    Ok(Exported {
        path: out.to_path_buf(),
        frames: copied + encoded,
        copied,
        encoded,
        smart,
        video,
        reasons,
        sessions: job.tally.sessions(),
    })
}

fn named<T>(path: &Path, many: bool, done: Result<T>) -> Result<T> {
    match done {
        Err(e) if many => Err(super::join::about(&path.display().to_string(), e)),
        done => done,
    }
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
    let mut frames_in = reencode_open(job, si, from, frames)?;
    if job.fit {
        let mut shaped = super::shape::Shaped::new(job, si, &mut frames_in)?;
        return reencode_run(job, &mut shaped, frames, at, push, overlay, progress);
    }
    reencode_run(job, &mut frames_in, frames, at, push, overlay, progress)
}

pub(super) fn reencode_open(job: &Job, si: usize, from: i64, frames: i64) -> Result<Video> {
    let src = &job.sources[si];
    if job.fit {
        return open(src, job.seconds(from), None, None, None);
    }
    let size = (src.probe.width, src.probe.height) != job.size;
    open(
        src,
        job.seconds(from),
        Some(frames.max(0) as u64),
        size.then_some(job.size),
        Some(job.fps),
    )
}

pub(super) fn reencode_run(
    job: &mut Job,
    frames_in: &mut dyn Pull,
    frames: i64,
    at: i64,
    push: &mut dyn FnMut(&Frame) -> Result<()>,
    overlay: &mut Option<Overlay>,
    progress: Tell,
) -> Result<()> {
    let mut n = 0i64;
    let mut last: Option<Frame> = None;
    let mut blender = Blender::new();
    while n < frames {
        let mut frame = match frames_in.pull() {
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
                frames_in.give(frame);
                clean
            }
            None => {
                push(&frame)?;
                frame
            }
        };
        if let Some(old) = last.replace(keep) {
            frames_in.give(old);
        }
        n += 1;
        job.done += 1;
        if n % 15 == 0 {
            job.tell(progress, Stage::Encoding)?;
        }
    }
    Ok(())
}

pub(super) fn convert_depth(frame: &Frame, format: PixelFormat) -> Frame {
    let mut out = Frame::new(
        format,
        frame.width,
        frame.height,
        frame.pts,
        frame.time_base,
    );
    for (o, i) in out.planes.iter_mut().zip(&frame.planes) {
        let rows = o
            .data
            .chunks_mut(o.stride.max(1))
            .zip(i.data.chunks(i.stride.max(1)));
        for (dst, src) in rows.take(i.height) {
            match format {
                PixelFormat::Yuv420p8 => {
                    for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)).take(i.width) {
                        *d = ((u16::from_le_bytes([s[0], s[1]]) + 2) / 4).min(255) as u8;
                    }
                }
                PixelFormat::Yuv420p10 => {
                    for (d, &s) in dst.chunks_exact_mut(2).zip(src).take(i.width) {
                        d.copy_from_slice(&(s as u16 * 4).to_le_bytes());
                    }
                }
            }
        }
    }
    out
}

fn unspliced(codec: Codec, profile: &str, bits: u8) -> Option<String> {
    let (fits, ours) = match codec {
        Codec::Vp9 => (
            profile == "Profile 0" && bits <= 8,
            "VP9 encoder writes profile 0 at 8 bits",
        ),
        Codec::H264 => (
            bits <= 8
                && matches!(
                    profile,
                    "Baseline" | "Constrained Baseline" | "Main" | "Extended" | "High"
                ),
            "H.264 encoder writes 4:2:0 at 8 bits",
        ),
        Codec::Hevc => (
            bits <= 10 && profile != "Rext",
            "HEVC encoder writes Main or Main 10",
        ),
        _ => (true, ""),
    };
    (!fits).then(|| {
        format!(
            "the source {codec} is {profile} at {bits} bits, our {ours}, so the whole export is re-encoded"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_our_encoders_cannot_match_re_encode_the_whole_export() {
        assert_eq!(unspliced(Codec::H264, "High", 8), None);
        assert_eq!(unspliced(Codec::H264, "Main", 8), None);
        assert_eq!(unspliced(Codec::H264, "Baseline", 8), None);
        assert_eq!(unspliced(Codec::H264, "Extended", 8), None);
        assert_eq!(unspliced(Codec::Hevc, "Main", 8), None);
        assert_eq!(unspliced(Codec::Hevc, "Main 10", 10), None);
        assert_eq!(unspliced(Codec::Vp9, "Profile 0", 8), None);
        for (codec, profile, bits) in [
            (Codec::H264, "High 10", 10),
            (Codec::H264, "High 4:2:2", 8),
            (Codec::H264, "High 4:4:4 Predictive", 8),
            (Codec::H264, "", 8),
            (Codec::Hevc, "Main 12", 12),
            (Codec::Hevc, "Rext", 8),
            (Codec::Vp9, "Profile 2", 10),
            (Codec::Vp9, "Profile 1", 8),
        ] {
            let why = unspliced(codec, profile, bits)
                .unwrap_or_else(|| panic!("{codec} {profile} {bits}"));
            assert!(why.contains("whole export is re-encoded"), "{why}");
            assert!(why.contains(profile), "{why}");
        }
    }

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
