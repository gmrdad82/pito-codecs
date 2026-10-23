mod cabac;
mod cabac_mb;
mod cavlc;
mod deblock;
mod dsp;
mod frame;
mod intra;
mod lookahead;
mod mbinfo;
mod mbtree;
mod nal;
mod picture;
mod ratecontrol;
mod residual;
mod slice;
mod syntax;
mod tables;
mod transform;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use cabac::CostTable;
use deblock::{DeblockParams, MAX_REFS};
use frame::{Ctx, Job, Scratch, encode_frame};
use lookahead::{Complexity, LowRes};
use nal::{
    NAL_AUD, NAL_IDR, NAL_PPS, NAL_SEI, NAL_SLICE, NAL_SPS, Pps, SliceHeader, SliceType, Sps, Vui,
};
use picture::{DONE, Frame, Pic};
use ratecontrol::RcKind;
use slice::{EncParams, Lambdas, SliceOut};

use transform::QuantTables;

#[derive(Debug)]
pub enum Error {
    InvalidDimensions(u32, u32),
    InvalidFrameRate(u32, u32),
    InvalidConfig(String),
    BadFrame,
    Failed(String),
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
            Error::Failed(s) => write!(f, "{s}"),
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
    pub threads: usize,
    pub format: PacketFormat,
    pub color: crate::media::color::Color,
}

const AQ_STRENGTH: f32 = 0.6;

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
            threads: 0,
            format: PacketFormat::AnnexB,
            color: crate::media::color::Color::BT709,
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
    low: Arc<LowRes>,
    pts: i64,
    display: u64,
    cplx: Complexity,
    aq: Vec<f32>,
}

struct La {
    input: Input,
    st: SliceType,
    idr: bool,
    base: u64,
    costs: Option<mbtree::Costs>,
    refs: [Option<u64>; 2],
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

struct Done {
    job: Job,
    out: std::thread::Result<SliceOut>,
}

struct Queue {
    jobs: Mutex<(VecDeque<Job>, bool)>,
    cv: Condvar,
}

struct Workers {
    queue: Arc<Queue>,
    done: mpsc::Receiver<Done>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl Workers {
    fn new(ctx: Arc<Ctx>, n: usize) -> Result<Self, Error> {
        let queue = Arc::new(Queue {
            jobs: Mutex::new((VecDeque::new(), false)),
            cv: Condvar::new(),
        });
        let (tx, done) = mpsc::channel();
        let mut workers = Workers {
            queue: queue.clone(),
            done,
            handles: Vec::with_capacity(n),
        };
        for i in 0..n {
            let (ctx, queue, tx) = (ctx.clone(), queue.clone(), tx.clone());
            let h = std::thread::Builder::new()
                .name(format!("h264enc-{i}"))
                .spawn(move || {
                    let mut scratch = Scratch::new();
                    loop {
                        let job = {
                            let mut g = queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
                            loop {
                                if let Some(j) = g.0.pop_front() {
                                    break Some(j);
                                }
                                if g.1 {
                                    break None;
                                }
                                g = queue.cv.wait(g).unwrap_or_else(|e| e.into_inner());
                            }
                        };
                        let Some(job) = job else { return };
                        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            encode_frame(&ctx, &job, &mut scratch)
                        }));
                        if out.is_err() {
                            job.pic.publish(DONE, DONE, DONE);
                        }
                        if tx.send(Done { job, out }).is_err() {
                            return;
                        }
                    }
                })
                .map_err(|e| Error::Failed(format!("cannot start a frame thread: {e}")))?;
            workers.handles.push(h);
        }
        Ok(workers)
    }

    fn push(&self, job: Job) {
        let mut g = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
        g.0.push_back(job);
        self.queue.cv.notify_one();
    }
}

