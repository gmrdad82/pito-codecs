use crate::hevc::bits::{annexb_nals, length_prefixed_nals, unescape_rbsp, unescape_rbsp_marked};
use crate::hevc::dpb::{Dpb, Output, OutputMeta, PocState, RefPicSet, build_ref_lists};
use crate::hevc::error::{Error, Result, ensure, invalid};
use crate::hevc::params::{Layout, Pps, Sps, Vps};
use crate::hevc::pel::Pel;
use crate::hevc::picture::{Frame, FrameRef, PlanePool};
use crate::hevc::slice::{NalHeader, SliceHeader, nal_type};
use crate::hevc::slicedec::{PicState, Pool, Scaling, SliceDecoder, SliceJob, SliceRun};
use crate::hevc::{ColorInfo, Hvcc};
use crate::media::{Frame as Image, Packet, PixelFormat, Plane, TimeBase};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

type LayoutEntry = (Arc<Pps>, Arc<Sps>, Arc<Layout>, Scaling);

struct PendingPic {
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    last_sh: SliceHeader,
    first_sh: SliceHeader,
    rps: RefPicSet<FrameRef>,
    poc: i32,
    output: bool,
    meta: Arc<OutputMeta>,
    slices: Vec<SliceJob>,
}

#[derive(Default)]
struct Shared {
    error: Mutex<Option<Error>>,
    pool8: Pool<u8>,
    pool16: Pool<u16>,
    rows: PlanePool,
    images: Mutex<Vec<Vec<u8>>>,
}

impl Shared {
    fn image_buffers(&self) -> [Vec<u8>; 3] {
        let mut p = self.images.lock().unwrap_or_else(PoisonError::into_inner);
        let mut b = [
            p.pop().unwrap_or_default(),
            p.pop().unwrap_or_default(),
            p.pop().unwrap_or_default(),
        ];
        b.sort_by_key(|v| std::cmp::Reverse(v.capacity()));
        b
    }
}

pub struct Decoder {
    vpss: Vec<Option<Arc<Vps>>>,
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb<FrameRef>,
    poc_state: PocState,
    pending: Option<PendingPic>,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    first_picture: bool,
    after_eos: bool,
    skip_rasl: bool,
    layout_cache: Vec<LayoutEntry>,
    out_queue: VecDeque<Output<FrameRef>>,
    shared: Arc<Shared>,
    pool: Option<rayon::ThreadPool>,
    in_flight: VecDeque<FrameRef>,
    max_in_flight: usize,
    draft: bool,
    time_base: TimeBase,
    threads: usize,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16)
}

