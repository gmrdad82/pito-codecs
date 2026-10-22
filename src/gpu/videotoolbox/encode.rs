use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock, PoisonError};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
};
use objc2_core_video::{
    CVColorPrimariesGetStringForIntegerCodePoint, CVImageBuffer, CVPixelBufferCreate,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVTransferFunctionGetStringForIntegerCodePoint,
    CVYCbCrMatrixGetStringForIntegerCodePoint, kCVImageBufferPixelAspectRatioHorizontalSpacingKey,
    kCVImageBufferPixelAspectRatioVerticalSpacingKey, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTPixelTransferSession, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ColorPrimaries,
    kVTCompressionPropertyKey_DataRateLimits, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_PixelAspectRatio,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_Quality,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_Baseline_AutoLevel, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_H264_Main_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTProfileLevel_HEVC_Main10_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use super::{Locked, NV12_FULL, NV12_VIDEO, P010_FULL, P010_VIDEO, codec_type, label, planes};
use crate::gpu::nal;
use crate::gpu::{EncoderConfig, GpuFrame, Profile};
use crate::media::{Bitstream, Rate};
use crate::media::{Codec, Error, Frame, Packet, PixelFormat, Result, TimeBase};

enum Output {
    Packet {
        data: Vec<u8>,
        pts: i64,
        keyframe: bool,
    },
    Failed(Error),
}

type Sets = [Vec<Vec<u8>>; 3];

struct Shared {
    codec: Codec,
    bitstream: Bitstream,
    sets: Mutex<Option<Sets>>,
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
    session: CFRetained<VTCompressionSession>,
    shared: Box<Shared>,
}

unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = self.session.complete_frames(invalid_time());
            self.session.invalidate();
        }
    }
}

fn invalid_time() -> CMTime {
    CMTime {
        value: 0,
        timescale: 0,
        flags: CMTimeFlags(0),
        epoch: 0,
    }
}

fn valid_time(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

fn ticks(t: CMTime, tb: TimeBase) -> Option<i64> {
    let flags = t.flags;
    let bad = CMTimeFlags::PositiveInfinity | CMTimeFlags::NegativeInfinity;
    if !flags.contains(CMTimeFlags::Valid) || flags.intersects(bad) || t.timescale <= 0 {
        return None;
    }
    let num = i128::from(t.value) * i128::from(tb.den);
    let den = i128::from(t.timescale) * i128::from(tb.num);
    if den == 0 {
        return None;
    }
    i64::try_from((2 * num + den).div_euclid(2 * den)).ok()
}

fn read_sets(sample: &CMSampleBuffer, codec: Codec) -> Result<Sets> {
    let format = unsafe { sample.format_description() }
        .ok_or_else(|| Error::Backend("compressed frame without a format description".into()))?;
    let mut sets: Sets = [Vec::new(), Vec::new(), Vec::new()];
    let (mut index, mut count) = (0usize, 1usize);
    while index < count && index < 32 {
        let (mut ptr, mut size, mut total, mut length): (*const u8, usize, usize, c_int) =
            (std::ptr::null(), 0, 0, 0);
        let status = unsafe {
            match codec {
                Codec::Hevc => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    &format,
                    index,
                    &mut ptr,
                    &mut size,
                    &mut total,
                    &mut length,
                ),
                _ => CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    &format,
                    index,
                    &mut ptr,
                    &mut size,
                    &mut total,
                    &mut length,
                ),
            }
        };
        if status != 0 {
            return Err(Error::Backend(format!(
                "cannot read the {codec:?} parameter sets ({status})"
            )));
        }
        if length != 4 {
            return Err(Error::Backend(format!("NAL length size {length}")));
        }
        if ptr.is_null() || size == 0 || size > 65_535 {
            return Err(Error::Backend("empty or oversized parameter set".into()));
        }
        count = total;
        let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        if let Some(i) = nal::set_slot(codec, &bytes) {
            sets[i].push(bytes);
        }
        index += 1;
    }
    if sets[1].is_empty() || sets[2].is_empty() || (codec == Codec::Hevc && sets[0].is_empty()) {
        return Err(Error::Backend(
            "the format description lacks parameter sets".into(),
        ));
    }
    Ok(sets)
}

fn to_start_codes(data: &mut [u8]) {
    let mut at = 0;
    while let Some(head) = data.get_mut(at..at + 4) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        head.copy_from_slice(&[0, 0, 0, 1]);
        at += 4 + len;
    }
}

