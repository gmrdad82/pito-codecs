#[cfg(any(target_os = "linux", target_os = "windows"))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
mod biplanar;
mod nal;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
mod vaapi;

#[cfg(any(
    target_os = "windows",
    all(target_os = "linux", target_pointer_width = "64")
))]
mod nvenc;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod media_foundation;

use crate::media::{Codec, Error, Frame, Packet, Result, TimeBase};

#[cfg(target_os = "linux")]
pub use vaapi::{DmaBuf, DmaLayer, DmaObject, DmaPlane};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Decode,
    Encode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    VaApi,
    Nvenc,
    VideoToolbox,
    MediaFoundation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    Baseline,
    Main,
    High,
    Main10,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rate {
    Crf(u8),
    Bitrate(u32),
    Vbr { kbps: u32, max_kbps: u32 },
    Qp(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bitstream {
    LengthPrefixed,
    AnnexB,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub profile: Profile,
    pub rate: Rate,
    pub gop: u32,
    pub b_frames: u8,
    pub bitstream: Bitstream,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: (30, 1),
            profile: Profile::High,
            rate: Rate::Crf(20),
            gop: 0,
            b_frames: 0,
            bitstream: Bitstream::LengthPrefixed,
        }
    }
}

pub fn available() -> Vec<(Codec, Direction, Backend)> {
    let mut out = Vec::new();
    #[cfg(target_os = "linux")]
    out.extend(
        vaapi::available()
            .into_iter()
            .map(|(c, d)| (c, d, Backend::VaApi)),
    );
    #[cfg(any(
        target_os = "windows",
        all(target_os = "linux", target_pointer_width = "64")
    ))]
    out.extend(
        nvenc::available()
            .into_iter()
            .map(|(c, d)| (c, d, Backend::Nvenc)),
    );
    #[cfg(target_os = "macos")]
    out.extend(
        videotoolbox::available()
            .into_iter()
            .map(|(c, d)| (c, d, Backend::VideoToolbox)),
    );
    #[cfg(target_os = "windows")]
    out.extend(
        media_foundation::available()
            .into_iter()
            .map(|(c, d)| (c, d, Backend::MediaFoundation)),
    );
    out
}

pub fn driver(backend: Backend) -> Result<String> {
    match backend {
        #[cfg(target_os = "linux")]
        Backend::VaApi => vaapi::driver(),
        b => Err(Error::Unsupported(format!("{b:?} on this system"))),
    }
}

#[cfg_attr(
    not(all(
        feature = "h264",
        feature = "hevc",
        feature = "aac",
        feature = "mp4",
        feature = "filter",
        feature = "mkv",
        feature = "vp9"
    )),
    allow(dead_code)
)]
pub(crate) fn color(codec: Codec, extradata: &[u8]) -> Option<(u8, bool)> {
    let sets = nal::parse_extradata(codec, extradata).ok()?.parameter_sets;
    match codec {
        Codec::H264 => {
            let nal = sets
                .iter()
                .find(|n| n.first().is_some_and(|h| h & 0x1f == 7))?;
            let rbsp = crate::syntax::bits::unescape_rbsp(nal.get(1..)?);
            let vui = crate::syntax::h264::params::Sps::parse(&rbsp).ok()?.vui?;
            Some((vui.matrix_coefficients, vui.full_range))
        }
        Codec::Hevc => {
            let nal = sets
                .iter()
                .find(|n| n.first().is_some_and(|h| (h >> 1) & 0x3f == 33))?;
            let rbsp = crate::syntax::bits::unescape_rbsp(nal.get(2..)?);
            let vui = crate::syntax::hevc::params::Sps::parse(&rbsp).ok()?.vui?;
            Some((vui.matrix_coefficients, vui.full_range))
        }
        _ => None,
    }
}

pub(crate) trait DecoderBackend: Send {
    fn backend(&self) -> Backend;
    fn name(&self) -> &str;
    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>>;
    fn flush(&mut self) -> Result<Vec<Frame>>;
    fn reset(&mut self);
    fn decode_gpu(&mut self, packet: &Packet) -> Result<Vec<GpuFrame>> {
        Ok(self
            .decode(packet)?
            .into_iter()
            .map(GpuFrame::cpu)
            .collect())
    }
    fn flush_gpu(&mut self) -> Result<Vec<GpuFrame>> {
        Ok(self.flush()?.into_iter().map(GpuFrame::cpu).collect())
    }
}

pub(crate) trait EncoderBackend: Send {
    fn backend(&self) -> Backend;
    fn name(&self) -> &str;
    fn b_frames(&self) -> u8;
    fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>>;
    fn flush(&mut self) -> Result<Vec<Packet>>;
    fn extradata(&self) -> Option<&[u8]>;
    fn encode_gpu(&mut self, frame: &GpuFrame) -> Result<Vec<Packet>> {
        self.encode(&frame.download()?)
    }
}

