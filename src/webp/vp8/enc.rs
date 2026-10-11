use super::bits::BoolEncoder;
use super::dec::{Quant, UV_OFF, Work, Y_OFF, quant};
use super::dsp::{self, B_PRED, BPS};
use super::tables::*;
use super::tokens::{self, Counter, Counts, Writer, bit_cost, block_cost};
use crate::media::{Error, Result};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Params {
    pub quality: f32,
    pub effort: u8,
    pub exact: bool,
}

pub struct Planes {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub ys: usize,
    pub uvs: usize,
    pub mb_w: usize,
    pub mb_h: usize,
}

impl Planes {
    fn new(mb_w: usize, mb_h: usize) -> Self {
        Planes {
            y: vec![0; mb_w * 16 * mb_h * 16],
            u: vec![0; mb_w * 8 * mb_h * 8],
            v: vec![0; mb_w * 8 * mb_h * 8],
            ys: mb_w * 16,
            uvs: mb_w * 8,
            mb_w,
            mb_h,
        }
    }
}

#[inline(always)]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    ((16839 * r + 33059 * g + 6420 * b + (16 << 16) + (1 << 15)) >> 16) as u8
}

#[inline(always)]
fn clip_uv(v: i32) -> u8 {
    let v = (v + (1 << 17) + (128 << 18)) >> 18;
    v.clamp(0, 255) as u8
}

pub fn rgba_to_yuv(rgba: &[u8], width: usize, height: usize, exact: bool) -> Planes {
    use rayon::prelude::*;
    let mb_w = width.div_ceil(16);
    let mb_h = height.div_ceil(16);
    let mut p = Planes::new(mb_w, mb_h);
    let (ys, uvs) = (p.ys, p.uvs);
    let px = |x: usize, y: usize| {
        let o = (y.min(height - 1) * width + x.min(width - 1)) * 4;
        &rgba[o..o + 4]
    };
    p.y.par_chunks_mut(ys * 16)
        .zip(p.u.par_chunks_mut(uvs * 8))
        .zip(p.v.par_chunks_mut(uvs * 8))
        .enumerate()
        .for_each(|(my, ((yb, ub), vb))| {
            for r in 0..16 {
                let y = my * 16 + r;
                for x in 0..ys {
                    let c = px(x, y);
                    yb[r * ys + x] = rgb_to_y(c[0] as i32, c[1] as i32, c[2] as i32);
                }
            }
            for r in 0..8 {
                let y = my * 16 + 2 * r;
                for x in 0..uvs {
                    let (mut sr, mut sg, mut sb) = (0, 0, 0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let c = px(2 * x + dx, y + dy);
                        sr += c[0] as i32;
                        sg += c[1] as i32;
                        sb += c[2] as i32;
                    }
                    ub[r * uvs + x] = clip_uv(-9719 * sr - 19081 * sg + 28800 * sb);
                    vb[r * uvs + x] = clip_uv(28800 * sr - 24116 * sg - 4684 * sb);
                }
            }
            if !exact {
                for mx in 0..mb_w {
                    let clear =
                        (0..16).all(|r| (0..16).all(|c| px(mx * 16 + c, my * 16 + r)[3] == 0));
                    if clear {
                        for r in 0..16 {
                            yb[r * ys + mx * 16..r * ys + mx * 16 + 16].fill(128);
                        }
                        for r in 0..8 {
                            ub[r * uvs + mx * 8..r * uvs + mx * 8 + 8].fill(128);
                            vb[r * uvs + mx * 8..r * uvs + mx * 8 + 8].fill(128);
                        }
                    }
                }
            }
        });
    p
}

pub fn quality_to_q(quality: f32) -> i32 {
    let c = (quality as f64 / 100.0).clamp(0.0, 1.0);
    let linear = if c < 0.75 {
        c * (2.0 / 3.0)
    } else {
        2.0 * c - 1.0
    };
    let v = linear.powf(1.0 / 3.0);
    ((127.0 * (1.0 - v)) as i32).clamp(0, 127)
}

#[derive(Clone, Copy)]
pub struct Matrix {
    q: [i32; 2],
    iq: [u32; 2],
    bias: [u32; 2],
    max: i32,
}

impl Matrix {
    pub fn new(q: [i32; 2], bias: [u32; 2]) -> Self {
        let iq = [
            (1u32 << 16).div_ceil(q[0] as u32),
            (1u32 << 16).div_ceil(q[1] as u32),
        ];
        Matrix {
            q,
            iq,
            bias: [(bias[0] * q[0] as u32) >> 8, (bias[1] * q[1] as u32) >> 8],
            max: (32767 / q[0].max(q[1])).min(2047),
        }
    }

    #[inline(always)]
    fn quantize(&self, c: &[i16; 16], first: usize, out: &mut [i16; 16]) -> bool {
        quantize(self, c, first, out)
    }

    #[inline(always)]
    fn dequant(&self, l: &[i16; 16], out: &mut [i16; 16]) {
        for i in 0..16 {
            out[i] = (l[i] as i32 * self.q[(i > 0) as usize]) as i16;
        }
    }
}

