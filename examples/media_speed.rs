#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use std::sync::Arc;

use pito_codecs::filter::{ColorSpec, Matrix, Overlay, Range};

use pito_codecs::media::{
    AudioBuffer, Blender, Canvas, Choice, Codec, ExportOptions, Frame, Piece, PixelFormat, Prefs,
    Rgba, Seek, Stills, TimeBase, VideoFrame, audio_mix_stream, audio_pcm, audio_stream, blend,
    decode_video, decode_video_gpu, export, export_frames, export_with_audio, first_frame,
    frame_count, frames_at, keyframes, probe, to_rgba, to_rgba_height,
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

fn main() -> ExitCode {
    fail::run("media_speed", run)
}

fn run() -> Res<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or(
        "usage: media_speed <file> [ffmpeg|auto|cpu|gpu|probe|cuts [n]|spans <h264|vp9> <from-to>...|scale <check> [arg]|frames [auto|cpu] [n]] | media_speed captions",
    )?);
    if path.as_os_str() == "captions" {
        return captions();
    }
    let mode = args.next();
    if mode.as_deref() == Some("keys") {
        let callers: usize = args.next().map_or(Ok(1), |n| n.parse())?;
        let rchar = || -> u64 {
            std::fs::read_to_string("/proc/self/io")
                .unwrap_or_default()
                .lines()
                .find_map(|l| l.strip_prefix("rchar:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)
        };
        let before = rchar();
        let t = Instant::now();
        let lists = std::thread::scope(|s| {
            let jobs: Vec<_> = (0..callers)
                .map(|_| s.spawn(|| keyframes(&path, &Prefs::default())))
                .collect();
            jobs.into_iter()
                .map(|j| {
                    j.join()
                        .map_err(|_| "a keyframes caller failed".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        let took = t.elapsed().as_secs_f64();
        let read = rchar().saturating_sub(before);
        for k in lists {
            let k = k?;
            let sum = k
                .iter()
                .fold(0u64, |a, t| a.wrapping_mul(31).wrapping_add(t.to_bits()));
            say!(
                "keys: {} in {:.1} ms, read {:.2} MB, first {:?} last {:?} sum {sum:x}",
                k.len(),
                took * 1e3,
                read as f64 / 1e6,
                k.first(),
                k.last()
            )?;
        }
        return Ok(());
    }
    if mode.as_deref() == Some("open") {
        let prefs = Prefs {
            decode: Choice::Cpu,
            encode: Choice::Cpu,
            ..Prefs::default()
        };
        let io = || -> (u64, u64) {
            let text = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
            let field = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0)
            };
            (field("rchar:"), field("syscr:"))
        };
        for at in args.by_ref() {
            let from: f64 = at.parse()?;
            let before = io();
            let t = Instant::now();
            let mut video = decode_video(&path, from, None, &prefs)?;
            let opened = t.elapsed().as_secs_f64();
            let f = video.next().ok_or("no frame")??;
            let sum = f.planes[0]
                .data
                .iter()
                .fold(0u64, |a, &b| a.wrapping_mul(31).wrapping_add(b as u64));
            let total = t.elapsed().as_secs_f64();
            let after = io();
            say!(
                "open at {from} s: open {:.1} ms, first frame {:.1} ms, read {:.2} MB in {} calls, pts {} sum {sum:x}",
                opened * 1e3,
                total * 1e3,
                after.0.saturating_sub(before.0) as f64 / 1e6,
                after.1.saturating_sub(before.1),
                f.pts
            )?;
        }
        return Ok(());
    }
    if mode.as_deref() == Some("frames") {
        let cpu = args.next().as_deref() == Some("cpu");
        let count = match args.next() {
            Some(n) => n.parse()?,
            None => 600,
        };
        return made_frames(&path, cpu, count);
    }
    if mode.as_deref() == Some("readback") {
        let choice = match args.next().as_deref() {
            Some("gpu") => Choice::Gpu,
            Some("auto") => Choice::Auto,
            _ => Choice::Cpu,
        };
        return readback(&path, choice);
    }
    if mode.as_deref() == Some("xport") {
        let pick = |a: Option<String>| match a.as_deref() {
            Some("gpu") => Choice::Gpu,
            Some("cpu") => Choice::Cpu,
            _ => Choice::Auto,
        };
        let decode = pick(args.next());
        let encode = pick(args.next());
        let overlay = args.next();
        return xport(&path, decode, encode, overlay.as_deref());
    }
    if mode.as_deref() == Some("exact") {
        let decode = match args.next().as_deref() {
            Some("gpu") => Choice::Gpu,
            Some("cpu") => Choice::Cpu,
            _ => Choice::Auto,
        };
        let pages = args.next().map(|n| n.parse()).transpose()?.unwrap_or(6);
        let per = args.next().map(|n| n.parse()).transpose()?.unwrap_or(4);
        let height = args.next().map(|n| n.parse()).transpose()?.unwrap_or(160);
        return exact_pages(&path, decode, pages, per, height);
    }
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
    if mode.as_deref() == Some("scale") {
        let what = args.next().unwrap_or_else(|| "probe".into());
        let arg = args.next();
        return scale(&path, &what, arg.as_deref());
    }
    if mode.as_deref() == Some("spans") {
        let video = match args.next().as_deref() {
            Some("vp9") => Codec::Vp9,
            _ => Codec::H264,
        };
        let mut pieces = Vec::new();
        for span in args.by_ref() {
            let (from, to) = span.split_once('-').ok_or("a span reads <from>-<to>")?;
            pieces.push(Piece {
                path: path.clone(),
                from: from.parse()?,
                to: to.parse()?,
            });
        }
        return timed(&pieces, video, "spans");
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

fn scale(path: &std::path::Path, what: &str, arg: Option<&str>) -> Res<()> {
    let prefs = Prefs {
        decode: Choice::Cpu,
        encode: Choice::Cpu,
        ..Prefs::default()
    };
    let t = Instant::now();
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1e3;
    match what {
        "probe" => {
            let p = probe(path, &prefs)?;
            say!(
                "probe {:.1} ms: {:.3} s, {}/{} fps, {:?} frames, {} audio",
                ms(t),
                p.duration,
                p.fps.0,
                p.fps.1,
                p.frames,
                p.audio.len()
            )?;
        }
        "keyframes" => {
            let k = keyframes(path, &prefs)?;
            say!(
                "keyframes {:.1} ms: {} keys, last {:.3}",
                ms(t),
                k.len(),
                k.last().copied().unwrap_or(0.0)
            )?;
        }
        "frames" => {
            let n = frame_count(path, &prefs)?;
            say!("frame_count {:.1} ms: {n}", ms(t))?;
        }
        "audio" => {
            let track = arg.map(str::parse).transpose()?.unwrap_or(0);
            let mut frames = 0usize;
            let mut sum = 0f64;
            for buf in audio_stream(path, track, 0.0, 48_000, 2, &prefs)? {
                let buf = buf?;
                frames += buf.frames();
                sum += buf.samples.iter().map(|s| *s as f64).sum::<f64>();
            }
            say!(
                "audio_stream track {track} {:.1} ms: {frames} frames ({:.3} s), sum {sum:.3}",
                ms(t),
                frames as f64 / 48_000.0
            )?;
        }
        "mix" => {
            let mut frames = 0usize;
            let mut sum = 0f64;
            for buf in audio_mix_stream(path, &[], 0.0, 48_000, &prefs)? {
                let buf = buf?;
                frames += buf.frames();
                sum += buf.samples.iter().map(|s| *s as f64).sum::<f64>();
            }
            say!(
                "audio_mix_stream {:.1} ms: {frames} frames ({:.3} s), sum {sum:.3}",
                ms(t),
                frames as f64 / 48_000.0
            )?;
        }
        "range" => {
            let track = arg.map(str::parse).transpose()?.unwrap_or(0);
            let buf = audio_pcm(path, track, 48_000, false, &prefs)?;
            say!(
                "audio_pcm track {track} {:.1} ms: {} frames",
                ms(t),
                buf.frames()
            )?;
        }
        "still" => {
            let at: f64 = arg.map(str::parse).transpose()?.unwrap_or(0.9);
            let p = probe(path, &prefs)?;
            let t = Instant::now();
            let s = frames_at(path, &[p.duration * at], 240, &prefs)?;
            say!(
                "still at {:.1} s {:.1} ms: {} stills",
                p.duration * at,
                ms(t),
                s.len()
            )?;
        }
        "export" => {
            let p = probe(path, &prefs)?;
            let out = PathBuf::from(arg.unwrap_or("tmp/scale-copy.mp4"));
            let done = export(
                &out,
                &[Piece {
                    path: path.to_path_buf(),
                    from: 0.0,
                    to: p.duration,
                }],
                ExportOptions {
                    prefs: prefs.clone(),
                    ..ExportOptions::default()
                },
                |_| true,
            )?;
            let t2 = Instant::now();
            let back = probe(&out, &prefs)?;
            say!(
                "export {:.1} ms: {} frames, {} copied; probe of it {:.1} ms: {:.3} s, {:?} frames",
                ms(t),
                done.frames,
                done.copied,
                ms(t2),
                back.duration,
                back.frames
            )?;
        }
        "index" => {
            let p = probe(path, &prefs)?;
            let t = Instant::now();
            let mut video = decode_video(path, p.duration * 0.9, None, &prefs)?;
            let got = video.next().transpose()?.is_some();
            say!("decode_video at 90 % {:.1} ms: frame {got}", ms(t))?;
        }
        _ => return Err(format!("no scale check {what}").into()),
    }
    peak()
}

fn peak() -> Res<()> {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status")
        && let Some(peak) = status.lines().find(|l| l.starts_with("VmHWM"))
    {
        say!("  peak RSS {}", peak.trim_start_matches("VmHWM:").trim())?;
    }
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
    timed(&pieces, Codec::H264, &format!("{n} cuts"))
}

fn timed(pieces: &[Piece], video: Codec, what: &str) -> Res<()> {
    let prefs = match std::env::var("ROUTE").as_deref() {
        Ok("gpu") => Prefs {
            decode: Choice::Gpu,
            encode: Choice::Gpu,
            ..Prefs::default()
        },
        Ok("cpu") => Prefs {
            decode: Choice::Cpu,
            encode: Choice::Cpu,
            ..Prefs::default()
        },
        _ => Prefs::default(),
    };
    let seconds: f64 = pieces.iter().map(|x| x.to - x.from).sum();
    let out = PathBuf::from("tmp/speed-cuts.mp4");
    let t = Instant::now();
    let mut at = 0usize;
    let done = export_with_audio(
        &out,
        pieces,
        ExportOptions {
            video,
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
        "{what}, {seconds:.1} s of video in {s:.2} s ({:.0}x realtime, {:.0} fps) {} on {}: {} frames, {} copied, {} encoded, smart {}",
        seconds / s,
        done.frames as f64 / s,
        done.video.codec,
        done.video.route,
        done.frames,
        done.copied,
        done.encoded,
        done.smart
    )?;
    for r in &done.reasons {
        say!("  {r}")?;
    }
    peak()
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

fn made_frames(out: &std::path::Path, cpu: bool, count: u64) -> Res<()> {
    let (w, h) = (1080u32, 1920u32);
    let looks: Vec<Frame> = (0..8)
        .map(|k| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, 0, TimeBase::new(1, 60));
            let stride = f.planes[0].stride;
            for (y, row) in f.planes[0].data.chunks_mut(stride).enumerate() {
                for (x, px) in row.iter_mut().take(w as usize).enumerate() {
                    *px = (16 + (x / 4 + y / 4 + k * 9) % 200) as u8;
                }
            }
            for (i, p) in f.planes[1..].iter_mut().enumerate() {
                p.data.fill(100 + 40 * i as u8 + k as u8 * 4);
            }
            f
        })
        .collect();
    let mut mark = Rgba::new(w, h);
    for (i, px) in mark.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        if x > w - 300 && y > h - 120 {
            px.copy_from_slice(&[255, 255, 255, 160]);
        }
    }
    let mark = Arc::new(mark);
    let mut frame = looks[0].clone();
    let t = Instant::now();
    for _ in 0..60 {
        blend(&mut frame, &mark);
    }
    let fresh = t.elapsed().as_secs_f64() * 1e3 / 60.0;
    let mut blender = Blender::new();
    blender.draw(&mut frame, &mark);
    let t = Instant::now();
    for _ in 0..600 {
        blender.draw(&mut frame, &mark);
    }
    let kept = t.elapsed().as_secs_f64() * 1e3 / 600.0;
    say!(
        "blend {w}x{h}: {fresh:.2} ms a frame cloning the overlay, {kept:.3} ms with a kept Blender"
    )?;
    let prefs = if cpu {
        Prefs {
            decode: Choice::Cpu,
            encode: Choice::Cpu,
            ..Prefs::default()
        }
    } else {
        Prefs::default()
    };
    let canvas = Canvas {
        width: w,
        height: h,
        fps: (60, 1),
        frames: Some(count),
    };
    let mut at = 0usize;
    let t = Instant::now();
    let done = export_frames(
        out,
        canvas,
        ExportOptions {
            prefs,
            ..ExportOptions::default()
        },
        |n, spent| {
            if n >= count {
                return Ok(None);
            }
            let look = &looks[n as usize % looks.len()];
            let mut f = spent.unwrap_or_else(|| look.clone());
            for (d, s) in f.planes.iter_mut().zip(&look.planes) {
                d.data.copy_from_slice(&s.data);
            }
            Ok(Some(VideoFrame::Cpu(f)))
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
        "export_frames {w}x{h} at 60 fps: {} frames in {s:.2} s ({:.0} fps) on {}",
        done.frames,
        done.frames as f64 / s,
        done.video.said()
    )?;
    peak()
}

fn caption_layer(w: u32, h: u32, lit: usize) -> Rgba {
    let mut img = Rgba::new(w, h);
    let line = (h as f64 * 0.1).max(4.0);
    let cell = line * 0.95;
    let r = line * 0.4;
    let edge = (line * 0.06).max(1.0);
    let left = w as f64 * 0.1;
    let right = w as f64 * 0.9;
    let tops = [0.0, 1.1, 2.2].map(|k| h as f64 * 0.6 + line * k);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i as u32 % w) as f64 + 0.5, (i as u32 / w) as f64 + 0.5);
        if x < left || x >= right {
            continue;
        }
        let Some(row) = tops.iter().position(|&t| y >= t && y < t + line) else {
            continue;
        };
        let k = ((x - left) / cell) as usize;
        let (gx, gy) = (left + (k as f64 + 0.5) * cell, tops[row] + line * 0.5);
        let (dx, dy) = (x - gx, y - gy);
        let d = match k % 3 {
            0 => (dx * dx + dy * dy).sqrt() - r,
            1 => ((dx * dx + dy * dy).sqrt() - r * 0.7).abs() - r * 0.3,
            _ => {
                let cy = dy.clamp(-r * 0.6, r * 0.6);
                (dx * dx + (dy - cy) * (dy - cy)).sqrt() - r * 0.4
            }
        };
        let alpha = (0.5 - d).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        let word = row * 64 + k / 4;
        let fill = if word < lit {
            [255.0, 220.0, 0.0]
        } else {
            [255.0, 255.0, 255.0]
        };
        let t = (d + edge + 0.5).clamp(0.0, 1.0);
        for c in 0..3 {
            px[c] = (fill[c] * (1.0 - t)).round() as u8;
        }
        px[3] = (alpha * 255.0).round() as u8;
    }
    img
}

fn full_layer(w: u32, h: u32, shift: u32) -> Rgba {
    let mut img = Rgba::new(w, h);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        px.copy_from_slice(&[
            (x * 7 + shift) as u8,
            (y * 5 + x / 3) as u8,
            (x ^ y).wrapping_add(shift) as u8,
            (64 + (x + y + shift) % 192) as u8,
        ]);
    }
    img
}

fn captions() -> Res<()> {
    for (w, h) in [(1920u32, 804u32), (1920, 1080), (3840, 2160)] {
        for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
            let mut frame = Frame::new(format, w, h, 0, TimeBase::new(1, 30));
            for (k, p) in frame.planes.iter_mut().enumerate() {
                let v: u16 = if k == 0 { 300 } else { 512 };
                match format {
                    PixelFormat::Yuv420p8 => p.data.fill((v / 4) as u8),
                    PixelFormat::Yuv420p10 => p
                        .data
                        .chunks_exact_mut(2)
                        .for_each(|s| s.copy_from_slice(&v.to_le_bytes())),
                }
            }
            for (name, layers) in [
                (
                    "caption band",
                    [caption_layer(w, h, 3), caption_layer(w, h, 4)],
                ),
                ("full layer", [full_layer(w, h, 0), full_layer(w, h, 1)]),
            ] {
                let painted = layers[0].data.chunks_exact(4).filter(|p| p[3] > 0).count();
                let layers = layers.map(Arc::new);
                let mut blender = Blender::new();
                blender.draw(&mut frame, &layers[1]);
                let n = if w * h > 4_000_000 { 20 } else { 40 };
                let t = Instant::now();
                for i in 0..n {
                    blender.draw(&mut frame, &layers[i % 2]);
                }
                let change = t.elapsed().as_secs_f64() * 1e3 / n as f64;
                let t = Instant::now();
                for _ in 0..200 {
                    blender.draw(&mut frame, &layers[(n - 1) % 2]);
                }
                let kept = t.elapsed().as_secs_f64() * 1e3 / 200.0;
                let spec = ColorSpec::new(Matrix::Bt709, Range::Limited);
                let t = Instant::now();
                for i in 0..n {
                    std::hint::black_box(Overlay::new(&layers[i % 2], spec, format)?);
                }
                let new = t.elapsed().as_secs_f64() * 1e3 / n as f64;
                say!(
                    "blend {name} {w}x{h} {format:?} ({:.1}% painted): Overlay::new {new:.2} ms, {change:.2} ms a layer change, {kept:.3} ms a kept frame",
                    painted as f64 * 100.0 / (w * h) as f64
                )?;
            }
        }
    }
    Ok(())
}

fn engines() -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc/self/fdinfo") else {
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for e in dir.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let id = text
            .lines()
            .find(|l| l.starts_with("drm-client-id:"))
            .map(str::to_string);
        let Some(id) = id else { continue };
        if !seen.insert(id) {
            continue;
        }
        for l in text.lines() {
            if let Some(rest) = l.strip_prefix("drm-engine-")
                && let Some((name, v)) = rest.split_once(':')
                && let Some(ns) = v
                    .trim()
                    .strip_suffix(" ns")
                    .and_then(|n| n.parse::<u64>().ok())
            {
                match out.iter_mut().find(|(n, _)| n == name) {
                    Some(slot) => slot.1 += ns,
                    None => out.push((name.to_string(), ns)),
                }
            }
        }
    }
    out
}

