use std::borrow::Cow;
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferLockFlags, kCVPixelBufferPixelFormatTypeKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTIsHardwareDecodeSupported,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

use super::order::{Key, Order};
use super::{Locked, NV12_FULL, NV12_VIDEO, P010_FULL, P010_VIDEO, codec_type, label, planes};
use crate::gpu::nal::{self, Framing};
use crate::gpu::syntax::bits::unescape_rbsp;
use crate::gpu::syntax::{h264 as h264_syntax, hevc as hevc_syntax};
use crate::media::{Codec, Error, Frame, Packet, PixelFormat, Result};

const IN_FLIGHT: usize = 4;

#[derive(Clone)]
struct Layout {
    pixel_format: u32,
    format: PixelFormat,
    crop: (u32, u32, u32, u32),
    reorder: usize,
}

enum Output {
    Frame(Frame),
    Dropped(i64),
    Failed(Error),
}

struct Shared {
    layout: Layout,
    out: Mutex<Vec<Output>>,
}

impl Shared {
    fn push(&self, o: Output) {
        self.out
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(o);
    }
}

struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    shared: Box<Shared>,
}

unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
        }
    }
}

unsafe extern "C-unwind" fn output(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    if refcon.is_null() {
        return;
    }
    let shared = unsafe { &*(refcon as *const Shared) };
    let pts = frame_refcon.addr() as isize as i64;
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Output::Failed(Error::Backend(format!(
                "VideoToolbox decode error {status}"
            )));
        }
        let Some(image) = NonNull::new(image) else {
            return Output::Dropped(pts);
        };
        match copy_out(unsafe { image.as_ref() }, &shared.layout, pts) {
            Ok(f) => Output::Frame(f),
            Err(e) => Output::Failed(e),
        }
    }));
    shared.push(result.unwrap_or_else(|_| {
        Output::Failed(Error::Backend(
            "panic while reading a decoded picture".into(),
        ))
    }));
}

fn copy_out(pb: &CVImageBuffer, l: &Layout, pts: i64) -> Result<Frame> {
    let fmt = CVPixelBufferGetPixelFormatType(pb);
    if fmt != l.pixel_format {
        return Err(Error::Backend(format!(
            "VideoToolbox decoded to pixel format {fmt:#x}"
        )));
    }
    let (bw, bh) = (
        CVPixelBufferGetWidth(pb) as u32,
        CVPixelBufferGetHeight(pb) as u32,
    );
    let (cx, cy, w, h) = l.crop;
    let origin = if (bw, bh) == (w, h) {
        (0, 0)
    } else if cx + w <= bw && cy + h <= bh {
        (cx as usize, cy as usize)
    } else {
        return Err(Error::Backend(format!(
            "decoded picture is {bw}x{bh}, expected {w}x{h}"
        )));
    };
    let lock = Locked::new(pb, CVPixelBufferLockFlags::ReadOnly)?;
    let (luma, chroma) = lock.planes()?;
    let size = (w as usize, h as usize);
    planes::to_planar(l.format, luma, chroma, origin, size, pts).map_err(Error::Backend)
}

