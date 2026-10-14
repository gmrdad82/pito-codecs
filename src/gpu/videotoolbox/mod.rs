mod decode;
mod encode;
mod order;
mod planes;

use objc2_core_media::{CMVideoCodecType, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPlaneCount, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

use crate::gpu::{Backend, DecoderBackend, Direction, EncoderBackend, EncoderConfig, GpuFrame};
use crate::media::{Codec, Error, Frame, Packet, Result};

use decode::VtDecoder;
use encode::VtEncoder;
use planes::{Source, Target};

const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const P010_VIDEO: u32 = u32::from_be_bytes(*b"x420");
const P010_FULL: u32 = u32::from_be_bytes(*b"xf20");

fn codec_type(codec: Codec) -> Result<CMVideoCodecType> {
    match codec {
        Codec::H264 => Ok(kCMVideoCodecType_H264),
        Codec::Hevc => Ok(kCMVideoCodecType_HEVC),
        Codec::Aac | Codec::Vp9 | Codec::Opus => {
            Err(Error::Unsupported(format!("{codec:?} on VideoToolbox")))
        }
        Codec::Ac3 | Codec::Eac3 => Err(Error::Unsupported(format!("{codec:?} on VideoToolbox"))),
    }
}

fn label(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "VideoToolbox HEVC",
        _ => "VideoToolbox H.264",
    }
}

struct Locked<'a> {
    buffer: &'a CVImageBuffer,
    flags: CVPixelBufferLockFlags,
}

impl<'a> Locked<'a> {
    fn new(buffer: &'a CVImageBuffer, flags: CVPixelBufferLockFlags) -> Result<Self> {
        if CVPixelBufferGetPlaneCount(buffer) != 2 {
            return Err(Error::Backend("pixel buffer is not biplanar".into()));
        }
        if unsafe { CVPixelBufferLockBaseAddress(buffer, flags) } != 0 {
            return Err(Error::Backend("cannot lock a pixel buffer".into()));
        }
        Ok(Self { buffer, flags })
    }

    fn raw(&self, i: usize) -> Result<(*mut u8, usize, usize)> {
        let base = CVPixelBufferGetBaseAddressOfPlane(self.buffer, i).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRowOfPlane(self.buffer, i);
        let height = CVPixelBufferGetHeightOfPlane(self.buffer, i);
        if base.is_null() || stride == 0 || height == 0 {
            return Err(Error::Backend(format!(
                "pixel buffer plane {i} is not mapped"
            )));
        }
        let len = stride
            .checked_mul(height)
            .ok_or_else(|| Error::Backend("pixel buffer plane size overflows".into()))?;
        Ok((base, stride, len))
    }

    fn planes(&self) -> Result<(Source<'_>, Source<'_>)> {
        let (a, b) = (self.raw(0)?, self.raw(1)?);
        let view = |(base, stride, len): (*mut u8, usize, usize)| Source {
            data: unsafe { std::slice::from_raw_parts(base, len) },
            stride,
        };
        Ok((view(a), view(b)))
    }

    fn planes_mut(&mut self) -> Result<(Target<'_>, Target<'_>)> {
        let (a, b) = (self.raw(0)?, self.raw(1)?);
        let view = |(base, stride, len): (*mut u8, usize, usize)| Target {
            data: unsafe { std::slice::from_raw_parts_mut(base, len) },
            stride,
        };
        Ok((view(a), view(b)))
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        unsafe { CVPixelBufferUnlockBaseAddress(self.buffer, self.flags) };
    }
}

pub fn available() -> Vec<(Codec, Direction)> {
    let mut out = Vec::new();
    for codec in [Codec::H264, Codec::Hevc] {
        if decode::hardware(codec) {
            out.push((codec, Direction::Decode));
        }
        if encode::hardware(codec) {
            out.push((codec, Direction::Encode));
        }
    }
    out
}

pub fn decoder(codec: Codec, extradata: &[u8]) -> Result<Box<dyn DecoderBackend>> {
    Ok(Box::new(VtDecoder::new(codec, extradata)?))
}

pub fn encoder(codec: Codec, config: &EncoderConfig) -> Result<Box<dyn EncoderBackend>> {
    Ok(Box::new(VtEncoder::new(codec, config)?))
}

impl DecoderBackend for VtDecoder {
    fn backend(&self) -> Backend {
        Backend::VideoToolbox
    }

    fn name(&self) -> &str {
        VtDecoder::name(self)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        VtDecoder::decode(self, packet)
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        VtDecoder::flush(self)
    }

    fn reset(&mut self) {
        VtDecoder::reset(self)
    }
}

impl EncoderBackend for VtEncoder {
    fn backend(&self) -> Backend {
        Backend::VideoToolbox
    }

    fn name(&self) -> &str {
        VtEncoder::name(self)
    }

    fn b_frames(&self) -> u8 {
        VtEncoder::b_frames(self)
    }

    fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        VtEncoder::encode(self, frame)
    }

    fn encode_gpu(&mut self, frame: &GpuFrame) -> Result<Vec<Packet>> {
        VtEncoder::encode_gpu(self, frame)
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        VtEncoder::flush(self)
    }

    fn extradata(&self) -> Option<&[u8]> {
        VtEncoder::extradata(self)
    }
}
