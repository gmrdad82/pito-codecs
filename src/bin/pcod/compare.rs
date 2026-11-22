use std::io::Write;
use std::path::{Path, PathBuf};

use pito_codecs::media::{self, Prefs, Rgba, Video};
use pito_codecs::png;

use super::json::Json;
use super::pipeline::ToRgba;
use super::source::{self, Kind, Media};
use super::{Fail, about};

const AMPLIFY: u32 = 16;

pub struct Job {
    pub a: PathBuf,
    pub b: PathBuf,
    pub fps: Option<(u32, u32)>,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub tolerance: u8,
    pub worst: usize,
    pub diff: Option<PathBuf>,
    pub prefs: Prefs,
    pub json: bool,
}

enum Feed {
    Video {
        video: Video,
        fix: Box<ToRgba>,
        first: i64,
        end: Option<i64>,
        done: bool,
    },
    Folder {
        files: Vec<PathBuf>,
        at: usize,
        end: usize,
    },
}

struct Side {
    path: PathBuf,
    media: Media,
    size: (u32, u32),
    feed: Feed,
}

struct Picture {
    time: f64,
    image: Rgba,
}

fn seconds((num, den): (u32, u32)) -> f64 {
    f64::from(den) / f64::from(num.max(1))
}

impl Side {
    fn open(path: &Path, job: &Job) -> Result<Side, Fail> {
        let media = source::open(path, job.fps)?;
        if media.kind == Kind::Gif {
            return Err(about(
                path,
                "is a GIF; compare reads video files and folders of PNG frames",
            ));
        }
        if media.fps.0 == 0 || media.fps.1 == 0 {
            return Err(about(path, "has no frame rate"));
        }
        let size = media.display_size();
        let rate = media.fps;
        let from = job.from.unwrap_or(0.0);
        let first = media::first_frame_at(from, rate).max(0);
        let end = job.to.map(|to| media::first_frame_at(to, rate).max(0));
        let feed = match &media.kind {
            Kind::Frames(files) => {
                let first = usize::try_from(first).unwrap_or(usize::MAX);
                let end = end
                    .map_or(files.len(), |e| usize::try_from(e).unwrap_or(usize::MAX))
                    .min(files.len());
                if first >= files.len() {
                    return Err(about(
                        path,
                        format!("--from {from} is at or past the end of the frames"),
                    ));
                }
                Feed::Folder {
                    files: files.clone(),
                    at: first,
                    end,
                }
            }
            _ => {
                let lead = (first as f64 * seconds(rate) - seconds(rate)).max(0.0);
                let video = media::decode_video(path, lead, None, &job.prefs)
                    .map_err(|e| about(path, e))?;
                Feed::Video {
                    video,
                    fix: Box::new(ToRgba::new(&media, size)),
                    first,
                    end,
                    done: false,
                }
            }
        };
        Ok(Side {
            path: path.to_path_buf(),
            media,
            size,
            feed,
        })
    }

    fn next(&mut self) -> Result<Option<Picture>, Fail> {
        let rate = self.media.fps;
        match &mut self.feed {
            Feed::Video {
                video,
                fix,
                first,
                end,
                done,
            } => {
                while !*done {
                    let Some(frame) = video.next().transpose().map_err(|e| about(&self.path, e))?
                    else {
                        *done = true;
                        break;
                    };
                    let time = frame.time_base.seconds(frame.pts);
                    let index = (time / seconds(rate)).round() as i64;
                    if index < *first {
                        continue;
                    }
                    if end.is_some_and(|e| index >= e) {
                        *done = true;
                        break;
                    }
                    return Ok(Some(Picture {
                        time,
                        image: fix.apply(frame)?,
                    }));
                }
                Ok(None)
            }
            Feed::Folder { files, at, end } => {
                if *at >= *end {
                    return Ok(None);
                }
                let file = &files[*at];
                let data = std::fs::read(file).map_err(|e| about(file, e))?;
                let image = png::decode(&data).map_err(|e| about(file, e))?;
                if (image.width, image.height) != self.size {
                    return Err(about(
                        file,
                        format!(
                            "is {}x{} where the folder's first frame is {}x{}; every frame needs one size",
                            image.width, image.height, self.size.0, self.size.1
                        ),
                    ));
                }
                let time = *at as f64 * seconds(rate);
                *at += 1;
                Ok(Some(Picture { time, image }))
            }
        }
    }

    fn rest(&mut self) -> Result<u64, Fail> {
        if let Feed::Folder { at, end, .. } = &mut self.feed {
            let n = end.saturating_sub(*at) as u64;
            *at = *end;
            return Ok(n);
        }
        let mut n = 0;
        while let Feed::Video {
            video,
            done,
            end,
            first,
            ..
        } = &mut self.feed
        {
            if *done {
                break;
            }
            let Some(frame) = video.next().transpose().map_err(|e| about(&self.path, e))? else {
                break;
            };
            let index =
                (frame.time_base.seconds(frame.pts) / seconds(self.media.fps)).round() as i64;
            if end.is_some_and(|e| index >= e) {
                break;
            }
            if index >= *first {
                n += 1;
            }
        }
        Ok(n)
    }
}