fn layout(codec: Codec, sets: &[Vec<u8>]) -> Result<Layout> {
    let find = |ty: u8| {
        sets.iter()
            .find(|n| nal::nal_type(codec, n) == ty)
            .ok_or_else(|| Error::Invalid(format!("no {codec:?} SPS")))
    };
    let (idc, luma, chroma, full, crop, reorder) = match codec {
        Codec::H264 => {
            let sps = find(7)?;
            let rbsp = unescape_rbsp(sps.get(1..).unwrap_or_default());
            let sps = h264_syntax::params::Sps::parse(&rbsp)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            if !sps.frame_mbs_only {
                return Err(Error::Unsupported(
                    "interlaced H.264 on VideoToolbox".into(),
                ));
            }
            (
                sps.chroma_format_idc,
                sps.bit_depth_luma,
                sps.bit_depth_chroma,
                sps.vui.as_ref().is_some_and(|v| v.full_range),
                sps.crop_rect(),
                sps.max_num_reorder_frames(),
            )
        }
        Codec::Hevc => {
            let sps = find(33)?;
            let rbsp = unescape_rbsp(sps.get(2..).unwrap_or_default());
            let sps = hevc_syntax::params::Sps::parse(&rbsp)
                .map_err(|e| Error::Invalid(e.to_string()))?;
            (
                sps.chroma_format_idc,
                sps.bit_depth_luma,
                sps.bit_depth_chroma,
                sps.vui.as_ref().is_some_and(|v| v.full_range),
                sps.crop_rect(),
                sps.max_num_reorder as usize,
            )
        }
        Codec::Aac | Codec::Vp9 | Codec::Opus => {
            return Err(Error::Unsupported(format!("{codec:?} on VideoToolbox")));
        }
    };
    let (format, pixel_format) = match (idc, luma, chroma, full) {
        (1, 8, 8, false) => (PixelFormat::Yuv420p8, NV12_VIDEO),
        (1, 8, 8, true) => (PixelFormat::Yuv420p8, NV12_FULL),
        (1, 10, 10, false) => (PixelFormat::Yuv420p10, P010_VIDEO),
        (1, 10, 10, true) => (PixelFormat::Yuv420p10, P010_FULL),
        _ => {
            return Err(Error::Unsupported(format!(
                "{codec:?} chroma format {idc} at {luma}-bit on VideoToolbox"
            )));
        }
    };
    let (_, _, w, h) = crop;
    if w == 0 || h == 0 {
        return Err(Error::Invalid(format!("{codec:?} picture size {w}x{h}")));
    }
    Ok(Layout {
        pixel_format,
        format,
        crop,
        reorder,
    })
}

fn format_description(codec: Codec, sets: &[Vec<u8>]) -> Result<CFRetained<CMFormatDescription>> {
    let mut ptrs: Vec<NonNull<u8>> = Vec::with_capacity(sets.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(sets.len());
    for p in sets.iter().filter(|p| !p.is_empty()) {
        ptrs.push(NonNull::from(&p[0]));
        sizes.push(p.len());
    }
    let no_sets = || Error::Invalid("no parameter sets".into());
    let ptr_array = NonNull::new(ptrs.as_mut_ptr()).ok_or_else(no_sets)?;
    let size_array = NonNull::new(sizes.as_mut_ptr()).ok_or_else(no_sets)?;
    let mut out: *const CMFormatDescription = std::ptr::null();
    let status = unsafe {
        match codec {
            Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                None,
                ptrs.len(),
                ptr_array,
                size_array,
                4,
                None,
                NonNull::from(&mut out),
            ),
            _ => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                None,
                ptrs.len(),
                ptr_array,
                size_array,
                4,
                NonNull::from(&mut out),
            ),
        }
    };
    let fd = NonNull::new(out as *mut CMFormatDescription)
        .filter(|_| status == 0)
        .ok_or_else(|| {
            Error::Invalid(format!(
                "VideoToolbox refused the parameter sets ({status})"
            ))
        })?;
    Ok(unsafe { CFRetained::from_raw(fd) })
}

fn sample_buffer(
    sample: &[u8],
    pts: i64,
    format: &CMFormatDescription,
) -> Result<CFRetained<CMSampleBuffer>> {
    let fail = |what: &str, status: i32| Error::Backend(format!("{what} failed ({status})"));
    let src = NonNull::new(sample.as_ptr() as *mut c_void)
        .filter(|_| !sample.is_empty())
        .ok_or_else(|| Error::Invalid("empty sample".into()))?;
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            sample.len(),
            None,
            std::ptr::null(),
            0,
            sample.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    let block = NonNull::new(block)
        .filter(|_| status == 0)
        .ok_or_else(|| fail("CMBlockBufferCreateWithMemoryBlock", status))?;
    let block = unsafe { CFRetained::from_raw(block) };
    let status = unsafe { CMBlockBuffer::replace_data_bytes(src, &block, 0, sample.len()) };
    if status != 0 {
        return Err(fail("CMBlockBufferReplaceDataBytes", status));
    }
    let invalid = CMTime {
        value: 0,
        timescale: 0,
        flags: CMTimeFlags(0),
        epoch: 0,
    };
    let timing = CMSampleTimingInfo {
        duration: invalid,
        presentationTimeStamp: CMTime {
            value: pts,
            timescale: 1,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        },
        decodeTimeStamp: invalid,
    };
    let size = sample.len();
    let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
    let status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block),
            Some(format),
            1,
            1,
            &timing,
            1,
            &size,
            NonNull::from(&mut out),
        )
    };
    let out = NonNull::new(out)
        .filter(|_| status == 0)
        .ok_or_else(|| fail("CMSampleBufferCreateReady", status))?;
    Ok(unsafe { CFRetained::from_raw(out) })
}

