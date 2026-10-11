use crate::hevc::cabac::{Cabac, Contexts, init_contexts};
use crate::hevc::error::{Error, Result, ensure, invalid};
use crate::hevc::grid::Grid;
use crate::hevc::inter;
use crate::hevc::intra::{self, Refs};
use crate::hevc::params::{Layout, Pps, Sps};
use crate::hevc::pel::Pel;
use crate::hevc::picture::{FrameRef, RefPic};
use crate::hevc::slice::{SliceHeader, SliceType};
use crate::hevc::spec_tables::*;
use crate::hevc::tables::{CTX_IDX_MAP, LEVEL_SCALE, SCAN_4, qpc_420, scan_order};
use crate::hevc::transform;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

pub type Scaling = Option<Arc<Vec<Vec<u8>>>>;

pub const F_INTRA: u8 = 1;
pub const F_SKIP: u8 = 2;
pub const F_NOFILTER: u8 = 4;
pub const F_CBF: u8 = 8;
pub const F_CODED: u8 = 16;

pub const E_TU_V: u8 = 1;
pub const E_PU_V: u8 = 2;
pub const E_TU_H: u8 = 4;
pub const E_PU_H: u8 = 8;

#[derive(Clone, Copy, Default)]
pub struct Blk {
    pub flags: u8,
    pub depth: u8,
    pub qp: i8,
    pub ipm: u8,
    pub edges: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MvField {
    pub mv: [[i16; 2]; 2],
    pub ref_idx: [i8; 2],
}

impl Default for MvField {
    fn default() -> Self {
        MvField {
            mv: [[0; 2]; 2],
            ref_idx: [-1, -1],
        }
    }
}

impl MvField {
    #[inline]
    pub fn pred(&self, l: usize) -> bool {
        self.ref_idx[l] >= 0
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct SaoParams {
    pub type_idx: [u8; 3],
    pub band_pos: [u8; 3],
    pub eo_class: [u8; 3],
    pub offsets: [[i16; 4]; 3],
}

pub struct SliceInfo {
    pub addr_rs: u32,
    pub deblock_disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub lf_across: bool,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub ref_ids: [Vec<u32>; 2],
    pub ref_pocs: [Vec<i32>; 2],
    pub ref_lt: [Vec<bool>; 2],
}

pub struct SliceJob {
    pub sh: SliceHeader,
    pub pps: Arc<Pps>,
    pub sps: Arc<Sps>,
    pub rbsp: Vec<u8>,
    pub escapes: Vec<u32>,
    pub refs: [Vec<RefPic>; 2],
}

impl SliceJob {
    fn escaped(&self, r: usize) -> usize {
        let mut e = r;
        for &p in &self.escapes {
            if p as usize <= e {
                e += 1;
            } else {
                break;
            }
        }
        e
    }

    fn unescaped(&self, e: usize) -> usize {
        e - self
            .escapes
            .iter()
            .take_while(|&&p| (p as usize) < e)
            .count()
    }

    pub fn row_plan<P: Pel>(&self, pic: &PicState<P>) -> Option<RowRun> {
        let (sh, pps) = (&self.sh, &self.pps);
        if !pps.entropy_coding_sync
            || pps.tiles_enabled
            || pps.dependent_slice_segments_enabled
            || sh.dependent
            || sh.entry_points.is_empty()
            || !(sh.segment_address as usize).is_multiple_of(pic.wctb)
        {
            return None;
        }
        let first_row = sh.segment_address as usize / pic.wctb;
        if first_row + sh.entry_points.len() + 1 > pic.hctb {
            return None;
        }
        let mut starts = vec![sh.data_offset];
        let mut at = self.escaped(sh.data_offset);
        for &off in &sh.entry_points {
            at = at.checked_add(off as usize)?;
            let start = self.unescaped(at);
            if start <= *starts.last()? || start >= self.rbsp.len() {
                return None;
            }
            starts.push(start);
        }
        Some(RowRun {
            next: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            abort: AtomicBool::new(false),
            error: Mutex::new(None),
            starts,
            first_row,
        })
    }
}

pub struct SliceRun {
    next: AtomicUsize,
    finished: AtomicUsize,
    count: usize,
    error: Mutex<Option<Error>>,
}

impl SliceRun {
    pub fn plan<P: Pel>(pic: &PicState<P>, jobs: &[SliceJob]) -> Option<SliceRun> {
        let first = jobs.first()?;
        let pps = &first.pps;
        if jobs.len() < 2
            || pps.tiles_enabled
            || pps.entropy_coding_sync
            || pps.dependent_slice_segments_enabled
        {
            return None;
        }
        let total = pic.wctb * pic.hctb;
        let mut prev: Option<usize> = None;
        for j in jobs {
            let addr = j.sh.segment_address as usize;
            if j.sh.dependent
                || !Arc::ptr_eq(&j.pps, pps)
                || !addr.is_multiple_of(pic.wctb)
                || addr >= total
                || prev.is_some_and(|p| p >= addr)
            {
                return None;
            }
            prev = Some(addr);
        }
        for (k, j) in jobs.iter().enumerate() {
            let start = j.sh.segment_address as usize;
            let end = jobs
                .get(k + 1)
                .map_or(total, |n| n.sh.segment_address as usize);
            for rs in start..end {
                pic.ctb_addr.set(rs, start as u32);
            }
        }
        Some(SliceRun {
            next: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            count: jobs.len(),
            error: Mutex::new(None),
        })
    }

    pub fn slices(&self) -> usize {
        self.count
    }

    pub fn done(&self) -> bool {
        self.finished.load(Ordering::SeqCst) >= self.count
    }

    pub fn take_error(&self) -> Option<Error> {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn work<P: Pel>(&self, pic: &PicState<P>, jobs: &[SliceJob]) {
        loop {
            let i = self.next.fetch_add(1, Ordering::SeqCst);
            let Some(job) = jobs.get(i).filter(|_| i < self.count) else {
                return;
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                SliceDecoder::decode(pic, job, i)
            }));
            let e = match r {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some(Error::Invalid(
                    "internal error while decoding a slice".into(),
                )),
            };
            if let Some(e) = e {
                self.error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert(e);
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            pic.signal.notify();
        }
    }
}

pub struct RowRun {
    next: AtomicUsize,
    finished: AtomicUsize,
    abort: AtomicBool,
    error: Mutex<Option<Error>>,
    starts: Vec<usize>,
    first_row: usize,
}

impl RowRun {
    pub fn rows(&self) -> usize {
        self.starts.len()
    }

    pub fn done(&self) -> bool {
        self.finished.load(Ordering::SeqCst) >= self.starts.len()
    }

    pub fn take_error(&self) -> Option<Error> {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn fail<P: Pel>(&self, pic: &PicState<P>, e: Error) {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(e);
        self.abort.store(true, Ordering::SeqCst);
        pic.signal.notify();
        for w in &pic.row_wakers {
            w.wake();
        }
    }

    pub fn work<P: Pel>(&self, pic: &PicState<P>, job: &SliceJob, index: usize) {
        loop {
            let k = self.next.fetch_add(1, Ordering::SeqCst);
            if k >= self.starts.len() {
                return;
            }
            if !self.abort.load(Ordering::SeqCst) {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    SliceDecoder::decode_row(pic, job, index, self, k)
                }));
                match r {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => self.fail(pic, e),
                    Err(_) => self.fail(
                        pic,
                        Error::Invalid("internal error while decoding a CTB row".into()),
                    ),
                }
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            pic.signal.notify();
        }
    }
}

#[derive(Default)]
pub struct Progress {
    pub rows_complete: usize,
    pub rows_deblocked: usize,
    pub rows_topped: usize,
    pub rows_published: usize,
}

#[derive(Default)]
pub struct Signal {
    lock: Mutex<()>,
    cv: Condvar,
    waiters: AtomicUsize,
}

#[derive(Default)]
pub struct Waker {
    thread: Mutex<Option<std::thread::Thread>>,
}

impl Waker {
    pub fn wake(&self) {
        let t = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(t) = t {
            t.unpark();
        }
    }

    pub fn wait_until(&self, ready: impl Fn() -> bool) {
        for _ in 0..256 {
            if ready() {
                return;
            }
            std::hint::spin_loop();
        }
        loop {
            *self.thread.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(std::thread::current());
            if ready() {
                self.thread
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                return;
            }
            std::thread::park_timeout(Duration::from_micros(500));
        }
    }
}

impl Signal {
    pub fn notify(&self) {
        if self.waiters.load(Ordering::SeqCst) > 0 {
            let _g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            self.cv.notify_all();
        }
    }

