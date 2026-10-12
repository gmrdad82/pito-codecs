use super::bool::{BoolWriter, cost, tree_costs};
use super::frame::{MiGrid, MiInfo, Mv, SbCoefs};
use super::mvref::{MvEnv, use_mv_hp};
use super::tables::*;
use crate::vp9::header::Segmentation;
use crate::vp9::probs::{Counts, FrameContext};
use crate::vp9::tile::{single_ref_p1_ctx, single_ref_p2_ctx};

pub type CoefTable<const N: usize> = [[[[[[u32; N]; 6]; 6]; 2]; 2]; 4];

pub struct Costs {
    pub more: Box<CoefTable<2>>,
    pub token: Box<CoefTable<11>>,
    pub skip: [[u32; 2]; 3],
    pub partition: [[u32; 4]; 16],
    pub kf_y: Box<[[[u32; 10]; 10]; 10]>,
    pub kf_uv: [[u32; 10]; 10],
    pub y_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub inter_mode: [[u32; 4]; 7],
    pub is_inter: [[u32; 2]; 4],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub mv_joint: [u32; 4],
    pub mv: [MvCompCosts; 2],
    pub tx: [[[u32; 4]; 2]; 4],
}

pub const MV_MAX: i32 = (1 << 14) - 1;

pub struct MvCompCosts {
    pub hp: Vec<u32>,
    pub lp: Vec<u32>,
}

impl MvCompCosts {
    fn new(fc: &FrameContext, comp: usize) -> Self {
        let mut class = [0u32; 11];
        tree_costs(&MV_CLASS_TREE, &fc.mv_class[comp], &mut class);
        let mut c0_fr = [[0u32; 4]; 2];
        for (i, c) in c0_fr.iter_mut().enumerate() {
            tree_costs(&MV_FR_TREE, &fc.mv_class0_fr[comp][i], c);
        }
        let mut fr = [0u32; 4];
        tree_costs(&MV_FR_TREE, &fc.mv_fr[comp], &mut fr);
        let n = (2 * MV_MAX + 1) as usize;
        let mut hp = vec![0u32; n];
        let mut lp = vec![0u32; n];
        for mag in 0..MV_MAX as usize {
            let (c, d) = mv_class(mag);
            let int = d >> 3;
            let f = (d >> 1) & 3;
            let e = d & 1;
            let mut base = class[c];
            let (hp_cost, f_cost) = if c == 0 {
                base += cost(int == 1, fc.mv_class0_bit[comp]);
                (cost(e == 1, fc.mv_class0_hp[comp]), c0_fr[int][f])
            } else {
                for i in 0..c {
                    base += cost((int >> i) & 1 == 1, fc.mv_bits[comp][i]);
                }
                (cost(e == 1, fc.mv_hp[comp]), fr[f])
            };
            let pos = cost(false, fc.mv_sign[comp]);
            let neg = cost(true, fc.mv_sign[comp]);
            let v = mag + 1;
            let h = base + f_cost + hp_cost;
            let l = base + f_cost;
            hp[MV_MAX as usize + v] = h + pos;
            hp[MV_MAX as usize - v] = h + neg;
            lp[MV_MAX as usize + v] = l + pos;
            lp[MV_MAX as usize - v] = l + neg;
        }
        MvCompCosts { hp, lp }
    }

    #[inline]
    pub fn get(&self, v: i32, use_hp: bool) -> u32 {
        let i = (v.clamp(-MV_MAX, MV_MAX) + MV_MAX) as usize;
        if use_hp { self.hp[i] } else { self.lp[i] }
    }
}

pub fn mv_class(mag: usize) -> (usize, usize) {
    if mag < 16 {
        return (0, mag);
    }
    let mut c = 1;
    while c < 10 && mag >= 2 << (c + 3) {
        c += 1;
    }
    (c, mag - (2 << (c + 2)))
}

pub fn mv_joint(diff: Mv) -> usize {
    ((diff.row != 0) as usize) << 1 | (diff.col != 0) as usize
}