impl Decoder {
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    pub fn with_threads(threads: usize) -> Self {
        let pool = if threads > 1 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("hevc-{i}"))
                .build()
                .ok()
        } else {
            None
        };
        Decoder {
            vpss: vec![None; 16],
            spss: vec![None; 16],
            ppss: vec![None; 64],
            nal_length_size: None,
            dpb: Dpb::default(),
            poc_state: PocState::default(),
            pending: None,
            next_id: 1,
            active_sps: None,
            first_picture: true,
            after_eos: false,
            skip_rasl: false,
            layout_cache: Vec::new(),
            out_queue: VecDeque::new(),
            shared: Arc::new(Shared::default()),
            pool,
            in_flight: VecDeque::new(),
            max_in_flight: threads.max(1) + 2,
            threads: threads.max(1),
            draft: false,
            time_base: TimeBase::new(1, 90000),
        }
    }

    pub fn from_hvcc(hvcc: &[u8]) -> crate::media::Result<Self> {
        let mut d = Decoder::new();
        d.configure_hvcc(hvcc)?;
        Ok(d)
    }

    pub fn configure_hvcc(&mut self, hvcc: &[u8]) -> crate::media::Result<()> {
        let config = Hvcc::parse(hvcc)?;
        for nal in config.vps.iter().chain(&config.sps).chain(&config.pps) {
            self.handle_nal(nal, 0)?;
        }
        self.nal_length_size = Some(config.nal_length_size);
        Ok(())
    }

    pub fn set_time_base(&mut self, time_base: TimeBase) {
        self.time_base = time_base;
    }

    pub fn color(&self) -> Option<ColorInfo> {
        let vui = self.active_sps.as_ref()?.vui.clone().unwrap_or_default();
        Some(ColorInfo {
            full_range: vui.full_range,
            primaries: vui.colour_primaries,
            transfer: vui.transfer_characteristics,
            matrix: vui.matrix_coefficients,
            sar: vui.sar,
        })
    }

    pub fn recycle(&self, frame: Image) {
        let mut p = self
            .shared
            .images
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for plane in frame.planes {
            if p.len() < 48 {
                p.push(plane.data);
            }
        }
    }

    pub fn decode(&mut self, packet: &Packet) -> crate::media::Result<Vec<Image>> {
        Ok(self.decode_au(&packet.data, packet.pts)?)
    }

    pub fn flush(&mut self) -> crate::media::Result<Vec<Image>> {
        let pics = self.flush_pictures();
        if let Some(e) = self.take_error() {
            return Err(e.into());
        }
        Ok(pics)
    }

    pub(crate) fn decode_au(&mut self, data: &[u8], pts: i64) -> Result<Vec<Image>> {
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
            if o.frame.image.get().is_none() && self.out_queue.len() <= max_queue {
                break;
            }
            let Some(o) = self.out_queue.pop_front() else {
                break;
            };
            pics.push(make_picture(&o, self.time_base));
        }
        Ok(pics)
    }

    pub(crate) fn flush_pictures(&mut self) -> Vec<Image> {
        self.submit_pending();
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.out_queue.extend(outs);
        let time_base = self.time_base;
        let pics = self
            .out_queue
            .drain(..)
            .map(|o| make_picture(&o, time_base))
            .collect();
        while let Some(f) = self.in_flight.pop_front() {
            f.wait_complete();
        }
        self.first_picture = true;
        pics
    }

    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub(crate) fn take_error(&mut self) -> Option<Error> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.len() < 2 {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal)?;
        if hdr.layer_id != 0 {
            return Ok(());
        }
        match hdr.nal_type {
            nal_type::VPS => {
                let v = Vps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = v.id as usize;
                self.vpss[id] = Some(Arc::new(v));
            }
            nal_type::SPS => {
                let s = Sps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = s.id as usize;
                self.spss[id] = Some(Arc::new(s));
            }
            nal_type::PPS => {
                let p = Pps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = p.id as usize;
                self.ppss[id] = Some(Arc::new(p));
            }
            nal_type::EOS | nal_type::EOB => {
                self.submit_pending();
                self.after_eos = true;
            }
            0..=9 | 16..=21 => self.handle_slice(nal, hdr, pts)?,
            _ => {}
        }
        Ok(())
    }

    fn layout_for(&mut self, pps: &Arc<Pps>, sps: &Arc<Sps>) -> Result<(Arc<Layout>, Scaling)> {
        if let Some((_, _, l, s)) = self
            .layout_cache
            .iter()
            .find(|(p, s, _, _)| Arc::ptr_eq(p, pps) && Arc::ptr_eq(s, sps))
        {
            return Ok((l.clone(), s.clone()));
        }
        let layout = Arc::new(Layout::new(sps, pps)?);
        let scaling = if sps.scaling_list_enabled {
            let list = pps
                .scaling_list
                .as_ref()
                .or(sps.scaling_list.as_ref())
                .cloned()
                .unwrap_or_else(crate::hevc::params::ScalingList::default_lists);
            let mut v = Vec::with_capacity(24);
            for size in 0..4 {
                for m in 0..6 {
                    v.push(list.factor(size, m));
                }
            }
            Some(Arc::new(v))
        } else {
            None
        };
        if self.layout_cache.len() > 8 {
            self.layout_cache.remove(0);
        }
        self.layout_cache
            .push((pps.clone(), sps.clone(), layout.clone(), scaling.clone()));
        Ok((layout, scaling))
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let (rbsp, escapes) = unescape_rbsp_marked(&nal[2..]);
        let first = rbsp.first().is_some_and(|b| b & 0x80 != 0);
        if first {
            self.submit_pending();
        }
        if hdr.is_rasl() && (self.skip_rasl || self.first_picture) {
            return Ok(());
        }
        let ppss = &self.ppss;
        let spss = &self.spss;
        let prev = self.pending.as_ref().map(|p| &p.last_sh);
        let (sh, pps, sps) = SliceHeader::parse(
            &rbsp,
            hdr,
            |id| {
                let pps = ppss[id as usize]
                    .as_ref()
                    .ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
                let sps = spss[pps.sps_id as usize]
                    .as_ref()
                    .ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
                Ok((pps.clone(), sps.clone()))
            },
            prev,
        )?;
        sps.check_supported()?;
        pps.check_supported()?;
        if first {
            self.start_picture(&sh, &sps, &pps, pts)?;
        } else {
            let Some(p) = &self.pending else {
                return invalid("slice segment without the first slice of its picture");
            };
            ensure!(Arc::ptr_eq(&p.pps, &pps), "PPS changed within a picture");
        }
        let Some(pending) = self.pending.as_mut() else {
            return invalid("slice segment without the first slice of its picture");
        };
        let refs = build_ref_lists(&sh, &pending.rps)?;
        if !sh.dependent {
            pending.last_sh = sh.clone();
        }
        pending.slices.push(SliceJob {
            sh,
            pps,
            sps,
            rbsp,
            escapes,
            refs,
        });
        Ok(())
    }

    fn start_picture(
        &mut self,
        sh: &SliceHeader,
        sps: &Arc<Sps>,
        pps: &Arc<Pps>,
        pts: i64,
    ) -> Result<()> {
        ensure!(
            sh.first_slice_segment_in_pic && !sh.dependent,
            "invalid first slice segment"
        );
        let hdr = sh.nal;
        let changed = match &self.active_sps {
            None => true,
            Some(a) => {
                a.width != sps.width
                    || a.height != sps.height
                    || a.bit_depth_luma != sps.bit_depth_luma
                    || a.bit_depth_chroma != sps.bit_depth_chroma
                    || a.log2_ctb != sps.log2_ctb
            }
        };
        let mut outs = Vec::new();
        if changed && self.active_sps.is_some() {
            self.dpb.flush(&mut outs);
        }
        self.active_sps = Some(sps.clone());
        self.dpb.max_dec_pic_buffering = sps.max_dec_pic_buffering as usize;
        self.dpb.max_num_reorder = sps.max_num_reorder as usize;
        self.dpb.max_latency = if sps.max_latency_increase_plus1 != 0 {
            sps.max_num_reorder + sps.max_latency_increase_plus1 - 1
        } else {
            0
        };
        let irap_no_rasl =
            hdr.is_irap() && (hdr.is_idr() || hdr.is_bla() || self.first_picture || self.after_eos);
        if hdr.is_irap() {
            self.skip_rasl = irap_no_rasl;
        }
        let max_lsb = sps.max_poc_lsb();
        let poc = self.poc_state.compute(sh, max_lsb, irap_no_rasl);
        let (w, h) = (sps.width as usize, sps.height as usize);
        let template = Frame::new(
            0,
            0,
            w,
            h,
            sps.log2_ctb,
            sps.bit_depth_luma,
            sps.bit_depth_chroma,
        );
        let next_id = &mut self.next_id;
        let mut make_missing = |p: i32| -> FrameRef {
            let id = *next_id;
            *next_id += 1;
            Arc::new(Frame::gray(id, p, &template))
        };
        let rps = self
            .dpb
            .apply_rps(sh, poc, max_lsb, irap_no_rasl, &mut make_missing)?;
        if irap_no_rasl && !self.first_picture {
            if sh.no_output_of_prior_pics && hdr.is_idr() {
                self.dpb.clear();
            } else {
                self.dpb.flush(&mut outs);
            }
        } else {
            self.dpb.bump_before_decode(&mut outs);
        }
        self.out_queue.extend(outs);
        let (layout, scaling) = self.layout_for(pps, sps)?;
        let _ = scaling;
        let id = self.next_id;
        self.next_id += 1;
        let frame = Arc::new(
            Frame::new(
                id,
                poc,
                w,
                h,
                sps.log2_ctb,
                sps.bit_depth_luma,
                sps.bit_depth_chroma,
            )
            .pooled(self.shared.rows.clone()),
        );
        let output = sh.pic_output && !(hdr.is_rasl() && self.skip_rasl);
        let draft = self.draft
            && hdr.is_sub_layer_non_ref()
            && hdr.temporal_id as u32 >= sps.max_sub_layers_minus1;
        let meta = Arc::new(output_meta(sps, pts, draft));
        self.pending = Some(PendingPic {
            frame,
            sps: sps.clone(),
            pps: pps.clone(),
            layout,
            last_sh: sh.clone(),
            first_sh: sh.clone(),
            rps,
            poc,
            output,
            meta,
            slices: Vec::new(),
        });
        self.first_picture = false;
        self.after_eos = false;
        Ok(())
    }

    fn submit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let PendingPic {
            frame,
            sps,
            pps,
            layout,
            first_sh,
            poc,
            output,
            meta,
            slices,
            ..
        } = p;
        let scaling = self
            .layout_cache
            .iter()
            .find(|(pp, ss, _, _)| Arc::ptr_eq(pp, &pps) && Arc::ptr_eq(ss, &sps))
            .and_then(|(_, _, _, s)| s.clone());
        let out = output.then(|| (meta.clone(), self.time_base));
        self.dispatch(Job {
            frame: frame.clone(),
            sps,
            pps,
            layout,
            scaling,
            slices,
            draft: meta.draft,
            out,
            helpers: if self.pool.is_some() {
                self.threads.saturating_sub(1).min(3)
            } else {
                0
            },
        });
        self.poc_state.update(&first_sh, poc);
        let mut outs = Vec::new();
        self.dpb.insert(frame, poc, output, meta, &mut outs);
        self.out_queue.extend(outs);
    }

    fn dispatch(&mut self, job: Job) {
        let shared = self.shared.clone();
        if let Some(pool) = &self.pool {
            self.in_flight.retain(|f| !f.is_complete());
            let rows = job.pps.entropy_coding_sync && !job.pps.tiles_enabled;
            let limit = if rows {
                self.threads / 4 + 1
            } else {
                self.max_in_flight
            };
            while self.in_flight.len() >= limit {
                if let Some(f) = self.in_flight.pop_front() {
                    f.wait_complete();
                }
            }
            self.in_flight.push_back(job.frame.clone());
            pool.spawn_fifo(move || run_job(job, &shared));
            return;
        }
        run_job(job, &shared);
    }
}