    pub fn wait_until(&self, ready: impl Fn() -> bool) {
        for _ in 0..64 {
            if ready() {
                return;
            }
            std::hint::spin_loop();
        }
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let mut g = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        while !ready() {
            g = match self.cv.wait_timeout(g, Duration::from_millis(2)) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        drop(g);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

pub type Pool<P> = Arc<Mutex<Vec<Buffers<P>>>>;

pub struct PicState<P: Pel> {
    pub frame: FrameRef,
    pub pps: Arc<Pps>,
    pub layout: Arc<Layout>,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub w4: usize,
    pub h4: usize,
    pub log2_ctb: u32,
    pub wctb: usize,
    pub hctb: usize,
    pub bd_y: u32,
    pub bd_c: u32,
    pub stride: [usize; 3],
    pub org: [usize; 3],
    pub saved: [Grid<P>; 3],
    pub sao_edges: [Grid<P>; 3],
    pub blk: Grid<Blk>,
    pub mvf: Grid<MvField>,
    pub ctb_slice: Grid<u32>,
    pub ctb_addr: Grid<u32>,
    pub sao: Grid<SaoParams>,
    pub slices: Vec<SliceInfo>,
    pub slice_of_job: Vec<Option<(u32, u32)>>,
    pub row_ctbs: Vec<AtomicU32>,
    pub filter: Mutex<Progress>,
    pub signal: Signal,
    pub row_wakers: Vec<Waker>,
    pub wpp_ctx: Vec<Mutex<Option<Box<Contexts>>>>,
    pub ds_ctx: Mutex<Option<(Box<Contexts>, i32)>>,
    pub scaling: Scaling,
    pub deblocking_enabled_anywhere: bool,
    pub nofilter_possible: bool,
    pub draft: bool,
    pool: Option<Pool<P>>,
}

pub struct Buffers<P: Pel> {
    sao_edges: [Vec<P>; 3],
    saved: [Vec<P>; 3],
    blk: Vec<Blk>,
    mvf: Vec<MvField>,
    ctb_slice: Vec<u32>,
    ctb_addr: Vec<u32>,
    sao: Vec<SaoParams>,
}

impl<P: Pel> Default for Buffers<P> {
    fn default() -> Self {
        Buffers {
            sao_edges: Default::default(),
            saved: Default::default(),
            blk: Vec::new(),
            mvf: Vec::new(),
            ctb_slice: Vec::new(),
            ctb_addr: Vec::new(),
            sao: Vec::new(),
        }
    }
}

fn reset<T: Clone>(v: &mut Vec<T>, n: usize, value: T) {
    v.clear();
    v.resize(n, value);
}

fn sized<T: Clone + Default>(v: &mut Vec<T>, n: usize) {
    if v.len() != n {
        v.clear();
        v.resize(n, T::default());
    }
}

fn slice_infos(jobs: &[SliceJob]) -> (Vec<SliceInfo>, Vec<Option<(u32, u32)>>) {
    let mut infos: Vec<SliceInfo> = Vec::new();
    let mut of_job = Vec::with_capacity(jobs.len());
    for job in jobs {
        let sh = &job.sh;
        if !sh.dependent {
            let ids = |l: usize| job.refs[l].iter().map(|r| r.frame.id).collect::<Vec<_>>();
            let pocs = |l: usize| job.refs[l].iter().map(|r| r.poc).collect::<Vec<_>>();
            let lts = |l: usize| job.refs[l].iter().map(|r| r.long_term).collect::<Vec<_>>();
            infos.push(SliceInfo {
                addr_rs: sh.segment_address,
                deblock_disabled: sh.deblocking_disabled,
                beta_offset: sh.beta_offset_div2 * 2,
                tc_offset: sh.tc_offset_div2 * 2,
                lf_across: sh.loop_filter_across_slices,
                sao_luma: sh.sao_luma,
                sao_chroma: sh.sao_chroma,
                ref_ids: [ids(0), ids(1)],
                ref_pocs: [pocs(0), pocs(1)],
                ref_lt: [lts(0), lts(1)],
            });
        }
        of_job.push(infos.last().map(|i| ((infos.len() - 1) as u32, i.addr_rs)));
    }
    (infos, of_job)
}

impl<P: Pel> PicState<P> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame: FrameRef,
        sps: &Sps,
        pps: Arc<Pps>,
        layout: Arc<Layout>,
        scaling: Scaling,
        jobs: &[SliceJob],
        draft: bool,
        pool: Option<Pool<P>>,
    ) -> Self {
        let mut b = pool
            .as_ref()
            .and_then(|p| p.lock().ok().and_then(|mut p| p.pop()))
            .unwrap_or_default();
        let (width, height) = (sps.width as usize, sps.height as usize);
        let (cwidth, cheight) = (width / 2, height / 2);
        let (w4, h4) = (width.div_ceil(4), height.div_ceil(4));
        let log2_ctb = sps.log2_ctb;
        let (wctb, hctb) = (
            sps.pic_width_in_ctbs() as usize,
            sps.pic_height_in_ctbs() as usize,
        );
        let (bd_y, bd_c) = (sps.bit_depth_luma, sps.bit_depth_chroma);
        sized(&mut b.sao_edges[0], 2 * width * hctb);
        sized(&mut b.sao_edges[1], 2 * cwidth * hctb);
        sized(&mut b.sao_edges[2], 2 * cwidth * hctb);
        frame.grids::<P>();
        let stride = [0, 1, 2].map(|c| frame.stride(c));
        let org = [0, 1, 2].map(|c| frame.origin(c));
        sized(&mut b.saved[0], width * hctb);
        sized(&mut b.saved[1], cwidth * hctb);
        sized(&mut b.saved[2], cwidth * hctb);
        reset(&mut b.blk, w4 * h4, Blk::default());
        reset(&mut b.mvf, w4 * h4, MvField::default());
        reset(&mut b.ctb_slice, wctb * hctb, u32::MAX);
        reset(&mut b.ctb_addr, wctb * hctb, u32::MAX);
        reset(&mut b.sao, wctb * hctb, SaoParams::default());
        let (slices, slice_of_job) = slice_infos(jobs);
        let deblocking_enabled_anywhere = slices.iter().any(|s| !s.deblock_disabled);
        let nofilter_possible = pps.transquant_bypass || sps.pcm_loop_filter_disabled;
        let [p0, p1, p2] = b.sao_edges;
        let [s0, s1, s2] = b.saved;
        PicState {
            poc: frame.poc,
            frame,
            pps,
            layout,
            width,
            height,
            cwidth,
            cheight,
            w4,
            h4,
            log2_ctb,
            wctb,
            hctb,
            bd_y,
            bd_c,
            stride,
            org,
            sao_edges: [p0.into(), p1.into(), p2.into()],
            saved: [s0.into(), s1.into(), s2.into()],
            blk: b.blk.into(),
            mvf: b.mvf.into(),
            ctb_slice: b.ctb_slice.into(),
            ctb_addr: b.ctb_addr.into(),
            sao: b.sao.into(),
            slices,
            slice_of_job,
            row_ctbs: (0..hctb).map(|_| AtomicU32::new(0)).collect(),
            filter: Mutex::new(Progress::default()),
            signal: Signal::default(),
            row_wakers: (0..hctb).map(|_| Waker::default()).collect(),
            wpp_ctx: (0..hctb).map(|_| Mutex::new(None)).collect(),
            ds_ctx: Mutex::new(None),
            scaling,
            deblocking_enabled_anywhere,
            nofilter_possible,
            draft,
            pool,
        }
    }

    pub fn fill_missing(&self) {
        let ctb = 1usize << self.log2_ctb;
        for rs in 0..self.wctb * self.hctb {
            if self.ctb_slice.get(rs) != u32::MAX {
                continue;
            }
            let (x0, y0) = ((rs % self.wctb) * ctb, (rs / self.wctb) * ctb);
            for c in 0..3 {
                let (w, h, s, bd) = if c == 0 {
                    (self.width, self.height, ctb, self.bd_y)
                } else {
                    (self.cwidth, self.cheight, ctb / 2, self.bd_c)
                };
                let (x0, y0) = if c == 0 { (x0, y0) } else { (x0 / 2, y0 / 2) };
                let (x1, y1) = ((x0 + s).min(w), (y0 + s).min(h));
                for y in y0..y1 {
                    let at = self.at(c, x0, y);
                    self.plane(c)
                        .slice_mut(at..at + x1 - x0)
                        .fill(P::from_i32(1 << (bd - 1)));
                }
            }
        }
    }

    #[inline(always)]
    pub fn plane(&self, c: usize) -> &Grid<P> {
        &self.frame.grids::<P>()[c]
    }

    #[inline(always)]
    pub fn at(&self, c: usize, x: usize, y: usize) -> usize {
        self.org[c] + y * self.stride[c] + x
    }

    pub fn lines_mut(&self, c: usize, y0: usize, n: usize) -> &mut [P] {
        let (w, h) = if c == 0 {
            (self.width, self.height)
        } else {
            (self.cwidth, self.cheight)
        };
        let y1 = (y0 + n).min(h);
        if y1 <= y0 {
            return &mut [];
        }
        let at = self.at(c, 0, y0);
        self.plane(c)
            .slice_mut(at..at + (y1 - y0 - 1) * self.stride[c] + w)
    }

    #[inline]
    pub fn zavail(&self, xc: i32, yc: i32, xn: i32, yn: i32, addr: u32) -> bool {
        if xn < 0 || yn < 0 || xn >= self.width as i32 || yn >= self.height as i32 {
            return false;
        }
        let s = self.log2_ctb;
        let ctb_n = (yn >> s) as usize * self.wctb + (xn >> s) as usize;
        let ctb_c = (yc >> s) as usize * self.wctb + (xc >> s) as usize;
        if ctb_n != ctb_c {
            let tsn = self.layout.rs_to_ts[ctb_n];
            let tsc = self.layout.rs_to_ts[ctb_c];
            tsn < tsc
                && self.ctb_addr.get(ctb_n) == addr
                && self.layout.tile_id[tsn as usize] == self.layout.tile_id[tsc as usize]
        } else {
            let m = (1 << s) - 1;
            morton(((xn & m) >> 2) as u32, ((yn & m) >> 2) as u32)
                <= morton(((xc & m) >> 2) as u32, ((yc & m) >> 2) as u32)
        }
    }

    #[inline]
    pub fn blk_at(&self, x: i32, y: i32) -> Blk {
        self.blk
            .get((y as usize >> 2) * self.w4 + (x as usize >> 2))
    }
}

impl<P: Pel> Drop for PicState<P> {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else { return };
        let take = |g: &mut Grid<P>| std::mem::take(g).into_vec();
        let b = Buffers {
            sao_edges: [
                take(&mut self.sao_edges[0]),
                take(&mut self.sao_edges[1]),
                take(&mut self.sao_edges[2]),
            ],
            saved: [
                take(&mut self.saved[0]),
                take(&mut self.saved[1]),
                take(&mut self.saved[2]),
            ],
            blk: std::mem::take(&mut self.blk).into_vec(),
            mvf: std::mem::take(&mut self.mvf).into_vec(),
            ctb_slice: std::mem::take(&mut self.ctb_slice).into_vec(),
            ctb_addr: std::mem::take(&mut self.ctb_addr).into_vec(),
            sao: std::mem::take(&mut self.sao).into_vec(),
        };
        if let Ok(mut p) = pool.lock()
            && p.len() < 8
        {
            p.push(b);
        }
    }
}

const SIG_PATTERN: [[u8; 16]; 4] = [
    [2, 1, 1, 0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0],
    [2, 2, 2, 2, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0],
    [2, 1, 0, 0, 2, 1, 0, 0, 2, 1, 0, 0, 2, 1, 0, 0],
    [2; 16],
];

#[inline]
fn morton(x: u32, y: u32) -> u32 {
    let mut r = 0;
    for i in 0..5 {
        r |= ((x >> i) & 1) << (2 * i);
        r |= ((y >> i) & 1) << (2 * i + 1);
    }
    r
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartMode {
    P2Nx2N,
    P2NxN,
    PNx2N,
    PNxN,
    P2NxnU,
    P2NxnD,
    PnLx2N,
    PnRx2N,
}

pub struct SliceDecoder<'a, P: Pel> {
    pub pic: &'a PicState<P>,
    pub sh: &'a SliceHeader,
    pub sps: &'a Sps,
    pub pps: &'a Pps,
    pub refs: &'a [Vec<RefPic>; 2],
    pub c: Cabac<'a>,
    pub slice_idx: u32,
    pub slice_addr: u32,
    pub slice_qp: i32,
    pub init_type: usize,
    pub qp_y: i32,
    pub qp_y_pred: i32,
    pub last_qp: i32,
    pub is_cu_qp_delta_coded: bool,
    pub cu_qp_delta: i32,
    pub log2_min_cu_qp_delta: u32,
    pub qp_cb: i32,
    pub qp_cr: i32,
    pub cu_transquant_bypass: bool,
    pub cu_intra: bool,
    pub intra_chroma_mode: u32,
    pub max_trafo_depth: u32,
    pub intra_split: bool,
    pub cu_part: PartMode,
    pub coeffs: Vec<i16>,
    pub pred0: Vec<i16>,
    pub pred1: Vec<i16>,
    pub mc: crate::hevc::inter::McScratch<P>,
    pub refs_buf: Refs,
    pub col: Option<RefPic>,
    pub no_backward_pred: bool,
    pub ctb_x: i32,
    pub ctb_y: i32,
}

impl<'a, P: Pel> SliceDecoder<'a, P> {
    #[inline(always)]
    fn dd(&mut self, ctx: usize) -> u32 {
        self.c.decode_decision(ctx)
    }
    #[inline(always)]
    fn bypass(&mut self) -> u32 {
        self.c.decode_bypass()
    }
    #[inline(always)]
    fn bypass_bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        let mut left = n;
        while left > 16 {
            v = (v << 16) | self.c.decode_bypass_bits(16);
            left -= 16;
        }
        v.checked_shl(left).unwrap_or(0) | self.c.decode_bypass_bits(left)
    }

