use crate::mp4::{CodecConfig, PcmConfig};

use super::demux::Demux;
use crate::media::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    U8,
    S8,
    S16,
    S24,
    S32,
    F32,
    F64,
    ALaw,
    MuLaw,
}

const fn g711(mu: bool, b: u8) -> i16 {
    if mu {
        let u = !b;
        let t = ((((u & 0x0f) as i32) << 3) + 0x84) << ((u & 0x70) >> 4);
        (if u & 0x80 != 0 { 0x84 - t } else { t - 0x84 }) as i16
    } else {
        let a = b ^ 0x55;
        let seg = ((a & 0x70) >> 4) as i32;
        let mut t = (((a & 0x0f) as i32) << 4) + if seg == 0 { 8 } else { 0x108 };
        if seg > 1 {
            t <<= seg - 1;
        }
        (if a & 0x80 != 0 { t } else { -t }) as i16
    }
}

const fn g711_table(mu: bool) -> [i16; 256] {
    let mut t = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = g711(mu, i as u8);
        i += 1;
    }
    t
}

const ALAW: [i16; 256] = g711_table(false);
const MULAW: [i16; 256] = g711_table(true);

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
        Pcm::of(kind, big_endian, channels, rate)
    }

    pub fn g711(mu: bool, channels: u64, rate: f64) -> Result<Pcm> {
        Pcm::of(
            if mu { Kind::MuLaw } else { Kind::ALaw },
            false,
            channels,
            rate,
        )
    }

    fn of(kind: Kind, big_endian: bool, channels: u64, rate: f64) -> Result<Pcm> {
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
            Kind::U8 | Kind::S8 | Kind::ALaw | Kind::MuLaw => 1,
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
            Kind::ALaw => out.extend(data.iter().map(|&b| ALAW[b as usize] as f32 / 32768.0)),
            Kind::MuLaw => out.extend(data.iter().map(|&b| MULAW[b as usize] as f32 / 32768.0)),
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
    #[cfg(feature = "ac3")]
    Ac3(Box<crate::ac3::Decoder>),
    #[cfg(feature = "opus")]
    Opus(Box<crate::opus::Decoder>),
    #[cfg(feature = "mp3")]
    Mp3(Box<crate::mp3::Decoder>),
    #[cfg(feature = "flac")]
    Flac(Box<crate::flac::Decoder>),
    #[cfg(feature = "alac")]
    Alac(Box<crate::alac::Decoder>),
    Pcm(Pcm),
}

fn unsupported(name: &str) -> Error {
    Error::Unsupported(format!(
        "{} audio is not supported",
        match super::probe::plain(name) {
            "" => "unknown",
            name => name,
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

pub(super) fn acm_format(private: &[u8]) -> Option<(u16, u16, u32, u16)> {
    let at = |i: usize| -> Option<u16> {
        Some(u16::from_le_bytes(private.get(i..i + 2)?.try_into().ok()?))
    };
    let tag = match at(0)? {
        0xfffe => at(24)?,
        tag => tag,
    };
    let rate = u32::from_le_bytes(private.get(4..8)?.try_into().ok()?);
    Some((tag, at(2)?, rate, at(14)?))
}

pub(super) fn acm_name(private: &[u8]) -> &'static str {
    match acm_format(private).map(|f| f.0) {
        Some(1 | 3) => "pcm",
        Some(6) => "alaw",
        Some(7) => "ulaw",
        _ => "A_MS/ACM",
    }
}

pub(super) fn acm_refused(private: &[u8]) -> Option<String> {
    let Some((tag, ..)) = acm_format(private) else {
        return Some("A_MS/ACM without its WAVEFORMATEX".to_string());
    };
    let name = match tag {
        1 | 3 | 6 | 7 => return None,
        0x0160 => "WMA v1",
        0x0161 => "WMA v2",
        0x0162 => "WMA Pro",
        0x0163 => "WMA Lossless",
        0x0055 => "MP3",
        0x2000 => "AC-3",
        0x00ff | 0x1610 => "AAC",
        _ => "Windows audio",
    };
    Some(format!("{name} (Windows audio format 0x{tag:04x})"))
}

pub(super) fn acm_channels(private: &[u8], container: u16) -> u16 {
    acm_format(private).map_or(container, |f| f.1).max(1)
}

fn acm_pcm(private: &[u8]) -> Result<Pcm> {
    let Some((tag, channels, rate, bits)) = acm_format(private) else {
        return Err(Error::Invalid(
            "an A_MS/ACM track without its WAVEFORMATEX".to_string(),
        ));
    };
    let (channels, rate) = (channels as u64, rate as f64);
    match tag {
        1 => Pcm::new(bits, false, bits != 8, false, channels, rate),
        3 => Pcm::new(bits, true, true, false, channels, rate),
        6 => Pcm::g711(false, channels, rate),
        7 => Pcm::g711(true, channels, rate),
        tag => Err(Error::Unsupported(format!(
            "Windows audio format 0x{tag:04x} in A_MS/ACM is not supported"
        ))),
    }
}

pub(super) fn child<'a>(e: &'a crate::mp4::SampleEntry, kind: &[u8; 4]) -> Option<&'a [u8]> {
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
        if rest.get(4..8)? == kind {
            return rest.get(8..size);
        }
        rest = rest.get(size..)?;
    }
    None
}

