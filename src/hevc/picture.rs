use crate::hevc::pel::{Pel, Samples};
use crate::media::Frame as Image;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColMv {
    pub mv: [[i16; 2]; 2],
    pub ref_poc: [i32; 2],
    pub flags: u8,
}

pub struct Part {
    pub y: Samples,
    pub cb: Samples,
    pub cr: Samples,
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
    top: Box<[OnceLock<Part>]>,
    bottom: Box<[OnceLock<Part>]>,
    col: Box<[OnceLock<Box<[ColMv]>>]>,
    pub image: OnceLock<Mutex<Option<Image>>>,
    pool: Option<RowPool>,
}

pub type RowPool = Arc<Mutex<Vec<Samples>>>;

impl Drop for Frame {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else { return };
        let Ok(mut pool) = pool.lock() else { return };
        for part in self.top.iter_mut().chain(self.bottom.iter_mut()) {
            if let Some(p) = part.take()
                && pool.len() + 3 <= 2048
            {
                pool.extend([p.y, p.cb, p.cr]);
            }
        }
    }
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
            image: OnceLock::new(),
            pool: None,
        }
    }

    pub fn pooled(mut self, pool: RowPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn row_buffer<P: Pel>(&self) -> Vec<P> {
        let taken = self
            .pool
            .as_ref()
            .and_then(|p| p.lock().ok().and_then(|mut p| p.pop()));
        taken.and_then(P::take).unwrap_or_default()
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

    pub fn width16(&self) -> usize {
        self.width.div_ceil(16)
    }

    pub fn publish_top(&self, r: usize, part: Part, col: Box<[ColMv]>) {
        let _ = self.col[r].set(col);
        let _ = self.top[r].set(part);
    }

    pub fn publish_bottom(&self, r: usize, part: Part) {
        let _ = self.bottom[r].set(part);
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
        let gray = Frame::gray(0, self.poc, self);
        for r in 0..self.num_rows() {
            if self.top[r].get().is_none()
                && let (Some(t), Some(c)) = (gray.top[r].get(), gray.col[r].get())
            {
                self.publish_top(
                    r,
                    Part {
                        y: t.y.clone(),
                        cb: t.cb.clone(),
                        cr: t.cr.clone(),
                    },
                    c.clone(),
                );
            }
            if self.bottom[r].get().is_none()
                && let Some(b) = gray.bottom[r].get()
            {
                self.publish_bottom(
                    r,
                    Part {
                        y: b.y.clone(),
                        cb: b.cb.clone(),
                        cr: b.cr.clone(),
                    },
                );
            }
        }
    }

    #[inline]
    pub fn col_mv(&self, x: usize, y: usize) -> ColMv {
        let r = y >> self.log2_ctb;
        let col = self.col[r].wait();
        let y16 = (y & ((1 << self.log2_ctb) - 1)) >> 4;
        col[y16 * self.width16() + (x >> 4)]
    }

    #[inline]
    fn part_data<P: Pel>(&self, plane: usize, r: usize, bottom: bool) -> &[P] {
        let part = if bottom {
            self.bottom[r].wait()
        } else {
            self.top[r].wait()
        };
        P::view(match plane {
            0 => &part.y,
            1 => &part.cb,
            _ => &part.cr,
        })
    }

    #[inline]
    fn locate(&self, plane: usize, y: usize) -> (usize, bool, usize) {
        let sh = if plane == 0 {
            self.log2_ctb
        } else {
            self.log2_ctb - 1
        };
        let l = y & ((1 << sh) - 1);
        let t = self.split(plane);
        if l < t {
            (y >> sh, false, l)
        } else {
            (y >> sh, true, l - t)
        }
    }

    fn line<P: Pel>(&self, plane: usize, y: usize) -> &[P] {
        let w = if plane == 0 { self.width } else { self.cwidth };
        let (r, bottom, l) = self.locate(plane, y);
        &self.part_data::<P>(plane, r, bottom)[l * w..(l + 1) * w]
    }

    fn plane_size(&self, plane: usize) -> (i32, i32) {
        if plane == 0 {
            (self.width as i32, self.height as i32)
        } else {
            (self.cwidth as i32, self.cheight as i32)
        }
    }

    pub fn lines<'a, P: Pel>(
        &'a self,
        plane: usize,
        x0: i32,
        y0: i32,
        w: usize,
        h: usize,
        out: &mut [&'a [P]],
    ) -> bool {
        let (pw, ph) = self.plane_size(plane);
        if x0 < 0 || x0 + w as i32 > pw {
            return false;
        }
        let x = x0 as usize;
        let pw = pw as usize;
        let mut cached: ((usize, bool), &[P]) = ((usize::MAX, false), &[]);
        for (r, o) in out.iter_mut().enumerate().take(h) {
            let y = (y0 + r as i32).clamp(0, ph - 1) as usize;
            let (row, bottom, l) = self.locate(plane, y);
            if (row, bottom) != cached.0 {
                cached = ((row, bottom), self.part_data::<P>(plane, row, bottom));
            }
            let off = l * pw + x;
            *o = &cached.1[off..off + w];
        }
        true
    }

    pub fn window<P: Pel>(
        &self,
        plane: usize,
        x0: i32,
        y0: i32,
        w: usize,
        h: usize,
        out: &mut [P],
        os: usize,
    ) {
        let (_, ph) = self.plane_size(plane);
        for r in 0..h {
            let y = (y0 + r as i32).clamp(0, ph - 1) as usize;
            copy_line(self.line(plane, y), x0, &mut out[r * os..r * os + w]);
        }
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
        let (cx, cy, cw, ch) = crop;
        let put = |out: &mut Vec<u8>, src: &[P]| P::extend_bytes(out, src);
        y.clear();
        y.reserve(cw * ch * P::BYTES);
        for r in cy..cy + ch {
            put(&mut y, &self.line::<P>(0, r)[cx..cx + cw]);
        }
        let (ccx, ccy, ccw, cch) = (cx / 2, cy / 2, cw.div_ceil(2), ch.div_ceil(2));
        u.clear();
        u.reserve(ccw * cch * P::BYTES);
        v.clear();
        v.reserve(ccw * cch * P::BYTES);
        for r in ccy..ccy + cch {
            put(&mut u, &self.line::<P>(1, r)[ccx..ccx + ccw]);
            put(&mut v, &self.line::<P>(2, r)[ccx..ccx + ccw]);
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
        let ctb = 1usize << f.log2_ctb;
        let w16 = f.width16();
        for r in 0..f.num_rows() {
            let lines = f.row_lines(r);
            let (lt, lb) = f.part_lines(r, 0);
            let (ct, cb_lines) = f.part_lines(r, 1);
            let (w, cw) = (f.width, f.cwidth);
            let (y0, c0) = (r * ctb * w, r * ctb / 2 * cw);
            let part = |a: usize, n: usize, ca: usize, cn: usize| Part {
                y: P::wrap(y[a..a + n * w].to_vec()),
                cb: P::wrap(cb[ca..ca + cn * cw].to_vec()),
                cr: P::wrap(cr[ca..ca + cn * cw].to_vec()),
            };
            f.publish_top(
                r,
                part(y0, lt, c0, ct),
                vec![ColMv::default(); w16 * lines.div_ceil(16)].into(),
            );
            f.publish_bottom(r, part(y0 + lt * w, lb, c0 + ct * cw, cb_lines));
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

#[inline(always)]
fn copy_line<P: Pel>(line: &[P], x0: i32, out: &mut [P]) {
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

pub type RefPic = crate::hevc::dpb::Ref<FrameRef>;
