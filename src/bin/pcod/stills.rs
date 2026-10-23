use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Instant;

use pito_codecs::media::{self, Frame, Plane, Prefs, Rgba, Seek, Stills, keyframes, probe};
use pito_codecs::{jpeg, png, webp};
use rayon::prelude::*;

use super::json::quoted;
use super::pipeline::{AlphaToRgba, ToRgba};
use super::size::Size;
use super::source::{self, Media};
use super::{Fail, about};

pub struct Job {
    pub file: PathBuf,
    pub count: Option<usize>,
    pub at: Vec<f64>,
    pub height: u32,
    pub out: Option<PathBuf>,
    pub sheet: Option<PathBuf>,
    pub rows: Option<usize>,
    pub cols: Option<usize>,
    pub pad: u32,
    pub seek: Seek,
    pub prefs: Prefs,
    pub json: bool,
    pub format: Format,
    pub quality: Option<u8>,
    pub range: Option<Range>,
}

const MAX_STILLS: usize = 10_000;
const DEFAULT_COUNT: usize = 12;
const DEFAULT_QUALITY: u8 = 85;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
}

impl Format {
    fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Webp => "webp",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpeg",
            Format::Webp => "webp",
        }
    }

    fn encode(self, image: &Rgba, quality: u8, alpha: bool) -> Result<Vec<u8>, Fail> {
        Ok(match self {
            Format::Png if alpha => png::encode_rgba(image)?,
            Format::Png => png::encode(image)?,
            Format::Jpeg if alpha => {
                let mut flat = Rgba::new(image.width, image.height);
                super::filmstrip::over(&mut flat.data, &image.data, [0, 0, 0]);
                jpeg::encode(&flat, quality)?
            }
            Format::Jpeg => jpeg::encode(image, quality)?,
            Format::Webp => webp::encode(image, &webp::Options::lossy(f32::from(quality)))?,
        })
    }
}

struct Grid {
    rows: usize,
    cols: usize,
}

fn count_of(job: &Job) -> usize {
    if !job.at.is_empty() {
        return job.at.len();
    }
    match (job.count, job.rows, job.cols) {
        (Some(n), _, _) => n,
        (None, Some(r), Some(c)) if job.sheet.is_some() => r.saturating_mul(c),
        _ => DEFAULT_COUNT,
    }
}

fn grid(n: usize, rows: Option<usize>, cols: Option<usize>) -> Result<Grid, Fail> {
    let (rows, cols) = match (rows, cols) {
        (Some(r), Some(c)) => (r, c),
        (Some(r), None) => (r, n.div_ceil(r)),
        (None, Some(c)) => (n.div_ceil(c), c),
        (None, None) => {
            let c = (n as f64).sqrt().ceil().max(1.0) as usize;
            (n.div_ceil(c), c)
        }
    };
    if rows == 0 || cols == 0 {
        return Err("a contact sheet needs at least one row and one column".into());
    }
    if rows.saturating_mul(cols) < n {
        return Err(format!("{n} stills do not fit a {rows}x{cols} contact sheet").into());
    }
    Ok(Grid { rows, cols })
}

fn evenly(duration: f64, n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| duration * (i as f64 + 0.5) / n as f64)
        .collect()
}

fn snapped(keys: &[f64], at: f64) -> f64 {
    if at <= 0.0 {
        return 0.0;
    }
    keys.iter()
        .copied()
        .take_while(|k| *k <= at + 1e-6)
        .last()
        .unwrap_or(0.0)
}

fn sheet(stills: &[Rgba], g: &Grid, pad: u32) -> Result<Rgba, Fail> {
    let cell_w = stills.iter().map(|s| s.width).max().unwrap_or(0);
    let cell_h = stills.iter().map(|s| s.height).max().unwrap_or(0);
    let span = |cells: usize, cell: u32| -> Option<u32> {
        let cells = u32::try_from(cells).ok()?;
        cell.checked_mul(cells)?
            .checked_add(pad.checked_mul(cells.checked_add(1)?)?)
    };
    let (w, h) = match (span(g.cols, cell_w), span(g.rows, cell_h)) {
        (Some(w), Some(h)) if (w as u64) * (h as u64) <= 1 << 28 => (w, h),
        _ => return Err("the contact sheet would be larger than 268 megapixels".into()),
    };
    let mut out = Rgba::new(w, h);
    for px in out.data.chunks_exact_mut(4) {
        px.copy_from_slice(&[0, 0, 0, 255]);
    }
    let stride = out.stride();
    for (i, s) in stills.iter().enumerate() {
        let (r, c) = ((i / g.cols) as u32, (i % g.cols) as u32);
        let x = pad + c * (cell_w + pad) + (cell_w - s.width) / 2;
        let y = pad + r * (cell_h + pad) + (cell_h - s.height) / 2;
        let row = s.stride();
        for (sy, line) in s.data.chunks_exact(row).take(s.height as usize).enumerate() {
            let at = (y as usize + sy) * stride + x as usize * 4;
            if let Some(dst) = out.data.get_mut(at..at + row) {
                dst.copy_from_slice(line);
            }
        }
    }
    Ok(out)
}

fn write_image(path: &Path, image: &Rgba, format: Format, quality: u8) -> Result<(), Fail> {
    let data = format
        .encode(image, quality, false)
        .map_err(|e| about(path, e))?;
    std::fs::write(path, data).map_err(|e| about(path, e))?;
    Ok(())
}

