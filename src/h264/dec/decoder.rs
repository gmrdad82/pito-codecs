use crate::h264::bits::{BitReader, annexb_nals, length_prefixed_nals, unescape_rbsp};
use crate::h264::dec::dpb::{Dpb, Output, OutputMeta, RefMark};
use crate::h264::dec::error::{Error, Result, ensure, invalid, unsupported};
use crate::h264::dec::params::{Pps, Sps};
use crate::h264::dec::picture::{Frame, FramePool, FrameRef, MbState, Output3, Planes, RefPic};
use crate::h264::dec::slice::{
    NalHeader, Poc, PocState, SliceHeader, SliceType, is_new_picture, nal_type,
};
use crate::h264::dec::slicedec::{PicState, SliceDecoder};
use crate::h264::dec::transform::LevelScale;
use crate::h264::dec::{ColorInfo, Picture};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct SliceJob {
    sh: SliceHeader,
    pps: Arc<Pps>,
    sps: Arc<Sps>,
    rbsp: Vec<u8>,
    refs: [Vec<RefPic>; 2],
    ls: Arc<LevelScale>,
}

struct PendingPic {
    frame: FrameRef,
    sps: Arc<Sps>,
    first: SliceHeader,
    poc: Poc,
    pts: i64,
    slices: Vec<SliceJob>,
}

#[derive(Default)]
struct Shared {
    error: Mutex<Option<Error>>,
    pool: Mutex<Vec<PicState>>,
    spare: Mutex<Vec<Vec<u8>>>,
}

impl Shared {
    fn buffer(&self, len: usize) -> Vec<u8> {
        let found = self.spare.lock().ok().and_then(|mut s| {
            let i = s
                .iter()
                .position(|b| b.capacity() >= len && b.capacity() <= 2 * len)?;
            Some(s.swap_remove(i))
        });
        match found {
            Some(mut b) => {
                b.resize(len, 0);
                b
            }
            None => vec![0; len],
        }
    }

    fn buffers(&self) -> [Vec<u8>; 3] {
        let mut spare = self.spare.lock().ok();
        let mut take = || spare.as_mut().and_then(|s| s.pop()).unwrap_or_default();
        [take(), take(), take()]
    }
}

pub struct Decoder {
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb<FrameRef>,
    poc_state: PocState,
    prev_ref_frame_num: u32,
    pending: Option<PendingPic>,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    ls_cache: Vec<(Arc<Pps>, Arc<LevelScale>)>,
    out_queue: VecDeque<Output<FrameRef>>,
    shared: Arc<Shared>,
    frames: Arc<FramePool>,
    pool: Option<rayon::ThreadPool>,
    in_flight: VecDeque<FrameRef>,
    max_in_flight: usize,
    threads: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(32)
}

