pub use crate::vp9::frame::{MiGrid, MiInfo, Mv};

#[derive(Clone, Copy, Debug, Default)]
pub struct TxRef {
    pub offset: u32,
    pub eob: u16,
}

pub struct SbCoefs {
    pub levels: Vec<i16>,
    pub luma: [TxRef; 256],
    pub chroma: [[TxRef; 64]; 2],
}

impl Default for SbCoefs {
    fn default() -> Self {
        SbCoefs {
            levels: Vec::new(),
            luma: [TxRef::default(); 256],
            chroma: [[TxRef::default(); 64]; 2],
        }
    }
}

impl SbCoefs {
    pub fn clear(&mut self) {
        self.levels.clear();
    }

    pub fn slot(&mut self, plane: usize, x4: usize, y4: usize) -> &mut TxRef {
        if plane == 0 {
            &mut self.luma[y4 * 16 + x4]
        } else {
            &mut self.chroma[plane - 1][y4 * 8 + x4]
        }
    }

    pub fn get(&self, plane: usize, x4: usize, y4: usize) -> TxRef {
        if plane == 0 {
            self.luma[y4 * 16 + x4]
        } else {
            self.chroma[plane - 1][y4 * 8 + x4]
        }
    }

    pub fn put(&mut self, plane: usize, x4: usize, y4: usize, levels: &[i16]) {
        let offset = self.levels.len() as u32;
        self.levels.extend_from_slice(levels);
        *self.slot(plane, x4, y4) = TxRef {
            offset,
            eob: levels.len() as u16,
        };
    }

    pub fn levels(&self, t: TxRef) -> &[i16] {
        &self.levels[t.offset as usize..t.offset as usize + t.eob as usize]
    }
}

pub struct SharedGrid<T> {
    ptr: *mut T,
    len: usize,
    pub cols: usize,
}

unsafe impl<T: Send> Sync for SharedGrid<T> {}
unsafe impl<T: Send> Send for SharedGrid<T> {}

impl<T: Copy> SharedGrid<T> {
    pub fn new(v: &mut [T], cols: usize) -> Self {
        SharedGrid {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            cols,
        }
    }

    #[inline(always)]
    pub fn get(&self, r: usize, c: usize) -> T {
        let i = r * self.cols + c;
        assert!(i < self.len);
        unsafe { self.ptr.add(i).read() }
    }

    #[inline(always)]
    pub fn set(&self, r: usize, c: usize, v: T) {
        let i = r * self.cols + c;
        assert!(i < self.len);
        unsafe { self.ptr.add(i).write(v) }
    }

    pub fn read(&self, i: usize, out: &mut [T]) {
        assert!(i + out.len() <= self.len);
        unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(i), out.as_mut_ptr(), out.len()) }
    }

    pub fn write(&self, i: usize, src: &[T]) {
        assert!(i + src.len() <= self.len);
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.add(i), src.len()) }
    }
}

pub const PAD: [usize; 3] = [96, 48, 48];

pub struct Picture {
    pub planes: [Vec<u8>; 3],
    pub stride: [usize; 3],
    pub origin: [usize; 3],
    pub width: usize,
    pub height: usize,
    pub aligned: [(usize, usize); 3],
}

impl Picture {
    pub fn new(width: usize, height: usize) -> Picture {
        let aw = width.div_ceil(64) * 64;
        let ah = height.div_ceil(64) * 64;
        let dims = [(aw, ah), (aw / 2, ah / 2), (aw / 2, ah / 2)];
        let mk = |p: usize| {
            let (w, h) = dims[p];
            let stride = w + 2 * PAD[p];
            (
                vec![0u8; stride * (h + 2 * PAD[p])],
                stride,
                PAD[p] * stride + PAD[p],
            )
        };
        let (a, b, c) = (mk(0), mk(1), mk(2));
        Picture {
            stride: [a.1, b.1, c.1],
            origin: [a.2, b.2, c.2],
            planes: [a.0, b.0, c.0],
            width,
            height,
            aligned: dims,
        }
    }

    #[inline(always)]
    pub fn idx(&self, p: usize, x: usize, y: usize) -> usize {
        self.origin[p] + y * self.stride[p] + x
    }

    pub fn visible(&self, p: usize) -> (usize, usize) {
        if p == 0 {
            (self.width, self.height)
        } else {
            (self.width.div_ceil(2), self.height.div_ceil(2))
        }
    }

    pub fn extend_borders(&mut self) {
        for p in 0..3 {
            let (w, h) = self.visible(p);
            let (aw, ah) = self.aligned[p];
            let (stride, origin, pad) = (self.stride[p], self.origin[p], PAD[p]);
            let d = &mut self.planes[p];
            for y in 0..h {
                let row = origin + y * stride;
                let l = d[row];
                let r = d[row + w - 1];
                d[row - pad..row].fill(l);
                d[row + w..row + aw + pad].fill(r);
            }
            let first = origin - pad;
            let last = origin + (h - 1) * stride - pad;
            for y in 1..=pad {
                d.copy_within(first..first + stride, first - y * stride);
            }
            for y in h..ah + pad {
                d.copy_within(last..last + stride, origin + y * stride - pad);
            }
        }
    }
}