    pub fn decode(pic: &'a PicState<P>, job: &'a SliceJob, index: usize) -> Result<()> {
        let mut d = SliceDecoder::new(pic, job, index)?;
        d.decode_data()
    }

    fn new(pic: &'a PicState<P>, job: &'a SliceJob, index: usize) -> Result<Self> {
        let sh = &job.sh;
        let (sps, pps) = (&*job.sps, &*job.pps);
        ensure!(sh.data_offset < job.rbsp.len(), "empty slice segment data");
        let slice_qp = pps.init_qp + sh.qp_delta;
        let init_type = match sh.slice_type {
            SliceType::I => 0,
            SliceType::P => {
                if sh.cabac_init_flag {
                    2
                } else {
                    1
                }
            }
            SliceType::B => {
                if sh.cabac_init_flag {
                    1
                } else {
                    2
                }
            }
        };
        let Some(&Some((slice_idx, slice_addr))) = pic.slice_of_job.get(index) else {
            return invalid("dependent slice segment without a preceding slice");
        };
        let col = if sh.temporal_mvp && !sh.is_intra() {
            let l = if sh.is_b() && !sh.collocated_from_l0 {
                1
            } else {
                0
            };
            job.refs[l].get(sh.collocated_ref_idx as usize).cloned()
        } else {
            None
        };
        let no_backward_pred = job.refs.iter().flatten().all(|r| r.poc <= pic.poc);
        let log2_min_cu_qp_delta = sps.log2_ctb - pps.diff_cu_qp_delta_depth;
        let c = Cabac::new(&job.rbsp, sh.data_offset, slice_qp, init_type)?;
        Ok(SliceDecoder {
            pic,
            sh,
            sps,
            pps,
            refs: &job.refs,
            c,
            slice_idx,
            slice_addr,
            slice_qp,
            init_type,
            qp_y: slice_qp,
            qp_y_pred: slice_qp,
            last_qp: slice_qp,
            is_cu_qp_delta_coded: false,
            cu_qp_delta: 0,
            log2_min_cu_qp_delta,
            qp_cb: 0,
            qp_cr: 0,
            cu_transquant_bypass: false,
            cu_intra: false,
            intra_chroma_mode: 0,
            max_trafo_depth: 0,
            intra_split: false,
            cu_part: PartMode::P2Nx2N,
            coeffs: vec![0; 32 * 32],
            pred0: vec![0; 64 * 64],
            pred1: vec![0; 64 * 64],
            mc: Default::default(),
            refs_buf: Refs::default(),
            col,
            no_backward_pred,
            ctb_x: 0,
            ctb_y: 0,
        })
    }

    fn decode_row(
        pic: &'a PicState<P>,
        job: &'a SliceJob,
        index: usize,
        run: &RowRun,
        k: usize,
    ) -> Result<()> {
        let mut d = SliceDecoder::new(pic, job, index)?;
        d.c = Cabac::new(&job.rbsp, run.starts[k], d.slice_qp, d.init_type)?;
        let row = run.first_row + k;
        let last = k + 1 == run.starts.len();
        let wctb = pic.wctb;
        let ctb = 1i32 << d.sps.log2_ctb;
        for rx in 0..wctb {
            if k > 0 {
                let need = (rx + 2).min(wctb) as u32;
                let above = &pic.row_ctbs[row - 1];
                pic.row_wakers[row - 1].wait_until(|| {
                    above.load(Ordering::SeqCst) >= need || run.abort.load(Ordering::SeqCst)
                });
                if run.abort.load(Ordering::SeqCst) {
                    return invalid("CTB row stopped after an error in another row");
                }
            }
            let rs = row * wctb + rx;
            ensure!(pic.ctb_slice.get(rs) == u32::MAX, "CTB {rs} decoded twice");
            d.ctb_x = rx as i32 * ctb;
            d.ctb_y = row as i32 * ctb;
            pic.ctb_slice.set(rs, d.slice_idx);
            pic.ctb_addr.set(rs, d.slice_addr);
            if rx == 0 && k > 0 {
                let (x0, y0) = (d.ctb_x, d.ctb_y);
                let avail = pic.zavail(x0, y0, x0 + ctb, y0 - ctb, d.slice_addr);
                let saved = if avail { d.wpp_saved(row) } else { None };
                match saved {
                    Some(ctx) => d.c.ctx = *ctx,
                    None => init_contexts(&mut d.c.ctx, d.slice_qp, d.init_type),
                }
                d.last_qp = d.slice_qp;
            }
            if d.sh.sao_luma || d.sh.sao_chroma {
                d.parse_sao(rx as i32, row as i32, rs as u32)?;
            }
            d.coding_quadtree(d.ctb_x, d.ctb_y, d.sps.log2_ctb, 0)?;
            let end = d.c.decode_terminate() == 1;
            ensure!(!d.c.overrun(), "slice data overrun");
            if rx == 1 {
                d.wpp_save(row);
            }
            d.save_bottom(rx, row);
            crate::hevc::filter::ctb_decoded(pic, row);
            if end {
                ensure!(last, "slice ends before its last substream");
                return Ok(());
            }
        }
        ensure!(!last, "slice data continues past its last substream");
        Ok(())
    }

