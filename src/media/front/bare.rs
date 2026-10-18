use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::probe::{AudioTrack, Probe};
use crate::media::wav;
use crate::media::{AudioBuffer, Error, Result};

const BATCH: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bare {
    Wav,
    #[cfg(feature = "mp3")]
    Mp3,
}

pub fn sniff(path: &Path) -> Option<Bare> {
    let mut head = [0u8; 12];
    let mut file = File::open(path).ok()?;
    let mut got = 0;
    while got < head.len() {
        match file.read(&mut head[got..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => got += n,
        }
    }
    if wav::is_wav(&head[..got]) {
        return Some(Bare::Wav);
    }
    #[cfg(feature = "mp3")]
    {
        let named = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["mp3", "mp2", "mp1"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        });
        let synced = head.starts_with(b"ID3") || (head[0] == 0xff && head[1] & 0xe0 == 0xe0);
        if named && got >= 3 && synced {
            return Some(Bare::Mp3);
        }
    }
    None
}

#[cfg(feature = "mp3")]
fn mp3(path: &Path) -> Result<AudioBuffer> {
    let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
    crate::mp3::decode_file(&data)
}

pub fn probe(path: &Path, kind: Bare) -> Result<Probe> {
    let (codec, channels, rate, bitrate, duration) = match kind {
        Bare::Wav => {
            let l = wav::Reader::open(path)?.layout;
            let bits = u32::from(l.bits);
            let codec = "pcm";
            (
                codec,
                l.channels,
                l.rate,
                l.rate
                    .checked_mul(u32::from(l.channels))
                    .and_then(|v| v.checked_mul(bits)),
                l.seconds(),
            )
        }
        #[cfg(feature = "mp3")]
        Bare::Mp3 => {
            let b = mp3(path)?;
            let seconds = b.frames() as f64 / f64::from(b.rate.max(1));
            let bytes = std::fs::metadata(path).map_or(0, |m| m.len());
            let bitrate = (seconds > 0.0).then(|| (bytes as f64 * 8.0 / seconds) as u32);
            ("mp3", b.channels, b.rate, bitrate, seconds)
        }
    };
    Ok(Probe {
        duration,
        start: 0.0,
        video_start: None,
        fps: (0, 1),
        width: 0,
        height: 0,
        rotation: 0,
        video_codec: None,
        video_codec_name: String::new(),
        profile: String::new(),
        bit_depth: 0,
        frames: None,
        audio: vec![AudioTrack {
            index: 0,
            stream: 0,
            title: None,
            language: None,
            codec: codec.into(),
            channels,
            rate,
            bitrate,
            default: true,
            start: Some(0.0),
        }],
        alpha: false,
    })
}

enum Src {
    Wav(Box<wav::Reader>),
    Memory(AudioBuffer, usize),
}

pub struct BareFeed {
    pub rate: u32,
    pub channels: u16,
    frames: u64,
    src: Src,
}

impl BareFeed {
    pub fn open(path: &Path, kind: Bare, track: usize, from: f64) -> Result<BareFeed> {
        if track != 0 {
            return Err(Error::Invalid(format!("no audio track {track}")));
        }
        let mut feed = match kind {
            Bare::Wav => {
                let r = wav::Reader::open(path)?;
                BareFeed {
                    rate: r.layout.rate,
                    channels: r.layout.channels,
                    frames: r.layout.frames(),
                    src: Src::Wav(Box::new(r)),
                }
            }
            #[cfg(feature = "mp3")]
            Bare::Mp3 => {
                let b = mp3(path)?;
                BareFeed {
                    rate: b.rate,
                    channels: b.channels,
                    frames: b.frames() as u64,
                    src: Src::Memory(b, 0),
                }
            }
        };
        if feed.rate == 0 || feed.channels == 0 {
            return Err(Error::Invalid(format!(
                "audio of {} channels at {} Hz",
                feed.channels, feed.rate
            )));
        }
        let first = ((from.max(0.0) * f64::from(feed.rate)).round() as u64).min(feed.frames);
        match &mut feed.src {
            Src::Wav(r) => r.seek(first),
            Src::Memory(_, at) => *at = first as usize,
        }
        Ok(feed)
    }

    pub fn end(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate)
    }

    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        match &mut self.src {
            Src::Wav(r) => Ok(r.read(BATCH, out)? > 0),
            Src::Memory(b, at) => {
                let ch = usize::from(b.channels);
                let total = b.samples.len() / ch;
                let n = BATCH.min(total - (*at).min(total));
                out.extend_from_slice(&b.samples[*at * ch..(*at + n) * ch]);
                *at += n;
                Ok(n > 0)
            }
        }
    }
}