fn path_json(p: Option<&Path>) -> String {
    match p {
        Some(p) => quoted(&p.display().to_string()),
        None => "null".into(),
    }
}

fn quality_note(job: &Job) -> Option<&'static str> {
    (job.format == Format::Png && job.quality.is_some())
        .then_some("pcod: png is lossless, so --quality is ignored")
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    let done = match &job.range {
        Some(range) => {
            let fresh = job.out.as_deref().is_some_and(|d| !d.exists());
            let done = run_range(out, job, range);
            if let (Err(_), true, Some(dir)) = (&done, fresh, job.out.as_deref()) {
                let _ = std::fs::remove_dir(dir);
            }
            done
        }
        None => run_stills(out, job),
    };
    if done.is_ok()
        && let Some(note) = quality_note(job)
    {
        eprintln!("{note}");
    }
    done
}

fn run_stills(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    let started = Instant::now();
    let n = count_of(job);
    if n == 0 {
        return Err("asked for no stills".into());
    }
    if n > MAX_STILLS {
        return Err(format!("asked for {n} stills, at most {MAX_STILLS}").into());
    }
    if job.height == 0 || job.height > 4320 {
        return Err(format!("height {} is outside 1 to 4320", job.height).into());
    }
    if let Some(t) = job.at.iter().find(|t| !t.is_finite() || **t < 0.0) {
        return Err(format!("time {t} is not a time in seconds").into());
    }
    let g = match &job.sheet {
        Some(_) => Some(grid(n, job.rows, job.cols)?),
        None => None,
    };
    let p = probe(&job.file, &job.prefs).map_err(|e| about(&job.file, e))?;
    let times = if job.at.is_empty() {
        if !(p.duration.is_finite() && p.duration > 0.0) {
            return Err(about(&job.file, "has no duration to space stills over"));
        }
        evenly(p.duration, n)
    } else {
        if p.duration.is_finite()
            && p.duration > 0.0
            && let Some(t) = job.at.iter().find(|t| **t >= p.duration)
        {
            return Err(about(
                &job.file,
                format!(
                    "{t:.3} s is past the end of the video ({:.3} s)",
                    p.duration
                ),
            ));
        }
        job.at.clone()
    };
    let stills =
        Stills::open(&job.file, job.height, &job.prefs).map_err(|e| about(&job.file, e))?;
    let mut got: Vec<Option<media::Result<Rgba>>> = (0..n).map(|_| None).collect();
    stills
        .frames(&times, job.seek, |i, r| {
            if let Some(slot) = got.get_mut(i) {
                *slot = Some(r);
            }
        })
        .map_err(|e| about(&job.file, e))?;
    let mut images = Vec::with_capacity(n);
    for (i, r) in got.into_iter().enumerate() {
        match r {
            Some(Ok(img)) => images.push(img),
            Some(Err(e)) => {
                return Err(about(&job.file, format!("still at {:.3} s: {e}", times[i])));
            }
            None => return Err(about(&job.file, format!("no still at {:.3} s", times[i]))),
        }
    }
    let shown: Vec<f64> = match job.seek {
        Seek::Exact => times.clone(),
        Seek::Keyframe => {
            let keys = keyframes(&job.file, &job.prefs).unwrap_or_else(|_| vec![0.0]);
            times.iter().map(|t| snapped(&keys, *t)).collect()
        }
    };
    let paths: Vec<Option<PathBuf>> = match &job.out {
        Some(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| about(dir, e))?;
            let digits = n.to_string().len().max(3);
            (1..=n)
                .map(|i| Some(dir.join(format!("still-{i:0digits$}.{}", job.format.ext()))))
                .collect()
        }
        None => vec![None; n],
    };
    let sheet_image = match &g {
        Some(g) => Some(sheet(&images, g, job.pad)?),
        None => None,
    };
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    let writes: Vec<(&Path, &Rgba, Format)> = paths
        .iter()
        .zip(&images)
        .filter_map(|(p, img)| p.as_deref().map(|p| (p, img, job.format)))
        .chain(
            job.sheet
                .as_deref()
                .zip(sheet_image.as_ref())
                .map(|(p, img)| (p, img, Format::Png)),
        )
        .collect();
    writes
        .par_iter()
        .map(|(p, img, format)| write_image(p, img, *format, quality).map_err(|e| e.to_string()))
        .collect::<Result<Vec<()>, String>>()?;
    let elapsed = started.elapsed().as_secs_f64();
    if job.json {
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"file\":{},\"height\":{},\"seek\":\"{}\",\"stills\":[",
            path_json(Some(&job.file)),
            job.height,
            match job.seek {
                Seek::Exact => "exact",
                Seek::Keyframe => "keyframe",
            }
        );
        for (i, img) in images.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(
                s,
                "{{\"index\":{i},\"time\":{:.6},\"asked\":{:.6},\"width\":{},\"height\":{},\"path\":{}}}",
                shown[i],
                times[i],
                img.width,
                img.height,
                path_json(paths[i].as_deref())
            );
        }
        s.push_str("],\"sheet\":");
        match (&job.sheet, &sheet_image, &g) {
            (Some(p), Some(img), Some(g)) => {
                let _ = write!(
                    s,
                    "{{\"path\":{},\"width\":{},\"height\":{},\"rows\":{},\"cols\":{},\"pad\":{}}}",
                    path_json(Some(p)),
                    img.width,
                    img.height,
                    g.rows,
                    g.cols,
                    job.pad
                );
            }
            _ => s.push_str("null"),
        }
        let _ = write!(s, ",\"seconds\":{elapsed:.3}}}");
        writeln!(out, "{s}")?;
    } else {
        for (i, img) in images.iter().enumerate() {
            let shown_at = format!("{:.3} s", shown[i]);
            match &paths[i] {
                Some(p) => writeln!(
                    out,
                    "{shown_at:>12}  {}x{}  {}",
                    img.width,
                    img.height,
                    p.display()
                )?,
                None => writeln!(out, "{shown_at:>12}  {}x{}", img.width, img.height)?,
            }
        }
        if let (Some(p), Some(img), Some(g)) = (&job.sheet, &sheet_image, &g) {
            writeln!(
                out,
                "sheet  {}x{} stills, {}x{}  {}",
                g.cols,
                g.rows,
                img.width,
                img.height,
                p.display()
            )?;
        }
        writeln!(out, "{n} stills in {elapsed:.2} s")?;
    }
    Ok(())
}