impl Drop for Workers {
    fn drop(&mut self) {
        {
            let mut g = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
            g.1 = true;
            self.queue.cv.notify_all();
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

pub struct Encoder {
    cfg: EncoderConfig,
    ctx: Arc<Ctx>,
    mbw: usize,
    mbh: usize,
    sps: Sps,
    sps_nal: Vec<u8>,
    pps_nal: Vec<u8>,
    rc: ratecontrol::RateControl,
    pending: VecDeque<Input>,
    prev_lowres: Option<Arc<LowRes>>,
    la: VecDeque<La>,
    la_depth: usize,
    la_next: u64,
    anchor: Option<(u64, Arc<LowRes>)>,

    display_count: u64,
    last_idr: u64,
    idr_count: u32,
    next_frame_num: u32,
    dpb: Vec<Arc<Pic>>,
    retired: Vec<Arc<Pic>>,
    pic_pool: Vec<Pic>,
    display_pts: VecDeque<i64>,
    dts_offset: Option<i64>,
    all_pts: Vec<i64>,
    held: Vec<Packet>,
    frame_pool: Vec<Frame>,
    sei_sent: bool,
    pool: Pool,
    ref_uid: u32,
    max_refs: usize,
    seq: u64,
    next_out: u64,
    finished: BTreeMap<u64, Done>,
    max_in_flight: usize,
    failed: Option<String>,
    workers: Workers,
}

const LOG2_MAX_FRAME_NUM: u32 = 8;
const LOOKAHEAD: usize = 40;
const LOOKAHEAD_BYTES: usize = 768 << 20;
const FLIGHT_BYTES: usize = 1 << 30;
const LOG2_MAX_POC_LSB: u32 = 8;

pub struct Budget {
    pub frame_threads: usize,
    pub la_depth: usize,
    pub bytes: u64,
}

pub fn threads_of(threads: usize) -> usize {
    if threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        threads
    }
}

pub fn budget(mbw: usize, mbh: usize, threads: usize, qp: bool) -> Budget {
    let frame_bytes = (mbw * 16 + 128) * (mbh * 16 + 128) * 2;
    let frame_threads = threads
        .clamp(1, 64)
        .min((FLIGHT_BYTES / (3 * frame_bytes)).max(2));
    let la_depth = if qp {
        0
    } else {
        (LOOKAHEAD_BYTES / frame_bytes).clamp(1, LOOKAHEAD)
    };
    let bytes = ((frame_threads + 4) * 3 + la_depth) as u64 * frame_bytes as u64;
    Budget {
        frame_threads,
        la_depth,
        bytes,
    }
}

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
            RateControl::Qp(q) => format!("rc=cqp qp={}", q.min(51)),
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
            "{rate} profile={profile} level={}.{} preset={preset} keyint={} bframes={} refs={} cabac={} 8x8dct={} slices=1 aq={AQ_STRENGTH:.2} scenecut=1",
            level / 10,
            level % 10,
            c.keyint,
            c.bframes,
            self.max_refs,
            u8::from(c.profile != Profile::Baseline),
            u8::from(c.profile == Profile::High),
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
        if matches!(cfg.rate, RateControl::Vbr { max_kbps: 0, .. }) {
            return Err(Error::InvalidConfig(
                "maximum bitrate must be non-zero".into(),
            ));
        }
        let top = nal::max_kbps(high);
        match &mut cfg.rate {
            RateControl::Cbr { kbps } => *kbps = (*kbps).min(top),
            RateControl::Vbr {
                target_kbps,
                max_kbps,
            } => {
                *max_kbps = (*max_kbps).min(top);
                *target_kbps = (*target_kbps).min(*max_kbps);
            }
            _ => {}
        }
        let params = match cfg.preset {
            Preset::Speed => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i8x8: false,
                partitions: false,
                me_range: 8,
                subpel_rounds: 0,
                rd: false,
                chroma_qp_offset: 0,
                adaptive_t8: false,
                always_intra: false,
            },
            Preset::Balanced => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i8x8: high,
                partitions: true,
                me_range: 16,
                subpel_rounds: 1,
                rd: true,
                chroma_qp_offset: 0,
                adaptive_t8: high,
                always_intra: false,
            },
            Preset::Quality => EncParams {
                mbw,
                mbh,
                cabac,
                t8x8: high,
                i8x8: high,
                partitions: true,
                me_range: 24,
                subpel_rounds: 2,
                rd: true,
                chroma_qp_offset: 0,
                adaptive_t8: high,
                always_intra: true,
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
            } => (
                Some(*max_kbps),
                RcKind::Abr { kbps: *target_kbps },
                (Some(*max_kbps), Some(*max_kbps * 2)),
            ),
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
        let (profile_idc, constraint) = match cfg.profile {
            Profile::Baseline => (66u8, 0xC0u8),
            Profile::Main => (77, 0x40),
            Profile::High => (100, 0x00),
        };
        let reorder = if cfg.bframes > 0 { 1 } else { 0 };
        let sps = Sps {
            profile_idc,
            constraint_flags: constraint,
            level_idc: auto,
            width_mbs: mbw as u32,
            height_mbs: mbh as u32,
            crop_right: mbw as u32 * 16 - cfg.width,
            crop_bottom: mbh as u32 * 16 - cfg.height,
            log2_max_frame_num: LOG2_MAX_FRAME_NUM,
            log2_max_poc_lsb: LOG2_MAX_POC_LSB,
            max_num_ref_frames: max_refs as u32,
            vui: Vui {
                num_units_in_tick: cfg.fps_den,
                time_scale: cfg.fps_num.saturating_mul(2),
                max_num_reorder_frames: reorder,
                max_dec_frame_buffering: max_refs as u32,
                color: cfg.color,
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
        let threads = threads_of(cfg.threads);
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
        let ctx = Arc::new(Ctx {
            p: params,
            qt: QuantTables::new(),
            cost_tab: CostTable::new(),
            lam: Lambdas::new(),
        });
        let Budget {
            frame_threads,
            la_depth,
            ..
        } = budget(mbw, mbh, threads, matches!(cfg.rate, RateControl::Qp(_)));
        let workers = Workers::new(ctx.clone(), frame_threads)?;
        Ok(Encoder {
            ctx,
            mbw,
            mbh,
            sps,
            sps_nal,
            pps_nal,
            rc,
            pending: VecDeque::new(),
            prev_lowres: None,
            la: VecDeque::new(),
            la_depth,
            la_next: 0,
            anchor: None,

            display_count: 0,
            last_idr: 0,
            idr_count: 0,
            next_frame_num: 0,
            dpb: Vec::new(),
            retired: Vec::new(),
            pic_pool: Vec::new(),
            display_pts: VecDeque::new(),
            dts_offset: None,
            all_pts: Vec::new(),
            held: Vec::new(),
            frame_pool: Vec::new(),
            sei_sent: false,
            pool,
            ref_uid: 0,
            max_refs,
            seq: 0,
            next_out: 0,
            finished: BTreeMap::new(),
            max_in_flight: frame_threads + 2,
            failed: None,
            workers,
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

    fn check(&self) -> Result<(), Error> {
        match &self.failed {
            Some(s) => Err(Error::Failed(s.clone())),
            None => Ok(()),
        }
    }

    fn fail<T>(&mut self, r: Result<T, Error>) -> Result<T, Error> {
        if let Err(Error::Failed(s)) = &r {
            self.failed = Some(s.clone());
        }
        r
    }

    pub fn try_encode(&mut self, frame: &YuvFrame, pts: i64) -> Result<Vec<Packet>, Error> {
        self.check()?;
        let r = self.encode_inner(frame, pts);
        self.fail(r)
    }

    fn encode_inner(&mut self, frame: &YuvFrame, pts: i64) -> Result<Vec<Packet>, Error> {
        let src = self.import(frame)?;
        let use_aq = !matches!(self.cfg.rate, RateControl::Qp(_));
        let (mbw, mbh) = (self.mbw, self.mbh);
        let prev = self.prev_lowres.take();
        let (lowres, cplx, aq) = self.pool.install(|| {
            let lowres = Arc::new(LowRes::new(&src));
            let cplx = lookahead::complexity(&lowres, prev.as_deref());
            let aq = if use_aq {
                lookahead::aq_offsets(&src, mbw, mbh, AQ_STRENGTH)
            } else {
                Vec::new()
            };
            (lowres, cplx, aq)
        });
        self.prev_lowres = Some(lowres.clone());
        let display = self.display_count;
        self.display_count += 1;
        self.display_pts.push_back(pts);
        if self.all_pts.len() < 2 {
            self.all_pts.push(pts);
        }
        let input = Input {
            frame: src,
            low: lowres,
            pts,
            display,
            cplx,
            aq,
        };
        let since = display - self.last_idr;
        let keyint = self.cfg.keyint as u64;
        let min_gap = (keyint / 10).clamp(1, 25);
        let span = keyint.saturating_sub(min_gap).max(1) as f64;
        let bias = 0.01 + 0.39 * (since.saturating_sub(min_gap) as f64 / span).min(1.0);
        let scenecut = display > 0
            && since >= min_gap
            && cplx.inter as f64 >= (1.0 - bias) * cplx.intra as f64;
        let idr = display == 0 || since >= self.cfg.keyint as u64 || scenecut;
        let mut out = Vec::new();
        if idr {
            self.flush_minigop();
            self.last_idr = display;
            self.enqueue(input, SliceType::I, true);
        } else {
            self.pending.push_back(input);
            if self.pending.len() > self.cfg.bframes as usize {
                self.flush_minigop();
            }
        }
        self.drain(self.la_depth, &mut out)?;
        Ok(self.release(out, false))
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>, Error> {
        self.check()?;
        let r = self.flush_inner();
        self.fail(r)
    }

    fn flush_inner(&mut self) -> Result<Vec<Packet>, Error> {
        let mut out = Vec::new();
        self.flush_minigop();
        self.drain(0, &mut out)?;
        self.collect(0, &mut out)?;
        Ok(self.release(out, true))
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

    fn flush_minigop(&mut self) {
        let Some(p) = self.pending.pop_back() else {
            return;
        };
        let bs: Vec<Input> = self.pending.drain(..).collect();
        let prev = self.anchor.clone();
        self.enqueue(p, SliceType::P, false);
        let next = self.anchor.clone();
        for b in bs {
            self.enqueue_b(b, prev.clone(), next.clone());
        }
    }

    fn tree(&self) -> bool {
        !matches!(self.cfg.rate, RateControl::Qp(_))
    }

    fn enqueue(&mut self, input: Input, st: SliceType, idr: bool) {
        let id = self.la_next;
        self.la_next += 1;
        let l0 = if idr { None } else { self.anchor.clone() };
        let costs = self.tree().then(|| {
            let (cur, r0) = (&input.low, l0.as_ref().map(|a| &*a.1));
            self.pool.install(|| mbtree::analyse(cur, r0, None))
        });
        self.anchor = Some((id, input.low.clone()));
        self.la.push_back(La {
            input,
            st,
            idr,
            base: self.last_idr,
            costs,
            refs: [l0.map(|a| a.0), None],
        });
    }

    fn enqueue_b(
        &mut self,
        input: Input,
        prev: Option<(u64, Arc<LowRes>)>,
        next: Option<(u64, Arc<LowRes>)>,
    ) {
        self.la_next += 1;
        let costs = self.tree().then(|| {
            let cur = &input.low;
            let (r0, r1) = (prev.as_ref().map(|a| &*a.1), next.as_ref().map(|a| &*a.1));
            self.pool.install(|| mbtree::analyse(cur, r0, r1))
        });
        self.la.push_back(La {
            input,
            st: SliceType::B,
            idr: false,
            base: self.last_idr,
            costs,
            refs: [prev.map(|a| a.0), next.map(|a| a.0)],
        });
    }

    fn drain(&mut self, depth: usize, out: &mut Vec<Packet>) -> Result<(), Error> {
        while self.la.len() > depth {
            let tree = self.tree_offsets();
            let Some(mut f) = self.la.pop_front() else {
                break;
            };
            if let Some(t) = tree {
                if f.input.aq.is_empty() {
                    f.input.aq = t;
                } else {
                    for (a, o) in f.input.aq.iter_mut().zip(t) {
                        *a += o;
                    }
                }
            }
            self.submit(f.input, f.st, f.idr, f.base, out)?;
        }
        Ok(())
    }

    fn tree_offsets(&self) -> Option<Vec<f32>> {
        let first = self.la.front()?;
        first.costs.as_ref()?;
        let start = self.la_next - self.la.len() as u64;
        let nodes: Vec<mbtree::Node> = self
            .la
            .iter()
            .filter_map(|f| {
                Some(mbtree::Node {
                    costs: f.costs.as_ref()?,
                    refs: f
                        .refs
                        .map(|r| r.and_then(|id| id.checked_sub(start)).map(|i| i as usize)),
                })
            })
            .collect();
        if nodes.len() != self.la.len() {
            return None;
        }
        let prop = mbtree::propagate(&nodes, self.mbw, self.mbh);
        Some(mbtree::offsets(nodes[0].costs, &prop[0]))
    }

    fn import(&mut self, f: &YuvFrame) -> Result<Frame, Error> {
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        let need = |stride: usize, rows: usize, w: usize| {
            stride.checked_mul(rows - 1).and_then(|n| n.checked_add(w))
        };
        let fits = |len: usize, stride: usize, rows: usize, w: usize| {
            stride >= w && need(stride, rows, w).is_some_and(|n| len >= n)
        };
        if !fits(f.y.len(), f.y_stride, h, w)
            || !fits(f.u.len(), f.uv_stride, ch, cw)
            || !fits(f.v.len(), f.uv_stride, ch, cw)
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
            .unwrap_or_else(|| Frame::new(self.mbw * 16, self.mbh * 16))
    }

    fn recycle(&mut self, f: Frame) {
        if self.frame_pool.len() < 8 {
            self.frame_pool.push(f);
        }
    }

    fn submit(
        &mut self,
        input: Input,
        mut st: SliceType,
        idr: bool,
        base: u64,
        out: &mut Vec<Packet>,
    ) -> Result<(), Error> {
        if idr {
            let old = std::mem::take(&mut self.dpb);
            self.retired.extend(old);
            self.next_frame_num = 0;
        }
        self.reclaim();
        let poc = 2 * (input.display as i64 - base as i64) as i32;
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
            slice_type: st,
            nal_ref_idc,
            idr,
            idr_pic_id: self.idr_count % 2,
            frame_num: self.next_frame_num % (1 << LOG2_MAX_FRAME_NUM),
            log2_max_frame_num: LOG2_MAX_FRAME_NUM,
            poc_lsb: (poc as u32) % (1 << LOG2_MAX_POC_LSB),
            log2_max_poc_lsb: LOG2_MAX_POC_LSB,
            l0_modification: None,
            cabac: self.ctx.p.cabac,
            slice_qp: qp as i32,
            pic_init_qp: 26,
            disable_deblocking: 0,
            alpha_offset_div2: 0,
            beta_offset_div2: 0,
        };
        if idr {
            self.idr_count += 1;
        }
        let refs = [
            l0.map(|i| self.dpb[i].clone()),
            l1.map(|i| self.dpb[i].clone()),
        ];
        let mut ids = [[-1i32; MAX_REFS]; 2];
        for (l, r) in refs.iter().enumerate() {
            if let Some(r) = r {
                ids[l][0] = r.id as i32;
            }
        }
        let mut pic = self
            .pic_pool
            .pop()
            .unwrap_or_else(|| Pic::new(self.mbw * 16, self.mbh * 16));
        let id = if is_ref {
            self.ref_uid += 1;
            self.ref_uid
        } else {
            0
        };
        pic.reset(poc, id);
        let pic = Arc::new(pic);
        if is_ref {
            if self.dpb.len() >= self.max_refs
                && let Some(oldest) = self
                    .dpb
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, r)| r.id)
                    .map(|(i, _)| i)
            {
                let r = self.dpb.remove(oldest);
                self.retired.push(r);
            }
            self.dpb.push(pic.clone());
            self.next_frame_num = (self.next_frame_num + 1) % (1 << LOG2_MAX_FRAME_NUM);
        }
        let job = Job {
            seq: self.seq,
            src: input.frame,
            pts: input.pts,
            st,
            idr,
            qp,
            qpf,
            cplx,
            aq,
            hdr,
            nal_ref_idc,
            nal_type: if idr { NAL_IDR } else { NAL_SLICE },
            pic,
            refs,
            publish: is_ref,
            dp: DeblockParams {
                alpha_offset: 0,
                beta_offset: 0,
                chroma_qp_offset: self.ctx.p.chroma_qp_offset,
                ref_ids: ids,
            },
        };
        self.seq += 1;
        self.workers.push(job);
        self.collect(self.max_in_flight, out)
    }

    fn reclaim(&mut self) {
        let mut i = 0;
        while i < self.retired.len() {
            if Arc::strong_count(&self.retired[i]) == 1 {
                let r = self.retired.swap_remove(i);
                if let Ok(p) = Arc::try_unwrap(r)
                    && self.pic_pool.len() < 4
                {
                    self.pic_pool.push(p);
                }
            } else {
                i += 1;
            }
        }
    }

    fn collect(&mut self, max_in_flight: usize, out: &mut Vec<Packet>) -> Result<(), Error> {
        let target = self.seq.saturating_sub(max_in_flight as u64);
        while self.next_out < target {
            if let Some(d) = self.finished.remove(&self.next_out) {
                self.next_out += 1;
                self.finish_frame(d, out)?;
                continue;
            }
            match self.workers.done.recv() {
                Ok(d) => {
                    self.finished.insert(d.job.seq, d);
                }
                Err(_) => return Err(Error::Failed("the frame threads stopped".into())),
            }
        }
        Ok(())
    }

    fn finish_frame(&mut self, d: Done, out: &mut Vec<Packet>) -> Result<(), Error> {
        let res = match d.out {
            Ok(r) => r,
            Err(e) => {
                let why = e
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| e.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown cause".into());
                return Err(Error::Failed(format!(
                    "frame {} failed in its thread: {why}",
                    d.job.seq
                )));
            }
        };
        let job = d.job;
        let st = job.st;
        let annexb = self.cfg.format == PacketFormat::AnnexB;
        let mut nals: Vec<Vec<u8>> = Vec::new();
        if annexb {
            let t = match st {
                SliceType::I => 0,
                SliceType::P => 1,
                SliceType::B => 2,
            };
            nals.push(nal::nal(0, NAL_AUD, &nal::aud_rbsp(t)));
        }
        if job.idr && annexb {
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
        }
        nals.push(res.nal);
        let mut data = Vec::with_capacity(nals.iter().map(|n| n.len() + 4).sum());
        for n in &nals {
            if annexb {
                data.extend_from_slice(&[0, 0, 0, 1]);
            } else {
                data.extend_from_slice(&(n.len() as u32).to_be_bytes());
            }
            data.extend_from_slice(n);
        }
        self.rc.update(st, job.qpf, job.cplx, data.len() as u64 * 8);
        let pos = self
            .display_pts
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| **p)
            .map(|(i, _)| i);
        let dts_base = pos
            .and_then(|p| self.display_pts.remove(p))
            .unwrap_or(job.pts);
        let Job {
            src,
            pic,
            refs,
            pts,
            idr,
            ..
        } = job;
        drop(refs);
        self.recycle(src);
        if !pic_is_ref(&pic) {
            if let Ok(p) = Arc::try_unwrap(pic)
                && self.pic_pool.len() < 4
            {
                self.pic_pool.push(p);
            }
        } else {
            drop(pic);
        }
        self.reclaim();
        out.push(Packet {
            data,
            pts,
            dts: dts_base,
            keyframe: idr,
        });
        Ok(())
    }
}

fn pic_is_ref(p: &Pic) -> bool {
    p.id != 0
}
