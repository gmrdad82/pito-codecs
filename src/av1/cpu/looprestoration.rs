#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_int, c_uint};
use std::ops::Add;
use std::{cmp, iter, mem, slice};

use crate::av1::cpu::extensions::To;
use crate::av1::cpu::flags::bitflags;
use crate::av1::cpu::pod::{AsBytes, FromBytes};

use crate::av1::cpu::align::AlignedVec64;
use crate::av1::cpu::cpu::CpuFlags;
use crate::av1::cpu::cursor::CursorMut;
use crate::av1::cpu::disjoint_mut::DisjointMut;
use crate::av1::cpu::ffi_safe::FFISafe;
use crate::av1::cpu::include::common::bitdepth::{
    AsPrimitive, BPC, BitDepth, DynPixel, LeftPixelRow, ToPrimitive,
};
use crate::av1::cpu::include::common::intops::iclip;
use crate::av1::cpu::include::dav1d::picture::{
    FFISafeRav1dPictureDataComponentOffset, Rav1dPictureDataComponentOffset,
};
use crate::av1::cpu::strided::Strided as _;
use crate::av1::cpu::tables::dav1d_sgr_x_by_x;
use crate::av1::cpu::wrap_fn_ptr::wrap_fn_ptr;

bitflags! {
    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct LrEdgeFlags: u8 {
        const LEFT = 1 << 0;
        const RIGHT = 1 << 1;
        const TOP = 1 << 2;
        const BOTTOM = 1 << 3;
    }
}

impl LrEdgeFlags {
    pub const fn select(&self, select: bool) -> Self {
        if select { *self } else { Self::empty() }
    }
}

#[repr(C)]
pub struct LooprestorationParamsSgr {
    pub s0: u32,
    pub s1: u32,
    pub w0: i16,
    pub w1: i16,
}

unsafe impl crate::av1::cpu::pod::FromZeroes for LooprestorationParamsSgr {}

unsafe impl crate::av1::cpu::pod::FromBytes for LooprestorationParamsSgr {}

unsafe impl crate::av1::cpu::pod::AsBytes for LooprestorationParamsSgr {}

#[derive(Default)]
#[repr(C)]
#[repr(align(16))]
pub struct LooprestorationParams {
    pub filter: [[i16; 8]; 2],
}

impl LooprestorationParams {
    pub fn sgr(&self) -> &LooprestorationParamsSgr {
        const _: () = assert!(
            mem::size_of::<LooprestorationParams>() >= mem::size_of::<LooprestorationParamsSgr>()
        );
        let _: () = assert!(
            mem::align_of::<LooprestorationParams>() >= mem::align_of::<LooprestorationParamsSgr>()
        );
        FromBytes::ref_from_prefix(AsBytes::as_bytes(&self.filter)).unwrap()
    }

    pub fn sgr_mut(&mut self) -> &mut LooprestorationParamsSgr {
        const _: () = assert!(
            mem::size_of::<LooprestorationParams>() >= mem::size_of::<LooprestorationParamsSgr>()
        );
        const _: () = assert!(
            mem::align_of::<LooprestorationParams>() >= mem::align_of::<LooprestorationParamsSgr>()
        );
        FromBytes::mut_from_prefix(AsBytes::as_bytes_mut(&mut self.filter)).unwrap()
    }
}

wrap_fn_ptr!(pub unsafe extern "C" fn loop_restoration_filter(
    dst_ptr: *mut DynPixel,
    dst_stride: isize,
    left: *const LeftPixelRow<DynPixel>,
    lpf_ptr: *const DynPixel,
    w: c_int,
    h: c_int,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bitdepth_max: c_int,
    _dst: FFISafeRav1dPictureDataComponentOffset,
    _lpf: *const FFISafe<DisjointMut<AlignedVec64<u8>>>,
) -> ());

