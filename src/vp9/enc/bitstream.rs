use std::sync::LazyLock;

use super::bool::{BoolWriter, cost};
use super::tables::*;
use crate::vp9::probs::{Counts, FrameContext};

pub struct BitWriter {
    pub buf: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter {
            buf: Vec::with_capacity(64),
            bits: 0,
        }
    }

    pub fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.buf.push(0);
            }
            if (v >> i) & 1 == 1 {
                let last = self.buf.len() - 1;
                self.buf[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    pub fn flag(&mut self, b: bool) {
        self.put(b as u32, 1);
    }
}

pub struct SegParams {
    pub tree_probs: [u8; 7],
    pub abs_delta: bool,
    pub q: [Option<i16>; 8],
    pub lf: [Option<i16>; 8],
}

pub struct Uncompressed {
    pub key: bool,
    pub width: u32,
    pub height: u32,
    pub color_space: u8,
    pub full_range: bool,
    pub refresh_frame_flags: u8,
    pub ref_frame_idx: [u8; 3],
    pub allow_hp: bool,
    pub interp_filter: u8,
    pub refresh_frame_context: bool,
    pub frame_parallel: bool,
    pub frame_context_idx: u8,
    pub lf_level: u8,
    pub sharpness: u8,
    pub base_q_idx: u8,
    pub seg: Option<SegParams>,
    pub tile_cols_log2: u32,
    pub tile_rows_log2: u32,
}

pub fn tile_cols_bounds(mi_cols: u32) -> (u32, u32) {
    let sb_cols = mi_cols.div_ceil(8);
    let mut min_log2 = 0;
    while (64 << min_log2) < sb_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1;
    while (sb_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    (min_log2, max_log2 - 1)
}

pub fn write_uncompressed(h: &Uncompressed, compressed_size: usize) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.put(2, 2);
    w.put(0, 1);
    w.put(0, 1);
    w.flag(false);
    w.flag(!h.key);
    w.flag(true);
    w.flag(false);
    if h.key {
        w.put(0x49, 8);
        w.put(0x83, 8);
        w.put(0x42, 8);
        w.put(h.color_space as u32, 3);
        w.flag(h.full_range);
        w.put(h.width - 1, 16);
        w.put(h.height - 1, 16);
        w.flag(false);
    } else {
        w.put(0, 2);
        w.put(h.refresh_frame_flags as u32, 8);
        for i in 0..3 {
            w.put(h.ref_frame_idx[i] as u32, 3);
            w.flag(false);
        }
        w.flag(true);
        w.flag(false);
        w.flag(h.allow_hp);
        if h.interp_filter == SWITCHABLE {
            w.flag(true);
        } else {
            w.flag(false);
            let lit = LITERAL_TO_TYPE
                .iter()
                .position(|&t| t == h.interp_filter)
                .unwrap_or(1);
            w.put(lit as u32, 2);
        }
    }
    w.flag(h.refresh_frame_context);
    w.flag(h.frame_parallel);
    w.put(h.frame_context_idx as u32, 2);
    w.put(h.lf_level as u32, 6);
    w.put(h.sharpness as u32, 3);
    w.flag(true);
    w.flag(false);
    w.put(h.base_q_idx as u32, 8);
    w.flag(false);
    w.flag(false);
    w.flag(false);
    match &h.seg {
        None => w.flag(false),
        Some(sp) => {
            w.flag(true);
            w.flag(true);
            for &p in &sp.tree_probs {
                w.flag(p != 255);
                if p != 255 {
                    w.put(p as u32, 8);
                }
            }
            w.flag(false);
            w.flag(true);
            w.flag(sp.abs_delta);
            for i in 0..8 {
                for (j, f) in [sp.q[i], sp.lf[i], None, None].into_iter().enumerate() {
                    w.flag(f.is_some());
                    if let Some(v) = f {
                        w.put(v.unsigned_abs() as u32, SEGMENTATION_FEATURE_BITS[j]);
                        if SEGMENTATION_FEATURE_SIGNED[j] {
                            w.flag(v < 0);
                        }
                    }
                }
            }
        }
    }
    let mi_cols = h.width.div_ceil(8);
    let (min_log2, max_log2) = tile_cols_bounds(mi_cols);
    let mut k = min_log2;
    while k < max_log2 {
        if k < h.tile_cols_log2 {
            w.flag(true);
            k += 1;
        } else {
            w.flag(false);
            break;
        }
    }
    w.flag(h.tile_rows_log2 > 0);
    if h.tile_rows_log2 > 0 {
        w.flag(h.tile_rows_log2 > 1);
    }
    w.put(compressed_size as u32, 16);
    w.buf
}

fn inv_recenter_nonneg(v: i32, m: i32) -> i32 {
    if v > 2 * m {
        v
    } else if v & 1 != 0 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

fn inv_remap_prob(delta: usize, prob: u8) -> u8 {
    let v = INV_MAP_TABLE[delta.min(254)] as i32;
    let m = prob as i32 - 1;
    let r = if (m << 1) <= 255 {
        1 + inv_recenter_nonneg(v, m)
    } else {
        255 - inv_recenter_nonneg(v, 255 - 1 - m)
    };
    r.clamp(1, 255) as u8
}

static REMAP: LazyLock<Vec<[u8; 256]>> = LazyLock::new(|| {
    let mut t = vec![[255u8; 256]; 256];
    for old in 1..=255u8 {
        for delta in (0..=254usize).rev() {
            t[old as usize][inv_remap_prob(delta, old) as usize] = delta as u8;
        }
    }
    t
});

fn subexp_bits(delta: u32) -> u32 {
    if delta < 16 {
        5
    } else if delta < 32 {
        6
    } else if delta < 64 {
        8
    } else if delta < 129 {
        10
    } else {
        11
    }
}

fn write_subexp(w: &mut BoolWriter, delta: u32) {
    if delta < 16 {
        w.literal(0, 1);
        w.literal(delta, 4);
    } else if delta < 32 {
        w.literal(2, 2);
        w.literal(delta - 16, 4);
    } else if delta < 64 {
        w.literal(6, 3);
        w.literal(delta - 32, 5);
    } else if delta < 129 {
        w.literal(7, 3);
        w.literal(delta - 64, 7);
    } else {
        w.literal(7, 3);
        w.literal((delta + 1) >> 1, 7);
        w.literal((delta + 1) & 1, 1);
    }
}

pub fn binary_prob(c0: u32, c1: u32) -> u8 {
    let den = c0 as u64 + c1 as u64;
    if den == 0 {
        return 128;
    }
    ((c0 as u64 * 256 + (den >> 1)) / den).clamp(1, 255) as u8
}

fn branch_bits(c: [u32; 2], p: u8) -> u64 {
    c[0] as u64 * cost(false, p) as u64 + c[1] as u64 * cost(true, p) as u64
}

const UPD: u8 = 252;

fn update_choice(c: [u32; 2], old: u8) -> Option<(u8, u32, i64)> {
    if c[0] + c[1] == 0 {
        return None;
    }
    let new = binary_prob(c[0], c[1]);
    if new == old {
        return None;
    }
    let delta = REMAP[old as usize][new as usize] as u32;
    let p = inv_remap_prob(delta as usize, old);
    let old_bits = branch_bits(c, old) as i64 + cost(false, UPD) as i64;
    let new_bits =
        branch_bits(c, p) as i64 + cost(true, UPD) as i64 + 256 * subexp_bits(delta) as i64;
    let saving = old_bits - new_bits;
    (saving > 0).then_some((p, delta, saving))
}

fn diff_update(w: &mut BoolWriter, prob: &mut u8, c: [u32; 2]) {
    match update_choice(c, *prob) {
        Some((p, delta, _)) => {
            w.put(true, UPD);
            write_subexp(w, delta);
            *prob = p;
        }
        None => w.put(false, UPD),
    }
}

fn mv_update(w: &mut BoolWriter, prob: &mut u8, c: [u32; 2]) {
    if c[0] + c[1] > 0 {
        let new = binary_prob(c[0], c[1]) | 1;
        let old_bits = branch_bits(c, *prob) as i64 + cost(false, UPD) as i64;
        let new_bits = branch_bits(c, new) as i64 + cost(true, UPD) as i64 + 7 * 256;
        if new != *prob && new_bits < old_bits {
            w.put(true, UPD);
            w.literal((new >> 1) as u32, 7);
            *prob = new;
            return;
        }
    }
    w.put(false, UPD);
}

fn tree_branches(tree: &[i8], counts: &[u32], out: &mut [[u32; 2]]) {
    fn walk(tree: &[i8], i: usize, counts: &[u32], out: &mut [[u32; 2]]) -> u32 {
        let mut sums = [0u32; 2];
        for (b, s) in sums.iter_mut().enumerate() {
            let t = tree[i + b];
            *s = if t <= 0 {
                counts.get((-t) as usize).copied().unwrap_or(0)
            } else {
                walk(tree, t as usize, counts, out)
            };
        }
        out[i >> 1] = sums;
        sums[0] + sums[1]
    }
    walk(tree, 0, counts, out);
}

fn tree_update(w: &mut BoolWriter, tree: &[i8], probs: &mut [u8], counts: &[u32]) {
    let mut br = [[0u32; 2]; 16];
    tree_branches(tree, counts, &mut br);
    for (i, p) in probs.iter_mut().enumerate() {
        diff_update(w, p, br[i]);
    }
}

pub struct CompressedParams {
    pub intra: bool,
    pub lossless: bool,
    pub tx_mode: u8,
    pub interp_filter: u8,
    pub allow_hp: bool,
}

pub fn write_compressed(h: &CompressedParams, fc: &mut FrameContext, c: &Counts) -> Vec<u8> {
    let mut w = BoolWriter::new(Vec::with_capacity(256));
    if !h.lossless {
        w.literal(h.tx_mode.min(3) as u32, 2);
        if h.tx_mode >= 3 {
            w.literal((h.tx_mode == TX_MODE_SELECT) as u32, 1);
        }
    }
    if h.tx_mode == TX_MODE_SELECT {
        let mut br = [[0u32; 2]; 4];
        for i in 0..2 {
            tree_branches(&TX_SIZE_8_TREE, &c.tx[1][i], &mut br);
            diff_update(&mut w, &mut fc.tx[1][i][0], br[0]);
        }
        for i in 0..2 {
            tree_branches(&TX_SIZE_16_TREE, &c.tx[2][i], &mut br);
            for j in 0..2 {
                diff_update(&mut w, &mut fc.tx[2][i][j], br[j]);
            }
        }
        for i in 0..2 {
            tree_branches(&TX_SIZE_32_TREE, &c.tx[3][i], &mut br);
            for j in 0..3 {
                diff_update(&mut w, &mut fc.tx[3][i][j], br[j]);
            }
        }
    }
    let max_tx = TX_MODE_TO_BIGGEST_TX_SIZE[h.tx_mode as usize] as usize;
    for tx in 0..=max_tx {
        coef_updates(&mut w, fc, c, tx);
    }
    for i in 0..3 {
        diff_update(&mut w, &mut fc.skip[i], c.skip[i]);
    }
    if !h.intra {
        for i in 0..7 {
            tree_update(
                &mut w,
                &INTER_MODE_TREE,
                &mut fc.inter_mode[i],
                &c.inter_mode[i],
            );
        }
        if h.interp_filter == SWITCHABLE {
            for j in 0..4 {
                tree_update(
                    &mut w,
                    &INTERP_FILTER_TREE,
                    &mut fc.interp_filter[j],
                    &c.interp_filter[j],
                );
            }
        }
        for i in 0..4 {
            diff_update(&mut w, &mut fc.is_inter[i], c.is_inter[i]);
        }
        for i in 0..5 {
            diff_update(&mut w, &mut fc.single_ref[i][0], c.single_ref[i][0]);
            diff_update(&mut w, &mut fc.single_ref[i][1], c.single_ref[i][1]);
        }
        for i in 0..4 {
            tree_update(
                &mut w,
                &INTRA_MODE_TREE,
                &mut fc.y_mode[i],
                &c.intra_mode[i],
            );
        }
        for i in 0..16 {
            tree_update(
                &mut w,
                &PARTITION_TREE,
                &mut fc.partition[i],
                &c.partition[i],
            );
        }
        let mut br = [[0u32; 2]; 16];
        tree_branches(&MV_JOINT_TREE, &c.mv_joint, &mut br);
        for j in 0..3 {
            mv_update(&mut w, &mut fc.mv_joint[j], br[j]);
        }
        for i in 0..2 {
            mv_update(&mut w, &mut fc.mv_sign[i], c.mv_sign[i]);
            tree_branches(&MV_CLASS_TREE, &c.mv_class[i], &mut br);
            for j in 0..10 {
                mv_update(&mut w, &mut fc.mv_class[i][j], br[j]);
            }
            mv_update(&mut w, &mut fc.mv_class0_bit[i], c.mv_class0_bit[i]);
            for j in 0..10 {
                mv_update(&mut w, &mut fc.mv_bits[i][j], c.mv_bits[i][j]);
            }
        }
        for i in 0..2 {
            for j in 0..2 {
                tree_branches(&MV_FR_TREE, &c.mv_class0_fr[i][j], &mut br);
                for k in 0..3 {
                    mv_update(&mut w, &mut fc.mv_class0_fr[i][j][k], br[k]);
                }
            }
            tree_branches(&MV_FR_TREE, &c.mv_fr[i], &mut br);
            for k in 0..3 {
                mv_update(&mut w, &mut fc.mv_fr[i][k], br[k]);
            }
        }
        if h.allow_hp {
            for i in 0..2 {
                mv_update(&mut w, &mut fc.mv_class0_hp[i], c.mv_class0_hp[i]);
                mv_update(&mut w, &mut fc.mv_hp[i], c.mv_hp[i]);
            }
        }
    }
    w.finish()
}

fn coef_branches(c: &Counts, tx: usize, i: usize, j: usize, k: usize, l: usize) -> [[u32; 2]; 3] {
    let t = c.token[tx][i][j][k][l];
    let m = c.more_coefs[tx][i][j][k][l];
    [[m[0], m[1]], [t[0], t[1] + t[2]], [t[1], t[2]]]
}

fn coef_updates(w: &mut BoolWriter, fc: &mut FrameContext, c: &Counts, tx: usize) {
    let mut saving = 0i64;
    let mut nodes = 0i64;
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..6 {
                let max_l = if k == 0 { 3 } else { 6 };
                for l in 0..max_l {
                    let br = coef_branches(c, tx, i, j, k, l);
                    for (m, b) in br.iter().enumerate() {
                        nodes += 1;
                        if let Some((_, _, s)) = update_choice(*b, fc.coef[tx][i][j][k][l][m]) {
                            saving += s;
                        }
                    }
                }
            }
        }
    }
    let flag_cost = nodes * (cost(false, UPD) as i64) + 256;
    if saving <= flag_cost {
        w.literal(0, 1);
        return;
    }
    w.literal(1, 1);
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..6 {
                let max_l = if k == 0 { 3 } else { 6 };
                for l in 0..max_l {
                    let br = coef_branches(c, tx, i, j, k, l);
                    for (m, b) in br.iter().enumerate() {
                        diff_update(w, &mut fc.coef[tx][i][j][k][l][m], *b);
                    }
                }
            }
        }
    }
}
