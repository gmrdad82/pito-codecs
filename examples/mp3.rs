#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use pito_codecs::media::{Prefs, audio_pcm_range, probe};
use pito_codecs::mp3::{Decoder, MAX_SAMPLES_PER_FRAME};

fn raw(data: &[u8]) -> (Vec<f32>, u32, u16) {
    let mut dec = Decoder::new();
    let mut pcm = vec![0.0f32; MAX_SAMPLES_PER_FRAME];
    let mut out = Vec::new();
    let (mut rate, mut channels) = (0, 0);
    let mut rest = data;
    while !rest.is_empty() {
        let f = dec.decode(rest, &mut pcm);
        if f.frame_bytes == 0 {
            break;
        }
        if f.samples > 0 {
            rate = f.rate;
            channels = f.channels;
            out.extend_from_slice(&pcm[..f.samples * f.channels as usize]);
        }
        rest = &rest[f.frame_bytes.min(rest.len())..];
    }
    (out, rate, channels)
}

fn main() -> ExitCode {
    fail::run("mp3", || -> Result<(), Box<dyn Error>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(input) = args.first() else {
            return Err("usage: mp3 <file> [out.f32] [from to]".into());
        };
        let path = Path::new(input);
        let t = Instant::now();
        let (samples, rate, channels) = if path
            .extension()
            .is_some_and(|e| e == "mp3" || e == "mp2" || e == "mp1")
        {
            raw(&std::fs::read(path)?)
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
        let secs = t.elapsed().as_secs_f64();
        let frames = samples.len() / channels.max(1) as usize;
        let audio = frames as f64 / rate.max(1) as f64;
        note!(
            "{frames} frames, {rate} Hz, {channels} ch, {audio:.2} s of audio in {:.3} s ({:.0}x real time)",
            secs,
            audio / secs.max(1e-9)
        );
        if let Some(out) = args.get(1) {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(out, bytes)?;
        }
        Ok(())
    })
}