impl loop_restoration_filter::Fn {
    pub fn call<BD: BitDepth>(
        &self,
        dst: Rav1dPictureDataComponentOffset,
        left: &[LeftPixelRow<BD::Pixel>],
        lpf: &DisjointMut<AlignedVec64<u8>>,
        lpf_off: isize,
        w: c_int,
        h: c_int,
        params: &LooprestorationParams,
        edges: LrEdgeFlags,
        bd: BD,
    ) {
        let dst_ptr = dst.as_mut_ptr::<BD>().cast();
        let dst_stride = dst.stride();
        let left = left[..h as usize].as_ptr().cast();

        let lpf_ptr = lpf
            .as_mut_ptr()
            .cast::<BD::Pixel>()
            .wrapping_offset(lpf_off)
            .cast();
        let bd = bd.into_c();
        let dst = dst.into_ffi_safe();
        let lpf = FFISafe::new(lpf);

        unsafe {
            self.get()(
                dst_ptr, dst_stride, left, lpf_ptr, w, h, params, edges, bd, dst, lpf,
            )
        }
    }
}

pub struct Rav1dLoopRestorationDSPContext {
    pub wiener: [loop_restoration_filter::Fn; 2],
    pub sgr: [loop_restoration_filter::Fn; 3],
}

const REST_UNIT_STRIDE: usize = 256 * 3 / 2 + 3 + 3;

#[inline(never)]
fn padding<BD: BitDepth>(
    dst: &mut [BD::Pixel; (64 + 3 + 3) * REST_UNIT_STRIDE],
    p: Rav1dPictureDataComponentOffset,
    left: &[LeftPixelRow<BD::Pixel>],
    lpf: &DisjointMut<AlignedVec64<u8>>,
    lpf_off: isize,
    unit_w: usize,
    stripe_h: usize,
    edges: LrEdgeFlags,
) {
    let left = &left[..stripe_h];
    assert!(stripe_h > 0);
    let stride = p.pixel_stride::<BD>();

    let [have_left, have_right, have_top, have_bottom] = [
        LrEdgeFlags::LEFT,
        LrEdgeFlags::RIGHT,
        LrEdgeFlags::TOP,
        LrEdgeFlags::BOTTOM,
    ]
    .map(|lr_have| edges.contains(lr_have));
    let [have_left_3, have_right_3] = [have_left, have_right].map(|have| 3 * have as usize);

    let unit_w = unit_w + have_left_3 + have_right_3;
    let dst_l = &mut dst[3 - have_left_3..];
    let p = p - have_left_3;
    let lpf_off = lpf_off - (have_left_3 as isize);
    let abs_stride = stride.unsigned_abs();

    if have_top {
        let lpf_guard;
        let (above_1, above_2) = if stride < 0 {
            lpf_guard = lpf
                .slice_as::<_, BD::Pixel>(((lpf_off + stride) as usize.., ..abs_stride + unit_w));
            let above_2 = &*lpf_guard;
            let above_1 = &above_2[abs_stride..];
            (above_1, above_2)
        } else {
            lpf_guard = lpf.slice_as((lpf_off as usize.., ..abs_stride + unit_w));
            let above_1 = &*lpf_guard;
            let above_2 = &above_1[abs_stride..];
            (above_1, above_2)
        };
        BD::pixel_copy(dst_l, above_1, unit_w);
        BD::pixel_copy(&mut dst_l[REST_UNIT_STRIDE..], above_1, unit_w);
        BD::pixel_copy(&mut dst_l[2 * REST_UNIT_STRIDE..], above_2, unit_w);
    } else {
        let p = &*p.slice::<BD>(unit_w);
        BD::pixel_copy(dst_l, p, unit_w);
        BD::pixel_copy(&mut dst_l[REST_UNIT_STRIDE..], p, unit_w);
        BD::pixel_copy(&mut dst_l[2 * REST_UNIT_STRIDE..], p, unit_w);
        if have_left {
            let left = &left[0][1..];
            BD::pixel_copy(dst_l, left, 3);
            BD::pixel_copy(&mut dst_l[REST_UNIT_STRIDE..], left, left.len());
            BD::pixel_copy(&mut dst_l[2 * REST_UNIT_STRIDE..], left, left.len());
        }
    }

    let dst_tl = &mut dst_l[3 * REST_UNIT_STRIDE..];
    if have_bottom {
        let offset = lpf_off + (6 + if stride < 0 { 1 } else { 0 }) * stride;
        let lpf = &*lpf.slice_as((offset as usize.., ..abs_stride + unit_w));
        let (below_1, below_2) = if stride < 0 {
            (&lpf[abs_stride..], lpf)
        } else {
            (lpf, &lpf[abs_stride..])
        };
        BD::pixel_copy(&mut dst_tl[stripe_h * REST_UNIT_STRIDE..], below_1, unit_w);
        BD::pixel_copy(
            &mut dst_tl[(stripe_h + 1) * REST_UNIT_STRIDE..],
            below_2,
            unit_w,
        );
        BD::pixel_copy(
            &mut dst_tl[(stripe_h + 2) * REST_UNIT_STRIDE..],
            below_2,
            unit_w,
        );
    } else {
        let src = p + ((stripe_h - 1) as isize * stride);
        let src = &*src.slice::<BD>(unit_w);
        BD::pixel_copy(&mut dst_tl[stripe_h * REST_UNIT_STRIDE..], src, unit_w);
        BD::pixel_copy(
            &mut dst_tl[(stripe_h + 1) * REST_UNIT_STRIDE..],
            src,
            unit_w,
        );
        BD::pixel_copy(
            &mut dst_tl[(stripe_h + 2) * REST_UNIT_STRIDE..],
            src,
            unit_w,
        );
        if have_left {
            let left = &left[stripe_h - 1][1..];
            BD::pixel_copy(&mut dst_tl[stripe_h * REST_UNIT_STRIDE..], left, left.len());
            BD::pixel_copy(
                &mut dst_tl[(stripe_h + 1) * REST_UNIT_STRIDE..],
                left,
                left.len(),
            );
            BD::pixel_copy(
                &mut dst_tl[(stripe_h + 2) * REST_UNIT_STRIDE..],
                left,
                left.len(),
            );
        }
    }

    let len = unit_w - have_left_3;
    for j in 0..stripe_h {
        let p = p + have_left_3 + (j as isize * stride);
        BD::pixel_copy(
            &mut dst_tl[j * REST_UNIT_STRIDE + have_left_3..],
            &p.slice::<BD>(len),
            len,
        );
    }

    if !have_right {
        for j in 0..stripe_h + 6 {
            let row_last = dst_l[(unit_w - 1) + j * REST_UNIT_STRIDE];
            let pad = &mut dst_l[unit_w + j * REST_UNIT_STRIDE..];
            BD::pixel_set(pad, row_last, 3);
        }
    }

    if !have_left {
        for j in 0..stripe_h + 6 {
            let offset = j * REST_UNIT_STRIDE;

            let val = dst[3 + offset];
            BD::pixel_set(&mut dst[offset..], val, 3);
        }
    } else {
        let dst = &mut dst[3 * REST_UNIT_STRIDE..];
        for j in 0..stripe_h {
            BD::pixel_copy(&mut dst[j * REST_UNIT_STRIDE..], &left[j][1..], 3);
        }
    };
}