struct Frame {
    number: u64,
    time: f64,
    squared: u64,
    sum: [u64; 3],
    peak: [u8; 3],
    beyond: u64,
}

fn measure(a: &Rgba, b: &Rgba, tolerance: u8) -> (u64, [u64; 3], [u8; 3], u64) {
    let (mut squared, mut sum, mut peak, mut beyond) = (0u64, [0u64; 3], [0u8; 3], 0u64);
    for (p, q) in a.data.chunks_exact(4).zip(b.data.chunks_exact(4)) {
        let mut over = false;
        for c in 0..3 {
            let d = p[c].abs_diff(q[c]);
            squared += u64::from(d) * u64::from(d);
            sum[c] += u64::from(d);
            peak[c] = peak[c].max(d);
            over |= d > tolerance;
        }
        beyond += u64::from(over);
    }
    (squared, sum, peak, beyond)
}

fn psnr(mean_squared: f64) -> Option<f64> {
    (mean_squared > 0.0).then(|| 10.0 * (255.0f64 * 255.0 / mean_squared).log10())
}

fn picture_of(a: &Rgba, b: &Rgba) -> Rgba {
    let mut out = Rgba::new(a.width, a.height);
    for ((p, q), o) in a
        .data
        .chunks_exact(4)
        .zip(b.data.chunks_exact(4))
        .zip(out.data.chunks_exact_mut(4))
    {
        for c in 0..3 {
            o[c] = (u32::from(p[c].abs_diff(q[c])) * AMPLIFY).min(255) as u8;
        }
        o[3] = 255;
    }
    out
}

