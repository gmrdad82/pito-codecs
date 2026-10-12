mod encode;
mod ffi;
mod h264;
mod headers;
mod hevc;
mod host;
mod va;
#[cfg(feature = "vp9")]
mod vp9;

#[cfg(test)]
mod abi_tests;

use crate::gpu::nal::{self, Framing};
use crate::gpu::{
    Backend, DecoderBackend, Direction, EncoderBackend, EncoderConfig, GpuFrame, GpuInner,
};
use crate::media::{Codec, Error, Frame, Packet, Result};
use crate::syntax::bits::unescape_rbsp;
use crate::syntax::{h264 as h264_syntax, hevc as hevc_syntax};

impl Picture {
    pub fn into_frame(self) -> Result<Frame> {
        match self {
            Picture::Cpu(f) => Ok(f),
            Picture::Gpu(g) => g.download().map_err(Error::Backend),
        }
    }
}

use crate::gpu::biplanar::Geometry;
use va::Session;

pub use va::{DmaBuf, DmaLayer, DmaObject, DmaPlane, GpuSurface, Picture, Scaler};

pub use encode::VaEncoder;

pub const LAG: usize = 3;

pub const HELD: usize = 12;

const HEADER_BYTES: usize = 512;

pub fn header_rbsp(payload: &[u8], full: bool) -> Option<Vec<u8>> {
    if full {
        return (payload.len() > HEADER_BYTES).then(|| unescape_rbsp(payload));
    }
    Some(unescape_rbsp(&payload[..payload.len().min(HEADER_BYTES)]))
}

pub trait Accel: Send {
    fn surfaces(&self) -> &[ffi::VASurfaceID];
    fn decode_h264(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferH264,
        iq: &ffi::VAIQMatrixBufferH264,
        slices: &[(ffi::VASliceParameterBufferH264, Vec<u8>)],
    ) -> std::result::Result<(), String>;
    fn decode_hevc(
        &mut self,
        target: usize,
        pic: &ffi::VAPictureParameterBufferHEVC,
        iq: Option<&ffi::VAIQMatrixBufferHEVC>,
        slices: &[(ffi::VASliceParameterBufferHEVC, Vec<u8>)],
    ) -> std::result::Result<(), String>;
    fn read(&mut self, index: usize) -> std::result::Result<Picture, String>;
    fn prefetch(&mut self, _index: usize) {}
    fn held(&self, index: usize) -> bool;
    fn fill(&mut self, index: usize, from: Option<usize>) -> std::result::Result<(), String>;
}

fn h264_profiles(profile_idc: u8) -> Option<&'static [ffi::VAProfile]> {
    Some(match profile_idc {
        66 => &[
            ffi::VAProfileH264ConstrainedBaseline,
            ffi::VAProfileH264Main,
            ffi::VAProfileH264High,
        ],
        77 => &[ffi::VAProfileH264Main, ffi::VAProfileH264High],
        100 => &[ffi::VAProfileH264High],
        _ => return None,
    })
}

fn hevc_profiles(profile_idc: u8, bits: u32) -> Option<&'static [ffi::VAProfile]> {
    Some(match (profile_idc, bits) {
        (1 | 3, 8) => &[ffi::VAProfileHEVCMain, ffi::VAProfileHEVCMain10],
        (2, 8) => &[ffi::VAProfileHEVCMain10, ffi::VAProfileHEVCMain],
        (2, 10) => &[ffi::VAProfileHEVCMain10],
        _ => return None,
    })
}

enum Front {
    H264(Box<h264::Front<Session>>),
    Hevc(Box<hevc::Front<Session>>),
}

pub struct VaDecoder {
    codec: Codec,
    framing: Framing,
    front: Option<Front>,
    name: String,
}

fn parameter_set(sets: &[Vec<u8>], header: usize, ty: impl Fn(u8) -> bool) -> Option<Vec<u8>> {
    sets.iter()
        .find(|n| n.first().is_some_and(|&h| ty(h)))
        .and_then(|n| Some(unescape_rbsp(n.get(header..)?)))
}

fn backend(e: impl std::fmt::Display) -> Error {
    Error::Backend(e.to_string())
}

