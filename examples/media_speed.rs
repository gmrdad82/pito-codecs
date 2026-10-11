use std::path::PathBuf;
use std::time::Instant;

use pito_codecs::media::{
    Choice, ExportOptions, Piece, Prefs, Rgba, decode_video, export, first_frame, frames_at, probe,
    to_rgba, to_rgba_height,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .ok_or("usage: media_speed <file> [ffmpeg|auto]")?,
    );
    let prefs = match args.next().as_deref() {
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
    println!("probe {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    let t = Instant::now();
    let f = first_frame(&path, 240, &prefs)?;
    println!(
        "first frame {}x{} {:.1} ms",
        f.width,
        f.height,
        t.elapsed().as_secs_f64() * 1e3
    );
    let times: Vec<f64> = (0..12)
        .map(|k| p.duration * (k as f64 + 0.5) / 12.0)
        .collect();
    let t = Instant::now();
    let strip = frames_at(&path, &times, 240, &prefs)?;
    println!(
        "filmstrip {} stills {:.1} ms",
        strip.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
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
        println!(
            "decode {n} frames at {:?}: {:.1} fps",
            height.unwrap_or(p.height),
            n as f64 / s
        );
        if let Some(fr) = last {
            let t = Instant::now();
            let mut img = Rgba::new(0, 0);
            for _ in 0..20 {
                img = to_rgba(&fr);
            }
            println!(
                "  to_rgba {}x{}: {:.2} ms",
                img.width,
                img.height,
                t.elapsed().as_secs_f64() * 1e3 / 20.0
            );
            let t = Instant::now();
            for _ in 0..20 {
                img = to_rgba_height(&fr, 240);
            }
            println!(
                "  to_rgba_height 240 ({}x{}): {:.2} ms",
                img.width,
                img.height,
                t.elapsed().as_secs_f64() * 1e3 / 20.0
            );
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
    println!(
        "smart export {seconds:.1} s of video in {s:.2} s ({:.0}x realtime): {} frames, {} copied, {} encoded",
        seconds / s,
        done.frames,
        done.copied,
        done.encoded
    );
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
    println!(
        "overlay export {} frames in {s:.2} s: {:.1} fps",
        done.frames,
        done.frames as f64 / s
    );
    Ok(())
}