    fn decode_data(&mut self) -> Result<()> {
        let layout = self.pic.layout.clone();
        let wctb = self.pic.wctb as u32;
        let npic = (self.pic.wctb * self.pic.hctb) as u32;
        let mut ts = layout.rs_to_ts[self.sh.segment_address as usize];
        let mut first = true;
        loop {
            let rs = layout.ts_to_rs[ts as usize];
            ensure!(
                self.pic.ctb_slice.get(rs as usize) == u32::MAX,
                "CTB {rs} decoded twice"
            );
            let (rx, ry) = ((rs % wctb) as i32, (rs / wctb) as i32);
            let ctb = 1i32 << self.sps.log2_ctb;
            self.ctb_x = rx * ctb;
            self.ctb_y = ry * ctb;
            self.pic.ctb_slice.set(rs as usize, self.slice_idx);
            if self.pic.ctb_addr.get(rs as usize) != self.slice_addr {
                self.pic.ctb_addr.set(rs as usize, self.slice_addr);
            }
            let first_in_tile =
                ts == 0 || layout.tile_id[ts as usize] != layout.tile_id[ts as usize - 1];
            let wpp_row_start = self.pps.entropy_coding_sync
                && (rs.is_multiple_of(wctb)
                    || layout.tile_id[ts as usize]
                        != layout.tile_id[layout.rs_to_ts[rs as usize - 1] as usize]);
            if first_in_tile {
                if !first {
                    init_contexts(&mut self.c.ctx, self.slice_qp, self.init_type);
                }
                self.last_qp = self.slice_qp;
            } else if wpp_row_start {
                let (x0, y0) = (self.ctb_x, self.ctb_y);
                let avail = self.pic.zavail(x0, y0, x0 + ctb, y0 - ctb, self.slice_addr);
                let saved = if avail {
                    self.wpp_saved(ry as usize)
                } else {
                    None
                };
                match saved {
                    Some(ctx) => self.c.ctx = *ctx,
                    None => init_contexts(&mut self.c.ctx, self.slice_qp, self.init_type),
                }
                self.last_qp = self.slice_qp;
            } else if first && self.sh.dependent {
                let saved = self
                    .pic
                    .ds_ctx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .map(|(c, qp)| (c.clone(), *qp));
                match saved {
                    Some((ctx, qp)) => {
                        self.c.ctx = *ctx;
                        self.last_qp = qp;
                    }
                    None => return invalid("dependent slice segment without stored contexts"),
                }
            }
            first = false;
            if self.sh.sao_luma || self.sh.sao_chroma {
                self.parse_sao(rx, ry, rs)?;
            }
            self.coding_quadtree(self.ctb_x, self.ctb_y, self.sps.log2_ctb, 0)?;
            let end = self.c.decode_terminate() == 1;
            ensure!(!self.c.overrun(), "slice data overrun");
            if self.pps.entropy_coding_sync
                && (rs % wctb == 1
                    || (rs > 1
                        && layout.tile_id[ts as usize]
                            != layout.tile_id[layout.rs_to_ts[rs as usize - 2] as usize]))
            {
                self.wpp_save(ry as usize);
            }
            self.save_bottom(rx as usize, ry as usize);
            crate::hevc::filter::ctb_decoded(self.pic, ry as usize);
            ts += 1;
            if end {
                if self.pps.dependent_slice_segments_enabled {
                    *self
                        .pic
                        .ds_ctx
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) =
                        Some((Box::new(self.c.ctx), self.last_qp));
                }
                break;
            }
            ensure!(ts < npic, "slice data continues past the last CTB");
            let nrs = layout.ts_to_rs[ts as usize];
            let new_tile = self.pps.tiles_enabled
                && layout.tile_id[ts as usize] != layout.tile_id[ts as usize - 1];
            let new_row = self.pps.entropy_coding_sync
                && (nrs.is_multiple_of(wctb)
                    || layout.tile_id[ts as usize]
                        != layout.tile_id[layout.rs_to_ts[nrs as usize - 1] as usize]);
            if new_tile || new_row {
                ensure!(self.c.decode_terminate() == 1, "end_of_subset_one_bit is 0");
                let pos = self.c.bit_pos().div_ceil(8);
                self.c.set_byte_pos(pos);
                self.c.init_engine()?;
            }
        }
        Ok(())
    }

    fn wpp_saved(&self, row: usize) -> Option<Box<Contexts>> {
        let slot = self.pic.wpp_ctx.get(row.checked_sub(1)?)?;
        slot.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn wpp_save(&self, row: usize) {
        if let Some(slot) = self.pic.wpp_ctx.get(row) {
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(self.c.ctx));
        }
    }

    fn save_bottom(&self, rx: usize, ry: usize) {
        let pic = self.pic;
        if ry + 1 >= pic.hctb {
            return;
        }
        let ctb = 1usize << pic.log2_ctb;
        for c in 0..3 {
            let (w, s) = if c == 0 {
                (pic.width, ctb)
            } else {
                (pic.cwidth, ctb / 2)
            };
            let x0 = rx * s;
            let x1 = (x0 + s).min(w);
            let line = (ry + 1) * s - 1;
            let at = pic.at(c, x0, line);
            let src = pic.plane(c).slice(at..at + x1 - x0);
            pic.saved[c]
                .slice_mut(ry * w + x0..ry * w + x1)
                .copy_from_slice(src);
        }
    }

    fn parse_sao(&mut self, rx: i32, ry: i32, rs: u32) -> Result<()> {
        let layout = self.pic.layout.clone();
        let wctb = self.pic.wctb as u32;
        let ts = layout.rs_to_ts[rs as usize];
        let mut merge_left = false;
        let mut merge_up = false;
        if rx > 0 {
            let left_in_slice = self.pic.ctb_addr.get(rs as usize - 1) == self.slice_addr;
            let left_in_tile = layout.tile_id[ts as usize]
                == layout.tile_id[layout.rs_to_ts[rs as usize - 1] as usize];
            if left_in_slice && left_in_tile {
                merge_left = self.dd(SAO_MERGE) == 1;
            }
        }
        if ry > 0 && !merge_left {
            let up = rs - wctb;
            let up_in_slice = self.pic.ctb_addr.get(up as usize) == self.slice_addr;
            let up_in_tile = layout.tile_id[ts as usize]
                == layout.tile_id[layout.rs_to_ts[up as usize] as usize];
            if up_in_slice && up_in_tile {
                merge_up = self.dd(SAO_MERGE) == 1;
            }
        }
        if merge_left {
            self.pic
                .sao
                .set(rs as usize, self.pic.sao.get(rs as usize - 1));
            return Ok(());
        }
        if merge_up {
            self.pic
                .sao
                .set(rs as usize, self.pic.sao.get((rs - wctb) as usize));
            return Ok(());
        }
        let mut p = SaoParams::default();
        for c in 0..3 {
            let enabled = if c == 0 {
                self.sh.sao_luma
            } else {
                self.sh.sao_chroma
            };
            if !enabled {
                continue;
            }
            if c == 2 {
                p.type_idx[2] = p.type_idx[1];
                p.eo_class[2] = p.eo_class[1];
            } else {
                p.type_idx[c] = if self.dd(SAO_TYPE) == 0 {
                    0
                } else if self.bypass() == 0 {
                    1
                } else {
                    2
                };
            }
            if p.type_idx[c] == 0 {
                continue;
            }
            let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
            let cmax = (1u32 << (bd.min(10) - 5)) - 1;
            let mut abs = [0i16; 4];
            for a in abs.iter_mut() {
                let mut v = 0;
                while v < cmax && self.bypass() == 1 {
                    v += 1;
                }
                *a = v as i16;
            }
            if p.type_idx[c] == 1 {
                for a in abs.iter_mut() {
                    if *a != 0 && self.bypass() == 1 {
                        *a = -*a;
                    }
                }
                p.band_pos[c] = self.bypass_bits(5) as u8;
                p.offsets[c] = abs;
            } else {
                p.offsets[c] = [abs[0], abs[1], -abs[2], -abs[3]];
                if c == 0 {
                    p.eo_class[0] = self.bypass_bits(2) as u8;
                }
                if c == 1 {
                    p.eo_class[1] = self.bypass_bits(2) as u8;
                }
            }
        }
        self.pic.sao.set(rs as usize, p);
        Ok(())
    }

    fn coding_quadtree(&mut self, x0: i32, y0: i32, log2: u32, depth: u32) -> Result<()> {
        let size = 1i32 << log2;
        let (w, h) = (self.pic.width as i32, self.pic.height as i32);
        let split = if x0 + size <= w && y0 + size <= h && log2 > self.sps.log2_min_cb {
            let mut inc = 0;
            if self.pic.zavail(x0, y0, x0 - 1, y0, self.slice_addr)
                && self.pic.blk_at(x0 - 1, y0).depth as u32 > depth
            {
                inc += 1;
            }
            if self.pic.zavail(x0, y0, x0, y0 - 1, self.slice_addr)
                && self.pic.blk_at(x0, y0 - 1).depth as u32 > depth
            {
                inc += 1;
            }
            self.dd(SPLIT_CU + inc) == 1
        } else {
            log2 > self.sps.log2_min_cb
        };
        if self.pps.cu_qp_delta_enabled && log2 >= self.log2_min_cu_qp_delta {
            self.is_cu_qp_delta_coded = false;
            self.cu_qp_delta = 0;
        }
        if split {
            let half = size >> 1;
            let (x1, y1) = (x0 + half, y0 + half);
            self.coding_quadtree(x0, y0, log2 - 1, depth + 1)?;
            if x1 < w {
                self.coding_quadtree(x1, y0, log2 - 1, depth + 1)?;
            }
            if y1 < h {
                self.coding_quadtree(x0, y1, log2 - 1, depth + 1)?;
            }
            if x1 < w && y1 < h {
                self.coding_quadtree(x1, y1, log2 - 1, depth + 1)?;
            }
            Ok(())
        } else {
            self.coding_unit(x0, y0, log2, depth)
        }
    }

    fn fill_blk(&mut self, x0: i32, y0: i32, w: i32, h: i32, f: impl Fn(&mut Blk)) {
        let (bx0, by0) = ((x0 >> 2) as usize, (y0 >> 2) as usize);
        let bx1 = (((x0 + w) as usize).min(self.pic.width).div_ceil(4)).max(bx0);
        let by1 = (((y0 + h) as usize).min(self.pic.height).div_ceil(4)).max(by0);
        for by in by0..by1 {
            let row = by * self.pic.w4;
            for b in self.pic.blk.slice_mut(row + bx0..row + bx1) {
                f(b);
            }
        }
    }