impl Sound {
    #[cfg(feature = "mp3")]
    fn mp3(rate: f64, channels: u64) -> Sound {
        let rate = if (1.0..=768_000.0).contains(&rate) {
            rate.round() as u32
        } else {
            0
        };
        Sound::Mp3(Box::new(crate::mp3::Decoder::with_format(
            rate,
            channels.min(2) as u16,
        )))
    }

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
                    CodecConfig::Other(f) if &f.0 == b"alaw" || &f.0 == b"ulaw" => {
                        let a = e.audio.clone().unwrap_or_default();
                        Ok(Sound::Pcm(Pcm::g711(
                            &f.0 == b"ulaw",
                            a.channels as u64,
                            a.sample_rate,
                        )?))
                    }
                    #[cfg(feature = "ac3")]
                    CodecConfig::Other(f) if &f.0 == b"ac-3" || &f.0 == b"ec-3" => {
                        Ok(Sound::Ac3(Box::new(crate::ac3::Decoder::new(
                            t.sample_rate(),
                            t.channels() as u16,
                        ))))
                    }
                    #[cfg(feature = "mp3")]
                    CodecConfig::Mp3 => {
                        let a = e.audio.clone().unwrap_or_default();
                        Ok(Sound::mp3(a.sample_rate, a.channels as u64))
                    }
                    #[cfg(feature = "alac")]
                    CodecConfig::Other(f) if &f.0 == b"alac" => {
                        let cookie = crate::alac::entry_cookie(e).ok_or_else(|| {
                            Error::Invalid("ALAC track without its magic cookie".to_string())
                        })?;
                        Ok(Sound::Alac(Box::new(crate::alac::Decoder::new(cookie)?)))
                    }
                    #[cfg(feature = "opus")]
                    CodecConfig::Other(f) if &f.0 == b"Opus" => {
                        let head = child(e, b"dOps").ok_or_else(|| {
                            Error::Invalid("Opus track without a dOps box".to_string())
                        })?;
                        let head = crate::opus::OpusHead::from_dops(head)?;
                        Ok(Sound::Opus(Box::new(crate::opus::Decoder::from_head(
                            head, 48000,
                        )?)))
                    }
                    #[cfg(feature = "flac")]
                    CodecConfig::Other(f) if &f.0 == b"fLaC" => {
                        let config = child(e, b"dfLa").ok_or_else(|| {
                            Error::Invalid("FLAC track without a dfLa box".to_string())
                        })?;
                        Ok(Sound::Flac(Box::new(crate::flac::Decoder::from_config(
                            config,
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
                    #[cfg(feature = "opus")]
                    "opus" => Ok(Sound::Opus(Box::new(crate::opus::Decoder::new(
                        &t.codec_private,
                    )?))),
                    "pcm" => Ok(Sound::Pcm(mkv_pcm(t)?)),
                    "A_MS/ACM" => Ok(Sound::Pcm(acm_pcm(&t.codec_private)?)),
                    #[cfg(feature = "flac")]
                    "flac" => Ok(Sound::Flac(Box::new(crate::flac::Decoder::from_config(
                        &t.codec_private,
                    )?))),
                    #[cfg(feature = "alac")]
                    "alac" if t.codec_private.is_empty() => Err(Error::Invalid(
                        "ALAC track without its magic cookie".to_string(),
                    )),
                    #[cfg(feature = "alac")]
                    "alac" => Ok(Sound::Alac(Box::new(crate::alac::Decoder::new(
                        &t.codec_private,
                    )?))),
                    #[cfg(feature = "ac3")]
                    "ac3" | "eac3" => Ok(Sound::Ac3(Box::new(crate::ac3::Decoder::new(
                        t.sample_rate(),
                        t.channels() as u16,
                    )))),
                    #[cfg(feature = "mp3")]
                    "mp3" | "mp2" | "mp1" => {
                        let a = t.audio.clone().unwrap_or_default();
                        Ok(Sound::mp3(a.sampling_frequency, a.channels))
                    }
                    name => Err(unsupported(name)),
                }
            }
        }
    }

    pub fn rate(&self) -> u32 {
        match self {
            Sound::Aac(d) => d.rate(),
            #[cfg(feature = "ac3")]
            Sound::Ac3(d) => d.rate(),
            #[cfg(feature = "opus")]
            Sound::Opus(d) => d.sample_rate(),
            #[cfg(feature = "mp3")]
            Sound::Mp3(d) => d.rate(),
            #[cfg(feature = "flac")]
            Sound::Flac(d) => d.rate(),
            #[cfg(feature = "alac")]
            Sound::Alac(d) => d.rate(),
            Sound::Pcm(p) => p.rate,
        }
    }

    pub fn channels(&self) -> u16 {
        match self {
            Sound::Aac(d) => d.channels(),
            #[cfg(feature = "ac3")]
            Sound::Ac3(d) => d.channels(),
            #[cfg(feature = "opus")]
            Sound::Opus(d) => d.channels() as u16,
            #[cfg(feature = "mp3")]
            Sound::Mp3(d) => d.channels(),
            #[cfg(feature = "flac")]
            Sound::Flac(d) => d.channels(),
            #[cfg(feature = "alac")]
            Sound::Alac(d) => d.channels(),
            Sound::Pcm(p) => p.channels,
        }
    }

    pub fn counts_frames(&self) -> bool {
        match self {
            #[cfg(feature = "flac")]
            Sound::Flac(_) => true,
            #[cfg(feature = "alac")]
            Sound::Alac(_) => true,
            _ => false,
        }
    }

    pub fn pcm_frames(&self, bytes: u32) -> Option<u64> {
        match self {
            Sound::Pcm(p) => Some(bytes as u64 / (p.width() * p.channels as usize).max(1) as u64),
            Sound::Aac(_) => None,
            #[cfg(feature = "ac3")]
            Sound::Ac3(_) => None,
            #[cfg(feature = "opus")]
            Sound::Opus(_) => None,
            #[cfg(feature = "mp3")]
            Sound::Mp3(_) => None,
            #[cfg(feature = "flac")]
            Sound::Flac(_) => None,
            #[cfg(feature = "alac")]
            Sound::Alac(_) => None,
        }
    }

    pub fn pre_roll(&self) -> f64 {
        match self {
            #[cfg(feature = "opus")]
            Sound::Opus(_) => 0.08,
            #[cfg(feature = "ac3")]
            Sound::Ac3(d) => 1536.0 / d.rate().max(1) as f64,
            #[cfg(feature = "mp3")]
            Sound::Mp3(_) => 0.2,
            #[cfg(feature = "flac")]
            Sound::Flac(_) => 0.0,
            #[cfg(feature = "alac")]
            Sound::Alac(_) => 0.0,
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
            #[cfg(feature = "opus")]
            Sound::Opus(d) => {
                out.clear();
                d.decode_into(Some(data), out)?;
                Ok(d.channels() as u16)
            }
            #[cfg(feature = "ac3")]
            Sound::Ac3(d) => {
                d.decode_into(data, out)?;
                Ok(d.channels())
            }
            #[cfg(feature = "mp3")]
            Sound::Mp3(d) => {
                d.decode_packet(data, out)?;
                Ok(d.channels())
            }
            #[cfg(feature = "flac")]
            Sound::Flac(d) => {
                d.decode_packet(data, out)?;
                Ok(d.channels())
            }
            #[cfg(feature = "alac")]
            Sound::Alac(d) => {
                d.decode_into(data, out)?;
                Ok(d.channels())
            }
            Sound::Pcm(p) => {
                p.decode(data, out)?;
                Ok(p.channels)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_expands_as_the_reference_does() {
        let alaw = g711_table(false);
        let mulaw = g711_table(true);
        assert_eq!(alaw[0x55], -8);
        assert_eq!(alaw[0xd5], 8);
        assert_eq!(alaw[0xaa], 32256);
        assert_eq!(alaw[0x2a], -32256);
        assert_eq!(mulaw[0xff], 0);
        assert_eq!(mulaw[0x7f], 0);
        assert_eq!(mulaw[0x80], 32124);
        assert_eq!(mulaw[0x00], -32124);
        assert_eq!(g711(false, 0xd4), 24);
        assert_eq!(g711(true, 0xfe), 8);
        for b in 0..=255u8 {
            assert_eq!(alaw[(b ^ 0x80) as usize], -alaw[b as usize], "{b:#x}");
            assert_eq!(mulaw[(b ^ 0x80) as usize], -mulaw[b as usize], "{b:#x}");
        }
        let mut positive: Vec<i16> = (0x80..=0xffu8).map(|b| mulaw[b as usize]).collect();
        positive.dedup();
        assert!(positive.windows(2).all(|w| w[0] > w[1]));
        let mut sorted: Vec<i16> = (0x80..=0xffu8).map(|b| alaw[(b ^ 0x55) as usize]).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 128);
    }

    #[test]
    fn g711_pcm_decodes_one_byte_per_sample_and_counts_frames() {
        let pcm = Pcm::g711(false, 2, 8000.0).unwrap();
        let mut out = Vec::new();
        pcm.decode(&[0xd5, 0x55, 0xaa, 0x2a], &mut out).unwrap();
        assert_eq!(
            out,
            [
                8.0 / 32768.0,
                -8.0 / 32768.0,
                32256.0 / 32768.0,
                -32256.0 / 32768.0
            ]
        );
        assert!(pcm.decode(&[0xd5], &mut out).is_err());
        assert_eq!(Sound::Pcm(pcm).pcm_frames(8000), Some(4000));
        let mu = Pcm::g711(true, 1, 8000.0).unwrap();
        mu.decode(&[0x80, 0xff], &mut out).unwrap();
        assert_eq!(out, [32124.0 / 32768.0, 0.0]);
        assert!(Pcm::g711(true, 0, 8000.0).is_err());
        assert!(Pcm::g711(false, 1, 10.0).is_err());
        assert!(Pcm::g711(false, 1, f64::NAN).is_err());
    }

    fn waveformatex(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend_from_slice(&tag.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * u32::from(channels) * u32::from(bits) / 8).to_le_bytes());
        w.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        w.extend_from_slice(&bits.to_le_bytes());
        w.extend_from_slice(&0u16.to_le_bytes());
        w
    }

    #[test]
    fn acm_names_pcm_and_g711_and_leaves_the_rest_refused() {
        assert_eq!(
            acm_format(&waveformatex(6, 2, 8000, 8)),
            Some((6, 2, 8000, 8))
        );
        assert_eq!(acm_name(&waveformatex(6, 1, 8000, 8)), "alaw");
        assert_eq!(acm_name(&waveformatex(7, 1, 8000, 8)), "ulaw");
        assert_eq!(acm_name(&waveformatex(1, 2, 48000, 16)), "pcm");
        assert_eq!(acm_name(&waveformatex(3, 2, 48000, 32)), "pcm");
        assert_eq!(acm_name(&waveformatex(0x0161, 2, 44100, 16)), "A_MS/ACM");
        let mut extensible = waveformatex(0xfffe, 2, 48000, 24);
        extensible[16..18].copy_from_slice(&22u16.to_le_bytes());
        extensible.extend_from_slice(&24u16.to_le_bytes());
        extensible.extend_from_slice(&3u32.to_le_bytes());
        extensible.extend_from_slice(&1u16.to_le_bytes());
        extensible.extend_from_slice(&[0; 14]);
        assert_eq!(acm_format(&extensible), Some((1, 2, 48000, 24)));
        assert_eq!(acm_format(&extensible[..20]), None);
        assert_eq!(acm_format(&[6, 0, 1]), None);
        assert_eq!(acm_name(&[]), "A_MS/ACM");
    }

    #[test]
    fn an_acm_refusal_names_the_codec_and_its_channels_come_from_the_waveformatex() {
        for tag in [1, 3, 6, 7] {
            assert_eq!(acm_refused(&waveformatex(tag, 2, 48000, 16)), None, "{tag}");
        }
        assert_eq!(
            acm_refused(&waveformatex(0x0161, 2, 44100, 16)).as_deref(),
            Some("WMA v2 (Windows audio format 0x0161)")
        );
        assert_eq!(
            acm_refused(&waveformatex(0x0163, 2, 44100, 16)).as_deref(),
            Some("WMA Lossless (Windows audio format 0x0163)")
        );
        assert_eq!(
            acm_refused(&waveformatex(0x1234, 1, 8000, 16)).as_deref(),
            Some("Windows audio (Windows audio format 0x1234)")
        );
        assert!(acm_refused(&[1, 0]).unwrap().contains("WAVEFORMATEX"));
        assert_eq!(acm_channels(&waveformatex(1, 2, 48000, 16), 1), 2);
        assert_eq!(acm_channels(&waveformatex(6, 1, 8000, 8), 2), 1);
        assert_eq!(acm_channels(&[], 6), 6);
        assert_eq!(acm_channels(&waveformatex(1, 0, 48000, 16), 2), 1);
    }

    #[test]
    fn every_refused_acm_tag_is_named() {
        for (tag, name) in [
            (0x0160, "WMA v1"),
            (0x0162, "WMA Pro"),
            (0x0055, "MP3"),
            (0x2000, "AC-3"),
            (0x00ff, "AAC"),
            (0x1610, "AAC"),
        ] {
            let said = acm_refused(&waveformatex(tag, 2, 48000, 16)).unwrap();
            assert!(said.starts_with(&format!("{name} (")), "{tag:#x}: {said}");
        }
    }

    #[test]
    fn an_acm_track_opens_pcm_float_and_g711_and_refuses_the_rest() {
        let one = |tag: u16, rate: u32, bits: u16, data: &[u8]| {
            let mut out = Vec::new();
            acm_pcm(&waveformatex(tag, 1, rate, bits))
                .unwrap()
                .decode(data, &mut out)
                .unwrap();
            out
        };
        assert_eq!(one(1, 48000, 16, &16384i16.to_le_bytes()), [0.5]);
        assert_eq!(one(3, 48000, 32, &0.25f32.to_le_bytes()), [0.25]);
        assert_eq!(one(6, 8000, 8, &[0xd5]), [8.0 / 32768.0]);
        assert_eq!(one(7, 8000, 8, &[0x80]), [32124.0 / 32768.0]);
        let e = acm_pcm(&waveformatex(0x0161, 2, 44100, 16)).unwrap_err();
        assert!(matches!(e, Error::Unsupported(_)), "{e}");
        assert!(e.to_string().contains("0x0161"), "{e}");
        let e = acm_pcm(&[1, 0]).unwrap_err();
        assert!(e.to_string().contains("WAVEFORMATEX"), "{e}");
    }
}