fn readback(path: &std::path::Path, choice: Choice) -> Res<()> {
    let prefs = Prefs {
        decode: choice,
        ..Prefs::default()
    };
    let t = Instant::now();
    let mut video = decode_video(path, 0.0, None, &prefs)?;
    let opened = t.elapsed().as_secs_f64();
    let before = engines();
    let mut n = 0u64;
    let mut last = None;
    let hash = std::env::var_os("HASH").is_some();
    let mut sum = 0xcbf2_9ce4_8422_2325u64;
    while let Some(fr) = video.next() {
        let fr = fr?;
        if hash {
            for pl in &fr.planes {
                let bpp = if fr.format == PixelFormat::Yuv420p10 {
                    2
                } else {
                    1
                };
                for row in pl.data.chunks(pl.stride.max(1)).take(pl.height) {
                    for &b in &row[..(pl.width * bpp).min(row.len())] {
                        sum = (sum ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
                    }
                }
            }
        }
        if let Some(old) = last.replace(fr) {
            video.recycle(old);
        }
        n += 1;
    }
    let s = t.elapsed().as_secs_f64();
    let after = engines();
    if hash {
        say!("  hash {sum:016x}")?;
    }
    say!(
        "{:?} {} {n} frames in {s:.2} s (open {:.0} ms): {:.1} fps",
        video.route,
        path.display(),
        opened * 1e3,
        n as f64 / s
    )?;
    let busy: Vec<String> = after
        .iter()
        .map(|(name, v)| {
            let b = before.iter().find(|(m, _)| m == name).map_or(0, |x| x.1);
            format!("{name} {:.1} ms", (v - b) as f64 / 1e6)
        })
        .collect();
    say!("  engines: {}", busy.join(", "))?;
    Ok(())
}

fn mark(w: u32, h: u32) -> Rgba {
    let mut m = Rgba::new(w, h);
    let (mw, mh) = ((w / 8).max(16), (h / 12).max(8));
    let (x0, y0) = (w - mw - w / 40, h - mh - h / 30);
    let r = mh as f32 / 2.0;
    for y in 0..mh {
        for x in 0..mw {
            let cx = (x as f32 + 0.5).clamp(r, mw as f32 - r);
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - r).powi(2)).sqrt();
            let a = ((r - d + 0.5).clamp(0.0, 1.0) * 200.0) as u8;
            let stripe = if (x / 6 + y / 6) % 2 == 0 { 255 } else { 40 };
            let o = (((y0 + y) * w + x0 + x) * 4) as usize;
            m.data[o..o + 4].copy_from_slice(&[stripe, 220, 255 - stripe / 2, a]);
        }
    }
    m
}

