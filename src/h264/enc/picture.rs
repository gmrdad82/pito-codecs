use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use crate::h264::enc::mbinfo::{MbInfo, Mv};

pub const LUMA_PAD: usize = 64;
pub const CHROMA_PAD: usize = 32;

#[derive(Clone)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
    pub pad: usize,
}

impl Plane {
    pub fn new(w: usize, h: usize, pad: usize) -> Self {
        let stride = w + 2 * pad;
        Plane {
            data: vec![0; stride * (h + 2 * pad)],
            stride,
            w,
            h,
            pad,
        }
    }
    #[inline]
    pub fn idx(&self, x: isize, y: isize) -> usize {
        ((y + self.pad as isize) as usize) * self.stride + (x + self.pad as isize) as usize
    }
    pub fn extend_edges(&mut self) {
        let (w, h, pad, stride) = (self.w, self.h, self.pad, self.stride);
        for y in 0..h {
            let r = (y + pad) * stride;
            let l = self.data[r + pad];
            let rr = self.data[r + pad + w - 1];
            self.data[r..r + pad].fill(l);
            self.data[r + pad + w..r + stride].fill(rr);
        }
        let first = pad * stride;
        let last = (pad + h - 1) * stride;
        for y in 0..pad {
            self.data.copy_within(first..first + stride, y * stride);
            self.data
                .copy_within(last..last + stride, (pad + h + y) * stride);
        }
    }
}

#[derive(Clone)]
pub struct Frame {
    pub y: Plane,
    pub u: Plane,
    pub v: Plane,
}

impl Frame {
    pub fn new(w: usize, h: usize) -> Self {
        Frame {
            y: Plane::new(w, h, LUMA_PAD),
            u: Plane::new(w / 2, h / 2, CHROMA_PAD),
            v: Plane::new(w / 2, h / 2, CHROMA_PAD),
        }
    }
    pub fn extend_edges(&mut self) {
        self.y.extend_edges();
        self.u.extend_edges();
        self.v.extend_edges();
    }
}

pub struct Shared<T> {
    ptr: NonNull<T>,
    len: usize,
}

unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send + Sync> Sync for Shared<T> {}

impl<T: Clone> Shared<T> {
    pub fn new(len: usize, v: T) -> Self {
        let b: Box<[T]> = vec![v; len].into_boxed_slice();
        let len = b.len();
        let ptr = NonNull::new(Box::into_raw(b) as *mut T).unwrap_or(NonNull::dangling());
        Shared { ptr, len }
    }
}

impl<T> Shared<T> {
    #[inline]
    pub unsafe fn head(&self, len: usize) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), len.min(self.len)) }
    }
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn tail(&self, from: usize) -> &mut [T] {
        let from = from.min(self.len);
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(from), self.len - from) }
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                self.ptr.as_ptr(),
                self.len,
            )));
        }
    }
}

pub const DONE: usize = usize::MAX;

pub struct Pic {
    pub w: usize,
    pub h: usize,
    pub ys: usize,
    pub cs: usize,
    pub mbw: usize,
    pub y: Shared<u8>,
    pub u: Shared<u8>,
    pub v: Shared<u8>,
    pub hp: [Shared<u8>; 3],
    pub mbs: Shared<MbInfo>,
    luma: AtomicUsize,
    chroma: AtomicUsize,
    mb_rows: AtomicUsize,
    lock: Mutex<()>,
    cv: Condvar,
    pub poc: i32,
    pub id: u32,
}

#[derive(Clone, Copy)]
pub struct View<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub pad: usize,
}

impl View<'_> {
    #[inline]
    pub fn idx(&self, x: isize, y: isize) -> usize {
        ((y + self.pad as isize) as usize) * self.stride + (x + self.pad as isize) as usize
    }
}

#[derive(Clone, Copy)]
pub struct RefPic<'a> {
    pub y: View<'a>,
    pub u: View<'a>,
    pub v: View<'a>,
    pub hpel: [View<'a>; 3],
    pub mbs: &'a [MbInfo],
    pub poc: i32,
    pub id: u32,
    pub lim: isize,
}