impl Decoder {
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    pub fn with_threads(threads: usize) -> Self {
        crate::h264::dec::cavlc::init_tables();
        let pool = if threads > 1 && cfg!(not(target_arch = "wasm32")) {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("h264-{i}"))
                .build()
                .ok()
        } else {
            None
        };
        let workers = if pool.is_some() { threads } else { 1 };
        Decoder {
            spss: vec![None; 32],
            ppss: vec![None; 256],
            nal_length_size: None,
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            prev_ref_frame_num: 0,
            pending: None,
            next_id: 1,
            active_sps: None,
            ls_cache: Vec::new(),
            out_queue: VecDeque::new(),
            shared: Arc::new(Shared::default()),
            frames: Arc::default(),
            pool,
            in_flight: VecDeque::new(),
            max_in_flight: threads.max(1) + 2,
            threads: workers,
        }
    }

    pub fn threads(&self) -> usize {
        self.threads
    }

    pub fn configure_avcc(&mut self, avcc: &[u8]) -> Result<()> {
        ensure!(avcc.len() >= 7, "avcC too short");
        ensure!(avcc[0] == 1, "unsupported avcC version {}", avcc[0]);
        self.nal_length_size = Some((avcc[4] & 3) as usize + 1);
        let mut pos = 5;
        let read_sets = |pos: &mut usize, count: usize, d: &mut Decoder| -> Result<()> {
            for _ in 0..count {
                ensure!(*pos + 2 <= avcc.len(), "avcC truncated");
                let len = u16::from_be_bytes([avcc[*pos], avcc[*pos + 1]]) as usize;
                *pos += 2;
                ensure!(*pos + len <= avcc.len(), "avcC truncated");
                d.handle_nal(&avcc[*pos..*pos + len], 0)?;
                *pos += len;
            }
            Ok(())
        };
        let nsps = (avcc[pos] & 0x1f) as usize;
        pos += 1;
        read_sets(&mut pos, nsps, self)?;
        ensure!(pos < avcc.len(), "avcC truncated");
        let npps = avcc[pos] as usize;
        pos += 1;
        read_sets(&mut pos, npps, self)
    }

    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let nals = match self.nal_length_size {
            Some(n) => length_prefixed_nals(data, n)?,
            None => annexb_nals(data),
        };
        let mut result = Ok(());
        for nal in nals {
            if let Err(e) = self.handle_nal(nal, pts) {
                result = Err(e);
                break;
            }
        }
        self.submit_pending();
        if let Some(e) = self.take_error() {
            result = result.and(Err(e));
        }
        result?;
        let max_queue = self.max_in_flight * 2;
        let mut pics = Vec::new();
        while let Some(o) = self.out_queue.front() {
            if !o.frame.output_ready() && self.out_queue.len() <= max_queue {
                break;
            }
            let Some(o) = self.out_queue.pop_front() else {
                break;
            };
            pics.push(make_picture(&o, &self.shared));
        }
        Ok(pics)
    }

    pub fn flush(&mut self) -> Vec<Picture> {
        self.submit_pending();
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.emit(outs);
        let shared = &self.shared;
        let pics = self
            .out_queue
            .drain(..)
            .map(|o| make_picture(&o, shared))
            .collect();
        while let Some(f) = self.in_flight.pop_front() {
            f.wait_complete();
        }
        pics
    }

    pub fn recycle(&mut self, buf: Vec<u8>) {
        if let Ok(mut spare) = self.shared.spare.lock()
            && spare.len() < 3 * (self.max_in_flight + 8)
            && buf.capacity() > 0
        {
            spare.push(buf);
        }
    }

    pub fn take_error(&mut self) -> Option<Error> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.is_empty() {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal[0])?;
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let sps = Sps::parse(&unescape_rbsp(&nal[1..]))?;
                let id = sps.id as usize;
                self.spss[id] = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let rbsp = unescape_rbsp(&nal[1..]);
                let spss: Vec<Option<Sps>> = self
                    .spss
                    .iter()
                    .map(|s| s.as_ref().map(|s| (**s).clone()))
                    .collect();
                let pps = Pps::parse(&rbsp, &spss)?;
                let id = pps.id as usize;
                self.ppss[id] = Some(Arc::new(pps));
            }
            nal_type::SLICE | nal_type::IDR => self.handle_slice(nal, hdr, pts)?,
            nal_type::SLICE_DPA | nal_type::SLICE_DPB | nal_type::SLICE_DPC => {
                return unsupported("data partitioning (Extended profile)");
            }
            nal_type::END_SEQ | nal_type::END_STREAM => self.submit_pending(),
            _ => {}
        }
        Ok(())
    }

    fn level_scale(&mut self, pps: &Arc<Pps>) -> Arc<LevelScale> {
        if let Some((_, ls)) = self.ls_cache.iter().find(|(p, _)| Arc::ptr_eq(p, pps)) {
            return ls.clone();
        }
        let ls: Arc<LevelScale> = Arc::from(LevelScale::new(&pps.scaling));
        self.ls_cache
            .retain(|(p, _)| self.ppss.iter().flatten().any(|q| Arc::ptr_eq(p, q)));
        self.ls_cache.push((pps.clone(), ls.clone()));
        ls
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let rbsp = unescape_rbsp(&nal[1..]);
        let ppss = &self.ppss;
        let spss = &self.spss;
        let mut found: Option<(Arc<Pps>, Arc<Sps>)> = None;
        let (sh, _, _) = SliceHeader::parse(&rbsp, hdr, |id| {
            let pps = ppss[id as usize]
                .as_ref()
                .ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
            let sps = spss[pps.sps_id as usize]
                .as_ref()
                .ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
            found = Some((pps.clone(), sps.clone()));
            Ok((&**pps, &**sps))
        })?;
        let Some((pps, sps)) = found else {
            return Err(Error::MissingParameterSet("PPS".into()));
        };
        sps.check_supported()?;
        if pps.num_slice_groups > 1 {
            return unsupported("slice groups (FMO)");
        }
        if sh.field_pic {
            return unsupported("field pictures");
        }
        if matches!(sh.slice_type, SliceType::Sp | SliceType::Si) {
            return unsupported("SP/SI slices");
        }
        if sh.redundant_pic_cnt > 0 {
            return Ok(());
        }
        let new_pic = match &self.pending {
            None => true,
            Some(p) => {
                is_new_picture(&p.first, &sh, &sps)
                    || sh.first_mb_in_slice == 0
                    || !Arc::ptr_eq(&p.sps, &sps)
            }
        };
        if new_pic {
            self.submit_pending();
            if sh.first_mb_in_slice != 0 && self.active_sps.is_none() {
                return invalid("stream does not start with the first slice of a picture");
            }
            self.start_picture(&sh, &sps, pts)?;
        }
        let ls = self.level_scale(&pps);
        let Some(pending) = self.pending.as_mut() else {
            return invalid("slice without a started picture");
        };
        let refs =
            self.dpb
                .build_ref_lists(&sh, pending.poc.frame(), sps.max_frame_num(), |e| RefPic {
                    frame: e.frame.clone(),
                    long_term: e.mark == RefMark::Long,
                })?;
        if !sh.slice_type.is_intra() {
            ensure!(
                !refs[0].is_empty(),
                "no reference pictures available for inter slice"
            );
            if sh.slice_type.is_b() {
                ensure!(!refs[1].is_empty(), "empty RefPicList1 in B slice");
            }
        }
        pending.slices.push(SliceJob {
            sh,
            pps,
            sps,
            rbsp,
            refs,
            ls,
        });
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pts: i64) -> Result<()> {
        let changed = match &self.active_sps {
            None => true,
            Some(a) => {
                a.width() != sps.width()
                    || a.height() != sps.height()
                    || a.max_dpb_frames() != sps.max_dpb_frames()
            }
        };
        if changed && self.active_sps.is_some() {
            let mut outs = Vec::new();
            self.dpb.flush(&mut outs);
            self.emit(outs);
            self.dpb.entries.clear();
        }
        self.active_sps = Some(sps.clone());
        self.dpb.capacity = sps.max_dpb_frames();
        self.dpb.max_reorder = sps.max_num_reorder_frames().min(self.dpb.capacity);
        let mb_w = sps.pic_width_in_mbs as usize;
        let mb_h = sps.frame_height_in_mbs() as usize;
        if sh.idr {
            let mut outs = Vec::new();
            self.dpb.idr(sh.no_output_of_prior_pics, &mut outs);
            self.emit(outs);
            self.prev_ref_frame_num = 0;
        } else {
            let max = sps.max_frame_num();
            if sh.frame_num != self.prev_ref_frame_num
                && sh.frame_num != (self.prev_ref_frame_num + 1) % max
            {
                let meta = Arc::new(output_meta(sps, pts));
                let next_id = &mut self.next_id;
                let poc_state = &mut self.poc_state;
                let frames = self.frames.clone();
                let last = self
                    .dpb
                    .entries
                    .iter()
                    .filter(|e| !e.non_existing && e.mark != RefMark::Unused)
                    .max_by_key(|e| e.frame.id)
                    .map(|e| e.frame.clone());
                let mut make = |_fnum: u32| {
                    let id = *next_id;
                    *next_id += 1;
                    let planes = match &last {
                        Some(f) if f.mb_w == mb_w && f.mb_h() == mb_h => {
                            let mut p = Planes::new(mb_w * 16, mb_h * 16);
                            let (y, u, v) =
                                f.copy_cropped((0, 0, mb_w * 16, mb_h * 16), Default::default());
                            p.y = y;
                            p.cb = u;
                            p.cr = v;
                            p
                        }
                        _ => Planes::gray(mb_w * 16, mb_h * 16),
                    };
                    (
                        Arc::new(Frame::from_planes(id, 0, &planes, &frames)),
                        0,
                        meta.clone(),
                    )
                };
                let mut upd = |fnum: u32| poc_state.update_gap_frame(fnum, sps);
                self.dpb.fill_frame_num_gap(
                    self.prev_ref_frame_num,
                    sh.frame_num,
                    max,
                    sps.max_num_ref_frames as usize,
                    &mut make,
                    &mut upd,
                );
                self.prev_ref_frame_num = (sh.frame_num + max - 1) % max;
            }
        }
        let poc = self.poc_state.compute(sh, sps);
        let frame_poc = if sh.has_mmco5() { 0 } else { poc.frame() };
        let id = self.next_id;
        self.next_id += 1;
        let bufs = (sh.nal_ref_idc == 0).then(|| {
            let n = mb_w * 16 * mb_h * 16;
            [
                self.shared.buffer(n),
                self.shared.buffer(n / 4),
                self.shared.buffer(n / 4),
            ]
        });
        let frame = Arc::new(Frame::new(id, frame_poc, mb_w, mb_h, &self.frames, bufs));
        self.pending = Some(PendingPic {
            frame,
            sps: sps.clone(),
            first: sh.clone(),
            poc,
            pts,
            slices: Vec::new(),
        });
        Ok(())
    }

    fn submit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let PendingPic {
            frame,
            sps,
            first,
            poc,
            pts,
            slices,
        } = p;
        self.dispatch(frame.clone(), slices);
        self.poc_state.update(&first, &poc);
        if first.nal_ref_idc != 0 {
            self.prev_ref_frame_num = if first.has_mmco5() {
                0
            } else {
                first.frame_num
            };
        }
        let meta = Arc::new(output_meta(&sps, pts));
        let mut outs = Vec::new();
        let fpoc = frame.poc;
        self.dpb.store_picture(
            &first,
            frame,
            fpoc,
            sps.max_frame_num(),
            sps.max_num_ref_frames as usize,
            meta,
            &mut outs,
        );
        self.emit(outs);
    }

    fn dispatch(&mut self, frame: FrameRef, slices: Vec<SliceJob>) {
        let shared = self.shared.clone();
        if let Some(pool) = &self.pool {
            self.in_flight.retain(|f| !f.is_published(f.mb_h() - 1));
            while self.in_flight.len() >= self.max_in_flight {
                if let Some(f) = self.in_flight.pop_front() {
                    f.wait_complete();
                }
            }
            self.in_flight.push_back(frame.clone());
            pool.spawn_fifo(move || run_job(frame, slices, &shared));
            return;
        }
        run_job(frame, slices, &shared);
    }

    fn emit(&mut self, outs: Vec<Output<FrameRef>>) {
        self.out_queue.extend(outs);
    }
}

