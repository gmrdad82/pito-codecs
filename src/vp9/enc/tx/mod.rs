#[cfg(target_arch = "x86_64")]
mod avx2;
mod lane;
mod net;
mod quant;
mod tables;
mod xform;

#[cfg(test)]
mod tests;

use lane::{Isa, Scalar};
use quant::Scan;

pub use quant::{MAX_LEVEL, bit_cost, token};
pub use tables::{COEFBAND_4X4, COEFBAND_8X8PLUS, ENERGY_CLASS, bands, neighbors, scan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quant {
    pub dc: i32,
    pub ac: i32,
}

pub struct TokenCosts<'a> {
    pub more: &'a [[[u32; 2]; 6]; 6],
    pub token: &'a [[[u32; 11]; 6]; 6],
}

pub struct Block<'a> {
    pub src: &'a [u8],
    pub src_stride: usize,
    pub dst: &'a mut [u8],
    pub dst_stride: usize,
    pub tx_size: u8,
    pub tx_type: u8,
    pub lossless: bool,
    pub ctx: usize,
    pub is_inter: bool,
    pub chroma: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub eob: usize,
    pub dist: u64,
    pub rate: u32,
}

#[repr(C, align(32))]
struct Buf<const N2: usize>([i16; N2]);

fn scan_for(tx_size: u8, tx_type: u8) -> Scan<'static> {
    let set = tables::scan_set(tx_size, tx_type);
    Scan {
        scan: scan(tx_size, tx_type),
        nb: &set.neighbors,
        bands: bands(tx_size),
        iscan_t: &set.iscan_t,
        log2n: 2 + tx_size as usize,
    }
}

fn effective_type(tx_size: u8, tx_type: u8, lossless: bool) -> u8 {
    if lossless || tx_size == 3 {
        0
    } else {
        tx_type & 3
    }
}

