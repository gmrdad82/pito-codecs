use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub struct Output3 {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
}

#[derive(Clone)]
pub struct Planes {
    pub y: Vec<u8>,
    pub cb: Vec<u8>,
    pub cr: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Planes {
    pub fn new(width: usize, height: usize) -> Self {
        let (cw, ch) = (width / 2, height / 2);
        Planes {
            y: vec![0; width * height],
            cb: vec![128; cw * ch],
            cr: vec![128; cw * ch],
            width,
            height,
        }
    }
    pub fn gray(width: usize, height: usize) -> Self {
        let mut p = Self::new(width, height);
        p.y.fill(128);
        p
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MbKind {
    #[default]
    None,
    I4x4,
    I8x8,
    I16x16,
    IPcm,
    PSkip,
    BSkip,
    BDirect16x16,
    Inter,
}

impl MbKind {
    #[inline]
    pub fn is_intra(self) -> bool {
        matches!(
            self,
            MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16 | MbKind::IPcm
        )
    }
}

#[derive(Clone, Copy)]
pub struct MbState {
    pub slice_num: u32,
    pub kind: MbKind,
    pub transform_8x8: bool,
    pub cbp: u8,
    pub qp: u8,
    pub qpc: [u8; 2],
    pub intra_chroma_mode: u8,
    pub intra_modes: [u8; 16],
    pub nnz: [u8; 16],
    pub nnz_c: [[u8; 4]; 2],
    pub cbf_dc: u8,
    pub nz_mask: u16,
    pub ref_idx: [[i8; 4]; 2],
    pub ids: [[u32; 4]; 2],
    pub mv: [[[i16; 2]; 16]; 2],
    pub mvd: [[[u8; 2]; 16]; 2],
    pub direct8x8: u8,
}

impl MbState {
    #[inline]
    pub fn kind_is_skip(&self) -> bool {
        matches!(self.kind, MbKind::PSkip | MbKind::BSkip)
    }
}

impl Default for MbState {
    fn default() -> Self {
        MbState {
            slice_num: u32::MAX,
            kind: MbKind::None,
            transform_8x8: false,
            cbp: 0,
            qp: 0,
            qpc: [0; 2],
            intra_chroma_mode: 0,
            intra_modes: [2; 16],
            nnz: [0; 16],
            nnz_c: [[0; 4]; 2],
            cbf_dc: 0,
            nz_mask: 0,
            ref_idx: [[-1; 4]; 2],
            ids: [[u32::MAX; 4]; 2],
            mv: [[[0; 2]; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            direct8x8: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ColMb {
    pub mv: [[[i16; 2]; 16]; 2],
    pub ref_id: [[u32; 4]; 2],
    pub ref_idx: [[i8; 4]; 2],
    pub intra: bool,
}

const COL_INTRA: ColMb = ColMb {
    mv: [[[0; 2]; 16]; 2],
    ref_id: [[u32::MAX; 4]; 2],
    ref_idx: [[-1; 4]; 2],
    intra: true,
};

struct Motion {
    ptr: *mut ColMb,
    len: usize,
    cap: usize,
}

unsafe impl Send for Motion {}

unsafe impl Sync for Motion {}

impl Motion {
    fn from_vec(v: Vec<ColMb>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        Motion {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            cap: v.capacity(),
        }
    }

    fn into_vec(self) -> Vec<ColMb> {
        let v = unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) };
        std::mem::forget(self);
        v
    }

    unsafe fn set(&self, at: usize, v: ColMb) {
        assert!(at < self.len);
        unsafe { self.ptr.add(at).write(v) }
    }

    unsafe fn get(&self, at: usize) -> &ColMb {
        assert!(at < self.len);
        unsafe { &*self.ptr.add(at) }
    }
}

impl Drop for Motion {
    fn drop(&mut self) {
        drop(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) });
    }
}

pub struct Frame {
    pub id: u32,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub mb_w: usize,
    y: SharedPlane,
    cb: SharedPlane,
    cr: SharedPlane,
    rows: Box<[OnceLock<()>]>,
    motion: Motion,
    pool: Option<Arc<FramePool>>,
    output: Mutex<Option<Output3>>,
    has_output: AtomicBool,
    pub job_output: bool,
}

#[derive(Default)]
pub struct FramePool(Mutex<Vec<([SharedPlane; 3], Vec<ColMb>)>>);

impl FramePool {
    fn take(&self, luma: usize, mbs: usize) -> ([SharedPlane; 3], Vec<ColMb>) {
        let reused = self.0.lock().ok().and_then(|mut v| {
            let i = v
                .iter()
                .position(|p| p.0[0].len == luma && p.1.len() == mbs)?;
            Some(v.swap_remove(i))
        });
        reused.unwrap_or_else(|| {
            (
                [
                    SharedPlane::new(luma),
                    SharedPlane::new(luma / 4),
                    SharedPlane::new(luma / 4),
                ],
                vec![COL_INTRA; mbs],
            )
        })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let whole = [&mut self.y, &mut self.cb, &mut self.cr]
            .iter_mut()
            .all(|p| !*p.taken.get_mut() && p.len > 0);
        if whole
            && self.motion.len > 0
            && let Some(pool) = self.pool.take()
            && let Ok(mut v) = pool.0.lock()
            && v.len() < 64
        {
            let empty = || SharedPlane::from_vec(Vec::new());
            let motion = std::mem::replace(&mut self.motion, Motion::from_vec(Vec::new()));
            v.push((
                [
                    std::mem::replace(&mut self.y, empty()),
                    std::mem::replace(&mut self.cb, empty()),
                    std::mem::replace(&mut self.cr, empty()),
                ],
                motion.into_vec(),
            ));
        }
    }
}

struct SharedPlane {
    ptr: *mut u8,
    len: usize,
    cap: usize,
    taken: AtomicBool,
}

pub struct PlaneMut {
    ptr: *mut u8,
    len: usize,
    floor: usize,
}

unsafe impl Send for PlaneMut {}

impl PlaneMut {
    #[inline(always)]
    fn check(&self, a: usize, b: usize) {
        assert!(
            self.floor <= a && a <= b && b <= self.len,
            "plane access {a}..{b} outside {}..{}",
            self.floor,
            self.len
        );
    }

    #[inline(always)]
    fn prefetch(&self, at: usize) {
        #[cfg(target_arch = "x86_64")]
        if at < self.len {
            use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            unsafe { _mm_prefetch::<_MM_HINT_T0>(self.ptr.add(at).cast_const().cast()) };
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = at;
    }

    pub fn set_floor(&mut self, floor: usize) {
        self.floor = floor.min(self.len);
    }

    pub fn near(&mut self, stride: usize, y: usize) -> (&mut [u8], usize) {
        let y0 = y.saturating_sub(4);
        (&mut self[y0 * stride..], y - y0)
    }
}

impl std::ops::Index<std::ops::RangeFrom<usize>> for PlaneMut {
    type Output = [u8];
    #[inline(always)]
    fn index(&self, r: std::ops::RangeFrom<usize>) -> &[u8] {
        self.check(r.start, self.len);
        unsafe { std::slice::from_raw_parts(self.ptr.add(r.start), self.len - r.start) }
    }
}

impl std::ops::IndexMut<std::ops::RangeFrom<usize>> for PlaneMut {
    #[inline(always)]
    fn index_mut(&mut self, r: std::ops::RangeFrom<usize>) -> &mut [u8] {
        self.check(r.start, self.len);
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(r.start), self.len - r.start) }
    }
}

impl std::ops::Index<std::ops::Range<usize>> for PlaneMut {
    type Output = [u8];
    #[inline(always)]
    fn index(&self, r: std::ops::Range<usize>) -> &[u8] {
        self.check(r.start, r.end);
        unsafe { std::slice::from_raw_parts(self.ptr.add(r.start), r.end - r.start) }
    }
}

impl std::ops::IndexMut<std::ops::Range<usize>> for PlaneMut {
    #[inline(always)]
    fn index_mut(&mut self, r: std::ops::Range<usize>) -> &mut [u8] {
        self.check(r.start, r.end);
        unsafe { std::slice::from_raw_parts_mut(self.ptr.add(r.start), r.end - r.start) }
    }
}

pub struct PicPlanes {
    pub y: PlaneMut,
    pub cb: PlaneMut,
    pub cr: PlaneMut,
    pub width: usize,
    pub cwidth: usize,
}

impl PicPlanes {
    pub fn prefetch_mb(&self, mb_x: usize, mb_y: usize) {
        let (x, y) = (mb_x * 16, mb_y * 16);
        if x >= self.width {
            return;
        }
        for r in 0..16 {
            self.y.prefetch((y + r) * self.width + x);
        }
        for r in 0..8 {
            let o = (y / 2 + r) * self.cwidth + x / 2;
            self.cb.prefetch(o);
            self.cr.prefetch(o);
        }
    }

    pub fn published_rows(&mut self, rows: usize) {
        self.y.set_floor(rows * 16 * self.width);
        self.cb.set_floor(rows * 8 * self.cwidth);
        self.cr.set_floor(rows * 8 * self.cwidth);
    }
}

unsafe impl Send for SharedPlane {}

unsafe impl Sync for SharedPlane {}

impl SharedPlane {
    fn new(len: usize) -> Self {
        Self::from_vec(vec![0u8; len])
    }

    fn from_vec(v: Vec<u8>) -> Self {
        let mut v = std::mem::ManuallyDrop::new(v);
        SharedPlane {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            cap: v.capacity(),
            taken: AtomicBool::new(false),
        }
    }

    unsafe fn take(&self) -> Option<Vec<u8>> {
        if self.taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) })
    }

