use crate::media::{Frame as Image, Packet, PixelFormat, TimeBase};
use crate::vp9::error::{Error, Result, ensure};
use crate::vp9::frame::{Frame, FrameInfo, MiGrid, MiInfo, Pools, RowSync};
use crate::vp9::header::{
    FrameHeader, HeaderState, KEY_FRAME, RefInfo, Segmentation, parse_compressed,
    parse_uncompressed, split_superframe,
};
use crate::vp9::loopfilter::{LfFrame, PlaneView, filter_superblock};
use crate::vp9::probs::{Counts, adapt_coef_probs, adapt_noncoef_probs};
use crate::vp9::tables::*;
use crate::vp9::tile::{FrameShared, RefUse, Strip, TileDecoder};
use crate::vp9::{ColorInfo, Picture, Plane};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub frames: u64,
    pub shown: u64,
    pub hidden: u64,
    pub key_frames: u64,
    pub intra_only: u64,
    pub inter_frames: u64,
    pub show_existing: u64,
    pub superframes: u64,
    pub error_resilient: u64,
    pub no_backward_adaptation: u64,
    pub size_changes: u64,
    pub scaled_ref_blocks: u64,
    pub compound_blocks: u64,
    pub intra_blocks: u64,
    pub inter_blocks: u64,
    pub lossless_frames: u64,
    pub max_tile_cols: u32,
    pub max_tile_rows: u32,
    pub segmentation_frames: u64,
    pub tx_select_frames: u64,
    pub switchable_interp_frames: u64,
    pub high_precision_mv_frames: u64,
    pub compound_frames: u64,
    pub bit_depths: [u64; 3],
    pub profiles: [u64; 4],
    pub draft_frames: u64,
}

enum Pending {
    Ready(Picture),
    Job(std::sync::mpsc::Receiver<Picture>),
    Existing(Arc<Frame>, i64, bool),
}