fn text_of(p: Option<f64>) -> String {
    p.map_or("identical".to_string(), |v| format!("{v:.2} dB"))
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    if let (Some(from), Some(to)) = (job.from, job.to)
        && to <= from
    {
        return Err(format!("--to {to} must come after --from {from}").into());
    }
    let mut a = Side::open(&job.a, job)?;
    let mut b = Side::open(&job.b, job)?;
    if a.size != b.size {
        return Err(format!(
            "{} is {}x{} and {} is {}x{}; compare needs one size",
            job.a.display(),
            a.size.0,
            a.size.1,
            job.b.display(),
            b.size.0,
            b.size.1
        )
        .into());
    }
    let pixels = u64::from(a.size.0) * u64::from(a.size.1);
    let mut compared = 0u64;
    let mut identical = 0u64;
    let (mut squared, mut sum, mut beyond) = (0u128, [0u128; 3], 0u128);
    let mut peak = [0u8; 3];
    let mut worst: Vec<(Frame, Option<Rgba>)> = Vec::new();
    let mut lowest: Option<(u64, f64, u64)> = None;
    let first = match &a.feed {
        Feed::Folder { at, .. } => *at as u64,
        Feed::Video { first, .. } => u64::try_from(*first).unwrap_or(0),
    };
    let (mut extra_a, mut extra_b) = (0u64, 0u64);
    loop {
        let (x, y) = match (a.next()?, b.next()?) {
            (Some(x), Some(y)) => (x, y),
            (Some(_), None) => {
                extra_a = 1;
                break;
            }
            (None, Some(_)) => {
                extra_b = 1;
                break;
            }
            (None, None) => break,
        };
        let (sq, s, p, over) = measure(&x.image, &y.image, job.tolerance);
        let number = first + compared;
        compared += 1;
        squared += u128::from(sq);
        for c in 0..3 {
            sum[c] += u128::from(s[c]);
            peak[c] = peak[c].max(p[c]);
        }
        beyond += u128::from(over);
        if sq == 0 {
            identical += 1;
            continue;
        }
        if lowest.is_none_or(|(w, _, _)| sq > w) {
            lowest = Some((sq, x.time, number));
        }
        let keep =
            job.worst > 0 && (worst.len() < job.worst || worst.iter().any(|(f, _)| f.squared < sq));
        if keep {
            let image = job.diff.as_ref().map(|_| picture_of(&x.image, &y.image));
            worst.push((
                Frame {
                    number,
                    time: x.time,
                    squared: sq,
                    sum: s,
                    peak: p,
                    beyond: over,
                },
                image,
            ));
            worst.sort_by(|l, r| {
                r.0.squared
                    .cmp(&l.0.squared)
                    .then(l.0.number.cmp(&r.0.number))
            });
            worst.truncate(job.worst);
        }
    }
    if compared == 0 {
        return Err("no frames to compare in that window".into());
    }
    let (rest_a, rest_b) = (a.rest()?, b.rest()?);
    let (count_a, count_b) = (compared + extra_a + rest_a, compared + extra_b + rest_b);
    let per_frame = (pixels * 3) as f64;
    let mean_squared = squared as f64 / (compared as f64 * per_frame);
    let mean_psnr = psnr(mean_squared);
    let worst_psnr = lowest.and_then(|(sq, _, _)| psnr(sq as f64 / per_frame));
    let total_pixels = (compared * pixels) as f64;
    let mean_abs: Vec<f64> = sum.iter().map(|s| *s as f64 / total_pixels).collect();
    let share = beyond as f64 / total_pixels;
    let written = match &job.diff {
        Some(dir) if !worst.is_empty() => {
            std::fs::create_dir_all(dir).map_err(|e| about(dir, e))?;
            let mut files = Vec::new();
            for (f, image) in &worst {
                if let Some(image) = image {
                    let path = dir.join(format!("diff-{:06}.png", f.number));
                    let data = png::encode(image).map_err(|e| about(&path, e))?;
                    std::fs::write(&path, data).map_err(|e| about(&path, e))?;
                    files.push(path);
                }
            }
            files
        }
        _ => Vec::new(),
    };
    let listed: Vec<(&Frame, Option<&PathBuf>)> = worst
        .iter()
        .enumerate()
        .map(|(i, (f, _))| (f, written.get(i)))
        .collect();
    if job.json {
        let doc = Json::Obj(vec![
            ("a", job.a.display().to_string().into()),
            ("b", job.b.display().to_string().into()),
            ("width", a.size.0.into()),
            ("height", a.size.1.into()),
            ("frames_a", count_a.into()),
            ("frames_b", count_b.into()),
            ("compared", compared.into()),
            ("identical_frames", identical.into()),
            ("identical", (identical == compared).into()),
            ("psnr_mean", mean_psnr.map_or(Json::Null, Json::from)),
            ("psnr_worst", worst_psnr.map_or(Json::Null, Json::from)),
            (
                "worst_frame",
                lowest.map_or(Json::Null, |(_, _, n)| n.into()),
            ),
            (
                "worst_time",
                lowest.map_or(Json::Null, |(_, t, _)| t.into()),
            ),
            (
                "mean_abs_diff",
                Json::Arr(mean_abs.iter().map(|v| (*v).into()).collect()),
            ),
            (
                "max_abs_diff",
                Json::Arr(peak.iter().map(|v| u32::from(*v).into()).collect()),
            ),
            ("tolerance", u32::from(job.tolerance).into()),
            ("beyond_tolerance", share.into()),
            (
                "worst",
                Json::Arr(
                    listed
                        .iter()
                        .map(|(f, path)| {
                            Json::Obj(vec![
                                ("frame", f.number.into()),
                                ("time", f.time.into()),
                                (
                                    "psnr",
                                    psnr(f.squared as f64 / per_frame)
                                        .map_or(Json::Null, Json::from),
                                ),
                                (
                                    "mean_abs_diff",
                                    Json::Arr(
                                        f.sum
                                            .iter()
                                            .map(|s| (*s as f64 / pixels as f64).into())
                                            .collect(),
                                    ),
                                ),
                                (
                                    "max_abs_diff",
                                    Json::Arr(
                                        f.peak.iter().map(|v| u32::from(*v).into()).collect(),
                                    ),
                                ),
                                ("beyond_tolerance", (f.beyond as f64 / pixels as f64).into()),
                                (
                                    "diff",
                                    path.map_or(Json::Null, |p| p.display().to_string().into()),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]);
        writeln!(out, "{}", doc.text())?;
        return Ok(());
    }
    writeln!(
        out,
        "a                {} ({}x{})",
        job.a.display(),
        a.size.0,
        a.size.1
    )?;
    writeln!(out, "b                {}", job.b.display())?;
    if count_a == count_b {
        writeln!(out, "frames compared  {compared}")?;
    } else {
        writeln!(
            out,
            "frames compared  {compared} (a has {count_a}, b has {count_b}; the shorter run is compared)"
        )?;
    }
    writeln!(out, "identical frames {identical}")?;
    match (mean_psnr, lowest) {
        (Some(m), Some((_, t, n))) => writeln!(
            out,
            "PSNR             {} mean, {} worst (frame {n} at {t:.3} s)",
            text_of(Some(m)),
            text_of(worst_psnr)
        )?,
        _ => writeln!(out, "PSNR             identical")?,
    }
    writeln!(
        out,
        "mean abs diff    R {:.4}  G {:.4}  B {:.4}  (0-255)",
        mean_abs[0], mean_abs[1], mean_abs[2]
    )?;
    writeln!(
        out,
        "max abs diff     R {}  G {}  B {}",
        peak[0], peak[1], peak[2]
    )?;
    writeln!(
        out,
        "beyond tolerance {:.4}% of pixels differ by more than {}",
        share * 100.0,
        job.tolerance
    )?;
    for (i, (f, path)) in listed.iter().enumerate() {
        let shown = path.map_or(String::new(), |p| format!("  {}", p.display()));
        writeln!(
            out,
            "worst {:<3}        frame {} at {:.3} s  {}{shown}",
            i + 1,
            f.number,
            f.time,
            text_of(psnr(f.squared as f64 / per_frame))
        )?;
    }
    Ok(())
}
