use std::collections::VecDeque;

use super::ffi::{self, VAPictureH264, VAPictureHEVC};
use super::headers::{self, Packed};
use super::va::{EncodeSession, EncodeSetup, GpuSurface};
use crate::gpu::nal;
use crate::gpu::{Bitstream, EncoderConfig, Profile, Rate};
use crate::media::{Codec, Error, Frame, Packet, Result, TimeBase};

const DEPTH: usize = 4;
const REFERENCES: usize = 4;
const PIC_INIT_QP: i32 = 26;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Idr,
    P,
    B,
}

struct Waiting {
    slot: usize,
    pts: i64,
    order: u64,
}

struct InFlight {
    slot: usize,
    pts: i64,
    key: bool,
}

struct Blocks {
    log2_ctb: u32,
    log2_min_cb: u32,
    log2_min_tb: u32,
    log2_max_tb: u32,
    depth_inter: u32,
    depth_intra: u32,
}

struct HevcTools {
    amp: bool,
    sao: bool,
    strong_intra: bool,
    temporal_mvp: bool,
}

pub struct VaEncoder {
    codec: Codec,
    config: Settings,
    session: EncodeSession,
    size: (u32, u32),
    bits: u32,
    level: u8,
    blocks: Blocks,
    tools: HevcTools,
    free: Vec<usize>,
    waiting: Vec<Waiting>,
    in_flight: VecDeque<InFlight>,
    display_pts: VecDeque<(u64, i64)>,
    coded: u64,
    received: u64,
    gop_start: u64,
    refs_since_idr: u32,
    idr_id: u16,
    anchors: [Option<(usize, u64, u32)>; 2],
    duration: i64,
    extradata: Option<Vec<u8>>,
    sets: [Vec<Vec<u8>>; 3],
    name: String,
}

#[derive(Clone)]
struct Settings {
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    profile: Profile,
    rate: Rate,
    gop: u32,
    b_frames: u32,
    bitstream: Bitstream,
}

fn rate_control(rate: Rate) -> u32 {
    match rate {
        Rate::Crf(_) | Rate::Qp(_) => ffi::VA_RC_CQP,
        Rate::Bitrate(_) => ffi::VA_RC_CBR,
        Rate::Vbr { .. } => ffi::VA_RC_VBR,
    }
}

fn constant_qp(rate: Rate) -> bool {
    matches!(rate, Rate::Crf(_) | Rate::Qp(_))
}

fn h264_level(width: u32, height: u32, fps: f64) -> u8 {
    let mbs = (width.div_ceil(16) * height.div_ceil(16)) as f64;
    let rate = mbs * fps;
    let table: [(f64, f64, u8); 9] = [
        (3600.0, 108000.0, 31),
        (5120.0, 216000.0, 32),
        (8192.0, 245760.0, 40),
        (8704.0, 522240.0, 42),
        (22080.0, 589824.0, 50),
        (36864.0, 983040.0, 51),
        (36864.0, 2073600.0, 52),
        (139264.0, 4177920.0, 60),
        (139264.0, 16711680.0, 62),
    ];
    table
        .iter()
        .find(|&&(f, r, _)| mbs <= f && rate <= r)
        .map_or(62, |&(_, _, l)| l)
}

fn hevc_level(width: u32, height: u32, fps: f64) -> u8 {
    let ps = f64::from(width) * f64::from(height);
    let rate = ps * fps;
    let table: [(f64, f64, u8); 8] = [
        (983040.0, 33177600.0, 93),
        (2228224.0, 66846720.0, 120),
        (2228224.0, 133693440.0, 123),
        (8912896.0, 267386880.0, 150),
        (8912896.0, 534773760.0, 153),
        (8912896.0, 1069547520.0, 156),
        (35651584.0, 1069547520.0, 180),
        (35651584.0, 4278190080.0, 186),
    ];
    table
        .iter()
        .find(|&&(p, r, _)| ps <= p && rate <= r)
        .map_or(186, |&(_, _, l)| l)
}

fn bytes_of<T>(value: &T) -> Vec<u8> {
    let p = (value as *const T).cast::<u8>();
    unsafe { std::slice::from_raw_parts(p, std::mem::size_of::<T>()) }.to_vec()
}

fn misc<T>(kind: u32, value: &T) -> (ffi::VABufferType, Vec<u8>) {
    let mut out = kind.to_ne_bytes().to_vec();
    out.extend_from_slice(&bytes_of(value));
    (ffi::VAEncMiscParameterBufferType, out)
}

fn packed(out: &mut Vec<(ffi::VABufferType, Vec<u8>)>, kind: u32, p: Packed) {
    let params = ffi::VAEncPackedHeaderParameterBuffer {
        type_: kind,
        bit_length: p.bits,
        has_emulation_bytes: 1,
        ..Default::default()
    };
    out.push((ffi::VAEncPackedHeaderParameterBufferType, bytes_of(&params)));
    out.push((ffi::VAEncPackedHeaderDataBufferType, p.bytes));
}

fn flag(on: bool, bit: u32) -> u32 {
    u32::from(on) << bit
}