#[cfg(any(test, not(target_arch = "x86_64")))]
pub fn quantize_scalar(m: &Matrix, c: &[i16; 16], first: usize, out: &mut [i16; 16]) -> bool {
    let mut nz = false;
    for i in 0..16 {
        let k = (i > 0) as usize;
        let v = c[i] as i32;
        let a = v.unsigned_abs();
        let l = if i < first {
            0
        } else {
            (((a + m.bias[k]) * m.iq[k]) >> 16) as i32
        };
        let l = l.min(m.max);
        out[i] = if v < 0 { -l as i16 } else { l as i16 };
        nz |= l != 0;
    }
    nz
}

#[cfg(not(target_arch = "x86_64"))]
use quantize_scalar as quantize;

#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn quantize(m: &Matrix, c: &[i16; 16], first: usize, out: &mut [i16; 16]) -> bool {
    use std::arch::x86_64::*;
    unsafe {
        let p = c.as_ptr();
        let lo_mask = _mm_setr_epi16(-1, 0, 0, 0, 0, 0, 0, 0);
        let pick = |dc: u32, ac: u32| {
            let d = _mm_set1_epi16(dc as i16);
            let a = _mm_set1_epi16(ac as i16);
            _mm_or_si128(_mm_and_si128(lo_mask, d), _mm_andnot_si128(lo_mask, a))
        };
        let bias0 = pick(m.bias[0], m.bias[1]);
        let iq0 = pick(m.iq[0], m.iq[1]);
        let bias1 = _mm_set1_epi16(m.bias[1] as i16);
        let iq1 = _mm_set1_epi16(m.iq[1] as i16);
        let max = _mm_set1_epi16(m.max as i16);
        let one = |x: __m128i, b: __m128i, q: __m128i| {
            let sign = _mm_srai_epi16(x, 15);
            let a = _mm_sub_epi16(_mm_xor_si128(x, sign), sign);
            let l = _mm_mulhi_epu16(_mm_add_epi16(a, b), q);
            let l = _mm_min_epi16(l, max);
            _mm_sub_epi16(_mm_xor_si128(l, sign), sign)
        };
        let mut l0 = one(_mm_loadu_si128(p.cast()), bias0, iq0);
        let l1 = one(_mm_loadu_si128(p.add(8).cast()), bias1, iq1);
        if first > 0 {
            l0 = _mm_andnot_si128(lo_mask, l0);
        }
        let o = out.as_mut_ptr();
        _mm_storeu_si128(o.cast(), l0);
        _mm_storeu_si128(o.add(8).cast(), l1);
        _mm_movemask_epi8(_mm_cmpeq_epi8(_mm_packs_epi16(l0, l1), _mm_setzero_si128())) != 0xffff
    }
}

#[derive(Clone, Copy)]
struct Seg {
    y1: Matrix,
    y2: Matrix,
    uv: Matrix,
    lambda: u64,
    lambda_uv: u64,
}

impl Seg {
    fn new(q: &Quant) -> Self {
        let ac = q.y1[1] as u64;
        Seg {
            y1: Matrix::new(q.y1, [96, 110]),
            y2: Matrix::new(q.y2, [96, 108]),
            uv: Matrix::new(q.uv, [110, 115]),
            lambda: (ac * ac * 8).max(64),
            lambda_uv: (q.uv[1] as u64 * q.uv[1] as u64 * 8).max(64),
        }
    }
}

#[derive(Clone, Default)]
pub struct EncMb {
    pub ymode: u8,
    pub uvmode: u8,
    pub segment: u8,
    pub bmodes: [u8; 16],
    pub levels: [[i16; 16]; 25],
    pub top_out: u32,
    pub left_out: u32,
    pub coded: bool,
}

struct Costs {
    ymode: [u32; 5],
    uvmode: [u32; 4],
    bmode: Box<[[[u32; 10]; 10]; 10]>,
}

fn tree_cost(tree: &[i8], probs: &[u8], value: u8) -> u32 {
    fn walk(tree: &[i8], probs: &[u8], i: usize, value: u8, acc: u32) -> Option<u32> {
        for b in 0..2 {
            let next = tree[i + b];
            let c = acc + bit_cost(b == 1, probs[i >> 1]);
            if next <= 0 {
                if (-next) as u8 == value {
                    return Some(c);
                }
            } else if let Some(r) = walk(tree, probs, next as usize, value, c) {
                return Some(r);
            }
        }
        None
    }
    walk(tree, probs, 0, value, 0).unwrap_or(0)
}

impl Costs {
    fn new() -> Self {
        let mut bmode = Box::new([[[0u32; 10]; 10]; 10]);
        for a in 0..10 {
            for l in 0..10 {
                for m in 0..10 {
                    bmode[a][l][m] = tree_cost(&BMODE_TREE, &KF_BMODE_PROBS[a][l], m as u8);
                }
            }
        }
        Costs {
            ymode: std::array::from_fn(|m| tree_cost(&KF_YMODE_TREE, &KF_YMODE_PROBS, m as u8)),
            uvmode: std::array::from_fn(|m| tree_cost(&UV_MODE_TREE, &KF_UV_MODE_PROBS, m as u8)),
            bmode,
        }
    }
}

struct Raw<T>(*mut T, usize);