fn read_packet(sample: &CMSampleBuffer, shared: &Shared, pts: i64) -> Result<Output> {
    let block = unsafe { sample.data_buffer() }
        .ok_or_else(|| Error::Backend("compressed frame without data".into()))?;
    let len = unsafe { block.data_length() };
    if len == 0 {
        return Err(Error::Backend("empty compressed frame".into()));
    }
    let mut data = vec![0u8; len];
    let dest = NonNull::from(&mut data[0]).cast::<c_void>();
    let status = unsafe { block.copy_data_bytes(0, len, dest) };
    if status != 0 {
        return Err(Error::Backend(format!(
            "cannot copy a compressed frame ({status})"
        )));
    }
    let codec = shared.codec;
    let mut guard = shared.sets.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(read_sets(sample, codec)?);
    }
    let sets = guard.as_ref().map(|s| s.as_slice()).unwrap_or_default();
    let (keyframe, inline) = {
        let nals = nal::length_prefixed(&data, 4)?;
        (
            nals.iter().any(|n| nal::is_keyframe(codec, n)),
            nals.iter().any(|n| nal::is_parameter_set(codec, n)),
        )
    };
    let annexb = shared.bitstream == Bitstream::AnnexB;
    let data = if !inline && !(annexb && keyframe) {
        if annexb {
            to_start_codes(&mut data);
        }
        data
    } else {
        let nals = nal::length_prefixed(&data, 4)?;
        let kept: Vec<&[u8]> = nals
            .into_iter()
            .filter(|n| !nal::is_parameter_set(codec, n))
            .collect();
        if annexb {
            let mut out = Vec::with_capacity(len + 256);
            let head = sets.iter().flatten().map(Vec::as_slice);
            nal::put_start_codes(head.chain(kept), &mut out);
            out
        } else {
            nal::length_prefix(&kept)
        }
    };
    Ok(Output::Packet {
        data,
        pts,
        keyframe,
    })
}

unsafe extern "C-unwind" fn output(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    if refcon.is_null() {
        return;
    }
    let shared = unsafe { &*(refcon as *const Shared) };
    let pts = frame_refcon.addr() as isize as i64;
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Output::Failed(Error::Backend(format!(
                "VideoToolbox encode error {status}"
            )));
        }
        if flags.contains(VTEncodeInfoFlags::FrameDropped) {
            return Output::Failed(Error::Backend("VideoToolbox dropped a frame".into()));
        }
        let Some(sample) = NonNull::new(sample) else {
            return Output::Failed(Error::Backend(
                "VideoToolbox returned no data for a frame".into(),
            ));
        };
        read_packet(unsafe { sample.as_ref() }, shared, pts).unwrap_or_else(Output::Failed)
    }));
    shared.push(result.unwrap_or_else(|_| {
        Output::Failed(Error::Backend(
            "panic while reading a compressed frame".into(),
        ))
    }));
}

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

fn optional_key(name: &CStr) -> Option<&'static CFString> {
    let any = std::ptr::without_provenance_mut::<c_void>(-2isize as usize);
    let symbol = unsafe { dlsym(any, name.as_ptr()) };
    let slot = NonNull::new(symbol)?.cast::<*const CFString>();
    unsafe { slot.read().as_ref() }
}

fn set(session: &VTCompressionSession, key: &CFString, value: &CFType) -> i32 {
    unsafe { VTSessionSetProperty(session.as_ref(), key, Some(value)) }
}

fn require(status: i32, what: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "VideoToolbox refused {what} ({status})"
        )))
    }
}

fn bits_per_second(kbps: u32) -> i32 {
    i32::try_from(u64::from(kbps) * 1000).unwrap_or(i32::MAX)
}

fn rate_limit(session: &VTCompressionSession, kbps: u32) {
    let key = unsafe { kVTCompressionPropertyKey_DataRateLimits };
    let bytes = CFNumber::new_i32(bits_per_second(kbps) / 8);
    let seconds = CFNumber::new_f64(1.0);
    let limits = CFArray::<CFType>::from_objects(&[bytes.as_ref(), seconds.as_ref()]);
    let _ = set(session, key, limits.as_ref());
}

