#[macro_use]
#[path = "support/fail.rs"]
mod fail;

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use pito_codecs::alac::{Decoder, entry_cookie};
use pito_codecs::media::{Prefs, audio_pcm_range, probe};

type Packets = (Vec<u8>, Vec<Vec<u8>>);

fn packets(path: &Path) -> Result<Packets, Box<dyn Error>> {
    if path.extension().is_some_and(|e| e == "mkv") {
        let r = pito_codecs::mkv::Reader::open(path)?;
        let i = *r.audio().first().ok_or("no audio track")?;
        let t = &r.tracks[i];
        let pkts = (0..t.samples.len())
            .map(|k| r.read(i, k))
            .collect::<Result<_, _>>()?;
        Ok((t.codec_private.clone(), pkts))
    } else {
        let r = pito_codecs::mp4::Reader::open(path)?;
        let i = *r.audio().first().ok_or("no audio track")?;
        let t = &r.tracks[i];
        let cookie = t
            .entry()
            .and_then(entry_cookie)
            .ok_or("no ALAC magic cookie")?
            .to_vec();
        let pkts = (0..t.samples.len())
            .map(|k| r.read(i, k))
            .collect::<Result<_, _>>()?;
        Ok((cookie, pkts))
    }
}

fn main() -> ExitCode {
    fail::run("alac", || -> Result<(), Box<dyn Error>> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(input) = args.first() else {
            return Err("usage: alac <file.m4a|mov|mkv> [out.f32]".into());
        };
        let path = Path::new(input);
        let (cookie, pkts) = packets(path)?;
        let mut d = Decoder::new(&cookie)?;
        let (rate, channels, depth) = (d.rate(), d.channels(), d.bit_depth());
        let t = Instant::now();
        let mut frames = 0usize;
        for p in &pkts {
            frames += d.decode(p)?.len() / channels.max(1) as usize;
        }
        let secs = t.elapsed().as_secs_f64();
        let audio = frames as f64 / rate.max(1) as f64;
        note!(
            "decoder: {frames} frames, {rate} Hz, {channels} ch, {depth}-bit, {audio:.2} s of audio in {secs:.3} s ({:.0}x real time)",
            audio / secs.max(1e-9)
        );
        let prefs = Prefs::default();
        let a = probe(path, &prefs)?
            .audio
            .first()
            .cloned()
            .ok_or("no audio track")?;
        let t = Instant::now();
        let b = audio_pcm_range(path, 0, 0.0, f64::INFINITY, a.rate, a.channels, &prefs)?;
        let secs = t.elapsed().as_secs_f64();
        let frames = b.samples.len() / b.channels.max(1) as usize;
        let audio = frames as f64 / b.rate.max(1) as f64;
        note!(
            "audio_pcm_range: {frames} frames, {audio:.2} s of audio in {secs:.3} s ({:.0}x real time)",
            audio / secs.max(1e-9)
        );
        if let Some(out) = args.get(1) {
            let bytes: Vec<u8> = b.samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            std::fs::write(out, bytes)?;
        }
        Ok(())
    })
}