impl Costs {
    pub fn new(fc: &FrameContext, key: bool) -> Costs {
        let pareto = pareto_full();
        let mut more = Box::new([[[[[[0u32; 2]; 6]; 6]; 2]; 2]; 4]);
        let mut token = Box::new([[[[[[0u32; 11]; 6]; 6]; 2]; 2]; 4]);
        for t in 0..4 {
            for i in 0..2 {
                for j in 0..2 {
                    for b in 0..6 {
                        for c in 0..6 {
                            let p = fc.coef[t][i][j][b][c];
                            if p[0] == 0 {
                                continue;
                            }
                            more[t][i][j][b][c] = [cost(false, p[0]), cost(true, p[0])];
                            let tk = &mut token[t][i][j][b][c];
                            tk[0] = cost(false, p[1]);
                            tk[1] = cost(true, p[1]) + cost(false, p[2]);
                            let pp = &pareto[p[2].max(1) as usize - 1];
                            let big = cost(true, p[1]) + cost(true, p[2]);
                            for (k, v) in tk.iter_mut().enumerate().skip(2) {
                                *v = big + pareto_cost(pp, k);
                            }
                        }
                    }
                }
            }
        }
        let mut skip = [[0u32; 2]; 3];
        for (i, s) in skip.iter_mut().enumerate() {
            *s = [cost(false, fc.skip[i]), cost(true, fc.skip[i])];
        }
        let mut partition = [[0u32; 4]; 16];
        for (i, p) in partition.iter_mut().enumerate() {
            let probs: &[u8] = if key {
                &KF_PARTITION_PROBS[i * 3..i * 3 + 3]
            } else {
                &fc.partition[i]
            };
            tree_costs(&PARTITION_TREE, probs, p);
        }
        let mut kf_y = Box::new([[[0u32; 10]; 10]; 10]);
        for a in 0..10 {
            for l in 0..10 {
                let o = (a * 10 + l) * 9;
                tree_costs(
                    &INTRA_MODE_TREE,
                    &KF_Y_MODE_PROBS[o..o + 9],
                    &mut kf_y[a][l],
                );
            }
        }
        let mut kf_uv = [[0u32; 10]; 10];
        for (y, u) in kf_uv.iter_mut().enumerate() {
            tree_costs(&INTRA_MODE_TREE, &KF_UV_MODE_PROBS[y * 9..y * 9 + 9], u);
        }
        let mut y_mode = [[0u32; 10]; 4];
        for (i, y) in y_mode.iter_mut().enumerate() {
            tree_costs(&INTRA_MODE_TREE, &fc.y_mode[i], y);
        }
        let mut uv_mode = [[0u32; 10]; 10];
        for (i, u) in uv_mode.iter_mut().enumerate() {
            tree_costs(&INTRA_MODE_TREE, &fc.uv_mode[i], u);
        }
        let mut inter_mode = [[0u32; 4]; 7];
        for (i, m) in inter_mode.iter_mut().enumerate() {
            tree_costs(&INTER_MODE_TREE, &fc.inter_mode[i], m);
        }
        let mut is_inter = [[0u32; 2]; 4];
        for (i, v) in is_inter.iter_mut().enumerate() {
            *v = [cost(false, fc.is_inter[i]), cost(true, fc.is_inter[i])];
        }
        let mut single_ref = [[[0u32; 2]; 2]; 5];
        for i in 0..5 {
            for j in 0..2 {
                single_ref[i][j] = [
                    cost(false, fc.single_ref[i][j]),
                    cost(true, fc.single_ref[i][j]),
                ];
            }
        }
        let mut mv_joint = [0u32; 4];
        tree_costs(&MV_JOINT_TREE, &fc.mv_joint, &mut mv_joint);
        let mut tx = [[[0u32; 4]; 2]; 4];
        for (max, t) in tx.iter_mut().enumerate().skip(1) {
            let tree: &[i8] = match max {
                3 => &TX_SIZE_32_TREE,
                2 => &TX_SIZE_16_TREE,
                _ => &TX_SIZE_8_TREE,
            };
            for (ctx, c) in t.iter_mut().enumerate() {
                tree_costs(tree, &fc.tx[max][ctx][..max], c);
            }
        }
        Costs {
            more,
            token,
            skip,
            partition,
            kf_y,
            kf_uv,
            y_mode,
            uv_mode,
            inter_mode,
            is_inter,
            single_ref,
            mv_joint,
            mv: [MvCompCosts::new(fc, 0), MvCompCosts::new(fc, 1)],
            tx,
        }
    }

    pub fn mv(&self, diff: Mv, use_hp: bool) -> u32 {
        let mut c = self.mv_joint[mv_joint(diff)];
        if diff.row != 0 {
            c += self.mv[0].get(diff.row as i32, use_hp);
        }
        if diff.col != 0 {
            c += self.mv[1].get(diff.col as i32, use_hp);
        }
        c
    }
}

