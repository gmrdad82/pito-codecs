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
    for (plane, view) in views.iter_mut().enumerate() {
        for pass in 0..2 {
            filter_sb_plane(view, mi, f, plane, pass, row, col);
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Group {
    pub size: u8,
    pub lvl: u8,
    pub apply: u8,
}

fn filter_sb_plane(
    v: &mut PlaneView,
    mi: &MiGrid,
    f: &LfFrame,
    plane: usize,
    pass: usize,
    row: usize,
    col: usize,
) {
    let (sub_x, sub_y) = if plane > 0 { (f.ss_x, f.ss_y) } else { (0, 0) };
    let (sub, edge_len) = if pass == 0 {
        (sub_x, 64 >> sub_y)
    } else {
        (sub_y, 64 >> sub_x)
    };
    let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
    let across = if pass == 0 { 1usize } else { v.stride };
    let along = if pass == 0 { v.stride } else { 1 };
    let groups_n = edge_len / 8;
    for edge in 0..(16 >> sub) {
        let mut groups = [Group::default(); 8];
        let mut any = false;
        let mut first = None;
        for (gi, g) in groups.iter_mut().enumerate().take(groups_n) {
            let i = gi * 8;
            let (x, y) = if pass == 0 {
                (col * 8 + edge * (4 << sub_x), row * 8 + (i << sub_y))
            } else {
                (col * 8 + (i << sub_x), row * 8 + edge * (4 << sub_y))
            };
            if x >= 8 * mi_cols
                || y >= 8 * mi_rows
                || (pass == 0 && x == 0)
                || (pass == 1 && y == 0)
            {
                continue;
            }
            if first.is_none() {
                first = Some((gi, x, y));
            }
            *g = decide(mi, f, plane, pass, edge, x, y, sub_x, sub_y, edge_len - i);
            any |= g.apply != 0;
        }
        let Some((g0, x, y)) = first else {
            continue;
        };
        if !any {
            continue;
        }
        let i0 = g0 * 8;
        let (x0, y0) = if pass == 0 {
            (x, y - (i0 << sub_y))
        } else {
            (x - (i0 << sub_x), y)
        };
        let px = x0 >> sub_x;
        let py = y0 >> sub_y;
        let pos = (py - v.oy) * v.stride + px - v.ox;
        filter_edge(
            v.data,
            pos,
            along,
            across,
            &groups[..groups_n],
            f,
            pass == 1,
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn decide(
    mi: &MiGrid,
    f: &LfFrame,
    plane: usize,
    pass: usize,
    edge: usize,
    x: usize,
    y: usize,
    sub_x: usize,
    sub_y: usize,
    remaining: usize,
) -> Group {
    let (mi_rows, mi_cols) = (f.mi_rows, f.mi_cols);
    let sub = if pass == 0 { sub_x } else { sub_y };
    let loop_col = ((x >> 3) >> sub_x) << sub_x;
    let loop_row = ((y >> 3) >> sub_y) << sub_y;
    let m = mi.at(loop_row, loop_col);
    let tx_sz = if plane > 0 {
        if m.sb_size < BLOCK_8X8 {
            0
        } else {
            m.tx_size.min(
                MAX_TXSIZE[SS_SIZE_LOOKUP[m.sb_size as usize][f.ss_x][f.ss_y].min(12) as usize],
            )
        }
    } else {
        m.tx_size
    };
    let sb_size = if sub == 0 {
        m.sb_size
    } else {
        m.sb_size.max(BLOCK_16X16)
    } as usize;
    let is_intra = m.ref_frame[0] <= INTRA_FRAME;
    let is_block_edge = if pass == 0 {
        x & (8 * NUM_8X8_WIDE[sb_size] as usize - 1) == 0
    } else {
        y & (8 * NUM_8X8_HIGH[sb_size] as usize - 1) == 0
    };
    let tx_ok = edge & ((1 << tx_sz) - 1) == 0 && (is_intra || !m.skip);
    if !is_block_edge && !tx_ok {
        return Group::default();
    }
    let lvl = f.level(m);
    if lvl == 0 {
        return Group::default();
    }
    let is32 = edge.is_multiple_of(8);
    let base_size = if tx_sz == TX_4X4 && is32 {
        TX_8X8
    } else {
        tx_sz.min(TX_16X16)
    };
    let filter_size = if base_size == TX_16X16
        && ((pass == 0 && sub_x == 1 && (x >> 3) == mi_cols - 1)
            || (pass == 1 && sub_y == 1 && (y >> 3) == mi_rows - 1))
    {
        TX_8X8
    } else {
        base_size
    };
    let n = if pass == 0 {
        (8 * mi_rows - y).div_ceil(1 << sub_y)
    } else {
        (8 * mi_cols - x).div_ceil(1 << sub_x)
    }
    .min(LANES)
    .min(remaining);
    let odd_last = pass == 1 && sub_x == 1 && mi_cols & 1 == 1 && edge & 1 == 1;
    let mut apply = 0u8;
    for l in 0..n {
        let is_tx_edge = tx_ok && !(odd_last && x + (l << sub_x) + 8 >= mi_cols * 8);
        if is_block_edge || is_tx_edge {
            apply |= 1 << l;
        }
    }
    Group {
        size: filter_size,
        lvl,
        apply,
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
