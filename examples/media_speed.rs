#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use pito_codecs::media::{
    AudioBuffer, Choice, ExportOptions, Piece, Prefs, Rgba, VideoFrame, decode_video,
    decode_video_gpu, export, export_with_audio, first_frame, frames_at, probe, to_rgba,
    to_rgba_height,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    fail::run("media_speed", run)
}

fn run() -> Res<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: media_speed <file> [ffmpeg|auto|cpu|gpu|probe|cuts [n]]")?,
    );
    let mode = args.next();
    if mode.as_deref() == Some("gpu") {
        return gpu_playback(&path);
    }
    if mode.as_deref() == Some("probe") {
        let t = Instant::now();
        let p = probe(&path, &Prefs::default())?;
        say!(
            "probe {:.1} ms: {:.3} s, {}/{} fps, {:?} frames",
            t.elapsed().as_secs_f64() * 1e3,
            p.duration,
            p.fps.0,
            p.fps.1,
            p.frames
        )?;
        return Ok(());
    }
    if mode.as_deref() == Some("cuts") {
        let n = match args.next() {
            Some(n) => n.parse()?,
            None => 300,
        };
        return cuts(&path, n);
    }
    let prefs = match mode.as_deref() {
        Some("auto") => Prefs::default(),
        Some("cpu") => Prefs {
            decode: Choice::Cpu,
            encode: Choice::Cpu,
            ..Prefs::default()
        },
        _ => Prefs::ffmpeg(),
    };
    let t = Instant::now();
    let p = probe(&path, &prefs)?;
    say!("probe {:.1} ms", t.elapsed().as_secs_f64() * 1e3)?;
    let t = Instant::now();
    let f = first_frame(&path, 240, &prefs)?;
    say!(
        "first frame {}x{} {:.1} ms",
        f.width,
        f.height,
        t.elapsed().as_secs_f64() * 1e3
    )?;
    let times: Vec<f64> = (0..12)
        .map(|k| p.duration * (k as f64 + 0.5) / 12.0)
        .collect();
    let t = Instant::now();
    let strip = frames_at(&path, &times, 240, &prefs)?;
    say!(
        "filmstrip {} stills {:.1} ms",
        strip.len(),
        t.elapsed().as_secs_f64() * 1e3
    )?;
    for height in [None, Some(1080)] {
        let t = Instant::now();
        let mut n = 0;
        let mut last = None;
        let mut video = decode_video(&path, 10.0, height, &prefs)?;
        while n < 300 {
            let Some(fr) = video.next() else { break };
            if let Some(old) = last.replace(fr?) {
                video.recycle(old);
            }
            n += 1;
        }
        let s = t.elapsed().as_secs_f64();
        say!(
            "decode {n} frames at {:?}: {:.1} fps",
            height.unwrap_or(p.height),
            n as f64 / s
        )?;
        if let Some(fr) = last {
            let t = Instant::now();
            let mut img = Rgba::new(0, 0);
            for _ in 0..20 {
                img = to_rgba(&fr);
            }
            say!(
                "  to_rgba {}x{}: {:.2} ms",
                img.width,
                img.height,
                t.elapsed().as_secs_f64() * 1e3 / 20.0
            )?;
            let t = Instant::now();
            for _ in 0..20 {
                img = to_rgba_height(&fr, 240);
            }
            say!(
                "  to_rgba_height 240 ({}x{}): {:.2} ms",
                img.width,
                img.height,
                t.elapsed().as_secs_f64() * 1e3 / 20.0
            )?;
        }
    }
    let out = PathBuf::from("tmp/speed-smart.mp4");
    let pieces: Vec<Piece> = [(0.2, 0.45), (0.1, 0.3), (0.2, 0.45)]
        .iter()
        .map(|(a, b)| Piece {
            path: path.clone(),
            from: p.duration * a,
            to: p.duration * b,
        })
        .collect();
    let seconds: f64 = pieces.iter().map(|x| x.to - x.from).sum();
    let t = Instant::now();
    let done = export(
        &out,
        &pieces,
        ExportOptions {
            prefs: prefs.clone(),
            ..ExportOptions::default()
        },
        |_| true,
    )?;
    let s = t.elapsed().as_secs_f64();
    say!(
        "smart export {seconds:.1} s of video in {s:.2} s ({:.0}x realtime): {} frames, {} copied, {} encoded",
        seconds / s,
        done.frames,
        done.copied,
        done.encoded
    )?;
    let layer = std::sync::Arc::new(Rgba::new(p.width, p.height));
    let t = Instant::now();
    let out = PathBuf::from("tmp/speed-overlay.mp4");
    let done = export(
        &out,
        &pieces[..1],
        ExportOptions {
            prefs: prefs.clone(),
            overlay: Some(Box::new(move |_, _, _| Some(layer.clone()))),
            ..ExportOptions::default()
        },
        |_| true,
    )?;
    let s = t.elapsed().as_secs_f64();
    say!(
        "overlay export {} frames in {s:.2} s: {:.1} fps",
        done.frames,
        done.frames as f64 / s
    )?;
    Ok(())
}