fn run_job(frame: FrameRef, slices: Vec<SliceJob>, shared: &Shared) {
    let reuse = shared.pool.lock().ok().and_then(|mut p| p.pop());
    let mut pic = match reuse {
        Some(mut p) if p.mb_w == frame.mb_w && p.mb_h == frame.mb_h() => {
            p.reset(frame.clone());
            p
        }
        _ => PicState::new(frame.clone()),
    };
    pic.reference = slices.first().is_none_or(|s| s.sh.nal_ref_idc != 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut first_err = None;
        for s in &slices {
            if let Err(e) = decode_slice(s, &mut pic) {
                first_err.get_or_insert(e);
            }
        }
        pic.flush_rows(true);
        first_err
    }));
    let err = match result {
        Ok(e) => e,
        Err(_) => {
            let fallback = Planes::gray(frame.width, frame.height);
            let blank = vec![MbState::default(); frame.mb_w * frame.mb_h()];
            for r in 0..frame.mb_h() {
                if !frame.is_published(r) {
                    frame.fill(
                        r,
                        &fallback,
                        Frame::make_row(frame.mb_w, r, &blank, &|_, _, _| u32::MAX),
                    );
                }
            }
            pic = PicState::new(frame.clone());
            Some(Error::Invalid(
                "internal error while decoding a picture".into(),
            ))
        }
    };
    if let (Some(e), Ok(mut slot)) = (err, shared.error.lock()) {
        slot.get_or_insert(e);
    }
    if let Some(s) = slices.first() {
        let (cx, cy, cw, ch) = s.sps.crop_rect();
        let crop = (cx as usize, cy as usize, cw as usize, ch as usize);
        let taken = if !pic.reference && crop.0 == 0 && crop.1 == 0 {
            unsafe { frame.take_planes() }
        } else {
            None
        };
        let out = match taken {
            Some([y, u, v]) => Output3 {
                y,
                u,
                v,
                y_stride: frame.width,
                uv_stride: frame.cwidth,
            },
            None => {
                let (y, u, v) = frame.copy_cropped(crop, shared.buffers());
                Output3 {
                    y,
                    u,
                    v,
                    y_stride: crop.2,
                    uv_stride: crop.2.div_ceil(2),
                }
            }
        };
        frame.set_output(out);
    }
    if let Ok(mut p) = shared.pool.lock()
        && p.len() < 64
    {
        p.push(pic);
    }
}

