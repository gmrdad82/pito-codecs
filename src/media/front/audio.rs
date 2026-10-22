use std::path::Path;

use rayon::prelude::*;

use super::probe::probe;
use super::route::Prefs;
use crate::media::{AudioBuffer, Error, Result};

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
    _: &Prefs,
) -> Result<AudioBuffer> {
    if rate == 0 || channels == 0 {
        return Err(Error::Invalid(format!(
            "a sample rate of {rate} with {channels} channels"
        )));
    }
    if from.is_nan() || to.is_nan() || to < from {
        return Err(Error::Invalid(format!("the range {from} to {to}")));
    }
    super::base::audio_range(path, track, from, to, rate, channels)
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
    let longest = decoded.iter().map(|(b, _)| b.frames()).max().unwrap_or(0);
    let frames = if to.is_finite() {
        (((to * rate as f64).round() - (from.max(0.0) * rate as f64).round()).max(0.0) as usize)
            .min(longest)
    } else {
        longest
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