fn reconstruct_lpf_offset<BD: BitDepth>(
    lpf: &DisjointMut<AlignedVec64<u8>>,
    ptr: *const BD::Pixel,
) -> isize {
    let base = lpf.as_mut_ptr().cast::<BD::Pixel>();
    (ptr as isize - base as isize) / (mem::size_of::<BD::Pixel>() as isize)
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn wiener_c_erased<BD: BitDepth>(
    _p_ptr: *mut DynPixel,
    _stride: isize,
    left: *const LeftPixelRow<DynPixel>,
    lpf_ptr: *const DynPixel,
    w: c_int,
    h: c_int,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bitdepth_max: c_int,
    p: FFISafeRav1dPictureDataComponentOffset,
    lpf: *const FFISafe<DisjointMut<AlignedVec64<u8>>>,
) {
    let p = unsafe { FFISafe::from_with_offset(p) };
    let left = left.cast();

    let lpf = unsafe { FFISafe::get(lpf) };
    let lpf_ptr = lpf_ptr.cast();
    let lpf_off = reconstruct_lpf_offset::<BD>(lpf, lpf_ptr);
    let bd = BD::from_c(bitdepth_max);
    let w = w as usize;
    let h = h as usize;

    let left = unsafe { slice::from_raw_parts(left, h) };
    wiener_rust(p, left, lpf, lpf_off, w, h, params, edges, bd)
}

fn wiener_rust<BD: BitDepth>(
    p: Rav1dPictureDataComponentOffset,
    left: &[LeftPixelRow<BD::Pixel>],
    lpf: &DisjointMut<AlignedVec64<u8>>,
    lpf_off: isize,
    w: usize,
    h: usize,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bd: BD,
) {
    let mut tmp = [0.into(); (64 + 3 + 3) * REST_UNIT_STRIDE];

    padding::<BD>(&mut tmp, p, left, lpf, lpf_off, w, h, edges);

    let mut hor = [0; (64 + 3 + 3) * REST_UNIT_STRIDE];

    let filter = &params.filter;
    let bitdepth = bd.bitdepth().as_::<c_int>();
    let round_bits_h = 3 + (bitdepth == 12) as c_int * 2;
    let rounding_off_h = 1 << round_bits_h - 1;
    let clip_limit = 1 << bitdepth + 1 + 7 - round_bits_h;
    for (tmp, hor) in tmp
        .chunks_exact(REST_UNIT_STRIDE)
        .zip(hor.chunks_exact_mut(REST_UNIT_STRIDE))
        .take(h + 6)
    {
        for i in 0..w {
            let mut sum = 1 << bitdepth + 6;

            if BD::BPC == BPC::BPC8 {
                sum += tmp[i + 3].to::<i32>() * 128;
            }

            for (&tmp, &filter) in iter::zip(&tmp[i..i + 7], &filter[0][..7]) {
                sum += tmp.to::<i32>() * filter as c_int;
            }

            hor[i] = iclip(sum + rounding_off_h >> round_bits_h, 0, clip_limit - 1) as u16;
        }
    }

    let round_bits_v = 11 - (bitdepth == 12) as c_int * 2;
    let rounding_off_v = 1 << round_bits_v - 1;
    let round_offset = 1 << bitdepth + (round_bits_v - 1);
    for j in 0..h {
        for i in 0..w {
            let mut sum = -round_offset;
            let z = &hor[j * REST_UNIT_STRIDE + i..(j + 7) * REST_UNIT_STRIDE];

            for k in 0..7 {
                sum += z[k * REST_UNIT_STRIDE] as c_int * filter[1][k] as c_int;
            }

            let p = p + (j as isize * p.pixel_stride::<BD>()) + i;
            *p.index_mut::<BD>() =
                iclip(sum + rounding_off_v >> round_bits_v, 0, bd.into_c()).as_();
        }
    }
}

fn boxsum3<BD: BitDepth>(
    sumsq: &mut [i32; (64 + 2 + 2) * REST_UNIT_STRIDE],
    sum: &mut [BD::Coef; (64 + 2 + 2) * REST_UNIT_STRIDE],
    src: &[BD::Pixel; (64 + 3 + 3) * REST_UNIT_STRIDE],
    w: usize,
    h: usize,
) {
    let src = &src[REST_UNIT_STRIDE..];

    for x in 1..w - 1 {
        let mut sum_v = &mut sum[x..];
        let mut sumsq_v = &mut sumsq[x..];
        let mut s = &src[x..];
        let mut a: c_int = s[0].as_();
        let mut a2 = a * a;
        let mut b: c_int = s[REST_UNIT_STRIDE].as_();
        let mut b2 = b * b;

        for _ in 2..h - 2 {
            s = &s[REST_UNIT_STRIDE..];
            let c: c_int = s[REST_UNIT_STRIDE].as_();
            let c2 = c * c;
            sum_v = &mut sum_v[REST_UNIT_STRIDE..];
            sumsq_v = &mut sumsq_v[REST_UNIT_STRIDE..];
            sum_v[0] = (a + b + c).as_();
            sumsq_v[0] = a2 + b2 + c2;
            a = b;
            a2 = b2;
            b = c;
            b2 = c2;
        }
    }

    let mut sum = &mut sum[REST_UNIT_STRIDE..];
    let mut sumsq = &mut sumsq[REST_UNIT_STRIDE..];

    for _ in 2..h - 2 {
        let mut a = sum[1];
        let mut a2 = sumsq[1];
        let mut b = sum[2];
        let mut b2 = sumsq[2];

        for x in 2..w - 2 {
            let c = sum[x + 1];
            let c2 = sumsq[x + 1];
            sum[x] = a + b + c;
            sumsq[x] = a2 + b2 + c2;
            a = b;
            a2 = b2;
            b = c;
            b2 = c2;
        }

        sum = &mut sum[REST_UNIT_STRIDE..];
        sumsq = &mut sumsq[REST_UNIT_STRIDE..];
    }
}

fn boxsum5<BD: BitDepth>(
    sumsq: &mut [i32; (64 + 2 + 2) * REST_UNIT_STRIDE],
    sum: &mut [BD::Coef; (64 + 2 + 2) * REST_UNIT_STRIDE],
    src: &[BD::Pixel; (64 + 3 + 3) * REST_UNIT_STRIDE],
    w: usize,
    h: usize,
) {
    for x in 0..w {
        let mut sum_v = &mut sum[x..];
        let mut sumsq_v = &mut sumsq[x..];
        let s = &src[x..];
        let mut a: c_int = (s[0]).as_();
        let mut a2 = a * a;
        let mut b: c_int = (s[1 * REST_UNIT_STRIDE]).as_();
        let mut b2 = b * b;
        let mut c: c_int = (s[2 * REST_UNIT_STRIDE]).as_();
        let mut c2 = c * c;
        let mut d: c_int = (s[3 * REST_UNIT_STRIDE]).as_();
        let mut d2 = d * d;

        let mut s = &src[3 * REST_UNIT_STRIDE + x..];

        for _ in 2..h - 2 {
            s = &s[REST_UNIT_STRIDE..];
            let e: c_int = s[0].as_();
            let e2 = e * e;
            sum_v = &mut sum_v[REST_UNIT_STRIDE..];
            sumsq_v = &mut sumsq_v[REST_UNIT_STRIDE..];
            sum_v[0] = (a + b + c + d + e).as_();
            sumsq_v[0] = a2 + b2 + c2 + d2 + e2;
            a = b;
            b = c;
            c = d;
            d = e;
            a2 = b2;
            b2 = c2;
            c2 = d2;
            d2 = e2;
        }
    }

    let mut sum = &mut sum[REST_UNIT_STRIDE..];
    let mut sumsq = &mut sumsq[REST_UNIT_STRIDE..];
    for _ in 2..h - 2 {
        let mut a = sum[0];
        let mut a2 = sumsq[0];
        let mut b = sum[1];
        let mut b2 = sumsq[1];
        let mut c = sum[2];
        let mut c2 = sumsq[2];
        let mut d = sum[3];
        let mut d2 = sumsq[3];

        for x in 2..w - 2 {
            let e = sum[x + 2];
            let e2 = sumsq[x + 2];
            sum[x] = a + b + c + d + e;
            sumsq[x] = a2 + b2 + c2 + d2 + e2;
            a = b;
            b = c;
            c = d;
            d = e;
            a2 = b2;
            b2 = c2;
            c2 = d2;
            d2 = e2;
        }
        sum = &mut sum[REST_UNIT_STRIDE..];
        sumsq = &mut sumsq[REST_UNIT_STRIDE..];
    }
}

#[inline(never)]
fn selfguided_filter<BD: BitDepth>(
    dst: &mut [BD::Coef; 64 * 384],
    src: &[BD::Pixel; (64 + 3 + 3) * REST_UNIT_STRIDE],
    w: usize,
    h: usize,
    n: c_int,
    s: c_uint,
    bd: BD,
) {
    let sgr_one_by_x = if n == 25 { 164 } else { 455 };

    let mut sumsq = [0; (64 + 2 + 2) * REST_UNIT_STRIDE];

    let mut sum = [0.as_::<BD::Coef>(); (64 + 2 + 2) * REST_UNIT_STRIDE];

    let step = (n == 25) as usize + 1;
    if n == 25 {
        boxsum5::<BD>(&mut sumsq, &mut sum, src, w + 6, h + 6);
    } else {
        boxsum3::<BD>(&mut sumsq, &mut sum, src, w + 6, h + 6);
    }
    let bitdepth_min_8 = bd.bitdepth() - 8;

    let mut a = CursorMut::new(&mut sumsq) + 2 * REST_UNIT_STRIDE + 3;
    let mut b = CursorMut::new(&mut sum) + 2 * REST_UNIT_STRIDE + 3;

    let mut aa = a.clone() - REST_UNIT_STRIDE;
    let mut bb = b.clone() - REST_UNIT_STRIDE;
    for _ in (-1..h as isize + 1).step_by(step) {
        for i in -1..w as isize + 1 {
            let a = aa[i] + (1 << 2 * bitdepth_min_8 >> 1) >> 2 * bitdepth_min_8;
            let b = bb[i].as_::<c_int>() + (1 << bitdepth_min_8 >> 1) >> bitdepth_min_8;

            let p = cmp::max(a * n - b * b, 0) as c_uint;
            let z = (p * s + (1 << 19)) >> 20;
            let x = dav1d_sgr_x_by_x[cmp::min(z, 255) as usize] as c_uint;

            aa[i] = ((x * bb[i].as_::<c_uint>() * sgr_one_by_x + (1 << 11)) >> 12) as c_int;
            bb[i] = x.as_::<BD::Coef>();
        }
        aa += step as usize * REST_UNIT_STRIDE;
        bb += step as usize * REST_UNIT_STRIDE;
    }

    fn six_neighbors<P>(p: &CursorMut<P>, i: isize) -> c_int
    where
        P: Add<Output = P> + ToPrimitive<c_int> + Copy,
    {
        let stride = REST_UNIT_STRIDE as isize;
        (p[i - stride] + p[i + stride]).as_::<c_int>() * 6
            + (p[i - 1 - stride] + p[i - 1 + stride] + p[i + 1 - stride] + p[i + 1 + stride])
                .as_::<c_int>()
                * 5
    }

    fn eight_neighbors<P>(p: &CursorMut<P>, i: isize) -> c_int
    where
        P: Add<Output = P> + ToPrimitive<c_int> + Copy,
    {
        let stride = REST_UNIT_STRIDE as isize;
        (p[i] + p[i - 1] + p[i + 1] + p[i - stride] + p[i + stride]).as_::<c_int>() * 4
            + (p[i - 1 - stride] + p[i - 1 + stride] + p[i + 1 - stride] + p[i + 1 + stride])
                .as_::<c_int>()
                * 3
    }

    const MAX_RESTORATION_WIDTH: usize = 256 * 3 / 2;

    let mut src = &src[3 * REST_UNIT_STRIDE + 3..];
    let mut dst = dst.as_mut_slice();
    if n == 25 {
        let mut j = 0;
        while j < h - 1 {
            for i in 0..w {
                let (a, b) = (six_neighbors(&b, i as isize), six_neighbors(&a, i as isize));
                dst[i] = ((b - a * src[i].as_::<c_int>() + (1 << 8)) >> 9).as_();
            }
            dst = &mut dst[MAX_RESTORATION_WIDTH..];
            src = &src[REST_UNIT_STRIDE..];
            b += REST_UNIT_STRIDE;
            a += REST_UNIT_STRIDE;
            for i in 0..w {
                let (a, b) = (
                    b[i].as_::<c_int>() * 6 + (b[i as isize - 1] + b[i + 1]).as_::<c_int>() * 5,
                    a[i] * 6 + (a[i as isize - 1] + a[i + 1]) * 5,
                );
                dst[i] = (b - a * src[i].as_::<c_int>() + (1 << 7) >> 8).as_();
            }
            dst = &mut dst[MAX_RESTORATION_WIDTH..];
            src = &src[REST_UNIT_STRIDE..];
            b += REST_UNIT_STRIDE;
            a += REST_UNIT_STRIDE;
            j += 2;
        }

        if j + 1 == h {
            for i in 0..w {
                let (a, b) = (six_neighbors(&b, i as isize), six_neighbors(&a, i as isize));
                dst[i] = (b - a * src[i].as_::<c_int>() + (1 << 8) >> 9).as_();
            }
        }
    } else {
        for _ in 0..h {
            for i in 0..w {
                let (a, b) = (
                    eight_neighbors(&b, i as isize),
                    eight_neighbors(&a, i as isize),
                );
                dst[i] = (b - a * src[i].as_::<c_int>() + (1 << 8) >> 9).as_();
            }
            dst = &mut dst[384..];
            src = &src[REST_UNIT_STRIDE..];
            b += REST_UNIT_STRIDE;
            a += REST_UNIT_STRIDE;
        }
    };
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn sgr_5x5_c_erased<BD: BitDepth>(
    _p_ptr: *mut DynPixel,
    _stride: isize,
    left: *const LeftPixelRow<DynPixel>,
    lpf_ptr: *const DynPixel,
    w: c_int,
    h: c_int,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bitdepth_max: c_int,
    p: FFISafeRav1dPictureDataComponentOffset,
    lpf: *const FFISafe<DisjointMut<AlignedVec64<u8>>>,
) {
    let p = unsafe { FFISafe::from_with_offset(p) };
    let left = left.cast();

    let lpf = unsafe { FFISafe::get(lpf) };
    let lpf_ptr = lpf_ptr.cast();
    let lpf_off = reconstruct_lpf_offset::<BD>(lpf, lpf_ptr);
    let w = w as usize;
    let h = h as usize;
    let bd = BD::from_c(bitdepth_max);

    let left = unsafe { slice::from_raw_parts(left, h) };
    sgr_5x5_rust(p, left, lpf, lpf_off, w, h, params, edges, bd)
}

fn sgr_5x5_rust<BD: BitDepth>(
    p: Rav1dPictureDataComponentOffset,
    left: &[LeftPixelRow<BD::Pixel>],
    lpf: &DisjointMut<AlignedVec64<u8>>,
    lpf_off: isize,
    w: usize,
    h: usize,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bd: BD,
) {
    let mut tmp = [0.as_(); (64 + 3 + 3) * REST_UNIT_STRIDE];

    let mut dst = [0.as_(); 64 * 384];

    padding::<BD>(&mut tmp, p, left, lpf, lpf_off, w, h, edges);
    let sgr = params.sgr();
    selfguided_filter(&mut dst, &mut tmp, w, h, 25, sgr.s0, bd);

    let w0 = sgr.w0 as c_int;
    for j in 0..h {
        let p = p + (j as isize * p.pixel_stride::<BD>());
        let p = &mut *p.slice_mut::<BD>(w);
        for i in 0..w {
            let v = w0 * dst[j * 384 + i].as_::<c_int>();
            p[i] = bd.iclip_pixel(p[i].as_::<c_int>() + (v + (1 << 10) >> 11));
        }
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn sgr_3x3_c_erased<BD: BitDepth>(
    _p_ptr: *mut DynPixel,
    _stride: isize,
    left: *const LeftPixelRow<DynPixel>,
    lpf_ptr: *const DynPixel,
    w: c_int,
    h: c_int,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bitdepth_max: c_int,
    p: FFISafeRav1dPictureDataComponentOffset,
    lpf: *const FFISafe<DisjointMut<AlignedVec64<u8>>>,
) {
    let p = unsafe { FFISafe::from_with_offset(p) };
    let left = left.cast();

    let lpf = unsafe { FFISafe::get(lpf) };
    let lpf_ptr = lpf_ptr.cast();
    let lpf_off = reconstruct_lpf_offset::<BD>(lpf, lpf_ptr);
    let w = w as usize;
    let h = h as usize;
    let bd = BD::from_c(bitdepth_max);

    let left = unsafe { slice::from_raw_parts(left, h) };
    sgr_3x3_rust(p, left, lpf, lpf_off, w, h, params, edges, bd)
}

fn sgr_3x3_rust<BD: BitDepth>(
    p: Rav1dPictureDataComponentOffset,
    left: &[LeftPixelRow<BD::Pixel>],
    lpf: &DisjointMut<AlignedVec64<u8>>,
    lpf_off: isize,
    w: usize,
    h: usize,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bd: BD,
) {
    let mut tmp = [0.as_(); (64 + 3 + 3) * REST_UNIT_STRIDE];
    let mut dst = [0.as_(); 64 * 384];

    padding::<BD>(&mut tmp, p, left, lpf, lpf_off, w, h, edges);
    let sgr = params.sgr();
    selfguided_filter(&mut dst, &mut tmp, w, h, 9, sgr.s1, bd);

    let w1 = sgr.w1 as c_int;
    for j in 0..h {
        let p = p + (j as isize * p.pixel_stride::<BD>());
        let p = &mut *p.slice_mut::<BD>(w);
        for i in 0..w {
            let v = w1 * dst[j * 384 + i].as_::<c_int>();
            p[i] = bd.iclip_pixel(p[i].as_::<c_int>() + (v + (1 << 10) >> 11));
        }
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn sgr_mix_c_erased<BD: BitDepth>(
    _p_ptr: *mut DynPixel,
    _stride: isize,
    left: *const LeftPixelRow<DynPixel>,
    lpf_ptr: *const DynPixel,
    w: c_int,
    h: c_int,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bitdepth_max: c_int,
    p: FFISafeRav1dPictureDataComponentOffset,
    lpf: *const FFISafe<DisjointMut<AlignedVec64<u8>>>,
) {
    let p = unsafe { FFISafe::from_with_offset(p) };
    let left = left.cast();

    let lpf = unsafe { FFISafe::get(lpf) };
    let lpf_ptr = lpf_ptr.cast();
    let lpf_off = reconstruct_lpf_offset::<BD>(lpf, lpf_ptr);
    let w = w as usize;
    let h = h as usize;
    let bd = BD::from_c(bitdepth_max);

    let left = unsafe { slice::from_raw_parts(left, h) };
    sgr_mix_rust(p, left, lpf, lpf_off, w, h, params, edges, bd)
}

fn sgr_mix_rust<BD: BitDepth>(
    p: Rav1dPictureDataComponentOffset,
    left: &[LeftPixelRow<BD::Pixel>],
    lpf: &DisjointMut<AlignedVec64<u8>>,
    lpf_off: isize,
    w: usize,
    h: usize,
    params: &LooprestorationParams,
    edges: LrEdgeFlags,
    bd: BD,
) {
    let mut tmp = [0.as_(); (64 + 3 + 3) * REST_UNIT_STRIDE];
    let mut dst0 = [0.as_(); 64 * 384];
    let mut dst1 = [0.as_(); 64 * 384];

    padding::<BD>(&mut tmp, p, left, lpf, lpf_off, w, h, edges);
    let sgr = params.sgr();
    selfguided_filter(&mut dst0, &mut tmp, w, h, 25, sgr.s0, bd);
    selfguided_filter(&mut dst1, &mut tmp, w, h, 9, sgr.s1, bd);

    let w0 = sgr.w0 as c_int;
    let w1 = sgr.w1 as c_int;
    for j in 0..h {
        let p = p + (j as isize * p.pixel_stride::<BD>());
        let p = &mut *p.slice_mut::<BD>(w);
        for i in 0..w {
            let v = w0 * dst0[j * 384 + i].as_::<c_int>() + w1 * dst1[j * 384 + i].as_::<c_int>();
            p[i] = bd.iclip_pixel(p[i].as_::<c_int>() + (v + (1 << 10) >> 11));
        }
    }
}

impl Rav1dLoopRestorationDSPContext {
    pub const fn default<BD: BitDepth>() -> Self {
        Self {
            wiener: [loop_restoration_filter::Fn::new(wiener_c_erased::<BD>); 2],
            sgr: [
                loop_restoration_filter::Fn::new(sgr_5x5_c_erased::<BD>),
                loop_restoration_filter::Fn::new(sgr_3x3_c_erased::<BD>),
                loop_restoration_filter::Fn::new(sgr_mix_c_erased::<BD>),
            ],
        }
    }

    #[inline(always)]
    const fn init<BD: BitDepth>(self, flags: CpuFlags, bpc: u8) -> Self {
        #[allow(unreachable_code)]
        {
            let _ = flags;
            let _ = bpc;
            self
        }
    }

    pub const fn new<BD: BitDepth>(flags: CpuFlags, bpc: u8) -> Self {
        Self::default::<BD>().init::<BD>(flags, bpc)
    }
}