pub struct BandMut<'a> {
    pub y: &'a mut [u8],
    pub u: &'a mut [u8],
    pub v: &'a mut [u8],
    pub mbs: &'a mut [MbInfo],
    pub ybase: usize,
    pub cbase: usize,
    pub mbase: usize,
}

impl Pic {
    pub fn new(w: usize, h: usize) -> Self {
        let ys = w + 2 * LUMA_PAD;
        let cs = w / 2 + 2 * CHROMA_PAD;
        let ylen = ys * (h + 2 * LUMA_PAD);
        let clen = cs * (h / 2 + 2 * CHROMA_PAD);
        let mbw = w / 16;
        Pic {
            w,
            h,
            ys,
            cs,
            mbw,
            y: Shared::new(ylen, 0),
            u: Shared::new(clen, 0),
            v: Shared::new(clen, 0),
            hp: [
                Shared::new(ylen, 0),
                Shared::new(ylen, 0),
                Shared::new(ylen, 0),
            ],
            mbs: Shared::new(mbw * (h / 16), MbInfo::default()),
            luma: AtomicUsize::new(0),
            chroma: AtomicUsize::new(0),
            mb_rows: AtomicUsize::new(0),
            lock: Mutex::new(()),
            cv: Condvar::new(),
            poc: 0,
            id: 0,
        }
    }

    pub fn reset(&mut self, poc: i32, id: u32) {
        *self.luma.get_mut() = 0;
        *self.chroma.get_mut() = 0;
        *self.mb_rows.get_mut() = 0;
        self.poc = poc;
        self.id = id;
    }

    pub fn luma_rows(&self) -> usize {
        self.h + 2 * LUMA_PAD
    }

    pub fn chroma_rows(&self) -> usize {
        self.h / 2 + 2 * CHROMA_PAD
    }

    pub fn publish(&self, luma: usize, chroma: usize, mb_rows: usize) {
        self.chroma.store(chroma, Ordering::Release);
        self.mb_rows.store(mb_rows, Ordering::Release);
        self.luma.store(luma, Ordering::Release);
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.cv.notify_all();
    }

    fn ready(&self, rows: usize) -> Option<usize> {
        let l = self.luma.load(Ordering::Acquire);
        (l == DONE || l >= rows).then_some(l)
    }

    pub fn view(&self, need: usize) -> RefPic<'_> {
        let full = need >= self.h;
        let rows = if full { DONE } else { LUMA_PAD + need + 2 };
        let mut spins = 0u32;
        let l = loop {
            if let Some(l) = self.ready(rows) {
                break l;
            }
            if spins < 64 {
                spins += 1;
                std::hint::spin_loop();
                continue;
            }
            let g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(l) = self.ready(rows) {
                break l;
            }
            drop(
                self.cv
                    .wait_timeout(g, std::time::Duration::from_millis(5))
                    .map(|r| r.0)
                    .unwrap_or_else(|e| e.into_inner().0),
            );
        };
        let c = self.chroma.load(Ordering::Acquire);
        let m = self.mb_rows.load(Ordering::Acquire);
        let ylen = l.min(self.luma_rows()) * self.ys;
        let clen = c.min(self.chroma_rows()) * self.cs;
        let mlen = m.min(self.h / 16) * self.mbw;
        unsafe {
            let yv = |d: &Shared<u8>| View {
                data: d.head(ylen),
                stride: self.ys,
                pad: LUMA_PAD,
            };
            let cv = |d: &Shared<u8>| View {
                data: d.head(clen),
                stride: self.cs,
                pad: CHROMA_PAD,
            };
            RefPic {
                y: yv(&self.y),
                u: cv(&self.u),
                v: cv(&self.v),
                hpel: [yv(&self.hp[0]), yv(&self.hp[1]), yv(&self.hp[2])],
                mbs: self.mbs.head(mlen),
                poc: self.poc,
                id: self.id,
                lim: if full {
                    (self.h + LUMA_PAD - 20) as isize
                } else {
                    need as isize
                },
            }
        }
    }

    pub unsafe fn band(&self, ybase: usize, cbase: usize, mbase: usize) -> BandMut<'_> {
        unsafe {
            BandMut {
                y: self.y.tail(ybase * self.ys),
                u: self.u.tail(cbase * self.cs),
                v: self.v.tail(cbase * self.cs),
                mbs: self.mbs.tail(mbase * self.mbw),
                ybase,
                cbase,
                mbase,
            }
        }
    }
}

