use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pito_codecs::filter::spectrum::{TEXT_HEIGHT, draw_text, text_width};
use pito_codecs::media::{self, Prefs, Rgba, Seek, Stills, probe};
use pito_codecs::{jpeg, png, webp};

use super::convert::land;
use super::json::Json;
use super::plan::{partial, same_file};
use super::{Fail, about};

pub const MAX_TILES: usize = 10_000;
const MAX_PIXELS: u64 = 1 << 28;
const LABEL_PAD: u32 = 4;
const JPEG_QUALITY: u8 = 85;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cadence {
    Every(f64),
    Count(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub wrap: Option<usize>,
    pub max_width: Option<u32>,
    pub gap: u32,
    pub background: [u8; 3],
    pub timecodes: bool,
}

pub struct Job {
    pub file: PathBuf,
    pub output: PathBuf,
    pub cadence: Cadence,
    pub height: u32,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub seek: Seek,
    pub prefs: Prefs,
    pub look: Look,
    pub force: bool,
    pub json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub cols: usize,
    pub rows: usize,
    pub tile: (u32, u32),
    pub cell: u32,
    pub label: u32,
    pub width: u32,
    pub height: u32,
}

pub fn format_of(path: &Path) -> Result<Format, Fail> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => Ok(Format::Png),
        Some("jpg" | "jpeg") => Ok(Format::Jpeg),
        Some("webp") => Ok(Format::Webp),
        _ => Err(format!(
            "{}: a filmstrip is a PNG, JPEG or WebP; name the output .png, .jpg or .webp",
            path.display()
        )
        .into()),
    }
}

pub fn color_of(text: &str) -> Result<[u8; 3], String> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    let bad = || format!("'{text}' is not a colour such as #000000");
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