fn estimate(codec: Codec, config: &EncoderConfig, quality: u8) -> i32 {
    let fps = f64::from(config.fps.0) / f64::from(config.fps.1);
    let pixels = f64::from(config.width) * f64::from(config.height) * fps;
    let scale = if codec == Codec::Hevc { 0.06 } else { 0.1 };
    let bps = pixels * scale * 2f64.powf((23.0 - f64::from(quality)) / 6.0);
    bps.clamp(100_000.0, f64::from(i32::MAX)) as i32
}

fn configure(
    session: &VTCompressionSession,
    codec: Codec,
    config: &EncoderConfig,
    profile: &CFString,
    reorder: bool,
) -> Result<()> {
    let key = unsafe { kVTCompressionPropertyKey_ProfileLevel };
    require(set(session, key, profile.as_ref()), "the profile")?;
    let key = unsafe { kVTCompressionPropertyKey_RealTime };
    let _ = set(session, key, CFBoolean::new(false).as_ref());
    let key = unsafe { kVTCompressionPropertyKey_AllowFrameReordering };
    require(
        set(session, key, CFBoolean::new(reorder).as_ref()),
        "frame reordering",
    )?;
    let key = unsafe { kVTCompressionPropertyKey_AllowOpenGOP };
    let _ = set(session, key, CFBoolean::new(false).as_ref());
    look(session, config);
    pace(session, codec, config)
}

fn look(session: &VTCompressionSession, config: &EncoderConfig) {
    let (h, v) = config.sar;
    if h != 0 && v != 0 {
        let keys: [&CFString; 2] = unsafe {
            [
                kCVImageBufferPixelAspectRatioHorizontalSpacingKey,
                kCVImageBufferPixelAspectRatioVerticalSpacingKey,
            ]
        };
        let (h, v) = (
            CFNumber::new_i32(i32::from(h)),
            CFNumber::new_i32(i32::from(v)),
        );
        let par = CFDictionary::<CFString, CFType>::from_slices(&keys, &[h.as_ref(), v.as_ref()]);
        let key = unsafe { kVTCompressionPropertyKey_PixelAspectRatio };
        let _ = set(session, key, par.as_ref());
    }
    let c = config.color;
    if !c.described() {
        return;
    }
    let tags = unsafe {
        [
            (
                kVTCompressionPropertyKey_ColorPrimaries,
                CVColorPrimariesGetStringForIntegerCodePoint(c_int::from(c.primaries)),
            ),
            (
                kVTCompressionPropertyKey_TransferFunction,
                CVTransferFunctionGetStringForIntegerCodePoint(c_int::from(c.transfer)),
            ),
            (
                kVTCompressionPropertyKey_YCbCrMatrix,
                CVYCbCrMatrixGetStringForIntegerCodePoint(c_int::from(c.matrix)),
            ),
        ]
    };
    for (key, value) in tags {
        if let Some(value) = value {
            let value: &CFString = &value;
            let _ = set(session, key, value.as_ref());
        }
    }
}

fn shift_past((value, scale): (i64, i32), first: i64, timescale: i32) -> i64 {
    let (scale, timescale) = (i128::from(scale.max(1)), i128::from(timescale));
    let at = (i128::from(value) * timescale + scale - 1).div_euclid(scale);
    let shift = (at - i128::from(first)).max(0);
    i64::try_from(shift).unwrap_or(i64::MAX / 2)
}

fn full_range(pixel_format: u32, full: bool) -> u32 {
    match (pixel_format, full) {
        (NV12_VIDEO, true) => NV12_FULL,
        (P010_VIDEO, true) => P010_FULL,
        (pf, _) => pf,
    }
}

