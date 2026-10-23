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
pub const DEFAULT_HEIGHT: u32 = 360;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Crop {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

pub fn crop_of(text: &str) -> Result<Crop, String> {
    let bad = || format!("'{text}' is not a crop such as 640:360:0:0 (width:height:x:y)");
    let parts: Vec<u32> = text
        .trim()
        .split(':')
        .map(|p| p.trim().parse::<u32>().map_err(|_| bad()))
        .collect::<Result<_, _>>()?;
    match parts[..] {
        [width, height, x, y] => Ok(Crop {
            width,
            height,
            x,
            y,
        }),
        _ => Err(bad()),
    }
}

impl Crop {
    fn text(self) -> String {
        format!("{}:{}:{}:{}", self.width, self.height, self.x, self.y)
    }

    pub fn check(self, (w, h): (u32, u32)) -> Result<(), Fail> {
        if self.width == 0 || self.height == 0 {
            return Err(
                format!("--crop {} needs a width and height above zero", self.text()).into(),
            );
        }
        if u64::from(self.x) + u64::from(self.width) > u64::from(w)
            || u64::from(self.y) + u64::from(self.height) > u64::from(h)
        {
            return Err(format!("--crop {} falls outside the {w}x{h} frame", self.text()).into());
        }
        Ok(())
    }

    pub fn apply(self, image: &Rgba) -> Result<Rgba, Fail> {
        self.check((image.width, image.height))?;
        let mut out = Rgba::new(self.width, self.height);
        let (row, from) = (out.stride(), image.stride());
        let left = self.x as usize * 4;
        for (dy, dst) in out.data.chunks_exact_mut(row).enumerate() {
            let at = (self.y as usize + dy) * from + left;
            let src = image
                .data
                .get(at..at + row)
                .ok_or("the frame is shorter than its size")?;
            dst.copy_from_slice(src);
        }
        Ok(out)
    }

    fn json(self) -> String {
        format!(
            "{{\"width\":{},\"height\":{},\"x\":{},\"y\":{}}}",
            self.width, self.height, self.x, self.y
        )
    }
}

fn see_through(color: png::Color, image: &Rgba) -> bool {
    matches!(color, png::Color::Rgba | png::Color::GreyAlpha)
        || image.data.chunks_exact(4).any(|px| px[3] != 255)
}

fn part_of(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    output.with_file_name(name)
}

fn write_whole(output: &Path, bytes: &[u8]) -> Result<(), Fail> {
    let part = part_of(output);
    let done = std::fs::write(&part, bytes).and_then(|()| std::fs::rename(&part, output));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&part);
        return Err(about(output, e));
    }
    Ok(())
}

