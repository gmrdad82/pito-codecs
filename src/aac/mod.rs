#![allow(clippy::needless_range_loop)]

mod bits;
mod config;
mod decoder;
mod encoder;
mod huffman;
mod huffman_tables;
mod ics;
mod mdct;
mod pcm;
#[cfg(test)]
mod pcm_edges;
mod tables;
mod tns;

pub use config::{AdtsHeader, AudioSpecificConfig, split_adts};
pub use pcm::{Resampler, downmix_stereo, gain, mono, resample};

use crate::media::{AudioBuffer, Error, Packet, Result};

pub const FRAME_LEN: usize = 1024;

pub const RATES: [u32; 13] = tables::SAMPLE_RATES;

fn wave_order(channel_config: u8) -> Option<&'static [usize]> {
    match channel_config {
        3 => Some(&[1, 2, 0]),
        4 => Some(&[1, 2, 0, 3]),
        5 => Some(&[1, 2, 0, 3, 4]),
        6 => Some(&[1, 2, 0, 5, 3, 4]),
        7 => Some(&[1, 2, 0, 7, 5, 6, 3, 4]),
        _ => None,
    }
}

fn aac_order(channels: usize) -> Option<&'static [usize]> {
    match channels {
        3 => Some(&[2, 0, 1]),
        4 => Some(&[2, 0, 1, 3]),
        5 => Some(&[2, 0, 1, 3, 4]),
        6 => Some(&[2, 0, 1, 4, 5, 3]),
        7 => Some(&[2, 0, 1, 5, 6, 4, 3]),
        8 => Some(&[2, 0, 1, 6, 7, 4, 5, 3]),
        _ => None,
    }
}

pub struct Decoder {
    core: decoder::Decoder,
    order: Option<&'static [usize]>,
}

impl Decoder {
    pub fn new(extradata: &[u8]) -> Result<Self> {
        let asc = AudioSpecificConfig::parse(extradata)?;
        let order = wave_order(asc.channel_config);
        Ok(Self {
            core: decoder::Decoder::from_config(asc)?,
            order,
        })
    }

    pub fn from_adts(header: &AdtsHeader) -> Result<Self> {
        Self::new(&header.to_asc().to_bytes())
    }

    pub fn rate(&self) -> u32 {
        self.core.sample_rate()
    }

    pub fn channels(&self) -> u16 {
        self.core.channels() as u16
    }

    pub fn reset(&mut self) {
        self.core.reset();
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<AudioBuffer> {
        self.decode_frame(&packet.data)
    }

    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioBuffer> {
        let planar = self.core.decode(data)?;
        let channels = planar.len();
        let frames = planar.first().map_or(0, Vec::len);
        let mut out = AudioBuffer::new(self.rate(), channels as u16);
        out.samples.reserve(frames * channels);
        for i in 0..frames {
            for c in 0..channels {
                let src = self.order.and_then(|o| o.get(c)).copied().unwrap_or(c);
                out.samples.push(planar[src][i]);
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub rate: u32,
    pub channels: u16,
    pub bitrate: u32,
}

pub struct Encoder {
    core: encoder::Encoder,
    config: EncoderConfig,
    order: Option<&'static [usize]>,
    planar: Vec<Vec<f32>>,
    next: i64,
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        if !RATES.contains(&config.rate) {
            return Err(Error::Unsupported(format!(
                "AAC sample rate {} (resample to 48000 first)",
                config.rate
            )));
        }
        let channels = config.channels as usize;
        let core = encoder::Encoder::new(if config.bitrate == 0 {
            encoder::EncoderConfig::vbr(config.rate, channels, 3.0)
        } else {
            encoder::EncoderConfig::cbr(config.rate, channels, config.bitrate)
        })?;
        Ok(Self {
            core,
            config,
            order: aac_order(config.channels as usize),
            planar: vec![Vec::new(); config.channels as usize],
            next: 0,
        })
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    pub fn extradata(&self) -> Vec<u8> {
        self.core.audio_specific_config()
    }

    pub fn priming(&self) -> u32 {
        self.core.priming_samples()
    }

    pub fn encode(&mut self, audio: &AudioBuffer) -> Result<Vec<Packet>> {
        if audio.channels != self.config.channels || audio.rate != self.config.rate {
            return Err(Error::Invalid(format!(
                "audio is {} Hz {} ch, the encoder takes {} Hz {} ch",
                audio.rate, audio.channels, self.config.rate, self.config.channels
            )));
        }
        let channels = self.config.channels as usize;
        for (c, plane) in self.planar.iter_mut().enumerate() {
            let src = self.order.and_then(|o| o.get(c)).copied().unwrap_or(c);
            plane.clear();
            plane.extend(audio.samples.chunks_exact(channels).map(|f| f[src]));
        }
        let planes: Vec<&[f32]> = self.planar.iter().map(Vec::as_slice).collect();
        let units = self.core.encode(&planes);
        Ok(self.packets(units))
    }

    pub fn flush(&mut self) -> Vec<Packet> {
        let units = self.core.flush();
        self.packets(units)
    }

    fn packets(&mut self, units: Vec<Vec<u8>>) -> Vec<Packet> {
        units
            .into_iter()
            .map(|data| {
                let pts = self.next;
                self.next += FRAME_LEN as i64;
                Packet {
                    data,
                    pts,
                    dts: pts,
                    keyframe: true,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod malformed;
#[cfg(test)]
mod tests;
