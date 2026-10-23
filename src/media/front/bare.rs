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
    #[cfg(feature = "flac")]
    Flac,
    #[cfg(feature = "opus")]
    Opus,
    #[cfg(feature = "vorbis")]
    Vorbis,
    Ogg(&'static str),
}

pub fn sniff(path: &Path) -> Option<Bare> {
    let mut head = [0u8; 64];
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
    #[cfg(feature = "flac")]
    {
        let named = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("flac"));
        if head[..got].starts_with(b"fLaC") || (named && head[..got].starts_with(b"ID3")) {
            return Some(Bare::Flac);
        }
    }
    #[cfg(feature = "opus")]
    if crate::opus::ogg::sniff(&head[..got])
        && head[..got]
            .get(27 + usize::from(head[26])..)
            .is_some_and(|body| body.starts_with(b"OpusHead"))
    {
        return Some(Bare::Opus);
    }
    #[cfg(feature = "vorbis")]
    if crate::vorbis::ogg::sniff(&head[..got]) {
        return Some(Bare::Vorbis);
    }
    if got >= 27 && head.starts_with(b"OggS") {
        let body = head[..got]
            .get(27 + usize::from(head[26])..)
            .unwrap_or_default();
        return Some(Bare::Ogg(ogg_name(body)));
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

fn ogg_name(body: &[u8]) -> &'static str {
    [
        (&b"OpusHead"[..], "Ogg Opus audio"),
        (b"\x01vorbis", "Ogg Vorbis audio"),
        (b"\x7fFLAC", "Ogg FLAC audio"),
        (b"\x80theora", "Ogg Theora video"),
        (b"Speex   ", "Ogg Speex audio"),
    ]
    .into_iter()
    .find(|(magic, _)| body.starts_with(magic))
    .map_or("unknown Ogg audio", |(_, name)| name)
}

fn refuse(name: &str) -> Error {
    Error::Unsupported(format!("{name} is not supported"))
}

#[cfg(feature = "mp3")]
fn mp3(path: &Path) -> Result<AudioBuffer> {
    let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
    crate::mp3::decode_file(&data)
}

#[cfg(feature = "vorbis")]
fn vorbis(path: &Path) -> Result<AudioBuffer> {
    let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
    crate::vorbis::decode(&data)
}

#[cfg(feature = "opus")]
fn opus(path: &Path) -> Result<AudioBuffer> {
    let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
    crate::opus::ogg::decode(&data)
}

pub fn probe(path: &Path, kind: Bare) -> Result<Probe> {
    let (codec, channels, rate, bitrate, duration, bits) = match kind {
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
                0,
            )
        }
        #[cfg(feature = "mp3")]
        Bare::Mp3 => {
            let b = mp3(path)?;
            let seconds = b.frames() as f64 / f64::from(b.rate.max(1));
            let bytes = std::fs::metadata(path).map_or(0, |m| m.len());
            let bitrate = (seconds > 0.0).then(|| (bytes as f64 * 8.0 / seconds) as u32);
            ("mp3", b.channels, b.rate, bitrate, seconds, 0)
        }
        #[cfg(feature = "flac")]
        Bare::Flac => {
            let i = crate::flac::read_info(path)?;
            let frames = if i.bits > 31 {
                i.samples
            } else {
                crate::flac::Reader::open(path)?.total()?
            };
            let seconds = frames as f64 / f64::from(i.rate.max(1));
            let bytes = std::fs::metadata(path).map_or(0, |m| m.len());
            let bitrate = (seconds > 0.0).then(|| (bytes as f64 * 8.0 / seconds) as u32);
            (
                "flac",
                u16::from(i.channels),
                i.rate,
                bitrate,
                seconds,
                i.bits,
            )
        }
        #[cfg(feature = "opus")]
        Bare::Opus => {
            let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
            let ogg = crate::opus::ogg::OggOpus::parse(&data)?;
            let seconds = ogg.samples() as f64 / 48_000.0;
            let bitrate = (seconds > 0.0).then(|| (data.len() as f64 * 8.0 / seconds) as u32);
            (
                "opus",
                ogg.head.channels as u16,
                48_000,
                bitrate,
                seconds,
                0,
            )
        }
        #[cfg(feature = "vorbis")]
        Bare::Vorbis => {
            let data = std::fs::read(path).map_err(|e| Error::at(path, e))?;
            let ogg = crate::vorbis::OggVorbis::parse(&data)?;
            let dec = ogg.decoder()?;
            let rate = dec.rate();
            let seconds = ogg.samples_with(&dec) as f64 / f64::from(rate.max(1));
            let bitrate = (seconds > 0.0).then(|| (data.len() as f64 * 8.0 / seconds) as u32);
            ("vorbis", dec.channels(), rate, bitrate, seconds, 0)
        }
        Bare::Ogg(name) => return Err(refuse(name)),
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
        chroma: None,
        interlaced: false,
        tools: None,
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
            bits,
            refused: None,
        }],
        alpha: false,
    })
}

enum Src {
    Wav(Box<wav::Reader>),
    #[cfg(feature = "flac")]
    Flac(Box<crate::flac::Reader<File>>),
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
            #[cfg(feature = "flac")]
            Bare::Flac => {
                let mut r = crate::flac::Reader::open(path)?;
                let i = *r.info();
                BareFeed {
                    rate: i.rate,
                    channels: u16::from(i.channels),
                    frames: r.total()?,
                    src: Src::Flac(Box::new(r)),
                }
            }
            #[cfg(feature = "opus")]
            Bare::Opus => {
                let b = opus(path)?;
                BareFeed {
                    rate: b.rate,
                    channels: b.channels,
                    frames: b.frames() as u64,
                    src: Src::Memory(b, 0),
                }
            }
            #[cfg(feature = "vorbis")]
            Bare::Vorbis => {
                let b = vorbis(path)?;
                BareFeed {
                    rate: b.rate,
                    channels: b.channels,
                    frames: b.frames() as u64,
                    src: Src::Memory(b, 0),
                }
            }
            Bare::Ogg(name) => return Err(refuse(name)),
        };
        if feed.rate == 0 || feed.channels == 0 {
            return Err(Error::Invalid(format!(
                "audio of {} channels at {} Hz",
                feed.channels, feed.rate
            )));
        }
        let first = ((from.max(0.0) * f64::from(feed.rate)).round() as u64).min(feed.frames);
        if first > 0 {
            match &mut feed.src {
                Src::Wav(r) => r.seek(first),
                #[cfg(feature = "flac")]
                Src::Flac(r) => r.seek(first)?,
                Src::Memory(_, at) => *at = first as usize,
            }
        }
        Ok(feed)
    }

    pub fn end(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate)
    }

    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        match &mut self.src {
            Src::Wav(r) => Ok(r.read(BATCH, out)? > 0),
            #[cfg(feature = "flac")]
            Src::Flac(r) => Ok(r.read_f32(out)? > 0),
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