unsafe impl<T> Send for Raw<T> {}
unsafe impl<T> Sync for Raw<T> {}

impl<T> Raw<T> {
    fn new(v: &mut [T]) -> Self {
        Raw(v.as_mut_ptr(), v.len())
    }

    #[inline(always)]
    unsafe fn at(&self, i: usize) -> *mut T {
        assert!(i < self.1);
        unsafe { self.0.add(i) }
    }
}

struct Ctx<'a> {
    src: &'a Planes,
    rec_y: Raw<u8>,
    rec_u: Raw<u8>,
    rec_v: Raw<u8>,
    mbs: Raw<EncMb>,
    segs: &'a [Seg; 4],
    seg_map: &'a [u8],
    probs: &'a super::dec::Probs,
    costs: &'a Costs,
    effort: u8,
}

struct Scratch {
    w: Work,
    src_y: [u8; BPS * 16],
    src_u: [u8; BPS * 8],
    src_v: [u8; BPS * 8],
    best: [u8; BPS * 17],
}

const SRC_Y: usize = 0;

impl Ctx<'_> {
    unsafe fn rec(&self, plane: usize, i: usize) -> u8 {
        unsafe {
            match plane {
                0 => *self.rec_y.at(i),
                1 => *self.rec_u.at(i),
                _ => *self.rec_v.at(i),
            }
        }
    }

    unsafe fn fill(&self, s: &mut Scratch, mx: usize, my: usize) {
        let w = &mut s.w;
        let (ys, uvs) = (self.src.ys, self.src.uvs);
        let last = mx + 1 == self.src.mb_w;
        unsafe {
            if my == 0 {
                w.y[Y_OFF - BPS - 1..Y_OFF - BPS + 20].fill(127);
                w.u[UV_OFF - BPS - 1..UV_OFF - BPS + 8].fill(127);
                w.v[UV_OFF - BPS - 1..UV_OFF - BPS + 8].fill(127);
            } else {
                let row = (my * 16 - 1) * ys;
                w.y[Y_OFF - BPS - 1] = if mx == 0 {
                    129
                } else {
                    self.rec(0, row + mx * 16 - 1)
                };
                for i in 0..16 {
                    w.y[Y_OFF - BPS + i] = self.rec(0, row + mx * 16 + i);
                }
                for i in 16..20 {
                    w.y[Y_OFF - BPS + i] = if last {
                        self.rec(0, row + mx * 16 + 15)
                    } else {
                        self.rec(0, row + mx * 16 + i)
                    };
                }
                let row = (my * 8 - 1) * uvs;
                for (p, buf) in [(1, &mut w.u), (2, &mut w.v)] {
                    buf[UV_OFF - BPS - 1] = if mx == 0 {
                        129
                    } else {
                        self.rec(p, row + mx * 8 - 1)
                    };
                    for i in 0..8 {
                        buf[UV_OFF - BPS + i] = self.rec(p, row + mx * 8 + i);
                    }
                }
            }
            for r in 0..16 {
                w.y[Y_OFF + r * BPS - 1] = if mx == 0 {
                    129
                } else {
                    self.rec(0, (my * 16 + r) * ys + mx * 16 - 1)
                };
            }
            for r in 0..8 {
                for (p, buf) in [(1, &mut w.u), (2, &mut w.v)] {
                    buf[UV_OFF + r * BPS - 1] = if mx == 0 {
                        129
                    } else {
                        self.rec(p, (my * 8 + r) * uvs + mx * 8 - 1)
                    };
                }
            }
        }
        for r in 0..16 {
            let o = (my * 16 + r) * ys + mx * 16;
            s.src_y[SRC_Y + r * BPS..SRC_Y + r * BPS + 16].copy_from_slice(&self.src.y[o..o + 16]);
        }
        for r in 0..8 {
            let o = (my * 8 + r) * uvs + mx * 8;
            s.src_u[r * BPS..r * BPS + 8].copy_from_slice(&self.src.u[o..o + 8]);
            s.src_v[r * BPS..r * BPS + 8].copy_from_slice(&self.src.v[o..o + 8]);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn i16(
        &self,
        s: &mut Scratch,
        seg: &Seg,
        mode: u8,
        top: u32,
        left: u32,
        my: usize,
        mx: usize,
        out: &mut EncMb,
    ) -> (u64, u32, u32) {
        let w = &mut s.w;
        dsp::pred_luma(mode, &mut w.y, Y_OFF, my > 0, mx > 0);
        let mut coefs = [[0i16; 16]; 16];
        for (b, c) in coefs.iter_mut().enumerate() {
            let off = (b >> 2) * 4 * BPS + (b & 3) * 4;
            dsp::fdct(&s.src_y, SRC_Y + off, &w.y, Y_OFF + off, c);
        }
        let mut dcs = [0i16; 16 * 16];
        for b in 0..16 {
            dcs[b * 16] = coefs[b][0];
        }
        let mut y2 = [0i16; 16];
        dsp::fwht(&dcs, &mut y2);
        let lv = &mut out.levels;
        seg.y2.quantize(&y2, 0, &mut lv[24]);
        let mut rate = 0u32;
        let mut t = top;
        let mut l = left;
        let ctx = (((t >> 8) & 1) + ((l >> 8) & 1)) as usize;
        let y2nz = tokens::last_nonzero(&lv[24], 0).is_some() as u32;
        rate += block_cost(&self.probs[1], &lv[24], 0, ctx);
        t = (t & !0x100) | y2nz << 8;
        l = (l & !0x100) | y2nz << 8;
        let mut dq = [0i16; 16];
        seg.y2.dequant(&lv[24], &mut dq);
        let mut rec = [0i16; 256];
        dsp::iwht(&dq, &mut rec);
        for by in 0..4 {
            let mut lb = (l >> by) & 1;
            for bx in 0..4 {
                let b = by * 4 + bx;
                seg.y1.quantize(&coefs[b], 1, &mut lv[b]);
                let ctx = (((t >> bx) & 1) + lb) as usize;
                rate += block_cost(&self.probs[0], &lv[b], 1, ctx);
                lb = tokens::last_nonzero(&lv[b], 1).is_some() as u32;
                t = (t & !(1 << bx)) | lb << bx;
                let dc = rec[b * 16];
                seg.y1.dequant(&lv[b], &mut dq);
                dq[0] = dc;
                let off = Y_OFF + (b >> 2) * 4 * BPS + (b & 3) * 4;
                dsp::idct_add(&dq, &mut w.y, off);
            }
            l = (l & !(1 << by)) | lb << by;
        }
        let d = dsp::sse16(&s.src_y, SRC_Y, &w.y, Y_OFF);
        rate += self.costs.ymode[mode as usize];
        ((d << 8) + seg.lambda * rate as u64 / 256, t, l)
    }

    #[allow(clippy::too_many_arguments)]
    fn i4(
        &self,
        s: &mut Scratch,
        seg: &Seg,
        top: u32,
        left: u32,
        tb: [u8; 4],
        lb: [u8; 4],
        out: &mut EncMb,
        limit: u64,
    ) -> Option<(u64, u32, u32)> {
        let w = &mut s.w;
        let t0 = Y_OFF - BPS + 16;
        let ar = [w.y[t0], w.y[t0 + 1], w.y[t0 + 2], w.y[t0 + 3]];
        for r in [3, 7, 11] {
            w.y[Y_OFF + r * BPS + 16..Y_OFF + r * BPS + 20].copy_from_slice(&ar);
        }
        let k = match self.effort {
            0..=1 => 1,
            2..=3 => 2,
            4 => 3,
            5 => 5,
            _ => 10,
        };
        let mut t = top;
        let mut l = left;
        let mut tbm = tb;
        let mut lbm = lb;
        let mut score = (self.costs.ymode[B_PRED as usize] as u64 * seg.lambda) / 256;
        let mut tmp = [0u8; 4 * BPS + 4];
        for b in 0..16 {
            let (bx, by) = (b & 3, b >> 2);
            let off = Y_OFF + by * 4 * BPS + bx * 4;
            let so = SRC_Y + by * 4 * BPS + bx * 4;
            let a = tbm[bx] as usize;
            let lm = lbm[by] as usize;
            let mut cand = [(u64::MAX, 0u8); 10];
            for (m, c) in cand.iter_mut().enumerate() {
                dsp::pred4(m as u8, &mut w.y, off);
                let d = dsp::sse4(&s.src_y, so, &w.y, off);
                *c = (
                    (d << 8) + seg.lambda * self.costs.bmode[a][lm][m] as u64 / 256,
                    m as u8,
                );
            }
            if k < 10 {
                cand.sort_unstable_by_key(|c| c.0);
            }
            let ctx = (((t >> bx) & 1) + ((l >> by) & 1)) as usize;
            let mut best = (u64::MAX, 0u8, [0i16; 16]);
            for &(_, m) in &cand[..k] {
                dsp::pred4(m, &mut w.y, off);
                let mut c = [0i16; 16];
                dsp::fdct(&s.src_y, so, &w.y, off, &mut c);
                let mut lv = [0i16; 16];
                seg.y1.quantize(&c, 0, &mut lv);
                let rate =
                    block_cost(&self.probs[3], &lv, 0, ctx) + self.costs.bmode[a][lm][m as usize];
                let mut dq = [0i16; 16];
                seg.y1.dequant(&lv, &mut dq);
                for r in 0..4 {
                    tmp[r * BPS..r * BPS + 4]
                        .copy_from_slice(&w.y[off + r * BPS..off + r * BPS + 4]);
                }
                dsp::idct_add(&dq, &mut tmp, 0);
                let d = dsp::sse4(&s.src_y, so, &tmp, 0);
                let sc = (d << 8) + seg.lambda * rate as u64 / 256;
                if sc < best.0 {
                    best = (sc, m, lv);
                }
            }
            let (sc, m, lv) = best;
            score += sc;
            if score > limit {
                return None;
            }
            dsp::pred4(m, &mut w.y, off);
            let mut dq = [0i16; 16];
            seg.y1.dequant(&lv, &mut dq);
            dsp::idct_add(&dq, &mut w.y, off);
            let nz = tokens::last_nonzero(&lv, 0).is_some() as u32;
            t = (t & !(1 << bx)) | nz << bx;
            l = (l & !(1 << by)) | nz << by;
            tbm[bx] = m;
            lbm[by] = m;
            out.bmodes[b] = m;
            out.levels[b] = lv;
        }
        Some((score, t, l))
    }

    #[allow(clippy::too_many_arguments)]
    fn chroma(
        &self,
        s: &mut Scratch,
        seg: &Seg,
        t: u32,
        l: u32,
        my: usize,
        mx: usize,
        out: &mut EncMb,
    ) -> (u32, u32) {
        let w = &mut s.w;
        let mut best = (u64::MAX, 0u8);
        for m in 0..4u8 {
            dsp::pred_chroma(m, &mut w.u, UV_OFF, my > 0, mx > 0);
            dsp::pred_chroma(m, &mut w.v, UV_OFF, my > 0, mx > 0);
            let d = dsp::sse8(&s.src_u, 0, &w.u, UV_OFF) + dsp::sse8(&s.src_v, 0, &w.v, UV_OFF);
            let sc = (d << 8) + seg.lambda_uv * self.costs.uvmode[m as usize] as u64 / 256;
            if sc < best.0 {
                best = (sc, m);
            }
        }
        let m = best.1;
        out.uvmode = m;
        dsp::pred_chroma(m, &mut w.u, UV_OFF, my > 0, mx > 0);
        dsp::pred_chroma(m, &mut w.v, UV_OFF, my > 0, mx > 0);
        let mut t = t;
        let mut l = l;
        for (k0, shift) in [(16usize, 4u32), (20, 6)] {
            let (src, buf) = if k0 == 16 {
                (&s.src_u, &mut w.u)
            } else {
                (&s.src_v, &mut w.v)
            };
            for by in 0..2 {
                for bx in 0..2 {
                    let k = k0 + by * 2 + bx;
                    let off = (by * 4 * BPS) + bx * 4;
                    let mut c = [0i16; 16];
                    dsp::fdct(src, off, buf, UV_OFF + off, &mut c);
                    seg.uv.quantize(&c, 0, &mut out.levels[k]);
                    let mut dq = [0i16; 16];
                    seg.uv.dequant(&out.levels[k], &mut dq);
                    dsp::idct_add(&dq, buf, UV_OFF + off);
                    let nz = tokens::last_nonzero(&out.levels[k], 0).is_some() as u32;
                    t = (t & !(1 << (shift + bx as u32))) | nz << (shift + bx as u32);
                    l = (l & !(1 << (shift + by as u32))) | nz << (shift + by as u32);
                }
            }
        }
        (t, l)
    }

    unsafe fn mb(&self, s: &mut Scratch, mx: usize, my: usize) {
        let mb_w = self.src.mb_w;
        unsafe { self.fill(s, mx, my) };
        let (top, tb) = if my > 0 {
            let a = unsafe { &*self.mbs.at((my - 1) * mb_w + mx) };
            (
                a.top_out,
                [a.bmodes[12], a.bmodes[13], a.bmodes[14], a.bmodes[15]],
            )
        } else {
            (0, [0; 4])
        };
        let (left, lb) = if mx > 0 {
            let a = unsafe { &*self.mbs.at(my * mb_w + mx - 1) };
            (
                a.left_out,
                [a.bmodes[3], a.bmodes[7], a.bmodes[11], a.bmodes[15]],
            )
        } else {
            (0, [0; 4])
        };
        let segment = self.seg_map[my * mb_w + mx];
        let seg = &self.segs[segment as usize];
        let mut out = EncMb {
            segment,
            ..EncMb::default()
        };
        let modes: &[u8] = if self.effort >= 4 {
            &[0, 1, 2, 3]
        } else {
            let mut best = (u64::MAX, 0u8);
            for m in 0..4u8 {
                dsp::pred_luma(m, &mut s.w.y, Y_OFF, my > 0, mx > 0);
                let d = dsp::sse16(&s.src_y, SRC_Y, &s.w.y, Y_OFF);
                let sc = (d << 8) + seg.lambda * self.costs.ymode[m as usize] as u64 / 256;
                if sc < best.0 {
                    best = (sc, m);
                }
            }
            &[[0u8], [1], [2], [3]][best.1 as usize]
        };
        let mut best16 = (u64::MAX, 0u8, 0u32, 0u32);
        let mut lv16 = [[0i16; 16]; 25];
        for &m in modes {
            let mut o = EncMb::default();
            let (sc, t, l) = self.i16(s, seg, m, top, left, my, mx, &mut o);
            if sc < best16.0 {
                best16 = (sc, m, t, l);
                lv16 = o.levels;
                for r in 0..16 {
                    s.best[r * BPS..r * BPS + 16]
                        .copy_from_slice(&s.w.y[Y_OFF + r * BPS..Y_OFF + r * BPS + 16]);
                }
            }
        }
        let i4 = if self.effort >= 1 {
            self.i4(s, seg, top, left, tb, lb, &mut out, best16.0)
        } else {
            None
        };
        let (mut t, mut l) = match i4 {
            Some((sc, t, l)) if sc < best16.0 => {
                out.ymode = B_PRED;
                (t | (top & 0x100), (l & !0x100) | (left & 0x100))
            }
            _ => {
                out.ymode = best16.1;
                out.levels = lv16;
                let b = [dsp::B_DC, dsp::B_VE, dsp::B_HE, dsp::B_TM][best16.1 as usize];
                out.bmodes = [b; 16];
                for r in 0..16 {
                    s.w.y[Y_OFF + r * BPS..Y_OFF + r * BPS + 16]
                        .copy_from_slice(&s.best[r * BPS..r * BPS + 16]);
                }
                (best16.2, best16.3)
            }
        };
        if out.ymode == B_PRED {
            t = (t & !0x100) | (top & 0x100);
            l = (l & !0x100) | (left & 0x100);
        }
        let (t, l) = self.chroma(s, seg, t, l, my, mx, &mut out);
        out.top_out = t;
        out.left_out = l;
        let first = if out.ymode == B_PRED { 0 } else { 1 };
        out.coded = (0..16).any(|b| tokens::last_nonzero(&out.levels[b], first).is_some())
            || (16..24).any(|b| tokens::last_nonzero(&out.levels[b], 0).is_some())
            || (out.ymode != B_PRED && tokens::last_nonzero(&out.levels[24], 0).is_some());
        let (ys, uvs) = (self.src.ys, self.src.uvs);
        unsafe {
            for r in 0..16 {
                let o = (my * 16 + r) * ys + mx * 16;
                std::ptr::copy_nonoverlapping(
                    s.w.y[Y_OFF + r * BPS..].as_ptr(),
                    self.rec_y.at(o),
                    16,
                );
            }
            for r in 0..8 {
                let o = (my * 8 + r) * uvs + mx * 8;
                std::ptr::copy_nonoverlapping(
                    s.w.u[UV_OFF + r * BPS..].as_ptr(),
                    self.rec_u.at(o),
                    8,
                );
                std::ptr::copy_nonoverlapping(
                    s.w.v[UV_OFF + r * BPS..].as_ptr(),
                    self.rec_v.at(o),
                    8,
                );
            }
            *self.mbs.at(my * mb_w + mx) = out;
        }
    }
}

fn analyse(ctx: &Ctx, threads: usize) {
    let (mb_w, mb_h) = (ctx.src.mb_w, ctx.src.mb_h);
    let progress: Vec<AtomicUsize> = (0..mb_h).map(|_| AtomicUsize::new(0)).collect();
    let next = AtomicUsize::new(0);
    let work = || {
        let mut s = Scratch {
            w: Work::new(),
            src_y: [0; BPS * 16],
            src_u: [0; BPS * 8],
            src_v: [0; BPS * 8],
            best: [0; BPS * 17],
        };
        loop {
            let my = next.fetch_add(1, Ordering::Relaxed);
            if my >= mb_h {
                break;
            }
            for mx in 0..mb_w {
                if my > 0 {
                    let need = (mx + 2).min(mb_w);
                    let mut spins = 0u32;
                    while progress[my - 1].load(Ordering::Acquire) < need {
                        spins += 1;
                        if spins < 64 {
                            std::hint::spin_loop();
                        } else {
                            std::thread::yield_now();
                        }
                    }
                }
                unsafe { ctx.mb(&mut s, mx, my) };
                progress[my].store(mx + 1, Ordering::Release);
            }
        }
    };
    if threads <= 1 {
        work();
    } else {
        rayon::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|_| work());
            }
        });
    }
}