fn pareto_cost(pp: &[u8; 8], token: usize) -> u32 {
    match token {
        2 => cost(false, pp[0]) + cost(false, pp[1]),
        3 => cost(false, pp[0]) + cost(true, pp[1]) + cost(false, pp[2]),
        4 => cost(false, pp[0]) + cost(true, pp[1]) + cost(true, pp[2]),
        5 => cost(true, pp[0]) + cost(false, pp[3]) + cost(false, pp[4]),
        6 => cost(true, pp[0]) + cost(false, pp[3]) + cost(true, pp[4]),
        7 => cost(true, pp[0]) + cost(true, pp[3]) + cost(false, pp[5]) + cost(false, pp[6]),
        8 => cost(true, pp[0]) + cost(true, pp[3]) + cost(false, pp[5]) + cost(true, pp[6]),
        9 => cost(true, pp[0]) + cost(true, pp[3]) + cost(true, pp[5]) + cost(false, pp[7]),
        _ => cost(true, pp[0]) + cost(true, pp[3]) + cost(true, pp[5]) + cost(true, pp[7]),
    }
}

pub trait Sink {
    fn put(&mut self, bit: bool, prob: u8);
    fn tree(&mut self, tree: &[i8], probs: &[u8], value: u8);
}

impl Sink for BoolWriter {
    #[inline(always)]
    fn put(&mut self, bit: bool, prob: u8) {
        BoolWriter::put(self, bit, prob)
    }

    fn tree(&mut self, tree: &[i8], probs: &[u8], value: u8) {
        BoolWriter::tree(self, tree, probs, value)
    }
}

pub struct NullSink;

impl Sink for NullSink {
    #[inline(always)]
    fn put(&mut self, _: bool, _: u8) {}

    fn tree(&mut self, _: &[i8], _: &[u8], _: u8) {}
}

pub struct FrameSyntax<'a> {
    pub intra: bool,
    pub lossless: bool,
    pub tx_mode: u8,
    pub interp_filter: u8,
    pub allow_hp: bool,
    pub sign_bias: [bool; 4],
    pub seg: &'a Segmentation,
    pub mi: &'a MiGrid,
    pub uv: &'a [u8],
    pub prev_mi: Option<&'a [MiInfo]>,
    pub coefs: &'a [SbCoefs],
    pub sb_cols: usize,
    pub fc: &'a FrameContext,
}

pub struct TileWriter<'a, S: Sink> {
    f: &'a FrameSyntax<'a>,
    pub w: S,
    pub counts: Counts,
    mi_rows: usize,
    mi_cols: usize,
    col_start: usize,
    col_end: usize,
    above_nonzero: [Vec<u8>; 3],
    left_nonzero: [[u8; 16]; 3],
    above_partition: Vec<u8>,
    left_partition: [u8; 8],
    token_cache: Vec<u8>,
}

impl<'a, S: Sink> TileWriter<'a, S> {
    pub fn new(f: &'a FrameSyntax<'a>, w: S, col_start: usize, col_end: usize) -> Self {
        let an = f.sb_cols * 16 + 16;
        TileWriter {
            f,
            w,
            counts: Counts::default(),
            mi_rows: f.mi.rows,
            mi_cols: f.mi.cols,
            col_start,
            col_end,
            above_nonzero: [vec![0; an], vec![0; an], vec![0; an]],
            left_nonzero: [[0; 16]; 3],
            above_partition: vec![0; f.sb_cols * 8 + 8],
            left_partition: [0; 8],
            token_cache: vec![0; 1024],
        }
    }

    pub fn tile(&mut self, row_start: usize, row_end: usize) {
        for r in (row_start..row_end).step_by(8) {
            self.left_nonzero = [[0; 16]; 3];
            self.left_partition = [0; 8];
            for c in (self.col_start..self.col_end).step_by(8) {
                self.partition(r, c, BLOCK_64X64);
            }
        }
    }

    fn mi(&self, r: usize, c: usize) -> &MiInfo {
        self.f.mi.at(r, c)
    }

