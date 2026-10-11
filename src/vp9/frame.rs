use crate::vp9::tables::{INTRA_FRAME, NONE};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mv {
    pub row: i16,
    pub col: i16,
}

impl Mv {
    pub const ZERO: Mv = Mv { row: 0, col: 0 };
    pub fn new(row: i32, col: i32) -> Mv {
        Mv {
            row: row as i16,
            col: col as i16,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MiInfo {
    pub sb_size: u8,
    pub skip: bool,
    pub tx_size: u8,
    pub y_mode: u8,
    pub sub_modes: [u8; 4],
    pub seg_id: u8,
    pub ref_frame: [i8; 2],
    pub interp_filter: u8,
    pub mv: [[Mv; 4]; 2],
}

impl Default for MiInfo {
    fn default() -> Self {
        MiInfo {
            sb_size: 0,
            skip: false,
            tx_size: 0,
            y_mode: 0,
            sub_modes: [0; 4],
            seg_id: 0,
            ref_frame: [INTRA_FRAME, NONE],
            interp_filter: 0,
            mv: [[Mv::ZERO; 4]; 2],
        }
    }
}

pub struct MiGrid {
    pub cols: usize,
    pub rows: usize,
    mi: Vec<MiInfo>,
    ptr: *mut MiInfo,
}

unsafe impl Send for MiGrid {}
unsafe impl Sync for MiGrid {}

impl MiGrid {
    pub fn new(rows: usize, cols: usize, mut mi: Vec<MiInfo>) -> MiGrid {
        mi.resize(rows * cols, MiInfo::default());
        let ptr = mi.as_mut_ptr();
        MiGrid {
            cols,
            rows,
            mi,
            ptr,
        }
    }

    #[inline]
    pub fn at(&self, r: usize, c: usize) -> &MiInfo {
        let i = r * self.cols + c;
        assert!(i < self.mi.len());
        unsafe { &*self.ptr.add(i) }
    }

    #[inline]
    pub fn fill(&self, r0: usize, r1: usize, c0: usize, c1: usize, info: MiInfo) {
        assert!(r1 <= self.rows && c0 <= c1 && c1 <= self.cols);
        for r in r0..r1 {
            for c in c0..c1 {
                unsafe { self.ptr.add(r * self.cols + c).write(info) };
            }
        }
    }

    pub fn seg_ids(&self) -> Vec<u8> {
        (0..self.mi.len())
            .map(|i| unsafe { (*self.ptr.add(i)).seg_id })
            .collect()
    }

    pub fn into_vec(self) -> Vec<MiInfo> {
        self.mi
    }

    pub fn as_slice(&self) -> &[MiInfo] {
        &self.mi
    }

    pub fn as_mut_slice(&mut self) -> &mut [MiInfo] {
        &mut self.mi
    }
}

pub struct RowSync {
    done: Vec<AtomicUsize>,
    need: usize,
    failed: std::sync::atomic::AtomicBool,
    lock: Mutex<()>,
    ready: Condvar,
}

impl RowSync {
    pub fn new(rows: usize, need: usize) -> RowSync {
        RowSync {
            done: (0..rows).map(|_| AtomicUsize::new(0)).collect(),
            need,
            failed: Default::default(),
            lock: Mutex::new(()),
            ready: Condvar::new(),
        }
    }

    pub fn mark(&self, r: usize) {
        if let Some(d) = self.done.get(r)
            && d.fetch_add(1, Ordering::AcqRel) + 1 == self.need
        {
            let _g = self
                .lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.ready.notify_all();
        }
    }

    pub fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        let _g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ready.notify_all();
    }

    pub fn wait(&self, r: usize) -> bool {
        let ok = |s: &Self| s.done[r].load(Ordering::Acquire) >= s.need;
        if ok(self) {
            return true;
        }
        let mut g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if ok(self) {
                return true;
            }
            if self.failed.load(Ordering::Acquire) {
                return false;
            }
            g = self
                .ready
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

pub const BORDER: usize = 80;

#[derive(Clone, Copy, Debug)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub ss_x: bool,
    pub ss_y: bool,
    pub bit_depth: u8,
    pub color_space: u8,
    pub color_range: bool,
    pub render_width: u32,
    pub render_height: u32,
    pub key: bool,
    pub intra_only: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub stride: usize,
    pub origin: usize,
    pub border_x: usize,
    pub border_y: usize,
    pub width: usize,
    pub height: usize,
    pub aligned_h: usize,
    pub band_h: usize,
    pub len: usize,
}

pub trait Pixel: Copy + Default + Send + Sync + PartialEq + std::fmt::Debug + 'static {
    const WIDE: bool;
    fn get(self) -> i32;
    fn put(v: i32) -> Self;
    fn wide(s: &[Self]) -> Option<&[u16]>;
    fn wide_mut(s: &mut [Self]) -> Option<&mut [u16]>;
    fn narrow(s: &[Self]) -> Option<&[u8]>;
    fn narrow_mut(s: &mut [Self]) -> Option<&mut [u8]>;
}

impl Pixel for u8 {
    const WIDE: bool = false;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn put(v: i32) -> Self {
        v as u8
    }
    fn wide(_: &[u8]) -> Option<&[u16]> {
        None
    }
    fn wide_mut(_: &mut [u8]) -> Option<&mut [u16]> {
        None
    }
    fn narrow(s: &[u8]) -> Option<&[u8]> {
        Some(s)
    }
    fn narrow_mut(s: &mut [u8]) -> Option<&mut [u8]> {
        Some(s)
    }
}

impl Pixel for u16 {
    const WIDE: bool = true;
    #[inline(always)]
    fn get(self) -> i32 {
        self as i32
    }
    #[inline(always)]
    fn put(v: i32) -> Self {
        v as u16
    }
    fn wide(s: &[u16]) -> Option<&[u16]> {
        Some(s)
    }
    fn wide_mut(s: &mut [u16]) -> Option<&mut [u16]> {
        Some(s)
    }
    fn narrow(_: &[u16]) -> Option<&[u8]> {
        None
    }
    fn narrow_mut(_: &mut [u16]) -> Option<&mut [u8]> {
        None
    }
}

pub struct PlaneMut<'a, P> {
    ptr: *mut P,
    pub stride: usize,
    frame: std::marker::PhantomData<&'a Frame>,
}

impl<P> Clone for PlaneMut<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for PlaneMut<'_, P> {}

impl<'a, P> PlaneMut<'a, P> {
    #[inline(always)]
    pub fn at(&self, x: usize, y: usize) -> *mut P {
        self.ptr.wrapping_add(y * self.stride + x)
    }