pub struct Decoder {
    st: HeaderState,
    slots: [Option<Arc<Frame>>; 8],
    prev_mi: Option<Arc<MiGrid>>,
    prev_seg_ids: Vec<u8>,
    last_size: Option<(u32, u32)>,
    last_show_frame: bool,
    stats: DecodeStats,
    threads: usize,
    bufs: Arc<Pools>,
    draft: bool,
    time_base: TimeBase,
    pending: VecDeque<Pending>,
    jobs: VecDeque<std::thread::JoinHandle<()>>,
    pool: Option<rayon::ThreadPool>,
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

fn tile_offset(i: usize, mis: usize, log2: u32) -> usize {
    let sbs = (mis + 7) >> 3;
    (((i * sbs) >> log2) << 3).min(mis)
}

impl Decoder {
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    pub fn with_threads(threads: usize) -> Self {
        let pool = if threads > 1 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("vp9-{i}"))
                .build()
                .ok()
        } else {
            None
        };
        Decoder {
            st: HeaderState::default(),
            slots: Default::default(),
            prev_mi: None,
            prev_seg_ids: Vec::new(),
            last_size: None,
            last_show_frame: false,
            stats: DecodeStats::default(),
            threads: threads.max(1),
            bufs: Arc::default(),
            draft: false,
            time_base: TimeBase::new(1, 90000),
            pending: VecDeque::new(),
            jobs: VecDeque::new(),
            pool,
        }
    }

    pub fn stats(&self) -> DecodeStats {
        self.stats.clone()
    }

    pub fn threads(&self) -> usize {
        if self.frame_threads() {
            self.threads
        } else {
            1
        }
    }

    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub fn set_time_base(&mut self, time_base: TimeBase) {
        self.time_base = time_base;
    }

    pub fn decode(&mut self, packet: &Packet) -> crate::media::Result<Vec<Image>> {
        let pics = self.decode_pictures(&packet.data, packet.pts)?;
        pics.into_iter()
            .map(|p| image_of(p, self.time_base))
            .collect()
    }

    pub fn flush(&mut self) -> crate::media::Result<Vec<Image>> {
        let time_base = self.time_base;
        self.flush_pictures()
            .into_iter()
            .map(|p| image_of(p, time_base))
            .collect()
    }

    pub fn decode_pictures(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let frames = split_superframe(data);
        if frames.len() > 1 {
            self.stats.superframes += 1;
        }
        for f in frames {
            if f.is_empty() {
                continue;
            }
            self.decode_frame(f, pts)?;
        }
        Ok(self.drain(false))
    }

    pub fn flush_pictures(&mut self) -> Vec<Picture> {
        let out = self.drain(true);
        for j in self.jobs.drain(..) {
            let _ = j.join();
        }
        out
    }

    pub fn reset(&mut self) {
        self.st = HeaderState::default();
        self.slots = Default::default();
        self.prev_mi = None;
        self.prev_seg_ids = Vec::new();
        self.last_size = None;
        self.last_show_frame = false;
        self.pending.clear();
    }

    fn frame_threads(&self) -> bool {
        self.pool.is_some()
    }

    fn drain(&mut self, all: bool) -> Vec<Picture> {
        let max_lag = (self.threads / 2).clamp(2, 6);
        let mut out = Vec::new();
        loop {
            let must = all || self.pending.len() > max_lag;
            let Some(front) = self.pending.front_mut() else {
                break;
            };
            let pic = match front {
                Pending::Ready(_) => match self.pending.pop_front() {
                    Some(Pending::Ready(p)) => Some(p),
                    _ => None,
                },
                Pending::Job(rx) => {
                    let p = if must {
                        rx.recv().ok()
                    } else {
                        match rx.try_recv() {
                            Ok(p) => Some(p),
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
                        }
                    };
                    self.pending.pop_front();
                    p
                }
                Pending::Existing(f, pts, draft) => {
                    if !must && !f.is_complete() {
                        break;
                    }
                    let p = picture_of(f, *pts, *draft);
                    self.pending.pop_front();
                    Some(p)
                }
            };
            out.extend(pic);
        }
        out
    }

    fn decode_frame(&mut self, data: &[u8], pts: i64) -> Result<()> {
        let refs: [Option<RefInfo>; 8] = std::array::from_fn(|i| {
            self.slots[i].as_ref().map(|f| RefInfo {
                width: f.info.width,
                height: f.info.height,
            })
        });
        let mut h = parse_uncompressed(data, &mut self.st, &refs)?;
        if h.show_existing_frame {
            self.stats.show_existing += 1;
            let f = self.slots[h.frame_to_show_map_idx as usize]
                .clone()
                .ok_or_else(|| {
                    Error::MissingReference(format!(
                        "show_existing_frame of empty slot {}",
                        h.frame_to_show_map_idx
                    ))
                })?;
            self.stats.shown += 1;
            self.pending.push_back(Pending::Existing(f, pts, false));
            return Ok(());
        }
        ensure!(
            h.width <= 16384 && h.height <= 16384,
            "frame size {}x{} too large",
            h.width,
            h.height
        );
        let (mi_rows, mi_cols) = (h.mi_rows as usize, h.mi_cols as usize);
        let size = (h.width, h.height);
        let size_changed = self.last_size != Some(size);
        if size_changed && self.last_size.is_some() {
            self.stats.size_changes += 1;
        }
        if self.st.reset_segment_map || size_changed || self.prev_seg_ids.len() != mi_rows * mi_cols
        {
            self.prev_seg_ids = vec![0; mi_rows * mi_cols];
            self.st.reset_segment_map = false;
        }
        let use_prev = !size_changed
            && self.last_show_frame
            && !h.error_resilient_mode
            && !h.frame_is_intra
            && self
                .prev_mi
                .as_ref()
                .is_some_and(|m| m.rows == mi_rows && m.cols == mi_cols);
        self.last_size = Some(size);
        self.last_show_frame = h.show_frame;

        let off = h.uncompressed_size;
        let hs = h.header_size_in_bytes as usize;
        ensure!(
            off + hs <= data.len(),
            "compressed header exceeds frame data"
        );
        let mut fc = self.st.contexts[h.frame_context_idx as usize].clone();
        parse_compressed(&data[off..off + hs], &mut h, &mut fc)?;
        self.count_frame(&h);

        let mut refs: [Option<RefUse>; 3] = [None, None, None];
        let ref_frames: Vec<Option<Arc<Frame>>> = (0..3)
            .map(|i| {
                if h.frame_is_intra {
                    None
                } else {
                    self.slots[h.ref_frame_idx[i] as usize].clone()
                }
            })
            .collect();
        if !h.frame_is_intra {
            for i in 0..3 {
                let Some(rf) = &ref_frames[i] else {
                    return Err(Error::MissingReference(format!(
                        "reference slot {} is empty",
                        h.ref_frame_idx[i]
                    )));
                };
                let ri = &rf.info;
                ensure!(
                    ri.bit_depth == h.color.bit_depth
                        && ri.ss_x == h.color.subsampling_x
                        && ri.ss_y == h.color.subsampling_y,
                    "reference frame format differs from the current frame"
                );
                let (rw, rh) = (ri.width as i64, ri.height as i64);
                let (w, hh) = (h.width as i64, h.height as i64);
                if 2 * w >= rw && 2 * hh >= rh && w <= 16 * rw && hh <= 16 * rh {
                    let x_scale = ((rw << 14) / w) as i32;
                    let y_scale = ((rh << 14) / hh) as i32;
                    refs[i] = Some(RefUse {
                        frame: rf,
                        x_scale,
                        y_scale,
                        x_step: (16 * x_scale) >> 14,
                        y_step: (16 * y_scale) >> 14,
                    });
                }
            }
        }
        let seg = self.st.seg.clone();
        let seg_q = segment_quant(&h, &seg);
        let tile_cols = 1usize << h.tile_cols_log2;
        let tile_rows = 1usize << h.tile_rows_log2;
        let mut pos = off + hs;
        let mut tile_data: Vec<Vec<&[u8]>> = vec![Vec::with_capacity(tile_rows); tile_cols];
        for tr in 0..tile_rows {
            for (tc, td) in tile_data.iter_mut().enumerate() {
                let last = tr == tile_rows - 1 && tc == tile_cols - 1;
                let sz = if last {
                    data.len() - pos
                } else {
                    let Some(&b) = data.get(pos..).and_then(|d| d.first_chunk::<4>()) else {
                        return Err(Error::Invalid("tile size beyond frame data".into()));
                    };
                    let s = u32::from_be_bytes(b) as usize;
                    pos += 4;
                    ensure!(s <= data.len() - pos, "tile size {s} beyond frame data");
                    s
                };
                td.push(&data[pos..pos + sz]);
                pos += sz;
            }
        }
        let counting = !h.error_resilient_mode && !h.frame_parallel_decoding_mode;
        let prev_mi = if use_prev { self.prev_mi.clone() } else { None };
        let frame = Arc::new(Frame::new(
            FrameInfo {
                width: h.width,
                height: h.height,
                ss_x: h.color.subsampling_x,
                ss_y: h.color.subsampling_y,
                bit_depth: h.color.bit_depth,
                color_space: h.color.color_space,
                color_range: h.color.color_range,
                render_width: h.render_width,
                render_height: h.render_height,
                key: h.frame_type == KEY_FRAME,
                intra_only: h.intra_only,
            },
            self.bufs.clone(),
        ));
        let mi = Arc::new(MiGrid::new(
            mi_rows,
            mi_cols,
            self.bufs.mi.take_any(mi_rows * mi_cols, MiInfo::default()),
        ));
        let sync = Arc::new(RowSync::new(h.sb64_rows as usize, tile_cols));
        let draft =
            self.draft && h.refresh_frame_flags == 0 && h.show_frame && self.st.lf.level > 0;
        self.stats.draft_frames += draft as u64;
        let lf = (self.st.lf.level > 0 && !draft).then(|| {
            LfFrame::new(
                &self.st.lf,
                &seg,
                mi_rows,
                mi_cols,
                h.color.subsampling_x,
                h.color.subsampling_y,
                h.color.bit_depth,
            )
        });
        let post = self.start_post(PostJob {
            frame: frame.clone(),
            mi: mi.clone(),
            sync: sync.clone(),
            lf,
            out: h.show_frame.then_some((pts, draft)),
        });
        let frame_ref = &*frame;
        let shared = FrameShared {
            h: &h,
            fc: &fc,
            seg: &seg,
            prev_seg_ids: &self.prev_seg_ids,
            prev_mi: prev_mi.as_deref(),
            refs,
            seg_q,
            counting,
            dst: frame_ref,
            mi: &mi,
            sync: &sync,
        };
        let col_bounds: Vec<(usize, usize)> = (0..tile_cols)
            .map(|i| {
                (
                    tile_offset(i, mi_cols, h.tile_cols_log2),
                    tile_offset(i + 1, mi_cols, h.tile_cols_log2),
                )
            })
            .collect();
        let row_bounds: Vec<(usize, usize)> = (0..tile_rows)
            .map(|i| {
                (
                    tile_offset(i, mi_rows, h.tile_rows_log2),
                    tile_offset(i + 1, mi_rows, h.tile_rows_log2),
                )
            })
            .collect();
        let decode_col = |ci: usize| -> (Strip, Option<Error>) {
            let (cs, ce) = col_bounds[ci];
            let mut td = TileDecoder::new(&shared, cs, ce);
            let mut err = None;
            if ce > cs {
                for (ri, &(rs, re)) in row_bounds.iter().enumerate() {
                    if !td.decode_tile(tile_data[ci][ri], rs, re) {
                        err = Some(Error::Invalid("tile data exhausted".into()));
                        break;
                    }
                }
            }
            let e = td.error.take().or(err);
            (td.strip, e)
        };
        let results: Vec<(Strip, Option<Error>)> = self.run_parallel(tile_cols, &decode_col);
        let mut counts = Counts::default();
        let mut error = None;
        for (s, e) in results {
            if let Some(e) = e {
                error.get_or_insert(e);
            }
            if counting {
                counts.add(&s.counts);
            }
            self.stats.compound_blocks += s.compound_blocks;
            self.stats.scaled_ref_blocks += s.scaled_blocks;
            self.stats.intra_blocks += s.intra_blocks;
            self.stats.inter_blocks += s.inter_blocks;
        }
        if let Some(e) = error {
            sync.fail();
            return Err(e);
        }
        drop(ref_frames);
        drop(prev_mi);
        self.finish_post(post);
        if counting {
            let saved = &self.st.contexts[h.frame_context_idx as usize];
            let mut a = saved.clone();
            a.tx = fc.tx;
            a.skip = fc.skip;
            adapt_coef_probs(
                &mut a,
                &counts,
                h.frame_is_intra,
                self.st.last_frame_type == KEY_FRAME,
            );
            if !h.frame_is_intra {
                a.tx = saved.tx;
                a.skip = saved.skip;
                adapt_noncoef_probs(
                    &mut a,
                    &counts,
                    h.interp_filter == SWITCHABLE,
                    h.tx_mode == TX_MODE_SELECT,
                    h.allow_high_precision_mv,
                );
            }
            fc = a;
        }
        if h.refresh_frame_context {
            self.st.contexts[h.frame_context_idx as usize] = fc;
        }
        if seg.enabled && seg.update_map {
            self.prev_seg_ids = mi.seg_ids();
        }
        for i in 0..8 {
            if (h.refresh_frame_flags >> i) & 1 == 1 {
                self.slots[i] = Some(frame.clone());
            }
        }
        if let Some(old) = self.prev_mi.replace(mi)
            && let Ok(old) = Arc::try_unwrap(old)
        {
            self.bufs.mi.put(old.into_vec());
        }
        if h.show_frame {
            self.stats.shown += 1;
        } else {
            self.stats.hidden += 1;
        }
        Ok(())
    }

    fn start_post(&mut self, job: PostJob) -> PostRun {
        if !self.frame_threads() {
            return PostRun::Inline(Box::new(job));
        }
        while self.jobs.front().is_some_and(|j| j.is_finished()) {
            let _ = self.jobs.pop_front().map(|j| j.join());
        }
        let max_jobs = (self.threads / 3).clamp(2, 6);
        while self.jobs.len() >= max_jobs {
            let _ = self.jobs.pop_front().map(|j| j.join());
        }
        let shown = job.out.is_some();
        let (tx, rx) = std::sync::mpsc::channel();
        let (job_tx, job_rx) = std::sync::mpsc::channel::<PostJob>();
        let spawned = std::thread::Builder::new()
            .name("vp9-post".into())
            .spawn(move || {
                if let Ok(job) = job_rx.recv()
                    && let Some(p) = job.run()
                {
                    let _ = tx.send(p);
                }
            });
        match spawned {
            Ok(handle) => match job_tx.send(job) {
                Ok(()) => {
                    self.jobs.push_back(handle);
                    PostRun::Thread(rx, shown)
                }
                Err(std::sync::mpsc::SendError(back)) => PostRun::Inline(Box::new(back)),
            },
            Err(_) => PostRun::Inline(Box::new(job)),
        }
    }

    fn finish_post(&mut self, post: PostRun) {
        match post {
            PostRun::Thread(rx, shown) => {
                if shown {
                    self.pending.push_back(Pending::Job(rx));
                }
            }
            PostRun::Inline(job) => {
                if let Some(p) = job.run() {
                    self.pending.push_back(Pending::Ready(p));
                }
            }
        }
    }

    fn count_frame(&mut self, h: &FrameHeader) {
        let s = &mut self.stats;
        s.frames += 1;
        if h.frame_type == KEY_FRAME {
            s.key_frames += 1;
        } else if h.intra_only {
            s.intra_only += 1;
        } else {
            s.inter_frames += 1;
        }
        s.error_resilient += h.error_resilient_mode as u64;
        s.no_backward_adaptation +=
            (h.error_resilient_mode || h.frame_parallel_decoding_mode) as u64;
        s.lossless_frames += h.lossless as u64;
        s.max_tile_cols = s.max_tile_cols.max(1 << h.tile_cols_log2);
        s.max_tile_rows = s.max_tile_rows.max(1 << h.tile_rows_log2);
        s.segmentation_frames += self.st.seg.enabled as u64;
        s.tx_select_frames += (h.tx_mode == TX_MODE_SELECT) as u64;
        s.switchable_interp_frames += (!h.frame_is_intra && h.interp_filter == SWITCHABLE) as u64;
        s.high_precision_mv_frames += (!h.frame_is_intra && h.allow_high_precision_mv) as u64;
        s.compound_frames += (!h.frame_is_intra && h.reference_mode != 0) as u64;
        s.bit_depths[((h.color.bit_depth - 8) >> 1) as usize] += 1;
        s.profiles[h.profile as usize] += 1;
    }

    fn run_parallel<T: Send>(&self, n: usize, f: &(dyn Fn(usize) -> T + Sync)) -> Vec<T> {
        if let Some(pool) = &self.pool
            && n > 1
        {
            use rayon::prelude::*;
            return pool.install(|| (0..n).into_par_iter().map(f).collect());
        }
        (0..n).map(f).collect()
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        for j in self.jobs.drain(..) {
            let _ = j.join();
        }
    }
}