pub struct Range {
    pub from: f64,
    pub to: Option<f64>,
    pub fps: Option<(u32, u32)>,
    pub every: Option<f64>,
    pub size: Option<Size>,
    pub force: bool,
}

impl Range {
    fn step(&self) -> Option<f64> {
        match (self.every, self.fps) {
            (Some(every), _) => Some(every),
            (None, Some((num, den))) => Some(f64::from(den) / f64::from(num.max(1))),
            (None, None) => None,
        }
    }
}

const MAX_RANGE: u64 = 100_000;
const NAME_DIGITS: usize = 6;

struct Slots {
    first: f64,
    step: f64,
    count: u64,
}

impl Slots {
    fn of(from: f64, end: f64, source: (u32, u32), step: Option<f64>) -> Slots {
        let seconds = |(num, den): (u32, u32)| f64::from(den) / f64::from(num.max(1));
        match step {
            None => {
                let (a, b) = (
                    media::first_frame_at(from, source),
                    media::first_frame_at(end, source),
                );
                Slots {
                    first: a as f64 * seconds(source),
                    step: seconds(source),
                    count: u64::try_from(b - a).unwrap_or(0),
                }
            }
            Some(step) => {
                let count = ((end - from) / step - 1e-6).ceil().max(0.0);
                Slots {
                    first: from,
                    step,
                    count: if count >= 9e15 {
                        u64::MAX
                    } else {
                        count as u64
                    },
                }
            }
        }
    }

    fn at(&self, k: u64) -> f64 {
        self.first + k as f64 * self.step
    }
}

fn picked(slots: &Slots, next: &mut u64, held: f64, coming: Option<f64>) -> Vec<u64> {
    let mut taken = Vec::new();
    while *next < slots.count {
        if let Some(t) = coming
            && slots.at(*next) > (held + t) / 2.0 + 1e-9
        {
            break;
        }
        taken.push(*next);
        *next += 1;
    }
    taken
}

fn tail(slots: &Slots, next: &mut u64, held: f64, step: f64) -> Vec<u64> {
    let mut taken = Vec::new();
    while *next < slots.count && slots.at(*next) + 1e-6 < held + step {
        taken.push(*next);
        *next += 1;
    }
    taken
}

enum Feed {
    Plain(media::Video),
    Alpha(Box<media::AlphaVideo>),
}

impl Feed {
    fn open(
        file: &Path,
        media: &Media,
        lead: f64,
        size: (u32, u32),
        prefs: &Prefs,
    ) -> Result<Feed, Fail> {
        let opened = if media.alpha {
            media::decode_alpha(file, lead).map(|v| Feed::Alpha(Box::new(v)))
        } else {
            media::decode_video(file, lead, shrunk(media, size), prefs).map(Feed::Plain)
        };
        opened.map_err(|e| about(file, e))
    }

    fn alpha(&self) -> Option<((u32, u32), media::Color)> {
        match self {
            Feed::Alpha(v) => Some(((v.width, v.height), v.color)),
            Feed::Plain(_) => None,
        }
    }

    fn next(&mut self) -> Result<Option<(Frame, Option<Plane>)>, media::Error> {
        match self {
            Feed::Alpha(v) => v.next_frame(),
            Feed::Plain(v) => v.next().transpose().map(|f| f.map(|f| (f, None))),
        }
    }
}

fn numbered(name: &str, ext: &str) -> bool {
    name.strip_suffix(ext)
        .and_then(|stem| stem.strip_suffix('.'))
        .is_some_and(|stem| stem.len() >= NAME_DIGITS && stem.bytes().all(|b| b.is_ascii_digit()))
}

fn a_frame(name: &str) -> bool {
    [Format::Png, Format::Jpeg, Format::Webp]
        .iter()
        .any(|f| numbered(name, f.ext()))
}