fn decode_slice(s: &SliceJob, pic: &mut PicState) -> Result<()> {
    let mut sd = SliceDecoder::new(&s.sh, &s.pps, &s.sps, pic, &s.refs, &s.ls)?;
    if s.pps.entropy_coding_mode {
        sd.decode_cabac(&s.rbsp)
    } else {
        let mut r = BitReader::new(&s.rbsp);
        r.seek_bits(s.sh.header_bits);
        sd.decode_cavlc(&mut r)
    }
}

fn output_meta(sps: &Sps, pts: i64) -> OutputMeta {
    let vui = sps.vui.clone().unwrap_or_default();
    OutputMeta {
        pts,
        crop: sps.crop_rect(),
        full_range: vui.full_range,
        colour_primaries: vui.colour_primaries,
        transfer_characteristics: vui.transfer_characteristics,
        matrix_coefficients: vui.matrix_coefficients,
    }
}

fn make_picture(o: &Output<FrameRef>, shared: &Shared) -> Picture {
    let (cx, cy, cw, ch) = o.meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    let out = match o.frame.take_output() {
        Some(out) => out,
        None if o.frame.job_output => loop {
            if let Some(out) = o.frame.take_output() {
                break out;
            }
            std::thread::yield_now();
        },
        None => {
            let (y, u, v) = o.frame.copy_cropped((cx, cy, cw, ch), shared.buffers());
            Output3 {
                y,
                u,
                v,
                y_stride: cw,
                uv_stride: cw.div_ceil(2),
            }
        }
    };
    let Output3 {
        y,
        u,
        v,
        y_stride,
        uv_stride,
    } = out;
    let (ccw, cch) = (cw.div_ceil(2), ch.div_ceil(2));
    Picture {
        width: cw as u32,
        height: ch as u32,
        chroma_width: ccw as u32,
        chroma_height: cch as u32,
        y,
        u,
        v,
        y_stride,
        uv_stride,
        pts: o.meta.pts,
        color: ColorInfo {
            full_range: o.meta.full_range,
            primaries: o.meta.colour_primaries,
            transfer: o.meta.transfer_characteristics,
            matrix: o.meta.matrix_coefficients,
        },
    }
}