    fn derive_qp_pred(&mut self, xq: i32, yq: i32) {
        let prev = self.last_qp;
        let s = self.sps.log2_ctb;
        let cur_ctb = ((yq >> s), (xq >> s));
        let qa = if self.pic.zavail(xq, yq, xq - 1, yq, self.slice_addr)
            && ((yq >> s), ((xq - 1) >> s)) == cur_ctb
        {
            self.pic.blk_at(xq - 1, yq).qp as i32
        } else {
            prev
        };
        let qb = if self.pic.zavail(xq, yq, xq, yq - 1, self.slice_addr)
            && (((yq - 1) >> s), (xq >> s)) == cur_ctb
        {
            self.pic.blk_at(xq, yq - 1).qp as i32
        } else {
            prev
        };
        self.qp_y_pred = (qa + qb + 1) >> 1;
    }

    fn update_qp(&mut self) {
        let off = 6 * (self.pic.bd_y as i32 - 8);
        self.qp_y = ((self.qp_y_pred + self.cu_qp_delta + 52 + 2 * off) % (52 + off)) - off;
        let offc = 6 * (self.pic.bd_c as i32 - 8);
        let qpi_cb = (self.qp_y + self.pps.cb_qp_offset + self.sh.cb_qp_offset).clamp(-offc, 57);
        let qpi_cr = (self.qp_y + self.pps.cr_qp_offset + self.sh.cr_qp_offset).clamp(-offc, 57);
        self.qp_cb = qpc_420(qpi_cb) + offc;
        self.qp_cr = qpc_420(qpi_cr) + offc;
    }

    fn coding_unit(&mut self, x0: i32, y0: i32, log2: u32, depth: u32) -> Result<()> {
        let n = 1i32 << log2;
        let qg_mask = (1 << self.log2_min_cu_qp_delta) - 1;
        if (x0 & qg_mask) == 0 && (y0 & qg_mask) == 0 {
            self.derive_qp_pred(x0, y0);
        }
        self.update_qp();
        self.cu_transquant_bypass = if self.pps.transquant_bypass {
            self.dd(CU_TRANSQUANT_BYPASS) == 1
        } else {
            false
        };
        let mut skip = false;
        if !self.sh.is_intra() {
            let mut inc = 0;
            if self.pic.zavail(x0, y0, x0 - 1, y0, self.slice_addr)
                && self.pic.blk_at(x0 - 1, y0).flags & F_SKIP != 0
            {
                inc += 1;
            }
            if self.pic.zavail(x0, y0, x0, y0 - 1, self.slice_addr)
                && self.pic.blk_at(x0, y0 - 1).flags & F_SKIP != 0
            {
                inc += 1;
            }
            skip = self.dd(CU_SKIP + inc) == 1;
        }
        let bypass = self.cu_transquant_bypass;
        let qp = self.qp_y as i8;
        self.fill_blk(x0, y0, n, n, |b| {
            b.depth = depth as u8;
            b.flags = F_CODED | if skip { F_SKIP } else { 0 } | if bypass { F_NOFILTER } else { 0 };
            b.qp = qp;
            b.ipm = 1;
            b.edges = 0;
        });
        self.mark_edges(x0, y0, n, n, E_TU_V | E_PU_V, E_TU_H | E_PU_H);
        if skip {
            self.cu_intra = false;
            self.prediction_unit(x0, y0, n, x0, y0, n, n, 0, PartMode::P2Nx2N, true)?;
            self.finish_cu(x0, y0, n);
            return Ok(());
        }
        let intra = if !self.sh.is_intra() {
            self.dd(PRED_MODE) == 1
        } else {
            true
        };
        self.cu_intra = intra;
        let mut part = PartMode::P2Nx2N;
        if !intra || log2 == self.sps.log2_min_cb {
            part = self.parse_part_mode(intra, log2)?;
        }
        self.cu_part = part;
        if intra {
            self.fill_blk(x0, y0, n, n, |b| b.flags |= F_INTRA);
            self.intra_split = part == PartMode::PNxN;
            let pcm = part == PartMode::P2Nx2N
                && self.sps.pcm
                && log2 >= self.sps.log2_min_pcm
                && log2 <= self.sps.log2_max_pcm
                && self.c.decode_terminate() == 1;
            if pcm {
                self.pcm_sample(x0, y0, log2)?;
                if self.sps.pcm_loop_filter_disabled {
                    self.fill_blk(x0, y0, n, n, |b| b.flags |= F_NOFILTER);
                }
                self.finish_cu(x0, y0, n);
                return Ok(());
            }
            self.parse_intra_modes(x0, y0, n, part)?;
            self.max_trafo_depth = self.sps.max_th_depth_intra + self.intra_split as u32;
            self.transform_tree(x0, y0, x0, y0, log2, 0, 0, [true, true])?;
        } else {
            self.intra_split = false;
            let mut merge2nx2n = false;
            let h = n / 2;
            let q = n / 4;
            let parts: &[(i32, i32, i32, i32)] = match part {
                PartMode::P2Nx2N => &[(0, 0, n, n)],
                PartMode::P2NxN => &[(0, 0, n, h), (0, h, n, h)],
                PartMode::PNx2N => &[(0, 0, h, n), (h, 0, h, n)],
                PartMode::P2NxnU => &[(0, 0, n, q), (0, q, n, n - q)],
                PartMode::P2NxnD => &[(0, 0, n, n - q), (0, n - q, n, q)],
                PartMode::PnLx2N => &[(0, 0, q, n), (q, 0, n - q, n)],
                PartMode::PnRx2N => &[(0, 0, n - q, n), (n - q, 0, q, n)],
                PartMode::PNxN => &[(0, 0, h, h), (h, 0, h, h), (0, h, h, h), (h, h, h, h)],
            };
            for (i, &(px, py, pw, ph)) in parts.iter().enumerate() {
                let merge =
                    self.prediction_unit(x0, y0, n, x0 + px, y0 + py, pw, ph, i, part, false)?;
                if part == PartMode::P2Nx2N {
                    merge2nx2n = merge;
                }
                if px != 0 {
                    self.mark_edges(x0 + px, y0 + py, pw, ph, E_PU_V, 0);
                }
                if py != 0 {
                    self.mark_edges(x0 + px, y0 + py, pw, ph, 0, E_PU_H);
                }
            }
            let root_cbf = if !merge2nx2n {
                self.dd(RQT_ROOT_CBF) == 1
            } else {
                true
            };
            if root_cbf {
                self.max_trafo_depth = self.sps.max_th_depth_inter;
                self.transform_tree(x0, y0, x0, y0, log2, 0, 0, [true, true])?;
            }
        }
        self.finish_cu(x0, y0, n);
        Ok(())
    }

    fn finish_cu(&mut self, x0: i32, y0: i32, n: i32) {
        let qp = self.qp_y as i8;
        self.fill_blk(x0, y0, n, n, |b| b.qp = qp);
        self.last_qp = self.qp_y;
    }

    fn mark_edges(&mut self, x0: i32, y0: i32, w: i32, h: i32, v: u8, hz: u8) {
        let w4 = self.pic.w4;
        let (bx0, by0) = ((x0 >> 2) as usize, (y0 >> 2) as usize);
        if v != 0 {
            let by1 = ((y0 + h) as usize).min(self.pic.height).div_ceil(4);
            for by in by0..by1 {
                let i = by * w4 + bx0;
                let mut b = self.pic.blk.get(i);
                b.edges |= v;
                self.pic.blk.set(i, b);
            }
        }
        if hz != 0 {
            let bx1 = ((x0 + w) as usize).min(self.pic.width).div_ceil(4);
            for b in self.pic.blk.slice_mut(by0 * w4 + bx0..by0 * w4 + bx1) {
                b.edges |= hz;
            }
        }
    }

    fn parse_part_mode(&mut self, intra: bool, log2: u32) -> Result<PartMode> {
        if intra {
            return Ok(if self.dd(PART_MODE) == 1 {
                PartMode::P2Nx2N
            } else {
                PartMode::PNxN
            });
        }
        if self.dd(PART_MODE) == 1 {
            return Ok(PartMode::P2Nx2N);
        }
        let min = log2 == self.sps.log2_min_cb;
        if min {
            if self.dd(PART_MODE + 1) == 1 {
                return Ok(PartMode::P2NxN);
            }
            if log2 == 3 {
                return Ok(PartMode::PNx2N);
            }
            return Ok(if self.dd(PART_MODE + 2) == 1 {
                PartMode::PNx2N
            } else {
                PartMode::PNxN
            });
        }
        let amp = self.sps.amp;
        let hor = self.dd(PART_MODE + 1) == 1;
        if !amp {
            return Ok(if hor {
                PartMode::P2NxN
            } else {
                PartMode::PNx2N
            });
        }
        let no_amp = self.dd(PART_MODE + 3) == 1;
        if no_amp {
            return Ok(if hor {
                PartMode::P2NxN
            } else {
                PartMode::PNx2N
            });
        }
        let b = self.bypass();
        Ok(match (hor, b) {
            (true, 0) => PartMode::P2NxnU,
            (true, _) => PartMode::P2NxnD,
            (false, 0) => PartMode::PnLx2N,
            (false, _) => PartMode::PnRx2N,
        })
    }