pub(crate) fn segment_quant(h: &FrameHeader, seg: &Segmentation) -> [[[i32; 2]; 2]; 8] {
    let bd_idx = ((h.color.bit_depth - 8) >> 1) as usize;
    let mut seg_q = [[[0i32; 2]; 2]; 8];
    for (s, q) in seg_q.iter_mut().enumerate() {
        let qindex = if seg.feature_active(s as u8, SEG_LVL_ALT_Q) {
            let d = seg.feature_data[s][SEG_LVL_ALT_Q] as i32;
            (if seg.abs_or_delta_update {
                d
            } else {
                h.base_q_idx as i32 + d
            })
            .clamp(0, 255)
        } else {
            h.base_q_idx as i32
        };
        let dc = |b: i32| DC_QLOOKUP[bd_idx * 256 + b.clamp(0, 255) as usize];
        let ac = |b: i32| AC_QLOOKUP[bd_idx * 256 + b.clamp(0, 255) as usize];
        q[0] = [dc(qindex + h.delta_q_y_dc as i32), ac(qindex)];
        q[1] = [
            dc(qindex + h.delta_q_uv_dc as i32),
            ac(qindex + h.delta_q_uv_ac as i32),
        ];
    }
    seg_q
}

enum PostRun {
    Thread(std::sync::mpsc::Receiver<Picture>, bool),
    Inline(Box<PostJob>),
}

