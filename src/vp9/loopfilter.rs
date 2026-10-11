use crate::vp9::frame::{MiGrid, MiInfo};
use crate::vp9::header::{LoopFilterParams, Segmentation};
use crate::vp9::tables::*;

#[cfg(target_arch = "x86_64")]
mod avx2;

pub struct LfFrame {
    pub lvl: [[[u8; 2]; 4]; 8],
    pub limits: [(i32, i32, i32); 64],
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub ss_x: usize,
    pub ss_y: usize,
    pub bit_depth: u8,
}

impl LfFrame {
    pub fn new(
        lf: &LoopFilterParams,
        seg: &Segmentation,
        mi_rows: usize,
        mi_cols: usize,
        ss_x: bool,
        ss_y: bool,
        bit_depth: u8,
    ) -> LfFrame {
        let mut lvl = [[[0u8; 2]; 4]; 8];
        let n_shift = lf.level >> 5;
        for (seg_id, l) in lvl.iter_mut().enumerate() {
            let mut lvl_seg = lf.level as i32;
            if seg.feature_active(seg_id as u8, SEG_LVL_ALT_L) {
                let d = seg.feature_data[seg_id][SEG_LVL_ALT_L] as i32;
                lvl_seg = if seg.abs_or_delta_update {
                    d
                } else {
                    d + lf.level as i32
                };
                lvl_seg = lvl_seg.clamp(0, MAX_LOOP_FILTER);
            }
            if !lf.delta_enabled {
                *l = [[lvl_seg as u8; 2]; 4];
            } else {
                let intra = lvl_seg + ((lf.ref_deltas[0] as i32) << n_shift);
                l[0][0] = intra.clamp(0, MAX_LOOP_FILTER) as u8;
                l[0][1] = l[0][0];
                for rf in 1..4 {
                    for mode in 0..2 {
                        let v = lvl_seg
                            + ((lf.ref_deltas[rf] as i32) << n_shift)
                            + ((lf.mode_deltas[mode] as i32) << n_shift);
                        l[rf][mode] = v.clamp(0, MAX_LOOP_FILTER) as u8;
                    }
                }
            }
        }
        let mut limits = [(0, 0, 0); 64];
        let sh = lf.sharpness as i32;
        let shift = if sh > 4 {
            2
        } else if sh > 0 {
            1
        } else {
            0
        };
        for (l, e) in limits.iter_mut().enumerate() {
            let l = l as i32;
            let limit = if sh > 0 {
                (l >> shift).clamp(1, 9 - sh)
            } else {
                (l >> shift).max(1)
            };
            *e = (limit, 2 * (l + 2) + limit, l >> 4);
        }
        LfFrame {
            lvl,
            limits,
            mi_rows,
            mi_cols,
            ss_x: ss_x as usize,
            ss_y: ss_y as usize,
            bit_depth,
        }
    }

    fn level(&self, mi: &MiInfo) -> u8 {
        let mode_type = matches!(mi.y_mode, NEARESTMV | NEARMV | NEWMV) as usize;
        let rf = mi.ref_frame[0].max(0) as usize;
        self.lvl[mi.seg_id as usize & 7][rf][mode_type]
    }
}

pub struct PlaneView<'a> {
    pub data: &'a mut [u16],
    pub stride: usize,
    pub ox: usize,
    pub oy: usize,
}