struct Job {
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    scaling: Scaling,
    slices: Vec<SliceJob>,
    draft: bool,
    out: Option<(Arc<OutputMeta>, TimeBase)>,
    helpers: usize,
}

fn run_job(job: Job, shared: &Shared) {
    let Job {
        frame,
        sps,
        pps,
        layout,
        scaling,
        slices,
        draft,
        out,
        helpers,
    } = job;
    let slices = Arc::new(slices);
    if let Some((meta, _)) = &out {
        let (cx, cy, cw, ch) = meta.crop;
        let crop = (cx as usize, cy as usize, cw as usize, ch as usize);
        frame.set_output(crop, shared.image_buffers());
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let input = Input {
            frame: frame.clone(),
            sps,
            pps,
            layout,
            scaling,
            slices,
            draft,
            helpers,
        };
        if input.sps.bit_depth_luma > 8 {
            decode_picture::<u16>(input, shared.pool16.clone())
        } else {
            decode_picture::<u8>(input, shared.pool8.clone())
        }
    }));
    let ready = result.is_ok();
    let err = match result {
        Ok(e) => e,
        Err(_) => {
            frame.fill_unpublished_gray();
            Some(Error::Invalid(
                "internal error while decoding a picture".into(),
            ))
        }
    };
    if let (Some(e), Ok(mut slot)) = (err, shared.error.lock()) {
        slot.get_or_insert(e);
    }
    if let Some((meta, time_base)) = out {
        let bufs = frame.take_output();
        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match bufs {
            Some(b) if ready => image(&meta, time_base, b),
            b => convert(&frame, &meta, time_base, b.unwrap_or_default()),
        }))
        .ok();
        let _ = frame.image.set(Mutex::new(image));
    }
}