fn open_front(codec: Codec, framing: Framing, sets: &[Vec<u8>]) -> Result<(Front, String)> {
    match codec {
        Codec::H264 => {
            let rbsp = parameter_set(sets, 1, |h| h & 0x1f == 7)
                .ok_or_else(|| Error::Invalid("no H.264 SPS".into()))?;
            let sps = h264_syntax::params::Sps::parse(&rbsp).map_err(backend)?;
            sps.check_supported()
                .map_err(|e| Error::Unsupported(e.to_string()))?;
            let bits = sps.bit_depth_luma;
            if sps.chroma_format_idc != 1 || bits != 8 || sps.bit_depth_chroma != 8 {
                return Err(Error::Unsupported(format!(
                    "H.264 chroma format {} at {bits}-bit on the GPU",
                    sps.chroma_format_idc
                )));
            }
            if !sps.frame_mbs_only {
                return Err(Error::Unsupported("interlaced H.264 on the GPU".into()));
            }
            let profiles = h264_profiles(sps.profile_idc).ok_or_else(|| {
                Error::Unsupported(format!("H.264 profile_idc {} on the GPU", sps.profile_idc))
            })?;
            let mbs = (sps.pic_width_in_mbs, sps.frame_height_in_mbs());
            let count = sps.max_dpb_frames().saturating_add(2 + LAG + HELD).min(32);
            let geometry = Geometry {
                crop: sps.crop_rect(),
                bits: 8,
            };
            let session = Session::new(profiles, (sps.width(), sps.height()), count, geometry)
                .map_err(Error::Backend)?;
            let name = format!("VA-API H.264 ({})", session.vendor());
            let front = h264::Front::new(session, framing, mbs, sets)?;
            Ok((Front::H264(Box::new(front)), name))
        }
        Codec::Hevc => {
            let rbsp = parameter_set(sets, 2, |h| (h >> 1) & 0x3f == 33)
                .ok_or_else(|| Error::Invalid("no HEVC SPS".into()))?;
            let sps = hevc_syntax::params::Sps::parse(&rbsp).map_err(backend)?;
            sps.check_gpu()
                .map_err(|e| Error::Unsupported(e.to_string()))?;
            let bits = sps.bit_depth_luma;
            if sps.chroma_format_idc != 1 || sps.bit_depth_chroma != bits || sps.range_extension {
                return Err(Error::Unsupported(format!(
                    "HEVC chroma format {} at {bits}-bit on the GPU",
                    sps.chroma_format_idc
                )));
            }
            let profiles = hevc_profiles(sps.profile_idc, bits).ok_or_else(|| {
                Error::Unsupported(format!(
                    "HEVC profile {} at {bits}-bit on the GPU",
                    sps.profile_idc
                ))
            })?;
            let count = (sps.max_dec_pic_buffering as usize)
                .saturating_add(2 + LAG + HELD)
                .min(32);
            let size = (sps.width, sps.height);
            let geometry = Geometry {
                crop: sps.crop_rect(),
                bits,
            };
            let session = Session::new(profiles, size, count, geometry).map_err(Error::Backend)?;
            let name = format!("VA-API HEVC ({})", session.vendor());
            let front = hevc::Front::new(session, framing, size, bits, sets)?;
            Ok((Front::Hevc(Box::new(front)), name))
        }
        c => Err(Error::Unsupported(format!("{c} on the GPU"))),
    }
}

impl VaDecoder {
    pub fn new(codec: Codec, extradata: &[u8]) -> Result<Self> {
        let config = nal::parse_extradata(codec, extradata)?;
        let mut d = Self {
            codec,
            framing: config.framing,
            front: None,
            name: "VA-API".into(),
        };
        if config.parameter_sets.is_empty() {
            va::probe().map_err(Error::Backend)?;
            check_profile(codec, Direction::Decode)?;
        } else {
            d.open(&config.parameter_sets)?;
        }
        Ok(d)
    }

    fn open(&mut self, sets: &[Vec<u8>]) -> Result<()> {
        let (front, name) = open_front(self.codec, self.framing, sets)?;
        self.front = Some(front);
        self.name = name;
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn decode(&mut self, packet: &Packet, gpu: bool) -> Result<Vec<Picture>> {
        if self.front.is_none() {
            let sets: Vec<Vec<u8>> = self
                .framing
                .split(&packet.data)?
                .into_iter()
                .filter(|n| nal::is_parameter_set(self.codec, n))
                .map(<[u8]>::to_vec)
                .collect();
            if sets.is_empty() {
                return Ok(Vec::new());
            }
            self.open(&sets)?;
        }
        match &mut self.front {
            Some(Front::H264(f)) => {
                f.accel_mut().gpu = gpu;
                f.decode(&packet.data, packet.pts)
            }
            Some(Front::Hevc(f)) => {
                f.accel_mut().gpu = gpu;
                f.decode(&packet.data, packet.pts)
            }
            None => Ok(Vec::new()),
        }
    }

    pub fn flush(&mut self, gpu: bool) -> Result<Vec<Picture>> {
        match &mut self.front {
            Some(Front::H264(f)) => {
                f.accel_mut().gpu = gpu;
                f.flush()
            }
            Some(Front::Hevc(f)) => {
                f.accel_mut().gpu = gpu;
                f.flush()
            }
            None => Ok(Vec::new()),
        }
    }

    pub fn reset(&mut self) {
        match &mut self.front {
            Some(Front::H264(f)) => f.reset(),
            Some(Front::Hevc(f)) => f.reset(),
            None => {}
        }
    }
}

fn profiles_for(codec: Codec) -> &'static [ffi::VAProfile] {
    match codec {
        Codec::H264 => &[
            ffi::VAProfileH264High,
            ffi::VAProfileH264Main,
            ffi::VAProfileH264ConstrainedBaseline,
        ],
        Codec::Hevc => &[ffi::VAProfileHEVCMain, ffi::VAProfileHEVCMain10],
        Codec::Vp9 => &[ffi::VAProfileVP9Profile0, ffi::VAProfileVP9Profile2],
        Codec::Aac | Codec::Opus => &[],
    }
}