pub fn encode(
    b: Block,
    q: Quant,
    costs: &TokenCosts,
    lambda: u32,
    levels: &mut [i16; 1024],
) -> Out {
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2_fma() {
        return unsafe { encode_avx2(b, q, costs, lambda, levels) };
    }
    encode_with::<Scalar>(b, q, costs, lambda, levels)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn encode_avx2(
    b: Block,
    q: Quant,
    costs: &TokenCosts,
    lambda: u32,
    levels: &mut [i16; 1024],
) -> Out {
    encode_with::<avx2::Avx2>(b, q, costs, lambda, levels)
}

#[allow(clippy::too_many_arguments)]
pub fn reconstruct(
    levels: &[i16],
    eob: usize,
    tx_size: u8,
    tx_type: u8,
    lossless: bool,
    q: Quant,
    dst: &mut [u8],
    dst_stride: usize,
) {
    #[cfg(target_arch = "x86_64")]
    if crate::media::isa::avx2_fma() {
        return unsafe {
            reconstruct_avx2(levels, eob, tx_size, tx_type, lossless, q, dst, dst_stride)
        };
    }
    reconstruct_with::<Scalar>(levels, eob, tx_size, tx_type, lossless, q, dst, dst_stride)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
#[allow(clippy::too_many_arguments)]
unsafe fn reconstruct_avx2(
    levels: &[i16],
    eob: usize,
    tx_size: u8,
    tx_type: u8,
    lossless: bool,
    q: Quant,
    dst: &mut [u8],
    dst_stride: usize,
) {
    reconstruct_with::<avx2::Avx2>(levels, eob, tx_size, tx_type, lossless, q, dst, dst_stride)
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn reconstruct_with<I: Isa>(
    levels: &[i16],
    eob: usize,
    tx_size: u8,
    tx_type: u8,
    lossless: bool,
    q: Quant,
    dst: &mut [u8],
    dst_stride: usize,
) {
    if eob == 0 {
        return;
    }
    let tx_size = if lossless { 0 } else { tx_size.min(3) };
    let tx_type = effective_type(tx_size, tx_type, lossless);
    let s = scan_for(tx_size, tx_type);
    let r = Recon {
        tx_size,
        tx_type,
        lossless,
        dst,
        stride: dst_stride,
    };
    match tx_size {
        0 => recon_n::<I, 16>(levels, eob, &s, q, r),
        1 => recon_n::<I, 64>(levels, eob, &s, q, r),
        2 => recon_n::<I, 256>(levels, eob, &s, q, r),
        _ => recon_n::<I, 1024>(levels, eob, &s, q, r),
    }
}

struct Recon<'a> {
    tx_size: u8,
    tx_type: u8,
    lossless: bool,
    dst: &'a mut [u8],
    stride: usize,
}

#[inline(always)]
fn recon_n<I: Isa, const N2: usize>(levels: &[i16], eob: usize, s: &Scan, q: Quant, r: Recon) {
    let mut dq = Buf([0; N2]);
    let rows = quant::dequantise(levels, eob, s, q, &mut dq.0);
    apply::<I>(&mut dq.0, eob, rows, r);
}

#[inline(always)]
fn apply<I: Isa>(dq: &mut [i16], eob: usize, rows: usize, r: Recon) {
    if eob == 0 {
        return;
    }
    if r.lossless {
        xform::inverse_wht_add(dq, r.dst, r.stride);
    } else if eob == 1 && r.tx_type == 0 {
        xform::dc_add(dq[0] as i32, r.tx_size, r.dst, r.stride);
    } else {
        xform::inverse_add::<I>(dq, r.tx_size, r.tx_type, rows, r.dst, r.stride);
    }
}

#[inline(always)]
fn encode_with<I: Isa>(
    b: Block,
    q: Quant,
    costs: &TokenCosts,
    lambda: u32,
    levels: &mut [i16; 1024],
) -> Out {
    let tx_size = if b.lossless { 0 } else { b.tx_size.min(3) };
    match tx_size {
        0 => encode_n::<I, 4, 16>(b, q, costs, lambda, levels),
        1 => encode_n::<I, 8, 64>(b, q, costs, lambda, levels),
        2 => encode_n::<I, 16, 256>(b, q, costs, lambda, levels),
        _ => encode_n::<I, 32, 1024>(b, q, costs, lambda, levels),
    }
}

#[inline(always)]
fn encode_n<I: Isa, const N: usize, const N2: usize>(
    b: Block,
    q: Quant,
    costs: &TokenCosts,
    lambda: u32,
    levels: &mut [i16; 1024],
) -> Out {
    let lossless = b.lossless;
    let tx_size = N.trailing_zeros() as u8 - 2;
    let tx_type = effective_type(tx_size, b.tx_type, lossless);
    let s = scan_for(tx_size, tx_type);
    let mut co = Buf([0; N2]);
    for i in 0..N {
        let src = &b.src[i * b.src_stride..][..N];
        let pred = &b.dst[i * b.dst_stride..][..N];
        for ((d, &x), &p) in co.0[i * N..][..N].iter_mut().zip(src).zip(pred) {
            *d = x as i16 - p as i16;
        }
    }
    let mut dq = Buf([0; N2]);
    let (eob, rows, rate) = if lossless {
        xform::forward_wht(&mut co.0);
        let mut eob = 0;
        for (c, &pos) in s.scan.iter().enumerate() {
            let v = co.0[s.tpos(pos as usize)];
            levels[c] = v;
            if v != 0 {
                eob = c + 1;
            }
        }
        for (d, &c) in dq.0.iter_mut().zip(&co.0) {
            *d = c.wrapping_mul(4);
        }
        (eob, 4, quant::rate::<N2>(levels, eob, &s, b.ctx, costs))
    } else {
        xform::forward::<I>(&mut co.0, tx_size, tx_type);
        if lambda == 0 {
            let deadzone = if b.is_inter { 14 } else { 21 };
            let (eob, rows) = quant::quantise::<N2>(&co.0, &mut dq.0, &s, q, deadzone, levels);
            (eob, rows, quant::rate::<N2>(levels, eob, &s, b.ctx, costs))
        } else {
            let (eob, rate) = quant::trellis::<N2>(&co.0, &s, q, b.ctx, costs, lambda, levels);
            let rows = quant::dequantise(levels, eob, &s, q, &mut dq.0);
            (eob, rows, rate)
        }
    };
    let r = Recon {
        tx_size,
        tx_type,
        lossless,
        dst: &mut *b.dst,
        stride: b.dst_stride,
    };
    apply::<I>(&mut dq.0, eob, rows, r);
    let mut dist = 0u32;
    for i in 0..N {
        let src = &b.src[i * b.src_stride..][..N];
        let rec = &b.dst[i * b.dst_stride..][..N];
        for (&x, &r) in src.iter().zip(rec) {
            let d = x as i32 - r as i32;
            dist += (d * d) as u32;
        }
    }
    Out {
        eob,
        dist: dist as u64,
        rate,
    }
}