impl Session {
    fn new(codec: Codec, sets: &[Vec<u8>], layout: &Layout) -> Result<Session> {
        let format = format_description(codec, sets)?;
        let key: &CFString =
            unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder };
        let spec =
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()]);
        let pf = CFNumber::new_i32(layout.pixel_format as i32);
        let pf_key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&[pf_key], &[pf.as_ref()]);
        let shared = Box::new(Shared {
            layout: layout.clone(),
            out: Mutex::new(Vec::new()),
        });
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output),
            decompressionOutputRefCon: &*shared as *const Shared as *mut c_void,
        };
        let mut out: *mut VTDecompressionSession = std::ptr::null_mut();
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_ref()),
                Some(attrs.as_ref()),
                &record,
                NonNull::from(&mut out),
            )
        };
        let session = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| {
            Error::Unsupported(format!(
                "VideoToolbox has no hardware decoder for this {codec:?} stream ({status})"
            ))
        })?;
        Ok(Session {
            session: unsafe { CFRetained::from_raw(session) },
            format,
            shared,
        })
    }

    fn submit(&self, sample: &[u8], pts: i64) -> Result<()> {
        let buffer = sample_buffer(sample, pts, &self.format)?;
        let mut info = VTDecodeInfoFlags(0);
        let status = unsafe {
            self.session.decode_frame(
                &buffer,
                VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression,
                std::ptr::without_provenance_mut(pts as isize as usize),
                &mut info,
            )
        };
        if status != 0 {
            return Err(Error::Backend(format!(
                "VTDecompressionSessionDecodeFrame failed ({status})"
            )));
        }
        Ok(())
    }

    fn wait(&self) {
        unsafe { self.session.wait_for_asynchronous_frames() };
    }

    fn take(&self) -> Vec<Output> {
        std::mem::take(
            &mut *self
                .shared
                .out
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

pub fn hardware(codec: Codec) -> bool {
    codec_type(codec).is_ok_and(|t| unsafe { VTIsHardwareDecodeSupported(t) })
}

fn has_picture(codec: Codec, nal: &[u8]) -> bool {
    let t = nal::nal_type(codec, nal);
    match codec {
        Codec::Hevc => t < 32,
        _ => (1..=5).contains(&t),
    }
}

pub struct VtDecoder {
    codec: Codec,
    framing: Framing,
    sets: Vec<Vec<u8>>,
    layout: Option<Layout>,
    session: Option<Session>,
    order: Order,
    seq: i64,
    pending: Vec<(Key, Frame)>,
    in_flight: Vec<(i64, i64, Key)>,
    skip_rasl: bool,
    first: bool,
}

impl VtDecoder {
    pub fn new(codec: Codec, extradata: &[u8]) -> Result<Self> {
        codec_type(codec)?;
        if !hardware(codec) {
            return Err(Error::Unsupported(format!(
                "this Mac has no hardware {codec:?} decoder"
            )));
        }
        let config = nal::parse_extradata(codec, extradata)?;
        let mut d = Self {
            codec,
            framing: config.framing,
            sets: Vec::new(),
            layout: None,
            session: None,
            order: Order::new(),
            seq: 0,
            pending: Vec::new(),
            in_flight: Vec::new(),
            skip_rasl: false,
            first: true,
        };
        if !config.parameter_sets.is_empty() {
            d.open(config.parameter_sets)?;
        }
        Ok(d)
    }

    fn open(&mut self, sets: Vec<Vec<u8>>) -> Result<()> {
        let layout = layout(self.codec, &sets)?;
        let nals: Vec<&[u8]> = sets.iter().map(Vec::as_slice).collect();
        self.order.key(self.codec, &nals)?;
        self.session = Some(Session::new(self.codec, &sets, &layout)?);
        self.layout = Some(layout);
        self.sets = sets;
        Ok(())
    }

    pub fn name(&self) -> &str {
        label(self.codec)
    }

    fn skip(&mut self, nals: &[&[u8]]) -> bool {
        if self.codec != Codec::Hevc {
            return false;
        }
        let mut types = nals.iter().map(|n| nal::nal_type(Codec::Hevc, n));
        if types.clone().any(|t| (16..=23).contains(&t)) {
            self.skip_rasl = self.first && types.any(|t| matches!(t, 16..=18 | 21));
            false
        } else {
            self.skip_rasl && types.any(|t| matches!(t, 8 | 9))
        }
    }

    fn insert(&mut self, key: Key, f: Frame) {
        let at = self.pending.partition_point(|(k, _)| *k <= key);
        self.pending.insert(at, (key, f));
    }

    fn bump(&mut self) -> Vec<Frame> {
        let reorder = self.layout.as_ref().map_or(0, |l| l.reorder);
        let mut out = Vec::new();
        while self.pending.len() + self.in_flight.len() > reorder {
            let first = self.pending.first().map(|(k, _)| *k);
            match (first, self.in_flight.iter().map(|&(_, _, k)| k).min()) {
                (Some(p), Some(q)) if q < p => break,
                (Some(_), _) => out.push(self.pending.remove(0).1),
                (None, _) => break,
            }
        }
        out
    }

    fn collect(&mut self) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        for o in session.take() {
            let seq = match &o {
                Output::Frame(f) => f.pts,
                Output::Dropped(seq) => *seq,
                Output::Failed(_) => -1,
            };
            let entry = self
                .in_flight
                .iter()
                .position(|&(s, _, _)| s == seq)
                .map(|i| self.in_flight.swap_remove(i));
            match (o, entry) {
                (Output::Failed(e), _) => return Err(e),
                (Output::Frame(mut f), Some((_, pts, key))) => {
                    f.pts = pts;
                    self.insert(key, f);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        let nals = self.framing.split(&packet.data)?;
        if self.layout.is_none() {
            let sets: Vec<Vec<u8>> = nals
                .iter()
                .filter(|n| nal::is_parameter_set(self.codec, n))
                .map(|n| n.to_vec())
                .collect();
            if sets.is_empty() {
                return Ok(Vec::new());
            }
            self.open(sets)?;
        }
        let key = self.order.key(self.codec, &nals)?;
        let skip = self.skip(&nals);
        self.first = false;
        if skip || !nals.iter().any(|n| has_picture(self.codec, n)) {
            return Ok(Vec::new());
        }
        let sample = match self.framing {
            Framing::Length(4) => Cow::Borrowed(packet.data.as_slice()),
            _ => Cow::Owned(nal::length_prefix(&nals)),
        };
        if self.session.is_none() {
            let layout = self
                .layout
                .as_ref()
                .ok_or_else(|| Error::Backend("no VideoToolbox stream layout".into()))?;
            self.session = Some(Session::new(self.codec, &self.sets, layout)?);
        }
        let Some(session) = self.session.as_ref() else {
            return Err(Error::Backend("no VideoToolbox session".into()));
        };
        let key = key.ok_or_else(|| Error::Invalid("a picture without its first slice".into()))?;
        let seq = self.seq;
        self.seq += 1;
        session.submit(&sample, seq)?;
        self.in_flight.push((seq, packet.pts, key));
        if self.in_flight.len() > IN_FLIGHT {
            session.wait();
        }
        self.collect()?;
        Ok(self.bump())
    }

    pub fn flush(&mut self) -> Result<Vec<Frame>> {
        if let Some(s) = self.session.as_ref() {
            s.wait();
        }
        let collected = self.collect();
        self.in_flight.clear();
        let frames = std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(_, f)| f)
            .collect();
        collected.map(|_| frames)
    }

    pub fn reset(&mut self) {
        self.session = None;
        self.order.reset();
        self.pending.clear();
        self.in_flight.clear();
        self.skip_rasl = false;
        self.first = true;
    }
}