fn pace(session: &VTCompressionSession, codec: Codec, config: &EncoderConfig) -> Result<()> {
    let gop = match config.gop {
        0 => (2 * u64::from(config.fps.0)).div_ceil(u64::from(config.fps.1)),
        n => u64::from(n),
    };
    let gop = gop.clamp(1, i32::MAX as u64) as i32;
    let key = unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval };
    require(
        set(session, key, CFNumber::new_i32(gop).as_ref()),
        "the keyframe interval",
    )?;
    let fps = f64::from(config.fps.0) / f64::from(config.fps.1);
    let key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
    let _ = set(session, key, CFNumber::new_f64(fps).as_ref());
    let average = unsafe { kVTCompressionPropertyKey_AverageBitRate };
    match config.rate {
        Rate::Crf(q) | Rate::Qp(q) => {
            let quality = (f64::from(51 - q.min(51)) / 51.0).sqrt();
            let key = unsafe { kVTCompressionPropertyKey_Quality };
            if set(session, key, CFNumber::new_f64(quality).as_ref()) != 0 {
                let bps = CFNumber::new_i32(estimate(codec, config, q));
                require(set(session, average, bps.as_ref()), "the bitrate")?;
            }
            if let Rate::Qp(q) = config.rate {
                let qp = CFNumber::new_i32(i32::from(q.min(51)));
                for name in [
                    c"kVTCompressionPropertyKey_MinAllowedFrameQP",
                    c"kVTCompressionPropertyKey_MaxAllowedFrameQP",
                ] {
                    if let Some(key) = optional_key(name) {
                        let _ = set(session, key, qp.as_ref());
                    }
                }
            }
        }
        Rate::Bitrate(kbps) => {
            let bps = CFNumber::new_i32(bits_per_second(kbps));
            require(set(session, average, bps.as_ref()), "the bitrate")?;
            rate_limit(session, kbps);
        }
        Rate::Vbr { kbps, max_kbps } => {
            let bps = CFNumber::new_i32(bits_per_second(kbps));
            require(set(session, average, bps.as_ref()), "the bitrate")?;
            rate_limit(session, max_kbps.max(kbps));
        }
    }
    Ok(())
}

pub fn hardware(codec: Codec) -> bool {
    static PROBE: OnceLock<(bool, bool)> = OnceLock::new();
    let (h264, hevc) = *PROBE.get_or_init(|| {
        let config = EncoderConfig {
            width: 1280,
            height: 720,
            ..EncoderConfig::default()
        };
        let probe = |c: Codec| VtEncoder::new(c, &config).is_ok();
        (probe(Codec::H264), probe(Codec::Hevc))
    });
    match codec {
        Codec::H264 => h264,
        Codec::Hevc => hevc,
        Codec::Aac | Codec::Vp9 | Codec::Av1 | Codec::Opus | Codec::Mp3 => false,
        Codec::Ac3 | Codec::Eac3 => false,
    }
}

pub struct VtEncoder {
    session: Session,
    codec: Codec,
    width: u32,
    height: u32,
    format: PixelFormat,
    pixel_format: u32,
    b_frames: u8,
    duration: CMTime,
    pool: Option<CFRetained<CVPixelBufferPool>>,
    attrs: CFRetained<CFDictionary<CFString, CFType>>,
    extradata: Option<Vec<u8>>,
    time_base: Option<TimeBase>,
    queued: VecDeque<i64>,
    scaler: Option<Scaler>,
    submitted: u64,
    asked: EncoderConfig,
    key: bool,
    shift: i64,
    end: Option<(i64, i32)>,
    floor: Option<(i64, i32)>,
}

struct Scaler {
    session: CFRetained<VTPixelTransferSession>,
    source: CFRetained<CVImageBuffer>,
    size: (u32, u32),
}

impl Scaler {
    fn new(size: (u32, u32), pixel_format: u32) -> Result<Self> {
        let mut raw: *mut VTPixelTransferSession = std::ptr::null_mut();
        let status = unsafe { VTPixelTransferSession::create(None, NonNull::from(&mut raw)) };
        let session = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| {
            Error::Backend(format!("cannot open a VideoToolbox scaler ({status})"))
        })?;
        let session = unsafe { CFRetained::from_raw(session) };
        let pf = CFNumber::new_i32(pixel_format as i32);
        let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let keys: [&CFString; 2] = unsafe {
            [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferIOSurfacePropertiesKey,
            ]
        };
        let attrs =
            CFDictionary::<CFString, CFType>::from_slices(&keys, &[pf.as_ref(), io.as_ref()]);
        let mut raw: *mut CVImageBuffer = std::ptr::null_mut();
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                size.0 as usize,
                size.1 as usize,
                pixel_format,
                Some(attrs.as_ref()),
                NonNull::from(&mut raw),
            )
        };
        let source = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| {
            Error::Backend(format!("cannot allocate a scaler input buffer ({status})"))
        })?;
        Ok(Self {
            session,
            source: unsafe { CFRetained::from_raw(source) },
            size,
        })
    }
}

impl Drop for Scaler {
    fn drop(&mut self) {
        unsafe { self.session.invalidate() };
    }
}

unsafe impl Send for VtEncoder {}