fn ready(dir: &Path, force: bool, format: Format) -> Result<(), Fail> {
    std::fs::create_dir_all(dir).map_err(|e| about(dir, e))?;
    let mut held = Vec::new();
    let mut other = false;
    for entry in std::fs::read_dir(dir).map_err(|e| about(dir, e))? {
        let path = entry.map_err(|e| about(dir, e))?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if a_frame(&name) {
            held.push(path);
        } else if format == Format::Png && name.ends_with(".png") {
            other = true;
        }
    }
    if other {
        return Err(about(
            dir,
            "holds PNGs that are not numbered frames, which pcod frames would read too; pick an empty folder",
        ));
    }
    if !force && !held.is_empty() {
        return Err(about(
            dir,
            "already holds numbered frames; use --force to replace them, or pick an empty folder",
        ));
    }
    for p in held {
        std::fs::remove_file(&p).map_err(|e| about(&p, e))?;
    }
    Ok(())
}

fn range_size(media: &Media, range: &Range, height: u32) -> Result<(u32, u32), Fail> {
    let (w, h) = media.display_size();
    if w == 0 || h == 0 {
        return Err(format!("the video's shape is {w}x{h}").into());
    }
    match range.size {
        Some(size) => Ok(size.resolve(w, h)?),
        None => {
            if height == 0 || height > 4320 {
                return Err(format!("height {height} is outside 1 to 4320").into());
            }
            let width = (u64::from(w) * u64::from(height) / u64::from(h)).min(u64::from(u32::MAX));
            Ok(((width as u32).max(2) & !1, height))
        }
    }
}

fn shrunk(media: &Media, (w, h): (u32, u32)) -> Option<u32> {
    let plain = media.rotation == 0 && media.sar.0 == media.sar.1 && h < media.height;
    let width = (u64::from(media.width) * u64::from(h) / u64::from(media.height.max(1))) as u32;
    (plain && w == width.max(2) & !1).then_some(h)
}

fn rate_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.0005 {
        format!("{fps:.0}")
    } else {
        format!("{fps:.3}")
    }
}