    fn pcm_sample(&mut self, x0: i32, y0: i32, log2: u32) -> Result<()> {
        let start = self.c.bit_pos().div_ceil(8);
        let data = self.c.data();
        let n = 1usize << log2;
        let (bl, bc) = (self.sps.pcm_bit_depth_luma, self.sps.pcm_bit_depth_chroma);
        let total_bits = n * n * bl as usize + 2 * (n / 2) * (n / 2) * bc as usize;
        ensure!(
            start + total_bits.div_ceil(8) <= data.len(),
            "PCM samples truncated"
        );
        let mut r = crate::hevc::bits::BitReader::new(&data[start..]);
        let (sy, sc) = (self.pic.bd_y - bl, self.pic.bd_c - bc);
        for y in 0..n {
            for x in 0..n {
                let v = r.read_bits(bl)? as u16;
                self.pic.plane(0).set(
                    self.pic.at(0, x0 as usize + x, y0 as usize + y),
                    P::from_i32((v << sy) as i32),
                );
            }
        }
        for c in 1..3 {
            for y in 0..n / 2 {
                for x in 0..n / 2 {
                    let v = r.read_bits(bc)? as u16;
                    self.pic.plane(c).set(
                        self.pic.at(c, x0 as usize / 2 + x, y0 as usize / 2 + y),
                        P::from_i32((v << sc) as i32),
                    );
                }
            }
        }
        let end = start + r.position().div_ceil(8);
        self.c.set_byte_pos(end);
        self.c.init_engine()
    }

    fn parse_intra_modes(&mut self, x0: i32, y0: i32, n: i32, part: PartMode) -> Result<()> {
        let nb = if part == PartMode::PNxN { 2 } else { 1 };
        let pb = n / nb;
        let mut prev = [false; 4];
        for (i, p) in prev.iter_mut().enumerate().take((nb * nb) as usize) {
            let _ = i;
            *p = self.dd(PREV_INTRA_LUMA) == 1;
        }
        for j in 0..nb {
            for i in 0..nb {
                let k = (j * nb + i) as usize;
                let (xp, yp) = (x0 + i * pb, y0 + j * pb);
                let cand_a = if self.pic.zavail(xp, yp, xp - 1, yp, self.slice_addr) {
                    let b = self.pic.blk_at(xp - 1, yp);
                    if b.flags & F_INTRA != 0 {
                        b.ipm as u32
                    } else {
                        1
                    }
                } else {
                    1
                };
                let cand_b = if self.pic.zavail(xp, yp, xp, yp - 1, self.slice_addr)
                    && yp > ((yp >> self.sps.log2_ctb) << self.sps.log2_ctb)
                {
                    let b = self.pic.blk_at(xp, yp - 1);
                    if b.flags & F_INTRA != 0 {
                        b.ipm as u32
                    } else {
                        1
                    }
                } else {
                    1
                };
                let mut list = if cand_a == cand_b {
                    if cand_a < 2 {
                        [0, 1, 26]
                    } else {
                        [
                            cand_a,
                            2 + ((cand_a + 29) % 32),
                            2 + ((cand_a - 2 + 1) % 32),
                        ]
                    }
                } else {
                    let c = if cand_a != 0 && cand_b != 0 {
                        0
                    } else if cand_a != 1 && cand_b != 1 {
                        1
                    } else {
                        26
                    };
                    [cand_a, cand_b, c]
                };
                let mode = if prev[k] {
                    let idx = if self.bypass() == 0 {
                        0
                    } else if self.bypass() == 0 {
                        1
                    } else {
                        2
                    };
                    list[idx]
                } else {
                    let mut m = self.bypass_bits(5);
                    list.sort_unstable();
                    for &c in &list {
                        if m >= c {
                            m += 1;
                        }
                    }
                    m
                };
                self.fill_blk(xp, yp, pb, pb, |b| b.ipm = mode as u8);
            }
        }
        let icpm = if self.dd(INTRA_CHROMA) == 0 {
            4
        } else {
            self.bypass_bits(2)
        };
        let luma = self.pic.blk_at(x0, y0).ipm as u32;
        self.intra_chroma_mode = match icpm {
            4 => luma,
            _ => {
                let m = [0, 26, 10, 1][icpm as usize];
                if m == luma { 34 } else { m }
            }
        };
        Ok(())
    }

    fn transform_tree(
        &mut self,
        x0: i32,
        y0: i32,
        xb: i32,
        yb: i32,
        log2: u32,
        depth: u32,
        blk_idx: u32,
        parent_cbf_c: [bool; 2],
    ) -> Result<()> {
        let split = if log2 <= self.sps.log2_max_tb
            && log2 > self.sps.log2_min_tb
            && depth < self.max_trafo_depth
            && !(self.intra_split && depth == 0)
        {
            self.dd(SPLIT_TRANSFORM + (5 - log2 as usize)) == 1
        } else {
            let inter_split = self.sps.max_th_depth_inter == 0
                && !self.cu_intra
                && self.cu_part != PartMode::P2Nx2N
                && depth == 0;
            log2 > self.sps.log2_max_tb || (self.intra_split && depth == 0) || inter_split
        };
        let mut cbf_c = [false, false];
        if log2 > 2 {
            for (c, cbf) in cbf_c.iter_mut().enumerate() {
                if depth == 0 || parent_cbf_c[c] {
                    *cbf = self.dd(CBF_CHROMA + depth as usize) == 1;
                }
            }
        } else {
            cbf_c = parent_cbf_c;
            if depth == 0 {
                cbf_c = [false, false];
            }
        }
        if split {
            let h = 1 << (log2 - 1);
            self.transform_tree(x0, y0, x0, y0, log2 - 1, depth + 1, 0, cbf_c)?;
            self.transform_tree(x0 + h, y0, x0, y0, log2 - 1, depth + 1, 1, cbf_c)?;
            self.transform_tree(x0, y0 + h, x0, y0, log2 - 1, depth + 1, 2, cbf_c)?;
            self.transform_tree(x0 + h, y0 + h, x0, y0, log2 - 1, depth + 1, 3, cbf_c)?;
            return Ok(());
        }
        let cbf_luma = if self.cu_intra || depth != 0 || cbf_c[0] || cbf_c[1] {
            self.dd(CBF_LUMA + if depth == 0 { 1 } else { 0 }) == 1
        } else {
            true
        };
        let n = 1 << log2;
        if depth > 0 {
            self.mark_edges(x0, y0, n, n, E_TU_V, E_TU_H);
        }
        self.transform_unit(x0, y0, xb, yb, log2, blk_idx, cbf_luma, cbf_c)
    }

    fn transform_unit(
        &mut self,
        x0: i32,
        y0: i32,
        xb: i32,
        yb: i32,
        log2: u32,
        blk_idx: u32,
        cbf_luma: bool,
        cbf_c: [bool; 2],
    ) -> Result<()> {
        let n = 1i32 << log2;
        let chroma_here = log2 > 2 || blk_idx == 3;
        let cbf_chroma = cbf_c[0] || cbf_c[1];
        if (cbf_luma || cbf_chroma) && self.pps.cu_qp_delta_enabled && !self.is_cu_qp_delta_coded {
            let mut v = 0;
            while v < 5 && self.dd(CU_QP_DELTA_ABS + if v == 0 { 0 } else { 1 }) == 1 {
                v += 1;
            }
            if v == 5 {
                let mut k = 0;
                while self.bypass() == 1 {
                    v += 1 << k;
                    k += 1;
                    ensure!(k < 32, "invalid cu_qp_delta_abs");
                }
                v += self.bypass_bits(k) as i32;
            }
            if v != 0 && self.bypass() == 1 {
                v = -v;
            }
            let off = 6 * (self.pic.bd_y as i32 - 8);
            ensure!(
                v >= -(26 + off / 2) && v <= 25 + off / 2,
                "CuQpDeltaVal out of range"
            );
            self.is_cu_qp_delta_coded = true;
            self.cu_qp_delta = v;
            self.update_qp();
        }
        if self.cu_intra {
            let mode = self.pic.blk_at(x0, y0).ipm as u32;
            self.intra_predict(x0, y0, log2, 0, mode);
        }
        if cbf_luma {
            let qp_bd = 6 * (self.pic.bd_y as i32 - 8);
            let qp = self.qp_y + qp_bd;
            self.residual(x0, y0, log2, 0, qp)?;
            self.fill_blk(x0, y0, n, n, |b| b.flags |= F_CBF);
        }
        if !chroma_here {
            return Ok(());
        }
        let (xc, yc, log2c) = if log2 > 2 {
            (x0 / 2, y0 / 2, log2 - 1)
        } else {
            (xb / 2, yb / 2, 2)
        };
        for c in 1..3 {
            if self.cu_intra {
                let mode = self.intra_chroma_mode;
                self.intra_predict(xc, yc, log2c, c, mode);
            }
            if cbf_c[c - 1] {
                let qp = if c == 1 { self.qp_cb } else { self.qp_cr };
                self.residual(xc, yc, log2c, c, qp)?;
            }
        }
        Ok(())
    }