pub fn extend_rows(data: &mut [u8], base: usize, stride: usize, pad: usize, w: usize, r0: usize, r1: usize) {
    for r in r0..r1 {
        let o = (r - base) * stride;
        let row = &mut data[o..o + stride];
        let l = row[pad];
        let rr = row[pad + w - 1];
        row[..pad].fill(l);
        row[pad + w..].fill(rr);
    }
}

pub fn copy_row(data: &mut [u8], base: usize, stride: usize, from: usize, to: std::ops::Range<usize>) {
    let src = (from - base) * stride;
    for r in to {
        let o = (r - base) * stride;
        data.copy_within(src..src + stride, o);
    }
}

pub fn hpel_rows(src: &[u8], stride: usize, rows: usize, r0: usize, r1: usize, out: [&mut [u8]; 3], scratch: &mut Vec<i16>) {
    let [hout, vout, cout] = out;
    let clampr = |r: isize| r.clamp(0, rows as isize - 1) as usize;
    let nrows = r1 - r0 + 5;
    scratch.resize(nrows * stride, 0);
    let hs = &mut scratch[..nrows * stride];
    for k in 0..nrows {
        let r = clampr(r0 as isize - 2 + k as isize);
        let row = &src[r * stride..r * stride + stride];
        let o = &mut hs[k * stride..k * stride + stride];
        let at = |x: isize| row[x.clamp(0, stride as isize - 1) as usize] as i16;
        for x in 0..2.min(stride) {
            let xi = x as isize;
            o[x] = at(xi - 2) + at(xi + 3) - 5 * (at(xi - 1) + at(xi + 2)) + 20 * (at(xi) + at(xi + 1));
        }
        for x in 2..stride.saturating_sub(3) {
            let e = &row[x - 2..x + 4];
            o[x] = (e[0] as i16 + e[5] as i16) - 5 * (e[1] as i16 + e[4] as i16)
                + 20 * (e[2] as i16 + e[3] as i16);
        }
        for x in stride.saturating_sub(3).max(2)..stride {
            let xi = x as isize;
            o[x] = at(xi - 2) + at(xi + 3) - 5 * (at(xi - 1) + at(xi + 2)) + 20 * (at(xi) + at(xi + 1));
        }
    }
    for r in r0..r1 {
        let lo = (r - r0) * stride;
        let k = r - r0 + 2;
        let hrow = &hs[k * stride..k * stride + stride];
        let ho = &mut hout[lo..lo + stride];
        for x in 0..stride {
            ho[x] = clip8((hrow[x] as i32 + 16) >> 5);
        }
        let rr: [&[u8]; 6] = std::array::from_fn(|i| {
            let s = clampr(r as isize - 2 + i as isize) * stride;
            &src[s..s + stride]
        });
        let vo = &mut vout[lo..lo + stride];
        for x in 0..stride {
            let s = rr[0][x] as i32 + rr[5][x] as i32 - 5 * (rr[1][x] as i32 + rr[4][x] as i32)
                + 20 * (rr[2][x] as i32 + rr[3][x] as i32);
            vo[x] = clip8((s + 16) >> 5);
        }
        let hk: [&[i16]; 6] =
            std::array::from_fn(|i| &hs[(k - 2 + i) * stride..(k - 1 + i) * stride]);
        let co = &mut cout[lo..lo + stride];
        for x in 0..stride {
            let s = hk[0][x] as i32 + hk[5][x] as i32 - 5 * (hk[1][x] as i32 + hk[4][x] as i32)
                + 20 * (hk[2][x] as i32 + hk[3][x] as i32);
            co[x] = clip8((s + 512) >> 10);
        }
    }
}

#[inline]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