fn run_range(out: &mut impl Write, job: &Job, range: &Range) -> Result<(), Fail> {
    let started = Instant::now();
    let dir = job
        .out
        .as_deref()
        .ok_or("a frame range writes its frames into --out")?;
    let to = range.to.unwrap_or(f64::INFINITY);
    if to <= range.from {
        return Err(format!("--to {to} must come after --from {}", range.from).into());
    }
    let media = source::file(&job.file)?;
    let length = media::track_length(media.seconds, media.frames, media.fps);
    if let Some(length) = length
        && range.from >= length
    {
        return Err(about(
            &job.file,
            format!(
                "--from {} is at or past the end of the video ({length:.3} s)",
                range.from
            ),
        ));
    }
    let end = length.map_or(to, |length| to.min(length));
    if !end.is_finite() {
        return Err(about(&job.file, "has no known length; give --to"));
    }
    if media.fps.0 == 0 || media.fps.1 == 0 {
        return Err(about(&job.file, "has no frame rate"));
    }
    let rate = range.fps.unwrap_or(media.fps);
    let per_second = match range.every {
        Some(every) => 1.0 / every,
        None => f64::from(rate.0) / f64::from(rate.1.max(1)),
    };
    let slots = Slots::of(range.from, end, media.fps, range.step());
    if slots.count == 0 {
        return Err(about(
            &job.file,
            format!("no frame starts between {:.3} s and {end:.3} s", range.from),
        ));
    }
    if slots.count > MAX_RANGE && !range.force {
        return Err(format!(
            "{} frames from {:.3} s to {end:.3} s is more than {MAX_RANGE}; use --force to write them all",
            slots.count, range.from
        )
        .into());
    }
    let size = range_size(&media, range, job.height)?;
    ready(dir, range.force, job.format)?;
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    let ext = job.format.ext();
    let digits = slots.count.to_string().len().max(NAME_DIGITS);
    let lead = (slots.at(0) - f64::from(media.fps.1) / f64::from(media.fps.0)).max(0.0);
    let mut feed = Feed::open(&job.file, &media, lead, size, &job.prefs)?;
    let see_through = feed
        .alpha()
        .map(|(coded, color)| AlphaToRgba::new(coded, color, media.rotation, size))
        .transpose()?;
    let step = f64::from(media.fps.1) / f64::from(media.fps.0);
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .clamp(1, 16);
    let (tx, rx) = std::sync::mpsc::sync_channel::<(Frame, Option<Plane>, Vec<u64>)>(workers * 2);
    let rx = Mutex::new(rx);
    let first_png: OnceLock<f64> = OnceLock::new();
    let written = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let made: Mutex<Vec<(u64, PathBuf, f64)>> = Mutex::new(Vec::new());
    let (read, wrote) = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let (rx, media, first_png, written, stop, made, see_through) = (
                    &rx,
                    &media,
                    &first_png,
                    &written,
                    &stop,
                    &made,
                    &see_through,
                );
                s.spawn(move || -> Result<(), Fail> {
                    let mut fix = see_through.is_none().then(|| ToRgba::new(media, size));
                    let mut failed: Option<Fail> = None;
                    loop {
                        let next = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok((frame, plane, taken)) = next else {
                            return failed.map_or(Ok(()), Err);
                        };
                        if stop.load(Ordering::Relaxed) {
                            continue;
                        }
                        let wrote = (|| -> Result<(), Fail> {
                            let time = frame.time_base.seconds(frame.pts);
                            let data = match (see_through, &mut fix) {
                                (Some(a), _) => job.format.encode(
                                    &a.apply(&frame, plane.as_ref())?,
                                    quality,
                                    true,
                                ),
                                (None, Some(fix)) => {
                                    job.format.encode(&fix.apply(frame)?, quality, false)
                                }
                                (None, None) => return Err("no RGBA converter".into()),
                            }
                            .map_err(|e| about(dir, e))?;
                            for k in taken {
                                let path = dir.join(format!("{k:0digits$}.{ext}"));
                                std::fs::write(&path, &data).map_err(|e| about(&path, e))?;
                                made.lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .push((k, path, time));
                                let _ = first_png.set(started.elapsed().as_secs_f64());
                                written.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(())
                        })();
                        if let Err(e) = wrote {
                            stop.store(true, Ordering::Relaxed);
                            failed.get_or_insert(e);
                        }
                    }
                })
            })
            .collect();
        let read = (|| -> Result<(), Fail> {
            let mut next = 0u64;
            let mut held: Option<(Frame, Option<Plane>, f64)> = None;
            while let Some((frame, plane)) = feed.next().map_err(|e| about(&job.file, e))? {
                if stop.load(Ordering::Relaxed) {
                    return Err(Fail::from("a frame writer stopped"));
                }
                let t = frame.time_base.seconds(frame.pts);
                if let Some((f, p, at)) = held.take() {
                    let taken = picked(&slots, &mut next, at, Some(t));
                    if !taken.is_empty() {
                        tx.send((f, p, taken))
                            .map_err(|_| Fail::from("a frame writer stopped"))?;
                    }
                }
                if next >= slots.count {
                    return Ok(());
                }
                held = Some((frame, plane, t));
            }
            match held {
                Some((f, p, at)) => {
                    let taken = tail(&slots, &mut next, at, step);
                    if !taken.is_empty() {
                        tx.send((f, p, taken))
                            .map_err(|_| Fail::from("a frame writer stopped"))?;
                    }
                    Ok(())
                }
                None => Err(about(
                    &job.file,
                    format!("no frame decoded from {:.3} s", range.from),
                )),
            }
        })();
        drop(tx);
        let wrote: Vec<Result<(), Fail>> = handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("a frame writer failed".into()))
            })
            .collect();
        (read, wrote)
    });
    let failed = wrote.into_iter().find_map(Result::err).map_or(read, Err);
    if let Err(e) = failed {
        for (_, path, _) in made.into_inner().unwrap_or_else(PoisonError::into_inner) {
            let _ = std::fs::remove_file(path);
        }
        return Err(e);
    }
    let count = written.load(Ordering::Relaxed);
    if count == 0 {
        return Err(about(
            &job.file,
            format!("the stream ends before {:.3} s", slots.at(0)),
        ));
    }
    let mut files = made.into_inner().unwrap_or_else(PoisonError::into_inner);
    files.sort_by_key(|(k, _, _)| *k);
    let elapsed = started.elapsed().as_secs_f64();
    let first = first_png.get().copied();
    let speed = if elapsed > 0.0 {
        count as f64 / elapsed
    } else {
        0.0
    };
    let last = slots.at(count.saturating_sub(1));
    if job.json {
        let mut listed = String::new();
        for (i, (k, path, time)) in files.iter().enumerate() {
            if i > 0 {
                listed.push(',');
            }
            let _ = write!(
                listed,
                "{{\"index\":{k},\"slot\":{:.6},\"time\":{time:.6},\"path\":{}}}",
                slots.at(*k),
                path_json(Some(path))
            );
        }
        writeln!(
            out,
            "{{\"file\":{},\"from\":{:.6},\"to\":{end:.6},\"first\":{:.6},\"last\":{last:.6},\"count\":{count},\"expected\":{},\"fps\":{},\"rate\":{},\"every\":{},\"format\":\"{}\",\"quality\":{},\"width\":{},\"height\":{},\"dir\":{},\"digits\":{digits},\"first_png\":{},\"seconds\":{elapsed:.3},\"frames_per_second\":{speed:.1},\"files\":[{listed}]}}",
            path_json(Some(&job.file)),
            range.from,
            slots.at(0),
            slots.count,
            rate_text(per_second),
            match range.every {
                Some(_) => "null".to_string(),
                None => format!("\"{}/{}\"", rate.0, rate.1),
            },
            range
                .every
                .map_or("null".to_string(), |e| format!("{e:.6}")),
            job.format.name(),
            match job.format {
                Format::Png => "null".to_string(),
                _ => quality.to_string(),
            },
            size.0,
            size.1,
            path_json(Some(dir)),
            first.map_or("null".to_string(), |t| format!("{t:.3}")),
        )?;
    } else {
        let cadence = match range.every {
            Some(every) => format!("one every {every:.3} s"),
            None => format!("at {} fps", rate_text(per_second)),
        };
        writeln!(
            out,
            "{count} {} frames, {}x{} {cadence}, {:.3} s to {last:.3} s, in {}",
            job.format.name(),
            size.0,
            size.1,
            slots.at(0),
            dir.display()
        )?;
        writeln!(
            out,
            "{elapsed:.2} s, {speed:.1} frames/s, the first image after {:.3} s",
            first.unwrap_or(elapsed)
        )?;
        if count < slots.count {
            writeln!(
                out,
                "note: the stream ends before its probed length: {count} of {} frames written",
                slots.count
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pito_codecs::gpu::Color;

    fn video(seconds: Option<f64>, frames: Option<u64>, fps: (u32, u32)) -> Media {
        Media {
            path: PathBuf::from("clip.mp4"),
            kind: source::Kind::File,
            width: 640,
            height: 360,
            fps,
            seconds,
            frames,
            codec: "h264".into(),
            bit_depth: 8,
            alpha: false,
            color: Color::UNKNOWN,
            rotation: 0,
            sar: (1, 1),
            audio: None,
        }
    }

    fn all(slots: &Slots, times: &[f64]) -> Vec<Vec<u64>> {
        let mut next = 0;
        let mut out = Vec::new();
        for (i, &t) in times.iter().enumerate() {
            out.push(picked(slots, &mut next, t, times.get(i + 1).copied()));
        }
        out
    }

    #[test]
    fn source_rate_slots_cover_the_frames_in_the_window() {
        let s = Slots::of(1.0, 1.5, (24, 1), None);
        assert_eq!(s.count, 12);
        assert!((s.at(0) - 1.0).abs() < 1e-9);
        assert!((s.at(11) - 1.0 - 11.0 / 24.0).abs() < 1e-9);
        let s = Slots::of(1.01, 1.5, (24, 1), None);
        assert_eq!(s.count, 11);
        assert!((s.at(0) - 25.0 / 24.0).abs() < 1e-9);
    }

    #[test]
    fn resampled_slots_count_the_target_rate() {
        assert_eq!(Slots::of(1.0, 1.5, (24, 1), Some(0.1)).count, 5);
        assert_eq!(Slots::of(0.0, 1.0, (24, 1), Some(1.0 / 30.0)).count, 30);
        assert_eq!(Slots::of(0.0, 0.05, (24, 1), Some(0.1)).count, 1);
        assert_eq!(Slots::of(1.0, 1.0, (24, 1), Some(0.1)).count, 0);
        assert_eq!(Slots::of(0.0, 1e300, (24, 1), Some(0.1)).count, u64::MAX);
    }

    #[test]
    fn every_source_frame_fills_its_own_slot() {
        let s = Slots::of(1.0, 1.5, (24, 1), None);
        let times: Vec<f64> = (20..40).map(|k| k as f64 / 24.0).collect();
        let got = all(&s, &times);
        for (i, taken) in got.iter().enumerate() {
            let frame = 20 + i as u64;
            if (24..36).contains(&frame) {
                assert_eq!(taken, &vec![frame - 24], "frame {frame}");
            } else {
                assert!(taken.is_empty(), "frame {frame} took {taken:?}");
            }
        }
    }

    #[test]
    fn a_resampled_slot_takes_a_nearer_frame_that_starts_before_from() {
        let s = Slots::of(1.01, 1.5, (24, 1), Some(0.1));
        let times: Vec<f64> = (22..37).map(|k| k as f64 / 24.0).collect();
        let got = all(&s, &times);
        assert_eq!(got[0], Vec::<u64>::new());
        assert_eq!(got[1], Vec::<u64>::new());
        assert_eq!(got[2], vec![0]);
    }

    #[test]
    fn a_resampled_slot_takes_the_nearest_frame_and_ties_go_earlier() {
        let s = Slots::of(1.0, 1.5, (24, 1), Some(0.1));
        let times: Vec<f64> = (20..40).map(|k| k as f64 / 24.0).collect();
        let got = all(&s, &times);
        let mut chosen = vec![0u64; 5];
        for (i, taken) in got.iter().enumerate() {
            for &k in taken {
                chosen[k as usize] = 20 + i as u64;
            }
        }
        assert_eq!(chosen, vec![24, 26, 29, 31, 34]);
        let s = Slots {
            first: 0.5,
            step: 1.0,
            count: 1,
        };
        assert_eq!(all(&s, &[0.0, 1.0]), vec![vec![0], vec![]]);
    }

    #[test]
    fn the_last_frame_takes_only_the_slots_it_covers() {
        let s = Slots::of(0.0, 0.5, (24, 1), Some(0.1));
        let mut next = 0;
        assert_eq!(tail(&s, &mut next, 0.0, 1.0 / 24.0), vec![0]);
        assert_eq!(tail(&s, &mut next, 0.0, 1.0 / 24.0), Vec::<u64>::new());
        let mut next = 0;
        assert_eq!(tail(&s, &mut next, 0.0, 0.5), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_stream_that_ends_short_leaves_the_later_slots_empty() {
        let s = Slots::of(0.0, 5.0, (24, 1), None);
        assert_eq!(s.count, 120);
        let times: Vec<f64> = (0..60).map(|k| f64::from(k) / 24.0).collect();
        let mut next = 0;
        let mut taken: Vec<u64> = Vec::new();
        for (i, &t) in times.iter().enumerate() {
            match times.get(i + 1) {
                Some(&n) => taken.extend(picked(&s, &mut next, t, Some(n))),
                None => taken.extend(tail(&s, &mut next, t, 1.0 / 24.0)),
            }
        }
        assert_eq!(taken, (0..60).collect::<Vec<u64>>());
        assert_eq!(next, 60);
    }

    #[test]
    fn a_full_stream_fills_every_slot_through_its_last_frame() {
        let s = Slots::of(1.0, 2.0, (24, 1), Some(0.1));
        let times: Vec<f64> = (24..48).map(|k| f64::from(k) / 24.0).collect();
        let mut next = 0;
        let mut taken: Vec<u64> = Vec::new();
        for (i, &t) in times.iter().enumerate() {
            match times.get(i + 1) {
                Some(&n) => taken.extend(picked(&s, &mut next, t, Some(n))),
                None => taken.extend(tail(&s, &mut next, t, 1.0 / 24.0)),
            }
        }
        assert_eq!(taken, (0..s.count).collect::<Vec<u64>>());
    }

    #[test]
    fn own_length_is_the_shorter_of_the_container_and_the_frames() {
        let own_length = |m: &Media| media::track_length(m.seconds, m.frames, m.fps);
        assert_eq!(own_length(&video(Some(5.5), Some(120), (24, 1))), Some(5.0));
        assert_eq!(own_length(&video(Some(4.0), Some(120), (24, 1))), Some(4.0));
        assert_eq!(own_length(&video(None, Some(48), (24, 1))), Some(2.0));
        assert_eq!(own_length(&video(Some(3.0), None, (24, 1))), Some(3.0));
        assert_eq!(own_length(&video(Some(3.0), Some(48), (0, 1))), Some(3.0));
        assert_eq!(own_length(&video(None, None, (24, 1))), None);
    }

    #[test]
    fn the_decoder_scales_only_a_plain_shrink_of_the_same_shape() {
        let m = video(Some(5.0), Some(120), (24, 1));
        assert_eq!(shrunk(&m, (320, 180)), Some(180));
        assert_eq!(shrunk(&m, (426, 240)), Some(240));
        assert_eq!(shrunk(&m, (640, 360)), None);
        assert_eq!(shrunk(&m, (1280, 720)), None);
        assert_eq!(shrunk(&m, (300, 180)), None);
        assert_eq!(
            shrunk(
                &Media {
                    rotation: 90,
                    ..video(Some(5.0), Some(120), (24, 1))
                },
                (320, 180)
            ),
            None
        );
        assert_eq!(shrunk(&Media { sar: (4, 3), ..m }, (320, 180)), None);
    }

    #[test]
    fn numbered_names_are_the_ones_frames_reads() {
        assert!(numbered("000000.png", "png"));
        assert!(numbered("1234567.png", "png"));
        assert!(!numbered("00001.png", "png"));
        assert!(!numbered("frame01.png", "png"));
        assert!(!numbered("000000.jpg", "png"));
        assert!(!numbered("00000a.png", "png"));
    }

    #[test]
    fn a_range_size_keeps_the_shape_for_a_height_and_refuses_a_bad_one() {
        let m = video(Some(5.0), Some(120), (24, 1));
        let r = Range {
            from: 0.0,
            to: None,
            fps: None,
            every: None,
            size: None,
            force: false,
        };
        assert_eq!(range_size(&m, &r, 180).unwrap(), (320, 180));
        assert_eq!(range_size(&m, &r, 241).unwrap(), (428, 241));
        assert!(range_size(&m, &r, 0).is_err());
        assert!(range_size(&m, &r, 4321).is_err());
        let flat = Media { width: 0, ..m };
        assert!(range_size(&flat, &r, 180).is_err());
    }

    #[test]
    fn ready_refuses_pngs_unless_forced_and_never_takes_a_stray_one() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-stills-ready");
        let _ = std::fs::remove_dir_all(&dir);
        ready(&dir, false, Format::Png).unwrap();
        std::fs::write(dir.join("000000.png"), b"x").unwrap();
        std::fs::write(dir.join("000001.png"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(ready(&dir, false, Format::Png).is_err());
        ready(&dir, true, Format::Png).unwrap();
        assert!(!dir.join("000000.png").exists());
        assert!(!dir.join("000001.png").exists());
        assert!(dir.join("notes.txt").exists());
        std::fs::write(dir.join("000005.png"), b"x").unwrap();
        std::fs::write(dir.join("cover.png"), b"x").unwrap();
        assert!(ready(&dir, false, Format::Png).is_err());
        assert!(ready(&dir, true, Format::Png).is_err());
        assert!(dir.join("cover.png").exists());
        assert!(dir.join("000005.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_range_step_comes_from_every_or_fps_or_the_source() {
        let r = |fps: Option<(u32, u32)>, every: Option<f64>| Range {
            from: 0.0,
            to: None,
            fps,
            every,
            size: None,
            force: false,
        };
        assert_eq!(r(None, None).step(), None);
        assert_eq!(r(Some((4, 1)), None).step(), Some(0.25));
        assert_eq!(r(Some((30000, 1001)), None).step(), Some(1001.0 / 30000.0));
        assert_eq!(r(None, Some(2.0)).step(), Some(2.0));
    }

    #[test]
    fn every_two_seconds_over_ten_takes_five_slots_on_the_nearest_frames() {
        let s = Slots::of(0.0, 10.0, (24, 1), Some(2.0));
        assert_eq!(s.count, 5);
        let times: Vec<f64> = (0..240).map(|k| f64::from(k) / 24.0).collect();
        let got = all(&s, &times);
        let chosen: Vec<usize> = got
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(chosen, vec![0, 48, 96, 144, 192]);
    }

    #[test]
    fn an_interval_longer_than_the_window_still_takes_its_first_frame() {
        let s = Slots::of(1.0, 1.5, (24, 1), Some(5.0));
        assert_eq!(s.count, 1);
        assert_eq!(s.at(0), 1.0);
    }

    #[test]
    fn a_window_with_an_interval_starts_at_from() {
        let s = Slots::of(2.5, 6.5, (24, 1), Some(2.0));
        assert_eq!(s.count, 2);
        assert_eq!((s.at(0), s.at(1)), (2.5, 4.5));
    }

    #[test]
    fn a_jpeg_with_alpha_is_flattened_onto_black_first() {
        let mut image = Rgba::new(16, 16);
        let mut flat = Rgba::new(16, 16);
        for (i, (px, want)) in image
            .data
            .chunks_exact_mut(4)
            .zip(flat.data.chunks_exact_mut(4))
            .enumerate()
        {
            let a = if i % 16 < 8 { 0 } else { 128 };
            px.copy_from_slice(&[255, 255, 255, a]);
            let v = if a == 0 { 0 } else { 128 };
            want.copy_from_slice(&[v, v, v, 255]);
        }
        let alpha = Format::Jpeg.encode(&image, 85, true).unwrap();
        assert_eq!(alpha, Format::Jpeg.encode(&flat, 85, false).unwrap());
        assert_ne!(alpha, Format::Jpeg.encode(&image, 85, false).unwrap());
    }

    #[test]
    fn frame_names_cover_every_format() {
        assert_eq!(Format::Png.ext(), "png");
        assert_eq!(Format::Jpeg.ext(), "jpg");
        assert_eq!(Format::Webp.ext(), "webp");
        assert!(a_frame("000000.png"));
        assert!(a_frame("000012.jpg"));
        assert!(a_frame("000012.webp"));
        assert!(!a_frame("000012.jpeg"));
        assert!(!a_frame("000012.gif"));
        assert!(!a_frame("poster.webp"));
        assert!(!numbered("000000png", "png"));
    }

    fn checker() -> Rgba {
        let mut img = Rgba::new(64, 48);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let v = if (i % 64 / 8 + i / 64 / 8) % 2 == 0 {
                255
            } else {
                0
            };
            px.copy_from_slice(&[v, v, v, 255]);
        }
        img
    }

    #[test]
    fn png_and_webp_encodes_decode_back_to_the_same_shape() {
        let img = checker();
        let png_back = png::decode(&Format::Png.encode(&img, 85, false).unwrap()).unwrap();
        assert_eq!(png_back.data, img.data);
        let webp_back = webp::decode(&Format::Webp.encode(&img, 85, false).unwrap()).unwrap();
        assert_eq!((webp_back.width, webp_back.height), (64, 48));
        let low = Format::Webp.encode(&img, 1, false).unwrap();
        let high = Format::Webp.encode(&img, 100, false).unwrap();
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
    }

    #[test]
    fn png_keeps_alpha_only_when_asked() {
        let mut img = checker();
        img.data[3] = 0;
        let kept = png::decode(&Format::Png.encode(&img, 85, true).unwrap()).unwrap();
        assert_eq!(kept.data[3], 0);
        let flat = png::decode(&Format::Png.encode(&img, 85, false).unwrap()).unwrap();
        assert_eq!(flat.data[3], 255);
    }

    #[test]
    fn jpeg_goes_through_the_jpeg_encoder() {
        let low = Format::Jpeg.encode(&checker(), 5, false).unwrap();
        let high = Format::Jpeg.encode(&checker(), 100, false).unwrap();
        for file in [&low, &high] {
            assert!(file.starts_with(&[0xff, 0xd8, 0xff, 0xe0]));
            assert!(file.ends_with(&[0xff, 0xd9]));
        }
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
    }

    #[test]
    fn ready_for_webp_replaces_numbered_frames_of_any_format_and_ignores_stray_pngs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-stills-ready-webp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("000000.png"), b"x").unwrap();
        std::fs::write(dir.join("000001.webp"), b"x").unwrap();
        std::fs::write(dir.join("cover.png"), b"x").unwrap();
        let e = ready(&dir, false, Format::Webp).unwrap_err().to_string();
        assert!(e.contains("already holds numbered frames"), "{e}");
        ready(&dir, true, Format::Webp).unwrap();
        assert!(!dir.join("000000.png").exists());
        assert!(!dir.join("000001.webp").exists());
        assert!(dir.join("cover.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn job(format: Format, quality: Option<u8>) -> Job {
        Job {
            file: PathBuf::from("v.mp4"),
            count: None,
            at: Vec::new(),
            height: 360,
            out: None,
            sheet: None,
            rows: None,
            cols: None,
            pad: 8,
            seek: Seek::Exact,
            prefs: Prefs::default(),
            json: false,
            format,
            quality,
            range: None,
        }
    }

    #[test]
    fn only_png_with_a_quality_gets_the_note() {
        assert!(quality_note(&job(Format::Png, Some(50))).is_some());
        assert!(quality_note(&job(Format::Png, None)).is_none());
        assert!(quality_note(&job(Format::Webp, Some(50))).is_none());
        assert!(quality_note(&job(Format::Jpeg, Some(50))).is_none());
        let note = quality_note(&job(Format::Png, Some(50))).unwrap();
        assert!(note.starts_with("pcod: ") && !note.contains('\n'));
    }
}
