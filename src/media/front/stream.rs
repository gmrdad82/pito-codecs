use std::io::Read;
use std::path::Path;
use std::sync::mpsc;

use crate::aac::Resampler;

use super::base::{self as native, PAD, shape};
use super::feed::Feed;
use super::ffmpeg::Child;
use super::probe::{container_ffmpeg, probe};
use super::route::{Caps, Prefs, pick_decode};
use super::sound::Sound;
use crate::media::{AudioBuffer, Codec, Error, Result, Route};

pub struct AudioStream {
    pub route: Route,
    pub rate: u32,
    pub channels: u16,
    inner: Inner,
}

impl std::fmt::Debug for AudioStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AudioStream({} {} Hz, {} channels)",
            self.route, self.rate, self.channels
        )
    }
}

enum Inner {
    Native(Box<Native>),
    Ffmpeg { child: Child, got: bool },
    Mix(Vec<Part>),
    Done,
}

struct Part {
    stream: AudioStream,
    gain: f32,
    pending: Vec<f32>,
    at: usize,
    ended: bool,
}

impl Part {
    fn frames(&self) -> usize {
        self.pending.len() / 2 - self.at
    }
}

struct Native {
    feed: Feed,
    resampler: Option<Resampler>,
    up_first: bool,
    rate: u32,
    channels: u16,
    raw: Vec<f32>,
    fed: usize,
    out: Vec<f32>,
    drop: usize,
    sent: usize,
    start: usize,
    eof: bool,
}