fn caption(w: u32, h: u32, word: u64) -> Rgba {
    let mut m = Rgba::new(w, h);
    let (bh, y0) = (h / 10, h - h / 10 - h / 20);
    let words = 3 + (word % 5) as u32;
    let span = w * 3 / 5;
    let x0 = (w - span) / 2;
    for y in 0..bh {
        for x in 0..span {
            let k = x * words / span;
            let edge = x % (span / words) < 4 || y < 3 || y + 3 >= bh;
            let o = (((y0 + y) * w + x0 + x) * 4) as usize;
            let px = if edge {
                [0, 0, 0, 160]
            } else {
                let lit = (k as u64 + word).is_multiple_of(3);
                if lit {
                    [255, 230, 40, 235]
                } else {
                    [250, 250, 250, 220]
                }
            };
            m.data[o..o + 4].copy_from_slice(&px);
        }
    }
    m
}

fn xport(path: &std::path::Path, decode: Choice, encode: Choice, overlay: Option<&str>) -> Res<()> {
    let prefs = Prefs {
        decode,
        encode,
        ..Prefs::default()
    };
    let p = probe(path, &prefs)?;
    let out = PathBuf::from(std::env::var("OUT").unwrap_or_else(|_| "tmp/rb/xport.mp4".into()));
    let crf = std::env::var("CRF")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(18);
    let seconds = std::env::var("SECONDS")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(p.duration);
    let layer = std::sync::Arc::new(Rgba::new(p.width, p.height));
    let marked = std::sync::Arc::new(mark(p.width & !1, p.height & !1));
    let (w, h) = (p.width & !1, p.height & !1);
    let mut said: Option<(u64, Arc<Rgba>)> = None;
    let overlay: Option<pito_codecs::media::Overlay> = match overlay {
        Some("full") | Some("clear") => Some(Box::new(move |_, _, _| Some(layer.clone()))),
        Some("empty") => Some(Box::new(|_, _, _| None)),
        Some("mark") => Some(Box::new(move |_, _, _| Some(marked.clone()))),
        Some("captions") => Some(Box::new(move |t, _, _| {
            let word = (t * 3.0) as u64;
            if word % 7 == 6 {
                return None;
            }
            match &said {
                Some((n, l)) if *n == word => Some(l.clone()),
                _ => {
                    let l = Arc::new(caption(w, h, word));
                    said = Some((word, l.clone()));
                    Some(l)
                }
            }
        })),
        _ => None,
    };
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut prev = engines();
            let mut window: std::collections::VecDeque<(Instant, Vec<(String, u64)>)> =
                std::collections::VecDeque::new();
            let mut worst: Vec<(String, u64, u64)> = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
                let now = engines();
                let at = Instant::now();
                for (name, v) in &now {
                    let b = prev.iter().find(|(m, _)| m == name).map_or(0, |x| x.1);
                    let step = v.saturating_sub(b);
                    match worst.iter_mut().find(|(n, _, _)| n == name) {
                        Some(slot) => slot.1 = slot.1.max(step),
                        None => worst.push((name.clone(), step, 0)),
                    }
                }
                window.push_back((at, now.clone()));
                while window
                    .front()
                    .is_some_and(|(t, _)| at.duration_since(*t).as_micros() > 16_700)
                {
                    window.pop_front();
                }
                if let Some((_, first)) = window.front() {
                    for (name, v) in &now {
                        let b = first.iter().find(|(m, _)| m == name).map_or(0, |x| x.1);
                        if let Some(slot) = worst.iter_mut().find(|(n, _, _)| n == name) {
                            slot.2 = slot.2.max(v.saturating_sub(b));
                        }
                    }
                }
                prev = now;
            }
            worst
        })
    };
    let before = engines();
    let cpu_before = cpu_seconds();
    let t = Instant::now();
    let mut route = String::new();
    let done = export(
        &out,
        &[Piece {
            path: path.to_path_buf(),
            from: 0.0,
            to: seconds.min(p.duration),
        }],
        ExportOptions {
            prefs,
            overlay,
            crf,
            video: if std::env::var("VIDEO").as_deref() == Ok("hevc") {
                Codec::Hevc
            } else {
                Codec::H264
            },
            ..ExportOptions::default()
        },
        |pr| {
            if route.is_empty() {
                route = format!("{:?}", pr.encoder);
            }
            true
        },
    )?;
    let s = t.elapsed().as_secs_f64();
    let cpu = cpu_seconds() - cpu_before;
    let after = engines();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let worst = sampler.join().unwrap_or_default();
    say!(
        "export {} enc {route}: {} frames ({} encoded) in {s:.2} s: {:.1} fps",
        path.display(),
        done.frames,
        done.encoded,
        done.frames as f64 / s
    )?;
    say!("  cpu {cpu:.1} s ({:.1} cores busy)", cpu / s)?;
    for r in &done.reasons {
        say!("  why: {r}")?;
    }
    let busy: Vec<String> = after
        .iter()
        .map(|(name, v)| {
            let b = before.iter().find(|(m, _)| m == name).map_or(0, |x| x.1);
            let per = (v - b) as f64 / 1e6 / done.frames.max(1) as f64;
            let (ms, win) = worst
                .iter()
                .find(|(n, _, _)| n == name)
                .map_or((0.0, 0.0), |w| (w.1 as f64 / 1e6, w.2 as f64 / 1e6));
            format!(
                "{name} {:.1} ms ({per:.2} ms/frame, worst 1 ms tick {ms:.2} ms, worst 16.7 ms window {win:.1} ms)",
                (v - b) as f64 / 1e6
            )
        })
        .collect();
    say!("  engines: {}", busy.join("; "))?;
    Ok(())
}

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or(Vec::new(), |(_, rest)| rest.split_whitespace().collect());
    let tick = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    (tick(11) + tick(12)) / 100.0
}