struct PostJob {
    frame: Arc<Frame>,
    mi: Arc<MiGrid>,
    sync: Arc<RowSync>,
    lf: Option<LfFrame>,
    out: Option<(i64, bool)>,
}

struct PublishGuard<'a>(&'a Frame);

impl Drop for PublishGuard<'_> {
    fn drop(&mut self) {
        if !self.0.is_complete() {
            self.0.publish(self.0.sb_rows());
        }
    }
}

impl PostJob {
    fn run(self) -> Option<Picture> {
        let f = &*self.frame;
        let _guard = PublishGuard(f);
        let mut pic = self
            .out
            .map(|(pts, draft)| PictureBuilder::new(f, pts, draft));
        let sb_rows = f.sb_rows();
        for r in 0..sb_rows {
            if !self.sync.wait(r) {
                return None;
            }
            if let Some(lf) = &self.lf {
                let mut views: [PlaneView; 3] = std::array::from_fn(|p| band_view(f, p, r));
                for c in 0..lf.mi_cols.div_ceil(8) {
                    filter_superblock(&mut views, &self.mi, lf, r * 8, c * 8);
                }
            }
            if r > 0 {
                finish(f, r - 1, &mut pic);
            }
        }
        finish(f, sb_rows - 1, &mut pic);
        pic.map(PictureBuilder::finish)
    }
}