fn qp_for(rate: Rate, kind: Kind) -> i32 {
    match rate {
        Rate::Crf(crf) => {
            let crf = i32::from(crf);
            let qp = match kind {
                Kind::Idr => crf - 1,
                Kind::P => crf + 1,
                Kind::B => crf + 3,
            };
            qp.clamp(0, 51)
        }
        Rate::Qp(q) => i32::from(q).clamp(0, 51),
        _ => PIC_INIT_QP,
    }
}

impl VaEncoder {
    pub fn new(codec: Codec, config: &EncoderConfig) -> Result<Self> {
        let config = Settings {
            width: config.width,
            height: config.height,
            fps_num: config.fps.0,
            fps_den: config.fps.1,
            profile: config.profile,
            rate: config.rate,
            gop: config.gop,
            b_frames: u32::from(config.b_frames),
            bitstream: config.bitstream,
        };
        if config.width == 0 || config.height == 0 || config.fps_num == 0 || config.fps_den == 0 {
            return Err(Error::Invalid(
                "encoder size and frame rate must be set".into(),
            ));
        }
        let (profile, bits) = match (codec, config.profile) {
            (Codec::H264, Profile::Baseline) => (ffi::VAProfileH264ConstrainedBaseline, 8),
            (Codec::H264, Profile::Main) => (ffi::VAProfileH264Main, 8),
            (Codec::H264, Profile::High) => (ffi::VAProfileH264High, 8),
            (Codec::Hevc, Profile::Main10) => (ffi::VAProfileHEVCMain10, 10),
            (Codec::Hevc, _) => (ffi::VAProfileHEVCMain, 8),
            (c, p) => {
                return Err(Error::Unsupported(format!("{c:?} {p:?} on the GPU")));
            }
        };
        let display = super::va::probe().map_err(Error::Backend)?;
        let entry = [ffi::VAEntrypointEncSlice, ffi::VAEntrypointEncSliceLP]
            .into_iter()
            .find_map(|e| {
                display
                    .attributes(
                        profile,
                        e,
                        &[
                            ffi::VAConfigAttribRTFormat,
                            ffi::VAConfigAttribRateControl,
                            ffi::VAConfigAttribEncMaxRefFrames,
                            ffi::VAConfigAttribMaxPictureWidth,
                            ffi::VAConfigAttribMaxPictureHeight,
                            ffi::VAConfigAttribEncHEVCFeatures,
                            ffi::VAConfigAttribEncHEVCBlockSizes,
                        ],
                    )
                    .ok()
                    .filter(|v| v[0] != ffi::VA_ATTRIB_NOT_SUPPORTED && v[0] != 0)
            })
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "this VA-API driver ({}) has no {codec:?} {:?} encoder",
                    display.vendor(),
                    config.profile
                ))
            })?;
        let supported = |v: u32| v != ffi::VA_ATTRIB_NOT_SUPPORTED;
        let rc = rate_control(config.rate);
        if supported(entry[1]) && entry[1] & rc == 0 {
            return Err(Error::Unsupported(format!(
                "the VA-API {codec:?} encoder has no {:?} rate control",
                config.rate
            )));
        }
        for (limit, value, what) in [
            (entry[3], config.width, "wide"),
            (entry[4], config.height, "tall"),
        ] {
            if supported(limit) && limit != 0 && value > limit {
                return Err(Error::Unsupported(format!(
                    "the VA-API {codec:?} encoder takes pictures up to {limit} {what}"
                )));
            }
        }
        let (max_l0, max_l1) = if supported(entry[2]) {
            (entry[2] & 0xffff, (entry[2] >> 16) & 0xffff)
        } else {
            (1, 0)
        };
        if max_l0 == 0 {
            return Err(Error::Unsupported(
                "the VA-API encoder takes no reference frames".into(),
            ));
        }
        let mut config = config;
        if codec == Codec::Hevc || max_l1 == 0 || config.profile == Profile::Baseline {
            config.b_frames = 0;
        }
        config.b_frames = config.b_frames.min(3);
        if config.gop == 0 {
            config.gop = (config.fps_num / config.fps_den.max(1))
                .max(1)
                .saturating_mul(2);
        }
        let blocks = if supported(entry[6]) && codec == Codec::Hevc {
            let v = entry[6];
            let field = |i: u32| (v >> (2 * i)) & 3;
            Blocks {
                log2_ctb: field(0) + 3,
                log2_min_cb: field(2) + 3,
                log2_max_tb: field(3) + 2,
                log2_min_tb: field(4) + 2,
                depth_inter: field(5),
                depth_intra: field(7),
            }
        } else {
            Blocks {
                log2_ctb: 5,
                log2_min_cb: 3,
                log2_min_tb: 2,
                log2_max_tb: 5,
                depth_inter: 2,
                depth_intra: 2,
            }
        };
        let tools = if supported(entry[5]) && codec == Codec::Hevc {
            let v = entry[5];
            let field = |i: u32| (v >> (2 * i)) & 3;
            HevcTools {
                amp: field(2) != 0,
                sao: field(3) != 0,
                strong_intra: field(6) != 0,
                temporal_mvp: field(5) != 0,
            }
        } else {
            HevcTools {
                amp: true,
                sao: true,
                strong_intra: true,
                temporal_mvp: true,
            }
        };
        let align = match codec {
            Codec::Hevc => 1u32 << blocks.log2_min_cb,
            _ => 16,
        };
        let size = (
            config.width.next_multiple_of(align),
            config.height.next_multiple_of(align),
        );
        let inputs = DEPTH + config.b_frames as usize + 1;
        let coded_size = (size.0 as usize * size.1 as usize * 3 / 2).max(1 << 20);
        let session = EncodeSession::new(&EncodeSetup {
            profile,
            rate_control: rc,
            size,
            bits,
            inputs,
            references: REFERENCES,
            coded_size,
        })
        .map_err(Error::Backend)?;
        let fps = f64::from(config.fps_num) / f64::from(config.fps_den);
        let level = match codec {
            Codec::Hevc => hevc_level(size.0, size.1, fps),
            _ => h264_level(size.0, size.1, fps),
        };
        let name = format!(
            "VA-API {} encoder ({})",
            if codec == Codec::Hevc {
                "HEVC"
            } else {
                "H.264"
            },
            session.vendor()
        );
        Ok(Self {
            codec,
            config,
            session,
            size,
            bits,
            level,
            blocks,
            tools,
            free: (0..inputs).rev().collect(),
            waiting: Vec::new(),
            in_flight: VecDeque::new(),
            display_pts: VecDeque::new(),
            coded: 0,
            received: 0,
            gop_start: 0,
            refs_since_idr: 0,
            idr_id: 0,
            anchors: [None, None],
            duration: 0,
            extradata: None,
            sets: [Vec::new(), Vec::new(), Vec::new()],
            name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn extradata(&self) -> Option<&[u8]> {
        self.extradata.as_deref()
    }

    pub fn b_frames(&self) -> u8 {
        self.config.b_frames as u8
    }

    fn kind_of(&self, order: u64) -> Kind {
        let i = order - self.gop_start;
        let gop = u64::from(self.config.gop);
        let period = u64::from(self.config.b_frames) + 1;
        if i == 0 {
            Kind::Idr
        } else if self.config.b_frames == 0 || i.is_multiple_of(period) || i + 1 == gop {
            Kind::P
        } else {
            Kind::B
        }
    }

    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        let expected = match self.bits {
            10 => crate::media::PixelFormat::Yuv420p10,
            _ => crate::media::PixelFormat::Yuv420p8,
        };
        if frame.format != expected
            || frame.width != self.config.width
            || frame.height != self.config.height
        {
            return Err(Error::Invalid(format!(
                "the encoder takes {}x{} {expected:?} frames, not {}x{} {:?}",
                self.config.width, self.config.height, frame.width, frame.height, frame.format
            )));
        }
        self.push(frame.pts, frame.time_base, |session, input| {
            session.upload(input, frame)
        })
    }

    pub fn encode_surface(&mut self, src: &GpuSurface, time_base: TimeBase) -> Result<Vec<Packet>> {
        let size = (self.config.width, self.config.height);
        self.push(src.pts, time_base, |session, input| {
            session.blit(input, src, size).or_else(|blit| {
                src.download()
                    .and_then(|f| session.upload(input, &f))
                    .map_err(|e| format!("{blit}; then {e}"))
            })
        })
    }

    fn push(
        &mut self,
        pts: i64,
        tb: TimeBase,
        fill: impl FnOnce(&mut EncodeSession, ffi::VASurfaceID) -> std::result::Result<(), String>,
    ) -> Result<Vec<Packet>> {
        if self.duration == 0 {
            let num = i128::from(tb.den) * i128::from(self.config.fps_den);
            let den = i128::from(tb.num) * i128::from(self.config.fps_num);
            self.duration = if den > 0 {
                ((num + den / 2) / den).max(1) as i64
            } else {
                1
            };
        }
        let mut out = Vec::new();
        let slot = self.take_slot(&mut out)?;
        let Some(ids) = self.session.slot(slot) else {
            self.free.push(slot);
            return Err(Error::Backend("encoder slot out of range".into()));
        };
        if let Err(e) = fill(&mut self.session, ids.input) {
            self.free.push(slot);
            return Err(Error::Backend(e));
        }
        let order = self.received;
        self.received += 1;
        if order - self.gop_start >= u64::from(self.config.gop) {
            self.gop_start = order;
        }
        self.display_pts.push_back((order, pts));
        let waiting = Waiting { slot, pts, order };
        match self.kind_of(order) {
            Kind::B => self.waiting.push(waiting),
            kind => {
                self.submit(waiting, kind)?;
                self.submit_waiting()?;
            }
        }
        self.drain(DEPTH, &mut out)?;
        Ok(out)
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        if let Some(last) = self.waiting.pop() {
            self.submit(last, Kind::P)?;
            self.submit_waiting()?;
        }
        let mut out = Vec::new();
        self.drain(0, &mut out)?;
        Ok(out)
    }

    fn take_slot(&mut self, out: &mut Vec<Packet>) -> Result<usize> {
        if self.free.is_empty() {
            self.drain(self.in_flight.len().saturating_sub(1), out)?;
        }
        self.free
            .pop()
            .ok_or_else(|| Error::Backend("no free encoder input surface".into()))
    }

    fn submit_waiting(&mut self) -> Result<()> {
        let waiting = std::mem::take(&mut self.waiting);
        for w in waiting {
            self.submit(w, Kind::B)?;
        }
        Ok(())
    }

    fn recon_target(&self) -> usize {
        (0..REFERENCES)
            .find(|i| !self.anchors.iter().flatten().any(|a| a.0 == *i))
            .unwrap_or(0)
    }

    fn submit(&mut self, w: Waiting, kind: Kind) -> Result<()> {
        if kind == Kind::Idr {
            self.refs_since_idr = 0;
            self.anchors = [None, None];
            self.idr_id = self.idr_id.wrapping_add(1);
        }
        let target = self.recon_target();
        let frame_num = self.refs_since_idr;
        let poc = u32::try_from((w.order - self.gop_start) * 2).unwrap_or(0);
        let params = match self.codec {
            Codec::Hevc => self.hevc_params(kind, w.slot, target, poc)?,
            _ => self.h264_params(kind, w.slot, target, poc, frame_num)?,
        };
        let input = self
            .session
            .slot(w.slot)
            .ok_or_else(|| Error::Backend("encoder slot out of range".into()))?
            .input;
        self.session
            .encode(input, &params)
            .map_err(Error::Backend)?;
        if kind != Kind::B {
            self.refs_since_idr += 1;
            self.anchors = [self.anchors[1], Some((target, w.order, frame_num))];
        }
        self.in_flight.push_back(InFlight {
            slot: w.slot,
            pts: w.pts,
            key: kind == Kind::Idr,
        });
        Ok(())
    }

    fn drain(&mut self, keep: usize, out: &mut Vec<Packet>) -> Result<()> {
        while self.in_flight.len() > keep {
            let Some(f) = self.in_flight.pop_front() else {
                break;
            };
            let slot = self
                .session
                .slot(f.slot)
                .ok_or_else(|| Error::Backend("encoder slot out of range".into()))?;
            let bytes = self.session.collect(slot.input, slot.coded);
            self.free.push(f.slot);
            let bytes = bytes.map_err(Error::Backend)?;
            out.push(self.packet(&bytes, f)?);
        }
        Ok(())
    }

    fn packet(&mut self, bytes: &[u8], f: InFlight) -> Result<Packet> {
        let codec = self.codec;
        let nals = nal::annexb(bytes);
        let mut keep: Vec<&[u8]> = Vec::with_capacity(nals.len());
        let mut found = [Vec::new(), Vec::new(), Vec::new()];
        let mut keyframe = f.key;
        for n in nals {
            let t = nal::nal_type(codec, n);
            let set = match codec {
                Codec::Hevc => match t {
                    32 => Some(0),
                    33 => Some(1),
                    34 => Some(2),
                    35 => continue,
                    _ => None,
                },
                _ => match t {
                    7 => Some(1),
                    8 => Some(2),
                    9 => continue,
                    _ => None,
                },
            };
            match set {
                Some(i) => found[i].push(n.to_vec()),
                None => {
                    keyframe |= nal::is_keyframe(codec, n);
                    keep.push(n);
                }
            }
        }
        if !found[1].is_empty() && self.extradata.is_none() {
            let record = match codec {
                Codec::Hevc => nal::build_hvcc(&found[0], &found[1], &found[2])?,
                _ => nal::build_avcc(&found[1], &found[2])?,
            };
            self.extradata = Some(record);
            self.sets = found;
        }
        if self.extradata.is_none() {
            return Err(Error::Backend(
                "the VA-API driver wrote no parameter sets before the first picture".into(),
            ));
        }
        let index = self.coded;
        self.coded += 1;
        while self.display_pts.front().is_some_and(|&(o, _)| o < index) {
            self.display_pts.pop_front();
        }
        let base = self.display_pts.front().map_or(f.pts, |&(_, pts)| pts);
        let delay = if self.config.b_frames > 0 {
            self.duration
        } else {
            0
        };
        let data = match self.config.bitstream {
            Bitstream::LengthPrefixed => nal::length_prefix(&keep),
            Bitstream::AnnexB => {
                let mut out = Vec::with_capacity(bytes.len() + 64);
                let sets = self.sets.iter().flatten().map(Vec::as_slice);
                let mut put = |n: &[u8]| {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(n);
                };
                if keyframe {
                    sets.for_each(&mut put);
                }
                keep.iter().for_each(|n| put(n));
                out
            }
        };
        Ok(Packet {
            data,
            pts: f.pts,
            dts: base - delay,
            keyframe,
        })
    }

    fn h264_ref(&self, anchor: (usize, u64, u32)) -> Result<VAPictureH264> {
        let surface = self
            .session
            .reference(anchor.0)
            .ok_or_else(|| Error::Backend("reference surface out of range".into()))?;
        let poc = i32::try_from((anchor.1 - self.gop_start.min(anchor.1)) * 2).unwrap_or(0);
        Ok(VAPictureH264 {
            picture_id: surface,
            frame_idx: anchor.2,
            flags: ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            TopFieldOrderCnt: poc,
            BottomFieldOrderCnt: poc,
            va_reserved: [0; ffi::VA_PADDING_LOW],
        })
    }

    fn sequence_misc(&self, out: &mut Vec<(ffi::VABufferType, Vec<u8>)>) {
        let fps = ffi::VAEncMiscParameterFrameRate {
            framerate: (self.config.fps_den << 16) | (self.config.fps_num & 0xffff),
            ..Default::default()
        };
        out.push(misc(ffi::VAEncMiscParameterTypeFrameRate, &fps));

        if let Some((target, peak)) = self.bitrate() {
            let rc = ffi::VAEncMiscParameterRateControl {
                bits_per_second: peak,
                target_percentage: (u64::from(target) * 100 / u64::from(peak.max(1))).clamp(1, 100)
                    as u32,
                window_size: 1000,
                ..Default::default()
            };
            out.push(misc(ffi::VAEncMiscParameterTypeRateControl, &rc));
            let hrd = ffi::VAEncMiscParameterHRD {
                initial_buffer_fullness: peak,
                buffer_size: peak,
                ..Default::default()
            };
            out.push(misc(ffi::VAEncMiscParameterTypeHRD, &hrd));
        }
    }

    fn bitrate(&self) -> Option<(u32, u32)> {
        match self.config.rate {
            Rate::Bitrate(kbps) => {
                let bps = kbps.saturating_mul(1000);
                Some((bps, bps))
            }
            Rate::Vbr { kbps, max_kbps } => Some((
                kbps.saturating_mul(1000),
                max_kbps.max(kbps).saturating_mul(1000),
            )),
            Rate::Crf(_) | Rate::Qp(_) => None,
        }
    }

    fn bits_per_second(&self) -> u32 {
        self.bitrate().map_or(0, |(_, peak)| peak)
    }

    fn h264_sequence(&self) -> headers::H264Sequence {
        let c = &self.config;
        let (profile_idc, constraints) = match c.profile {
            Profile::Baseline => (66, 0xc0),
            Profile::Main => (77, 0x40),
            _ => (100, 0),
        };
        headers::H264Sequence {
            profile_idc,
            constraints,
            level_idc: self.level,
            width_mbs: self.size.0 / 16,
            height_mbs: self.size.1 / 16,
            crop_right: (self.size.0 - c.width) / 2,
            crop_bottom: (self.size.1 - c.height) / 2,
            max_refs: if c.b_frames > 0 { 2 } else { 1 },
            reorder: u32::from(c.b_frames > 0),
            fps_num: c.fps_num,
            fps_den: c.fps_den,
            cabac: c.profile != Profile::Baseline,
            transform_8x8: c.profile == Profile::High,
        }
    }

    fn hevc_sequence(&self) -> headers::HevcSequence {
        let c = &self.config;
        let b = &self.blocks;
        let t = &self.tools;
        headers::HevcSequence {
            profile_idc: if self.bits == 10 { 2 } else { 1 },
            level_idc: self.level,
            width: self.size.0,
            height: self.size.1,
            crop_right: (self.size.0 - c.width) / 2,
            crop_bottom: (self.size.1 - c.height) / 2,
            bit_depth: self.bits,
            log2_min_cb: b.log2_min_cb,
            log2_ctb: b.log2_ctb,
            log2_min_tb: b.log2_min_tb,
            log2_max_tb: b.log2_max_tb,
            depth_inter: b.depth_inter,
            depth_intra: b.depth_intra,
            amp: t.amp,
            sao: t.sao,
            temporal_mvp: t.temporal_mvp,
            strong_intra: t.strong_intra,
            cu_qp_delta: !constant_qp(c.rate),
            fps_num: c.fps_num,
            fps_den: c.fps_den,
        }
    }

    fn h264_params(
        &self,
        kind: Kind,
        slot: usize,
        target: usize,
        poc: u32,
        frame_num: u32,
    ) -> Result<Vec<(ffi::VABufferType, Vec<u8>)>> {
        let c = &self.config;
        let mut out = Vec::new();
        let (wm, hm) = (self.size.0 / 16, self.size.1 / 16);
        if kind == Kind::Idr {
            let crop_r = (self.size.0 - c.width) / 2;
            let crop_b = (self.size.1 - c.height) / 2;
            use ffi::{enc_seq_bits as s, enc_vui_bits as v};
            let mut seq = ffi::VAEncSequenceParameterBufferH264 {
                seq_parameter_set_id: 0,
                level_idc: self.level,
                intra_period: c.gop,
                intra_idr_period: c.gop,
                ip_period: c.b_frames + 1,
                bits_per_second: self.bits_per_second(),
                max_num_ref_frames: if c.b_frames > 0 { 2 } else { 1 },
                picture_width_in_mbs: wm as u16,
                picture_height_in_mbs: hm as u16,
                seq_fields: 1 << s::CHROMA_FORMAT_IDC
                    | 1 << s::FRAME_MBS_ONLY
                    | 1 << s::DIRECT_8X8_INFERENCE
                    | 12 << s::LOG2_MAX_FRAME_NUM_MINUS4
                    | 12 << s::LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4,
                bit_depth_luma_minus8: 0,
                bit_depth_chroma_minus8: 0,
                num_ref_frames_in_pic_order_cnt_cycle: 0,
                offset_for_non_ref_pic: 0,
                offset_for_top_to_bottom_field: 0,
                offset_for_ref_frame: [0; 256],
                frame_cropping_flag: u8::from(crop_r > 0 || crop_b > 0),
                frame_crop_left_offset: 0,
                frame_crop_right_offset: crop_r,
                frame_crop_top_offset: 0,
                frame_crop_bottom_offset: crop_b,
                vui_parameters_present_flag: 1,
                vui_fields: 1 << v::TIMING_INFO_PRESENT
                    | 1 << v::BITSTREAM_RESTRICTION
                    | 15 << v::LOG2_MAX_MV_LENGTH_HORIZONTAL
                    | 15 << v::LOG2_MAX_MV_LENGTH_VERTICAL
                    | 1 << v::FIXED_FRAME_RATE,
                aspect_ratio_idc: 0,
                sar_width: 0,
                sar_height: 0,
                num_units_in_tick: c.fps_den,
                time_scale: c.fps_num.saturating_mul(2),
                va_reserved: [0; ffi::VA_PADDING_LOW],
            };
            if seq.picture_width_in_mbs == 0 {
                seq.picture_width_in_mbs = 1;
            }
            out.push((ffi::VAEncSequenceParameterBufferType, bytes_of(&seq)));
            self.sequence_misc(&mut out);
        }
        let slot_ids = self
            .session
            .slot(slot)
            .ok_or_else(|| Error::Backend("encoder slot out of range".into()))?;
        let recon = self
            .session
            .reference(target)
            .ok_or_else(|| Error::Backend("reference surface out of range".into()))?;
        let mut refs = [VAPictureH264::INVALID; 16];
        let mut l0 = [VAPictureH264::INVALID; 32];
        let mut l1 = [VAPictureH264::INVALID; 32];
        let (mut n0, mut n1) = (0u8, 0u8);
        match kind {
            Kind::P => {
                let prev = self.anchors[1]
                    .ok_or_else(|| Error::Backend("P picture without a reference".into()))?;
                refs[0] = self.h264_ref(prev)?;
                l0[0] = refs[0];
                n0 = 1;
            }
            Kind::B => {
                let past = self.anchors[0]
                    .ok_or_else(|| Error::Backend("B picture without a past reference".into()))?;
                let future = self.anchors[1]
                    .ok_or_else(|| Error::Backend("B picture without a future reference".into()))?;
                refs[0] = self.h264_ref(past)?;
                refs[1] = self.h264_ref(future)?;
                l0[0] = refs[0];
                l1[0] = refs[1];
                n0 = 1;
                n1 = 1;
            }
            Kind::Idr => {}
        }
        use ffi::enc_pic_bits as p;
        let high = c.profile != Profile::Baseline;
        let pic = ffi::VAEncPictureParameterBufferH264 {
            CurrPic: VAPictureH264 {
                picture_id: recon,
                frame_idx: frame_num,
                flags: 0,
                TopFieldOrderCnt: poc as i32,
                BottomFieldOrderCnt: poc as i32,
                va_reserved: [0; ffi::VA_PADDING_LOW],
            },
            ReferenceFrames: refs,
            coded_buf: slot_ids.coded,
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            last_picture: 0,
            frame_num: frame_num as u16,
            pic_init_qp: PIC_INIT_QP as u8,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            chroma_qp_index_offset: 0,
            second_chroma_qp_index_offset: 0,
            pic_fields: flag(kind == Kind::Idr, p::IDR_PIC)
                | flag(kind != Kind::B, p::REFERENCE_PIC)
                | flag(high, p::ENTROPY_CODING_MODE)
                | flag(c.profile == Profile::High, p::TRANSFORM_8X8_MODE)
                | flag(true, p::DEBLOCKING_FILTER_CONTROL_PRESENT),
            va_reserved: [0; ffi::VA_PADDING_LOW],
        };
        out.push((ffi::VAEncPictureParameterBufferType, bytes_of(&pic)));
        if kind == Kind::Idr {
            let seq = self.h264_sequence();
            packed(
                &mut out,
                ffi::VAEncPackedHeaderSequence,
                headers::join(vec![headers::h264_sps(&seq), headers::h264_pps(&seq)]),
            );
        }
        let qp_delta = qp_for(c.rate, kind) - PIC_INIT_QP;
        packed(
            &mut out,
            ffi::VAEncPackedHeaderSlice,
            headers::h264_slice(&headers::H264Slice {
                idr: kind == Kind::Idr,
                reference: kind != Kind::B,
                slice_type: match kind {
                    Kind::Idr => 2,
                    Kind::P => 0,
                    Kind::B => 1,
                },
                frame_num,
                idr_pic_id: u32::from(self.idr_id),
                poc_lsb: poc & 0xffff,
                cabac: c.profile != Profile::Baseline,
                qp_delta,
            }),
        );
        let slice = ffi::VAEncSliceParameterBufferH264 {
            macroblock_address: 0,
            num_macroblocks: wm * hm,
            macroblock_info: ffi::VA_INVALID_ID,
            slice_type: match kind {
                Kind::Idr => 2,
                Kind::P => 0,
                Kind::B => 1,
            },
            pic_parameter_set_id: 0,
            idr_pic_id: self.idr_id,
            pic_order_cnt_lsb: (poc & 0xffff) as u16,
            delta_pic_order_cnt_bottom: 0,
            delta_pic_order_cnt: [0; 2],
            direct_spatial_mv_pred_flag: 1,
            num_ref_idx_active_override_flag: 0,
            num_ref_idx_l0_active_minus1: n0.saturating_sub(1),
            num_ref_idx_l1_active_minus1: n1.saturating_sub(1),
            RefPicList0: l0,
            RefPicList1: l1,
            luma_log2_weight_denom: 0,
            chroma_log2_weight_denom: 0,
            luma_weight_l0_flag: 0,
            luma_weight_l0: [0; 32],
            luma_offset_l0: [0; 32],
            chroma_weight_l0_flag: 0,
            chroma_weight_l0: [[0; 2]; 32],
            chroma_offset_l0: [[0; 2]; 32],
            luma_weight_l1_flag: 0,
            luma_weight_l1: [0; 32],
            luma_offset_l1: [0; 32],
            chroma_weight_l1_flag: 0,
            chroma_weight_l1: [[0; 2]; 32],
            chroma_offset_l1: [[0; 2]; 32],
            cabac_init_idc: 0,
            slice_qp_delta: (qp_for(c.rate, kind) - PIC_INIT_QP) as i8,
            disable_deblocking_filter_idc: 0,
            slice_alpha_c0_offset_div2: 0,
            slice_beta_offset_div2: 0,
            va_reserved: [0; ffi::VA_PADDING_LOW],
        };
        out.push((ffi::VAEncSliceParameterBufferType, bytes_of(&slice)));
        Ok(out)
    }

    fn hevc_ref(&self, anchor: (usize, u64, u32)) -> Result<VAPictureHEVC> {
        let surface = self
            .session
            .reference(anchor.0)
            .ok_or_else(|| Error::Backend("reference surface out of range".into()))?;
        let poc = i32::try_from((anchor.1 - self.gop_start.min(anchor.1)) * 2).unwrap_or(0);
        Ok(VAPictureHEVC {
            picture_id: surface,
            pic_order_cnt: poc,
            flags: 0,
            va_reserved: [0; ffi::VA_PADDING_LOW],
        })
    }

    fn hevc_params(
        &self,
        kind: Kind,
        slot: usize,
        target: usize,
        poc: u32,
    ) -> Result<Vec<(ffi::VABufferType, Vec<u8>)>> {
        let c = &self.config;
        let b = &self.blocks;
        let t = &self.tools;
        let mut out = Vec::new();
        let cqp = constant_qp(c.rate);
        if kind == Kind::Idr {
            use ffi::{enc_hevc_seq_bits as s, enc_hevc_vui_bits as v};
            let depth = self.bits - 8;
            let seq = ffi::VAEncSequenceParameterBufferHEVC {
                general_profile_idc: if self.bits == 10 { 2 } else { 1 },
                general_level_idc: self.level,
                general_tier_flag: 0,
                intra_period: c.gop,
                intra_idr_period: c.gop,
                ip_period: 1,
                bits_per_second: self.bits_per_second(),
                pic_width_in_luma_samples: self.size.0 as u16,
                pic_height_in_luma_samples: self.size.1 as u16,
                seq_fields: 1 << s::CHROMA_FORMAT_IDC
                    | depth << s::BIT_DEPTH_LUMA_MINUS8
                    | depth << s::BIT_DEPTH_CHROMA_MINUS8
                    | flag(t.strong_intra, s::STRONG_INTRA_SMOOTHING)
                    | flag(t.amp, s::AMP)
                    | flag(t.sao, s::SAO)
                    | flag(t.temporal_mvp, s::TEMPORAL_MVP)
                    | 1 << s::LOW_DELAY_SEQ,
                log2_min_luma_coding_block_size_minus3: (b.log2_min_cb - 3) as u8,
                log2_diff_max_min_luma_coding_block_size: (b.log2_ctb - b.log2_min_cb) as u8,
                log2_min_transform_block_size_minus2: (b.log2_min_tb - 2) as u8,
                log2_diff_max_min_transform_block_size: (b.log2_max_tb - b.log2_min_tb) as u8,
                max_transform_hierarchy_depth_inter: b.depth_inter as u8,
                max_transform_hierarchy_depth_intra: b.depth_intra as u8,
                pcm_sample_bit_depth_luma_minus1: 0,
                pcm_sample_bit_depth_chroma_minus1: 0,
                log2_min_pcm_luma_coding_block_size_minus3: 0,
                log2_max_pcm_luma_coding_block_size_minus3: 0,
                vui_parameters_present_flag: 1,
                vui_fields: 1 << v::TIMING_INFO_PRESENT
                    | 1 << v::BITSTREAM_RESTRICTION
                    | 1 << v::MOTION_VECTORS_OVER_PIC_BOUNDARIES
                    | 15 << v::LOG2_MAX_MV_LENGTH_HORIZONTAL
                    | 15 << v::LOG2_MAX_MV_LENGTH_VERTICAL,
                aspect_ratio_idc: 0,
                sar_width: 0,
                sar_height: 0,
                vui_num_units_in_tick: c.fps_den,
                vui_time_scale: c.fps_num,
                min_spatial_segmentation_idc: 0,
                max_bytes_per_pic_denom: 0,
                max_bits_per_min_cu_denom: 0,
                scc_fields: 0,
                va_reserved: [0; ffi::VA_PADDING_MEDIUM - 1],
            };
            out.push((ffi::VAEncSequenceParameterBufferType, bytes_of(&seq)));
            self.sequence_misc(&mut out);
        }
        let slot_ids = self
            .session
            .slot(slot)
            .ok_or_else(|| Error::Backend("encoder slot out of range".into()))?;
        let recon = self
            .session
            .reference(target)
            .ok_or_else(|| Error::Backend("reference surface out of range".into()))?;
        let mut refs = [VAPictureHEVC::INVALID; 15];
        let mut l0 = [VAPictureHEVC::INVALID; 15];
        if kind == Kind::P {
            let prev = self.anchors[1]
                .ok_or_else(|| Error::Backend("P picture without a reference".into()))?;
            refs[0] = self.hevc_ref(prev)?;
            l0[0] = refs[0];
        }
        use ffi::{enc_hevc_pic_bits as p, enc_hevc_slice_bits as sb};
        let pic = ffi::VAEncPictureParameterBufferHEVC {
            decoded_curr_pic: VAPictureHEVC {
                picture_id: recon,
                pic_order_cnt: poc as i32,
                flags: 0,
                va_reserved: [0; ffi::VA_PADDING_LOW],
            },
            reference_frames: refs,
            coded_buf: slot_ids.coded,
            collocated_ref_pic_index: if kind == Kind::P { 0 } else { 0xff },
            last_picture: 0,
            pic_init_qp: PIC_INIT_QP as u8,
            diff_cu_qp_delta_depth: 0,
            pps_cb_qp_offset: 0,
            pps_cr_qp_offset: 0,
            num_tile_columns_minus1: 0,
            num_tile_rows_minus1: 0,
            column_width_minus1: [0; 19],
            row_height_minus1: [0; 21],
            log2_parallel_merge_level_minus2: 0,
            ctu_max_bitsize_allowed: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            slice_pic_parameter_set_id: 0,
            nal_unit_type: if kind == Kind::Idr { 19 } else { 1 },
            pic_fields: flag(kind == Kind::Idr, p::IDR_PIC)
                | (if kind == Kind::Idr { 1 } else { 2 }) << p::CODING_TYPE
                | 1 << p::REFERENCE_PIC
                | flag(!cqp, p::CU_QP_DELTA_ENABLED)
                | 1 << p::PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED,
            hierarchical_level_plus1: 0,
            va_byte_reserved: 0,
            scc_fields: 0,
            va_reserved: [0; ffi::VA_PADDING_HIGH - 1],
        };
        out.push((ffi::VAEncPictureParameterBufferType, bytes_of(&pic)));
        if kind == Kind::Idr {
            let seq = self.hevc_sequence();
            packed(
                &mut out,
                ffi::VAEncPackedHeaderSequence,
                headers::join(vec![
                    headers::hevc_vps(&seq),
                    headers::hevc_sps(&seq),
                    headers::hevc_pps(&seq),
                ]),
            );
        }
        packed(
            &mut out,
            ffi::VAEncPackedHeaderSlice,
            headers::hevc_slice(&headers::HevcSlice {
                idr: kind == Kind::Idr,
                poc,
                ref_delta: self.anchors[1].map_or(2, |a| {
                    poc.saturating_sub(
                        u32::try_from((a.1 - self.gop_start.min(a.1)) * 2).unwrap_or(0),
                    )
                }),
                sao: t.sao,
                temporal_mvp: t.temporal_mvp,
                qp_delta: qp_for(c.rate, kind) - PIC_INIT_QP,
            }),
        );
        let ctb = 1u32 << b.log2_ctb;
        let ctus = self.size.0.div_ceil(ctb) * self.size.1.div_ceil(ctb);
        let slice = ffi::VAEncSliceParameterBufferHEVC {
            slice_segment_address: 0,
            num_ctu_in_slice: ctus,
            slice_type: if kind == Kind::Idr { 2 } else { 1 },
            slice_pic_parameter_set_id: 0,
            num_ref_idx_l0_active_minus1: 0,
            num_ref_idx_l1_active_minus1: 0,
            ref_pic_list0: l0,
            ref_pic_list1: [VAPictureHEVC::INVALID; 15],
            luma_log2_weight_denom: 0,
            delta_chroma_log2_weight_denom: 0,
            delta_luma_weight_l0: [0; 15],
            luma_offset_l0: [0; 15],
            delta_chroma_weight_l0: [[0; 2]; 15],
            chroma_offset_l0: [[0; 2]; 15],
            delta_luma_weight_l1: [0; 15],
            luma_offset_l1: [0; 15],
            delta_chroma_weight_l1: [[0; 2]; 15],
            chroma_offset_l1: [[0; 2]; 15],
            max_num_merge_cand: 5,
            slice_qp_delta: (qp_for(c.rate, kind) - PIC_INIT_QP) as i8,
            slice_cb_qp_offset: 0,
            slice_cr_qp_offset: 0,
            slice_beta_offset_div2: 0,
            slice_tc_offset_div2: 0,
            slice_fields: 1 << sb::LAST_SLICE_OF_PIC
                | flag(t.sao, sb::SLICE_SAO_LUMA)
                | flag(t.sao, sb::SLICE_SAO_CHROMA)
                | 1 << sb::SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED,
            pred_weight_table_bit_offset: 0,
            pred_weight_table_bit_length: 0,
            va_reserved: [0; ffi::VA_PADDING_MEDIUM - 2],
        };
        out.push((ffi::VAEncSliceParameterBufferType, bytes_of(&slice)));
        Ok(out)
    }
}
