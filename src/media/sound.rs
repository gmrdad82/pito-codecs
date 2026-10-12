use crate::mp4::{CodecConfig, PcmConfig, SampleEntry};

use super::demux::Demux;
use super::types::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    U8,
    S8,
    S16,
    S24,
    S32,
    F32,
    F64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pcm {
    kind: Kind,
    big_endian: bool,
    channels: u16,
    rate: u32,
}

impl Pcm {
    pub fn new(
        bits: u16,
        float: bool,
        signed: bool,
        big_endian: bool,
        channels: u64,
        rate: f64,
    ) -> Result<Pcm> {
        let kind = match (float, bits) {
            (false, 8) if !signed => Kind::U8,
            (false, 8) => Kind::S8,
            (false, 16) => Kind::S16,
            (false, 24) => Kind::S24,
            (false, 32) => Kind::S32,
            (true, 32) => Kind::F32,
            (true, 64) => Kind::F64,
            _ => {
                let form = match (float, signed) {
                    (true, _) => "float",
                    (false, true) => "signed",
                    (false, false) => "unsigned",
                };
                return Err(Error::Unsupported(format!("{form} {bits}-bit PCM")));
            }
        };
        if !(1..=64).contains(&channels) {
            return Err(Error::Invalid(format!("PCM with {channels} channels")));
        }
        if !rate.is_finite() || !(1000.0..=768_000.0).contains(&rate) {
            return Err(Error::Invalid(format!("PCM at {rate} Hz")));
        }
        Ok(Pcm {
            kind,
            big_endian,
            channels: channels as u16,
            rate: rate.round() as u32,
        })
    }

    pub fn from_mp4(p: &PcmConfig) -> Result<Pcm> {
        Pcm::new(
            p.bits,
            p.float,
            p.signed,
            p.big_endian,
            p.channels as u64,
            p.sample_rate,
        )
    }

    fn width(&self) -> usize {
        match self.kind {
            Kind::U8 | Kind::S8 => 1,
            Kind::S16 => 2,
            Kind::S24 => 3,
            Kind::S32 | Kind::F32 => 4,
            Kind::F64 => 8,
        }
    }

    pub fn decode(&self, data: &[u8], out: &mut Vec<f32>) -> Result<()> {
        let w = self.width();
        let frame = w * self.channels as usize;
        if !data.len().is_multiple_of(frame) {
            return Err(Error::Invalid(format!(
                "a PCM packet of {} bytes, not whole {frame}-byte frames",
                data.len()
            )));
        }
        out.clear();
        out.reserve(data.len() / w);
        let be = self.big_endian;
        match self.kind {
            Kind::U8 => out.extend(data.iter().map(|&b| (b as f32 - 128.0) / 128.0)),
            Kind::S8 => out.extend(data.iter().map(|&b| b as i8 as f32 / 128.0)),
            Kind::S16 => out.extend(data.chunks_exact(2).map(|c| {
                let v = [c[0], c[1]];
                let s = if be {
                    i16::from_be_bytes(v)
                } else {
                    i16::from_le_bytes(v)
                };
                s as f32 / 32768.0
            })),
            Kind::S24 => out.extend(data.chunks_exact(3).map(|c| {
                let v = if be {
                    [c[0], c[1], c[2], 0]
                } else {
                    [c[2], c[1], c[0], 0]
                };
                (i32::from_be_bytes(v) >> 8) as f32 / 8_388_608.0
            })),
            Kind::S32 => out.extend(data.chunks_exact(4).map(|c| {
                let v = [c[0], c[1], c[2], c[3]];
                let s = if be {
                    i32::from_be_bytes(v)
                } else {
                    i32::from_le_bytes(v)
                };
                s as f32 / 2_147_483_648.0
            })),
            Kind::F32 => out.extend(data.chunks_exact(4).map(|c| {
                let v = [c[0], c[1], c[2], c[3]];
                if be {
                    f32::from_be_bytes(v)
                } else {
                    f32::from_le_bytes(v)
                }
            })),
            Kind::F64 => out.extend(data.chunks_exact(8).map(|c| {
                let v = [c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]];
                (if be {
                    f64::from_be_bytes(v)
                } else {
                    f64::from_le_bytes(v)
                }) as f32
            })),
        }
        Ok(())
    }
}

pub enum Sound {
    Aac(Box<crate::aac::Decoder>),
    Opus(Box<crate::opus::Decoder>),
    Pcm(Pcm),
}

fn unsupported(name: &str) -> Error {
    Error::Unsupported(format!(
        "our audio decoders read AAC, Opus and PCM, not {}",
        if name.is_empty() {
            "this track's codec"
        } else {
            name
        }
    ))
}

