use super::entropy::{Costs, single_ref_p1_ctx};
use super::frame::{MiInfo, Mv, PAD, Picture, SbCoefs, SharedGrid, TxRef};
use super::mvref::{MvEnv, RefMvs, use_mv_hp};
use super::pred::inter::{self, BlockPos};
use super::pred::intra::{self, EdgeFlags};
use super::pred::mode::{
    self, Decision, InterChoice, InterRef, InterScratch, IntraBlock, IntraChoice, IntraScratch,
};
use super::pred::motion::Subpel;
use super::pred::mvcost::MvCost;
use super::pred::{self, Plane};
use super::tables::*;
use super::tx::{self, Quant};

pub struct Speed {
    pub intra_modes: &'static [u8],
    pub subpel: Subpel,
    pub range: i32,
    pub intra_margin: u64,
    pub trellis: bool,
}

pub struct FrameEnv<'a> {
    pub src: &'a Picture,
    pub refs: [Option<&'a Picture>; 3],
    pub key: bool,
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub q: [[Quant; 2]; 8],
    pub lambda: [u32; 8],
    pub seg_map: Option<&'a [u8]>,
    pub tx_mode: u8,
    pub allow_hp: bool,
    pub costs: &'a Costs,
    pub mv_cost: &'a MvCost,
    pub speed: &'a Speed,
    pub prev_mi: Option<&'a [MiInfo]>,
    pub sign_bias: [bool; 4],
}

pub struct Shared {
    pub recon: [SharedGrid<u8>; 3],
    pub origin: [usize; 3],
    pub mi: SharedGrid<MiInfo>,
    pub uv: SharedGrid<u8>,
}

impl Shared {
    pub fn new(recon: &mut Picture, mi: &mut [MiInfo], uv: &mut [u8], mi_cols: usize) -> Shared {
        let origin = recon.origin;
        let stride = recon.stride;
        let [a, b, c] = &mut recon.planes;
        Shared {
            recon: [
                SharedGrid::new(a, stride[0]),
                SharedGrid::new(b, stride[1]),
                SharedGrid::new(c, stride[2]),
            ],
            origin,
            mi: SharedGrid::new(mi, mi_cols),
            uv: SharedGrid::new(uv, mi_cols),
        }
    }
}

const WIN: [usize; 3] = [65, 33, 33];

struct Win {
    buf: Vec<u8>,
    x0: isize,
    y0: isize,
}

impl Win {
    #[inline(always)]
    fn at(&self, p: usize, x: usize, y: usize) -> usize {
        ((y as isize - self.y0) as usize) * WIN[p] + (x as isize - self.x0) as usize
    }
}

struct Snapshot {
    pix: [Vec<u8>; 3],
    mi: Vec<MiInfo>,
    uv: Vec<u8>,
    luma: [TxRef; 256],
    chroma: [[TxRef; 64]; 2],
    nz: [[u8; 256]; 3],
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot {
            pix: [vec![0; 64 * 64], vec![0; 32 * 32], vec![0; 32 * 32]],
            mi: vec![MiInfo::default(); 64],
            uv: vec![0; 64],
            luma: [TxRef::default(); 256],
            chroma: [[TxRef::default(); 64]; 2],
            nz: [[0; 256]; 3],
        }
    }
}

pub struct Workspace {
    win: [Win; 3],
    nz: [[u8; 256]; 3],
    levels: [i16; 1024],
    snaps: Vec<Snapshot>,
    intra: [IntraScratch; 2],
    inter: InterScratch,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            win: std::array::from_fn(|p| Win {
                buf: vec![0; WIN[p] * WIN[p]],
                x0: 0,
                y0: 0,
            }),
            nz: [[0; 256]; 3],
            levels: [0; 1024],
            snaps: (0..4).map(|_| Snapshot::default()).collect(),
            intra: Default::default(),
            inter: InterScratch::default(),
        }
    }
}

#[derive(Clone, Copy)]
struct Choice {
    y_mode: u8,
    uv_mode: u8,
    mv: Mv,
    inter: bool,
}

