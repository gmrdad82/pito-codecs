use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pito_codecs::media::{self, Prefs, Rgba, Seek, Stills, keyframes, probe};
use pito_codecs::png;
use rayon::prelude::*;

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
}

const MAX_STILLS: usize = 10_000;
const DEFAULT_COUNT: usize = 12;

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

fn write_png(path: &Path, image: &Rgba) -> Result<(), Fail> {
    let data = png::encode(image).map_err(|e| about(path, e))?;
    std::fs::write(path, data).map_err(|e| about(path, e))?;
    Ok(())
}

fn quoted(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn path_json(p: Option<&Path>) -> String {
    match p {
        Some(p) => quoted(&p.display().to_string()),
        None => "null".into(),
    }
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
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
                .map(|i| Some(dir.join(format!("still-{i:0digits$}.png"))))
                .collect()
        }
        None => vec![None; n],
    };
    let sheet_image = match &g {
        Some(g) => Some(sheet(&images, g, job.pad)?),
        None => None,
    };
    let writes: Vec<(&Path, &Rgba)> = paths
        .iter()
        .zip(&images)
        .filter_map(|(p, img)| p.as_deref().map(|p| (p, img)))
        .chain(job.sheet.as_deref().zip(sheet_image.as_ref()))
        .collect();
    writes
        .par_iter()
        .map(|(p, img)| write_png(p, img).map_err(|e| e.to_string()))
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