pub fn crop_png(
    input: &Path,
    output: &Path,
    crop: Crop,
    overwrite: bool,
    json: bool,
) -> Result<String, Fail> {
    let started = Instant::now();
    if super::plan::same_file(input, output) {
        return Err("input and output are the same file".into());
    }
    if output.exists() && !overwrite {
        return Err(format!(
            "{} already exists; use --force to replace it",
            output.display()
        )
        .into());
    }
    let data = std::fs::read(input).map_err(|e| about(input, e))?;
    let info = png::info(&data).map_err(|e| about(input, e))?;
    let image = png::decode(&data).map_err(|e| about(input, e))?;
    let cut = crop.apply(&image).map_err(|e| about(input, e))?;
    let bytes = Format::Png
        .encode(&cut, DEFAULT_QUALITY, see_through(info.color, &image))
        .map_err(|e| about(output, e))?;
    write_whole(output, &bytes)?;
    let elapsed = started.elapsed().as_secs_f64();
    Ok(if json {
        format!(
            "{{\"input\":{},\"output\":{},\"source\":{{\"width\":{},\"height\":{}}},\"crop\":{},\"seconds\":{elapsed:.3}}}",
            path_json(Some(input)),
            path_json(Some(output)),
            image.width,
            image.height,
            crop.json()
        )
    } else {
        format!(
            "{}x{} at {},{} of {}x{}  {}",
            cut.width,
            cut.height,
            crop.x,
            crop.y,
            image.width,
            image.height,
            output.display()
        )
    })
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
        None if job.file.is_dir() => Err(about(
            &job.file,
            "is a folder of PNG frames; give --fps for their rate and --out for the frames",
        )),
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
    pub sized: bool,
    pub every: Option<f64>,
    pub size: Option<Size>,
    pub force: bool,
    pub crop: Option<Crop>,
    pub hold: Option<f64>,
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

fn same_size(image: &Rgba, (w, h): (u32, u32)) -> Result<(), Fail> {
    if (image.width, image.height) != (w, h) {
        return Err(format!(
            "is {}x{} where the folder's first frame is {w}x{h}; every frame needs one size",
            image.width, image.height
        )
        .into());
    }
    Ok(())
}

fn crop_unscaled(range: &Range) -> Result<(), Fail> {
    if range.crop.is_some() && range.sized {
        return Err(
            "--crop cuts the frame as it is shown, unscaled; drop --size and --height".into(),
        );
    }
    Ok(())
}

fn held_frames(range: &Range) -> Result<u64, Fail> {
    let Some(hold) = range.hold else {
        return Ok(0);
    };
    let Some((num, den)) = range.fps else {
        return Err("--hold needs --fps, the rate it holds the last frame at".into());
    };
    if !(hold.is_finite() && hold > 0.0) {
        return Err(format!("--hold {hold} is not a time above zero").into());
    }
    let frames = (hold * f64::from(num) / f64::from(den.max(1)))
        .round()
        .max(1.0);
    Ok(if frames >= 9e15 {
        u64::MAX
    } else {
        frames as u64
    })
}

fn too_many(total: u64, from: f64, end: f64, force: bool) -> Result<(), Fail> {
    if total > MAX_RANGE && !force {
        return Err(format!(
            "{total} frames from {from:.3} s to {end:.3} s is more than {MAX_RANGE}; use --force to write them all"
        )
        .into());
    }
    Ok(())
}

fn hold_last(
    dir: &Path,
    files: &mut Vec<(u64, PathBuf, f64)>,
    held: u64,
    digits: usize,
    ext: &str,
) -> Result<(), Fail> {
    let Some((k, last, time)) = files.last().cloned() else {
        return Err(about(dir, "no frame was written to hold"));
    };
    for i in 1..=held {
        let n = k
            .checked_add(i)
            .ok_or("--hold runs past the last frame number")?;
        let path = dir.join(format!("{n:0digits$}.{ext}"));
        std::fs::copy(&last, &path).map_err(|e| about(&path, e))?;
        files.push((n, path, time));
    }
    Ok(())
}

fn finish(
    files: &mut Vec<(u64, PathBuf, f64)>,
    dir: &Path,
    held: u64,
    digits: usize,
    ext: &str,
) -> Result<(), Fail> {
    files.sort_by_key(|(k, _, _)| *k);
    let done = hold_last(dir, files, held, digits, ext);
    if done.is_err() {
        for (_, path, _) in files.iter() {
            let _ = std::fs::remove_file(path);
        }
    }
    done
}

struct Written {
    end: f64,
    slots: Slots,
    per_second: f64,
    rate: (u32, u32),
    size: (u32, u32),
    digits: usize,
    first_image: Option<f64>,
    count: u64,
    held: u64,
    files: Vec<(u64, PathBuf, f64)>,
}

fn report(
    out: &mut impl Write,
    job: &Job,
    range: &Range,
    dir: &Path,
    w: &Written,
    elapsed: f64,
) -> Result<(), Fail> {
    let total = w.count.saturating_add(w.held);
    let speed = if elapsed > 0.0 {
        total as f64 / elapsed
    } else {
        0.0
    };
    let last = w.files.last().map_or_else(
        || w.slots.at(total.saturating_sub(1)),
        |(k, _, _)| w.slots.at(*k),
    );
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    if job.json {
        let mut listed = String::new();
        for (i, (k, path, time)) in w.files.iter().enumerate() {
            if i > 0 {
                listed.push(',');
            }
            let _ = write!(
                listed,
                "{{\"index\":{k},\"slot\":{:.6},\"time\":{time:.6},\"path\":{}}}",
                w.slots.at(*k),
                path_json(Some(path))
            );
        }
        writeln!(
            out,
            "{{\"file\":{},\"from\":{:.6},\"to\":{:.6},\"first\":{:.6},\"last\":{last:.6},\"count\":{total},\"expected\":{},\"held\":{},\"fps\":{},\"rate\":{},\"every\":{},\"format\":\"{}\",\"quality\":{},\"width\":{},\"height\":{},\"crop\":{},\"dir\":{},\"digits\":{},\"first_png\":{},\"seconds\":{elapsed:.3},\"frames_per_second\":{speed:.1},\"files\":[{listed}]}}",
            path_json(Some(&job.file)),
            range.from,
            w.end,
            w.slots.at(0),
            w.slots.count,
            w.held,
            rate_text(w.per_second),
            match range.every {
                Some(_) => "null".to_string(),
                None => format!("\"{}/{}\"", w.rate.0, w.rate.1),
            },
            range
                .every
                .map_or("null".to_string(), |e| format!("{e:.6}")),
            job.format.name(),
            match job.format {
                Format::Png => "null".to_string(),
                _ => quality.to_string(),
            },
            w.size.0,
            w.size.1,
            range.crop.map_or("null".to_string(), Crop::json),
            path_json(Some(dir)),
            w.digits,
            w.first_image
                .map_or("null".to_string(), |t| format!("{t:.3}")),
        )?;
    } else {
        let cadence = match range.every {
            Some(every) => format!("one every {every:.3} s"),
            None => format!("at {} fps", rate_text(w.per_second)),
        };
        writeln!(
            out,
            "{total} {} frames, {}x{} {cadence}, {:.3} s to {last:.3} s, in {}",
            job.format.name(),
            w.size.0,
            w.size.1,
            w.slots.at(0),
            dir.display()
        )?;
        if w.held > 0 {
            writeln!(out, "the last frame held for {} more frames", w.held)?;
        }
        writeln!(
            out,
            "{elapsed:.2} s, {speed:.1} frames/s, the first image after {:.3} s",
            w.first_image.unwrap_or(elapsed)
        )?;
        if w.count < w.slots.count {
            writeln!(
                out,
                "note: the stream ends before its probed length: {} of {} frames written",
                w.count, w.slots.count
            )?;
        }
    }
    Ok(())
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn run_folder(out: &mut impl Write, job: &Job, range: &Range, dir: &Path) -> Result<(), Fail> {
    let started = Instant::now();
    crop_unscaled(range)?;
    let held = held_frames(range)?;
    if range.every.is_some() {
        return Err(about(
            &job.file,
            "is a folder of PNG frames, taken at its --fps rate; --every fits a video",
        ));
    }
    let Some(rate) = range.fps else {
        return Err(about(
            &job.file,
            "is a folder of PNG frames; give --fps for their rate",
        ));
    };
    if range.sized {
        return Err(about(
            &job.file,
            "is a folder of PNG frames, written at their own size; drop --size and --height",
        ));
    }
    let to = range.to.unwrap_or(f64::INFINITY);
    if to <= range.from {
        return Err(format!("--to {to} must come after --from {}", range.from).into());
    }
    let sources = source::listed(&job.file)?;
    let media = source::frames_of(&job.file, sources.clone(), Some(rate))?;
    let length = sources.len() as f64 * f64::from(rate.1) / f64::from(rate.0.max(1));
    if range.from >= length {
        return Err(about(
            &job.file,
            format!(
                "--from {} is at or past the end of the frames ({length:.3} s)",
                range.from
            ),
        ));
    }
    let end = to.min(length);
    let slots = Slots::of(range.from, end, rate, None);
    let first = u64::try_from(media::first_frame_at(range.from, rate)).unwrap_or(0);
    if slots.count == 0 {
        return Err(about(
            &job.file,
            format!("no frame starts between {:.3} s and {end:.3} s", range.from),
        ));
    }
    too_many(
        slots.count.saturating_add(held),
        range.from,
        end,
        range.force,
    )?;
    let size = match range.crop {
        Some(c) => {
            c.check((media.width, media.height))
                .map_err(|e| about(&job.file, e))?;
            (c.width, c.height)
        }
        None => (media.width, media.height),
    };
    if same_folder(dir, &job.file) {
        return Err(about(
            dir,
            "is the source folder; write the frames somewhere else",
        ));
    }
    ready(dir, range.force, job.format)?;
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    let ext = job.format.ext();
    let digits = slots
        .count
        .saturating_add(held)
        .to_string()
        .len()
        .max(NAME_DIGITS);
    let first_image: OnceLock<f64> = OnceLock::new();
    let made: Mutex<Vec<(u64, PathBuf, f64)>> = Mutex::new(Vec::new());
    let wrote = (0..slots.count)
        .into_par_iter()
        .try_for_each(|k| -> Result<(), Fail> {
            let index = first.saturating_add(k);
            let src = usize::try_from(index)
                .ok()
                .and_then(|i| sources.get(i))
                .ok_or_else(|| about(&job.file, format!("has no frame {index}")))?;
            let data = std::fs::read(src).map_err(|e| about(src, e))?;
            let image = png::decode(&data).map_err(|e| about(src, e))?;
            same_size(&image, (media.width, media.height)).map_err(|e| about(src, e))?;
            let alpha = media.alpha || image.data.chunks_exact(4).any(|px| px[3] != 255);
            let image = match range.crop {
                Some(c) => c.apply(&image).map_err(|e| about(src, e))?,
                None => image,
            };
            let bytes = job
                .format
                .encode(&image, quality, alpha)
                .map_err(|e| about(src, e))?;
            let path = dir.join(format!("{k:0digits$}.{ext}"));
            std::fs::write(&path, bytes).map_err(|e| about(&path, e))?;
            made.lock().unwrap_or_else(PoisonError::into_inner).push((
                k,
                path,
                index as f64 * f64::from(rate.1) / f64::from(rate.0.max(1)),
            ));
            let _ = first_image.set(started.elapsed().as_secs_f64());
            Ok(())
        });
    let mut files = made.into_inner().unwrap_or_else(PoisonError::into_inner);
    if let Err(e) = wrote {
        for (_, path, _) in files {
            let _ = std::fs::remove_file(path);
        }
        return Err(e);
    }
    finish(&mut files, dir, held, digits, ext)?;
    let written = Written {
        end,
        per_second: f64::from(rate.0) / f64::from(rate.1.max(1)),
        rate,
        size,
        digits,
        first_image: first_image.get().copied(),
        count: slots.count,
        held,
        files,
        slots,
    };
    report(
        out,
        job,
        range,
        dir,
        &written,
        started.elapsed().as_secs_f64(),
    )
}

fn run_range(out: &mut impl Write, job: &Job, range: &Range) -> Result<(), Fail> {
    let started = Instant::now();
    let dir = job
        .out
        .as_deref()
        .ok_or("a frame range writes its frames into --out")?;
    if job.file.is_dir() {
        return run_folder(out, job, range, dir);
    }
    crop_unscaled(range)?;
    let held = held_frames(range)?;
    let to = range.to.unwrap_or(f64::INFINITY);
    if to <= range.from {
        return Err(format!("--to {to} must come after --from {}", range.from).into());
    }
    let media = source::file(&job.file)?;
    if media.kind == source::Kind::Gif {
        return run_gif_range(out, job, range, &media, dir, started);
    }
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
    too_many(
        slots.count.saturating_add(held),
        range.from,
        end,
        range.force,
    )?;
    let size = match range.crop {
        Some(c) => {
            let whole = media.display_size();
            c.check(whole).map_err(|e| about(&job.file, e))?;
            whole
        }
        None => range_size(&media, range, job.height)?,
    };
    ready(dir, range.force, job.format)?;
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    let ext = job.format.ext();
    let digits = slots
        .count
        .saturating_add(held)
        .to_string()
        .len()
        .max(NAME_DIGITS);
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
                            let (image, alpha) = match (see_through, &mut fix) {
                                (Some(a), _) => (a.apply(&frame, plane.as_ref())?, true),
                                (None, Some(fix)) => (fix.apply(frame)?, false),
                                (None, None) => return Err("no RGBA converter".into()),
                            };
                            let image = match range.crop {
                                Some(c) => c.apply(&image)?,
                                None => image,
                            };
                            let data = job
                                .format
                                .encode(&image, quality, alpha)
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
    finish(&mut files, dir, held, digits, ext)?;
    let shape = range.crop.map_or(size, |c| (c.width, c.height));
    let written = Written {
        end,
        slots,
        per_second,
        rate,
        size: shape,
        digits,
        first_image: first_png.get().copied(),
        count,
        held,
        files,
    };
    report(
        out,
        job,
        range,
        dir,
        &written,
        started.elapsed().as_secs_f64(),
    )
}

fn first_where(total: u64, at: impl Fn(u64) -> bool) -> u64 {
    let (mut lo, mut hi) = (0, total);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if at(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

const GIF_BATCH_BYTES: u64 = 64 << 20;

fn gif_batch(threads: usize, size: (u32, u32)) -> usize {
    let frame = (u64::from(size.0) * u64::from(size.1) * 4).max(1);
    let fits = usize::try_from(GIF_BATCH_BYTES / frame).unwrap_or(usize::MAX);
    (threads.clamp(2, 32) * 2).min(fits).max(2)
}

fn gif_spans(
    info: &media::GifInfo,
    (num, den): (u32, u32),
    from: f64,
    to: f64,
) -> (std::ops::Range<u64>, Vec<f64>) {
    let total = info
        .ticks(Some((num, den)))
        .iter()
        .map(|s| s.end)
        .max()
        .unwrap_or(0);
    let time = |k: u64| k as f64 * f64::from(den) / f64::from(num.max(1));
    let first = first_where(total, |k| time(k) >= from - 1e-9);
    let end = first_where(total, |k| time(k) >= to - 1e-9).max(first);
    let times = [first, end.saturating_sub(1)].map(time).to_vec();
    (first..end, times)
}

struct GifWritten {
    paths: Vec<(u64, PathBuf)>,
    failed: Option<Fail>,
}

struct GifOut<'a> {
    dir: &'a Path,
    size: (u32, u32),
    first: u64,
    digits: usize,
    format: Format,
    quality: u8,
}

fn write_gif_batch(
    batch: &mut Vec<(Rgba, std::ops::Range<u64>)>,
    to: &GifOut,
    written: &mut GifWritten,
) {
    let wrote: Vec<GifWritten> = batch
        .par_drain(..)
        .map(|(mut img, ticks)| {
            let mut w = GifWritten {
                paths: Vec::new(),
                failed: None,
            };
            media::flatten_alpha(&mut img);
            if (img.width, img.height) != to.size {
                img = media::scale(&img, to.size.0, to.size.1);
            }
            let data = match to.format.encode(&img, to.quality, false) {
                Ok(data) => data,
                Err(e) => {
                    w.failed = Some(about(to.dir, e));
                    return w;
                }
            };
            for k in ticks {
                let path = to.dir.join(format!(
                    "{:0digits$}.{}",
                    k - to.first,
                    to.format.ext(),
                    digits = to.digits
                ));
                if let Err(e) = std::fs::write(&path, &data) {
                    w.failed = Some(about(&path, e));
                    return w;
                }
                w.paths.push((k, path));
            }
            w
        })
        .collect();
    for w in wrote {
        written.paths.extend(w.paths);
        if let Some(e) = w.failed {
            written.failed.get_or_insert(e);
        }
    }
}

fn undo(written: GifWritten, read: Result<(), Fail>) -> Result<Vec<(u64, PathBuf)>, Fail> {
    match read.err().or(written.failed) {
        Some(e) => {
            for (_, path) in written.paths {
                let _ = std::fs::remove_file(path);
            }
            Err(e)
        }
        None => Ok(written.paths),
    }
}

fn every_rate(every: f64) -> (u32, u32) {
    let ms = (every * 1000.0).round().clamp(1.0, f64::from(u32::MAX));
    (1000, ms as u32)
}

fn run_gif_range(
    out: &mut impl Write,
    job: &Job,
    range: &Range,
    media: &Media,
    dir: &Path,
    started: Instant,
) -> Result<(), Fail> {
    if range.crop.is_some() || range.hold.is_some() {
        return Err(about(
            &job.file,
            "is a GIF; --crop and --hold take a video or a folder of PNG frames",
        ));
    }
    let data = std::fs::read(&job.file).map_err(|e| about(&job.file, e))?;
    let info = media::scan_gif(&data).map_err(|e| about(&job.file, e))?;
    let rate = match range.every {
        Some(every) => every_rate(every),
        None => range.fps.unwrap_or(media.fps),
    };
    let to = range.to.unwrap_or(f64::INFINITY);
    let (kept, times) = gif_spans(&info, rate, range.from, to);
    let count = kept.end - kept.start;
    if count == 0 {
        return Err(about(
            &job.file,
            format!(
                "no frame starts between {:.3} s and {:.3} s",
                range.from,
                to.min(info.duration())
            ),
        ));
    }
    if count > MAX_RANGE && !range.force {
        return Err(format!(
            "{count} frames from {:.3} s is more than {MAX_RANGE}; use --force to write them all",
            range.from
        )
        .into());
    }
    let size = if range.sized {
        range_size(media, range, job.height)?
    } else {
        (media.width, media.height)
    };
    ready(dir, range.force, job.format)?;
    let quality = job.quality.unwrap_or(DEFAULT_QUALITY);
    let target = GifOut {
        dir,
        size,
        first: kept.start,
        digits: count.to_string().len().max(NAME_DIGITS),
        format: job.format,
        quality,
    };
    let batch_size = gif_batch(rayon::current_num_threads(), (info.width, info.height));
    let mut batch = Vec::with_capacity(batch_size);
    let mut written = GifWritten {
        paths: Vec::new(),
        failed: None,
    };
    let mut first_png = None;
    let read = media::decode_scanned(&data, &info, Some(rate), |f| -> Result<(), Fail> {
        let ticks = f.ticks.start.max(kept.start)..f.ticks.end.min(kept.end);
        if !ticks.is_empty() {
            batch.push((f.image.clone(), ticks));
        }
        if batch.len() >= batch_size {
            write_gif_batch(&mut batch, &target, &mut written);
            first_png.get_or_insert(started.elapsed().as_secs_f64());
        }
        match written.failed.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    });
    if read.is_ok() && !batch.is_empty() {
        write_gif_batch(&mut batch, &target, &mut written);
        first_png.get_or_insert(started.elapsed().as_secs_f64());
    }
    let mut paths = undo(written, read)?;
    paths.sort_by_key(|(k, _)| *k);
    let step = f64::from(rate.1) / f64::from(rate.0.max(1));
    let files: Vec<(u64, PathBuf, f64)> = paths
        .into_iter()
        .map(|(k, path)| (k - kept.start, path, k as f64 * step))
        .collect();
    let written = Written {
        end: to.min(info.duration()),
        slots: Slots {
            first: times[0],
            step,
            count,
        },
        per_second: range
            .every
            .map_or(f64::from(rate.0) / f64::from(rate.1.max(1)), |e| 1.0 / e),
        rate,
        size,
        digits: target.digits,
        first_image: first_png,
        count: files.len() as u64,
        held: 0,
        files,
    };
    report(
        out,
        job,
        range,
        dir,
        &written,
        started.elapsed().as_secs_f64(),
    )
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
            sized: false,
            every: None,
            size: None,
            force: false,
            crop: None,
            hold: None,
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
            sized: false,
            every,
            size: None,
            force: false,
            crop: None,
            hold: None,
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
    fn placed() -> Rgba {
        let mut img = Rgba::new(40, 30);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % 40) as u8, (i / 40) as u8);
            px.copy_from_slice(&[x, y, x ^ y, 200]);
        }
        img
    }

    fn crop(width: u32, height: u32, x: u32, y: u32) -> Crop {
        Crop {
            width,
            height,
            x,
            y,
        }
    }

    fn gif(delays: &[u16]) -> media::GifInfo {
        media::GifInfo {
            width: 4,
            height: 4,
            loop_count: None,
            delays_cs: delays.to_vec(),
            transparent: false,
        }
    }

    #[test]
    fn a_hold_after_a_stream_that_ends_early_reports_its_last_file() {
        let slots = Slots::of(0.0, 1.0, (4, 1), None);
        assert_eq!(slots.count, 4);
        let files: Vec<(u64, PathBuf, f64)> = (0..4)
            .map(|k| (k, PathBuf::from(format!("{k:06}.png")), 0.25))
            .collect();
        let w = Written {
            end: 1.0,
            slots,
            per_second: 4.0,
            rate: (4, 1),
            size: (8, 8),
            digits: 6,
            first_image: Some(0.01),
            count: 2,
            held: 2,
            files,
        };
        let range = held_range(Some((4, 1)), Some(0.5));
        let mut plain = Vec::new();
        report(
            &mut plain,
            &job(Format::Png, None),
            &range,
            Path::new("out"),
            &w,
            1.0,
        )
        .unwrap();
        let plain = String::from_utf8(plain).unwrap();
        assert!(
            plain.starts_with("4 png frames, 8x8 at 4 fps, 0.000 s to 0.750 s"),
            "{plain}"
        );
        assert!(
            plain.contains("the last frame held for 2 more frames"),
            "{plain}"
        );
        assert!(plain.contains("2 of 4 frames written"), "{plain}");
        let mut json = Vec::new();
        let mut j = job(Format::Png, None);
        j.json = true;
        report(&mut json, &j, &range, Path::new("out"), &w, 1.0).unwrap();
        let json = String::from_utf8(json).unwrap();
        assert!(
            json.contains("\"last\":0.750000,\"count\":4,\"expected\":4,\"held\":2"),
            "{json}"
        );
        let mut gap = w;
        for (file, k) in gap.files.iter_mut().zip([0, 2, 3, 4]) {
            file.0 = k;
        }
        let mut gapped = Vec::new();
        report(&mut gapped, &j, &range, Path::new("out"), &gap, 1.0).unwrap();
        let gapped = String::from_utf8(gapped).unwrap();
        assert!(gapped.contains("\"last\":1.000000"), "{gapped}");
    }

    struct Short(usize);

    impl std::io::Write for Short {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Err(std::io::Error::other("the reader went away"));
            }
            let n = buf.len().min(self.0);
            self.0 -= n;
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_report_that_cannot_be_written_fails_and_a_zero_time_reads_as_no_speed() {
        let w = || Written {
            end: 1.0,
            slots: Slots::of(0.0, 1.0, (4, 1), None),
            per_second: 4.0,
            rate: (4, 1),
            size: (8, 8),
            digits: 6,
            first_image: None,
            count: 2,
            held: 2,
            files: Vec::new(),
        };
        let range = held_range(Some((4, 1)), Some(0.5));
        let mut j = job(Format::Png, None);
        for json in [false, true] {
            j.json = json;
            let mut full = Vec::new();
            report(&mut full, &j, &range, Path::new("out"), &w(), 0.0).unwrap();
            let text = String::from_utf8(full.clone()).unwrap();
            if json {
                assert!(text.contains("\"last\":0.750000"), "{text}");
                assert!(text.contains("\"frames_per_second\":0.0"), "{text}");
                assert!(text.contains("\"files\":[]"), "{text}");
            } else {
                assert_eq!(text.lines().count(), 4, "{text}");
                assert!(text.contains("0.00 s, 0.0 frames/s"), "{text}");
                assert!(text.contains("the first image after 0.000 s"), "{text}");
            }
            for cut in 0..full.len() {
                let got = report(&mut Short(cut), &j, &range, Path::new("out"), &w(), 0.0);
                assert!(got.is_err(), "json {json}, cut at {cut} of {}", full.len());
            }
            report(
                &mut Short(full.len()),
                &j,
                &range,
                Path::new("out"),
                &w(),
                0.0,
            )
            .unwrap();
        }
    }

    fn held_range(fps: Option<(u32, u32)>, hold: Option<f64>) -> Range {
        Range {
            from: 0.0,
            to: None,
            fps,
            sized: false,
            every: None,
            size: None,
            force: false,
            crop: None,
            hold,
        }
    }

    #[test]
    fn a_crop_reads_width_height_x_and_y() {
        assert_eq!(crop_of("640:360:10:20").unwrap(), crop(640, 360, 10, 20));
        assert_eq!(crop_of(" 1:2:0:0 ").unwrap(), crop(1, 2, 0, 0));
        assert_eq!(crop_of("0:0:0:0").unwrap(), crop(0, 0, 0, 0));
        for bad in [
            "",
            "640:360",
            "640:360:0",
            "640:360:0:0:0",
            "a:b:c:d",
            "-1:2:3:4",
            "1.5:2:3:4",
            "640x360+0+0",
        ] {
            let e = crop_of(bad).unwrap_err();
            assert!(e.contains("is not a crop such as"), "{bad}: {e}");
        }
    }

    #[test]
    fn a_crop_is_the_same_rectangle_of_the_full_frame_pixel_for_pixel() {
        let full = placed();
        let c = crop(7, 5, 30, 20);
        let cut = c.apply(&full).unwrap();
        assert_eq!((cut.width, cut.height), (7, 5));
        for y in 0..5 {
            for x in 0..7 {
                let a = &cut.data[(y * 7 + x) * 4..][..4];
                let b = &full.data[((y + 20) * 40 + x + 30) * 4..][..4];
                assert_eq!(a, b, "{x},{y}");
            }
        }
        assert_eq!(crop(40, 30, 0, 0).apply(&full).unwrap().data, full.data);
        let corner = crop(1, 1, 39, 29).apply(&full).unwrap();
        assert_eq!(corner.data, vec![39, 29, 39 ^ 29, 200]);
    }

    #[test]
    fn a_crop_outside_the_frame_or_with_a_zero_side_is_refused_in_one_line() {
        let full = placed();
        for (c, words) in [
            (crop(41, 1, 0, 0), "falls outside the 40x30 frame"),
            (crop(1, 31, 0, 0), "falls outside the 40x30 frame"),
            (crop(10, 10, 31, 0), "falls outside the 40x30 frame"),
            (crop(10, 10, 0, 21), "falls outside the 40x30 frame"),
            (crop(1, 1, u32::MAX, 0), "falls outside the 40x30 frame"),
            (crop(u32::MAX, 1, 1, 0), "falls outside the 40x30 frame"),
            (crop(0, 10, 0, 0), "needs a width and height above zero"),
            (crop(10, 0, 0, 0), "needs a width and height above zero"),
        ] {
            let e = c.apply(&full).unwrap_err().to_string();
            assert!(e.contains(words), "{c:?}: {e}");
            assert!(e.starts_with("--crop "), "{e}");
            assert!(!e.contains('\n'), "{e}");
        }
        assert!(crop(10, 10, 30, 20).check((40, 30)).is_ok());
    }

    #[test]
    fn a_crop_refuses_a_frame_shorter_than_its_size() {
        let mut short = placed();
        short.data.truncate(40 * 4 * 10);
        assert!(crop(5, 5, 0, 20).apply(&short).is_err());
    }

    #[test]
    fn a_hold_is_its_length_at_the_fps_rate() {
        assert_eq!(
            held_frames(&held_range(Some((30, 1)), Some(2.0))).unwrap(),
            60
        );
        assert_eq!(
            held_frames(&held_range(Some((24, 1)), Some(0.5))).unwrap(),
            12
        );
        assert_eq!(
            held_frames(&held_range(Some((30000, 1001)), Some(1.0))).unwrap(),
            30
        );
        assert_eq!(
            held_frames(&held_range(Some((30, 1)), Some(0.001))).unwrap(),
            1
        );
        assert_eq!(held_frames(&held_range(Some((30, 1)), None)).unwrap(), 0);
        assert_eq!(held_frames(&held_range(None, None)).unwrap(), 0);
        assert_eq!(
            held_frames(&held_range(Some((240, 1)), Some(1e300))).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn a_hold_without_a_rate_or_a_time_is_refused() {
        let e = held_frames(&held_range(None, Some(2.0)))
            .unwrap_err()
            .to_string();
        assert!(e.contains("--hold needs --fps"), "{e}");
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let e = held_frames(&held_range(Some((30, 1)), Some(bad)))
                .unwrap_err()
                .to_string();
            assert!(e.contains("is not a time above zero"), "{bad}: {e}");
        }
    }

    #[test]
    fn a_crop_refuses_a_scale() {
        let mut r = held_range(Some((30, 1)), None);
        r.crop = Some(crop(10, 10, 0, 0));
        assert!(crop_unscaled(&r).is_ok());
        r.sized = true;
        assert!(
            crop_unscaled(&r)
                .unwrap_err()
                .to_string()
                .contains("drop --size and --height")
        );
        r.crop = None;
        assert!(crop_unscaled(&r).is_ok());
        assert_eq!(held_frames(&r).unwrap(), 0);
    }

    #[test]
    fn too_many_frames_need_force() {
        assert!(too_many(MAX_RANGE, 0.0, 1.0, false).is_ok());
        assert!(too_many(MAX_RANGE + 1, 0.0, 1.0, false).is_err());
        assert!(too_many(MAX_RANGE + 1, 0.0, 1.0, true).is_ok());
    }

    #[test]
    fn a_hold_copies_the_last_frame_numbered_on_and_cleans_up_on_failure() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-stills-hold");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut files = Vec::new();
        for (k, body) in [(1u64, b"b"), (0, b"a")] {
            let path = dir.join(format!("{k:06}.png"));
            std::fs::write(&path, body).unwrap();
            files.push((k, path, k as f64));
        }
        finish(&mut files, &dir, 3, 6, "png").unwrap();
        let ks: Vec<u64> = files.iter().map(|(k, _, _)| *k).collect();
        assert_eq!(ks, vec![0, 1, 2, 3, 4]);
        for k in 2..=4 {
            assert_eq!(
                std::fs::read(dir.join(format!("{k:06}.png"))).unwrap(),
                b"b"
            );
            assert_eq!(files[k as usize].2, 1.0);
        }
        assert!(!dir.join("000005.png").exists());
        let mut none = Vec::new();
        assert!(finish(&mut none, &dir, 2, 6, "png").is_err());
        let mut gone = vec![
            (0u64, dir.join("000000.png"), 0.0),
            (9u64, dir.join("missing.png"), 0.0),
        ];
        assert!(finish(&mut gone, &dir, 1, 6, "png").is_err());
        assert!(!dir.join("000000.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_is_never_its_own_output() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-stills-same");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).unwrap();
        assert!(same_folder(&dir.join("in"), &dir.join("in/../in")));
        assert!(!same_folder(&dir.join("in"), &dir));
        assert!(!same_folder(&dir.join("nowhere"), &dir.join("nowhere")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transparency_is_kept_from_the_colour_type_or_the_pixels() {
        let mut img = Rgba::new(2, 1);
        img.data.fill(255);
        assert!(!see_through(png::Color::Rgb, &img));
        assert!(!see_through(png::Color::Palette, &img));
        assert!(see_through(png::Color::Rgba, &img));
        assert!(see_through(png::Color::GreyAlpha, &img));
        img.data[7] = 0;
        assert!(see_through(png::Color::Palette, &img));
        assert!(see_through(png::Color::Rgb, &img));
        assert!(see_through(png::Color::Grey, &img));
    }

    #[test]
    fn a_whole_write_leaves_no_part_behind() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-stills-whole");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.png");
        assert_eq!(part_of(&out), dir.join("out.png.part"));
        write_whole(&out, b"abc").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"abc");
        assert!(!part_of(&out).exists());
        std::fs::create_dir_all(part_of(&dir.join("dir.png"))).unwrap();
        assert!(write_whole(&dir.join("dir.png"), b"abc").is_err());
        assert!(!dir.join("dir.png").exists());
        let missing = dir.join("nowhere/out.png");
        let e = write_whole(&missing, b"abc").unwrap_err().to_string();
        assert!(e.contains("out.png"), "{e}");
        assert!(!part_of(&missing).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_frame_of_another_size_is_refused() {
        let img = Rgba::new(4, 2);
        assert!(same_size(&img, (4, 2)).is_ok());
        let e = same_size(&img, (4, 3)).unwrap_err().to_string();
        assert!(
            e.contains("is 4x2 where the folder's first frame is 4x3"),
            "{e}"
        );
        assert!(same_size(&img, (2, 4)).is_err());
    }

    #[test]
    fn first_where_finds_the_first_true_tick() {
        assert_eq!(first_where(10, |k| k >= 4), 4);
        assert_eq!(first_where(10, |_| true), 0);
        assert_eq!(first_where(10, |_| false), 10);
        assert_eq!(first_where(0, |_| true), 0);
    }

    #[test]
    fn gif_spans_cover_every_tick_at_a_rate() {
        let (kept, times) = gif_spans(&gif(&[7, 10, 13]), (30, 1), 0.0, f64::INFINITY);
        assert_eq!(kept, 0..9);
        assert!((times[0] - 0.0).abs() < 1e-12);
        assert!((times[1] - 8.0 / 30.0).abs() < 1e-12);
    }

    #[test]
    fn gif_spans_keep_the_ticks_from_from_up_to_to() {
        let info = gif(&[7, 10, 13]);
        assert_eq!(gif_spans(&info, (30, 1), 0.1, f64::INFINITY).0, 3..9);
        assert_eq!(gif_spans(&info, (30, 1), 0.0, 0.2).0, 0..6);
        assert_eq!(gif_spans(&info, (30, 1), 0.05, 0.2).0, 2..6);
    }

    #[test]
    fn gif_spans_past_the_end_keep_nothing() {
        let info = gif(&[7, 10, 13]);
        let (kept, _) = gif_spans(&info, (10, 1), 0.5, f64::INFINITY);
        assert!(kept.is_empty());
    }

    #[test]
    fn a_gif_batch_stays_under_its_byte_cap() {
        assert_eq!(gif_batch(16, (16, 16)), 32);
        assert_eq!(gif_batch(1, (16, 16)), 4);
        assert_eq!(gif_batch(64, (16, 16)), 64);
        assert_eq!(gif_batch(32, (1280, 800)), 16);
        assert!(gif_batch(32, (1280, 800)) as u64 * 1280 * 800 * 4 <= GIF_BATCH_BYTES);
        assert_eq!(gif_batch(32, (8192, 8192)), 2);
    }

    fn gif_dir(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/unit-stills-gif")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(dir: &Path, size: (u32, u32)) -> GifOut<'_> {
        GifOut {
            dir,
            size,
            first: 10,
            digits: 6,
            format: Format::Png,
            quality: 85,
        }
    }

    fn unwritten() -> GifWritten {
        GifWritten {
            paths: Vec::new(),
            failed: None,
        }
    }

    #[test]
    fn a_gif_batch_writes_each_tick_once_named_from_the_first() {
        let dir = gif_dir("batch");
        let mut batch = vec![(checker(), 10..12), (checker(), 12..13)];
        let mut written = unwritten();
        write_gif_batch(&mut batch, &target(&dir, (32, 24)), &mut written);
        assert!(batch.is_empty());
        assert!(written.failed.is_none());
        let mut names: Vec<_> = written
            .paths
            .iter()
            .map(|(_, p)| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["000000.png", "000001.png", "000002.png"]);
        let back = png::decode(&std::fs::read(&written.paths[0].1).unwrap()).unwrap();
        assert_eq!((back.width, back.height), (32, 24));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gif_batch_into_a_missing_folder_fails_and_keeps_no_path() {
        let dir = gif_dir("missing").join("gone");
        let mut batch = vec![(checker(), 10..14)];
        let mut written = unwritten();
        write_gif_batch(&mut batch, &target(&dir, (64, 48)), &mut written);
        assert!(written.paths.is_empty());
        let e = written.failed.unwrap().to_string();
        assert!(
            e.starts_with(&dir.join("000000.png").display().to_string()),
            "{e}"
        );
    }

    #[test]
    fn a_gif_batch_stops_at_the_first_tick_it_cannot_write() {
        let dir = gif_dir("blocked");
        std::fs::create_dir(dir.join("000003.png")).unwrap();
        let mut batch = vec![(checker(), 10..16)];
        let mut written = unwritten();
        write_gif_batch(&mut batch, &target(&dir, (64, 48)), &mut written);
        let names: Vec<_> = written
            .paths
            .iter()
            .map(|(_, p)| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["000000.png", "000001.png", "000002.png"]);
        let e = written.failed.unwrap().to_string();
        assert!(e.contains("000003.png"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gif_frame_that_cannot_encode_fails_the_batch_naming_the_folder() {
        let dir = gif_dir("empty-frame");
        let mut batch = vec![(Rgba::new(0, 0), 10..11)];
        let mut written = unwritten();
        write_gif_batch(&mut batch, &target(&dir, (0, 0)), &mut written);
        assert!(written.paths.is_empty());
        let e = written.failed.unwrap().to_string();
        assert!(e.starts_with(&dir.display().to_string()), "{e}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gif_batch_writes_jpeg_and_webp_when_asked() {
        let dir = gif_dir("formats");
        for (format, magic) in [
            (Format::Jpeg, &b"\xff\xd8\xff"[..]),
            (Format::Webp, b"RIFF"),
        ] {
            let mut batch = vec![(checker(), 10..11)];
            let mut written = unwritten();
            let to = GifOut {
                format,
                ..target(&dir, (64, 48))
            };
            write_gif_batch(&mut batch, &to, &mut written);
            assert!(written.failed.is_none());
            let path = dir.join(format!("000000.{}", format.ext()));
            assert_eq!(written.paths, [(10, path.clone())]);
            assert!(std::fs::read(&path).unwrap().starts_with(magic));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_gif_range_removes_every_frame_it_wrote() {
        let dir = gif_dir("undo");
        let paths: Vec<PathBuf> = (0..3).map(|k| dir.join(format!("{k:06}.png"))).collect();
        for p in &paths {
            std::fs::write(p, b"x").unwrap();
        }
        std::fs::write(dir.join("keep.txt"), b"x").unwrap();
        let written = GifWritten {
            paths: paths
                .iter()
                .cloned()
                .enumerate()
                .map(|(k, p)| (k as u64, p))
                .collect(),
            failed: Some("disk full".into()),
        };
        assert_eq!(undo(written, Ok(())).unwrap_err().to_string(), "disk full");
        assert!(paths.iter().all(|p| !p.exists()));
        for p in &paths {
            std::fs::write(p, b"x").unwrap();
        }
        let written = GifWritten {
            paths: paths
                .iter()
                .cloned()
                .enumerate()
                .map(|(k, p)| (k as u64, p))
                .collect(),
            failed: Some("later".into()),
        };
        let e = undo(written, Err("the GIF ends early".into())).unwrap_err();
        assert_eq!(e.to_string(), "the GIF ends early");
        assert!(paths.iter().all(|p| !p.exists()));
        assert!(dir.join("keep.txt").exists());
        let kept = GifWritten {
            paths: vec![(0, dir.join("keep.txt"))],
            failed: None,
        };
        assert_eq!(undo(kept, Ok(())).unwrap(), [(0, dir.join("keep.txt"))]);
        assert!(dir.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_interval_becomes_a_tick_rate_in_milliseconds() {
        assert_eq!(every_rate(2.0), (1000, 2000));
        assert_eq!(every_rate(0.5), (1000, 500));
        assert_eq!(every_rate(1.0 / 3.0), (1000, 333));
        assert_eq!(every_rate(0.0001), (1000, 1));
        assert_eq!(every_rate(1e12), (1000, u32::MAX));
    }

    fn reported(
        every: Option<f64>,
        files: Vec<(u64, PathBuf, f64)>,
        expected: u64,
        json: bool,
    ) -> String {
        let mut range = held_range(Some((5, 1)), None);
        range.every = every;
        let w = Written {
            end: 1.0,
            slots: Slots {
                first: 0.0,
                step: 0.2,
                count: expected,
            },
            per_second: every.map_or(5.0, |e| 1.0 / e),
            rate: (5, 1),
            size: (64, 48),
            digits: 6,
            first_image: None,
            count: files.len() as u64,
            held: 0,
            files,
        };
        let mut j = job(Format::Png, None);
        j.file = PathBuf::from("in.gif");
        j.json = json;
        let mut out = Vec::new();
        report(&mut out, &j, &range, Path::new("out"), &w, 0.5).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_report_lists_each_file_and_nulls_what_was_not_asked_or_done() {
        let files = vec![
            (0, PathBuf::from("out/000000.png"), 0.01),
            (1, PathBuf::from("out/000001.png"), 0.21),
        ];
        let v: serde_json::Value =
            serde_json::from_str(&reported(None, files.clone(), 2, true)).unwrap();
        assert_eq!(v["file"], "in.gif");
        assert_eq!(v["count"], 2);
        assert_eq!(v["expected"], 2);
        assert_eq!(v["held"], 0);
        assert_eq!(v["rate"], "5/1");
        assert_eq!(v["every"], serde_json::Value::Null);
        assert_eq!(v["quality"], serde_json::Value::Null);
        assert_eq!(v["crop"], serde_json::Value::Null);
        assert_eq!(v["first_png"], serde_json::Value::Null);
        assert_eq!(v["last"], 0.2);
        assert_eq!(v["frames_per_second"], 4.0);
        let listed = v["files"].as_array().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1]["index"], 1);
        assert_eq!(listed[1]["slot"], 0.2);
        assert_eq!(listed[1]["time"], 0.21);
        assert_eq!(listed[1]["path"], "out/000001.png");
        let v: serde_json::Value =
            serde_json::from_str(&reported(Some(0.2), files, 2, true)).unwrap();
        assert_eq!(v["rate"], serde_json::Value::Null);
        assert_eq!(v["every"], 0.2);
    }

    #[test]
    fn a_report_in_words_notes_a_short_stream() {
        let files = vec![(0, PathBuf::from("out/000000.png"), 0.0)];
        let said = reported(Some(0.2), files.clone(), 1, false);
        assert_eq!(
            said,
            "1 png frames, 64x48 one every 0.200 s, 0.000 s to 0.000 s, in out\n0.50 s, 2.0 frames/s, the first image after 0.500 s\n"
        );
        let said = reported(None, files, 3, false);
        assert!(said.contains("64x48 at 5 fps"), "{said}");
        assert!(
            said.ends_with(
                "note: the stream ends before its probed length: 1 of 3 frames written\n"
            ),
            "{said}"
        );
    }
}
