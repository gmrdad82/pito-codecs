pub(crate) mod celt;

use crate::media::{AudioBuffer, Error, Packet, Result};
use crate::opus::OpusHead;
use crate::opus::packet::Bandwidth;
use crate::opus::range::RangeEncoder;

use celt::{CeltEncoder, FrameParams};

pub const FRAME: usize = 960;
const DELAY: usize = 192;
const CELT_DELAY: usize = 120;
const MAX_PACKET: usize = 1276;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    Audio,
    Voip,
    LowDelay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub rate: u32,
    pub channels: u16,
    pub bitrate: u32,
    pub application: Application,
}

impl EncoderConfig {
    pub fn new(channels: u16, bitrate: u32) -> EncoderConfig {
        EncoderConfig {
            rate: 48000,
            channels,
            bitrate,
            application: Application::Audio,
        }
    }
}

pub struct Encoder {
    cfg: EncoderConfig,
    celt: CeltEncoder,
    rc: RangeEncoder,
    pending: Vec<f32>,
    consumed: usize,
    frames: u64,
    input: u64,
    final_range: u32,
}

impl Encoder {
    pub fn new(cfg: EncoderConfig) -> Result<Encoder> {
        if cfg.rate != 48000 {
            return Err(Error::Unsupported(format!(
                "Opus encoder input at {} Hz; resample to 48000 first",
                cfg.rate
            )));
        }
        if !(1..=2).contains(&cfg.channels) {
            return Err(Error::Unsupported(format!(
                "Opus encoder with {} channels",
                cfg.channels
            )));
        }
        if !(6_000..=510_000).contains(&cfg.bitrate) {
            return Err(Error::Invalid(format!(
                "Opus bitrate {} outside 6000..=510000",
                cfg.bitrate
            )));
        }
        let c = cfg.channels as usize;
        Ok(Encoder {
            cfg,
            celt: CeltEncoder::new(c),
            rc: RangeEncoder::new(MAX_PACKET),
            pending: vec![0.0; DELAY * c],
            consumed: 0,
            frames: 0,
            input: 0,
            final_range: 0,
        })
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    pub fn pre_skip(&self) -> u16 {
        (DELAY + CELT_DELAY) as u16
    }

    pub fn head(&self) -> OpusHead {
        OpusHead {
            channels: self.cfg.channels as usize,
            pre_skip: self.pre_skip(),
            input_sample_rate: self.cfg.rate,
            output_gain: 0,
            mapping_family: 0,
            streams: 1,
            coupled_streams: self.cfg.channels as usize - 1,
            mapping: (0..self.cfg.channels as u8).collect(),
        }
    }

    pub fn codec_private(&self) -> Vec<u8> {
        self.head().to_bytes()
    }

    pub fn reset(&mut self) {
        let c = self.cfg.channels as usize;
        self.celt.reset();
        self.pending.clear();
        self.pending.resize(DELAY * c, 0.0);
        self.consumed = 0;
        self.frames = 0;
        self.input = 0;
    }

    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    pub fn frame_samples(&self) -> usize {
        FRAME
    }

    pub fn encode(&mut self, audio: &AudioBuffer) -> Result<Vec<Packet>> {
        if audio.rate != self.cfg.rate || audio.channels != self.cfg.channels {
            return Err(Error::Invalid(format!(
                "audio is {} Hz x{}, encoder expects {} Hz x{}",
                audio.rate, audio.channels, self.cfg.rate, self.cfg.channels
            )));
        }
        self.input += audio.frames() as u64;
        self.pending.extend_from_slice(&audio.samples);
        let mut out = Vec::new();
        self.drain(&mut out);
        Ok(out)
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        let c = self.cfg.channels as usize;
        let need = self.input + self.pre_skip() as u64;
        let mut out = Vec::new();
        while self.frames * (FRAME as u64) < need {
            let have = self.pending.len() / c - self.consumed / c;
            if have < FRAME {
                self.pending
                    .resize(self.pending.len() + (FRAME - have) * c, 0.0);
            }
            self.drain(&mut out);
        }
        Ok(out)
    }

    fn drain(&mut self, out: &mut Vec<Packet>) {
        let c = self.cfg.channels as usize;
        while self.pending.len() - self.consumed >= FRAME * c {
            let start = self.consumed;
            self.consumed += FRAME * c;
            let pkt = self.encode_frame(start);
            out.push(pkt);
        }
        if self.consumed > 0 && self.consumed * 2 >= self.pending.len() {
            self.pending.drain(..self.consumed);
            self.consumed = 0;
        }
    }

    fn encode_frame(&mut self, start: usize) -> Packet {
        let c = self.cfg.channels as usize;
        let total = ((self.cfg.bitrate as usize * FRAME) / (48000 * 8)).clamp(3, MAX_PACKET);
        let nbytes = total - 1;
        let bandwidth = Bandwidth::Fullband;
        let end = bandwidth.celt_end_band();
        let params = FrameParams {
            start: 0,
            end,
            nbytes,
            intensity: intensity_band(self.cfg.bitrate, c, end),
        };
        self.rc.reset(nbytes);
        let pcm = &self.pending[start..start + FRAME * c];
        self.celt.encode_frame(pcm, FRAME, &params, &mut self.rc);
        let toc = (31u8 << 3) | (((c == 2) as u8) << 2);
        let mut data = Vec::with_capacity(total);
        data.push(toc);
        data.extend_from_slice(self.rc.finish());
        self.final_range = self.rc.rng();
        let pts = self.frames as i64 * FRAME as i64;
        self.frames += 1;
        Packet {
            data,
            pts,
            dts: pts,
            keyframe: true,
        }
    }
}

fn intensity_band(bitrate: u32, channels: usize, end: usize) -> usize {
    if channels == 1 {
        return end;
    }
    let kbps = bitrate / 1000;
    let band = match kbps {
        0..=31 => 4,
        32..=47 => 8,
        48..=63 => 11,
        64..=79 => 13,
        80..=95 => 15,
        96..=111 => 17,
        112..=127 => 19,
        128..=143 => 20,
        _ => 21,
    };
    band.min(end)
}

#[cfg(test)]
mod tests;
