use super::{Blk, TileDecoder};
use crate::vp9::boolcoder::BoolDecoder;
use crate::vp9::error::Error;
use crate::vp9::frame::Mv;
use crate::vp9::inter::{self, RefPlane};
use crate::vp9::intra::{self, IntraEdge};
use crate::vp9::tables::*;
use crate::vp9::transform;

fn round_q2(v: i32) -> i32 {
    (if v < 0 { v - 1 } else { v + 1 }) / 2
}
fn round_q4(v: i32) -> i32 {
    (if v < 0 { v - 2 } else { v + 2 }) / 4
}

impl<'a> TileDecoder<'a> {
    pub(super) fn residual(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        let bsize = b.size.max(BLOCK_8X8) as usize;
        let (ssx, ssy) = (self.ss_x, self.ss_y);
        let uv_tx = if b.size < BLOCK_8X8 {
            0
        } else {
            b.tx_size
                .min(MAX_TXSIZE[SS_SIZE_LOOKUP[b.size as usize][ssx][ssy] as usize])
        };
        for plane in 0..3 {
            let tx_sz = if plane > 0 { uv_tx } else { b.tx_size };
            let step = 1usize << tx_sz;
            let (sx, sy) = if plane > 0 { (ssx, ssy) } else { (0, 0) };
            let plane_sz = if plane > 0 {
                SS_SIZE_LOOKUP[bsize][ssx][ssy] as usize
            } else {
                bsize
            };
            let n4w = NUM_4X4_WIDE[plane_sz] as usize;
            let n4h = NUM_4X4_HIGH[plane_sz] as usize;
            let base_x = (b.c * 8) >> sx;
            let base_y = (b.r * 8) >> sy;
            if b.is_inter {
                if b.size < BLOCK_8X8 {
                    for y in 0..n4h {
                        for x in 0..n4w {
                            self.predict_inter(
                                b,
                                plane,
                                base_x + 4 * x,
                                base_y + 4 * y,
                                4,
                                4,
                                y * n4w + x,
                            );
                        }
                    }
                } else {
                    self.predict_inter(b, plane, base_x, base_y, n4w * 4, n4h * 4, 0);
                }
            }
            if b.is_inter && b.skip {
                let ax = base_x >> 2;
                let an = &mut self.above_nonzero[plane];
                let e = (ax + n4w.div_ceil(step) * step).min(an.len());
                an[ax.min(e)..e].fill(0);
                let ly = base_y >> 2;
                let ln = &mut self.left_nonzero[plane];
                let e = (ly + n4h.div_ceil(step) * step).min(ln.len());
                ln[ly.min(e)..e].fill(0);
                continue;
            }
            let max_x = (self.mi_cols * 8) >> sx;
            let max_y = (self.mi_rows * 8) >> sy;
            let mut block_idx = 0;
            for y in (0..n4h).step_by(step) {
                for x in (0..n4w).step_by(step) {
                    let start_x = base_x + 4 * x;
                    let start_y = base_y + 4 * y;
                    let mut nonzero = false;
                    if start_x < max_x && start_y < max_y {
                        let tx_type = self.tx_type(b, plane, tx_sz, block_idx);
                        if !b.is_inter {
                            let mode = if plane > 0 {
                                b.uv_mode
                            } else if b.size >= BLOCK_8X8 {
                                b.y_mode
                            } else {
                                b.sub_modes[block_idx]
                            };
                            let edge = IntraEdge {
                                have_left: b.avail_l || x > 0,
                                have_above: b.avail_u || y > 0,
                                not_on_right: x + step < n4w,
                                tx_size: tx_sz,
                                max_x: max_x - 1,
                                max_y: max_y - 1,
                                bit_depth: self.bit_depth,
                            };
                            intra::predict(
                                self.planes[plane],
                                self.strides[plane],
                                start_x,
                                start_y,
                                mode,
                                &edge,
                            );
                        }
                        if !b.skip {
                            let (eob, rows) =
                                self.tokens(bd, b, plane, start_x, start_y, tx_sz, tx_type);
                            nonzero = eob > 0;
                            b.eob_total += nonzero as u32;
                            if eob > 0 {
                                let stride = self.strides[plane];
                                let o = start_y * stride + start_x;
                                transform::inverse_transform_add(
                                    &self.coefs,
                                    tx_sz,
                                    tx_type,
                                    self.h.lossless,
                                    eob,
                                    rows,
                                    self.bit_depth,
                                    &mut self.planes[plane][o..],
                                    stride,
                                );
                                let side = 4usize << tx_sz;
                                if rows * side <= eob * 4 {
                                    self.coefs[..rows * side].fill(0);
                                } else {
                                    for &p in &scan(tx_sz, tx_type)[..eob] {
                                        self.coefs[p as usize] = 0;
                                    }
                                }
                            }
                        }
                    }
                    let ax = start_x >> 2;
                    let ly = start_y >> 2;
                    let an = &mut self.above_nonzero[plane];
                    let e = (ax + step).min(an.len());
                    an[ax.min(e)..e].fill(nonzero as u8);
                    let ln = &mut self.left_nonzero[plane];
                    let e = (ly + step).min(ln.len());
                    ln[ly.min(e)..e].fill(nonzero as u8);
                    block_idx += 1;
                }
            }
        }
    }