type DecoderMaker = fn(Codec, &[u8]) -> Result<Box<dyn DecoderBackend>>;
type EncoderMaker = fn(Codec, &EncoderConfig) -> Result<Box<dyn EncoderBackend>>;

#[allow(clippy::vec_init_then_push)]
fn decoders() -> Vec<(&'static str, DecoderMaker)> {
    let mut out: Vec<(&'static str, DecoderMaker)> = Vec::new();
    #[cfg(target_os = "linux")]
    out.push(("VA-API", vaapi::decoder));
    #[cfg(target_os = "macos")]
    out.push(("VideoToolbox", videotoolbox::decoder));
    #[cfg(target_os = "windows")]
    out.push(("Media Foundation", media_foundation::decoder));
    out
}

#[allow(clippy::vec_init_then_push)]
fn encoders() -> Vec<(&'static str, EncoderMaker)> {
    let mut out: Vec<(&'static str, EncoderMaker)> = Vec::new();
    #[cfg(target_os = "linux")]
    out.push(("VA-API", vaapi::encoder));
    #[cfg(any(
        target_os = "windows",
        all(target_os = "linux", target_pointer_width = "64")
    ))]
    out.push(("NVENC", nvenc::encoder));
    #[cfg(target_os = "macos")]
    out.push(("VideoToolbox", videotoolbox::encoder));
    out
}

fn first<T>(makers: Vec<(&'static str, impl Fn() -> Result<T>)>, none: &str) -> Result<T> {
    let mut why = Vec::new();
    for (name, make) in makers {
        match make() {
            Ok(v) => return Ok(v),
            Err(e) => why.push(format!("{name}: {e}")),
        }
    }
    if why.is_empty() {
        why.push(none.to_string());
    }
    Err(Error::Unsupported(why.join("; ")))
}

pub struct HwDecoder {
    inner: Box<dyn DecoderBackend>,
    time_base: TimeBase,
}

impl HwDecoder {
    pub fn new(codec: Codec, extradata: &[u8]) -> Result<Self> {
        let makers = decoders()
            .into_iter()
            .map(|(name, make)| (name, move || make(codec, extradata)))
            .collect();
        let inner = first(makers, "no GPU decoder on this system")?;
        Ok(Self {
            inner,
            time_base: TimeBase::new(1, 90000),
        })
    }

    pub fn with_time_base(mut self, time_base: TimeBase) -> Self {
        self.time_base = time_base;
        self
    }

    pub fn backend(&self) -> Backend {
        self.inner.backend()
    }

    pub fn name(&self) -> &str {
        self.inner.name()
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        let frames = self.inner.decode(packet)?;
        Ok(self.stamp(frames))
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        let frames = self.inner.flush()?;
        Ok(self.stamp(frames))
    }

    pub fn decode_gpu(&mut self, packet: &Packet) -> Result<Vec<GpuFrame>> {
        let frames = self.inner.decode_gpu(packet)?;
        Ok(self.stamp_gpu(frames))
    }

    pub fn flush_gpu(&mut self) -> Result<Vec<GpuFrame>> {
        let frames = self.inner.flush_gpu()?;
        Ok(self.stamp_gpu(frames))
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    fn stamp(&self, mut frames: Vec<Frame>) -> Vec<Frame> {
        for f in &mut frames {
            f.time_base = self.time_base;
        }
        frames
    }

    fn stamp_gpu(&self, mut frames: Vec<GpuFrame>) -> Vec<GpuFrame> {
        for f in &mut frames {
            f.time_base = self.time_base;
        }
        frames
    }
}

pub(crate) enum GpuInner {
    Cpu(Frame),
    #[cfg(target_os = "linux")]
    VaApi(vaapi::GpuSurface),
}

pub struct GpuFrame {
    pub(crate) inner: GpuInner,
    time_base: TimeBase,
}

impl GpuFrame {
    pub(crate) fn cpu(frame: Frame) -> Self {
        let time_base = frame.time_base;
        Self {
            inner: GpuInner::Cpu(frame),
            time_base,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn vaapi(surface: vaapi::GpuSurface) -> Self {
        Self {
            inner: GpuInner::VaApi(surface),
            time_base: TimeBase::new(1, 90000),
        }
    }

    pub fn pts(&self) -> i64 {
        match &self.inner {
            GpuInner::Cpu(f) => f.pts,
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => g.pts,
        }
    }

    pub fn time_base(&self) -> TimeBase {
        self.time_base
    }

    pub fn size(&self) -> (u32, u32) {
        match &self.inner {
            GpuInner::Cpu(f) => (f.width, f.height),
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => (g.width(), g.height()),
        }
    }

    #[cfg_attr(
        not(all(
            feature = "h264",
            feature = "hevc",
            feature = "aac",
            feature = "mp4",
            feature = "filter",
            feature = "mkv",
            feature = "vp9"
        )),
        allow(dead_code)
    )]
    pub(crate) fn retime(&mut self, pts: i64, time_base: TimeBase) {
        self.time_base = time_base;
        match &mut self.inner {
            GpuInner::Cpu(f) => {
                f.pts = pts;
                f.time_base = time_base;
            }
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => g.pts = pts,
        }
    }

    #[cfg_attr(
        not(all(
            feature = "h264",
            feature = "hevc",
            feature = "aac",
            feature = "mp4",
            feature = "filter",
            feature = "mkv",
            feature = "vp9"
        )),
        allow(dead_code)
    )]
    pub(crate) fn into_cpu(self) -> Option<Frame> {
        match self.inner {
            GpuInner::Cpu(f) => Some(f),
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(_) => None,
        }
    }

    pub fn on_gpu(&self) -> bool {
        !matches!(self.inner, GpuInner::Cpu(_))
    }

    pub fn surface(&self) -> Option<(u64, u32)> {
        match &self.inner {
            GpuInner::Cpu(_) => None,
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => Some(g.key()),
        }
    }

    pub fn sync(&self) -> Result<()> {
        match &self.inner {
            GpuInner::Cpu(_) => Ok(()),
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => g.sync().map_err(Error::Backend),
        }
    }

    #[cfg(target_os = "linux")]
    pub fn dmabuf(&self, separate_layers: bool) -> Result<DmaBuf> {
        match &self.inner {
            GpuInner::VaApi(g) => g.dmabuf(separate_layers).map_err(Error::Backend),
            GpuInner::Cpu(_) => Err(Error::Unsupported(
                "this frame is in system memory, not on the GPU".into(),
            )),
        }
    }

    pub fn download(&self) -> Result<Frame> {
        let frame = match &self.inner {
            GpuInner::Cpu(f) => f.clone(),
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => g.download().map_err(Error::Backend)?,
        };
        Ok(Frame {
            time_base: self.time_base,
            ..frame
        })
    }
}

