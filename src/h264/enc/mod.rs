mod cabac;
mod cabac_mb;
mod cavlc;
mod deblock;
mod dsp;
mod intra;
mod lookahead;
mod mbinfo;
mod nal;
mod picture;
mod ratecontrol;
mod slice;
mod syntax;
mod tables;
mod transform;

use std::collections::VecDeque;

use cabac::CostTable;
use deblock::{BandPlane, DeblockParams, deblock_band};
use lookahead::{Complexity, LowRes};
use mbinfo::MbInfo;
use nal::{
    NAL_AUD, NAL_IDR, NAL_PPS, NAL_SEI, NAL_SLICE, NAL_SPS, Pps, SliceHeader, SliceType, Sps, Vui,
};
use picture::{CHROMA_PAD, Frame, LUMA_PAD, RefPic, build_hpel};
use ratecontrol::RcKind;
use slice::{Band, EncParams, FrameEnc, Lambdas, SliceEnc, SliceOut};
use tables::NUM_CTX;
use transform::QuantTables;

#[derive(Debug)]
pub enum Error {
    InvalidDimensions(u32, u32),
    InvalidFrameRate(u32, u32),
    InvalidConfig(String),
    BadFrame,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidDimensions(w, h) => {
                write!(f, "invalid dimensions {w}x{h} (must be even and non-zero)")
            }
            Error::InvalidFrameRate(n, d) => write!(f, "invalid frame rate {n}/{d}"),
            Error::InvalidConfig(s) => write!(f, "invalid configuration: {s}"),
            Error::BadFrame => write!(f, "input frame planes too small for the configured size"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Profile {
    Baseline,
    Main,
    #[default]
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Preset {
    Speed,
    #[default]
    Balanced,
    Quality,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateControl {
    Crf(f32),
    Cbr { kbps: u32 },
    Vbr { target_kbps: u32, max_kbps: u32 },
    Qp(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorConfig {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl Default for ColorConfig {
    fn default() -> Self {
        ColorConfig {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PacketFormat {
    #[default]
    AnnexB,
    LengthPrefixed,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub profile: Profile,
    pub preset: Preset,
    pub rate: RateControl,
    pub keyint: u32,
    pub bframes: u8,
    pub color: ColorConfig,
    pub threads: usize,
    pub format: PacketFormat,
    pub aq_strength: f32,
    pub sar: (u16, u16),
    pub scenecut: bool,
    pub slices: usize,
    pub aud: bool,
    pub mastering_display: Option<[u8; 24]>,
    pub content_light: Option<(u16, u16)>,
    pub level: Option<u8>,
}

impl EncoderConfig {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        EncoderConfig {
            width,
            height,
            fps_num,
            fps_den,
            profile: Profile::High,
            preset: Preset::Balanced,
            rate: RateControl::Crf(23.0),
            keyint: 250,
            bframes: 2,
            color: ColorConfig::default(),
            threads: 0,
            format: PacketFormat::AnnexB,
            aq_strength: 0.6,
            sar: (1, 1),
            scenecut: true,
            slices: 0,
            mastering_display: None,
            content_light: None,
            aud: true,
            level: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct YuvFrame<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub keyframe: bool,
}

struct Input {
    frame: Frame,
    pts: i64,
    display: u64,
    cplx: Complexity,
    aq: Vec<f32>,
}

struct Pool {
    pool: Option<rayon::ThreadPool>,
}

impl Pool {
    fn install<R: Send>(&self, f: impl FnOnce() -> R + Send) -> R {
        if let Some(p) = &self.pool {
            return p.install(f);
        }
        f()
    }
}

pub struct Encoder {
    cfg: EncoderConfig,
    params: EncParams,
    sps: Sps,
    sps_nal: Vec<u8>,
    pps_nal: Vec<u8>,
    qt: QuantTables,
    cost_tab: CostTable,
    lam: Lambdas,
    cabac_i: Box<[(i8, i8); NUM_CTX]>,
    cabac_pb: Box<[(i8, i8); NUM_CTX]>,
    rc: ratecontrol::RateControl,
    pending: VecDeque<Input>,
    prev_lowres: Option<LowRes>,
    display_count: u64,
    last_idr: u64,
    idr_count: u32,
    next_frame_num: u32,
    dpb: Vec<RefPic>,
    coded_count: u64,
    display_pts: VecDeque<i64>,
    dts_offset: Option<i64>,
    all_pts: Vec<i64>,
    held: Vec<Packet>,
    frame_pool: Vec<Frame>,
    hpel_pool: Vec<[picture::Plane; 3]>,
    sei_sent: bool,
    slices: usize,
    pool: Pool,
    ref_uid: u32,
    max_refs: usize,
}

const LOG2_MAX_FRAME_NUM: u32 = 8;
const LOG2_MAX_POC_LSB: u32 = 8;

fn crf_level_kbps(crf: f32, mbw: usize, mbh: usize, fps: f64) -> u32 {
    let crf = if crf.is_finite() {
        f64::from(crf.clamp(0.0, 51.0))
    } else {
        23.0
    };
    let bpp = 3.0 * (-crf / 6.0).exp2();
    let pixels = (mbw as f64) * (mbh as f64) * 256.0;
    (pixels * fps * bpp / 1000.0).ceil() as u32
}

impl Encoder {
    fn options(&self) -> String {
        let c = &self.cfg;
        let rate = match c.rate {
            RateControl::Qp(q) => format!("rc=cqp qp={q}"),
            RateControl::Crf(f) => format!("rc=crf crf={:.1}", f.clamp(0.0, 51.0)),
            RateControl::Cbr { kbps } => format!("rc=cbr bitrate={kbps}"),
            RateControl::Vbr {
                target_kbps,
                max_kbps,
            } => format!("rc=vbr bitrate={target_kbps} vbv_maxrate={max_kbps}"),
        };
        let profile = match c.profile {
            Profile::Baseline => "baseline",
            Profile::Main => "main",
            Profile::High => "high",
        };
        let preset = match c.preset {
            Preset::Speed => "speed",
            Preset::Balanced => "balanced",
            Preset::Quality => "quality",
        };
        let level = self.sps.level_idc;
        format!(
            "{rate} profile={profile} level={}.{} preset={preset} keyint={} bframes={} refs={} cabac={} 8x8dct={} slices={} aq={:.2} scenecut={}",
            level / 10,
            level % 10,
            c.keyint,
            c.bframes,
            self.max_refs,
            u8::from(c.profile != Profile::Baseline),
            u8::from(c.profile == Profile::High),
            self.slices,
            c.aq_strength,
            u8::from(c.scenecut),
        )
    }

    pub fn new(mut cfg: EncoderConfig) -> Result<Self, Error> {
        if cfg.width == 0
            || cfg.height == 0
            || !cfg.width.is_multiple_of(2)
            || !cfg.height.is_multiple_of(2)
            || cfg.width > 16384
            || cfg.height > 16384
        {
            return Err(Error::InvalidDimensions(cfg.width, cfg.height));
        }
        if cfg.fps_num == 0 || cfg.fps_den == 0 {
            return Err(Error::InvalidFrameRate(cfg.fps_num, cfg.fps_den));
        }
        if cfg.bframes > 3 {
            return Err(Error::InvalidConfig("bframes must be 0..=3".into()));
        }
        if cfg.profile == Profile::Baseline {
            cfg.bframes = 0;
        }
        cfg.keyint = cfg.keyint.max(1);
        let mbw = cfg.width.div_ceil(16) as usize;
        let mbh = cfg.height.div_ceil(16) as usize;
        let fps = cfg.fps_num as f64 / cfg.fps_den as f64;
        let cabac = cfg.profile != Profile::Baseline;
        let high = cfg.profile == Profile::High;
        let params = match cfg.preset {
            Preset::Speed => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i4x4: true,
                i8x8: false,
                partitions: false,
                me_range: 8,
                subpel: 2,
                subpel_rounds: 0,
                rd: false,
                chroma_qp_offset: 0,
                decimate: true,
                adaptive_t8: false,
                always_intra: false,
                cavlc_clamp: !cabac,
            },
            Preset::Balanced => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i4x4: true,
                i8x8: high,
                partitions: true,
                me_range: 16,
                subpel: 2,
                subpel_rounds: 1,
                rd: false,
                chroma_qp_offset: 0,
                decimate: true,
                adaptive_t8: high,
                always_intra: false,
                cavlc_clamp: !cabac,
            },
            Preset::Quality => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i4x4: true,
                i8x8: high,
                partitions: true,
                me_range: 24,
                subpel: 2,
                subpel_rounds: 2,
                rd: true,
                chroma_qp_offset: 0,
                decimate: true,
                adaptive_t8: high,
                always_intra: true,
                cavlc_clamp: !cabac,
            },
        };
        let max_refs = if cfg.bframes > 0 { 2 } else { 1 };
        let (kbps_for_level, rc_kind, vbv) = match &cfg.rate {
            RateControl::Qp(q) => (None, RcKind::Qp((*q).min(51)), (None, None)),
            RateControl::Crf(c) => (
                Some(crf_level_kbps(*c, mbw, mbh, fps)),
                RcKind::Crf(c.clamp(0.0, 51.0)),
                (None, None),
            ),
            RateControl::Cbr { kbps } => (
                Some(*kbps),
                RcKind::Abr { kbps: *kbps },
                (Some(*kbps), Some(*kbps)),
            ),
            RateControl::Vbr {
                target_kbps,
                max_kbps,
            } => {
                let buffer = max_kbps.checked_mul(2).ok_or_else(|| {
                    Error::InvalidConfig(
                        "VBR buffer rate exceeds the supported integer range".into(),
                    )
                })?;
                (
                    Some(*max_kbps),
                    RcKind::Abr { kbps: *target_kbps },
                    (Some(*max_kbps), Some(buffer)),
                )
            }
        };
        if matches!(
            cfg.rate,
            RateControl::Cbr { kbps: 0 } | RateControl::Vbr { target_kbps: 0, .. }
        ) {
            return Err(Error::InvalidConfig("bitrate must be non-zero".into()));
        }
        let auto = nal::pick_level(
            mbw as u32,
            mbh as u32,
            fps,
            max_refs as u32,
            kbps_for_level,
            high,
        );
        let level = cfg.level.map_or(auto, |l| nal::level_at_least(l, auto));
        let (profile_idc, constraint) = match cfg.profile {
            Profile::Baseline => (66u8, 0xC0u8),
            Profile::Main => (77, 0x40),
            Profile::High => (100, 0x00),
        };
        let reorder = if cfg.bframes > 0 { 1 } else { 0 };
        let sps = Sps {
            profile_idc,
            constraint_flags: constraint,
            level_idc: level,
            width_mbs: mbw as u32,
            height_mbs: mbh as u32,
            crop_right: mbw as u32 * 16 - cfg.width,
            crop_bottom: mbh as u32 * 16 - cfg.height,
            log2_max_frame_num: LOG2_MAX_FRAME_NUM,
            log2_max_poc_lsb: LOG2_MAX_POC_LSB,
            max_num_ref_frames: max_refs as u32,
            vui: Vui {
                sar: cfg.sar,
                video_full_range: cfg.color.full_range,
                colour_primaries: cfg.color.primaries,
                transfer: cfg.color.transfer,
                matrix: cfg.color.matrix,
                num_units_in_tick: cfg.fps_den,
                time_scale: cfg.fps_num.saturating_mul(2),
                max_num_reorder_frames: reorder,
                max_dec_frame_buffering: max_refs as u32,
            },
        };
        let pps = Pps {
            cabac,
            pic_init_qp: 26,
            chroma_qp_offset: params.chroma_qp_offset,
            transform_8x8: params.t8x8,
            high,
        };
        let sps_nal = nal::nal(3, NAL_SPS, &sps.rbsp());
        let pps_nal = nal::nal(3, NAL_PPS, &pps.rbsp());
        let threads = if cfg.threads == 0 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            cfg.threads
        };
        let slices = if cfg.slices > 0 {
            cfg.slices.min(mbh)
        } else {
            threads.min(mbh.div_ceil(4)).max(1)
        };
        let pool = Pool {
            pool: if threads > 1 {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .ok()
            } else {
                None
            },
        };
        let rc = ratecontrol::RateControl::new(rc_kind, fps, vbv.0, vbv.1);
        Ok(Encoder {
            params,
            sps,
            sps_nal,
            pps_nal,
            qt: QuantTables::new(),
            cost_tab: CostTable::new(),
            lam: Lambdas::new(),
            cabac_i: Box::new(tables::cabac_init_i()),
            cabac_pb: Box::new(tables::cabac_init_pb0()),
            rc,
            pending: VecDeque::new(),
            prev_lowres: None,
            display_count: 0,
            last_idr: 0,
            idr_count: 0,
            next_frame_num: 0,
            dpb: Vec::new(),
            coded_count: 0,
            display_pts: VecDeque::new(),
            dts_offset: None,
            all_pts: Vec::new(),
            held: Vec::new(),
            frame_pool: Vec::new(),
            hpel_pool: Vec::new(),
            sei_sent: false,
            slices,
            pool,
            ref_uid: 0,
            max_refs,
            cfg,
        })
    }

    pub fn sps_pps(&self) -> (Vec<u8>, Vec<u8>) {
        (self.sps_nal.clone(), self.pps_nal.clone())
    }

    pub fn avcc(&self) -> Vec<u8> {
        nal::avcc(
            &self.sps_nal,
            &self.pps_nal,
            self.sps.profile_idc,
            self.sps.constraint_flags,
            self.sps.level_idc,
        )
    }

    pub fn encode(&mut self, frame: &YuvFrame, pts: i64) -> Result<Vec<Packet>, Error> {
        self.try_encode(frame, pts)
    }

    pub fn try_encode(&mut self, frame: &YuvFrame, pts: i64) -> Result<Vec<Packet>, Error> {
        let src = self.import(frame)?;
        let use_aq = self.cfg.aq_strength > 0.0 && !matches!(self.cfg.rate, RateControl::Qp(_));
        let (mbw, mbh, strength) = (self.params.mbw, self.params.mbh, self.cfg.aq_strength);
        let prev = self.prev_lowres.take();
        let (lowres, cplx, aq) = self.pool.install(|| {
            let lowres = LowRes::new(&src);
            let cplx = lookahead::complexity(&lowres, prev.as_ref());
            let aq = if use_aq {
                lookahead::aq_offsets(&src, mbw, mbh, strength)
            } else {
                Vec::new()
            };
            (lowres, cplx, aq)
        });
        self.prev_lowres = Some(lowres);
        let display = self.display_count;
        self.display_count += 1;
        self.display_pts.push_back(pts);
        if self.all_pts.len() < 2 {
            self.all_pts.push(pts);
        }
        let input = Input {
            frame: src,
            pts,
            display,
            cplx,
            aq,
        };
        let since = display - self.last_idr;
        let fps = self.cfg.fps_num as f64 / self.cfg.fps_den as f64;
        let min_gap = (fps / 2.0).max(1.0) as u64;
        let scenecut = self.cfg.scenecut
            && display > 0
            && since >= min_gap
            && cplx.inter as f64 > 0.75 * cplx.intra as f64;
        let idr = display == 0 || since >= self.cfg.keyint as u64 || scenecut;
        let mut out = Vec::new();
        if idr {
            self.flush_minigop(&mut out);
            self.last_idr = display;
            self.encode_batch(vec![(input, SliceType::I, true)], &mut out);
        } else {
            self.pending.push_back(input);
            if self.pending.len() > self.cfg.bframes as usize {
                self.flush_minigop(&mut out);
            }
        }
        Ok(self.release(out, false))
    }

    pub fn flush(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        self.flush_minigop(&mut out);
        self.release(out, true)
    }

    fn release(&mut self, out: Vec<Packet>, flushing: bool) -> Vec<Packet> {
        self.held.extend(out);
        if self.dts_offset.is_none() {
            if self.cfg.bframes == 0 {
                self.dts_offset = Some(0);
            } else if self.all_pts.len() >= 2 {
                let mut v = self.all_pts.clone();
                v.sort_unstable();
                self.dts_offset = Some(v[1] - v[0]);
            } else if flushing {
                self.dts_offset = Some(0);
            } else {
                return Vec::new();
            }
        }
        let off = self.dts_offset.unwrap_or(0);
        let mut res = std::mem::take(&mut self.held);
        for p in res.iter_mut() {
            p.dts -= off;
        }
        res
    }

    fn flush_minigop(&mut self, out: &mut Vec<Packet>) {
        let Some(p) = self.pending.pop_back() else {
            return;
        };
        let bs: Vec<(Input, SliceType, bool)> = self
            .pending
            .drain(..)
            .map(|b| (b, SliceType::B, false))
            .collect();
        self.encode_batch(vec![(p, SliceType::P, false)], out);
        if !bs.is_empty() {
            self.encode_batch(bs, out);
        }
    }

    fn import(&mut self, f: &YuvFrame) -> Result<Frame, Error> {
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        if f.y_stride < w
            || f.uv_stride < cw
            || f.y.len() < f.y_stride * (h - 1) + w
            || f.u.len() < f.uv_stride * (ch - 1) + cw
            || f.v.len() < f.uv_stride * (ch - 1) + cw
        {
            return Err(Error::BadFrame);
        }
        let mut fr = self.new_frame();
        for (pl, src, stride, pw, ph) in [
            (&mut fr.y, f.y, f.y_stride, w, h),
            (&mut fr.u, f.u, f.uv_stride, cw, ch),
            (&mut fr.v, f.v, f.uv_stride, cw, ch),
        ] {
            let aw = pl.w;
            for y in 0..pl.h {
                let sy = y.min(ph - 1);
                let o = pl.idx(0, y as isize);
                pl.data[o..o + pw].copy_from_slice(&src[sy * stride..sy * stride + pw]);
                let last = pl.data[o + pw - 1];
                pl.data[o + pw..o + aw].fill(last);
            }
        }
        Ok(fr)
    }

    fn new_frame(&mut self) -> Frame {
        self.frame_pool
            .pop()
            .unwrap_or_else(|| Frame::new(self.params.mbw * 16, self.params.mbh * 16))
    }

    fn recycle(&mut self, f: Frame) {
        if self.frame_pool.len() < 8 {
            self.frame_pool.push(f);
        }
    }

    fn encode_batch(&mut self, pics: Vec<(Input, SliceType, bool)>, out: &mut Vec<Packet>) {
        struct Job {
            input: Input,
            st: SliceType,
            idr: bool,
            qpf: f64,
            qp: u8,
            cplx: f64,
            aq: Vec<i8>,
            hdr: SliceHeader,
            nal_ref_idc: u8,
            l0: Option<usize>,
            l1: Option<usize>,
            poc: i32,
            rec: Option<Frame>,
            mbs: Vec<MbInfo>,
            results: Vec<SliceOut>,
        }
        let mut jobs: Vec<Job> = Vec::with_capacity(pics.len());
        for (input, mut st, idr) in pics {
            if idr {
                let old: Vec<RefPic> = std::mem::take(&mut self.dpb);
                for r in old {
                    self.recycle_ref(r);
                }
                self.next_frame_num = 0;
            }
            let poc = 2 * (input.display as i64 - self.last_idr as i64) as i32;
            let by_id = self
                .dpb
                .iter()
                .enumerate()
                .max_by_key(|(_, r)| r.id)
                .map(|(i, _)| i);
            let (mut l0, mut l1) = match st {
                SliceType::I => (None, None),
                SliceType::P => (by_id, None),
                SliceType::B => (
                    self.dpb
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.poc < poc)
                        .max_by_key(|(_, r)| r.poc)
                        .map(|(i, _)| i),
                    self.dpb
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| r.poc > poc)
                        .min_by_key(|(_, r)| r.poc)
                        .map(|(i, _)| i),
                ),
            };
            if st == SliceType::B && (l0.is_none() || l1.is_none()) {
                st = SliceType::P;
                l0 = by_id;
                l1 = None;
            }
            if st == SliceType::P && l0.is_none() {
                st = SliceType::I;
            }
            let is_ref = st != SliceType::B;
            let cplx = match st {
                SliceType::I => input.cplx.intra,
                _ => input.cplx.inter,
            } as f64;
            let qpf = self.rc.frame_qp(st, cplx);
            let qp = qpf.round().clamp(0.0, 51.0) as u8;
            let aq: Vec<i8> = if input.aq.is_empty() {
                Vec::new()
            } else {
                input
                    .aq
                    .iter()
                    .map(|&o| ((qpf + o as f64).round().clamp(0.0, 51.0) as i32 - qp as i32) as i8)
                    .collect()
            };
            let nal_ref_idc = if idr {
                3
            } else if is_ref {
                2
            } else {
                0
            };
            let hdr = SliceHeader {
                first_mb: 0,
                slice_type: st,
                nal_ref_idc,
                idr,
                idr_pic_id: self.idr_count % 2,
                frame_num: self.next_frame_num % (1 << LOG2_MAX_FRAME_NUM),
                log2_max_frame_num: LOG2_MAX_FRAME_NUM,
                poc_lsb: (poc as u32) % (1 << LOG2_MAX_POC_LSB),
                log2_max_poc_lsb: LOG2_MAX_POC_LSB,
                l0_modification: None,
                cabac: self.params.cabac,
                slice_qp: qp as i32,
                pic_init_qp: 26,
                disable_deblocking: if self.slices > 1 { 2 } else { 0 },
                alpha_offset_div2: 0,
                beta_offset_div2: 0,
            };
            let rec = self.new_frame();
            let mbs = vec![MbInfo::default(); self.params.mbw * self.params.mbh];
            jobs.push(Job {
                input,
                st,
                idr,
                qpf,
                qp,
                cplx,
                aq,
                hdr,
                nal_ref_idc,
                l0,
                l1,
                poc,
                rec: Some(rec),
                mbs,
                results: Vec::new(),
            });
        }

        let (mbw, mbh) = (self.params.mbw, self.params.mbh);
        let n = self.slices;
        let bounds: Vec<(usize, usize)> = (0..n)
            .map(|s| (s * mbh / n, (s + 1) * mbh / n))
            .filter(|(a, b)| b > a)
            .collect();
        let mut slice_results: Vec<Vec<SliceOut>> = (0..jobs.len()).map(|_| Vec::new()).collect();
        let mut bufs: Vec<(Frame, Vec<MbInfo>)> = jobs
            .iter_mut()
            .filter_map(|j| Some((j.rec.take()?, std::mem::take(&mut j.mbs))))
            .collect();
        {
            let this = &*self;
            let fencs: Vec<FrameEnc> = jobs
                .iter()
                .map(|j| FrameEnc {
                    p: &this.params,
                    qt: &this.qt,
                    cost_tab: &this.cost_tab,
                    st: j.hdr.slice_type,
                    src: &j.input.frame,
                    l0: j.l0.map(|i| &this.dpb[i]),
                    l1: j.l1.map(|i| &this.dpb[i]),
                    qp: j.qp,
                    aq: &j.aq,
                    hdr: j.hdr.clone(),
                    nal_ref_idc: j.nal_ref_idc,
                    nal_type: if j.idr { NAL_IDR } else { NAL_SLICE },
                    cabac_table: if j.hdr.slice_type == SliceType::I {
                        &this.cabac_i
                    } else {
                        &this.cabac_pb
                    },
                    lam: &this.lam,
                })
                .collect();
            let dps: Vec<DeblockParams> = jobs
                .iter()
                .map(|j| DeblockParams {
                    alpha_offset: 0,
                    beta_offset: 0,
                    chroma_qp_offset: this.params.chroma_qp_offset,
                    ref_ids: [
                        j.l0.map_or(-1, |i| this.dpb[i].id as i32),
                        j.l1.map_or(-1, |i| this.dpb[i].id as i32),
                    ],
                })
                .collect();
            let deblock: Vec<bool> = jobs
                .iter()
                .map(|j| j.hdr.slice_type != SliceType::B)
                .collect();
            type Task<'t> = (
                usize,
                usize,
                usize,
                usize,
                &'t mut [u8],
                &'t mut [u8],
                &'t mut [u8],
                &'t mut [MbInfo],
            );
            let mut tasks: Vec<Task> = Vec::new();
            for (pi, (rec, mbs)) in bufs.iter_mut().enumerate() {
                let ys = rec.y.stride;
                let cs = rec.u.stride;
                let mut yrest = &mut rec.y.data[LUMA_PAD * ys..];
                let mut urest = &mut rec.u.data[CHROMA_PAD * cs..];
                let mut vrest = &mut rec.v.data[CHROMA_PAD * cs..];
                let mut mrest = &mut mbs[..];
                for (k, &(r0, r1)) in bounds.iter().enumerate() {
                    let rows = r1 - r0;
                    let (a, b) = std::mem::take(&mut yrest).split_at_mut(rows * 16 * ys);
                    yrest = b;
                    let (ua, ub) = std::mem::take(&mut urest).split_at_mut(rows * 8 * cs);
                    urest = ub;
                    let (va, vb) = std::mem::take(&mut vrest).split_at_mut(rows * 8 * cs);
                    vrest = vb;
                    let (ma, mb) = std::mem::take(&mut mrest).split_at_mut(rows * mbw);
                    mrest = mb;
                    tasks.push((pi, k, r0, r1, a, ua, va, ma));
                }
            }
            let ys = jobs_stride_y(mbw);
            let cs = jobs_stride_c(mbw);
            let run = |(pi, k, r0, r1, y, u, v, m): Task| -> (usize, usize, SliceOut) {
                let out = SliceEnc::new(
                    &fencs[pi],
                    Band {
                        y: &mut *y,
                        u: &mut *u,
                        v: &mut *v,
                        ys,
                        cs,
                    },
                    &mut *m,
                    r0,
                    r1,
                    k as u16,
                )
                .encode();
                if deblock[pi] {
                    let mut by = BandPlane {
                        data: y,
                        stride: ys,
                        pad: LUMA_PAD,
                    };
                    let mut bu = BandPlane {
                        data: u,
                        stride: cs,
                        pad: CHROMA_PAD,
                    };
                    let mut bv = BandPlane {
                        data: v,
                        stride: cs,
                        pad: CHROMA_PAD,
                    };
                    deblock_band(&mut by, &mut bu, &mut bv, m, mbw, r1 - r0, &dps[pi]);
                }
                (pi, k, out)
            };
            let results: Vec<(usize, usize, SliceOut)> = {
                use rayon::prelude::*;
                if tasks.len() > 1 {
                    this.pool
                        .install(|| tasks.into_par_iter().map(run).collect())
                } else {
                    tasks.into_iter().map(run).collect()
                }
            };
            let mut results = results;
            results.sort_by_key(|r| (r.0, r.1));
            for (pi, _, r) in results {
                slice_results[pi].push(r);
            }
        }
        for ((j, (rec, mbs)), res) in jobs.iter_mut().zip(bufs).zip(slice_results) {
            j.rec = Some(rec);
            j.mbs = mbs;
            j.results = res;
        }

        for job in jobs {
            let Job {
                input,
                st,
                idr,
                qpf,
                qp: _,
                cplx,
                hdr: _,
                nal_ref_idc: _,
                l0: _,
                l1: _,
                poc,
                rec,
                mbs,
                results,
                aq: _,
            } = job;
            let Some(mut rec) = rec else { continue };
            let is_ref = st != SliceType::B;
            let annexb = self.cfg.format == PacketFormat::AnnexB;
            let mut nals: Vec<Vec<u8>> = Vec::new();
            if annexb && self.cfg.aud {
                let t = match st {
                    SliceType::I => 0,
                    SliceType::P => 1,
                    SliceType::B => 2,
                };
                nals.push(nal::nal(0, NAL_AUD, &nal::aud_rbsp(t)));
            }
            if idr && annexb {
                nals.push(self.sps_nal.clone());
                nals.push(self.pps_nal.clone());
            }
            if !self.sei_sent {
                self.sei_sent = true;
                let text = format!(
                    "pito-codecs {} - H.264 encoder - options: {}",
                    env!("CARGO_PKG_VERSION"),
                    self.options()
                );
                nals.push(nal::nal(0, NAL_SEI, &nal::sei_user_data_rbsp(&text)));
                if self.cfg.mastering_display.is_some() || self.cfg.content_light.is_some() {
                    nals.push(nal::nal(
                        0,
                        NAL_SEI,
                        &nal::sei_hdr_rbsp(
                            self.cfg.mastering_display.as_ref(),
                            self.cfg.content_light,
                        ),
                    ));
                }
            }
            for r in results {
                nals.push(r.nal);
            }
            let mut data = Vec::with_capacity(nals.iter().map(|n| n.len() + 4).sum());
            for n in &nals {
                if annexb {
                    data.extend_from_slice(&[0, 0, 0, 1]);
                } else {
                    data.extend_from_slice(&(n.len() as u32).to_be_bytes());
                }
                data.extend_from_slice(n);
            }
            let total_bits = data.len() as u64 * 8;
            self.rc.update(st, qpf, cplx, total_bits);
            if is_ref {
                let hp = self.hpel_pool.pop();
                let hpel = self.pool.install(|| {
                    rec.extend_edges();
                    build_hpel(&rec.y, hp)
                });
                self.ref_uid += 1;
                let rp = RefPic {
                    frame: rec,
                    hpel,
                    mbs,
                    poc,
                    id: self.ref_uid,
                };
                if self.dpb.len() >= self.max_refs
                    && let Some(oldest) = self
                        .dpb
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, r)| r.id)
                        .map(|(i, _)| i)
                {
                    let r = self.dpb.remove(oldest);
                    self.recycle_ref(r);
                }
                self.dpb.push(rp);
                self.next_frame_num = (self.next_frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM);
            } else {
                self.recycle(rec);
            }
            if idr {
                self.idr_count += 1;
            }
            let pos = self
                .display_pts
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| **p)
                .map(|(i, _)| i);
            let dts_base = pos
                .and_then(|p| self.display_pts.remove(p))
                .unwrap_or(input.pts);
            self.coded_count += 1;
            self.recycle(input.frame);
            out.push(Packet {
                data,
                pts: input.pts,
                dts: dts_base,
                keyframe: idr,
            });
        }
    }

    fn recycle_ref(&mut self, r: RefPic) {
        self.recycle(r.frame);
        if self.hpel_pool.len() < 4 {
            self.hpel_pool.push(r.hpel);
        }
    }
}

fn jobs_stride_y(mbw: usize) -> usize {
    mbw * 16 + 2 * LUMA_PAD
}

fn jobs_stride_c(mbw: usize) -> usize {
    mbw * 8 + 2 * CHROMA_PAD
}