fn exact_pages(
    path: &std::path::Path,
    decode: Choice,
    pages: usize,
    per: usize,
    height: u32,
) -> Res<()> {
    let prefs = Prefs {
        decode,
        ..Prefs::default()
    };
    let p = probe(path, &prefs)?;
    let n = (pages * per).max(1);
    let times: Vec<f64> = (0..n)
        .map(|k| p.duration * (k as f64 + 0.37) / n as f64)
        .collect();
    let t = Instant::now();
    let stills = Stills::open(path, height, &prefs)?;
    let opened = t.elapsed().as_secs_f64() * 1e3;
    let mut all = Vec::new();
    for (i, page) in times.chunks(per.max(1)).enumerate() {
        let t = Instant::now();
        let mut got = 0usize;
        let mut failed = None;
        let mut sums = vec![0u64; page.len()];
        stills.frames(page, Seek::Exact, |k, r| match r {
            Ok(img) => {
                got += 1;
                if let Some(slot) = sums.get_mut(k) {
                    *slot = img
                        .data
                        .iter()
                        .fold(0u64, |a, &b| a.wrapping_mul(31).wrapping_add(b as u64));
                }
            }
            Err(e) => failed = Some(e.to_string()),
        })?;
        if let Some(e) = failed {
            return Err(e.into());
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        all.push(ms);
        let sums: Vec<String> = sums.iter().map(|s| format!("{s:016x}")).collect();
        say!(
            "page {i}: {got} exact stills at {page:.2?} in {ms:.1} ms, sums {}",
            sums.join(" ")
        )?;
    }
    let sum: f64 = all.iter().sum();
    say!("open {opened:.1} ms, {} pages in {sum:.1} ms", all.len())?;
    Ok(())
}