struct Input {
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    scaling: Scaling,
    slices: Arc<Vec<SliceJob>>,
    draft: bool,
    helpers: usize,
}

fn decode_picture<P: Pel>(input: Input, pool: Pool<P>) -> Option<Error> {
    let Input {
        frame,
        sps,
        pps,
        layout,
        scaling,
        slices,
        draft,
        helpers,
    } = input;
    let pic = Arc::new(PicState::<P>::new(
        frame,
        &sps,
        pps,
        layout,
        scaling,
        &slices,
        draft,
        Some(pool),
    ));
    if helpers > 0
        && let Some(run) = SliceRun::plan(&pic, &slices)
    {
        let run = Arc::new(run);
        for _ in 0..helpers.min(run.slices() - 1) {
            let (pic, slices, run) = (pic.clone(), slices.clone(), run.clone());
            rayon::spawn(move || run.work(&pic, &slices));
        }
        run.work(&pic, &slices);
        pic.signal.wait_until(|| run.done());
        crate::hevc::filter::finish(&pic);
        return run.take_error();
    }
    let mut first_err = None;
    for (i, s) in slices.iter().enumerate() {
        let plan = if helpers > 0 { s.row_plan(&pic) } else { None };
        let Some(plan) = plan else {
            if let Err(e) = SliceDecoder::decode(&pic, s, i) {
                first_err.get_or_insert(e);
            }
            continue;
        };
        let plan = Arc::new(plan);
        for _ in 0..helpers.min(plan.rows() - 1) {
            let (pic, slices, plan) = (pic.clone(), slices.clone(), plan.clone());
            rayon::spawn(move || {
                if let Some(s) = slices.get(i) {
                    plan.work(&pic, s, i);
                }
            });
        }
        plan.work(&pic, s, i);
        pic.signal.wait_until(|| plan.done());
        if let Some(e) = plan.take_error() {
            first_err.get_or_insert(e);
        }
    }
    crate::hevc::filter::finish(&pic);
    first_err
}

