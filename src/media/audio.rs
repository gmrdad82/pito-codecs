use std::path::Path;

use super::probe::container_ffmpeg;
use super::route::{Caps, Prefs, pick_decode};
use super::types::{AudioBuffer, Codec, Error, Result, Route};

pub fn audio_pcm(
    path: &Path,
    track: usize,
    rate: u32,
    mono: bool,
    prefs: &Prefs,
) -> Result<AudioBuffer> {
    if rate == 0 {
        return Err(Error::Invalid("a sample rate of 0".to_string()));
    }
    let picked = pick_decode(Codec::Aac, prefs, &Caps::detect(prefs))?;
    if picked.route != Route::Ffmpeg {
        match super::native::audio_pcm(path, track, rate, mono) {
            Ok(buf) => return Ok(buf),
            Err(e) if prefs.user_ffmpeg().is_none() => return Err(e),
            Err(_) => {}
        }
    }
    let f = container_ffmpeg(prefs)?;
    let channels: u16 = if mono { 1 } else { 2 };
    let args: Vec<String> = vec![
        "-i".into(),
        path.display().to_string(),
        "-map".into(),
        format!("0:a:{track}"),
        "-vn".into(),
        "-ac".into(),
        channels.to_string(),
        "-ar".into(),
        rate.to_string(),
        "-f".into(),
        "f32le".into(),
        "-".into(),
    ];
    let raw = f.run_args(&args)?;
    let mut buf = AudioBuffer::new(rate, channels);
    buf.samples = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(buf)
}