pub fn filter_superblock(
    views: &mut [PlaneView; 3],
    mi: &MiGrid,
    f: &LfFrame,
    row: usize,
    col: usize,
) {
    let mut cells = [Cell::default(); 64];
    let rows = (f.mi_rows - row).min(8);
    let cols = (f.mi_cols - col).min(8);
    for r in 0..rows {
        for c in 0..cols {
            let m = mi.at(row + r, col + c);
            let tx_uv = if m.sb_size < BLOCK_8X8 {
                0
            } else {
                m.tx_size.min(
                    MAX_TXSIZE[SS_SIZE_LOOKUP[m.sb_size as usize][f.ss_x][f.ss_y].min(12) as usize],
                )
            };
            cells[r * 8 + c] = Cell {
                lvl: f.level(m),
                tx: [m.tx_size, tx_uv],
                sb: m.sb_size,
                coded: m.ref_frame[0] <= INTRA_FRAME || !m.skip,
            };
        }
    }
    let (luma, chroma) = views.split_at_mut(1);
    for pass in 0..2 {
        filter_sb_planes(luma, &cells, f, false, pass, row, col);
    }
    for pass in 0..2 {
        filter_sb_planes(chroma, &cells, f, true, pass, row, col);
    }
}

#[derive(Clone, Copy, Default)]
struct Cell {
    lvl: u8,
    tx: [u8; 2],
    sb: u8,
    coded: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Group {
    pub size: u8,
    pub lvl: u8,
    pub apply: u8,
}

fn filter_sb_planes(
    views: &mut [PlaneView],
    cells: &[Cell; 64],
    f: &LfFrame,
    chroma: bool,
    pass: usize,
    row: usize,
    col: usize,
) {
    let (sub_x, sub_y) = if chroma { (f.ss_x, f.ss_y) } else { (0, 0) };
    let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
    let ci = chroma as usize;
    let (sub, sub_along, edge_len) = if pass == 0 {
        (sub_x, sub_y, 64 >> sub_y)
    } else {
        (sub_y, sub_x, 64 >> sub_x)
    };
    let (base_across, base_along, end_across, end_along) = if pass == 0 {
        (col * 8, row * 8, 8 * mi_cols, 8 * mi_rows)
    } else {
        (row * 8, col * 8, 8 * mi_rows, 8 * mi_cols)
    };
    let groups_all = edge_len / 8;
    let groups_n = groups_all.min((end_along - base_along).div_ceil(8 << sub_along));
    let mut full = [0u8; 8];
    let mut odd = [0u8; 8];
    let odd_last = pass == 1 && sub_x == 1 && mi_cols & 1 == 1;
    for (gi, (fl, od)) in full
        .iter_mut()
        .zip(odd.iter_mut())
        .enumerate()
        .take(groups_n)
    {
        let at = base_along + ((gi * 8) << sub_along);
        let n = (end_along - at).div_ceil(1 << sub_along).min(LANES);
        *fl = ((1u32 << n) - 1) as u8;
        *od = if odd_last {
            let k = (mi_cols * 8)
                .saturating_sub(at + 8)
                .div_ceil(1 << sub_x)
                .min(n);
            ((1u32 << k) - 1) as u8
        } else {
            *fl
        };
    }
    let sizes: [u8; 13] = std::array::from_fn(|b| {
        let b = if sub == 0 {
            b as u8
        } else {
            (b as u8).max(BLOCK_16X16)
        } as usize;
        let n8 = if pass == 0 {
            NUM_8X8_WIDE[b]
        } else {
            NUM_8X8_HIGH[b]
        };
        8 * n8 - 1
    });
    for a in 0..(8 >> sub) {
        let at = base_across + (a << (3 + sub));
        if at >= end_across {
            break;
        }
        let cap = if sub == 1 && (at >> 3) == (end_across >> 3) - 1 {
            TX_8X8
        } else {
            TX_16X16
        };
        let is32 = (a & 3 == 0) as u8;
        let edge_bit = (2 * a) as u8;
        let cell_across = a << sub;
        let mut even = [Group::default(); 8];
        let mut odds = [Group::default(); 8];
        let (mut any_even, mut any_odd) = (0u8, 0u8);
        for gi in 0..groups_n {
            let along = gi << sub_along;
            let m = if pass == 0 {
                cells[along * 8 + cell_across]
            } else {
                cells[cell_across * 8 + along]
            };
            let tx = m.tx[ci];
            let live = m.coded & (m.lvl != 0);
            let block = (at & sizes[m.sb as usize] as usize) == 0;
            let tx_ok = m.coded & (edge_bit & ((1u8 << tx) - 1) == 0);
            let on = 0u8.wrapping_sub(((block | tx_ok) & (m.lvl != 0)) as u8);
            let size = tx.min(TX_16X16).max(is32 & (tx == TX_4X4) as u8).min(cap);
            let apply = on & full[gi];
            even[gi] = Group {
                size: size & on,
                lvl: m.lvl & on,
                apply,
            };
            any_even |= apply;
            let on4 = 0u8.wrapping_sub((live & (tx == TX_4X4)) as u8);
            let apply4 = on4 & odd[gi];
            odds[gi] = Group {
                size: TX_4X4,
                lvl: m.lvl & on4,
                apply: apply4,
            };
            any_odd |= apply4;
        }
        if at == 0 {
            any_even = 0;
        }
        if at + (4 << sub) >= end_across {
            any_odd = 0;
        }
        for (groups, any, shift) in [(&even, any_even, 0usize), (&odds, any_odd, 4usize << sub)] {
            if any == 0 {
                continue;
            }
            let (x0, y0) = if pass == 0 {
                (at + shift, base_along)
            } else {
                (base_along, at + shift)
            };
            for v in views.iter_mut() {
                let (across, along) = if pass == 0 {
                    (1, v.stride)
                } else {
                    (v.stride, 1)
                };
                let pos = ((y0 >> sub_y) - v.oy) * v.stride + (x0 >> sub_x) - v.ox;
                filter_edge(
                    v.data,
                    pos,
                    along,
                    across,
                    &groups[..groups_all],
                    f,
                    pass == 1,
                );
            }
        }
    }
}

crate::vp9::isa::dispatch! {
fn filter_edge(
    d: &mut [u16],
    pos: usize,
    along: usize,
    across: usize,
    groups: &[Group],
    f: &LfFrame,
    rows: bool,
) {
    #[cfg(target_arch = "x86_64")]
    if f.bit_depth <= 10 && crate::vp9::isa::avx2() {
        for (k, pair) in groups.chunks(2).enumerate() {
            let g1 = pair.get(1).copied().unwrap_or_default();
            if pair[0].apply | g1.apply == 0 {
                continue;
            }
            let p = pos + k * 16 * along;
            unsafe { avx2::filter16(d, p, along, across, [pair[0], g1], f, rows) };
        }
        return;
    }
    for (k, g) in groups.iter().enumerate() {
        if g.apply == 0 {
            continue;
        }
        let (limit, blimit, thresh) = f.limits[g.lvl as usize];
        let lim = Limits {
            apply: std::array::from_fn(|l| g.apply >> l & 1 == 1),
            limit,
            blimit,
            thresh,
            filter_size: g.size,
            bit_depth: f.bit_depth,
        };
        let p = pos + k * 8 * along;
        let wide = g.size == TX_16X16;
        match (f.bit_depth <= 10, wide, rows) {
            (true, false, false) => lanes16::filter_group::<4, false>(d, p, along, across, &lim),
            (true, false, true) => lanes16::filter_group::<4, true>(d, p, along, across, &lim),
            (true, true, false) => lanes16::filter_group::<8, false>(d, p, along, across, &lim),
            (true, true, true) => lanes16::filter_group::<8, true>(d, p, along, across, &lim),
            (false, false, false) => lanes32::filter_group::<4, false>(d, p, along, across, &lim),
            (false, false, true) => lanes32::filter_group::<4, true>(d, p, along, across, &lim),
            (false, true, false) => lanes32::filter_group::<8, false>(d, p, along, across, &lim),
            (false, true, true) => lanes32::filter_group::<8, true>(d, p, along, across, &lim),
        }
    }
}
}

const LANES: usize = 8;

struct Limits {
    apply: [bool; LANES],
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: u8,
    bit_depth: u8,
}

macro_rules! group_filter {
    ($m:ident, $t:ty) => {
        mod $m {
            use super::{LANES, Limits};
            use crate::vp9::tables::*;
            type T = $t;

            #[inline(always)]
            pub(super) fn filter_group<const R: usize, const ROWS: bool>(
                d: &mut [u16],
                pos: usize,
                along: usize,
                step: usize,
                g: &Limits,
            ) {
                let Limits {
                    apply,
                    limit,
                    blimit,
                    thresh,
                    filter_size,
                    bit_depth,
                } = *g;
                let (limit, blimit, thresh) = (limit as T, blimit as T, thresh as T);
                let mut v = [[0 as T; LANES]; 16];
                let start = pos - R * step;
                if ROWS {
                    for k in 0..2 * R {
                        let row: &[u16; LANES] = d[start + k * step..start + k * step + LANES]
                            .try_into()
                            .expect("lanes");
                        v[8 - R + k] = std::array::from_fn(|l| row[l] as T);
                    }
                } else {
                    for l in 0..LANES {
                        let s = start + l * along;
                        let line: &[u16] = &d[s..s + 2 * R];
                        for k in 0..2 * R {
                            v[8 - R + k][l] = line[k] as T;
                        }
                    }
                }
                let shift = bit_depth as u32 - 8;
                let limit_bd = limit << shift;
                let blimit_bd = blimit << shift;
                let thresh_bd = thresh << shift;
                let one = 1 << shift;
                let (p3, p2, p1, p0, q0, q1, q2, q3) =
                    (&v[4], &v[5], &v[6], &v[7], &v[8], &v[9], &v[10], &v[11]);
                let le = |a: T, b: T| -((a <= b) as T);
                let sel = |m: T, a: T, b: T| (a & m) | (b & !m);
                let mut mask = [0 as T; LANES];
                let mut hev = [0 as T; LANES];
                let mut flat = [0 as T; LANES];
                let mut flat2 = [0 as T; LANES];
                let can_flat = -((filter_size >= TX_8X8) as T);
                let can_flat2 = R == 8 && filter_size >= TX_16X16;
                let apply: [T; LANES] = apply.map(|a| -(a as T));
                for l in 0..LANES {
                    let d_p1p0 = (p1[l] - p0[l]).abs();
                    let d_q1q0 = (q1[l] - q0[l]).abs();
                    mask[l] = apply[l]
                        & le((p3[l] - p2[l]).abs(), limit_bd)
                        & le((p2[l] - p1[l]).abs(), limit_bd)
                        & le(d_p1p0, limit_bd)
                        & le(d_q1q0, limit_bd)
                        & le((q2[l] - q1[l]).abs(), limit_bd)
                        & le((q3[l] - q2[l]).abs(), limit_bd)
                        & le(
                            (p0[l] - q0[l]).abs() * 2 + ((p1[l] - q1[l]).abs() >> 1),
                            blimit_bd,
                        );
                    hev[l] = !(le(d_p1p0, thresh_bd) & le(d_q1q0, thresh_bd));
                    flat[l] = can_flat
                        & mask[l]
                        & le(d_p1p0, one)
                        & le(d_q1q0, one)
                        & le((p2[l] - p0[l]).abs(), one)
                        & le((q2[l] - q0[l]).abs(), one)
                        & le((p3[l] - p0[l]).abs(), one)
                        & le((q3[l] - q0[l]).abs(), one);
                }
                if mask.iter().fold(0, |a, &m| a | m) == 0 {
                    return;
                }
                if can_flat2 {
                    for l in 0..LANES {
                        flat2[l] = flat[l]
                            & le((v[0][l] - p0[l]).abs(), one)
                            & le((v[1][l] - p0[l]).abs(), one)
                            & le((v[2][l] - p0[l]).abs(), one)
                            & le((v[3][l] - p0[l]).abs(), one)
                            & le((v[12][l] - q0[l]).abs(), one)
                            & le((v[13][l] - q0[l]).abs(), one)
                            & le((v[14][l] - q0[l]).abs(), one)
                            & le((v[15][l] - q0[l]).abs(), one);
                    }
                }
                let any_flat = flat.iter().fold(0, |a, &f| a | f) != 0;
                let any_flat2 = flat2.iter().fold(0, |a, &f| a | f) != 0;
                let mut out = v;
                {
                    let lo = -(1 << (bit_depth - 1)) as T;
                    let hi = ((1 << (bit_depth - 1)) - 1) as T;
                    let c = |x: T| x.max(lo).min(hi);
                    let off: T = 0x80 << shift;
                    for l in 0..LANES {
                        let (ps1, ps0, qs0, qs1) =
                            (p1[l] - off, p0[l] - off, q0[l] - off, q1[l] - off);
                        let filter = c((c(ps1 - qs1) & hev[l]) + 3 * (qs0 - ps0));
                        let filter1 = c(filter + 4) >> 3;
                        let filter2 = c(filter + 3) >> 3;
                        let f = (filter1 + 1) >> 1;
                        let narrow = mask[l] & !flat[l];
                        let outer = narrow & !hev[l];
                        out[8][l] = sel(narrow, c(qs0 - filter1) + off, out[8][l]);
                        out[7][l] = sel(narrow, c(ps0 + filter2) + off, out[7][l]);
                        out[9][l] = sel(outer, c(qs1 - f) + off, out[9][l]);
                        out[6][l] = sel(outer, c(ps1 + f) + off, out[6][l]);
                    }
                }
                if any_flat {
                    let w8 = wide_filter::<3>(&v);
                    for k in 5..11 {
                        for l in 0..LANES {
                            out[k][l] = sel(flat[l] & !flat2[l], w8[k][l], out[k][l]);
                        }
                    }
                }
                if any_flat2 {
                    let w16 = wide_filter::<4>(&v);
                    for k in 1..15 {
                        for l in 0..LANES {
                            out[k][l] = sel(flat2[l], w16[k][l], out[k][l]);
                        }
                    }
                }
                let (lo, hi) = if any_flat2 {
                    (1, 15)
                } else if any_flat {
                    (5, 11)
                } else {
                    (6, 10)
                };
                if ROWS {
                    for k in lo..hi {
                        let o = start + (k + R - 8) * step;
                        let row: &mut [u16; LANES] =
                            (&mut d[o..o + LANES]).try_into().expect("lanes");
                        for l in 0..LANES {
                            row[l] = out[k][l] as u16;
                        }
                    }
                } else {
                    for l in 0..LANES {
                        let s = start + l * along;
                        let line: &mut [u16] = &mut d[s..s + 2 * R];
                        for k in 0..2 * R {
                            line[k] = out[8 - R + k][l] as u16;
                        }
                    }
                }
            }

            #[inline(always)]
            fn wide_filter<const LOG2: u32>(v: &[[T; LANES]; 16]) -> [[T; LANES]; 16] {
                let n = (1isize << (LOG2 - 1)) - 1;
                let s = |k: isize| &v[(k.clamp(-(n + 1), n) + 8) as usize];
                let mut out = [[0 as T; LANES]; 16];
                let mut sum = [0 as T; LANES];
                for j in -n..=n {
                    let a = s(-n + j);
                    for l in 0..LANES {
                        sum[l] += a[l];
                    }
                }
                let round: T = 1 << (LOG2 - 1);
                for i in -n..n {
                    let (cur, add, sub) = (s(i), s(i + 1 + n), s(i - n));
                    let o = &mut out[(i + 8) as usize];
                    for l in 0..LANES {
                        o[l] = (sum[l] + cur[l] + round) >> LOG2;
                        sum[l] += add[l] - sub[l];
                    }
                }
                out
            }
        }
    };
}
group_filter!(lanes16, i16);
group_filter!(lanes32, i32);