    unsafe fn write(&self, at: usize, src: &[u8]) {
        assert!(at + src.len() <= self.len);
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.add(at), src.len()) };
    }

    #[inline(always)]
    fn prefetch_rows(&self, at: usize, stride: usize, rows: usize, span: usize) {
        #[cfg(target_arch = "x86_64")]
        if rows > 0 && at + (rows - 1) * stride + span <= self.len {
            use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
            let mut p = unsafe { self.ptr.add(at) }.cast_const();
            for _ in 0..rows {
                unsafe { _mm_prefetch::<_MM_HINT_T0>(p.cast()) };
                if (p as usize & 63) + span > 64 {
                    unsafe { _mm_prefetch::<_MM_HINT_T0>(p.add(span - 1).cast()) };
                }
                p = p.wrapping_add(stride);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = (at, stride, rows, span);
    }

    fn writer(&self) -> PlaneMut {
        PlaneMut {
            ptr: self.ptr,
            len: self.len,
            floor: 0,
        }
    }

    unsafe fn slice(&self, at: usize, len: usize) -> &[u8] {
        assert!(at + len <= self.len);
        unsafe { std::slice::from_raw_parts(self.ptr.add(at), len) }
    }
}

impl Drop for SharedPlane {
    fn drop(&mut self) {
        if !*self.taken.get_mut() {
            drop(unsafe { Vec::from_raw_parts(self.ptr, self.len, self.cap) });
        }
    }
}

impl Frame {
    pub fn new(
        id: u32,
        poc: i32,
        mb_w: usize,
        mb_h: usize,
        pool: &Arc<FramePool>,
        bufs: Option<[Vec<u8>; 3]>,
    ) -> Self {
        let ([y, cb, cr], motion) = match bufs {
            Some(b) => (b.map(SharedPlane::from_vec), Vec::new()),
            None => pool.take(mb_w * 16 * mb_h * 16, mb_w * mb_h),
        };
        Frame {
            id,
            poc,
            width: mb_w * 16,
            height: mb_h * 16,
            cwidth: mb_w * 8,
            cheight: mb_h * 8,
            mb_w,
            y,
            cb,
            cr,
            rows: (0..mb_h).map(|_| OnceLock::new()).collect(),
            motion: Motion::from_vec(motion),
            pool: Some(pool.clone()),
            output: Mutex::new(None),
            has_output: AtomicBool::new(false),
            job_output: true,
        }
    }

    pub unsafe fn take_planes(&self) -> Option<[Vec<u8>; 3]> {
        let y = unsafe { self.y.take() }?;
        let cb = unsafe { self.cb.take() }?;
        let cr = unsafe { self.cr.take() }?;
        Some([y, cb, cr])
    }

    pub fn mb_h(&self) -> usize {
        self.rows.len()
    }

    pub fn set_output(&self, o: Output3) {
        if let Ok(mut slot) = self.output.lock() {
            *slot = Some(o);
            self.has_output.store(true, Ordering::Release);
        }
    }

    pub fn output_ready(&self) -> bool {
        self.has_output.load(Ordering::Acquire)
    }

    pub fn take_output(&self) -> Option<Output3> {
        self.output.lock().ok().and_then(|mut s| s.take())
    }

    pub unsafe fn writer(&self) -> PicPlanes {
        PicPlanes {
            y: self.y.writer(),
            cb: self.cb.writer(),
            cr: self.cr.writer(),
            width: self.width,
            cwidth: self.cwidth,
        }
    }

    pub fn publish(&self, r: usize, mbs: Option<&[MbState]>) {
        if self.rows[r].get().is_some() {
            return;
        }
        if self.motion.len > 0 {
            for x in 0..self.mb_w {
                let v = match mbs.and_then(|m| m.get(x)) {
                    Some(st) => col_mb(st),
                    None => COL_INTRA,
                };
                unsafe { self.motion.set(r * self.mb_w + x, v) };
            }
        }
        let _ = self.rows[r].set(());
    }

    pub fn fill(&self, r: usize, planes: &Planes) {
        if self.rows[r].get().is_some() {
            return;
        }
        let (w, cw) = (self.width, self.cwidth);
        let (yo, co) = (r * 16 * w, r * 8 * cw);
        unsafe {
            self.y.write(yo, &planes.y[yo..yo + 16 * w]);
            self.cb.write(co, &planes.cb[co..co + 8 * cw]);
            self.cr.write(co, &planes.cr[co..co + 8 * cw]);
        }
        self.publish(r, None);
    }

    #[inline]
    pub fn luma(&self, y0: usize, y1: usize) -> &[u8] {
        for r in y0 >> 4..=(y1 - 1) >> 4 {
            self.rows[r].wait();
        }
        unsafe { self.y.slice(y0 * self.width, (y1 - y0) * self.width) }
    }

    #[inline]
    pub fn chroma(&self, c: usize, y0: usize, y1: usize) -> &[u8] {
        for r in y0 >> 3..=(y1 - 1) >> 3 {
            self.rows[r].wait();
        }
        let p = if c == 0 { &self.cb } else { &self.cr };
        unsafe { p.slice(y0 * self.cwidth, (y1 - y0) * self.cwidth) }
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows[r].get().is_some()
    }

    #[inline]
    pub fn col_mb(&self, r: usize, mb: usize) -> Option<&ColMb> {
        if self.motion.len == 0 || mb >= self.mb_w {
            return None;
        }
        self.rows.get(r)?.wait();
        Some(unsafe { self.motion.get(r * self.mb_w + mb) })
    }

    pub fn prefetch_motion(&self, r: usize, mb: usize) {
        if self.motion.len == 0
            || mb >= self.mb_w
            || self.rows.get(r).and_then(|row| row.get()).is_none()
        {
            return;
        }
        let m = unsafe { self.motion.get(r * self.mb_w + mb) };
        let p = (m as *const ColMb).cast::<u8>();
        for k in 0..3 {
            prefetch_ptr(Some(unsafe { &*p.add(k * 64) }));
        }
    }

    pub fn wait_complete(&self) {
        for r in self.rows.iter() {
            r.wait();
        }
    }

    pub fn from_planes(id: u32, poc: i32, planes: &Planes, pool: &Arc<FramePool>) -> Self {
        let (mb_w, mb_h) = (planes.width / 16, planes.height / 16);
        let mut f = Frame::new(id, poc, mb_w, mb_h, pool, None);
        f.job_output = false;
        for r in 0..mb_h {
            f.fill(r, planes);
        }
        f
    }

    pub fn prefetch(&self, x0: i32, y0: i32) {
        let (width, height) = (self.width as i32, self.height as i32);
        if width < 21 || height < 21 {
            return;
        }
        let x = (x0 - 2).clamp(0, width - 21) as usize;
        let y = (y0 - 2).clamp(0, height - 21) as usize;
        self.y.prefetch_rows(y * self.width + x, self.width, 21, 21);
        let (cw, ch) = (self.cwidth as i32, self.cheight as i32);
        let cx = (x0 >> 1).clamp(0, cw - 9) as usize;
        let cy = (y0 >> 1).clamp(0, ch - 9) as usize;
        let o = cy * self.cwidth + cx;
        self.cb.prefetch_rows(o, self.cwidth, 9, 9);
        self.cr.prefetch_rows(o, self.cwidth, 9, 9);
    }

    #[inline]
    pub fn luma_window(&self, x0: i32, y0: i32, w: usize, h: usize, out: &mut [u8], os: usize) {
        let (width, height) = (self.width, self.height);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, height as i32 - 1) as usize;
            let line = self.luma(yy, yy + 1);
            copy_line::<24>(&line[..width], x0, &mut out[r * os..], w);
        }
    }

    #[inline]
    pub fn chroma_window(
        &self,
        c: usize,
        x0: i32,
        y0: i32,
        w: usize,
        h: usize,
        out: &mut [u8],
        os: usize,
    ) {
        let (cw, ch) = (self.cwidth, self.cheight);
        for r in 0..h {
            let yy = (y0 + r as i32).clamp(0, ch as i32 - 1) as usize;
            let line = self.chroma(c, yy, yy + 1);
            copy_line::<16>(&line[..cw], x0, &mut out[r * os..], w);
        }
    }

    pub fn copy_cropped(
        &self,
        crop: (usize, usize, usize, usize),
        bufs: [Vec<u8>; 3],
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cx, cy, cw, ch) = crop;
        let [mut y, mut u, mut v] = bufs.map(|mut b| {
            b.clear();
            b
        });
        y.reserve(cw * ch);
        let src = self.luma(cy, cy + ch);
        if cx == 0 && cw == self.width {
            y.extend_from_slice(src);
        } else {
            for line in src.chunks_exact(self.width) {
                y.extend_from_slice(&line[cx..cx + cw]);
            }
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        u.reserve(ccw * cch);
        v.reserve(ccw * cch);
        for (out, c) in [(&mut u, 0), (&mut v, 1)] {
            let src = self.chroma(c, ccy, ccy + cch);
            if ccx == 0 && ccw == self.cwidth {
                out.extend_from_slice(src);
            } else {
                for line in src.chunks_exact(self.cwidth) {
                    out.extend_from_slice(&line[ccx..ccx + ccw]);
                }
            }
        }
        (y, u, v)
    }
}

