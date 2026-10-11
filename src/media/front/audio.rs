use std::path::Path;

use rayon::prelude::*;

use super::probe::{container_ffmpeg, probe};
use super::route::{Caps, Prefs, pick_decode};
use crate::media::{AudioBuffer, Codec, Error, Result, Route};

pub fn audio_pcm(
    path: &Path,
    track: usize,
    rate: u32,
    mono: bool,
    prefs: &Prefs,
) -> Result<AudioBuffer> {
    let channels = if mono { 1 } else { 2 };
    audio_pcm_range(path, track, 0.0, f64::INFINITY, rate, channels, prefs)
}

pub fn audio_pcm_range(
    path: &Path,
    track: usize,
    from: f64,
    to: f64,
    rate: u32,
    channels: u16,
    prefs: &Prefs,
) -> Result<AudioBuffer> {
    if rate == 0 || channels == 0 {
        return Err(Error::Invalid(format!(
            "a sample rate of {rate} with {channels} channels"
        )));
    }
    if from.is_nan() || to.is_nan() || to < from {
        return Err(Error::Invalid(format!("the range {from} to {to}")));
    }
    let picked = pick_decode(Codec::Aac, prefs, &Caps::detect(prefs))?;
    if picked.route != Route::Ffmpeg {
        match super::native::audio_range(path, track, from, to, rate, channels) {
            Ok(buf) => return Ok(buf),
            Err(e) if prefs.user_ffmpeg().is_none() => return Err(e),
            Err(_) => {}
        }
    }
    let f = container_ffmpeg(prefs)?;
    let from = from.max(0.0);
    let start = (from * rate as f64).round() as usize;
    let end = to.is_finite().then(|| (to * rate as f64).round() as usize);
    if end.is_some_and(|e| e <= start) {
        return Ok(AudioBuffer::new(rate, channels));
    }
    let mut args: Vec<String> = Vec::new();
    if from > 0.0 {
        args.extend(["-ss".into(), format!("{from:.6}")]);
    }
    if end.is_some() {
        args.extend(["-t".into(), format!("{:.6}", to - from)]);
    }
    args.extend([
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
    ]);
    let raw = f.run_args(&args)?;
    let mut buf = AudioBuffer::new(rate, channels);
    buf.samples = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(super::native::cut(
        buf,
        0,
        end.map(|e| e.saturating_sub(start)),
    ))
}

pub fn audio_mix(
    path: &Path,
    gains: &[f32],
    from: f64,
    to: f64,
    rate: u32,
    prefs: &Prefs,
) -> Result<AudioBuffer> {
    if from.is_nan() || to.is_nan() || to < from {
        return Err(Error::Invalid(format!("the range {from} to {to}")));
    }
    let tracks: Vec<(usize, f32)> = probe(path, prefs)?
        .audio
        .iter()
        .map(|t| (t.index, gains.get(t.index).copied().unwrap_or(1.0)))
        .filter(|&(_, g)| g > 0.0)
        .collect();
    let decoded: Vec<(AudioBuffer, f32)> = tracks
        .par_iter()
        .map(|&(t, g)| audio_pcm_range(path, t, from, to, rate, 2, prefs).map(|b| (b, g)))
        .collect::<Result<_>>()?;
    let frames = if to.is_finite() {
        ((to * rate as f64).round() - (from.max(0.0) * rate as f64).round()).max(0.0) as usize
    } else {
        decoded.iter().map(|(b, _)| b.frames()).max().unwrap_or(0)
    };
    let mut out = AudioBuffer::new(rate, 2);
    let samples = frames
        .checked_mul(2)
        .ok_or_else(|| Error::Invalid(format!("the range {from} to {to}")))?;
    out.samples = vec![0.0; samples];
    for (b, g) in &decoded {
        for (o, s) in out.samples.iter_mut().zip(&b.samples) {
            *o += s * g;
        }
    }
    Ok(out)
}