fn output_meta(sps: &Sps, pts: i64, draft: bool) -> OutputMeta {
    OutputMeta {
        pts,
        crop: sps.crop_rect(),
        bit_depth: sps.bit_depth_luma,
        draft,
    }
}

fn make_picture(o: &Output<FrameRef>, time_base: TimeBase) -> Image {
    let ready = o.frame.image.wait().lock().ok().and_then(|mut s| s.take());
    ready.unwrap_or_else(|| convert(&o.frame, &o.meta, time_base, Default::default()))
}

crate::media::isa::dispatch! {
fn convert(frame: &Frame, meta: &OutputMeta, time_base: TimeBase, bufs: [Vec<u8>; 3]) -> Image {
    let (cx, cy, cw, ch) = meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    image(meta, time_base, frame.copy_cropped((cx, cy, cw, ch), bufs))
}
}

fn image(meta: &OutputMeta, time_base: TimeBase, [y, u, v]: [Vec<u8>; 3]) -> Image {
    let (cw, ch) = (meta.crop.2 as usize, meta.crop.3 as usize);
    let wide = meta.bit_depth > 8;
    let (ccw, cch) = (cw.div_ceil(2), ch.div_ceil(2));
    let bps = if wide { 2 } else { 1 };
    let plane = |data: Vec<u8>, w: usize, h: usize| Plane {
        data,
        stride: w * bps,
        width: w,
        height: h,
    };
    Image {
        format: if wide {
            PixelFormat::Yuv420p10
        } else {
            PixelFormat::Yuv420p8
        },
        width: cw as u32,
        height: ch as u32,
        pts: meta.pts,
        time_base,
        planes: [plane(y, cw, ch), plane(u, ccw, cch), plane(v, ccw, cch)],
    }
}
