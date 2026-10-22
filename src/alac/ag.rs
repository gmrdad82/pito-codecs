use super::bits::{Bits, overrun, shl, shr, word};
use crate::media::{Error, Result};

const QBSHIFT: u32 = 9;
const QB: u32 = 1 << QBSHIFT;
const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: u32 = 24;
const MAX_PREFIX_16: u32 = 9;
const MAX_PREFIX_32: u32 = 9;
const MAX_DATATYPE_BITS_16: u32 = 16;
const N_MAX_MEAN_CLAMP: u32 = 0xffff;
const N_MEAN_CLAMP_VAL: u32 = 0xffff;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub mb0: u32,
    pub pb: u32,
    pub kb: u32,
    pub wb: u32,
}

impl Params {
    pub fn new(m: u32, p: u32, k: u32) -> Self {
        Self {
            mb0: m,
            pb: p,
            kb: k,
            wb: shl(1, k).wrapping_sub(1),
        }
    }
}

#[inline(always)]
fn lead(m: u32) -> u32 {
    m.leading_zeros()
}

#[inline(always)]
fn lg3a(x: u32) -> u32 {
    31u32.wrapping_sub(lead(x.wrapping_add(3)))
}

#[inline(always)]
fn getstreambits(data: &[u8], bitoffset: usize, numbits: u32) -> u32 {
    let byteoffset = bitoffset / 8;
    let off = (bitoffset & 7) as u32;
    let load1 = word(data, byteoffset);
    let mut result = if numbits + off > 32 {
        let load2 = super::bits::byte(data, byteoffset + 4);
        let load2shift = 8 - (numbits + off - 32);
        shr(shl(load1, off), 32 - numbits) | shr(load2, load2shift)
    } else {
        shr(load1, 32u32.wrapping_sub(numbits).wrapping_sub(off))
    };
    if numbits != 32 {
        result &= !shl(0xffff_ffff, numbits);
    }
    result
}

#[inline(always)]
fn dyn_get(data: &[u8], pos: &mut usize, m: u32, k: u32) -> u32 {
    let mut bits = *pos;
    let mut streamlong = shl(word(data, bits >> 3), (bits & 7) as u32);
    let pre = lead(!streamlong);
    let result;
    if pre >= MAX_PREFIX_16 {
        bits += MAX_PREFIX_16 as usize;
        streamlong = shl(streamlong, MAX_PREFIX_16);
        result = shr(streamlong, 32 - MAX_DATATYPE_BITS_16);
        bits += MAX_DATATYPE_BITS_16 as usize;
    } else {
        bits += pre as usize + 1;
        streamlong = shl(streamlong, pre + 1);
        let v = shr(streamlong, 32u32.wrapping_sub(k));
        bits = bits.wrapping_add(k as usize);
        let mut r = pre.wrapping_mul(m).wrapping_add(v).wrapping_sub(1);
        if v < 2 {
            r = r.wrapping_sub(v.wrapping_sub(1));
            bits -= 1;
        }
        result = r;
    }
    *pos = bits;
    result
}

#[inline(always)]
fn dyn_get_32bit(data: &[u8], pos: &mut usize, m: u32, k: u32, maxbits: u32) -> u32 {
    let mut bits = *pos;
    let mut streamlong = shl(word(data, bits >> 3), (bits & 7) as u32);
    let mut result = lead(!streamlong);
    if result >= MAX_PREFIX_32 {
        result = getstreambits(data, bits + MAX_PREFIX_32 as usize, maxbits);
        bits += (MAX_PREFIX_32 + maxbits) as usize;
    } else {
        bits += result as usize + 1;
        if k != 1 {
            streamlong = shl(streamlong, result + 1);
            let v = shr(streamlong, 32u32.wrapping_sub(k));
            bits = bits.wrapping_add(k as usize).wrapping_sub(1);
            result = result.wrapping_mul(m);
            if v >= 2 {
                result = result.wrapping_add(v - 1);
                bits += 1;
            }
        }
    }
    *pos = bits;
    result
}

pub fn dyn_decomp(
    params: &Params,
    bits: &mut Bits,
    pc: &mut [i32],
    num: usize,
    max_size: u32,
    apple: bool,
) -> Result<()> {
    let data = bits.data;
    let max_pos = bits.end();
    let mut pos = bits.pos;
    let pb = params.pb;
    let kb = params.kb;
    let wb = params.wb;
    let pc = pc.get_mut(..num).ok_or_else(overrun)?;
    let mut mb = params.mb0;
    let mut zmode = 0u32;
    let mut c = 0usize;
    let mut status = Ok(());
    while c < num {
        if pos >= max_pos {
            status = Err(overrun());
            break;
        }
        let k = lg3a(mb >> QBSHIFT).min(kb);
        let m = shl(1, k).wrapping_sub(1);
        let n = dyn_get_32bit(data, &mut pos, m, k, max_size);
        let ndecode = n.wrapping_add(zmode);
        let multiplier = (ndecode & 1).wrapping_neg() | 1;
        pc[c] = (ndecode.wrapping_add(1) >> 1).wrapping_mul(multiplier) as i32;
        c += 1;
        mb = pb
            .wrapping_mul(n.wrapping_add(zmode))
            .wrapping_add(mb)
            .wrapping_sub(pb.wrapping_mul(mb) >> QBSHIFT);
        if (if apple { n } else { ndecode }) > N_MAX_MEAN_CLAMP {
            mb = N_MEAN_CLAMP_VAL;
        }
        zmode = 0;
        if shl(mb, MMULSHIFT) < QB && c < num {
            zmode = 1;
            let k = lead(mb)
                .wrapping_sub(BITOFF)
                .wrapping_add(mb.wrapping_add(MOFF) >> MDENSHIFT);
            let mz = shl(1, k).wrapping_sub(1) & wb;
            let n = dyn_get(data, &mut pos, mz, k) as usize;
            let Some(zeros) = pc.get_mut(c..c.saturating_add(n)) else {
                status = Err(Error::Invalid("ALAC zero run past the frame".into()));
                break;
            };
            zeros.fill(0);
            c += n;
            if n >= 65535 {
                zmode = 0;
            }
            mb = 0;
        }
    }
    bits.pos = pos;
    status?;
    bits.check()
}