pub struct GpuScaler {
    #[cfg(target_os = "linux")]
    inner: vaapi::Scaler,
    size: (u32, u32),
}

impl GpuScaler {
    pub fn new(width: u32, height: u32, ten_bit: bool) -> Result<Self> {
        Self::with_frames(width, height, ten_bit, 8)
    }

    pub fn with_frames(width: u32, height: u32, ten_bit: bool, frames: usize) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(Error::Invalid("scaler size must be set".into()));
        }
        #[cfg(target_os = "linux")]
        {
            let bits = if ten_bit { 10 } else { 8 };
            let inner =
                vaapi::Scaler::new((width, height), bits, frames).map_err(Error::Backend)?;
            Ok(Self {
                inner,
                size: (width, height),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (ten_bit, frames);
            Err(Error::Unsupported("no GPU scaler on this system".into()))
        }
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn scale(&self, frame: &GpuFrame) -> Result<GpuFrame> {
        match &frame.inner {
            #[cfg(target_os = "linux")]
            GpuInner::VaApi(g) => {
                let out = self.inner.scale(g).map_err(Error::Backend)?;
                Ok(GpuFrame {
                    inner: GpuInner::VaApi(out),
                    time_base: frame.time_base,
                })
            }
            GpuInner::Cpu(_) => Err(Error::Unsupported(
                "the GPU scaler takes frames on the GPU".into(),
            )),
        }
    }
}

pub struct HwEncoder {
    inner: Box<dyn EncoderBackend>,
}

impl HwEncoder {
    pub fn new(codec: Codec, config: &EncoderConfig) -> Result<Self> {
        let makers = encoders()
            .into_iter()
            .map(|(name, make)| (name, move || make(codec, config)))
            .collect();
        let inner = first(makers, "no GPU encoder on this system")?;
        Ok(Self { inner })
    }

    pub fn backend(&self) -> Backend {
        self.inner.backend()
    }

    pub fn name(&self) -> &str {
        self.inner.name()
    }

    pub fn b_frames(&self) -> u8 {
        self.inner.b_frames()
    }

    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        self.inner.encode(frame)
    }

    pub fn encode_gpu(&mut self, frame: &GpuFrame) -> Result<Vec<Packet>> {
        self.inner.encode_gpu(frame)
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        self.inner.flush()
    }

    pub fn extradata(&self) -> Option<&[u8]> {
        self.inner.extradata()
    }
}