    fn partition(&mut self, r: usize, c: usize, bsize: u8) {
        if r >= self.mi_rows || c >= self.mi_cols {
            return;
        }
        let num8x8 = NUM_8X8_WIDE[bsize as usize] as usize;
        let half = num8x8 >> 1;
        let has_rows = (r + half) < self.mi_rows;
        let has_cols = (c + half) < self.mi_cols;
        let bsl = MI_WIDTH_LOG2[bsize as usize] as usize;
        let boffset = 3 - bsl;
        let mut above = 0u8;
        let mut left = 0u8;
        for i in 0..num8x8 {
            above |= self.above_partition[c + i];
            left |= self.left_partition[(r + i) & 7];
        }
        let above = (above >> boffset) & 1;
        let left = (left >> boffset) & 1;
        let ctx = bsl * 4 + left as usize * 2 + above as usize;
        let sb = self.mi(r, c).sb_size;
        let partition = if sb == bsize {
            0
        } else if sb == SUBSIZE_LOOKUP[1][bsize as usize] {
            1
        } else if sb == SUBSIZE_LOOKUP[2][bsize as usize] {
            2
        } else {
            3
        };
        let fc = self.f.fc;
        let probs: &[u8] = if self.f.intra {
            &KF_PARTITION_PROBS[ctx * 3..ctx * 3 + 3]
        } else {
            &fc.partition[ctx]
        };
        if has_rows && has_cols {
            self.w.tree(&PARTITION_TREE, probs, partition);
        } else if has_cols {
            self.w.put(partition == 3, probs[1]);
        } else if has_rows {
            self.w.put(partition == 3, probs[2]);
        }
        self.counts.partition[ctx][partition as usize] += 1;
        let subsize = SUBSIZE_LOOKUP[partition as usize][bsize as usize];
        if subsize < BLOCK_8X8 || partition == 0 {
            self.block(r, c, subsize);
        } else if partition == 1 {
            self.block(r, c, subsize);
            if has_rows {
                self.block(r + half, c, subsize);
            }
        } else if partition == 2 {
            self.block(r, c, subsize);
            if has_cols {
                self.block(r, c + half, subsize);
            }
        } else {
            self.partition(r, c, subsize);
            self.partition(r, c + half, subsize);
            self.partition(r + half, c, subsize);
            self.partition(r + half, c + half, subsize);
        }
        if bsize == BLOCK_8X8 || partition != 3 {
            let a = 15 >> B_WIDTH_LOG2[subsize as usize];
            let l = 15 >> B_HEIGHT_LOG2[subsize as usize];
            self.above_partition[c..c + num8x8].fill(a);
            for i in 0..num8x8 {
                self.left_partition[(r + i) & 7] = l;
            }
        }
    }

    fn block(&mut self, r: usize, c: usize, size: u8) {
        let m = *self.mi(r, c);
        let avail_u = r > 0;
        let avail_l = c > self.col_start;
        let above = avail_u.then(|| *self.mi(r - 1, c));
        let left = avail_l.then(|| *self.mi(r, c - 1));
        if self.f.intra {
            self.intra_frame_mode_info(&m, r, c, size, above.as_ref(), left.as_ref());
        } else {
            self.inter_frame_mode_info(&m, r, c, size, above.as_ref(), left.as_ref());
        }
        self.residual(&m, r, c, size);
    }

    fn seg_id(&mut self, m: &MiInfo) {
        let seg = self.f.seg;
        if seg.enabled && seg.update_map {
            self.w.tree(&SEGMENT_TREE, &seg.tree_probs, m.seg_id);
        }
    }

    fn skip(&mut self, m: &MiInfo, above: Option<&MiInfo>, left: Option<&MiInfo>) {
        if self.f.seg.feature_active(m.seg_id, SEG_LVL_SKIP) {
            return;
        }
        let ctx = above.map_or(0, |a| a.skip as usize) + left.map_or(0, |l| l.skip as usize);
        self.w.put(m.skip, self.f.fc.skip[ctx]);
        self.counts.skip[ctx][m.skip as usize] += 1;
    }