pub fn audio_stream(
    path: &Path,
    track: usize,
    from: f64,
    rate: u32,
    channels: u16,
    prefs: &Prefs,
) -> Result<AudioStream> {
    if rate == 0 || channels == 0 {
        return Err(Error::Invalid(format!(
            "a sample rate of {rate} with {channels} channels"
        )));
    }
    if !from.is_finite() {
        return Err(Error::Invalid(format!("a stream from {from}")));
    }
    let picked = pick_decode(Codec::Aac, prefs, &Caps::for_audio(prefs))?;
    if picked.route != Route::Ffmpeg {
        match Native::open(path, track, from, rate, channels) {
            Ok(n) => {
                return Ok(AudioStream {
                    route: Route::Cpu,
                    rate,
                    channels,
                    inner: Inner::Native(Box::new(n)),
                });
            }
            Err(e) if prefs.user_ffmpeg().is_none() => return Err(e),
            Err(_) => {}
        }
    }
    let f = container_ffmpeg(prefs)?;
    let from = from.max(0.0);
    let mut args: Vec<String> = Vec::new();
    if from > 0.0 {
        args.extend(["-ss".into(), format!("{from:.6}")]);
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
    let child = f.spawn(&args, false, true)?;
    Ok(AudioStream {
        route: Route::Ffmpeg,
        rate,
        channels,
        inner: Inner::Ffmpeg { child, got: false },
    })
}

pub fn audio_mix_stream(
    path: &Path,
    gains: &[f32],
    from: f64,
    rate: u32,
    prefs: &Prefs,
) -> Result<AudioStream> {
    if rate == 0 {
        return Err(Error::Invalid(format!("a sample rate of {rate}")));
    }
    if !from.is_finite() {
        return Err(Error::Invalid(format!("a stream from {from}")));
    }
    let mut parts = Vec::new();
    for t in probe(path, prefs)?.audio {
        let gain = gains.get(t.index).copied().unwrap_or(1.0);
        if gain > 0.0 {
            parts.push(Part {
                stream: audio_stream(path, t.index, from, rate, 2, prefs)?,
                gain,
                pending: Vec::new(),
                at: 0,
                ended: false,
            });
        }
    }
    let route = if parts.iter().any(|p| p.stream.route == Route::Ffmpeg) {
        Route::Ffmpeg
    } else {
        Route::Cpu
    };
    Ok(AudioStream {
        route,
        rate,
        channels: 2,
        inner: Inner::Mix(parts),
    })
}

fn mix(parts: &mut [Part], rate: u32) -> Result<Option<AudioBuffer>> {
    let want = rate as usize;
    for p in parts.iter_mut() {
        while !p.ended && p.frames() < want {
            match p.stream.next() {
                Some(b) => {
                    if p.at > 0 {
                        p.pending.drain(..p.at * 2);
                        p.at = 0;
                    }
                    p.pending.extend_from_slice(&b?.samples);
                }
                None => p.ended = true,
            }
        }
    }
    let n = parts
        .iter()
        .map(|p| p.frames().min(want))
        .max()
        .unwrap_or(0);
    if n == 0 {
        return Ok(None);
    }
    let mut out = AudioBuffer::new(rate, 2);
    out.samples = vec![0.0; n * 2];
    for p in parts.iter_mut() {
        let m = p.frames().min(n);
        let from = p.at * 2;
        let got = p.pending.get(from..from + m * 2).unwrap_or_default();
        for (o, s) in out.samples.iter_mut().zip(got) {
            *o += s * p.gain;
        }
        p.at += m;
    }
    Ok(Some(out))
}

impl Native {
    fn open(path: &Path, track: usize, from: f64, rate: u32, channels: u16) -> Result<Native> {
        let d = native::audio_demux(path)?;
        let i = native::audio_track(&d, track)?;
        let pad = Sound::open(&d, i)?.pre_roll().max(PAD);
        let start = (from.max(0.0) * rate as f64).round() as usize;
        let pad = start.min((pad * rate as f64).round() as usize);
        let t0 = (start - pad) as f64 / rate as f64;
        let feed = Feed::open(&d, i, t0)?;
        if channels != feed.channels && channels > 2 {
            return Err(Error::Invalid(format!(
                "{channels} channels from a {}-channel track: ask for 1, 2 or {}",
                feed.channels, feed.channels
            )));
        }
        let up_first = channels > feed.channels;
        let resampler = (feed.rate != rate).then(|| {
            Resampler::new(
                feed.rate,
                rate,
                if up_first { feed.channels } else { channels },
            )
        });
        Ok(Native {
            feed,
            resampler,
            up_first,
            rate,
            channels,
            raw: Vec::new(),
            fed: 0,
            out: Vec::new(),
            drop: pad,
            sent: 0,
            start,
            eof: false,
        })
    }

    fn convert(&mut self, buf: AudioBuffer) -> Result<AudioBuffer> {
        if self.up_first {
            let buf = match self.resampler.as_mut() {
                Some(r) => r.process(&buf),
                None => buf,
            };
            shape(buf, self.channels)
        } else {
            let buf = shape(buf, self.channels)?;
            Ok(match self.resampler.as_mut() {
                Some(r) => r.process(&buf),
                None => buf,
            })
        }
    }

    fn keep(&mut self, buf: AudioBuffer) {
        let ch = self.channels as usize;
        let frames = buf.samples.len() / ch;
        let drop = self.drop.min(frames);
        self.drop -= drop;
        self.out
            .extend_from_slice(buf.samples.get(drop * ch..frames * ch).unwrap_or_default());
    }

    fn pump(&mut self) -> Result<()> {
        let src_ch = self.feed.channels as usize;
        let batch = self.feed.rate as usize;
        self.raw.clear();
        let mut ended = false;
        loop {
            let have = self.fed + self.raw.len() / src_ch;
            if let Some(w) = self.feed.want(f64::INFINITY)
                && have >= w
            {
                ended = true;
                break;
            }
            if self.raw.len() / src_ch >= batch {
                break;
            }
            if !self.feed.next(&mut self.raw)? {
                ended = true;
                break;
            }
        }
        if ended && let Some(w) = self.feed.want(f64::INFINITY) {
            let keep = w.saturating_sub(self.fed) * src_ch;
            self.raw.resize(keep, 0.0);
        }
        self.fed += self.raw.len() / src_ch;
        let mut buf = AudioBuffer::new(self.feed.rate, self.feed.channels);
        buf.samples = std::mem::take(&mut self.raw);
        let done = self.convert(buf)?;
        self.raw = Vec::with_capacity(batch * src_ch);
        self.keep(done);
        if ended {
            self.eof = true;
            if let Some(tail) = self.resampler.as_mut().map(Resampler::flush) {
                let tail = if self.up_first {
                    shape(tail, self.channels)?
                } else {
                    tail
                };
                self.keep(tail);
            }
            let length = self.feed.end();
            if length > 0.0 {
                let end = ((length * self.rate as f64).round() as usize).max(self.start);
                let total = (end - self.start).saturating_sub(self.sent);
                self.out.resize(total * self.channels as usize, 0.0);
            }
        }
        Ok(())
    }

    fn read(&mut self) -> Result<Option<AudioBuffer>> {
        let block = self.rate as usize * self.channels as usize;
        while self.out.len() < block && !self.eof {
            self.pump()?;
        }
        if self.out.is_empty() {
            return Ok(None);
        }
        let n = block.min(self.out.len());
        let mut buf = AudioBuffer::new(self.rate, self.channels);
        buf.samples = self.out.drain(..n).collect();
        self.sent += buf.frames();
        Ok(Some(buf))
    }
}

pub(super) fn native_stream(
    path: &Path,
    track: usize,
    from: f64,
    rate: u32,
    channels: u16,
) -> Result<AudioStream> {
    if rate == 0 || channels == 0 || !from.is_finite() {
        return Err(Error::Invalid(format!(
            "a stream from {from} at {rate} Hz with {channels} channels"
        )));
    }
    let n = Native::open(path, track, from, rate, channels)?;
    Ok(AudioStream {
        route: Route::Cpu,
        rate,
        channels,
        inner: Inner::Native(Box::new(n)),
    })
}

impl AudioStream {
    fn read(&mut self) -> Result<Option<AudioBuffer>> {
        let block = self.rate as usize * self.channels as usize * 4;
        match &mut self.inner {
            Inner::Native(n) => n.read(),
            Inner::Mix(parts) => mix(parts, self.rate),
            Inner::Done => Ok(None),
            Inner::Ffmpeg { child, got } => {
                let mut bytes = vec![0u8; block];
                let mut filled = 0;
                if let Some(out) = child.stdout.as_mut() {
                    while filled < block {
                        match out.read(&mut bytes[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(e) => return Err(Error::Io(e)),
                        }
                    }
                }
                let whole = filled / 4 / self.channels as usize * self.channels as usize * 4;
                if whole == 0 {
                    let first = !*got;
                    if let Inner::Ffmpeg { child, .. } =
                        std::mem::replace(&mut self.inner, Inner::Done)
                    {
                        let done = child.finish();
                        if first {
                            done?;
                        }
                    }
                    return Ok(None);
                }
                *got = true;
                let mut buf = AudioBuffer::new(self.rate, self.channels);
                buf.samples = bytes[..whole]
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                Ok(Some(buf))
            }
        }
    }
}

impl Iterator for AudioStream {
    type Item = Result<AudioBuffer>;

    fn next(&mut self) -> Option<Result<AudioBuffer>> {
        let got = self.read();
        if got.is_err() {
            self.inner = Inner::Done;
        }
        got.transpose()
    }
}

const QUEUE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioSpan {
    pub from: f64,
    pub to: f64,
    pub rate: u32,
    pub channels: u16,
}

impl AudioSpan {
    pub const WHOLE: AudioSpan = AudioSpan {
        from: 0.0,
        to: f64::INFINITY,
        rate: 48_000,
        channels: 2,
    };

    fn limit(&self) -> Option<u64> {
        self.to
            .is_finite()
            .then(|| ((self.to - self.from).max(0.0) * self.rate as f64).round() as u64)
    }
}

pub fn decode_audio_span(
    path: &Path,
    track: usize,
    prefs: &Prefs,
    span: &AudioSpan,
    mut each: impl FnMut(AudioBuffer) -> Result<()>,
) -> Result<()> {
    if !span.from.is_finite() || span.from < 0.0 || span.to.is_nan() || span.to < span.from {
        return Err(Error::Invalid(format!(
            "the range {} to {}",
            span.from, span.to
        )));
    }
    let limit = span.limit();
    if limit == Some(0) {
        return Ok(());
    }
    let stream = audio_stream(path, track, span.from, span.rate, span.channels, prefs)?;
    std::thread::scope(|s| {
        let (tx, rx) = mpsc::sync_channel::<Result<AudioBuffer>>(QUEUE);
        s.spawn(move || {
            for b in stream {
                let failed = b.is_err();
                if tx.send(b).is_err() || failed {
                    break;
                }
            }
        });
        let mut sent = 0u64;
        for b in rx {
            let mut b = b?;
            if let Some(limit) = limit {
                let left = limit - sent;
                if b.frames() as u64 >= left {
                    b.samples.truncate(left as usize * b.channels as usize);
                    if !b.samples.is_empty() {
                        each(b)?;
                    }
                    break;
                }
            }
            sent += b.frames() as u64;
            each(b)?;
        }
        Ok(())
    })
}