fn supports(display: &va::Display, profile: ffi::VAProfile, direction: Direction) -> bool {
    let entrypoints: &[ffi::VAEntrypoint] = match direction {
        Direction::Decode => &[ffi::VAEntrypointVLD],
        Direction::Encode => &[ffi::VAEntrypointEncSlice, ffi::VAEntrypointEncSliceLP],
    };
    entrypoints.iter().any(|&e| {
        display
            .attributes(profile, e, &[ffi::VAConfigAttribRTFormat])
            .is_ok_and(|v| {
                v.first()
                    .is_some_and(|&v| v != ffi::VA_ATTRIB_NOT_SUPPORTED && v != 0)
            })
    })
}

fn check_profile(codec: Codec, direction: Direction) -> Result<()> {
    let display = va::probe().map_err(Error::Backend)?;
    if profiles_for(codec)
        .iter()
        .any(|&p| supports(&display, p, direction))
    {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "this VA-API driver ({}) has no {codec:?} {direction:?}",
            display.vendor()
        )))
    }
}

pub fn available() -> Vec<(Codec, Direction)> {
    let mut out = Vec::new();
    for codec in [Codec::H264, Codec::Hevc, Codec::Vp9] {
        for direction in [Direction::Decode, Direction::Encode] {
            if check_profile(codec, direction).is_ok() {
                out.push((codec, direction));
            }
        }
    }
    out
}

pub fn decoder(codec: Codec, extradata: &[u8]) -> Result<Box<dyn DecoderBackend>> {
    #[cfg(feature = "vp9")]
    if codec == Codec::Vp9 {
        return Ok(Box::new(vp9::Vp9Decoder::new()?));
    }
    Ok(Box::new(VaDecoder::new(codec, extradata)?))
}

pub fn encoder(codec: Codec, config: &EncoderConfig) -> Result<Box<dyn EncoderBackend>> {
    Ok(Box::new(VaEncoder::new(codec, config)?))
}

fn frames(pictures: Vec<Picture>) -> Result<Vec<Frame>> {
    pictures.into_iter().map(Picture::into_frame).collect()
}

fn gpu_frames(pictures: Vec<Picture>) -> Vec<GpuFrame> {
    pictures
        .into_iter()
        .map(|p| match p {
            Picture::Cpu(f) => GpuFrame::cpu(f),
            Picture::Gpu(g) => GpuFrame::vaapi(g),
        })
        .collect()
}

impl DecoderBackend for VaDecoder {
    fn backend(&self) -> Backend {
        Backend::VaApi
    }

    fn name(&self) -> &str {
        VaDecoder::name(self)
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        frames(VaDecoder::decode(self, packet, false)?)
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        frames(VaDecoder::flush(self, false)?)
    }

    fn reset(&mut self) {
        VaDecoder::reset(self)
    }

    fn decode_gpu(&mut self, packet: &Packet) -> Result<Vec<GpuFrame>> {
        Ok(gpu_frames(VaDecoder::decode(self, packet, true)?))
    }

    fn flush_gpu(&mut self) -> Result<Vec<GpuFrame>> {
        Ok(gpu_frames(VaDecoder::flush(self, true)?))
    }
}

impl EncoderBackend for VaEncoder {
    fn backend(&self) -> Backend {
        Backend::VaApi
    }

    fn name(&self) -> &str {
        VaEncoder::name(self)
    }

    fn b_frames(&self) -> u8 {
        VaEncoder::b_frames(self)
    }

    fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        VaEncoder::encode(self, frame)
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        VaEncoder::flush(self)
    }

    fn extradata(&self) -> Option<&[u8]> {
        VaEncoder::extradata(self)
    }

    fn encode_gpu(&mut self, frame: &GpuFrame) -> Result<Vec<Packet>> {
        match &frame.inner {
            GpuInner::VaApi(g) => self.encode_surface(g, frame.time_base()),
            GpuInner::Cpu(f) => VaEncoder::encode(self, f),
        }
    }
}

pub fn driver() -> Result<String> {
    va::probe()
        .map(|d| d.vendor().to_string())
        .map_err(Error::Backend)
}
