#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use pito_codecs::gif::{self, Dither, EncodeOptions};
use pito_codecs::media::{Blender, Prefs, Rgba, decode_video, frames_at, probe, scale, to_rgba};
use pito_codecs::webp::{AnimEncoder, Options};

type Res<T> = Result<T, Box<dyn Error>>;

const WIDTH: u32 = 1280;
const STILLS: usize = 12;
const GAP: u32 = 8;
const FROM: f64 = 0.05;
const TO: f64 = 0.85;
const BACKDROP: [u8; 4] = [13, 17, 23, 255];
const WHITE: [u8; 3] = [255, 255, 255];
const YELLOW: [u8; 3] = [255, 212, 0];
const OVERLAY_AT: f64 = 21.0;
const CLIP_AT: f64 = 69.0;
const CLIP_SECONDS: f64 = 3.0;
const CLIP_HEIGHT: u32 = 270;
const CLIP_STEP: usize = 6;

fn main() -> ExitCode {
    fail::run("readme_shots", run)
}

fn run() -> Res<()> {
    let mut args = std::env::args().skip(1);
    let (Some(clip), Some(out)) = (args.next(), args.next()) else {
        return Err("usage: readme_shots <clip.mp4> <out dir>".into());
    };
    let clip = PathBuf::from(clip);
    let out = PathBuf::from(out);
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let prefs = Prefs::default();
    filmstrip(&clip, &out, &prefs)?;
    overlay(&clip, &out, &prefs)?;
    animated(&clip, &out, &prefs)?;
    Ok(())
}

fn write(path: &Path, data: &[u8]) -> Res<()> {
    std::fs::write(path, data).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn still_gif(image: Rgba) -> Res<Vec<u8>> {
    let opts = EncodeOptions {
        loop_count: None,
        ..EncodeOptions::default()
    };
    Ok(gif::encode(&[image], &[0], opts)?)
}

fn paste(dst: &mut Rgba, src: &Rgba, x: u32, y: u32) {
    let (dw, sw) = (dst.width as usize, src.width as usize);
    for row in 0..src.height as usize {
        let dy = y as usize + row;
        if dy >= dst.height as usize {
            break;
        }
        let w = sw.min(dw.saturating_sub(x as usize));
        let d = (dy * dw + x as usize) * 4;
        let s = row * sw * 4;
        if let (Some(to), Some(from)) = (dst.data.get_mut(d..d + w * 4), src.data.get(s..s + w * 4))
        {
            to.copy_from_slice(from);
        }
    }
}

fn filmstrip(clip: &Path, out: &Path, prefs: &Prefs) -> Res<()> {
    let info = probe(clip, prefs)?;
    let times: Vec<f64> = (0..STILLS)
        .map(|i| info.duration * (FROM + (TO - FROM) * i as f64 / (STILLS - 1) as f64))
        .collect();
    let t = Instant::now();
    let stills = frames_at(clip, &times, 240, prefs)?;
    let took = ms(t);
    let per_row = STILLS.div_ceil(2) as u32;
    let cell_w = (WIDTH - GAP * (per_row + 1)) / per_row;
    let cell_h = cell_w * info.height.max(1) / info.width.max(1);
    let mut sheet = Rgba::new(WIDTH, GAP * 3 + cell_h * 2);
    for px in sheet.data.chunks_exact_mut(4) {
        px.copy_from_slice(&BACKDROP);
    }
    for (i, still) in stills.iter().enumerate() {
        let (col, row) = (i as u32 % per_row, i as u32 / per_row);
        let cell = scale(still, cell_w, cell_h);
        paste(
            &mut sheet,
            &cell,
            GAP + col * (cell_w + GAP),
            GAP + row * (cell_h + GAP),
        );
    }
    let t = Instant::now();
    let data = still_gif(sheet)?;
    let coded = ms(t);
    write(&out.join("filmstrip.gif"), &data)?;
    say!(
        "filmstrip: {} stills in {took:.1} ms, gif {coded:.1} ms, {} bytes",
        stills.len(),
        data.len()
    )?;
    Ok(())
}

type Stroke = Vec<(f32, f32)>;

fn arc(cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    let n = 12;
    (0..=n)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / n as f32).to_radians();
            (cx + rx * a.cos(), cy - ry * a.sin())
        })
        .collect()
}

