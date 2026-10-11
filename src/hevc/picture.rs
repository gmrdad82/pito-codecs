use crate::hevc::pel::{Pel, Samples};
use crate::media::Frame as Image;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColMv {
    pub mv: [[i16; 2]; 2],
    pub ref_poc: [i32; 2],
    pub flags: u8,
}

pub struct FrameRow {
    pub y: Samples,
    pub cb: Samples,
    pub cr: Samples,
    pub col: Box<[ColMv]>,
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
    rows: Box<[OnceLock<FrameRow>]>,
    pub image: OnceLock<Mutex<Option<Image>>>,
    pool: Option<RowPool>,
}

pub type RowPool = Arc<Mutex<Vec<Samples>>>;

impl Drop for Frame {
    fn drop(&mut self) {
        let Some(pool) = self.pool.take() else { return };
        let Ok(mut pool) = pool.lock() else { return };
        for row in self.rows.iter_mut() {
            if let Some(r) = row.take()
                && pool.len() + 3 <= 1024
            {
                pool.extend([r.y, r.cb, r.cr]);
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
            rows: (0..n).map(|_| OnceLock::new()).collect(),
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
        self.rows.len()
    }

    pub fn row_lines(&self, r: usize) -> usize {
        let ctb = 1usize << self.log2_ctb;
        ctb.min(self.height - r * ctb)
    }

    pub fn width16(&self) -> usize {
        self.width.div_ceil(16)
    }

    pub fn publish(&self, r: usize, row: FrameRow) {
        let _ = self.rows[r].set(row);
    }

    pub fn is_published(&self, r: usize) -> bool {
        self.rows[r].get().is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.rows.last().is_none_or(|r| r.get().is_some())
    }

    #[inline]
    pub fn row(&self, r: usize) -> &FrameRow {
        self.rows[r].wait()
    }

    pub fn wait_complete(&self) {
        for r in 0..self.rows.len() {
            self.row(r);
        }
    }

    #[inline]
    pub fn col_mv(&self, x: usize, y: usize) -> ColMv {
        let r = y >> self.log2_ctb;
        let row = self.row(r);
        let y16 = (y & ((1 << self.log2_ctb) - 1)) >> 4;
        row.col[y16 * self.width16() + (x >> 4)]
    }

    fn line<P: Pel>(&self, plane: usize, y: usize) -> &[P] {
        let (sh, w) = if plane == 0 {
            (self.log2_ctb, self.width)
        } else {
            (self.log2_ctb - 1, self.cwidth)
        };
        let row = self.row(y >> sh);
        let data = P::view(match plane {
            0 => &row.y,
            1 => &row.cb,
            _ => &row.cr,
        });
        let off = (y & ((1 << sh) - 1)) * w;
        &data[off..off + w]
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
        let (sh, pw) = if plane == 0 {
            (self.log2_ctb, self.width)
        } else {
            (self.log2_ctb - 1, self.cwidth)
        };
        let mut cached: (usize, &[P]) = (usize::MAX, &[]);
        for (r, o) in out.iter_mut().enumerate().take(h) {
            let y = (y0 + r as i32).clamp(0, ph - 1) as usize;
            let row = y >> sh;
            if row != cached.0 {
                let data = self.row(row);
                cached = (
                    row,
                    P::view(match plane {
                        0 => &data.y,
                        1 => &data.cb,
                        _ => &data.cr,
                    }),
                );
            }
            let off = (y & ((1 << sh) - 1)) * pw + x;
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
            let (y0, y1) = (r * ctb * f.width, (r * ctb + lines) * f.width);
            let clines = lines.div_ceil(2);
            let (c0, c1) = (r * ctb / 2 * f.cwidth, (r * ctb / 2 + clines) * f.cwidth);
            f.publish(
                r,
                FrameRow {
                    y: P::wrap(y[y0..y1].to_vec()),
                    cb: P::wrap(cb[c0..c1].to_vec()),
                    cr: P::wrap(cr[c0..c1].to_vec()),
                    col: vec![ColMv::default(); w16 * lines.div_ceil(16)].into(),
                },
            );
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