impl VtEncoder {
    pub fn new(codec: Codec, config: &EncoderConfig) -> Result<Self> {
        if codec == Codec::Av1 {
            return Err(Error::Unsupported(
                "AV1 encode isn't offered on VideoToolbox".into(),
            ));
        }
        let codec_type = codec_type(codec)?;
        let (w, h) = (config.width, config.height);
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(Error::Invalid(format!("encoder picture size {w}x{h}")));
        }
        if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            return Err(Error::Unsupported(format!(
                "odd picture size {w}x{h} on VideoToolbox"
            )));
        }
        let (num, den) = config.fps;
        if num == 0 || den == 0 || num > i32::MAX as u32 {
            return Err(Error::Invalid("encoder frame rate must be set".into()));
        }
        let (profile, format, pixel_format) = unsafe {
            match (codec, config.profile) {
                (Codec::H264, Profile::Baseline) => (
                    kVTProfileLevel_H264_Baseline_AutoLevel,
                    PixelFormat::Yuv420p8,
                    NV12_VIDEO,
                ),
                (Codec::H264, Profile::Main) => (
                    kVTProfileLevel_H264_Main_AutoLevel,
                    PixelFormat::Yuv420p8,
                    NV12_VIDEO,
                ),
                (Codec::H264, Profile::High) => (
                    kVTProfileLevel_H264_High_AutoLevel,
                    PixelFormat::Yuv420p8,
                    NV12_VIDEO,
                ),
                (Codec::Hevc, Profile::Main10) => (
                    kVTProfileLevel_HEVC_Main10_AutoLevel,
                    PixelFormat::Yuv420p10,
                    P010_VIDEO,
                ),
                (Codec::Hevc, _) => (
                    kVTProfileLevel_HEVC_Main_AutoLevel,
                    PixelFormat::Yuv420p8,
                    NV12_VIDEO,
                ),
                (c, p) => {
                    return Err(Error::Unsupported(format!("{c:?} {p:?} on VideoToolbox")));
                }
            }
        };
        let pixel_format = full_range(pixel_format, config.color.full_range);
        let reorder = config.b_frames > 0 && config.profile != Profile::Baseline;
        let shared = Box::new(Shared {
            codec,
            bitstream: config.bitstream,
            sets: Mutex::new(None),
            out: Mutex::new(Vec::new()),
        });
        let require_key: &CFString =
            unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        let spec = CFDictionary::<CFString, CFType>::from_slices(
            &[require_key],
            &[CFBoolean::new(true).as_ref()],
        );
        let pf = CFNumber::new_i32(pixel_format as i32);
        let (cw, ch) = (CFNumber::new_i32(w as i32), CFNumber::new_i32(h as i32));
        let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let keys: [&CFString; 4] = unsafe {
            [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferIOSurfacePropertiesKey,
            ]
        };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &keys,
            &[pf.as_ref(), cw.as_ref(), ch.as_ref(), io.as_ref()],
        );
        let mut out: *mut VTCompressionSession = std::ptr::null_mut();
        let status = unsafe {
            VTCompressionSession::create(
                None,
                w as i32,
                h as i32,
                codec_type,
                Some(spec.as_ref()),
                Some(attrs.as_ref()),
                None,
                Some(output),
                &*shared as *const Shared as *mut c_void,
                NonNull::from(&mut out),
            )
        };
        let session = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| {
            Error::Unsupported(format!(
                "this Mac has no hardware {codec:?} encoder for {w}x{h} ({status})"
            ))
        })?;
        let session = Session {
            session: unsafe { CFRetained::from_raw(session) },
            shared,
        };
        configure(&session.session, codec, config, profile, reorder)?;
        require(
            unsafe { session.session.prepare_to_encode_frames() },
            "to prepare the encoder",
        )?;
        let pool = unsafe { session.session.pixel_buffer_pool() };
        Ok(Self {
            session,
            codec,
            width: w,
            height: h,
            format,
            pixel_format,
            b_frames: if reorder { config.b_frames } else { 0 },
            duration: valid_time(i64::from(den), num as i32),
            pool,
            attrs,
            extradata: None,
            time_base: None,
            queued: VecDeque::new(),
            scaler: None,
            submitted: 0,
            asked: config.clone(),
            key: false,
            shift: 0,
            end: None,
            floor: None,
        })
    }

    pub fn restart(&mut self, config: &EncoderConfig) -> Result<()> {
        let same_rate =
            std::mem::discriminant(&config.rate) == std::mem::discriminant(&self.asked.rate);
        if !crate::gpu::restartable(&self.asked, config) || !same_rate {
            return Err(Error::Unsupported(
                "a restart changes only the rate and the GOP, within the same rate control".into(),
            ));
        }
        if !self.queued.is_empty() {
            return Err(Error::Invalid(
                "the encoder restarts only after a flush".into(),
            ));
        }
        pace(&self.session.session, self.codec, config)?;
        self.asked.rate = config.rate;
        self.asked.gop = config.gop;
        self.key = true;
        self.floor = self.end;
        self.time_base = None;
        Ok(())
    }

    pub fn name(&self) -> &str {
        label(self.codec)
    }

    pub fn b_frames(&self) -> u8 {
        self.b_frames
    }

    pub fn extradata(&self) -> Option<&[u8]> {
        self.extradata.as_deref()
    }

    fn buffer(&self) -> Result<CFRetained<CVImageBuffer>> {
        let mut raw: *mut CVImageBuffer = std::ptr::null_mut();
        let status = match &self.pool {
            Some(pool) => unsafe {
                CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut raw))
            },
            None => unsafe {
                CVPixelBufferCreate(
                    None,
                    self.width as usize,
                    self.height as usize,
                    self.pixel_format,
                    Some(self.attrs.as_ref()),
                    NonNull::from(&mut raw),
                )
            },
        };
        let raw = NonNull::new(raw).filter(|_| status == 0).ok_or_else(|| {
            Error::Backend(format!(
                "cannot allocate an encoder input buffer ({status})"
            ))
        })?;
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        if frame.format != self.format || frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(format!(
                "the encoder takes {}x{} {:?} frames, not {}x{} {:?}",
                self.width, self.height, self.format, frame.width, frame.height, frame.format
            )));
        }
        self.submit(frame)
    }

    pub fn encode_gpu(&mut self, frame: &GpuFrame) -> Result<Vec<Packet>> {
        let frame = frame.download()?;
        if frame.format != self.format {
            return Err(Error::Invalid(format!(
                "the encoder takes {:?} frames, not {:?}",
                self.format, frame.format
            )));
        }
        self.submit(&frame)
    }

    fn scale(&mut self, frame: &Frame, target: &CVImageBuffer) -> Result<()> {
        let size = (frame.width, frame.height);
        if self.scaler.as_ref().is_none_or(|s| s.size != size) {
            self.scaler = Some(Scaler::new(size, self.pixel_format)?);
        }
        let Some(scaler) = self.scaler.as_ref() else {
            return Err(Error::Backend("no VideoToolbox scaler".into()));
        };
        {
            let mut lock = Locked::new(&scaler.source, CVPixelBufferLockFlags(0))?;
            let (luma, chroma) = lock.planes_mut()?;
            planes::from_planar(frame, luma, chroma).map_err(Error::Backend)?;
        }
        let status = unsafe { scaler.session.transfer_image(&scaler.source, target) };
        if status != 0 {
            return Err(Error::Backend(format!(
                "VideoToolbox could not scale {}x{} to {}x{} ({status})",
                size.0, size.1, self.width, self.height
            )));
        }
        Ok(())
    }

    fn submit(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        let tb = *self.time_base.get_or_insert(frame.time_base);
        let timescale = i32::try_from(tb.den)
            .ok()
            .filter(|&d| d > 0 && tb.num > 0)
            .ok_or_else(|| Error::Invalid(format!("time base {}/{}", tb.num, tb.den)))?;
        let value = frame
            .pts
            .checked_mul(i64::from(tb.num))
            .ok_or_else(|| Error::Invalid(format!("pts {} out of range", frame.pts)))?;
        if let Some(floor) = self.floor.take() {
            self.shift = shift_past(floor, value, timescale);
        }
        let value = value
            .checked_add(self.shift)
            .ok_or_else(|| Error::Invalid(format!("pts {} out of range", frame.pts)))?;
        let pts = valid_time(value, timescale);
        let buffer = self.buffer()?;
        if (frame.width, frame.height) == (self.width, self.height) {
            let mut lock = Locked::new(&buffer, CVPixelBufferLockFlags(0))?;
            let (luma, chroma) = lock.planes_mut()?;
            planes::from_planar(frame, luma, chroma).map_err(Error::Backend)?;
        } else {
            self.scale(frame, &buffer)?;
        }
        let refcon = std::ptr::without_provenance_mut::<c_void>(frame.pts as isize as usize);
        let force = self.key.then(|| {
            let key: &CFString = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()])
        });
        let status = unsafe {
            self.session.session.encode_frame(
                &buffer,
                pts,
                self.duration,
                force.as_deref().map(|d| d.as_opaque()),
                refcon,
                std::ptr::null_mut(),
            )
        };
        if status != 0 {
            return Err(Error::Backend(format!(
                "VTCompressionSessionEncodeFrame failed ({status})"
            )));
        }
        self.key = false;
        let step = ticks(self.duration, tb).unwrap_or(1).max(1) * i64::from(tb.num);
        self.end = Some((value.saturating_add(step), timescale));
        self.queued.push_back(frame.pts);
        self.submitted += 1;
        if self.submitted == 1 {
            let status = unsafe { self.session.session.complete_frames(pts) };
            if status != 0 {
                return Err(Error::Backend(format!(
                    "VideoToolbox could not finish the first frame ({status})"
                )));
            }
        }
        self.drain()
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        let status = unsafe { self.session.session.complete_frames(invalid_time()) };
        if status != 0 {
            return Err(Error::Backend(format!(
                "VideoToolbox could not finish the pending frames ({status})"
            )));
        }
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        let shared = &self.session.shared;
        let outputs =
            std::mem::take(&mut *shared.out.lock().unwrap_or_else(PoisonError::into_inner));
        if outputs.is_empty() {
            return Ok(Vec::new());
        }
        if self.extradata.is_none() {
            let sets = shared.sets.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(sets) = sets.as_ref() {
                self.extradata = Some(nal::record(self.codec, sets)?);
            }
        }
        let tb = self.time_base.unwrap_or(TimeBase::new(1, 90000));
        let delay = ticks(self.duration, tb).unwrap_or(1).max(1) * i64::from(self.b_frames);
        let mut packets = Vec::with_capacity(outputs.len());
        for o in outputs {
            let (data, pts, keyframe) = match o {
                Output::Packet {
                    data,
                    pts,
                    keyframe,
                } => (data, pts, keyframe),
                Output::Failed(e) => return Err(e),
            };
            let base = self.queued.pop_front().unwrap_or(pts);
            let dts = (base - delay).min(pts);
            packets.push(Packet {
                data,
                pts,
                dts,
                keyframe,
            });
        }
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restarted_session_starts_past_the_last_frame_it_encoded() {
        assert_eq!(shift_past((3_000, 90_000), 0, 90_000), 3_000);
        assert_eq!(shift_past((3_000, 90_000), 1_000, 90_000), 2_000);
        assert_eq!(shift_past((3_000, 90_000), 9_000, 90_000), 0);
        assert_eq!(shift_past((1_001, 30_000), 0, 90_000), 3_003);
        assert_eq!(shift_past((1, 3), 0, 2), 1);
        assert_eq!(shift_past((10, 0), 0, 90_000), 900_000);
    }

    #[test]
    fn a_full_range_source_picks_the_full_range_pixel_format() {
        assert_eq!(full_range(NV12_VIDEO, false), NV12_VIDEO);
        assert_eq!(full_range(NV12_VIDEO, true), NV12_FULL);
        assert_eq!(full_range(P010_VIDEO, true), P010_FULL);
        assert_eq!(full_range(P010_VIDEO, false), P010_VIDEO);
    }

    #[test]
    fn the_codec_points_name_videotoolbox_colour_strings() {
        let s = |v: Option<CFRetained<CFString>>| v.map(|s| s.to_string());
        let want = |k: &CFString| Some(k.to_string());
        unsafe {
            assert_eq!(
                s(CVColorPrimariesGetStringForIntegerCodePoint(1)),
                want(objc2_core_video::kCVImageBufferColorPrimaries_ITU_R_709_2)
            );
            assert_eq!(
                s(CVTransferFunctionGetStringForIntegerCodePoint(16)),
                want(objc2_core_video::kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ)
            );
            assert_eq!(
                s(CVYCbCrMatrixGetStringForIntegerCodePoint(9)),
                want(objc2_core_video::kCVImageBufferYCbCrMatrix_ITU_R_2020)
            );
        }
    }
}