fn joined(parts: &[Vec<(f32, f32)>]) -> Stroke {
    parts.iter().flatten().copied().collect()
}

fn glyph(c: char) -> Vec<Stroke> {
    let p = |v: &[(f32, f32)]| v.to_vec();
    let bowl = || {
        joined(&[
            p(&[(0.0, 6.0), (0.0, 0.0), (2.5, 0.0)]),
            arc(2.5, 1.5, 1.5, 1.5, 90.0, -90.0),
            p(&[(0.0, 3.0)]),
        ])
    };
    let open_c = |end: f32| {
        joined(&[
            arc(2.0, 2.0, 2.0, 2.0, 35.0, 180.0),
            arc(2.0, 4.0, 2.0, 2.0, 180.0, end),
        ])
    };
    match c {
        'A' => vec![
            p(&[(0.0, 6.0), (2.0, 0.0), (4.0, 6.0)]),
            p(&[(0.7, 4.0), (3.3, 4.0)]),
        ],
        'C' => vec![open_c(325.0)],
        'E' => vec![
            p(&[(4.0, 0.0), (0.0, 0.0), (0.0, 6.0), (4.0, 6.0)]),
            p(&[(0.0, 3.0), (3.2, 3.0)]),
        ],
        'F' => vec![
            p(&[(4.0, 0.0), (0.0, 0.0), (0.0, 6.0)]),
            p(&[(0.0, 3.0), (3.2, 3.0)]),
        ],
        'G' => vec![joined(&[open_c(360.0), p(&[(4.0, 3.2), (2.4, 3.2)])])],
        'I' => vec![
            p(&[(2.0, 0.0), (2.0, 6.0)]),
            p(&[(1.0, 0.0), (3.0, 0.0)]),
            p(&[(1.0, 6.0), (3.0, 6.0)]),
        ],
        'L' => vec![p(&[(0.0, 0.0), (0.0, 6.0), (4.0, 6.0)])],
        'M' => vec![p(&[
            (0.0, 6.0),
            (0.0, 0.0),
            (2.0, 3.6),
            (4.0, 0.0),
            (4.0, 6.0),
        ])],
        'N' => vec![p(&[(0.0, 6.0), (0.0, 0.0), (4.0, 6.0), (4.0, 0.0)])],
        'O' => vec![joined(&[
            arc(2.0, 2.0, 2.0, 2.0, 0.0, 180.0),
            arc(2.0, 4.0, 2.0, 2.0, 180.0, 360.0),
            p(&[(4.0, 2.0)]),
        ])],
        'P' => vec![bowl()],
        'R' => vec![bowl(), p(&[(2.0, 3.0), (4.0, 6.0)])],
        'S' => vec![joined(&[
            arc(2.0, 1.5, 2.0, 1.5, 30.0, 270.0),
            arc(2.0, 4.5, 2.0, 1.5, 90.0, -150.0),
        ])],
        _ => Vec::new(),
    }
}

struct Text<'a> {
    words: &'a [(&'a str, [u8; 3])],
    unit: f32,
    stroke: f32,
    outline: f32,
}

impl Text<'_> {
    const ADVANCE: f32 = 5.6;
    const SPACE: f32 = 3.0;

    fn width(&self) -> f32 {
        let letters: usize = self.words.iter().map(|(w, _)| w.len()).sum();
        let spaces = self.words.len().saturating_sub(1);
        (letters as f32 * Self::ADVANCE - 1.6 + spaces as f32 * Self::SPACE) * self.unit
    }

    fn draw(&self, layer: &mut Rgba, x: f32, y: f32) {
        let mut strokes = Vec::new();
        let mut pen = x;
        for (word, color) in self.words {
            for c in word.chars() {
                for stroke in glyph(c) {
                    let pts: Vec<(f32, f32)> = stroke
                        .iter()
                        .map(|&(gx, gy)| (pen + gx * self.unit, y + gy * self.unit))
                        .collect();
                    strokes.push((pts, *color));
                }
                pen += Self::ADVANCE * self.unit;
            }
            pen += Self::SPACE * self.unit;
        }
        if self.outline > 0.0 {
            for (pts, _) in &strokes {
                shade(layer, pts, self.stroke + self.outline);
            }
        }
        for (pts, color) in &strokes {
            fill(layer, pts, self.stroke, *color);
        }
    }
}