    fn tx_type(&self, b: &Blk, plane: usize, tx_sz: u8, block_idx: usize) -> u8 {
        if plane > 0 || tx_sz == TX_32X32 {
            DCT_DCT
        } else if tx_sz == TX_4X4 {
            if self.h.lossless || b.is_inter {
                DCT_DCT
            } else {
                MODE2TXFM[if b.size < BLOCK_8X8 {
                    b.sub_modes[block_idx]
                } else {
                    b.y_mode
                } as usize]
            }
        } else {
            MODE2TXFM[b.y_mode as usize]
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tokens(
        &mut self,
        bd: &mut BoolDecoder,
        b: &Blk,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx_sz: u8,
        tx_type: u8,
    ) -> (usize, usize) {
        let tx = tx_sz as usize;
        let ptype = (plane > 0) as usize;
        let rtype = b.is_inter as usize;
        let (sx, sy) = if plane > 0 {
            (self.ss_x, self.ss_y)
        } else {
            (0, 0)
        };
        let max_x4 = (2 * self.mi_cols) >> sx;
        let max_y4 = (2 * self.mi_rows) >> sy;
        let n = 1usize << tx_sz;
        let (x4, y4) = (start_x >> 2, start_y >> 2);
        let an = &self.above_nonzero[plane];
        let ae = (x4 + n).min(max_x4);
        let above = an[x4.min(ae)..ae].iter().fold(0, |a, &v| a | v);
        let ln = &self.left_nonzero[plane];
        let le = (y4 + n).min(max_y4);
        let left = ln[y4.min(le)..le].iter().fold(0, |a, &v| a | v);
        let fc = self.fc;
        let counts = &mut *self.strip.counts;
        let mut k = CoefSetup {
            probs: &fc.coef[tx][ptype][rtype],
            token_counts: &mut counts.token[tx][ptype][rtype],
            more_counts: &mut counts.more_coefs[tx][ptype][rtype],
            ctx0: (above + left) as usize,
            q: self.s.seg_q[b.seg_id as usize][ptype],
            dq_shift: (tx_sz == TX_32X32) as u32,
            bit_depth: self.bit_depth,
        };
        let Some(tc) = self.token_cache.first_chunk_mut::<1024>() else {
            return (0, 0);
        };
        let Some(cf) = self.coefs.first_chunk_mut::<1024>() else {
            return (0, 0);
        };
        let sc = scan(tx_sz, tx_type);
        let nb = neighbors(tx_sz, tx_type);
        let wide = &COEFBAND_8X8PLUS;
        match (tx_sz, self.s.counting) {
            (0, true) => read_coefs::<true, 16>(bd, sc, nb, &COEFBAND_4X4, &mut k, tc, cf),
            (0, false) => read_coefs::<false, 16>(bd, sc, nb, &COEFBAND_4X4, &mut k, tc, cf),
            (1, true) => read_coefs::<true, 64>(bd, sc, nb, wide, &mut k, tc, cf),
            (1, false) => read_coefs::<false, 64>(bd, sc, nb, wide, &mut k, tc, cf),
            (2, true) => read_coefs::<true, 256>(bd, sc, nb, wide, &mut k, tc, cf),
            (2, false) => read_coefs::<false, 256>(bd, sc, nb, wide, &mut k, tc, cf),
            (_, true) => read_coefs::<true, 1024>(bd, sc, nb, wide, &mut k, tc, cf),
            (_, false) => read_coefs::<false, 1024>(bd, sc, nb, wide, &mut k, tc, cf),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn predict_inter(
        &mut self,
        b: &Blk,
        plane: usize,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        block_idx: usize,
    ) {
        let is_compound = b.ref_frame[1] > INTRA_FRAME;
        let (sx, sy) = if plane > 0 {
            (self.ss_x, self.ss_y)
        } else {
            (0, 0)
        };
        for rl in 0..1 + is_compound as usize {
            let bm = &b.block_mvs[rl];
            let mv = if plane == 0 || b.size >= BLOCK_8X8 || (sx == 0 && sy == 0) {
                bm[block_idx]
            } else if sx == 0 && sy == 1 {
                Mv::new(
                    round_q2(bm[block_idx].row as i32 + bm[block_idx + 2].row as i32),
                    round_q2(bm[block_idx].col as i32 + bm[block_idx + 2].col as i32),
                )
            } else if sx == 1 && sy == 0 {
                Mv::new(
                    round_q2(bm[block_idx].row as i32 + bm[block_idx + 1].row as i32),
                    round_q2(bm[block_idx].col as i32 + bm[block_idx + 1].col as i32),
                )
            } else {
                let sr: i32 = bm.iter().map(|m| m.row as i32).sum();
                let sc: i32 = bm.iter().map(|m| m.col as i32).sum();
                Mv::new(round_q4(sr), round_q4(sc))
            };
            let bh = NUM_8X8_HIGH[b.size as usize] as i32;
            let bw = NUM_8X8_WIDE[b.size as usize] as i32;
            let (r, c) = (b.r as i32, b.c as i32);
            let to_top = (-(r * 8 * 16)) >> sy;
            let to_bottom = (((self.mi_rows as i32 - bh - r) * 8) * 16) >> sy;
            let to_left = (-(c * 8 * 16)) >> sx;
            let to_right = (((self.mi_cols as i32 - bw - c) * 8) * 16) >> sx;
            let spel_left = (INTERP_EXTEND + ((bw * 8) >> sx)) << 4;
            let spel_right = spel_left - 16;
            let spel_top = (INTERP_EXTEND + ((bh * 8) >> sy)) << 4;
            let spel_bottom = spel_top - 16;
            let cmv_row = ((2 * mv.row as i32) >> sy).clamp(
                to_top - spel_top,
                (to_bottom + spel_bottom).max(to_top - spel_top),
            );
            let cmv_col = ((2 * mv.col as i32) >> sx).clamp(
                to_left - spel_left,
                (to_right + spel_right).max(to_left - spel_left),
            );
            let ri = (b.ref_frame[rl] - 1).clamp(0, 2) as usize;
            let Some(rf) = &self.s.refs[ri] else {
                if self.error.is_none() {
                    self.error = Some(Error::MissingReference(format!(
                        "block uses unusable reference {}",
                        b.ref_frame[rl]
                    )));
                }
                let v = 1 << (self.bit_depth - 1);
                if rl == 0 {
                    let stride = self.strides[plane];
                    for i in 0..h {
                        let o = (y + i) * stride + x;
                        self.planes[plane][o..o + w].fill(v);
                    }
                } else {
                    self.pred2[..w * h].fill(v);
                }
                continue;
            };
            let (x_scale, y_scale) = (rf.x_scale as i64, rf.y_scale as i64);
            let (start_x, start_y, step_x, step_y) =
                if rf.x_step == 16 && rf.y_step == 16 && x_scale == 1 << 14 && y_scale == 1 << 14 {
                    (
                        ((x as i32) << 4) + cmv_col,
                        ((y as i32) << 4) + cmv_row,
                        16,
                        16,
                    )
                } else {
                    self.strip.scaled_blocks += 1;
                    let base_x = (x as i64 * x_scale) >> 14;
                    let base_y = (y as i64 * y_scale) >> 14;
                    let luma_x = (x << sx) as i64;
                    let luma_y = (y << sy) as i64;
                    let frac_x = ((16 * luma_x * x_scale) >> 14) & 15;
                    let frac_y = ((16 * luma_y * y_scale) >> 14) & 15;
                    let dx = ((cmv_col as i64 * x_scale) >> 14) + frac_x;
                    let dy = ((cmv_row as i64 * y_scale) >> 14) + frac_y;
                    (
                        ((base_x << 4) + dx) as i32,
                        ((base_y << 4) + dy) as i32,
                        rf.x_step,
                        rf.y_step,
                    )
                };
            let f = rf.frame;
            let rp = RefPlane {
                frame: f,
                plane,
                last_x: ((f.info.width as i32 + sx as i32) >> sx) - 1,
                last_y: ((f.info.height as i32 + sy as i32) >> sy) - 1,
            };
            let stride = self.strides[plane];
            let (out, os) = if rl == 0 {
                (&mut self.planes[plane][y * stride + x..], stride)
            } else {
                (&mut self.pred2[..], w)
            };
            inter::predict(
                &rp,
                start_x,
                start_y,
                step_x,
                step_y,
                w,
                h,
                b.interp_filter,
                self.bit_depth,
                out,
                os,
                &mut self.mc_tmp,
                &mut self.mc_win,
            );
        }
        if is_compound {
            let stride = self.strides[plane];
            let dst = &mut self.planes[plane];
            for i in 0..h {
                let o = (y + i) * stride + x;
                for (d, &bb) in dst[o..o + w].iter_mut().zip(&self.pred2[i * w..i * w + w]) {
                    *d = ((*d as u32 + bb as u32 + 1) >> 1) as u16;
                }
            }
        }
    }
}

struct CoefSetup<'t> {
    probs: &'t [[[u8; 3]; 6]; 6],
    token_counts: &'t mut [[[u32; 3]; 6]; 6],
    more_counts: &'t mut [[[u32; 2]; 6]; 6],
    ctx0: usize,
    q: [i32; 2],
    dq_shift: u32,
    bit_depth: u8,
}

#[inline(always)]
fn big_token(d: &mut BoolDecoder, pp: &[u8; 8], bit_depth: u8) -> (u8, i64) {
    if !d.read_bool(pp[0]) {
        return if !d.read_bool(pp[1]) {
            (2, 2)
        } else if !d.read_bool(pp[2]) {
            (3, 3)
        } else {
            (3, 4)
        };
    }
    let token = if !d.read_bool(pp[3]) {
        if !d.read_bool(pp[4]) { 5 } else { 6 }
    } else if !d.read_bool(pp[5]) {
        if !d.read_bool(pp[6]) { 7 } else { 8 }
    } else if !d.read_bool(pp[7]) {
        9
    } else {
        10
    };
    let (cat, num_extra, base) = EXTRA_BITS[token];
    let mut v = base as i64;
    if token == 10 {
        for e in 0..(bit_depth as u32 - 8) {
            if d.read_bool(255) {
                v += 1 << (5 + bit_depth as u32 - e);
            }
        }
    }
    let cp = CAT_PROBS[cat as usize];
    for (e, &p) in cp.iter().enumerate().take(num_extra as usize) {
        if d.read_bool(p) {
            v += 1 << (num_extra as usize - 1 - e);
        }
    }
    (ENERGY_CLASS[token], v)
}

fn read_coefs<const COUNT: bool, const N: usize>(
    bd: &mut BoolDecoder,
    sc: &[u16],
    nb: &[(u16, u16)],
    bands: &[u8],
    k: &mut CoefSetup,
    tc: &mut [u8; 1024],
    coefs: &mut [i32; 1024],
) -> (usize, usize) {
    let (Some(sc), Some(nb), Some(bands)) = (
        sc.first_chunk::<N>(),
        nb.first_chunk::<N>(),
        bands.first_chunk::<N>(),
    ) else {
        return (0, 0);
    };
    let row_shift = N.trailing_zeros() / 2;
    let pareto = pareto_full();
    let mut d = *bd;
    let mut max_row = 0usize;
    let mut c = 0usize;
    let mut ctx = k.ctx0;
    'outer: loop {
        let mut band = bands[c] as usize;
        let mut p = &k.probs[band][ctx];
        let more = d.read_bool(p[0]);
        if COUNT {
            k.more_counts[band][ctx][more as usize] += 1;
        }
        if !more {
            break;
        }
        while !d.read_bool(p[1]) {
            if COUNT {
                k.token_counts[band][ctx][0] += 1;
            }
            tc[sc[c] as usize & (N - 1)] = 0;
            c += 1;
            if c == N {
                break 'outer;
            }
            let (a, bb) = nb[c];
            ctx = ((1 + tc[a as usize & (N - 1)] + tc[bb as usize & (N - 1)]) >> 1) as usize;
            band = bands[c] as usize;
            p = &k.probs[band][ctx];
        }
        let pos = sc[c] as usize & (N - 1);
        let (energy, coef) = if !d.read_bool(p[2]) {
            if COUNT {
                k.token_counts[band][ctx][1] += 1;
            }
            (1, 1)
        } else {
            if COUNT {
                k.token_counts[band][ctx][2] += 1;
            }
            big_token(&mut d, &pareto[p[2].max(1) as usize - 1], k.bit_depth)
        };
        tc[pos] = energy;
        let sign = d.read_half();
        let qv = k.q[(pos != 0) as usize] as i64;
        let v = ((coef * qv) >> k.dq_shift) as i32;
        coefs[pos] = if sign { v.wrapping_neg() } else { v };
        max_row = max_row.max(pos >> row_shift);
        c += 1;
        if c == N {
            break;
        }
        let (a, bb) = nb[c];
        ctx = ((1 + tc[a as usize & (N - 1)] + tc[bb as usize & (N - 1)]) >> 1) as usize;
    }
    *bd = d;
    (c, max_row + 1)
}