fn modes_header(
    bw: &mut BoolEncoder,
    mbs: &[EncMb],
    mb_w: usize,
    seg_probs: Option<[u8; 3]>,
    skip_prob: Option<u8>,
) {
    let mut intra_t = vec![0u8; mb_w * 4];
    for row in mbs.chunks(mb_w) {
        let mut intra_l = [0u8; 4];
        for (mx, mb) in row.iter().enumerate() {
            if let Some(p) = seg_probs {
                bw.tree(&SEGMENT_TREE, &p, mb.segment);
            }
            if let Some(p) = skip_prob {
                bw.put(!mb.coded, p);
            }
            bw.tree(&KF_YMODE_TREE, &KF_YMODE_PROBS, mb.ymode);
            let top = &mut intra_t[mx * 4..mx * 4 + 4];
            if mb.ymode == B_PRED {
                for (i, &m) in mb.bmodes.iter().enumerate() {
                    let a = top[i & 3] as usize;
                    let l = intra_l[i >> 2] as usize;
                    bw.tree(&BMODE_TREE, &KF_BMODE_PROBS[a][l], m);
                    top[i & 3] = m;
                    intra_l[i >> 2] = m;
                }
            } else {
                let b = mb.bmodes[0];
                top.fill(b);
                intra_l.fill(b);
            }
            bw.tree(&UV_MODE_TREE, &KF_UV_MODE_PROBS, mb.uvmode);
        }
    }
}