fn distance(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 {
        (((px - a.0) * dx + (py - a.1) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx - px, a.1 + t * dy - py);
    (cx * cx + cy * cy).sqrt()
}

fn coverage(
    pts: &[(f32, f32)],
    radius: f32,
    layer: &mut Rgba,
    mut put: impl FnMut(&mut [u8], f32),
) {
    let reach = radius + 1.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in pts {
        x0 = x0.min(x - reach);
        y0 = y0.min(y - reach);
        x1 = x1.max(x + reach);
        y1 = y1.max(y + reach);
    }
    let (w, h) = (layer.width as i64, layer.height as i64);
    for py in (y0.floor() as i64).max(0)..(y1.ceil() as i64).min(h) {
        for px in (x0.floor() as i64).max(0)..(x1.ceil() as i64).min(w) {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let d = pts
                .windows(2)
                .map(|s| distance(fx, fy, s[0], s[1]))
                .fold(f32::MAX, f32::min);
            let cov = (radius + 0.5 - d).clamp(0.0, 1.0);
            let i = ((py * w + px) * 4) as usize;
            if cov > 0.0
                && let Some(dst) = layer.data.get_mut(i..i + 4)
            {
                put(dst, cov);
            }
        }
    }
}

fn shade(layer: &mut Rgba, pts: &[(f32, f32)], radius: f32) {
    coverage(pts, radius, layer, |dst, cov| {
        dst[3] = dst[3].max((cov * 255.0).round() as u8);
    });
}

fn fill(layer: &mut Rgba, pts: &[(f32, f32)], radius: f32, color: [u8; 3]) {
    coverage(pts, radius, layer, |dst, cov| {
        let a = dst[3] as f32 / 255.0;
        let out = cov + a * (1.0 - cov);
        for (o, c) in dst[..3].iter_mut().zip(color) {
            *o = ((c as f32 * cov + *o as f32 * a * (1.0 - cov)) / out).round() as u8;
        }
        dst[3] = (out * 255.0).round() as u8;
    });
}

fn rounded_box(layer: &mut Rgba, x: u32, y: u32, w: u32, h: u32, r: f32, rgba: [u8; 4]) {
    for py in y..(y + h).min(layer.height) {
        for px in x..(x + w).min(layer.width) {
            let (fx, fy) = ((px - x) as f32 + 0.5, (py - y) as f32 + 0.5);
            let cx = fx.clamp(r, w as f32 - r);
            let cy = fy.clamp(r, h as f32 - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let cov = (r + 0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let i = ((py * layer.width + px) * 4) as usize;
            if let Some(dst) = layer.data.get_mut(i..i + 4) {
                dst.copy_from_slice(&[
                    rgba[0],
                    rgba[1],
                    rgba[2],
                    (rgba[3] as f32 * cov).round() as u8,
                ]);
            }
        }
    }
}

fn dot(layer: &mut Rgba, cx: f32, cy: f32, r: f32, rgb: [u8; 3]) {
    fill(layer, &[(cx, cy), (cx, cy)], r, rgb);
}

fn caption_layer(width: u32, height: u32) -> Rgba {
    let mut layer = Rgba::new(width, height);
    let s = height as f32 / 720.0;
    let caption = Text {
        words: &[
            ("A", WHITE),
            ("MESSAGE", WHITE),
            ("OF", WHITE),
            ("RESILIENCE", YELLOW),
        ],
        unit: 8.0 * s,
        stroke: 4.4 * s,
        outline: 3.4 * s,
    };
    let x = (width as f32 - caption.width()) / 2.0;
    caption.draw(&mut layer, x, height as f32 - 96.0 * s);
    let mark = Text {
        words: &[("SAMPLE", WHITE)],
        unit: 3.2 * s,
        stroke: 1.5 * s,
        outline: 0.0,
    };
    let (bw, bh) = (
        (mark.width() + 58.0 * s).round() as u32,
        (40.0 * s).round() as u32,
    );
    let (bx, by) = (width - bw - (24.0 * s) as u32, (24.0 * s) as u32);
    rounded_box(&mut layer, bx, by, bw, bh, 10.0 * s, [0, 0, 0, 150]);
    dot(
        &mut layer,
        bx as f32 + 20.0 * s,
        by as f32 + bh as f32 / 2.0,
        6.0 * s,
        [230, 40, 40],
    );
    mark.draw(
        &mut layer,
        bx as f32 + 36.0 * s,
        by as f32 + (bh as f32 - 6.0 * mark.unit) / 2.0,
    );
    layer
}

fn overlay(clip: &Path, out: &Path, prefs: &Prefs) -> Res<()> {
    let mut video = decode_video(clip, OVERLAY_AT, None, prefs)?;
    let mut frame = video.next().ok_or("no frame at the overlay time")??;
    let t = Instant::now();
    let layer = Arc::new(caption_layer(frame.width, frame.height));
    let drawn = ms(t);
    let t = Instant::now();
    Blender::new().draw(&mut frame, &layer);
    let blended = ms(t);
    let mut image = to_rgba(&frame);
    if image.width > WIDTH {
        let h = image.height * WIDTH / image.width;
        image = scale(&image, WIDTH, h);
    }
    let t = Instant::now();
    let data = still_gif(image)?;
    let coded = ms(t);
    write(&out.join("overlay.gif"), &data)?;
    say!(
        "overlay: caption layer drawn in {drawn:.1} ms, blended into a {}x{} frame in {blended:.1} ms, gif {coded:.1} ms, {} bytes",
        frame.width,
        frame.height,
        data.len()
    )?;
    Ok(())
}

fn animated(clip: &Path, out: &Path, prefs: &Prefs) -> Res<()> {
    let fps = probe(clip, prefs)?.fps().max(1.0);
    let want = (CLIP_SECONDS * fps) as usize;
    let t = Instant::now();
    let mut frames = Vec::new();
    for (i, frame) in decode_video(clip, CLIP_AT, Some(CLIP_HEIGHT), prefs)?
        .take(want)
        .enumerate()
    {
        let frame = frame?;
        if i % CLIP_STEP == 0 {
            frames.push(to_rgba(&frame));
        }
    }
    let decoded = ms(t);
    let Some(first) = frames.first() else {
        return Err("no frames at the clip time".into());
    };
    let (w, h) = (first.width, first.height);
    let step_ms = 1000.0 * CLIP_STEP as f64 / fps;
    let delays: Vec<u16> = (0..frames.len())
        .map(|i| {
            let at = |k: usize| (k as f64 * step_ms / 10.0).round() as u16;
            at(i + 1) - at(i)
        })
        .collect();
    let opts = EncodeOptions {
        colors: 128,
        dither: Dither::Ordered,
        ..EncodeOptions::default()
    };
    let t = Instant::now();
    let gif_data = gif::encode(&frames, &delays, opts)?;
    let gif_ms = ms(t);
    write(&out.join("animated.gif"), &gif_data)?;
    let t = Instant::now();
    let mut enc = AnimEncoder::new(w, h, Options::lossy(75.0), 0)?;
    for (f, d) in frames.iter().zip(&delays) {
        enc.add(f, *d as u32 * 10)?;
    }
    let webp_data = enc.finish()?;
    let webp_ms = ms(t);
    write(&out.join("animated.webp"), &webp_data)?;
    say!(
        "animated: {} frames {w}x{h} decoded in {decoded:.1} ms, gif {gif_ms:.1} ms {} bytes, webp {webp_ms:.1} ms {} bytes",
        frames.len(),
        gif_data.len(),
        webp_data.len()
    )?;
    Ok(())
}