fn finish(f: &Frame, r: usize, pic: &mut Option<PictureBuilder>) {
    if let Some(pic) = pic {
        pic.add(f, r);
    }
    f.finish_rows(r);
    f.publish(r + 1);
}

fn band_view(f: &Frame, p: usize, r: usize) -> PlaneView<'_> {
    let g = f.geo[p];
    let y0 = (r * g.band_h).saturating_sub(8);
    let y1 = (r + 1) * g.band_h;
    let pm = f.plane_mut(p);
    PlaneView {
        data: pm.row(0, y0, (y1 - y0) * g.stride),
        stride: g.stride,
        ox: 0,
        oy: y0,
    }
}

struct PictureBuilder {
    planes: [Plane; 3],
    info: FrameInfo,
    pts: i64,
    draft: bool,
}

impl PictureBuilder {
    fn new(f: &Frame, pts: i64, draft: bool) -> Self {
        let planes = f.geo.map(|g| {
            if f.info.bit_depth == 8 {
                Plane::U8(Vec::with_capacity(g.width * g.height))
            } else {
                Plane::U16(Vec::with_capacity(g.width * g.height))
            }
        });
        PictureBuilder {
            planes,
            info: f.info,
            pts,
            draft,
        }
    }

    fn add(&mut self, f: &Frame, r: usize) {
        for p in 0..3 {
            let g = f.geo[p];
            let pm = f.plane_mut(p);
            for y in r * g.band_h..((r + 1) * g.band_h).min(g.height) {
                let src: &[u16] = pm.row(0, y, g.width);
                match &mut self.planes[p] {
                    Plane::U8(d) => narrow_row(src, d),
                    Plane::U16(d) => d.extend_from_slice(src),
                }
            }
        }
    }