pub struct SbEncoder<'a> {
    env: &'a FrameEnv<'a>,
    sh: &'a Shared,
    ws: &'a mut Workspace,
    coefs: &'a mut SbCoefs,
    tile_start: usize,
    tile_end: usize,
    sb_r: usize,
    sb_c: usize,
    seg: u8,
}

pub fn rdcost(lambda: u32, rate: u32, dist: u64) -> u64 {
    (dist << 8) + lambda as u64 * rate as u64
}

impl<'a> SbEncoder<'a> {
    pub fn encode(
        env: &'a FrameEnv<'a>,
        sh: &'a Shared,
        ws: &'a mut Workspace,
        coefs: &'a mut SbCoefs,
        tile: (usize, usize),
        sb_r: usize,
        sb_c: usize,
    ) {
        coefs.clear();
        let mut e = SbEncoder {
            env,
            sh,
            ws,
            coefs,
            tile_start: tile.0,
            tile_end: tile.1,
            sb_r,
            sb_c,
            seg: 0,
        };
        e.load();
        e.partition(sb_r * 8, sb_c * 8, BLOCK_64X64, 0);
        e.store();
    }

    fn load(&mut self) {
        for p in 0..3 {
            let s = (p > 0) as usize;
            let (x, y) = ((self.sb_c * 64) >> s, (self.sb_r * 64) >> s);
            let w = &mut self.ws.win[p];
            w.x0 = x as isize - 1;
            w.y0 = y as isize - 1;
            let stride = self.sh.recon[p].cols;
            let skip = (self.sb_c * 8 == self.tile_start) as usize;
            for i in 0..WIN[p] {
                let at = self.sh.origin[p] - stride - 1 + (y + i) * stride + x + skip;
                self.sh.recon[p].read(at, &mut w.buf[i * WIN[p] + skip..(i + 1) * WIN[p]]);
            }
        }
        self.ws.nz = [[0; 256]; 3];
    }

    fn store(&mut self) {
        for p in 0..3 {
            let s = (p > 0) as usize;
            let n = 64 >> s;
            let (x, y) = ((self.sb_c * 64) >> s, (self.sb_r * 64) >> s);
            let w = &self.ws.win[p];
            let stride = self.sh.recon[p].cols;
            for i in 0..n {
                let at = self.sh.origin[p] + (y + i) * stride + x;
                let o = (i + 1) * WIN[p] + 1;
                self.sh.recon[p].write(at, &w.buf[o..o + n]);
            }
        }
    }

    fn mi(&self, r: usize, c: usize) -> MiInfo {
        self.sh.mi.get(r, c)
    }

    fn partition_ctx(&self, r: usize, c: usize, bsize: u8) -> usize {
        let num8x8 = NUM_8X8_WIDE[bsize as usize] as usize;
        let bsl = MI_WIDTH_LOG2[bsize as usize] as usize;
        let boffset = 3 - bsl;
        let mut above = 0u8;
        let mut left = 0u8;
        if r > 0 {
            for i in 0..num8x8 {
                if c + i < self.env.mi_cols {
                    above |= 15 >> B_WIDTH_LOG2[self.mi(r - 1, c + i).sb_size as usize];
                }
            }
        }
        if c > self.tile_start {
            for i in 0..num8x8 {
                if r + i < self.env.mi_rows {
                    left |= 15 >> B_HEIGHT_LOG2[self.mi(r + i, c - 1).sb_size as usize];
                }
            }
        }
        bsl * 4 + ((left >> boffset) & 1) as usize * 2 + ((above >> boffset) & 1) as usize
    }