    fn intra_predict(&mut self, x0: i32, y0: i32, log2: u32, c: usize, mode: u32) {
        let n = 1usize << log2;
        let sc = if c == 0 { 1 } else { 2 };
        let (xl, yl) = (x0 * sc, y0 * sc);
        let unit = 4 / sc as usize;
        let stride = self.pic.stride[c];
        let width = if c == 0 {
            self.pic.width
        } else {
            self.pic.cwidth
        };
        let constrained = self.pps.constrained_intra_pred;
        let mut al = [false; 129];
        let mut at = [false; 129];
        let addr = self.slice_addr;
        let avail = |pic: &PicState<P>, xn: i32, yn: i32| -> bool {
            pic.zavail(xl, yl, xn, yn, addr)
                && (!constrained || pic.blk_at(xn, yn).flags & F_INTRA != 0)
        };
        let plane = self.pic.plane(c);
        let org = self.pic.org[c];
        let ctb_lines = (1usize << self.pic.log2_ctb) / sc as usize;
        let above = (y0 as usize).wrapping_sub(1);
        let (top_src, top_base) = if y0 > 0 && (y0 as usize).is_multiple_of(ctb_lines) {
            (&self.pic.saved[c], above / ctb_lines * width)
        } else {
            (plane, org.wrapping_add(above.wrapping_mul(stride)))
        };
        let top_line = |x: usize| top_src.get(top_base.wrapping_add(x));
        let r = &mut self.refs_buf;
        if avail(self.pic, xl - 1, yl - 1) {
            al[0] = true;
            at[0] = true;
            let v = top_line((x0 - 1) as usize).into() as u16;
            r.left[0] = v;
            r.top[0] = v;
        }
        let mut k = 0;
        while k < 2 * n {
            let yn = y0 + k as i32;
            if avail(self.pic, xl - 1, yn * sc) {
                for i in 0..unit {
                    al[1 + k + i] = true;
                    r.left[1 + k + i] = plane
                        .get(org + (yn as usize + i) * stride + (x0 - 1) as usize)
                        .into() as u16;
                }
            }
            let xn = x0 + k as i32;
            if avail(self.pic, xn * sc, yl - 1) {
                let o = xn as usize;
                for i in 0..unit {
                    at[1 + k + i] = true;
                    r.top[1 + k + i] = top_line(o + i).into() as u16;
                }
            }
            k += unit;
        }
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        intra::substitute(r, &al, &at, n, bd);
        if c == 0 {
            intra::filter(r, mode, n, self.sps.strong_intra_smoothing, true, bd);
        }
        let dst = self.pic.lines_mut(c, y0 as usize, n);
        intra::predict(r, mode, n, c == 0, bd, &mut dst[x0 as usize..], stride);
    }

