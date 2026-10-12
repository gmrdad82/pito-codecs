#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod dec;
mod enc;

#[cfg(test)]
mod tests;

use crate::media::{self, Error, Frame, Packet, PixelFormat, Plane, Result, TimeBase};

pub use dec::ColorInfo;
pub use enc::{Preset, Profile};

pub struct Decoder {
    inner: dec::Decoder,
    time_base: TimeBase,
    color: Option<ColorInfo>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            inner: dec::Decoder::new(),
            time_base: TimeBase::new(1, 90000),
            color: None,
        }
    }

    pub fn with_threads(threads: usize) -> Self {
        Self {
            inner: dec::Decoder::with_threads(threads),
            time_base: TimeBase::new(1, 90000),
            color: None,
        }
    }

    pub fn with_extradata(avcc: &[u8]) -> Result<Self> {
        let mut d = Self::new();
        d.inner.configure_avcc(avcc).map_err(dec_error)?;
        Ok(d)
    }

    pub fn set_time_base(&mut self, time_base: TimeBase) {
        self.time_base = time_base;
    }

    pub fn color(&self) -> Option<ColorInfo> {
        self.color
    }

    pub fn threads(&self) -> usize {
        self.inner.threads()
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        let pics = self
            .inner
            .decode(&packet.data, packet.pts)
            .map_err(dec_error)?;
        Ok(pics.into_iter().map(|p| self.frame(p)).collect())
    }

    pub fn recycle(&mut self, frame: Frame) {
        for p in frame.planes {
            self.inner.recycle(p.data);
        }
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        let pics = self.inner.flush();
        if let Some(e) = self.inner.take_error() {
            return Err(dec_error(e));
        }
        Ok(pics.into_iter().map(|p| self.frame(p)).collect())
    }

    fn frame(&mut self, p: dec::Picture) -> Frame {
        self.color = Some(p.color);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let plane = |data: Vec<u8>, stride: usize, width: usize, height: usize| Plane {
            data,
            stride,
            width,
            height,
        };
        Frame {
            format: PixelFormat::Yuv420p8,
            width: p.width,
            height: p.height,
            pts: p.pts,
            time_base: self.time_base,
            planes: [
                plane(p.y, p.y_stride, p.width as usize, p.height as usize),
                plane(p.u, p.uv_stride, cw, ch),
                plane(p.v, p.uv_stride, cw, ch),
            ],
        }
    }
}

fn dec_error(e: dec::Error) -> Error {
    e.media("h264")
}

fn enc_error(e: enc::Error) -> Error {
    Error::Invalid(format!("h264 encoder: {e}"))
}

#[deprecated(note = "use pito_codecs::media::Rate")]
pub type Rate = crate::media::Rate;

#[deprecated(note = "use pito_codecs::media::Bitstream")]
pub type Bitstream = crate::media::Bitstream;

#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub profile: Profile,
    pub preset: Preset,
    pub rate: media::Rate,
    pub gop: u32,
    pub b_frames: u8,
    pub threads: usize,
    pub bitstream: media::Bitstream,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: (30, 1),
            profile: Profile::High,
            preset: Preset::Balanced,
            rate: media::Rate::Crf(23),
            gop: 250,
            b_frames: 2,
            threads: 0,
            bitstream: media::Bitstream::LengthPrefixed,
        }
    }
}

pub struct Encoder {
    inner: enc::Encoder,
    width: u32,
    height: u32,
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        let mut cfg =
            enc::EncoderConfig::new(config.width, config.height, config.fps.0, config.fps.1);
        cfg.profile = config.profile;
        cfg.preset = config.preset;
        cfg.rate = match config.rate {
            media::Rate::Crf(q) => enc::RateControl::Crf(f32::from(q)),
            media::Rate::Bitrate(kbps) => enc::RateControl::Cbr { kbps },
            media::Rate::Vbr { kbps, max_kbps } => enc::RateControl::Vbr {
                target_kbps: kbps,
                max_kbps,
            },
            media::Rate::Qp(q) => enc::RateControl::Qp(q),
        };
        cfg.keyint = config.gop;
        cfg.bframes = config.b_frames;
        cfg.threads = config.threads;
        cfg.format = match config.bitstream {
            media::Bitstream::LengthPrefixed => enc::PacketFormat::LengthPrefixed,
            media::Bitstream::AnnexB => enc::PacketFormat::AnnexB,
        };
        let inner = enc::Encoder::new(cfg).map_err(enc_error)?;
        Ok(Self {
            inner,
            width: config.width,
            height: config.height,
        })
    }

    pub fn avcc(&self) -> Vec<u8> {
        self.inner.avcc()
    }

    pub fn sps(&self) -> Vec<u8> {
        self.inner.sps_pps().0
    }

    pub fn pps(&self) -> Vec<u8> {
        self.inner.sps_pps().1
    }

    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        if frame.format != PixelFormat::Yuv420p8 {
            return Err(Error::Unsupported(format!(
                "h264 encoder: {:?} input, 8-bit 4:2:0 only",
                frame.format
            )));
        }
        if (frame.width, frame.height) != (self.width, self.height) {
            return Err(Error::Invalid(format!(
                "h264 encoder: {}x{} frame for a {}x{} stream",
                frame.width, frame.height, self.width, self.height
            )));
        }
        let [y, u, v] = &frame.planes;
        if u.stride != v.stride {
            return Err(Error::Invalid(
                "h264 encoder: chroma planes with different strides".into(),
            ));
        }
        let yuv = enc::YuvFrame {
            y: &y.data,
            u: &u.data,
            v: &v.data,
            y_stride: y.stride,
            uv_stride: u.stride,
        };
        let packets = self.inner.encode(&yuv, frame.pts).map_err(enc_error)?;
        Ok(packets.into_iter().map(packet).collect())
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        let packets = self.inner.flush().map_err(enc_error)?;
        Ok(packets.into_iter().map(packet).collect())
    }
}

fn packet(p: enc::Packet) -> Packet {
    Packet {
        data: p.data,
        pts: p.pts,
        dts: p.dts,
        keyframe: p.keyframe,
    }
}
