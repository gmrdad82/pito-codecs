use crate::hevc::grid::Grid;
use crate::hevc::pel::{Pel, Slots, Store};
use crate::media::Frame as Image;
use std::sync::{Arc, Mutex, OnceLock};

pub const PAD: usize = 80;
pub const SLACK: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColMv {
    pub mv: [[i16; 2]; 2],
    pub ref_poc: [i32; 2],
    pub flags: u8,
}

pub struct Frame {
    pub id: u32,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub log2_ctb: u32,
    pub bit_depth: u32,
    pub bit_depth_c: u32,
    top: Box<[OnceLock<()>]>,
    bottom: Box<[OnceLock<()>]>,
    col: Box<[OnceLock<Box<[ColMv]>>]>,
    store: Slots,
    pub image: OnceLock<Mutex<Option<Image>>>,
    out: Mutex<Option<Output>>,
    pool: Option<PlanePool>,
}

pub type Crop = (usize, usize, usize, usize);

struct Output {
    crop: Crop,
    planes: [Vec<u8>; 3],
}

pub type PlanePool = Arc<Mutex<Vec<Store>>>;

impl Drop for Frame {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else {
            return;
        };
        let store = match (self.store.u8.take(), self.store.u16.take()) {
            (Some(g), _) => Store::U8(g),
            (_, Some(g)) => Store::U16(g),
            _ => return,
        };
        if let Ok(mut pool) = pool.lock()
            && pool.len() < 8
        {
            pool.push(store);
        }
    }
}

fn pad(plane: usize) -> usize {
    if plane == 0 { PAD } else { PAD / 2 }
}

impl Frame {
    pub fn new(
        id: u32,
        poc: i32,
        width: usize,
        height: usize,
        log2_ctb: u32,
        bit_depth: u32,
        bit_depth_c: u32,
    ) -> Self {
        let n = height.div_ceil(1 << log2_ctb);
        Frame {
            id,
            poc,
            width,
            height,
            cwidth: width / 2,
            cheight: height / 2,
            log2_ctb,
            bit_depth,
            bit_depth_c,
            top: (0..n).map(|_| OnceLock::new()).collect(),
            bottom: (0..n).map(|_| OnceLock::new()).collect(),
            col: (0..n).map(|_| OnceLock::new()).collect(),
            store: Slots::default(),
            image: OnceLock::new(),
            out: Mutex::new(None),
            pool: None,
        }
    }

    pub fn pooled(mut self, pool: PlanePool) -> Self {
        self.pool = Some(pool);
        self
    }

    fn plane_size(&self, plane: usize) -> (usize, usize) {
        if plane == 0 {
            (self.width, self.height)
        } else {
            (self.cwidth, self.cheight)
        }
    }

    pub fn stride(&self, plane: usize) -> usize {
        (self.plane_size(plane).0 + 2 * pad(plane) + 16).next_multiple_of(64)
    }

    fn origin(&self, plane: usize) -> usize {
        pad(plane) * self.stride(plane) + pad(plane)
    }

    fn plane_len(&self, plane: usize) -> usize {
        self.stride(plane) * (self.plane_size(plane).1 + 2 * pad(plane)) + SLACK
    }

    fn grids<P: Pel>(&self) -> &[Grid<P>; 3] {
        P::slot(&self.store).get_or_init(|| {
            let lens = [0, 1, 2].map(|p| self.plane_len(p));
            let reused = self
                .pool
                .as_ref()
                .and_then(|p| p.lock().ok().and_then(|mut p| p.pop()))
                .and_then(P::unstore);
            if let Some(g) = reused
                && g.iter().zip(lens).all(|(g, n)| g.len() == n)
            {
                return g;
            }
            lens.map(|n| Grid::from(vec![P::default(); n]))
        })
    }

    pub fn num_rows(&self) -> usize {
        self.top.len()
    }

    pub fn row_lines(&self, r: usize) -> usize {
        let ctb = 1usize << self.log2_ctb;
        ctb.min(self.height - r * ctb)
    }

    pub fn split(&self, plane: usize) -> usize {
        let ctb = 1usize << self.log2_ctb;
        if plane == 0 { ctb - 4 } else { ctb / 2 - 2 }
    }

    pub fn part_lines(&self, r: usize, plane: usize) -> (usize, usize) {
        let lines = if plane == 0 {
            self.row_lines(r)
        } else {
            self.row_lines(r).div_ceil(2)
        };
        let top = lines.min(self.split(plane));
        (top, lines - top)
    }

    pub fn part_range(&self, r: usize, plane: usize, bottom: bool) -> (usize, usize) {
        let ctb = 1usize << self.log2_ctb;
        let y0 = if plane == 0 { r * ctb } else { r * ctb / 2 };
        let (top, rest) = self.part_lines(r, plane);
        if bottom {
            (y0 + top, y0 + top + rest)
        } else {
            (y0, y0 + top)
        }
    }

    pub fn width16(&self) -> usize {
        self.width.div_ceil(16)
    }