    fn residual(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32) -> Result<()> {
        #[cfg(target_arch = "x86_64")]
        if crate::hevc::isa::avx2() {
            return unsafe { self.residual_fast(x0, y0, log2, c, qp) };
        }
        self.residual_body(x0, y0, log2, c, qp)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt,fma")]
    fn residual_fast(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32) -> Result<()> {
        self.residual_body(x0, y0, log2, c, qp)
    }

    #[inline(always)]
    fn residual_body(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32) -> Result<()> {
        let n = 1usize << log2;
        let bypass = self.cu_transquant_bypass;
        let ts = if self.pps.transform_skip && !bypass && log2 == 2 {
            self.dd(TRANSFORM_SKIP + (c > 0) as usize) == 1
        } else {
            false
        };
        let cmax = (log2 << 1) - 1;
        let (off, shift) = if c == 0 {
            (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2)
        } else {
            (15, log2 - 2)
        };
        let mut px = 0;
        while px < cmax && self.dd(LAST_X_PREFIX + (off + (px >> shift)) as usize) == 1 {
            px += 1;
        }
        let mut py = 0;
        while py < cmax && self.dd(LAST_Y_PREFIX + (off + (py >> shift)) as usize) == 1 {
            py += 1;
        }
        let mut lx = px;
        if px > 3 {
            let nb = (px >> 1) - 1;
            lx = (1 << nb) * (2 + (px & 1)) + self.bypass_bits(nb);
        }
        let mut ly = py;
        if py > 3 {
            let nb = (py >> 1) - 1;
            ly = (1 << nb) * (2 + (py & 1)) + self.bypass_bits(nb);
        }
        let scan_idx = if self.cu_intra && (log2 == 2 || (log2 == 3 && c == 0)) {
            let m = if c == 0 {
                self.pic.blk_at(x0, y0).ipm as u32
            } else {
                self.intra_chroma_mode
            };
            if (6..=14).contains(&m) {
                2
            } else if (22..=30).contains(&m) {
                1
            } else {
                0
            }
        } else {
            0
        };
        if scan_idx == 2 {
            std::mem::swap(&mut lx, &mut ly);
        }
        ensure!(
            (lx as usize) < n && (ly as usize) < n,
            "last significant coefficient outside the block"
        );
        let log2sb = log2 - 2;
        let sb_scan = scan_order(log2sb, scan_idx);
        let pos_scan = &SCAN_4[scan_idx];
        let sb_w = 1usize << log2sb;
        let last_sb = sb_scan
            .iter()
            .position(|&(x, y)| x as u32 == lx >> 2 && y as u32 == ly >> 2)
            .unwrap_or(0);
        let last_pos = pos_scan
            .iter()
            .position(|&(x, y)| x as u32 == lx & 3 && y as u32 == ly & 3)
            .unwrap_or(0);
        self.coeffs[..n * n].fill(0);
        let mut csbf = [0u8; 64];
        let mut prev_g1: Option<u32> = None;
        let sdh = self.pps.sign_data_hiding && !bypass;
        let (mut max_x, mut max_y) = (0usize, 0usize);
        let c_off_sig = if c == 0 { 0 } else { 27 };
        for i in (0..=last_sb).rev() {
            let (xs, ys) = (sb_scan[i].0 as usize, sb_scan[i].1 as usize);
            let mut infer_dc = false;
            let right = if xs + 1 < sb_w {
                csbf[ys * 8 + xs + 1]
            } else {
                0
            };
            let below = if ys + 1 < sb_w {
                csbf[(ys + 1) * 8 + xs]
            } else {
                0
            };
            if i < last_sb && i > 0 {
                let inc = (right | below).min(1) as usize + if c > 0 { 2 } else { 0 };
                csbf[ys * 8 + xs] = self.c.decode_decision(CODED_SUB_BLOCK + inc) as u8;
                infer_dc = true;
            } else {
                csbf[ys * 8 + xs] = 1;
            }
            let mut sig = [0u8; 16];
            let mut nsig = 0;
            let start: i32 = if i == last_sb {
                sig[0] = last_pos as u8;
                nsig = 1;
                last_pos as i32 - 1
            } else {
                15
            };
            if csbf[ys * 8 + xs] != 0 {
                let prev_csbf = (right + (below << 1)).min(3) as usize;
                let mut tab = [0u8; 16];
                let add = if c == 0 {
                    (if xs + ys > 0 { 3 } else { 0 })
                        + if log2 == 3 {
                            if scan_idx == 0 { 9 } else { 15 }
                        } else {
                            21
                        }
                } else if log2 == 3 {
                    9
                } else {
                    12
                };
                for (k, t) in tab.iter_mut().enumerate() {
                    let (xp, yp) = (pos_scan[k].0 as usize, pos_scan[k].1 as usize);
                    let ctx = if log2 == 2 {
                        CTX_IDX_MAP[(yp << 2) + xp] as usize
                    } else if xs + ys + xp + yp == 0 {
                        0
                    } else {
                        SIG_PATTERN[prev_csbf][(yp << 2) + xp] as usize + add
                    };
                    *t = (SIG_COEFF + c_off_sig + ctx) as u8;
                }
                let mut np = start;
                while np >= 0 {
                    if np > 0 || !infer_dc {
                        if self.c.decode_decision(tab[np as usize] as usize) == 1 {
                            sig[nsig] = np as u8;
                            nsig += 1;
                            infer_dc = false;
                        }
                    } else {
                        sig[nsig] = 0;
                        nsig += 1;
                    }
                    np -= 1;
                }
            }
            if nsig == 0 {
                continue;
            }
            let mut ctx_set = if i == 0 || c > 0 { 0 } else { 2 };
            if let Some(g) = prev_g1
                && g == 0
            {
                ctx_set += 1;
            }
            let mut g1ctx = 1u32;
            let mut gt1 = [0u8; 16];
            let mut first_g1: Option<usize> = None;
            let c_off_g1 = if c > 0 { 16 } else { 0 };
            for k in 0..nsig.min(8) {
                let inc = ctx_set * 4 + g1ctx.min(3) as usize + c_off_g1;
                let f = self.c.decode_decision(GT1 + inc);
                gt1[k] = f as u8;
                if f == 1 {
                    g1ctx = 0;
                    if first_g1.is_none() {
                        first_g1 = Some(k);
                    }
                } else if g1ctx > 0 {
                    g1ctx += 1;
                }
            }
            prev_g1 = Some(g1ctx);
            let mut gt2 = 0u8;
            if let Some(k) = first_g1 {
                gt2 = self
                    .c
                    .decode_decision(GT2 + ctx_set + if c > 0 { 4 } else { 0 })
                    as u8;
                let _ = k;
            }
            let last_sig_pos = sig[0] as i32;
            let first_sig_pos = sig[nsig - 1] as i32;
            let sign_hidden = sdh && last_sig_pos - first_sig_pos > 3;
            let nsigns = if sign_hidden { nsig - 1 } else { nsig };
            let mut signs = if nsigns > 0 {
                self.bypass_bits(nsigns as u32) << (32 - nsigns)
            } else {
                0
            };
            let mut rice = 0u32;
            let mut sum_abs = 0i32;
            for k in 0..nsig {
                let mut base = 1 + gt1[k] as i32;
                if Some(k) == first_g1 {
                    base += gt2 as i32;
                }
                let thresh = if k < 8 {
                    if Some(k) == first_g1 { 3 } else { 2 }
                } else {
                    1
                };
                let mut level = base;
                if base == thresh {
                    let rem = self.coeff_abs_level_remaining(rice)?;
                    level = base + rem;
                    if level > 3 * (1 << rice) {
                        rice = (rice + 1).min(4);
                    }
                }
                let np = sig[k] as usize;
                let (xc, yc) = (
                    xs * 4 + pos_scan[np].0 as usize,
                    ys * 4 + pos_scan[np].1 as usize,
                );
                let mut v = level;
                if !(sign_hidden && k == nsig - 1) {
                    if signs & 0x8000_0000 != 0 {
                        v = -v;
                    }
                    signs <<= 1;
                }
                if sign_hidden {
                    sum_abs += level;
                    if k == nsig - 1 && sum_abs & 1 == 1 {
                        v = -v;
                    }
                }
                self.coeffs[yc * n + xc] = v.clamp(-32768, 32767) as i16;
                max_x = max_x.max(xc);
                max_y = max_y.max(yc);
            }
        }
        self.reconstruct(x0, y0, log2, c, qp, ts, max_x, max_y)
    }

    #[inline(always)]
    fn coeff_abs_level_remaining(&mut self, rice: u32) -> Result<i32> {
        let mut prefix = 0u32;
        while prefix < 32 && self.bypass() == 1 {
            prefix += 1;
        }
        ensure!(prefix < 32, "invalid coeff_abs_level_remaining");
        if prefix <= 3 {
            Ok(((prefix << rice) + self.bypass_bits(rice)) as i32)
        } else {
            let nb = prefix - 3 + rice;
            ensure!(nb <= 24, "coeff_abs_level_remaining too large");
            Ok(((((1u32 << (prefix - 3)) + 2) << rice) + self.bypass_bits(nb)) as i32)
        }
    }

    fn reconstruct(
        &mut self,
        x0: i32,
        y0: i32,
        log2: u32,
        c: usize,
        qp: i32,
        ts: bool,
        max_x: usize,
        max_y: usize,
    ) -> Result<()> {
        let n = 1usize << log2;
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        let coeffs = &mut self.coeffs[..n * n];
        let mut dc_only = false;
        if !self.cu_transquant_bypass {
            let bd_shift = bd + log2 - 5;
            let round = 1i64 << (bd_shift - 1);
            let scale = (LEVEL_SCALE[(qp % 6) as usize] as i64) << (qp / 6);
            let m: Option<&[u8]> = match &self.pic.scaling {
                Some(s) if !(ts && n > 4) => {
                    let size_id = (log2 - 2) as usize;
                    let matrix_id = if self.cu_intra { c } else { 3 + c };
                    Some(&s[size_id * 6 + matrix_id])
                }
                _ => None,
            };
            transform::dequant(coeffs, n, max_x, max_y, m, scale, round, bd_shift);
            let bd_shift = 20 - bd;
            if ts {
                transform::transform_skip(coeffs, n, bd_shift);
            } else if max_x == 0 && max_y == 0 && !(self.cu_intra && c == 0 && n == 4) {
                let v = transform::inverse_dc(coeffs[0] as i32, bd_shift);
                coeffs.fill(v.clamp(-32768, 32767) as i16);
                dc_only = true;
            } else {
                let dst = self.cu_intra && c == 0 && n == 4;
                transform::inverse_transform(coeffs, n, dst, max_x, max_y, bd_shift);
            }
        }
        let _ = dc_only;
        let (stride, max) = (self.pic.stride[c], (1i32 << bd) - 1);
        let dst = self.pic.lines_mut(c, y0 as usize, n);
        transform::add_residual(&mut dst[x0 as usize..], stride, n, coeffs, max);
        Ok(())
    }

    fn prediction_unit(
        &mut self,
        xc: i32,
        yc: i32,
        ncb: i32,
        xp: i32,
        yp: i32,
        w: i32,
        h: i32,
        part_idx: usize,
        part: PartMode,
        skip: bool,
    ) -> Result<bool> {
        let merge = if skip { true } else { self.dd(MERGE_FLAG) == 1 };
        let mvf = if merge {
            let idx = if self.sh.max_num_merge_cand > 1 {
                let mut i = 0;
                if self.dd(MERGE_IDX) == 1 {
                    i = 1;
                    while i < self.sh.max_num_merge_cand - 1 && self.bypass() == 1 {
                        i += 1;
                    }
                }
                i
            } else {
                0
            };
            self.derive_merge(xc, yc, ncb, xp, yp, w, h, part_idx, part, idx as usize)
        } else {
            let pred_idc = if self.sh.is_b() {
                if w + h != 12 {
                    let depth = self.pic.blk_at(xc, yc).depth as usize;
                    if self.dd(INTER_PRED_IDC + depth) == 1 {
                        2
                    } else {
                        self.dd(INTER_PRED_IDC + 4)
                    }
                } else {
                    self.dd(INTER_PRED_IDC + 4)
                }
            } else {
                0
            };
            let mut ref_idx = [-1i8; 2];
            let mut mvd = [[0i32; 2]; 2];
            let mut mvp_flag = [0u32; 2];
            for l in 0..2 {
                let used = if l == 0 { pred_idc != 1 } else { pred_idc != 0 };
                if !used {
                    continue;
                }
                let nref = self.sh.num_ref_idx[l];
                let mut ri = 0;
                if nref > 1 {
                    while ri < nref - 1 {
                        let bin = if ri < 2 {
                            self.dd(REF_IDX + ri as usize)
                        } else {
                            self.bypass()
                        };
                        if bin == 0 {
                            break;
                        }
                        ri += 1;
                    }
                }
                ref_idx[l] = ri as i8;
                if l == 1 && self.sh.mvd_l1_zero && pred_idc == 2 {
                    mvd[1] = [0, 0];
                } else {
                    mvd[l] = self.mvd_coding()?;
                }
                mvp_flag[l] = self.dd(MVP_FLAG);
            }
            let mut f = MvField {
                mv: [[0; 2]; 2],
                ref_idx,
            };
            for l in 0..2 {
                if ref_idx[l] < 0 {
                    continue;
                }
                let mvp = self.derive_amvp(
                    xc,
                    yc,
                    ncb,
                    xp,
                    yp,
                    w,
                    h,
                    part_idx,
                    l,
                    ref_idx[l] as usize,
                    mvp_flag[l] as usize,
                );
                f.mv[l] = [
                    (mvp[0] as i32 + mvd[l][0]) as i16,
                    (mvp[1] as i32 + mvd[l][1]) as i16,
                ];
            }
            f
        };
        for l in 0..2 {
            if mvf.ref_idx[l] >= 0 {
                ensure!(
                    (mvf.ref_idx[l] as usize) < self.refs[l].len(),
                    "reference index out of range"
                );
            }
        }
        let w4 = self.pic.w4;
        for by in (yp >> 2)..((yp + h) >> 2) {
            let o = by as usize * w4;
            self.pic
                .mvf
                .slice_mut(o + (xp >> 2) as usize..o + ((xp + w) >> 2) as usize)
                .fill(mvf);
        }
        self.motion_compensate(xp, yp, w as usize, h as usize, &mvf);
        Ok(merge)
    }

    fn mvd_coding(&mut self) -> Result<[i32; 2]> {
        let g0 = [self.dd(ABS_MVD_GT0), self.dd(ABS_MVD_GT0)];
        let mut g1 = [0, 0];
        for k in 0..2 {
            if g0[k] == 1 {
                g1[k] = self.dd(ABS_MVD_GT1);
            }
        }
        let mut out = [0i32; 2];
        for k in 0..2 {
            if g0[k] == 1 {
                let mut v = 1i32;
                if g1[k] == 1 {
                    let mut kk = 1;
                    let mut val = 0i32;
                    while self.bypass() == 1 {
                        val += 1 << kk;
                        kk += 1;
                        ensure!(kk < 31, "invalid abs_mvd_minus2");
                    }
                    val += self.bypass_bits(kk) as i32;
                    v = val + 2;
                }
                if self.bypass() == 1 {
                    v = -v;
                }
                out[k] = v;
            }
        }
        Ok(out)
    }

    fn motion_compensate(&mut self, xp: i32, yp: i32, w: usize, h: usize, f: &MvField) {
        let weighted = match self.sh.slice_type {
            SliceType::P => self.pps.weighted_pred,
            SliceType::B => self.pps.weighted_bipred,
            SliceType::I => false,
        };
        let bi = f.pred(0) && f.pred(1);
        for c in 0..3 {
            let (x, y, bw, bh, bd) = if c == 0 {
                (xp, yp, w, h, self.pic.bd_y)
            } else {
                (xp / 2, yp / 2, w / 2, h / 2, self.pic.bd_c)
            };
            let mut lists = [false; 2];
            for l in 0..2 {
                if !f.pred(l) {
                    continue;
                }
                lists[l] = true;
                let frame = &self.refs[l][f.ref_idx[l] as usize].frame;
                let buf = if l == 0 || !bi {
                    &mut self.pred0
                } else {
                    &mut self.pred1
                };
                if c == 0 {
                    inter::mc_luma(frame, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                } else {
                    inter::mc_chroma(frame, c - 1, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                }
            }
            let stride = self.pic.stride[c];
            let dst = &mut self.pic.lines_mut(c, y as usize, bh)[x as usize..];
            let Some(pwt) = self.sh.pwt.as_ref().filter(|_| weighted) else {
                if bi {
                    inter::put_bi(&self.pred0, &self.pred1, bw, bh, bd, dst, stride);
                } else {
                    inter::put_uni(&self.pred0, bw, bh, bd, dst, stride);
                }
                continue;
            };
            let shift1 = 14 - bd;
            let log2wd = if c == 0 {
                pwt.luma_log2_denom
            } else {
                pwt.chroma_log2_denom
            } + shift1;
            let get = |l: usize| -> (i32, i32) {
                let e = &pwt.l[l][f.ref_idx[l] as usize];
                let (wt, o) = if c == 0 { e.luma } else { e.chroma[c - 1] };
                (wt, o << (bd - 8))
            };
            if bi {
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                inter::put_weighted_bi(
                    &self.pred0,
                    &self.pred1,
                    bw,
                    bh,
                    bd,
                    log2wd,
                    w0,
                    w1,
                    o0,
                    o1,
                    dst,
                    stride,
                );
            } else {
                let l = if lists[0] { 0 } else { 1 };
                let (wt, o) = get(l);
                inter::put_weighted_uni(&self.pred0, bw, bh, bd, log2wd, wt, o, dst, stride);
            }
        }
    }
}