    fn partition(&mut self, r: usize, c: usize, bsize: u8, depth: usize) -> u64 {
        let env = self.env;
        if r >= env.mi_rows || c >= env.mi_cols {
            return 0;
        }
        let half = NUM_8X8_WIDE[bsize as usize] as usize >> 1;
        let has_rows = (r + half) < env.mi_rows;
        let has_cols = (c + half) < env.mi_cols;
        let ctx = self.partition_ctx(r, c, bsize);
        let pc = &env.costs.partition[ctx];
        self.seg = self.seg_at(r, c);
        let lambda = env.lambda[self.seg as usize];
        let none_ok = has_rows && has_cols;
        let split_ok = bsize > BLOCK_8X8;
        let mut j_none = u64::MAX;
        if none_ok {
            let rate = if bsize > BLOCK_8X8 { pc[0] } else { 0 };
            j_none = self.block(r, c, bsize) + rdcost(lambda, rate, 0);
        }
        if !split_ok {
            return j_none;
        }
        if none_ok {
            self.snapshot(r, c, bsize, depth);
        }
        let sub = SUBSIZE_LOOKUP[3][bsize as usize];
        let split_rate = if none_ok { pc[3] } else { 0 };
        let mut j_split = rdcost(lambda, split_rate, 0);
        for (dr, dc) in [(0, 0), (0, half), (half, 0), (half, half)] {
            j_split += self.partition(r + dr, c + dc, sub, depth + 1);
            if none_ok && j_split >= j_none {
                break;
            }
        }
        if none_ok && j_none <= j_split {
            self.restore(r, c, bsize, depth);
            return j_none;
        }
        j_split
    }

    fn seg_at(&self, r: usize, c: usize) -> u8 {
        self.env.seg_map.map_or(0, |m| m[r * self.env.mi_cols + c])
    }

    fn lambda(&self) -> u32 {
        self.env.lambda[self.seg as usize]
    }

    fn region(&self, r: usize, c: usize, bsize: u8) -> (usize, usize, usize, usize) {
        let n = NUM_8X8_WIDE[bsize as usize] as usize;
        let rows = n.min(self.env.mi_rows - r);
        let cols = n.min(self.env.mi_cols - c);
        (n, rows, cols, n * 8)
    }

    fn snapshot(&mut self, r: usize, c: usize, bsize: u8, depth: usize) {
        let (_, rows, cols, px) = self.region(r, c, bsize);
        let snap = &mut self.ws.snaps[depth];
        for p in 0..3 {
            let s = (p > 0) as usize;
            let (x, y, n) = ((c * 8) >> s, (r * 8) >> s, px >> s);
            let w = &self.ws.win[p];
            for i in 0..n {
                let o = w.at(p, x, y + i);
                snap.pix[p][i * n..(i + 1) * n].copy_from_slice(&w.buf[o..o + n]);
            }
        }
        for i in 0..rows {
            for j in 0..cols {
                snap.mi[i * 8 + j] = self.sh.mi.get(r + i, c + j);
                snap.uv[i * 8 + j] = self.sh.uv.get(r + i, c + j);
            }
        }
        snap.luma = self.coefs.luma;
        snap.chroma = self.coefs.chroma;
        snap.nz = self.ws.nz;
    }

    fn restore(&mut self, r: usize, c: usize, bsize: u8, depth: usize) {
        let (_, rows, cols, px) = self.region(r, c, bsize);
        let snap = &self.ws.snaps[depth];
        for p in 0..3 {
            let s = (p > 0) as usize;
            let (x, y, n) = ((c * 8) >> s, (r * 8) >> s, px >> s);
            let w = &mut self.ws.win[p];
            for i in 0..n {
                let o = w.at(p, x, y + i);
                w.buf[o..o + n].copy_from_slice(&snap.pix[p][i * n..(i + 1) * n]);
            }
        }
        for i in 0..rows {
            for j in 0..cols {
                self.sh.mi.set(r + i, c + j, snap.mi[i * 8 + j]);
                self.sh.uv.set(r + i, c + j, snap.uv[i * 8 + j]);
            }
        }
        self.coefs.luma = snap.luma;
        self.coefs.chroma = snap.chroma;
        self.ws.nz = snap.nz;
    }

    fn tx_size(&self, bsize: u8) -> u8 {
        MAX_TXSIZE[bsize as usize].min(TX_MODE_TO_BIGGEST_TX_SIZE[self.env.tx_mode as usize])
    }