fn mb_tokens<S: tokens::Sink>(sink: &mut S, mb: &EncMb, top: &mut u32, left: &mut u32) {
    let mut t = *top;
    let mut l = *left;
    let first = if mb.ymode != B_PRED {
        let ctx = (((t >> 8) & 1) + ((l >> 8) & 1)) as usize;
        let nz = tokens::block(sink, 1, &mb.levels[24], 0, ctx) as u32;
        t = (t & !0x100) | nz << 8;
        l = (l & !0x100) | nz << 8;
        1
    } else {
        0
    };
    let ty = if first == 1 { 0 } else { 3 };
    for by in 0..4 {
        for bx in 0..4 {
            let ctx = (((t >> bx) & 1) + ((l >> by) & 1)) as usize;
            let nz = tokens::block(sink, ty, &mb.levels[by * 4 + bx], first, ctx) as u32;
            t = (t & !(1 << bx)) | nz << bx;
            l = (l & !(1 << by)) | nz << by;
        }
    }
    for (k0, shift) in [(16usize, 4u32), (20, 6)] {
        for by in 0..2u32 {
            for bx in 0..2u32 {
                let ctx = (((t >> (shift + bx)) & 1) + ((l >> (shift + by)) & 1)) as usize;
                let k = k0 + by as usize * 2 + bx as usize;
                let nz = tokens::block(sink, 2, &mb.levels[k], 0, ctx) as u32;
                t = (t & !(1 << (shift + bx))) | nz << (shift + bx);
                l = (l & !(1 << (shift + by))) | nz << (shift + by);
            }
        }
    }
    *top = t;
    *left = l;
}