const QPEL_SRC: [[(u8, u8, u8); 2]; 16] = [
    [(0, 0, 0), (0, 0, 0)],
    [(0, 0, 0), (1, 0, 0)],
    [(1, 0, 0), (1, 0, 0)],
    [(1, 0, 0), (0, 1, 0)],
    [(0, 0, 0), (2, 0, 0)],
    [(1, 0, 0), (2, 0, 0)],
    [(1, 0, 0), (3, 0, 0)],
    [(1, 0, 0), (2, 1, 0)],
    [(2, 0, 0), (2, 0, 0)],
    [(2, 0, 0), (3, 0, 0)],
    [(3, 0, 0), (3, 0, 0)],
    [(3, 0, 0), (2, 1, 0)],
    [(2, 0, 0), (0, 0, 1)],
    [(2, 0, 0), (1, 0, 1)],
    [(3, 0, 0), (1, 0, 1)],
    [(2, 1, 0), (1, 0, 1)],
];

impl RefPic<'_> {
    #[inline]
    fn plane(&self, p: u8) -> &View<'_> {
        match p {
            0 => &self.y,
            n => &self.hpel[n as usize - 1],
        }
    }

    pub fn mc_luma(
        &self,
        x: usize,
        y: usize,
        mv: Mv,
        w: usize,
        h: usize,
        out: &mut [u8],
        ostride: usize,
    ) {
        let xi = x as isize + (mv.x as isize >> 2);
        let yi = y as isize + (mv.y as isize >> 2);
        let q = ((mv.y & 3) * 4 + (mv.x & 3)) as usize;
        let [(pa, dxa, dya), (pb, dxb, dyb)] = QPEL_SRC[q];
        let a = self.plane(pa);
        let stride = a.stride;
        let ia = a.idx(xi + dxa as isize, yi + dya as isize);
        if pa == pb && dxa == dxb && dya == dyb {
            for r in 0..h {
                out[r * ostride..r * ostride + w]
                    .copy_from_slice(&a.data[ia + r * stride..ia + r * stride + w]);
            }
        } else {
            let b = self.plane(pb);
            let ib = b.idx(xi + dxb as isize, yi + dyb as isize);
            for r in 0..h {
                let ra = &a.data[ia + r * stride..ia + r * stride + w];
                let rb = &b.data[ib + r * stride..ib + r * stride + w];
                let o = &mut out[r * ostride..r * ostride + w];
                for i in 0..w {
                    o[i] = ((ra[i] as u16 + rb[i] as u16 + 1) >> 1) as u8;
                }
            }
        }
    }

    pub fn mc_chroma(
        &self,
        x: usize,
        y: usize,
        mv: Mv,
        w: usize,
        h: usize,
        out_u: &mut [u8],
        out_v: &mut [u8],
        ostride: usize,
    ) {
        let xi = x as isize + (mv.x as isize >> 3);
        let yi = y as isize + (mv.y as isize >> 3);
        let fx = (mv.x & 7) as u32;
        let fy = (mv.y & 7) as u32;
        let w00 = (8 - fx) * (8 - fy);
        let w01 = fx * (8 - fy);
        let w10 = (8 - fx) * fy;
        let w11 = fx * fy;
        for (p, out) in [(&self.u, out_u), (&self.v, out_v)] {
            let s = p.stride;
            let i0 = p.idx(xi, yi);
            for r in 0..h {
                let a = &p.data[i0 + r * s..i0 + r * s + w + 1];
                let b = &p.data[i0 + (r + 1) * s..i0 + (r + 1) * s + w + 1];
                let o = &mut out[r * ostride..r * ostride + w];
                for i in 0..w {
                    o[i] = ((w00 * a[i] as u32
                        + w01 * a[i + 1] as u32
                        + w10 * b[i] as u32
                        + w11 * b[i + 1] as u32
                        + 32)
                        >> 6) as u8;
                }
            }
        }
    }
}

#[inline]
pub fn avg_into(dst: &mut [u8], src: &[u8]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = ((*d as u16 + s as u16 + 1) >> 1) as u8;
    }
}