pub fn clock(t: f64) -> String {
    let s = if t.is_finite() && t > 0.0 {
        t.floor() as u64
    } else {
        0
    };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn times(cadence: Cadence, start: f64, end: f64) -> Result<Vec<f64>, Fail> {
    if !(start.is_finite() && end.is_finite() && end > start) {
        return Err(
            format!("no time between {start:.3} s and {end:.3} s to take tiles from").into(),
        );
    }
    let span = end - start;
    match cadence {
        Cadence::Every(step) => {
            if !(step.is_finite() && step > 0.0) {
                return Err("--every must be longer than 0 s".into());
            }
            let n = (span / step - 1e-9).ceil().max(1.0);
            if n > MAX_TILES as f64 {
                return Err(format!(
                    "a tile every {step} s makes {n:.0} tiles, at most {MAX_TILES}"
                )
                .into());
            }
            Ok((0..n as usize).map(|i| start + i as f64 * step).collect())
        }
        Cadence::Count(0) => Err("--count must be at least 1".into()),
        Cadence::Count(n) if n > MAX_TILES => {
            Err(format!("asked for {n} tiles, at most {MAX_TILES}").into())
        }
        Cadence::Count(n) => Ok((0..n)
            .map(|i| start + span * (i as f64 + 0.5) / n as f64)
            .collect()),
    }
}

pub fn label_width(labels: &[String], look: &Look) -> u32 {
    if !look.timecodes {
        return 0;
    }
    labels
        .iter()
        .map(|l| text_width(l).max(0) as u32 + 2 * LABEL_PAD)
        .max()
        .unwrap_or(0)
}

pub fn geometry(n: usize, tile: (u32, u32), label_w: u32, look: &Look) -> Result<Geometry, Fail> {
    let too_large = || -> Fail { "the filmstrip would be larger than 268 megapixels".into() };
    if n == 0 || tile.0 == 0 || tile.1 == 0 {
        return Err("a filmstrip needs at least one tile".into());
    }
    let cell = tile.0.max(label_w);
    let (tw, th, gap) = (u64::from(cell), u64::from(tile.1), u64::from(look.gap));
    let cols = match (look.wrap, look.max_width) {
        (Some(0), _) => return Err("--wrap must be at least 1".into()),
        (Some(w), _) => w.min(n),
        (None, Some(max)) => {
            let fit = u64::from(max).saturating_sub(gap) / (tw + gap);
            if fit == 0 {
                return Err(format!(
                    "--max-width {max} is narrower than one tile ({} px with its gaps)",
                    tw + 2 * gap
                )
                .into());
            }
            usize::try_from(fit).unwrap_or(usize::MAX).min(n)
        }
        (None, None) => n,
    };
    let rows = n.div_ceil(cols);
    let label = if look.timecodes {
        TEXT_HEIGHT as u32 + 2 * LABEL_PAD
    } else {
        0
    };
    let span = |cells: usize, cell: u64| -> Option<u64> {
        let cells = cells as u64;
        cell.checked_mul(cells)?
            .checked_add(gap.checked_mul(cells.checked_add(1)?)?)
    };
    let (w, h) = match (span(cols, tw), span(rows, th + u64::from(label))) {
        (Some(w), Some(h)) if w.saturating_mul(h) <= MAX_PIXELS => (w, h),
        _ => return Err(too_large()),
    };
    Ok(Geometry {
        cols,
        rows,
        tile,
        cell,
        label,
        width: u32::try_from(w).map_err(|_| too_large())?,
        height: u32::try_from(h).map_err(|_| too_large())?,
    })
}

fn ink(background: [u8; 3]) -> [u8; 3] {
    let [r, g, b] = background.map(u32::from);
    if r * 299 + g * 587 + b * 114 > 128_000 {
        [16, 19, 25]
    } else {
        [232, 235, 240]
    }
}

pub fn compose(tiles: &[Rgba], labels: &[String], g: &Geometry, look: &Look) -> Rgba {
    let mut out = Rgba::new(g.width, g.height);
    let bg = look.background;
    for px in out.data.chunks_exact_mut(4) {
        px.copy_from_slice(&[bg[0], bg[1], bg[2], 255]);
    }
    let stride = out.stride();
    let (cw, ch) = (g.cell, g.tile.1);
    for (i, s) in tiles.iter().enumerate() {
        let (row, col) = ((i / g.cols) as u32, (i % g.cols) as u32);
        let left = look.gap + col * (cw + look.gap);
        let top = look.gap + row * (ch + g.label + look.gap);
        let x = left + cw.saturating_sub(s.width) / 2;
        let y = top + ch.saturating_sub(s.height) / 2;
        let line = s.width.min(cw) as usize * 4;
        for (sy, src) in s
            .data
            .chunks_exact(s.stride())
            .take(ch as usize)
            .enumerate()
        {
            let at = (y as usize + sy) * stride + x as usize * 4;
            if let (Some(dst), Some(src)) = (out.data.get_mut(at..at + line), src.get(..line)) {
                over(dst, src, bg);
            }
        }
        if let Some(text) = labels.get(i).filter(|_| g.label > 0) {
            let tx = left as i32 + (cw as i32 - text_width(text)) / 2;
            let ty = (top + ch + LABEL_PAD) as i32;
            draw_text(&mut out, tx, ty, text, ink(look.background));
        }
    }
    out
}

pub fn over(dst: &mut [u8], src: &[u8], bg: [u8; 3]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let a = u32::from(s[3]);
        for ((dc, sc), bc) in d.iter_mut().zip(s).zip(bg) {
            *dc = ((u32::from(*sc) * a + u32::from(bc) * (255 - a) + 127) / 255) as u8;
        }
        d[3] = 255;
    }
}

pub fn when(labels: &[String]) -> String {
    const SHOWN: usize = 12;
    if labels.len() <= SHOWN {
        return labels.join(" ");
    }
    let n = labels.len();
    format!(
        "{} … {} ({n} times)",
        labels[..3].join(" "),
        labels[n - 2..].join(" ")
    )
}