fn mkv_pcm(t: &crate::mkv::Track) -> Result<Pcm> {
    let a = t.audio.clone().unwrap_or_default();
    let (float, big_endian) = match t.codec_id.as_str() {
        "A_PCM/INT/LIT" => (false, false),
        "A_PCM/INT/BIG" => (false, true),
        "A_PCM/FLOAT/IEEE" => (true, false),
        id => return Err(unsupported(id)),
    };
    let bits = a.bit_depth.unwrap_or(if float { 32 } else { 16 });
    let bits = u16::try_from(bits).map_err(|_| Error::Invalid(format!("{bits}-bit PCM")))?;
    Pcm::new(
        bits,
        float,
        bits != 8,
        big_endian,
        a.channels,
        a.sampling_frequency,
    )
}

fn dops(e: &SampleEntry) -> Option<&[u8]> {
    let extra = match e.audio.as_ref().map_or(0, |a| a.qt_version) {
        1 => 16,
        2 => 36,
        _ => 0,
    };
    let mut rest = e.raw.get(28 + extra..)?;
    while rest.len() >= 8 {
        let size = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
        if size < 8 || size > rest.len() {
            return None;
        }
        if rest.get(4..8)? == b"dOps" {
            return rest.get(8..size);
        }
        rest = rest.get(size..)?;
    }
    None
}

impl Sound {
    pub fn open(d: &Demux, track: usize) -> Result<Sound> {
        match d {
            Demux::Mp4(r) => {
                let t = r
                    .tracks
                    .get(track)
                    .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
                let Some(e) = t.entry() else {
                    return Err(unsupported(""));
                };
                match &e.codec {
                    CodecConfig::Aac(_) => {
                        let asc = t.extradata().ok_or_else(|| {
                            Error::Invalid("AAC track without a config".to_string())
                        })?;
                        Ok(Sound::Aac(Box::new(crate::aac::Decoder::new(&asc)?)))
                    }
                    CodecConfig::Pcm(p) => Ok(Sound::Pcm(Pcm::from_mp4(p)?)),
                    CodecConfig::Other(f) if &f.0 == b"Opus" => {
                        let head = dops(e).ok_or_else(|| {
                            Error::Invalid("Opus track without a dOps box".to_string())
                        })?;
                        let head = crate::opus::OpusHead::from_dops(head)?;
                        Ok(Sound::Opus(Box::new(crate::opus::Decoder::from_head(
                            head, 48000,
                        )?)))
                    }
                    other => Err(unsupported(&other.name())),
                }
            }
            Demux::Mkv(r) => {
                let t = r
                    .tracks
                    .get(track)
                    .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
                match t.codec_name() {
                    "aac" => {
                        let asc = t.extradata().ok_or_else(|| {
                            Error::Invalid("AAC track without a config".to_string())
                        })?;
                        Ok(Sound::Aac(Box::new(crate::aac::Decoder::new(&asc)?)))
                    }
                    "opus" => Ok(Sound::Opus(Box::new(crate::opus::Decoder::new(
                        &t.codec_private,
                    )?))),
                    "pcm" => Ok(Sound::Pcm(mkv_pcm(t)?)),
                    name => Err(unsupported(name)),
                }
            }
        }
    }

    pub fn rate(&self) -> u32 {
        match self {
            Sound::Aac(d) => d.rate(),
            Sound::Opus(d) => d.sample_rate(),
            Sound::Pcm(p) => p.rate,
        }
    }

    pub fn channels(&self) -> u16 {
        match self {
            Sound::Aac(d) => d.channels(),
            Sound::Opus(d) => d.channels() as u16,
            Sound::Pcm(p) => p.channels,
        }
    }

    pub fn pcm_frames(&self, bytes: u32) -> Option<u64> {
        match self {
            Sound::Pcm(p) => Some(bytes as u64 / (p.width() * p.channels as usize).max(1) as u64),
            Sound::Aac(_) | Sound::Opus(_) => None,
        }
    }

    pub fn pre_roll(&self) -> f64 {
        match self {
            Sound::Opus(_) => 0.08,
            Sound::Aac(_) | Sound::Pcm(_) => 0.0,
        }
    }

    pub fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<u16> {
        match self {
            Sound::Aac(d) => {
                let b = d.decode_frame(data)?;
                *out = b.samples;
                Ok(b.channels)
            }
            Sound::Opus(d) => {
                out.clear();
                d.decode_into(Some(data), out)?;
                Ok(d.channels() as u16)
            }
            Sound::Pcm(p) => {
                p.decode(data, out)?;
                Ok(p.channels)
            }
        }
    }
}
