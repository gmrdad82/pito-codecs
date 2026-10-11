pub(crate) mod celt;
mod isa;

use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::media::{AudioBuffer, Error, Packet, Result};
use crate::opus::OpusHead;
use crate::opus::ogg;
use crate::opus::packet::Bandwidth;
use crate::opus::range::RangeEncoder;

use celt::{CeltEncoder, FrameParams};

pub const FRAME: usize = 960;
const DELAY: usize = 192;
const CELT_DELAY: usize = 120;
const MAX_PACKET: usize = 1276;
const CHUNK: usize = 256;
const HISTORY: usize = 1032;

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
    pub vbr: bool,
}

impl EncoderConfig {
    pub fn new(channels: u16, bitrate: u32) -> EncoderConfig {
        EncoderConfig {
            rate: 48000,
            channels,
            bitrate,
            application: Application::Audio,
            vbr: true,
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
        if !audio
            .samples
            .len()
            .is_multiple_of(self.cfg.channels as usize)
        {
            return Err(Error::Invalid(format!(
                "{} samples do not split into {} channels",
                audio.samples.len(),
                self.cfg.channels
            )));
        }
        self.input += audio.frames() as u64;
        self.pending.extend_from_slice(&audio.samples);
        let mut out = Vec::new();
        self.drain(&mut out)?;
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
            self.drain(&mut out)?;
        }
        Ok(out)
    }

    fn drain(&mut self, out: &mut Vec<Packet>) -> Result<()> {
        let c = self.cfg.channels as usize;
        let step = FRAME * c;
        loop {
            let avail = (self.pending.len() - self.consumed) / step;
            if avail == 0 {
                break;
            }
            let at_chunk = self.frames.is_multiple_of(CHUNK as u64);
            if at_chunk && avail >= 2 * CHUNK && rayon::current_num_threads() > 1 {
                self.encode_chunks(avail / CHUNK, out)?;
                continue;
            }
            if at_chunk && self.frames > 0 {
                let h = self.consumed.saturating_sub(HISTORY * c);
                self.celt.prime(&self.pending[h..self.consumed]);
            }
            let start = self.consumed;
            self.consumed += step;
            let pcm = &self.pending[start..start + step];
            let (cfg, celt, rc, index) = (&self.cfg, &mut self.celt, &mut self.rc, self.frames);
            let (pkt, rng) = catch_unwind(AssertUnwindSafe(|| {
                encode_packet(cfg, celt, rc, pcm, index)
            }))
            .map_err(|_| crashed())?;
            self.final_range = rng;
            self.frames += 1;
            out.push(pkt);
        }
        let keep = HISTORY * c;
        if self.consumed > keep && (self.consumed - keep) * 2 >= self.pending.len() {
            self.pending.drain(..self.consumed - keep);
            self.consumed = keep;
        }
        Ok(())
    }

    fn encode_chunks(&mut self, chunks: usize, out: &mut Vec<Packet>) -> Result<()> {
        use rayon::prelude::*;
        let c = self.cfg.channels as usize;
        let step = FRAME * c;
        let cfg = self.cfg;
        let base = self.consumed;
        let first = self.frames;
        let pending = &self.pending;
        let done: Vec<Option<(Vec<Packet>, CeltEncoder, u32)>> = (0..chunks)
            .into_par_iter()
            .map(|k| {
                catch_unwind(|| {
                    let mut celt = CeltEncoder::new(c);
                    let mut rc = RangeEncoder::new(MAX_PACKET);
                    let start = base + k * CHUNK * step;
                    let f0 = first + (k * CHUNK) as u64;
                    if f0 > 0 {
                        celt.prime(&pending[start.saturating_sub(HISTORY * c)..start]);
                    }
                    let mut packets = Vec::with_capacity(CHUNK);
                    let mut rng = 0;
                    for i in 0..CHUNK {
                        let s = start + i * step;
                        let (p, r) = encode_packet(
                            &cfg,
                            &mut celt,
                            &mut rc,
                            &pending[s..s + step],
                            f0 + i as u64,
                        );
                        packets.push(p);
                        rng = r;
                    }
                    (packets, celt, rng)
                })
                .ok()
            })
            .collect();
        if done.iter().any(Option::is_none) {
            return Err(crashed());
        }
        let mut last = None;
        for (packets, celt, rng) in done.into_iter().flatten() {
            out.extend(packets);
            last = Some((celt, rng));
        }
        if let Some((celt, rng)) = last {
            self.celt = celt;
            self.final_range = rng;
        }
        self.frames += (chunks * CHUNK) as u64;
        self.consumed += chunks * CHUNK * step;
        Ok(())
    }
}

fn crashed() -> Error {
    Error::Backend("Opus encoder failed on this input".into())
}

fn encode_packet(
    cfg: &EncoderConfig,
    celt: &mut CeltEncoder,
    rc: &mut RangeEncoder,
    pcm: &[f32],
    index: u64,
) -> (Packet, u32) {
    let c = cfg.channels as usize;
    let end = Bandwidth::Fullband.celt_end_band();
    let allow_pf = index % CHUNK as u64 != CHUNK as u64 - 1;
    let a = celt.analyse(pcm, FRAME, end, allow_pf);
    let base = (cfg.bitrate as usize * FRAME) as f32 / (48000.0 * 8.0);
    let total = if cfg.vbr {
        celt.vbr.bytes(base, MAX_PACKET, &a)
    } else {
        (base as usize).clamp(3, MAX_PACKET)
    };
    let nbytes = total - 1;
    let params = FrameParams {
        start: 0,
        end,
        nbytes,
        intensity: intensity_band(cfg.bitrate, c, end),
    };
    rc.reset(nbytes);
    celt.encode_frame(FRAME, &params, &a, rc);
    let toc = (31u8 << 3) | (((c == 2) as u8) << 2);
    let mut data = Vec::with_capacity(total);
    data.push(toc);
    data.extend_from_slice(rc.finish());
    let pts = index as i64 * FRAME as i64;
    (
        Packet {
            data,
            pts,
            dts: pts,
            keyframe: true,
        },
        rc.rng(),
    )
}

pub struct OggWriter<W: Write> {
    enc: Encoder,
    out: ogg::Writer<W>,
}

impl<W: Write> OggWriter<W> {
    pub fn new(cfg: EncoderConfig, w: W) -> Result<OggWriter<W>> {
        let enc = Encoder::new(cfg)?;
        let out = ogg::Writer::new(w, 0x5049_544f, &enc.head(), "PITO codecs")?;
        Ok(OggWriter { enc, out })
    }

    pub fn write(&mut self, audio: &AudioBuffer) -> Result<()> {
        for p in self.enc.encode(audio)? {
            self.out.push(&p.data, p.pts + FRAME as i64)?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        let end = (self.enc.input + self.enc.pre_skip() as u64) as i64;
        for p in self.enc.flush()? {
            self.out.push(&p.data, (p.pts + FRAME as i64).min(end))?;
        }
        self.out.finish()
    }
}

pub fn encode_ogg(cfg: EncoderConfig, audio: &AudioBuffer) -> Result<Vec<u8>> {
    let mut w = OggWriter::new(cfg, Vec::new())?;
    w.write(audio)?;
    w.finish()
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