fn skip_context(mb: &EncMb, top: &mut u32, left: &mut u32) {
    let keep = if mb.ymode == B_PRED { 0x100 } else { 0 };
    *top &= keep;
    *left &= keep;
}

fn count(mbs: &[EncMb], mb_w: usize, skip: bool) -> Counts {
    use rayon::prelude::*;
    let zero = || Box::new([[[[[0u32; 2]; 11]; 3]; 8]; 4]);
    let total = mbs
        .par_chunks(mb_w)
        .enumerate()
        .fold(zero, |mut counts, (my, row)| {
            let mut sink = Counter {
                counts: &mut counts,
            };
            let mut left = 0u32;
            for (mx, mb) in row.iter().enumerate() {
                let mut top = if my > 0 {
                    mbs[(my - 1) * mb_w + mx].top_out
                } else {
                    0
                };
                if skip && !mb.coded {
                    skip_context(mb, &mut top, &mut left);
                } else {
                    mb_tokens(&mut sink, mb, &mut top, &mut left);
                }
            }
            counts
        })
        .reduce(zero, |mut a, b| {
            let dst = a.as_flattened_mut().as_flattened_mut().as_flattened_mut();
            let src = b.as_flattened().as_flattened().as_flattened();
            for (x, y) in dst.iter_mut().zip(src) {
                x[0] += y[0];
                x[1] += y[1];
            }
            a
        });
    *total
}