    fn finish(self) -> Picture {
        let i = self.info;
        let [y, u, v] = self.planes;
        let (cw, ch) = (
            (i.width as usize + i.ss_x as usize) >> i.ss_x as usize,
            (i.height as usize + i.ss_y as usize) >> i.ss_y as usize,
        );
        Picture {
            width: i.width,
            height: i.height,
            chroma_width: cw as u32,
            chroma_height: ch as u32,
            bit_depth: i.bit_depth as u32,
            subsampling_x: i.ss_x,
            subsampling_y: i.ss_y,
            y,
            u,
            v,
            y_stride: i.width as usize,
            uv_stride: cw,
            pts: self.pts,
            key: i.key,
            intra_only: i.intra_only,
            color: ColorInfo {
                color_space: i.color_space,
                full_range: i.color_range,
            },
            render_width: i.render_width,
            render_height: i.render_height,
            draft: self.draft,
        }
    }
}

crate::vp9::isa::dispatch! {
fn narrow_row(src: &[u16], d: &mut Vec<u8>) {
    let n = d.len();
    d.resize(n + src.len(), 0);
    for (o, &s) in d[n..].iter_mut().zip(src) {
        *o = s as u8;
    }
}
}

fn picture_of(f: &Frame, pts: i64, draft: bool) -> Picture {
    f.wait(f.sb_rows());
    let mut b = PictureBuilder::new(f, pts, draft);
    for r in 0..f.sb_rows() {
        b.add(f, r);
    }
    b.finish()
}

fn image_of(p: Picture, time_base: TimeBase) -> crate::media::Result<Image> {
    if !(p.subsampling_x && p.subsampling_y) || !matches!(p.bit_depth, 8 | 10) {
        return Err(crate::media::Error::Unsupported(format!(
            "vp9: {}-bit {} as a 4:2:0 frame",
            p.bit_depth,
            match (p.subsampling_x, p.subsampling_y) {
                (false, false) => "4:4:4",
                (true, false) => "4:2:2",
                _ => "4:4:0",
            }
        )));
    }
    let format = if p.bit_depth == 8 {
        PixelFormat::Yuv420p8
    } else {
        PixelFormat::Yuv420p10
    };
    let bps = format.bytes_per_sample();
    let plane = |data: Plane, w: u32, h: u32| crate::media::Plane {
        data: match data {
            Plane::U8(v) => v,
            Plane::U16(v) => v.iter().flat_map(|s| s.to_le_bytes()).collect(),
        },
        stride: w as usize * bps,
        width: w as usize,
        height: h as usize,
    };
    let (cw, ch) = (p.chroma_width, p.chroma_height);
    Ok(Image {
        format,
        width: p.width,
        height: p.height,
        pts: p.pts,
        time_base,
        planes: [
            plane(p.y, p.width, p.height),
            plane(p.u, cw, ch),
            plane(p.v, cw, ch),
        ],
    })
}