fn cuts(path: &std::path::Path, n: usize) -> Res<()> {
    let prefs = Prefs::default();
    let p = probe(path, &prefs)?;
    let slot = p.duration / n.max(1) as f64;
    let pieces: Vec<Piece> = (0..n)
        .map(|k| {
            let from = k as f64 * slot + slot * ((k * 13 % 7) as f64 / 20.0);
            let len = slot * (0.05 + (k * 37 % 57) as f64 / 100.0);
            Piece {
                path: path.to_path_buf(),
                from,
                to: (from + len).min(p.duration),
            }
        })
        .collect();
    let seconds: f64 = pieces.iter().map(|x| x.to - x.from).sum();
    let out = PathBuf::from("tmp/speed-cuts.mp4");
    let t = Instant::now();
    let mut at = 0usize;
    let done = export_with_audio(
        &out,
        &pieces,
        ExportOptions {
            prefs,
            ..ExportOptions::default()
        },
        |_, frames| {
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = (at..at + frames)
                .flat_map(|k| {
                    let s = (k as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32;
                    [s * 0.25, s * 0.25]
                })
                .collect();
            at += frames;
            Ok(buf)
        },
        |_| true,
    )?;
    let s = t.elapsed().as_secs_f64();
    say!(
        "{n} cuts, {seconds:.1} s of video in {s:.2} s ({:.0}x realtime) on {}: {} frames, {} copied, {} encoded, smart {}",
        seconds / s,
        done.video.route,
        done.frames,
        done.copied,
        done.encoded,
        done.smart
    )?;
    for r in &done.reasons {
        say!("  {r}")?;
    }
    Ok(())
}

fn gpu_playback(path: &std::path::Path) -> Res<()> {
    let prefs = Prefs::default();
    let p = probe(path, &prefs)?;
    for size in [None, Some((1920, 1080)), Some((1280, 720))] {
        for touch in 0..3 {
            let t = Instant::now();
            let mut video = decode_video_gpu(path, 0.0, size, &prefs)?;
            let mut first = None;
            let mut held = std::collections::VecDeque::new();
            let mut n = 0;
            while let Some(f) = video.next() {
                let f = f?;
                if let VideoFrame::Gpu(g) = &f {
                    match touch {
                        1 => g.sync()?,
                        #[cfg(target_os = "linux")]
                        2 => drop(g.dmabuf(false)?),
                        _ => {}
                    }
                }
                first.get_or_insert_with(|| t.elapsed().as_secs_f64() * 1e3);
                held.push_back(f);
                if held.len() > 3
                    && let Some(old) = held.pop_front()
                {
                    video.recycle(old);
                }
                n += 1;
            }
            let s = t.elapsed().as_secs_f64();
            let (w, h) = size.unwrap_or((p.width, p.height));
            say!(
                "gpu playback {}x{} -> {w}x{h} {}: {n} frames, first {:.1} ms, {:.1} fps ({}, {})",
                p.width,
                p.height,
                ["frames only", "sync each", "dmabuf each"][touch],
                first.unwrap_or(0.0),
                n as f64 / s,
                video.route,
                if video.on_gpu() {
                    "on the GPU"
                } else {
                    "in memory"
                }
            )?;
        }
    }
    Ok(())
}