#[inline(always)]
pub fn mv_bits(m: [i16; 2]) -> u32 {
    m[0] as u16 as u32 | (m[1] as u16 as u32) << 16
}

#[inline(always)]
pub fn uniform_mv(mv: &[[i16; 2]; 16]) -> bool {
    let first = mv_bits(mv[0]);
    mv.iter().fold(true, |ok, &m| ok & (mv_bits(m) == first))
}

#[inline(always)]
fn col_mb(st: &MbState) -> ColMb {
    if st.slice_num == u32::MAX || st.kind.is_intra() {
        return COL_INTRA;
    }
    ColMb {
        mv: st.mv,
        ref_id: st.ids,
        ref_idx: st.ref_idx,
        intra: false,
    }
}

#[inline(always)]
fn prefetch_ptr<T>(p: Option<&T>) {
    #[cfg(target_arch = "x86_64")]
    if let Some(p) = p {
        use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
        unsafe { _mm_prefetch::<_MM_HINT_T0>((p as *const T).cast()) };
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = p;
}

#[inline(always)]
fn copy_line<const N: usize>(line: &[u8], x0: i32, out: &mut [u8], w: usize) {
    if w <= N
        && x0 >= 0
        && let Some(src) = line.get(x0 as usize..).and_then(|l| l.first_chunk::<N>())
        && let Some(dst) = out.first_chunk_mut::<N>()
    {
        *dst = *src;
    } else {
        copy_clamped(line, x0, &mut out[..w]);
    }
}

#[inline(always)]
fn copy_clamped(line: &[u8], x0: i32, out: &mut [u8]) {
    let w = out.len();
    if x0 >= 0 && x0 as usize + w <= line.len() {
        out.copy_from_slice(&line[x0 as usize..x0 as usize + w]);
    } else {
        let max = line.len() as i32 - 1;
        for (c, o) in out.iter_mut().enumerate() {
            *o = line[(x0 + c as i32).clamp(0, max) as usize];
        }
    }
}

pub type FrameRef = Arc<Frame>;

#[derive(Clone)]
pub struct RefPic {
    pub frame: FrameRef,
    pub long_term: bool,
}

impl RefPic {
    pub fn poc(&self) -> i32 {
        self.frame.poc
    }
    pub fn id(&self) -> u32 {
        self.frame.id
    }
}