    fn block(&mut self, r: usize, c: usize, bsize: u8) -> u64 {
        let env = self.env;
        self.seg = self.seg_at(r, c);
        let above = (r > 0).then(|| self.mi(r - 1, c));
        let left = (c > self.tile_start).then(|| self.mi(r, c - 1));
        let skip_ctx = above.map_or(0, |m| m.skip as usize) + left.map_or(0, |m| m.skip as usize);
        let mut best: Option<(u64, Choice)> = None;
        let mut refs = None;
        let mut inter_cost = u64::MAX;
        if !env.key {
            let rm = self.mv_refs(r, c, bsize);
            if let Some(d) = self.best_inter(r, c, bsize, &rm, above, left) {
                inter_cost = d.cost;
                let ch = Choice {
                    y_mode: d.mode,
                    uv_mode: DC_PRED,
                    mv: fmv(d.mv[0]),
                    inter: true,
                };
                let (j, ch, _) =
                    self.eval(r, c, bsize, ch, Some(&rm), above, left, skip_ctx, false);
                best = Some((j, ch));
            }
            refs = Some(rm);
        }
        let d = self.best_intra(r, c, bsize, above, left);
        if env.key || d.cost.saturating_mul(100) < inter_cost.saturating_mul(env.speed.intra_margin)
        {
            let ch = Choice {
                y_mode: d.mode,
                uv_mode: d.uv_mode,
                mv: Mv::ZERO,
                inter: false,
            };
            let (j, ch, _) = self.eval(r, c, bsize, ch, None, above, left, skip_ctx, false);
            if best.is_none_or(|b| j < b.0) {
                best = Some((j, ch));
            }
        }
        let Some((_, ch)) = best else {
            return u64::MAX / 4;
        };
        let refs = if ch.inter { refs } else { None };
        self.commit(r, c, bsize, ch, refs.as_ref(), above, left, skip_ctx)
    }

    fn mv_refs(&self, r: usize, c: usize, bsize: u8) -> RefMvs {
        let sh = self.sh;
        let env = MvEnv {
            mi: |r: usize, c: usize| sh.mi.get(r, c),
            prev: self.env.prev_mi,
            mi_rows: self.env.mi_rows,
            mi_cols: self.env.mi_cols,
            tile_start: self.tile_start,
            tile_end: self.tile_end,
            sign_bias: self.env.sign_bias,
            allow_hp: self.env.allow_hp,
        };
        env.refs(r, c, bsize, LAST_FRAME)
    }

    fn is_inter_ctx(above: Option<MiInfo>, left: Option<MiInfo>) -> usize {
        let ai = above.map(|m| m.ref_frame[0] <= INTRA_FRAME);
        let li = left.map(|m| m.ref_frame[0] <= INTRA_FRAME);
        match (ai, li) {
            (Some(a), Some(l)) => {
                if a && l {
                    3
                } else {
                    (a || l) as usize
                }
            }
            (Some(a), None) => 2 * a as usize,
            (None, Some(l)) => 2 * l as usize,
            _ => 0,
        }
    }

    fn mode_rate(
        &self,
        ch: Choice,
        bsize: u8,
        refs: Option<&RefMvs>,
        above: Option<MiInfo>,
        left: Option<MiInfo>,
    ) -> u32 {
        let costs = self.env.costs;
        if self.env.key {
            let a = above.map_or(DC_PRED, |m| m.sub_modes[2]);
            let l = left.map_or(DC_PRED, |m| m.sub_modes[1]);
            return costs.kf_y[a as usize][l as usize][ch.y_mode as usize]
                + costs.kf_uv[ch.y_mode as usize][ch.uv_mode as usize];
        }
        let ictx = Self::is_inter_ctx(above, left);
        if !ch.inter {
            return costs.is_inter[ictx][0]
                + costs.y_mode[SIZE_GROUP[bsize as usize] as usize][ch.y_mode as usize]
                + costs.uv_mode[ch.y_mode as usize][ch.uv_mode as usize];
        }
        let refs = refs.copied().unwrap_or_default();
        let a = above.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let l = left.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let rctx = single_ref_p1_ctx(above.is_some(), left.is_some(), a, l);
        let mut rate = costs.is_inter[ictx][1]
            + costs.single_ref[rctx][0][0]
            + costs.inter_mode[refs.mode_context.min(6) as usize][(ch.y_mode - NEARESTMV) as usize];
        if ch.y_mode == NEWMV {
            let use_hp = self.env.allow_hp && use_mv_hp(refs.best);
            let d = Mv::new(
                ch.mv.row as i32 - refs.best.row as i32,
                ch.mv.col as i32 - refs.best.col as i32,
            );
            rate += costs.mv(d, use_hp);
        }
        rate
    }