    fn tx_size(
        &mut self,
        m: &MiInfo,
        size: u8,
        allow: bool,
        above: Option<&MiInfo>,
        left: Option<&MiInfo>,
    ) {
        let max_tx = MAX_TXSIZE[size as usize];
        if !(allow && self.f.tx_mode == TX_MODE_SELECT && size >= BLOCK_8X8) {
            return;
        }
        let mut a = above.map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
        let mut l = left.map_or(max_tx, |m| if m.skip { max_tx } else { m.tx_size });
        if left.is_none() {
            l = a;
        }
        if above.is_none() {
            a = l;
        }
        let ctx = ((a + l) > max_tx) as usize;
        let tree: &[i8] = match max_tx {
            3 => &TX_SIZE_32_TREE,
            2 => &TX_SIZE_16_TREE,
            _ => &TX_SIZE_8_TREE,
        };
        self.w
            .tree(tree, &self.f.fc.tx[max_tx as usize][ctx], m.tx_size);
        self.counts.tx[max_tx as usize][ctx][m.tx_size as usize] += 1;
    }

    fn intra_frame_mode_info(
        &mut self,
        m: &MiInfo,
        r: usize,
        c: usize,
        size: u8,
        above: Option<&MiInfo>,
        left: Option<&MiInfo>,
    ) {
        self.seg_id(m);
        self.skip(m, above, left);
        self.tx_size(m, size, true, above, left);
        if size >= BLOCK_8X8 {
            let a = above.map_or(DC_PRED, |m| m.sub_modes[2]);
            let l = left.map_or(DC_PRED, |m| m.sub_modes[1]);
            let o = (a as usize * 10 + l as usize) * 9;
            self.w
                .tree(&INTRA_MODE_TREE, &KF_Y_MODE_PROBS[o..o + 9], m.y_mode);
        } else {
            let n4w = NUM_4X4_WIDE[size as usize] as usize;
            let n4h = NUM_4X4_HIGH[size as usize] as usize;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let a = if idy > 0 {
                        m.sub_modes[idx]
                    } else {
                        above.map_or(DC_PRED, |a| a.sub_modes[2 + idx])
                    };
                    let l = if idx > 0 {
                        m.sub_modes[idy * 2]
                    } else {
                        left.map_or(DC_PRED, |l| l.sub_modes[1 + idy * 2])
                    };
                    let o = (a as usize * 10 + l as usize) * 9;
                    self.w.tree(
                        &INTRA_MODE_TREE,
                        &KF_Y_MODE_PROBS[o..o + 9],
                        m.sub_modes[idy * 2 + idx],
                    );
                    idx += n4w;
                }
                idy += n4h;
            }
        }
        let uv = self.uv_mode_of(r, c);
        let o = m.y_mode as usize * 9;
        self.w
            .tree(&INTRA_MODE_TREE, &KF_UV_MODE_PROBS[o..o + 9], uv);
    }

    fn uv_mode_of(&self, r: usize, c: usize) -> u8 {
        self.f.uv[r * self.mi_cols + c]
    }

    fn inter_frame_mode_info(
        &mut self,
        m: &MiInfo,
        r: usize,
        c: usize,
        size: u8,
        above: Option<&MiInfo>,
        left: Option<&MiInfo>,
    ) {
        let left_ref = left.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let above_ref = above.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let left_intra = left_ref[0] <= INTRA_FRAME;
        let above_intra = above_ref[0] <= INTRA_FRAME;
        self.seg_id(m);
        self.skip(m, above, left);
        let is_inter = m.ref_frame[0] > INTRA_FRAME;
        if !self.f.seg.feature_active(m.seg_id, SEG_LVL_REF_FRAME) {
            let ctx = match (above.is_some(), left.is_some()) {
                (true, true) => {
                    if left_intra && above_intra {
                        3
                    } else {
                        (left_intra || above_intra) as usize
                    }
                }
                (true, false) => 2 * above_intra as usize,
                (false, true) => 2 * left_intra as usize,
                _ => 0,
            };
            self.w.put(is_inter, self.f.fc.is_inter[ctx]);
            self.counts.is_inter[ctx][is_inter as usize] += 1;
        }
        self.tx_size(m, size, !m.skip || !is_inter, above, left);
        if is_inter {
            self.inter_block(m, r, c, size, above, left, left_ref, above_ref);
        } else {
            self.intra_block(m, r, c, size);
        }
    }

    fn intra_block(&mut self, m: &MiInfo, r: usize, c: usize, size: u8) {
        let fc = self.f.fc;
        if size >= BLOCK_8X8 {
            let ctx = SIZE_GROUP[size as usize] as usize;
            self.w.tree(&INTRA_MODE_TREE, &fc.y_mode[ctx], m.y_mode);
            self.counts.intra_mode[ctx][m.y_mode as usize] += 1;
        } else {
            let n4w = NUM_4X4_WIDE[size as usize] as usize;
            let n4h = NUM_4X4_HIGH[size as usize] as usize;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let mode = m.sub_modes[idy * 2 + idx];
                    self.w.tree(&INTRA_MODE_TREE, &fc.y_mode[0], mode);
                    self.counts.intra_mode[0][mode as usize] += 1;
                    idx += n4w;
                }
                idy += n4h;
            }
        }
        let uv = self.uv_mode_of(r, c);
        self.w
            .tree(&INTRA_MODE_TREE, &fc.uv_mode[m.y_mode as usize], uv);
        self.counts.uv_mode[m.y_mode as usize][uv as usize] += 1;
    }

    #[allow(clippy::too_many_arguments)]
    fn inter_block(
        &mut self,
        m: &MiInfo,
        r: usize,
        c: usize,
        size: u8,
        above: Option<&MiInfo>,
        left: Option<&MiInfo>,
        left_ref: [i8; 2],
        above_ref: [i8; 2],
    ) {
        let fc = self.f.fc;
        let rf = m.ref_frame[0];
        if !self.f.seg.feature_active(m.seg_id, SEG_LVL_REF_FRAME) {
            let ctx = single_ref_p1_ctx(above.is_some(), left.is_some(), above_ref, left_ref);
            let p1 = rf != LAST_FRAME;
            self.w.put(p1, fc.single_ref[ctx][0]);
            self.counts.single_ref[ctx][0][p1 as usize] += 1;
            if p1 {
                let ctx2 = single_ref_p2_ctx(above.is_some(), left.is_some(), above_ref, left_ref);
                let p2 = rf == ALTREF_FRAME;
                self.w.put(p2, fc.single_ref[ctx2][1]);
                self.counts.single_ref[ctx2][1][p2 as usize] += 1;
            }
        }
        let env = self.mv_env();
        let refs = env.refs(r, c, size, rf);
        if !self.f.seg.feature_active(m.seg_id, SEG_LVL_SKIP) && size >= BLOCK_8X8 {
            let ctx = refs.mode_context.min(6) as usize;
            let mode = m.y_mode - NEARESTMV;
            self.w.tree(&INTER_MODE_TREE, &fc.inter_mode[ctx], mode);
            self.counts.inter_mode[ctx][mode as usize] += 1;
        }
        if self.f.interp_filter == SWITCHABLE {
            let lf = if left.is_some() && left_ref[0] > INTRA_FRAME {
                left.map_or(3, |l| l.interp_filter)
            } else {
                3
            };
            let af = if above.is_some() && above_ref[0] > INTRA_FRAME {
                above.map_or(3, |a| a.interp_filter)
            } else {
                3
            };
            let ctx = if lf == af {
                lf
            } else if lf == 3 && af != 3 {
                af
            } else if lf != 3 && af == 3 {
                lf
            } else {
                3
            } as usize;
            self.w
                .tree(&INTERP_FILTER_TREE, &fc.interp_filter[ctx], m.interp_filter);
            self.counts.interp_filter[ctx][m.interp_filter as usize] += 1;
        }
        if m.y_mode == NEWMV {
            let mv = m.mv[0][3];
            let best = refs.best;
            let use_hp = self.f.allow_hp && use_mv_hp(best);
            let diff = Mv::new(
                mv.row as i32 - best.row as i32,
                mv.col as i32 - best.col as i32,
            );
            self.write_mv(diff, use_hp);
        }
    }

    fn mv_env(&self) -> MvEnv<'a, impl Fn(usize, usize) -> MiInfo + 'a> {
        let mi = self.f.mi;
        MvEnv {
            mi: move |r: usize, c: usize| *mi.at(r, c),
            prev: self.f.prev_mi,
            mi_rows: self.mi_rows,
            mi_cols: self.mi_cols,
            tile_start: self.col_start,
            tile_end: self.col_end,
            sign_bias: self.f.sign_bias,
            allow_hp: self.f.allow_hp,
        }
    }

    fn write_mv(&mut self, diff: Mv, use_hp: bool) {
        let fc = self.f.fc;
        let joint = mv_joint(diff);
        self.w.tree(&MV_JOINT_TREE, &fc.mv_joint, joint as u8);
        self.counts.mv_joint[joint] += 1;
        if joint >> 1 == 1 {
            self.write_mv_comp(diff.row as i32, 0, use_hp);
        }
        if joint & 1 == 1 {
            self.write_mv_comp(diff.col as i32, 1, use_hp);
        }
    }

    fn write_mv_comp(&mut self, v: i32, comp: usize, use_hp: bool) {
        let fc = self.f.fc;
        let sign = v < 0;
        let mag = v.unsigned_abs() as usize - 1;
        let (class, d) = mv_class(mag);
        let int = d >> 3;
        let fr = (d >> 1) & 3;
        let hp = d & 1;
        self.w.put(sign, fc.mv_sign[comp]);
        self.w.tree(&MV_CLASS_TREE, &fc.mv_class[comp], class as u8);
        let counts = &mut self.counts;
        if class == 0 {
            self.w.put(int == 1, fc.mv_class0_bit[comp]);
            self.w
                .tree(&MV_FR_TREE, &fc.mv_class0_fr[comp][int], fr as u8);
            if use_hp {
                self.w.put(hp == 1, fc.mv_class0_hp[comp]);
            }
            counts.mv_class0_bit[comp][int] += 1;
            counts.mv_class0_fr[comp][int][fr] += 1;
            counts.mv_class0_hp[comp][if use_hp { hp } else { 1 }] += 1;
        } else {
            for i in 0..class {
                let bit = (int >> i) & 1;
                self.w.put(bit == 1, fc.mv_bits[comp][i]);
                counts.mv_bits[comp][i][bit] += 1;
            }
            self.w.tree(&MV_FR_TREE, &fc.mv_fr[comp], fr as u8);
            if use_hp {
                self.w.put(hp == 1, fc.mv_hp[comp]);
            }
            counts.mv_fr[comp][fr] += 1;
            counts.mv_hp[comp][if use_hp { hp } else { 1 }] += 1;
        }
        counts.mv_sign[comp][sign as usize] += 1;
        counts.mv_class[comp][class] += 1;
    }

    fn residual(&mut self, m: &MiInfo, r: usize, c: usize, size: u8) {
        let bsize = size.max(BLOCK_8X8) as usize;
        let is_inter = m.ref_frame[0] > INTRA_FRAME;
        let uv_tx = if size < BLOCK_8X8 {
            0
        } else {
            m.tx_size
                .min(MAX_TXSIZE[SS_SIZE_LOOKUP[size as usize][1][1] as usize])
        };
        let sb = (r >> 3) * self.f.sb_cols + (c >> 3);
        let coefs = &self.f.coefs[sb];
        for plane in 0..3 {
            let tx_sz = if plane > 0 { uv_tx } else { m.tx_size };
            let step = 1usize << tx_sz;
            let s = (plane > 0) as usize;
            let plane_sz = if plane > 0 {
                SS_SIZE_LOOKUP[bsize][1][1] as usize
            } else {
                bsize
            };
            let n4w = NUM_4X4_WIDE[plane_sz] as usize;
            let n4h = NUM_4X4_HIGH[plane_sz] as usize;
            let base_x4 = (c * 2) >> s;
            let base_y4 = (r * 2) >> s;
            let max_x4 = (2 * self.mi_cols) >> s;
            let max_y4 = (2 * self.mi_rows) >> s;
            let sb_x4 = ((c >> 3) * 16) >> s;
            let sb_y4 = ((r >> 3) * 16) >> s;
            let mut block_idx = 0;
            for y in (0..n4h).step_by(step) {
                for x in (0..n4w).step_by(step) {
                    let x4 = base_x4 + x;
                    let y4 = base_y4 + y;
                    let mut nonzero = false;
                    if !m.skip && x4 < max_x4 && y4 < max_y4 {
                        let tx_type = tx_type_of(m, plane, tx_sz, block_idx, size, self.f.lossless);
                        let t = coefs.get(plane, x4 - sb_x4, y4 - sb_y4);
                        let levels = coefs.levels(t);
                        let mut above = 0u8;
                        let mut left = 0u8;
                        for i in 0..step {
                            if x4 + i < max_x4 {
                                above |= self.above_nonzero[plane][x4 + i];
                            }
                            if y4 + i < max_y4 {
                                left |= self.left_nonzero[plane][(y4 + i) & (15 >> s)];
                            }
                        }
                        let ctx0 = (above + left) as usize;
                        self.tokens(plane, tx_sz, tx_type, is_inter, ctx0, levels);
                        nonzero = !levels.is_empty();
                    }
                    let an = &mut self.above_nonzero[plane];
                    let e = (x4 + step).min(an.len());
                    an[x4.min(e)..e].fill(nonzero as u8);
                    let mask = 15 >> s;
                    for i in 0..step {
                        self.left_nonzero[plane][(y4 + i) & mask] = nonzero as u8;
                    }
                    block_idx += 1;
                }
            }
        }
    }

    fn tokens(
        &mut self,
        plane: usize,
        tx_size: u8,
        tx_type: u8,
        is_inter: bool,
        ctx0: usize,
        levels: &[i16],
    ) {
        let (t, i, j) = (tx_size as usize, (plane > 0) as usize, is_inter as usize);
        let probs = &self.f.fc.coef[t][i][j];
        let sc = scan(tx_size, tx_type);
        let nb = neighbors(tx_size, tx_type);
        let bands = bands(tx_size);
        let pareto = pareto_full();
        let tc = &mut self.token_cache;
        let counts = &mut self.counts;
        let mut check_eob = true;
        let n = levels.len();
        let seg_eob = 16usize << (2 * tx_size);
        let mut c = 0;
        loop {
            if c == n && (c == seg_eob || !check_eob) {
                break;
            }
            let ctx = if c == 0 {
                ctx0
            } else {
                let (a, b) = nb[c];
                ((1 + tc[a as usize] + tc[b as usize]) >> 1) as usize
            };
            let band = bands[c] as usize;
            let p = &probs[band][ctx];
            if c == n {
                self.w.put(false, p[0]);
                counts.more_coefs[t][i][j][band][ctx][0] += 1;
                break;
            }
            if check_eob {
                self.w.put(true, p[0]);
                counts.more_coefs[t][i][j][band][ctx][1] += 1;
            }
            let pos = sc[c] as usize;
            let l = levels[c];
            let v = l.unsigned_abs() as u32;
            if v == 0 {
                self.w.put(false, p[1]);
                counts.token[t][i][j][band][ctx][0] += 1;
                tc[pos] = 0;
                check_eob = false;
                c += 1;
                continue;
            }
            self.w.put(true, p[1]);
            check_eob = true;
            let tk = super::tx::token(v);
            if tk == 1 {
                self.w.put(false, p[2]);
                counts.token[t][i][j][band][ctx][1] += 1;
            } else {
                self.w.put(true, p[2]);
                counts.token[t][i][j][band][ctx][2] += 1;
                let pp = &pareto[p[2].max(1) as usize - 1];
                match tk {
                    2..=4 => {
                        self.w.put(false, pp[0]);
                        self.w.put(tk > 2, pp[1]);
                        if tk > 2 {
                            self.w.put(tk > 3, pp[2]);
                        }
                    }
                    5 | 6 => {
                        self.w.put(true, pp[0]);
                        self.w.put(false, pp[3]);
                        self.w.put(tk == 6, pp[4]);
                    }
                    7 | 8 => {
                        self.w.put(true, pp[0]);
                        self.w.put(true, pp[3]);
                        self.w.put(false, pp[5]);
                        self.w.put(tk == 8, pp[6]);
                    }
                    _ => {
                        self.w.put(true, pp[0]);
                        self.w.put(true, pp[3]);
                        self.w.put(true, pp[5]);
                        self.w.put(tk == 10, pp[7]);
                    }
                }
                let (cat, ne, base) = EXTRA_BITS[tk];
                let cp = CAT_PROBS[cat as usize];
                let e = v - base as u32;
                for b in 0..ne as usize {
                    self.w.put((e >> (ne as usize - 1 - b)) & 1 == 1, cp[b]);
                }
            }
            tc[pos] = ENERGY_CLASS[tk];
            self.w.put(l < 0, 128);
            c += 1;
        }
    }
}

pub fn tx_type_of(
    m: &MiInfo,
    plane: usize,
    tx_size: u8,
    block_idx: usize,
    size: u8,
    lossless: bool,
) -> u8 {
    let is_inter = m.ref_frame[0] > INTRA_FRAME;
    if plane > 0 || tx_size == TX_32X32 {
        DCT_DCT
    } else if tx_size == TX_4X4 {
        if lossless || is_inter {
            DCT_DCT
        } else {
            MODE2TXFM[if size < BLOCK_8X8 {
                m.sub_modes[block_idx]
            } else {
                m.y_mode
            } as usize]
        }
    } else {
        MODE2TXFM[m.y_mode as usize]
    }
}