fn filter_level(src: &Planes, rec: &Planes, mbs: &[EncMb], q: i32, effort: u8) -> u8 {
    let guess = ((AC_Q[q.clamp(0, 127) as usize] as i32 * 3) / 8 + 2).clamp(0, 63);
    if effort < 3 || rec.mb_h < 2 {
        return guess as u8;
    }
    use rayon::prelude::*;
    let (ys, uvs, mb_w) = (rec.ys, rec.uvs, rec.mb_w);
    let step = if effort >= 5 { 2 } else { 3 };
    let rows: Vec<usize> = (1..rec.mb_h).step_by(step).collect();
    let try_level = |level: i32| -> u64 {
        let mut y = vec![0u8; 20 * ys];
        let mut u = vec![0u8; 12 * uvs];
        let mut v = vec![0u8; 12 * uvs];
        let mut total = 0u64;
        for &my in &rows {
            let (yr, cr) = (my * 16 - 4, my * 8 - 4);
            y.copy_from_slice(&rec.y[yr * ys..(my * 16 + 16) * ys]);
            u.copy_from_slice(&rec.u[cr * uvs..(my * 8 + 8) * uvs]);
            v.copy_from_slice(&rec.v[cr * uvs..(my * 8 + 8) * uvs]);
            let mut p = super::filter::Planes {
                y: &mut y,
                u: &mut u,
                v: &mut v,
                ys,
                uvs,
                yr,
                cr,
            };
            for mx in 0..mb_w {
                let mb = &mbs[my * mb_w + mx];
                let inner = mb.ymode == B_PRED || mb.coded;
                super::filter::filter_mb_fast(
                    &mut p,
                    mx,
                    my,
                    super::filter::strength(level, 0, inner),
                    false,
                );
            }
            let d = |a: &[u8], b: &[u8]| {
                a.iter()
                    .zip(b)
                    .map(|(x, y)| ((*x as i32 - *y as i32).pow(2)) as u64)
                    .sum::<u64>()
            };
            total += d(&src.y[yr * ys..(my * 16 + 16) * ys], &y)
                + d(&src.u[cr * uvs..(my * 8 + 8) * uvs], &u)
                + d(&src.v[cr * uvs..(my * 8 + 8) * uvs], &v);
        }
        total
    };
    let cands: Vec<i32> = if effort >= 5 {
        (0..=8).map(|k| (guess * k / 4).min(63)).collect()
    } else {
        (0..=4).map(|k| (guess * k / 2).min(63)).collect()
    };
    let scores: Vec<u64> = cands.par_iter().map(|&l| try_level(l)).collect();
    let (i, _) = scores.iter().enumerate().min_by_key(|(_, s)| **s).unwrap();
    cands[i] as u8
}