    #[allow(clippy::mut_from_ref)]
    pub fn rows_mut<P: Pel>(&self, plane: usize, y0: usize, y1: usize) -> (&mut [P], usize) {
        let s = self.stride(plane);
        let w = self.plane_size(plane).0;
        let at = self.origin(plane) + y0 * s;
        let end = if y1 > y0 {
            at + (y1 - y0 - 1) * s + w
        } else {
            at
        };
        (self.grids::<P>()[plane].slice_mut(at..end), s)
    }

    pub fn pad_rows<P: Pel>(&self, plane: usize, y0: usize, y1: usize) {
        let g = &self.grids::<P>()[plane];
        let (w, h) = self.plane_size(plane);
        let (s, pd, o) = (self.stride(plane), pad(plane), self.origin(plane));
        for y in y0..y1 {
            let row = g.slice_mut(o + y * s - pd..o + y * s + w + pd);
            let (l, r) = (row[pd], row[pd + w - 1]);
            row[..pd].fill(l);
            row[pd + w..].fill(r);
        }
        let span = w + 2 * pd;
        if y0 == 0 && y1 > 0 {
            let src = g.slice(o - pd..o - pd + span);
            for k in 1..=pd {
                g.slice_mut(o - pd - k * s..o - pd - k * s + span)
                    .copy_from_slice(src);
            }
        }
        if y1 == h && y1 > y0 {
            let last = o - pd + (h - 1) * s;
            let src = g.slice(last..last + span);
            for k in 1..=pd {
                g.slice_mut(last + k * s..last + k * s + span)
                    .copy_from_slice(src);
            }
        }
    }

    pub fn publish_top(&self, r: usize, col: Box<[ColMv]>) {
        let _ = self.col[r].set(col);
        let _ = self.top[r].set(());
    }

    pub fn publish_bottom(&self, r: usize) {
        let _ = self.bottom[r].set(());
    }

    pub fn is_complete(&self) -> bool {
        self.bottom.last().is_none_or(|r| r.get().is_some())
    }

    pub fn wait_complete(&self) {
        for (t, b) in self.top.iter().zip(self.bottom.iter()) {
            t.wait();
            b.wait();
        }
    }

    pub fn fill_unpublished_gray(&self) {
        if self.bit_depth > 8 {
            self.fill_gray::<u16>();
        } else {
            self.fill_gray::<u8>();
        }
    }

    fn fill_gray<P: Pel>(&self) {
        let w16 = self.width16();
        for r in 0..self.num_rows() {
            for bottom in [false, true] {
                let flag = if bottom {
                    &self.bottom[r]
                } else {
                    &self.top[r]
                };
                if flag.get().is_some() {
                    continue;
                }
                for plane in 0..3 {
                    let bd = if plane == 0 {
                        self.bit_depth
                    } else {
                        self.bit_depth_c
                    };
                    let (y0, y1) = self.part_range(r, plane, bottom);
                    let (rows, s) = self.rows_mut::<P>(plane, y0, y1);
                    let w = self.plane_size(plane).0;
                    for line in rows.chunks_mut(s) {
                        line[..w].fill(P::from_i32(1 << (bd - 1)));
                    }
                    self.pad_rows::<P>(plane, y0, y1);
                }
                if bottom {
                    self.publish_bottom(r);
                } else {
                    let lines = self.row_lines(r);
                    self.publish_top(r, vec![ColMv::default(); w16 * lines.div_ceil(16)].into());
                }
            }
        }
    }