    fn nz_ctx(&self, p: usize, x4: usize, y4: usize, step: usize) -> usize {
        let s = (p > 0) as usize;
        let n = 16 >> s;
        let lx = x4 - ((self.sb_c * 16) >> s);
        let ly = y4 - ((self.sb_r * 16) >> s);
        let max_x4 = (2 * self.env.mi_cols) >> s;
        let max_y4 = (2 * self.env.mi_rows) >> s;
        let mut a = 0;
        let mut l = 0;
        for i in 0..step {
            if ly > 0 && lx + i < n && x4 + i < max_x4 {
                a |= self.ws.nz[p][(ly - 1) * n + lx + i];
            }
            if lx > 0 && ly + i < n && y4 + i < max_y4 {
                l |= self.ws.nz[p][(ly + i) * n + lx - 1];
            }
        }
        (a + l) as usize
    }

    fn set_nz(&mut self, p: usize, x4: usize, y4: usize, step: usize, v: u8) {
        let s = (p > 0) as usize;
        let n = 16 >> s;
        let lx = x4 - ((self.sb_c * 16) >> s);
        let ly = y4 - ((self.sb_r * 16) >> s);
        for i in 0..step {
            for j in 0..step {
                if ly + i < n && lx + j < n {
                    self.ws.nz[p][(ly + i) * n + lx + j] = v;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn code_plane(
        &mut self,
        p: usize,
        r: usize,
        c: usize,
        bsize: u8,
        tx_size: u8,
        mode: Option<u8>,
        inter: bool,
        store: bool,
    ) -> (u64, u32, bool) {
        let env = self.env;
        let s = (p > 0) as usize;
        let plane_sz = if p > 0 {
            SS_SIZE_LOOKUP[bsize as usize][1][1]
        } else {
            bsize
        } as usize;
        let n4w = NUM_4X4_WIDE[plane_sz] as usize;
        let n4h = NUM_4X4_HIGH[plane_sz] as usize;
        let step = 1usize << tx_size;
        let bx = (c * 8) >> s;
        let by = (r * 8) >> s;
        let max_x = (env.mi_cols * 8) >> s;
        let max_y = (env.mi_rows * 8) >> s;
        let q = env.q[self.seg as usize][s];
        let mut dist = 0u64;
        let mut rate = 0u32;
        let mut any = false;
        let tx_type = if p > 0 || tx_size == TX_32X32 || inter {
            DCT_DCT
        } else {
            MODE2TXFM[mode.unwrap_or(DC_PRED) as usize]
        };
        let lambda = if env.speed.trellis { self.lambda() } else { 0 };
        for y in (0..n4h).step_by(step) {
            for x in (0..n4w).step_by(step) {
                let sx = bx + 4 * x;
                let sy = by + 4 * y;
                if sx >= max_x || sy >= max_y {
                    continue;
                }
                let w = &mut self.ws.win[p];
                let o = w.at(p, sx, sy);
                if let Some(m) = mode {
                    let f = EdgeFlags {
                        have_left: c > self.tile_start || x > 0,
                        have_above: r > 0 || y > 0,
                        not_on_right: x + step < n4w,
                        max_x: (max_x as isize - 1 - w.x0) as usize,
                        max_y: (max_y as isize - 1 - w.y0) as usize,
                    };
                    let (lx, ly) = ((sx as isize - w.x0) as usize, (sy as isize - w.y0) as usize);
                    intra::predict_in_place(&mut w.buf, WIN[p], lx, ly, m, tx_size, f);
                }
                let x4 = sx >> 2;
                let y4 = sy >> 2;
                let ctx0 = self.nz_ctx(p, x4, y4, step);
                let src = &env.src.planes[p][env.src.idx(p, sx, sy)..];
                let ss = env.src.stride[p];
                let w = &mut self.ws.win[p];
                let (t, pt, ii) = (tx_size as usize, s, inter as usize);
                let tc = tx::TokenCosts {
                    more: &env.costs.more[t][pt][ii],
                    token: &env.costs.token[t][pt][ii],
                };
                let out = tx::encode(
                    tx::Block {
                        src,
                        src_stride: ss,
                        dst: &mut w.buf[o..],
                        dst_stride: WIN[p],
                        tx_size,
                        tx_type,
                        lossless: false,
                        ctx: ctx0,
                        is_inter: inter,
                        chroma: p > 0,
                    },
                    q,
                    &tc,
                    lambda,
                    &mut self.ws.levels,
                );
                let eob = out.eob;
                dist += out.dist;
                rate += out.rate;
                any |= eob > 0;
                self.set_nz(p, x4, y4, step, (eob > 0) as u8);
                if store {
                    let lx = x4 - ((self.sb_c * 16) >> s);
                    let ly = y4 - ((self.sb_r * 16) >> s);
                    let lv = self.ws.levels;
                    self.coefs.put(p, lx, ly, &lv[..eob]);
                }
            }
        }
        (dist, rate, any)
    }

    fn sse_plane(&self, p: usize, r: usize, c: usize, bsize: u8) -> u64 {
        let env = self.env;
        let s = (p > 0) as usize;
        let (bx, by) = ((c * 8) >> s, (r * 8) >> s);
        let (w, h) = self.dims(r, c, bsize, s);
        let win = &self.ws.win[p];
        let o = win.at(p, bx, by);
        let si = env.src.idx(p, bx, by);
        pred::dist::sse(
            &env.src.planes[p][si..],
            env.src.stride[p],
            &win.buf[o..],
            WIN[p],
            w,
            h,
        ) as u64
    }

    fn inter_pred(
        &mut self,
        r: usize,
        c: usize,
        bsize: u8,
        mv: Mv,
        planes: std::ops::Range<usize>,
    ) {
        let env = self.env;
        let Some(rf) = env.refs[0] else {
            return;
        };
        let pos = BlockPos {
            mi_row: r,
            mi_col: c,
            bsize,
            mi_rows: env.mi_rows,
            mi_cols: env.mi_cols,
        };
        for p in planes {
            let s = (p > 0) as usize;
            let (bx, by) = ((c * 8) >> s, (r * 8) >> s);
            let w = &mut self.ws.win[p];
            let o = w.at(p, bx, by);
            inter::predict_plane(
                &[plane_of(rf, p)],
                &[pmv(mv)],
                &pos,
                p,
                0,
                &mut w.buf[o..],
                WIN[p],
            );
        }
    }

    fn src_slice(&self, p: usize, x: usize, y: usize) -> &'a [u8] {
        let env: &'a FrameEnv<'a> = self.env;
        &env.src.planes[p][env.src.idx(p, x, y)..]
    }

    fn dims(&self, r: usize, c: usize, bsize: u8, s: usize) -> (usize, usize) {
        let n = (NUM_8X8_WIDE[bsize as usize] as usize * 8) >> s;
        let w = ((self.env.mi_cols * 8) >> s)
            .saturating_sub((c * 8) >> s)
            .min(n);
        let h = ((self.env.mi_rows * 8) >> s)
            .saturating_sub((r * 8) >> s)
            .min(n);
        (w, h)
    }

    fn best_inter(
        &mut self,
        r: usize,
        c: usize,
        bsize: u8,
        refs: &RefMvs,
        above: Option<MiInfo>,
        left: Option<MiInfo>,
    ) -> Option<Decision> {
        let env = self.env;
        let rf = env.refs[0]?;
        let costs = env.costs;
        let a = above.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let l = left.map_or([INTRA_FRAME, NONE], |m| m.ref_frame);
        let rctx = single_ref_p1_ctx(above.is_some(), left.is_some(), a, l);
        let ictx = Self::is_inter_ctx(above, left);
        let (w, h) = self.dims(r, c, bsize, 0);
        let iref = [InterRef {
            frame: LAST_FRAME,
            luma: plane_of(rf, 0),
            nearest: pmv(refs.nearest),
            near: pmv(refs.near),
            best: pmv(refs.best),
            hints: &[],
            ref_rate: costs.single_ref[rctx][0][0],
        }];
        let choice = InterChoice {
            src: self.src_slice(0, c * 8, r * 8),
            src_stride: env.src.stride[0],
            pos: BlockPos {
                mi_row: r,
                mi_col: c,
                bsize,
                mi_rows: env.mi_rows,
                mi_cols: env.mi_cols,
            },
            w,
            h,
            refs: &iref,
            mode_rates: &costs.inter_mode[refs.mode_context.min(6) as usize],
            is_inter_rate: costs.is_inter[ictx][1],
            mv_cost: env.mv_cost,
            allow_hp: env.allow_hp,
            filter: 0,
            lambda: self.lambda(),
            range: env.speed.range,
            subpel: env.speed.subpel,
        };
        Some(mode::best_inter(&choice, &mut self.ws.inter))
    }

    fn best_intra(
        &mut self,
        r: usize,
        c: usize,
        bsize: u8,
        above: Option<MiInfo>,
        left: Option<MiInfo>,
    ) -> Decision {
        let env = self.env;
        let costs = env.costs;
        let tx = self.tx_size(bsize);
        let uv_tx = tx.min(MAX_TXSIZE[SS_SIZE_LOOKUP[bsize as usize][1][1] as usize]);
        let (y_rates, uv_rates) = if env.key {
            let a = above.map_or(DC_PRED, |m| m.sub_modes[2]);
            let l = left.map_or(DC_PRED, |m| m.sub_modes[1]);
            (&costs.kf_y[a as usize][l as usize], &costs.kf_uv)
        } else {
            (
                &costs.y_mode[SIZE_GROUP[bsize as usize] as usize],
                &costs.uv_mode,
            )
        };
        let mut scratch = std::mem::take(&mut self.ws.intra);
        let block = |p: usize, tx_size: u8| {
            let s = (p > 0) as usize;
            let w = &self.ws.win[p];
            let (bx, by) = ((c * 8) >> s, (r * 8) >> s);
            let (bw, bh) = self.dims(r, c, bsize, s);
            IntraBlock {
                src: self.src_slice(p, bx, by),
                src_stride: env.src.stride[p],
                recon: &w.buf,
                recon_stride: WIN[p],
                x: (bx as isize - w.x0) as usize,
                y: (by as isize - w.y0) as usize,
                w: bw,
                h: bh,
                tx_size,
                have_left: c > self.tile_start,
                have_above: r > 0,
                max_x: (((env.mi_cols * 8) >> s) as isize - 1 - w.x0) as usize,
                max_y: (((env.mi_rows * 8) >> s) as isize - 1 - w.y0) as usize,
            }
        };
        let choice = IntraChoice {
            luma: block(0, tx),
            chroma: [block(1, uv_tx), block(2, uv_tx)],
            y_mode_rates: y_rates,
            uv_mode_rates: uv_rates,
            lambda: self.lambda(),
            modes: env.speed.intra_modes,
        };
        let d = mode::best_intra(&choice, &mut scratch);
        self.ws.intra = scratch;
        d
    }

    #[allow(clippy::too_many_arguments)]
    fn eval(
        &mut self,
        r: usize,
        c: usize,
        bsize: u8,
        ch: Choice,
        refs: Option<&RefMvs>,
        above: Option<MiInfo>,
        left: Option<MiInfo>,
        skip_ctx: usize,
        store: bool,
    ) -> (u64, Choice, bool) {
        let env = self.env;
        let tx = self.tx_size(bsize);
        let uv_tx = tx.min(MAX_TXSIZE[SS_SIZE_LOOKUP[bsize as usize][1][1] as usize]);
        let (mut dist, mut rate, mut any);
        let mrate = self.mode_rate(ch, bsize, refs, above, left);
        if ch.inter {
            self.inter_pred(r, c, bsize, ch.mv, 0..3);
            let sse: u64 = (0..3).map(|p| self.sse_plane(p, r, c, bsize)).sum();
            let (d0, r0, a0) = self.code_plane(0, r, c, bsize, tx, None, true, store);
            let (d1, r1, a1) = self.code_plane(1, r, c, bsize, uv_tx, None, true, store);
            let (d2, r2, a2) = self.code_plane(2, r, c, bsize, uv_tx, None, true, store);
            dist = d0 + d1 + d2;
            rate = r0 + r1 + r2;
            any = a0 || a1 || a2;
            let coded = rdcost(
                self.lambda(),
                rate + env.costs.skip[skip_ctx][0] + mrate,
                dist,
            );
            let skipped = rdcost(self.lambda(), env.costs.skip[skip_ctx][1] + mrate, sse);
            if !any || skipped <= coded {
                if any {
                    self.inter_pred(r, c, bsize, ch.mv, 0..3);
                }
                any = false;
                dist = sse;
                rate = 0;
            }
        } else {
            let (d0, r0, a0) = self.code_plane(0, r, c, bsize, tx, Some(ch.y_mode), false, store);
            let (d1, r1, a1) =
                self.code_plane(1, r, c, bsize, uv_tx, Some(ch.uv_mode), false, store);
            let (d2, r2, a2) =
                self.code_plane(2, r, c, bsize, uv_tx, Some(ch.uv_mode), false, store);
            dist = d0 + d1 + d2;
            rate = r0 + r1 + r2;
            any = a0 || a1 || a2;
        }
        rate += mrate + env.costs.skip[skip_ctx][!any as usize];
        (rdcost(self.lambda(), rate, dist), ch, !any)
    }

    #[allow(clippy::too_many_arguments)]
    fn commit(
        &mut self,
        r: usize,
        c: usize,
        bsize: u8,
        ch: Choice,
        refs: Option<&RefMvs>,
        above: Option<MiInfo>,
        left: Option<MiInfo>,
        skip_ctx: usize,
    ) -> u64 {
        let (j, ch, skip) = self.eval(r, c, bsize, ch, refs, above, left, skip_ctx, true);
        if skip {
            for p in 0..3 {
                let s = (p > 0) as usize;
                let n4 = (NUM_8X8_WIDE[bsize as usize] as usize * 2) >> s;
                self.set_nz(p, (c * 2) >> s, (r * 2) >> s, n4, 0);
            }
        }
        let mut mi = MiInfo {
            sb_size: bsize,
            skip,
            tx_size: self.tx_size(bsize),
            y_mode: ch.y_mode,
            sub_modes: [ch.y_mode; 4],
            seg_id: self.seg,
            ..MiInfo::default()
        };
        if ch.inter {
            mi.sub_modes = [DC_PRED; 4];
            mi.ref_frame = [LAST_FRAME, NONE];
            mi.mv[0] = [ch.mv; 4];
        }
        let n = NUM_8X8_WIDE[bsize as usize] as usize;
        for i in 0..n.min(self.env.mi_rows - r) {
            for k in 0..n.min(self.env.mi_cols - c) {
                self.sh.mi.set(r + i, c + k, mi);
                self.sh.uv.set(r + i, c + k, ch.uv_mode);
            }
        }
        j
    }
}

pub fn plane_of(p: &Picture, plane: usize) -> Plane<'_> {
    let (w, h) = p.visible(plane);
    Plane {
        data: &p.planes[plane],
        stride: p.stride[plane],
        origin: p.origin[plane],
        width: w,
        height: h,
        border: PAD[plane],
    }
}

fn pmv(m: Mv) -> pred::Mv {
    pred::Mv::new(m.row, m.col)
}

fn fmv(m: pred::Mv) -> Mv {
    Mv {
        row: m.row,
        col: m.col,
    }
}