pub fn encode(rgba: &[u8], width: usize, height: usize, params: &Params) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 16383 || height > 16383 {
        return Err(Error::Invalid("webp: VP8 frame size".into()));
    }
    if rgba.len() < width * height * 4 {
        return Err(Error::Invalid("webp: RGBA buffer too small".into()));
    }
    let src = rgba_to_yuv(rgba, width, height, params.exact);
    let (mb_w, mb_h) = (src.mb_w, src.mb_h);
    let q = quality_to_q(params.quality);
    let dq = [0i32; 5];
    let segs = [Seg::new(&quant(q, &dq)); 4];
    let seg_map = vec![0u8; mb_w * mb_h];
    let probs = super::dec::expand(&COEFF_PROBS);
    let costs = Costs::new();
    let mut rec = Planes::new(mb_w, mb_h);
    let mut mbs = vec![EncMb::default(); mb_w * mb_h];
    let threads = if mb_w * mb_h < 64 {
        1
    } else {
        rayon::current_num_threads().min(mb_h)
    };
    {
        let ctx = Ctx {
            src: &src,
            rec_y: Raw::new(&mut rec.y),
            rec_u: Raw::new(&mut rec.u),
            rec_v: Raw::new(&mut rec.v),
            mbs: Raw::new(&mut mbs),
            segs: &segs,
            seg_map: &seg_map,
            probs: &probs,
            costs: &costs,
            effort: params.effort,
        };
        analyse(&ctx, threads);
    }
    let level = filter_level(&src, &rec, &mbs, q, params.effort);
    let skippable = mbs.iter().filter(|m| !m.coded).count();
    let skip_prob = (skippable > 0).then(|| {
        let p = (255 * (mbs.len() - skippable) / mbs.len()).clamp(1, 254);
        p as u8
    });
    let counts = count(&mbs, mb_w, skip_prob.is_some());
    let (coeff, update) = tokens::choose_probs(&counts, &COEFF_PROBS);
    let wprobs = tokens::expand(&coeff);

    let nparts: usize = if mb_h >= 32 && mb_w * mb_h >= 2048 {
        8
    } else if mb_h >= 8 && mb_w * mb_h >= 512 {
        4
    } else {
        1
    };
    let mut first = BoolEncoder::with_capacity(mb_w * mb_h * 4 + 1024);
    first.flag(false);
    first.flag(false);
    first.flag(false);
    first.flag(false);
    first.literal(level as u32, 6);
    first.literal(0, 3);
    first.flag(false);
    first.literal(nparts.trailing_zeros(), 2);
    first.literal(q as u32, 7);
    for _ in 0..5 {
        first.optional_signed(0, 4);
    }
    first.flag(true);
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for i in 0..11 {
                    let u = update[t][b][c][i];
                    first.put(u, COEFF_UPDATE_PROBS[t][b][c][i]);
                    if u {
                        first.literal(coeff[t][b][c][i] as u32, 8);
                    }
                }
            }
        }
    }
    first.flag(skip_prob.is_some());
    if let Some(p) = skip_prob {
        first.literal(p as u32, 8);
    }
    modes_header(&mut first, &mbs, mb_w, None, skip_prob);
    let first = first.finish();
    if first.len() >= 1 << 19 {
        return Err(Error::Unsupported(
            "webp: VP8 first partition too large".into(),
        ));
    }

    use rayon::prelude::*;
    let parts: Vec<Vec<u8>> = (0..nparts)
        .into_par_iter()
        .map(|p| {
            let mut bw = BoolEncoder::with_capacity(rgba.len() / (8 * nparts) + 64);
            for my in (p..mb_h).step_by(nparts) {
                let mut left = 0u32;
                for mx in 0..mb_w {
                    let mb = &mbs[my * mb_w + mx];
                    let mut top = if my > 0 {
                        mbs[(my - 1) * mb_w + mx].top_out
                    } else {
                        0
                    };
                    if skip_prob.is_some() && !mb.coded {
                        skip_context(mb, &mut top, &mut left);
                        continue;
                    }
                    let mut sink = Writer {
                        bw: &mut bw,
                        probs: &wprobs,
                    };
                    mb_tokens(&mut sink, mb, &mut top, &mut left);
                }
            }
            bw.finish()
        })
        .collect();

    let total = 10 + first.len() + 3 * (nparts - 1) + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    let tag = (1u32 << 4) | ((first.len() as u32) << 5);
    out.extend_from_slice(&tag.to_le_bytes()[..3]);
    out.extend_from_slice(&[0x9d, 0x01, 0x2a]);
    out.extend_from_slice(&(width as u16).to_le_bytes());
    out.extend_from_slice(&(height as u16).to_le_bytes());
    out.extend_from_slice(&first);
    for p in &parts[..nparts - 1] {
        if p.len() >= 1 << 24 {
            return Err(Error::Unsupported("webp: VP8 partition too large".into()));
        }
        out.extend_from_slice(&(p.len() as u32).to_le_bytes()[..3]);
    }
    for p in &parts {
        out.extend_from_slice(p);
    }
    Ok(out)
}
