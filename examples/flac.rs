#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::error::Error;
use std::io::Cursor;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use pito_codecs::flac::Reader;
use pito_codecs::media::{Prefs, audio_pcm_range, probe};

fn raw(data: &[u8]) -> Result<(Vec<f32>, u32, u16), Box<dyn Error>> {
    let mut r = Reader::new(Cursor::new(data))?;
    let (rate, channels) = (r.info().rate, u16::from(r.info().channels));
    let mut out = Vec::with_capacity((r.info().samples as usize).saturating_mul(channels as usize));
    while r.read_f32(&mut out)? > 0 {}
    Ok((out, rate, channels))
}

fn reference(path: &Path) -> Result<f64, Box<dyn Error>> {
    let t = Instant::now();
    let ok = Command::new("flac")
        .args([
            "-d",
            "-c",
            "--silent",
            "--force-raw-format",
            "--endian=little",
            "--sign=signed",
        ])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("flac: {e}"))?
        .success();
    if !ok {
        return Err(format!("flac -d failed on {}", path.display()).into());
    }
    Ok(t.elapsed().as_secs_f64())
}

fn main() -> ExitCode {
    fail::run("flac", || -> Result<(), Box<dyn Error>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let against = args.iter().any(|a| a == "--against-flac");
        let args: Vec<&String> = args.iter().filter(|a| *a != "--against-flac").collect();
        let Some(input) = args.first() else {
            return Err("usage: flac <file> [out.f32] [from to] [--against-flac]".into());
        };
        let path = Path::new(input.as_str());
        let bytes = std::fs::metadata(path)?.len();
        let bare = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("flac"));
        let data = if bare {
            std::fs::read(path)?
        } else {
            Vec::new()
        };
        let t = Instant::now();
        let (samples, rate, channels) = if bare {
            raw(&data)?
        } else {
            let prefs = Prefs::default();
            let a = probe(path, &prefs)?
                .audio
                .first()
                .cloned()
                .ok_or("no audio track")?;
            let from: f64 = args.get(2).map_or(Ok(0.0), |s| s.parse())?;
            let to: f64 = args.get(3).map_or(Ok(f64::INFINITY), |s| s.parse())?;
            let b = audio_pcm_range(path, 0, from, to, a.rate, a.channels, &prefs)?;
            (b.samples, b.rate, b.channels)
        };
        let secs = t.elapsed().as_secs_f64().max(1e-9);
        let frames = samples.len() / channels.max(1) as usize;
        let audio = frames as f64 / rate.max(1) as f64;
        note!(
            "{frames} frames, {rate} Hz, {channels} ch, {audio:.2} s of audio in {secs:.3} s ({:.0}x real time, {:.1} MB/s of FLAC)",
            audio / secs,
            bytes as f64 / secs / 1e6
        );
        if against {
            let theirs = reference(path)?.max(1e-9);
            note!(
                "flac -d: {theirs:.3} s ({:.0}x real time); pito-codecs is {:.2}x its speed",
                audio / theirs,
                theirs / secs
            );
        }
        if let Some(out) = args.get(1) {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(out.as_str(), bytes)?;
        }
        Ok(())
    })
}