fn encoded(format: Format, image: &Rgba, output: &Path) -> Result<Vec<u8>, Fail> {
    match format {
        Format::Png => png::encode(image).map_err(|e| about(output, e)),
        Format::Jpeg => jpeg::encode(image, JPEG_QUALITY).map_err(|e| about(output, e)),
        Format::Webp => {
            webp::encode(image, &webp::Options::default()).map_err(|e| about(output, e))
        }
    }
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    let started = Instant::now();
    let file = &job.file;
    let format = format_of(&job.output)?;
    if job.height == 0 || job.height > 4320 {
        return Err(format!("height {} is outside 1 to 4320", job.height).into());
    }
    if same_file(file, &job.output) {
        return Err("the output is the input; write to another file".into());
    }
    if !job.force && job.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            job.output.display()
        )
        .into());
    }
    let p = probe(file, &job.prefs).map_err(|e| about(file, e))?;
    if !(p.duration.is_finite() && p.duration > 0.0) {
        return Err(about(file, "has no duration to take tiles over"));
    }
    let start = job.from.unwrap_or(0.0);
    let end = job.to.unwrap_or(p.duration).min(p.duration);
    if start >= p.duration {
        return Err(about(
            file,
            format!(
                "--from {start} is at or past the end of the video ({:.3} s)",
                p.duration
            ),
        ));
    }
    if end <= start {
        return Err(format!("--to {end} must come after --from {start}").into());
    }
    let asked = times(job.cadence, start, end)?;
    let (sw, sh) = p.shown_size();
    if sw == 0 || sh == 0 {
        return Err(about(file, format!("the video's shape is {sw}x{sh}")));
    }
    let guess =
        (u64::from(sw) * u64::from(job.height) / u64::from(sh)).clamp(1, u64::from(u32::MAX));
    let asked_labels: Vec<String> = asked.iter().map(|t| clock(*t)).collect();
    geometry(
        asked.len(),
        (guess as u32, job.height),
        label_width(&asked_labels, &job.look),
        &job.look,
    )?;
    encoded(format, &Rgba::new(1, 1), &job.output)?;
    let stills = Stills::open(file, job.height, &job.prefs).map_err(|e| about(file, e))?;
    let mut got: Vec<Option<media::Result<Rgba>>> = (0..asked.len()).map(|_| None).collect();
    stills
        .frames(&asked, job.seek, |i, r| {
            if let Some(slot) = got.get_mut(i) {
                *slot = Some(r);
            }
        })
        .map_err(|e| about(file, e))?;
    let mut tiles = Vec::with_capacity(asked.len());
    for (r, t) in got.into_iter().zip(&asked) {
        match r {
            Some(Ok(img)) => tiles.push(img),
            Some(Err(e)) => return Err(about(file, format!("tile at {t:.3} s: {e}"))),
            None => return Err(about(file, format!("no tile at {t:.3} s"))),
        }
    }
    let shown = stills
        .shown(&asked, job.seek)
        .map_err(|e| about(file, format!("the tiles' times: {e}")))?;
    drop(stills);
    let tile = (
        tiles.iter().map(|t| t.width).max().unwrap_or(0),
        tiles.iter().map(|t| t.height).max().unwrap_or(0),
    );
    let labels: Vec<String> = shown.iter().map(|t| clock(*t)).collect();
    let g = geometry(
        tiles.len(),
        tile,
        label_width(&labels, &job.look),
        &job.look,
    )?;
    let count = tiles.len();
    let image = compose(&tiles, &labels, &g, &job.look);
    drop(tiles);
    let data = encoded(format, &image, &job.output)?;
    drop(image);
    let part = partial(&job.output)?;
    let landed = std::fs::write(&part, &data)
        .map_err(|e| about(&job.output, e))
        .and_then(|()| land(&part, &job.output, job.force));
    if landed.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    landed?;
    let seconds = started.elapsed().as_secs_f64();
    if job.json {
        let doc = Json::Obj(vec![
            ("file", file.display().to_string().into()),
            ("output", job.output.display().to_string().into()),
            ("width", g.width.into()),
            ("height", g.height.into()),
            ("tile_width", g.tile.0.into()),
            ("tile_height", g.tile.1.into()),
            ("tiles", count.into()),
            ("cols", g.cols.into()),
            ("rows", g.rows.into()),
            (
                "seek",
                match job.seek {
                    Seek::Exact => "exact",
                    Seek::Keyframe => "keyframe",
                }
                .into(),
            ),
            (
                "times",
                Json::Arr(shown.iter().map(|t| (*t).into()).collect()),
            ),
            (
                "asked",
                Json::Arr(asked.iter().map(|t| (*t).into()).collect()),
            ),
            ("bytes", data.len().into()),
            ("seconds", seconds.into()),
        ]);
        writeln!(out, "{}", doc.text())?;
    } else {
        writeln!(
            out,
            "{} {}x{}, {} tiles of {}x{} at {}",
            job.output.display(),
            g.width,
            g.height,
            count,
            g.tile.0,
            g.tile.1,
            when(&labels)
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look() -> Look {
        Look {
            wrap: None,
            max_width: None,
            gap: 4,
            background: [0, 0, 0],
            timecodes: false,
        }
    }

    fn solid(w: u32, h: u32, c: [u8; 3]) -> Rgba {
        let mut img = Rgba::new(w, h);
        for px in img.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        img
    }

    fn at(img: &Rgba, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize * img.width as usize + x as usize) * 4;
        [img.data[i], img.data[i + 1], img.data[i + 2]]
    }

    fn close(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn every_takes_a_tile_at_each_step_from_the_start() {
        assert!(close(
            &times(Cadence::Every(3.0), 0.0, 10.0).unwrap(),
            &[0.0, 3.0, 6.0, 9.0]
        ));
        assert!(close(
            &times(Cadence::Every(2.0), 2.0, 8.0).unwrap(),
            &[2.0, 4.0, 6.0]
        ));
        assert!(close(
            &times(Cadence::Every(2.5), 0.0, 10.0).unwrap(),
            &[0.0, 2.5, 5.0, 7.5]
        ));
    }

    #[test]
    fn a_video_shorter_than_one_interval_gives_one_tile() {
        assert!(close(
            &times(Cadence::Every(5.0), 0.0, 1.5).unwrap(),
            &[0.0]
        ));
        assert!(close(
            &times(Cadence::Every(5.0), 1.0, 1.5).unwrap(),
            &[1.0]
        ));
    }

    #[test]
    fn count_spaces_tiles_evenly_over_the_window() {
        assert!(close(
            &times(Cadence::Count(4), 0.0, 8.0).unwrap(),
            &[1.0, 3.0, 5.0, 7.0]
        ));
        assert!(close(&times(Cadence::Count(1), 2.0, 4.0).unwrap(), &[3.0]));
    }

    #[test]
    fn zero_negative_and_huge_cadences_are_refused() {
        assert!(times(Cadence::Every(0.0), 0.0, 10.0).is_err());
        assert!(times(Cadence::Every(-1.0), 0.0, 10.0).is_err());
        assert!(times(Cadence::Every(f64::NAN), 0.0, 10.0).is_err());
        assert!(times(Cadence::Count(0), 0.0, 10.0).is_err());
        assert!(times(Cadence::Count(MAX_TILES + 1), 0.0, 10.0).is_err());
        assert!(times(Cadence::Every(0.0001), 0.0, 10.0).is_err());
        assert!(times(Cadence::Count(3), 5.0, 5.0).is_err());
        assert!(times(Cadence::Count(3), 0.0, f64::INFINITY).is_err());
    }

    #[test]
    fn a_strip_is_one_row_with_gaps_around_every_tile() {
        let g = geometry(5, (160, 90), 0, &look()).unwrap();
        assert_eq!((g.cols, g.rows), (5, 1));
        assert_eq!((g.width, g.height), (5 * 160 + 6 * 4, 90 + 8));
        assert_eq!(g.label, 0);
    }

    #[test]
    fn wrap_and_max_width_make_a_grid() {
        let g = geometry(
            5,
            (160, 90),
            0,
            &Look {
                wrap: Some(2),
                ..look()
            },
        )
        .unwrap();
        assert_eq!((g.cols, g.rows), (2, 3));
        assert_eq!((g.width, g.height), (2 * 160 + 3 * 4, 3 * 90 + 4 * 4));
        let g = geometry(
            3,
            (160, 90),
            0,
            &Look {
                wrap: Some(8),
                ..look()
            },
        )
        .unwrap();
        assert_eq!((g.cols, g.rows), (3, 1));
        let g = geometry(
            5,
            (160, 90),
            0,
            &Look {
                max_width: Some(400),
                ..look()
            },
        )
        .unwrap();
        assert_eq!((g.cols, g.rows), (2, 3));
        assert!(g.width <= 400);
        let g = geometry(
            5,
            (160, 90),
            0,
            &Look {
                max_width: Some(332),
                ..look()
            },
        )
        .unwrap();
        assert_eq!(g.cols, 2);
        let e = geometry(
            5,
            (160, 90),
            0,
            &Look {
                max_width: Some(50),
                ..look()
            },
        )
        .unwrap_err();
        assert!(
            e.to_string().contains("narrower than one tile (168 px"),
            "{e}"
        );
        let g = geometry(
            5,
            (160, 90),
            0,
            &Look {
                max_width: Some(168),
                ..look()
            },
        )
        .unwrap();
        assert_eq!((g.cols, g.rows), (1, 5));
        assert!(
            geometry(
                5,
                (160, 90),
                0,
                &Look {
                    max_width: Some(0),
                    ..look()
                }
            )
            .is_err()
        );
        assert!(
            geometry(
                5,
                (160, 90),
                0,
                &Look {
                    max_width: Some(167),
                    ..look()
                }
            )
            .is_err()
        );
        assert!(
            geometry(
                5,
                (160, 90),
                0,
                &Look {
                    wrap: Some(0),
                    ..look()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn timecodes_add_a_label_row_under_each_tile_row() {
        let plain = geometry(
            4,
            (160, 90),
            0,
            &Look {
                wrap: Some(2),
                ..look()
            },
        )
        .unwrap();
        let labelled = geometry(
            4,
            (160, 90),
            0,
            &Look {
                wrap: Some(2),
                timecodes: true,
                ..look()
            },
        )
        .unwrap();
        assert_eq!(labelled.label, TEXT_HEIGHT as u32 + 2 * LABEL_PAD);
        assert_eq!(labelled.height, plain.height + 2 * labelled.label);
        assert_eq!(labelled.width, plain.width);
    }

    #[test]
    fn a_strip_over_268_megapixels_is_refused() {
        let e = geometry(20, (5760, 4320), 0, &look()).unwrap_err();
        assert!(e.to_string().contains("268 megapixels"), "{e}");
        assert!(geometry(MAX_TILES, (u32::MAX, 1), 0, &look()).is_err());
        assert!(geometry(1, (16_384, 16_384), 0, &Look { gap: 0, ..look() }).is_ok());
        assert!(geometry(1, (16_384, 16_385), 0, &Look { gap: 0, ..look() }).is_err());
        assert!(geometry(0, (160, 90), 0, &look()).is_err());
    }

    #[test]
    fn gaps_take_the_background_and_tiles_their_pixels() {
        let l = Look {
            gap: 6,
            background: [255, 0, 0],
            ..look()
        };
        let tiles = vec![solid(20, 10, [0, 255, 0]), solid(20, 10, [0, 0, 255])];
        let g = geometry(2, (20, 10), 0, &l).unwrap();
        let img = compose(&tiles, &[], &g, &l);
        assert_eq!((img.width, img.height), (6 + 20 + 6 + 20 + 6, 6 + 10 + 6));
        assert_eq!(at(&img, 0, 0), [255, 0, 0]);
        assert_eq!(at(&img, 5, 5), [255, 0, 0]);
        assert_eq!(at(&img, 6, 6), [0, 255, 0]);
        assert_eq!(at(&img, 25, 15), [0, 255, 0]);
        assert_eq!(at(&img, 26, 10), [255, 0, 0]);
        assert_eq!(at(&img, 31, 10), [255, 0, 0]);
        assert_eq!(at(&img, 32, 6), [0, 0, 255]);
        assert_eq!(at(&img, img.width - 1, img.height - 1), [255, 0, 0]);
    }

    #[test]
    fn a_narrower_tile_is_centred_in_its_cell() {
        let l = Look { gap: 0, ..look() };
        let tiles = vec![solid(20, 10, [0, 255, 0]), solid(10, 10, [0, 0, 255])];
        let g = geometry(2, (20, 10), 0, &l).unwrap();
        let img = compose(&tiles, &[], &g, &l);
        assert_eq!(at(&img, 24, 5), [0, 0, 0]);
        assert_eq!(at(&img, 25, 5), [0, 0, 255]);
        assert_eq!(at(&img, 34, 5), [0, 0, 255]);
        assert_eq!(at(&img, 35, 5), [0, 0, 0]);
    }

    #[test]
    fn timecodes_draw_ink_in_the_label_row_only() {
        let l = Look {
            timecodes: true,
            ..look()
        };
        let tiles = vec![solid(80, 20, [0, 128, 0]); 2];
        let g = geometry(2, (80, 20), 0, &l).unwrap();
        let labels = vec![clock(0.0), clock(65.0)];
        let img = compose(&tiles, &labels, &g, &l);
        let label_rows = (l.gap + 20)..(l.gap + 20 + g.label);
        let inked = |cols: std::ops::Range<u32>| {
            label_rows
                .clone()
                .flat_map(|y| cols.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| at(&img, x, y) == ink(l.background))
                .count()
        };
        assert!(inked(l.gap..l.gap + 80) > 0);
        assert!(inked(2 * l.gap + 80..2 * l.gap + 160) > 0);
        assert_eq!(at(&img, l.gap + 40, l.gap + 10), [0, 128, 0]);
        let none = compose(&tiles, &[], &g, &l);
        assert!(
            label_rows
                .clone()
                .all(|y| (0..none.width).all(|x| at(&none, x, y) == [0, 0, 0]))
        );
    }

    #[test]
    fn ink_contrasts_with_the_background() {
        assert_eq!(ink([0, 0, 0]), [232, 235, 240]);
        assert_eq!(ink([255, 255, 255]), [16, 19, 25]);
    }

    #[test]
    fn clock_is_m_ss_and_h_mm_ss_past_an_hour() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(599.0), "9:59");
        assert_eq!(clock(3599.9), "59:59");
        assert_eq!(clock(3600.0), "1:00:00");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(-1.0), "0:00");
        assert_eq!(clock(f64::NAN), "0:00");
    }

    #[test]
    fn colours_parse_with_or_without_the_hash() {
        assert_eq!(color_of("#ff8000"), Ok([255, 128, 0]));
        assert_eq!(color_of("1E1E2E"), Ok([30, 30, 46]));
        assert!(color_of("#12345").is_err());
        assert!(color_of("#gg0000").is_err());
        assert!(color_of("#ff80001").is_err());
        assert!(color_of("").is_err());
    }

    #[test]
    fn the_extension_picks_the_format() {
        assert_eq!(format_of(Path::new("a.png")).unwrap(), Format::Png);
        assert_eq!(format_of(Path::new("a.JPG")).unwrap(), Format::Jpeg);
        assert_eq!(format_of(Path::new("a.jpeg")).unwrap(), Format::Jpeg);
        assert_eq!(format_of(Path::new("a.webp")).unwrap(), Format::Webp);
        assert!(format_of(Path::new("a.gif")).is_err());
        assert!(format_of(Path::new("strip")).is_err());
    }

    #[test]
    fn a_label_wider_than_its_tile_widens_the_cell_and_stays_inside_it() {
        let l = Look {
            timecodes: true,
            ..look()
        };
        let labels = vec![clock(600.0), clock(3725.0), clock(4000.0)];
        let lw = label_width(&labels, &l);
        assert_eq!(lw, text_width("1:02:05") as u32 + 2 * LABEL_PAD);
        assert_eq!(label_width(&labels, &look()), 0);
        let g = geometry(3, (50, 90), lw, &l).unwrap();
        assert_eq!(g.tile, (50, 90));
        assert_eq!(g.cell, lw);
        assert_eq!(g.width, 3 * lw + 4 * l.gap);
        let narrow = geometry(3, (50, 90), 20, &l).unwrap();
        assert_eq!(narrow.cell, 50);
        let tiles = vec![solid(50, 90, [0, 128, 0]); 3];
        let img = compose(&tiles, &labels, &g, &l);
        let rows = (l.gap + 90)..(l.gap + 90 + g.label);
        for c in 0..=3u32 {
            let gap_x = c * (lw + l.gap)..c * (lw + l.gap) + l.gap;
            for y in rows.clone() {
                for x in gap_x.clone() {
                    assert_eq!(at(&img, x, y), [0, 0, 0], "ink in the gap at {x},{y}");
                }
            }
        }
        let left = l.gap + (lw - 50) / 2;
        assert_eq!(at(&img, left, l.gap + 45), [0, 128, 0]);
        assert_eq!(at(&img, left - 1, l.gap + 45), [0, 0, 0]);
    }

    #[test]
    fn each_label_lands_under_its_own_tile() {
        let l = Look {
            timecodes: true,
            ..look()
        };
        let tiles = vec![solid(80, 20, [0, 128, 0]); 2];
        let g = geometry(2, (80, 20), 0, &l).unwrap();
        let img = compose(&tiles, &[clock(0.0), String::new()], &g, &l);
        let rows = (l.gap + 20)..(l.gap + 20 + g.label);
        let inked = |cols: std::ops::Range<u32>| {
            rows.clone()
                .flat_map(|y| cols.clone().map(move |x| (x, y)))
                .filter(|&(x, y)| at(&img, x, y) == ink(l.background))
                .count()
        };
        assert!(inked(l.gap..l.gap + 80) > 0);
        assert_eq!(inked(2 * l.gap + 80..2 * l.gap + 160), 0);
    }

    #[test]
    fn see_through_tiles_are_flattened_onto_the_background() {
        let l = Look {
            gap: 0,
            background: [200, 100, 0],
            ..look()
        };
        let mut tile = solid(3, 1, [0, 0, 255]);
        tile.data[3] = 0;
        tile.data[7] = 128;
        let g = geometry(1, (3, 1), 0, &l).unwrap();
        let img = compose(&[tile], &[], &g, &l);
        assert_eq!(&img.data[0..4], &[200, 100, 0, 255]);
        assert_eq!(&img.data[4..8], &[100, 50, 128, 255]);
        assert_eq!(&img.data[8..12], &[0, 0, 255, 255]);
    }

    #[test]
    fn the_text_line_names_every_time_up_to_twelve_then_the_ends() {
        let few: Vec<String> = (0..12).map(|s| clock(s as f64)).collect();
        assert_eq!(when(&few), few.join(" "));
        let many: Vec<String> = (0..20).map(|s| clock(s as f64)).collect();
        assert_eq!(when(&many), "0:00 0:01 0:02 … 0:18 0:19 (20 times)");
        assert_eq!(when(&[]), "");
    }
}