    fn crop_of(&self, plane: usize, crop: Crop) -> Crop {
        let (cx, cy, cw, ch) = crop;
        if plane == 0 {
            crop
        } else {
            (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2))
        }
    }

    pub fn set_output(&self, crop: Crop, mut planes: [Vec<u8>; 3]) {
        let bytes = if self.bit_depth > 8 { 2 } else { 1 };
        for (plane, v) in planes.iter_mut().enumerate() {
            let (_, _, w, h) = self.crop_of(plane, crop);
            if v.len() != w * h * bytes {
                v.clear();
                v.resize(w * h * bytes, 0);
            }
        }
        if let Ok(mut o) = self.out.lock() {
            *o = Some(Output { crop, planes });
        }
    }

    pub fn take_output(&self) -> Option<[Vec<u8>; 3]> {
        self.out.lock().ok()?.take().map(|o| o.planes)
    }

    pub fn emit<P: Pel>(&self, plane: usize, y0: usize, y1: usize) {
        let Ok(mut o) = self.out.lock() else { return };
        let Some(o) = o.as_mut() else { return };
        let (cx, cy, cw, ch) = self.crop_of(plane, o.crop);
        let g = &self.grids::<P>()[plane];
        let (s, origin) = (self.stride(plane), self.origin(plane));
        let n = cw * P::BYTES;
        for y in y0.max(cy)..y1.min(cy + ch) {
            let at = origin + y * s + cx;
            let Some(dst) = o.planes[plane].get_mut((y - cy) * n..(y - cy + 1) * n) else {
                return;
            };
            P::write_bytes(dst, g.slice(at..at + cw));
        }
    }

    #[inline]
    pub fn col_mv(&self, x: usize, y: usize) -> ColMv {
        let r = y >> self.log2_ctb;
        let col = self.col[r].wait();
        let y16 = (y & ((1 << self.log2_ctb) - 1)) >> 4;
        col[y16 * self.width16() + (x >> 4)]
    }

    fn wait_lines(&self, plane: usize, y0: usize, y1: usize) {
        let sh = if plane == 0 {
            self.log2_ctb
        } else {
            self.log2_ctb - 1
        };
        let t = self.split(plane);
        let part = |y: usize| 2 * (y >> sh) + ((y & ((1 << sh) - 1)) >= t) as usize;
        for p in part(y0)..=part(y1) {
            if p & 1 == 0 {
                self.top[p >> 1].wait();
            } else {
                self.bottom[p >> 1].wait();
            }
        }
    }

    pub fn block<P: Pel>(
        &self,
        plane: usize,
        x0: i32,
        y0: i32,
        w: usize,
        h: usize,
    ) -> (&[P], usize) {
        let (pw, ph) = self.plane_size(plane);
        let pd = pad(plane) as i32;
        let x0 = x0.clamp(-pd, pw as i32 + pd - w as i32);
        let y0 = y0.clamp(-pd, ph as i32 + pd - h as i32);
        let top = y0.clamp(0, ph as i32 - 1) as usize;
        let bottom = (y0 + h as i32 - 1).clamp(0, ph as i32 - 1) as usize;
        self.wait_lines(plane, top, bottom);
        let s = self.stride(plane);
        let at = (self.origin(plane) as isize + y0 as isize * s as isize + x0 as isize) as usize;
        let g = &self.grids::<P>()[plane];
        let end = (at + (h - 1) * s + w + 16).min(g.len());
        (g.slice(at..end), s)
    }

    #[inline(always)]
    pub fn copy_cropped(
        &self,
        crop: (usize, usize, usize, usize),
        bufs: [Vec<u8>; 3],
    ) -> [Vec<u8>; 3] {
        if self.bit_depth > 8 {
            self.copy_cropped_as::<u16>(crop, bufs)
        } else {
            self.copy_cropped_as::<u8>(crop, bufs)
        }
    }

    #[inline(always)]
    fn copy_cropped_as<P: Pel>(
        &self,
        crop: (usize, usize, usize, usize),
        [mut y, mut u, mut v]: [Vec<u8>; 3],
    ) -> [Vec<u8>; 3] {
        self.wait_complete();
        let (cx, cy, cw, ch) = crop;
        let g = self.grids::<P>();
        let put = |out: &mut Vec<u8>, plane: usize, x: usize, r: usize, n: usize| {
            let at = self.origin(plane) + r * self.stride(plane) + x;
            P::extend_bytes(out, g[plane].slice(at..at + n));
        };
        y.clear();
        y.reserve(cw * ch * P::BYTES);
        for r in cy..cy + ch {
            put(&mut y, 0, cx, r, cw);
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        u.clear();
        u.reserve(ccw * cch * P::BYTES);
        v.clear();
        v.reserve(ccw * cch * P::BYTES);
        for r in ccy..ccy + cch {
            put(&mut u, 1, ccx, r, ccw);
            put(&mut v, 2, ccx, r, ccw);
        }
        [y, u, v]
    }

    pub fn from_planes<P: Pel>(
        id: u32,
        poc: i32,
        like: &Frame,
        y: &[P],
        cb: &[P],
        cr: &[P],
    ) -> Frame {
        let f = Frame::new(
            id,
            poc,
            like.width,
            like.height,
            like.log2_ctb,
            like.bit_depth,
            like.bit_depth_c,
        );
        for (plane, src) in [y, cb, cr].into_iter().enumerate() {
            let (w, h) = f.plane_size(plane);
            let (rows, s) = f.rows_mut::<P>(plane, 0, h);
            for (line, src) in rows.chunks_mut(s).zip(src.chunks(w)) {
                line[..w].copy_from_slice(src);
            }
            f.pad_rows::<P>(plane, 0, h);
        }
        let w16 = f.width16();
        for r in 0..f.num_rows() {
            let lines = f.row_lines(r);
            f.publish_top(r, vec![ColMv::default(); w16 * lines.div_ceil(16)].into());
            f.publish_bottom(r);
        }
        f
    }

    pub fn gray(id: u32, poc: i32, like: &Frame) -> Frame {
        if like.bit_depth > 8 {
            Frame::gray_as::<u16>(id, poc, like)
        } else {
            Frame::gray_as::<u8>(id, poc, like)
        }
    }

    fn gray_as<P: Pel>(id: u32, poc: i32, like: &Frame) -> Frame {
        let y = vec![P::from_i32(1 << (like.bit_depth - 1)); like.width * like.height];
        let c = vec![P::from_i32(1 << (like.bit_depth_c - 1)); like.cwidth * like.cheight];
        Frame::from_planes(id, poc, like, &y, &c, &c)
    }
}

pub type FrameRef = Arc<Frame>;

pub type RefPic = crate::hevc::dpb::Ref<FrameRef>;