    #[inline(always)]
    pub fn row(&self, x: usize, y: usize, n: usize) -> &'a mut [P] {
        unsafe { std::slice::from_raw_parts_mut(self.at(x, y), n) }
    }
}

pub struct Frame {
    pub info: FrameInfo,
    pub geo: [Geometry; 3],
    data: [Vec<u16>; 3],
    ptrs: [*mut u8; 3],
    wide: bool,
    sb_rows: usize,
    progress: AtomicUsize,
    lock: Mutex<()>,
    ready: Condvar,
    pools: Arc<Pools>,
}

unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn new(info: FrameInfo, pools: Arc<Pools>) -> Frame {
        let geo = plane_geometry(info.width, info.height, info.ss_x, info.ss_y);
        let wide = info.bit_depth > 8;
        let mut data = geo.map(|g| {
            pools
                .planes
                .take_any(if wide { g.len } else { g.len.div_ceil(2) }, 0)
        });
        let ptrs = std::array::from_fn(|p| data[p].as_mut_ptr().cast());
        Frame {
            info,
            geo,
            data,
            ptrs,
            wide,
            sb_rows: (info.height as usize).div_ceil(64),
            progress: AtomicUsize::new(0),
            lock: Mutex::new(()),
            ready: Condvar::new(),
            pools,
        }
    }

    pub fn sb_rows(&self) -> usize {
        self.sb_rows
    }

    pub fn wide(&self) -> bool {
        self.wide
    }

    #[inline(always)]
    fn base<P: Pixel>(&self, p: usize) -> *mut P {
        assert!(P::WIDE == self.wide);
        self.ptrs[p].cast()
    }

    pub fn plane_mut<P: Pixel>(&self, p: usize) -> PlaneMut<'_, P> {
        PlaneMut {
            ptr: self.base::<P>(p).wrapping_add(self.geo[p].origin),
            stride: self.geo[p].stride,
            frame: std::marker::PhantomData,
        }
    }

    #[allow(clippy::mut_from_ref)]
    pub unsafe fn planes_mut<P: Pixel>(&self) -> [&mut [P]; 3] {
        std::array::from_fn(|p| {
            let g = &self.geo[p];
            unsafe {
                std::slice::from_raw_parts_mut(self.base::<P>(p).add(g.origin), g.len - g.origin)
            }
        })
    }

    pub fn publish(&self, rows: usize) {
        let _g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.progress.store(rows, Ordering::Release);
        self.ready.notify_all();
    }

    pub fn is_complete(&self) -> bool {
        self.progress.load(Ordering::Acquire) >= self.sb_rows
    }

    #[inline]
    pub fn wait(&self, rows: usize) {
        let rows = rows.min(self.sb_rows);
        if self.progress.load(Ordering::Acquire) >= rows {
            return;
        }
        let mut g = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while self.progress.load(Ordering::Acquire) < rows {
            g = self
                .ready
                .wait(g)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    #[inline]
    pub fn wait_plane_row(&self, p: usize, y: isize) {
        let g = &self.geo[p];
        let y = y.clamp(0, g.aligned_h as isize - 1) as usize;
        self.wait(y / g.band_h + 1);
    }

    #[inline]
    pub fn plane<P: Pixel>(&self, p: usize) -> &[P] {
        unsafe { std::slice::from_raw_parts(self.base::<P>(p), self.geo[p].len) }
    }

    pub fn finish_rows(&self, r: usize) {
        if self.wide {
            self.extend_rows::<u16>(r);
        } else {
            self.extend_rows::<u8>(r);
        }
    }

    fn extend_rows<P: Pixel>(&self, r: usize) {
        for p in 0..3 {
            let g = self.geo[p];
            let pm = self.plane_mut::<P>(p);
            let y0 = r * g.band_h;
            let y1 = ((r + 1) * g.band_h).min(g.height);
            let bx = g.border_x;
            let right = g.stride - bx - g.width;
            for y in y0..y1 {
                let row = unsafe {
                    std::slice::from_raw_parts_mut(pm.at(0, y).wrapping_sub(bx), g.stride)
                };
                let l = row[bx];
                row[..bx].fill(l);
                let rv = row[bx + g.width - 1];
                row[bx + g.width..bx + g.width + right].fill(rv);
            }
            let copy_row = |from: usize, to: isize| unsafe {
                let src = pm.at(0, from).wrapping_sub(bx);
                let dst = pm
                    .ptr
                    .wrapping_offset(to * g.stride as isize)
                    .wrapping_sub(bx);
                std::ptr::copy_nonoverlapping(src, dst, g.stride);
            };
            if r == 0 {
                for y in 1..=g.border_y as isize {
                    copy_row(0, -y);
                }
            }
            if r + 1 == self.sb_rows {
                for y in g.height..g.aligned_h + g.border_y {
                    copy_row(g.height - 1, y as isize);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn from_planes<P: Pixel>(info: FrameInfo, planes: [&[P]; 3]) -> Frame {
        let f = Frame::new(info, Default::default());
        for p in 0..3 {
            let g = f.geo[p];
            let pm = f.plane_mut::<P>(p);
            for y in 0..g.height {
                pm.row(0, y, g.width)
                    .copy_from_slice(&planes[p][y * g.width..y * g.width + g.width]);
            }
        }
        for r in 0..f.sb_rows {
            f.finish_rows(r);
        }
        f.publish(f.sb_rows);
        f
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        for d in std::mem::take(&mut self.data) {
            self.pools.planes.put(d);
        }
    }
}

pub fn plane_geometry(width: u32, height: u32, ss_x: bool, ss_y: bool) -> [Geometry; 3] {
    let sb_cols = (width as usize).div_ceil(64);
    let sb_rows = (height as usize).div_ceil(64);
    let (aw, ah) = (sb_cols * 64, sb_rows * 64);
    let (sx, sy) = (ss_x as usize, ss_y as usize);
    let g = |w: usize, h: usize, aw: usize, ah: usize, bx: usize, by: usize, band_h: usize| {
        let stride = (aw + 2 * bx).next_multiple_of(32);
        Geometry {
            stride,
            origin: by * stride + bx,
            border_x: bx,
            border_y: by,
            width: w,
            height: h,
            aligned_h: ah,
            band_h,
            len: (ah + 2 * by) * stride,
        }
    };
    let cw = (width as usize + sx) >> sx;
    let ch = (height as usize + sy) >> sy;
    let luma = g(width as usize, height as usize, aw, ah, BORDER, BORDER, 64);
    let chroma = g(
        cw,
        ch,
        aw >> sx,
        ah >> sy,
        BORDER >> sx,
        BORDER >> sy,
        64 >> sy,
    );
    [luma, chroma, chroma]
}

pub struct Pool<T> {
    bufs: std::sync::Mutex<Vec<Vec<T>>>,
    max: usize,
}

impl<T> Default for Pool<T> {
    fn default() -> Self {
        Pool::with_max(32)
    }
}

impl<T> Pool<T> {
    pub fn with_max(max: usize) -> Self {
        Pool {
            bufs: Default::default(),
            max,
        }
    }
}

impl<T: Clone> Pool<T> {
    pub fn take_any(&self, len: usize, fill: T) -> Vec<T> {
        let found = {
            let mut b = self
                .bufs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            b.iter()
                .position(|v| v.capacity() >= len)
                .map(|i| b.swap_remove(i))
        };
        match found {
            Some(mut v) => {
                if v.len() < len {
                    v.resize(len, fill);
                }
                v.truncate(len);
                v
            }
            None => vec![fill; len],
        }
    }

    pub fn put(&self, v: Vec<T>) {
        if v.capacity() == 0 {
            return;
        }
        let mut b = self
            .bufs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if b.len() < self.max {
            b.push(v);
        }
    }
}

pub struct Pools {
    pub planes: Pool<u16>,
    pub mi: Pool<MiInfo>,
}

impl Default for Pools {
    fn default() -> Self {
        Pools {
            planes: Pool::with_max(64),
            mi: Pool::default(),
        }
    }
}
