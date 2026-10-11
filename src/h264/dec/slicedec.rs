use crate::h264::bits::BitReader;
use crate::h264::dec::cavlc;
use crate::h264::dec::deblock;
use crate::h264::dec::error::{Result, ensure, invalid};
use crate::h264::dec::inter::{Mc, Weight};
use crate::h264::dec::intra::{self, Avail};
use crate::h264::dec::mbtypes::*;
use crate::h264::dec::params::{Pps, Sps};
use crate::h264::dec::picture::*;
use crate::h264::dec::slice::{SliceHeader, SliceType};
use crate::h264::dec::tables::*;
use crate::h264::dec::transform::{self, LevelScale};

const RING: usize = 4;

pub struct MbRing {
    data: Vec<MbState>,
    slot: Box<[u32]>,
    tag: [usize; RING],
    mb_w: usize,
    mb_h: usize,
    full: bool,
}

impl MbRing {
    fn new(mb_w: usize, mb_h: usize) -> Self {
        let mut r = MbRing {
            data: Vec::new(),
            slot: Box::new([]),
            tag: [usize::MAX; RING],
            mb_w,
            mb_h,
            full: true,
        };
        r.reset();
        r
    }

    fn reset(&mut self) {
        if self.full || self.data.is_empty() {
            let rows = RING.min(self.mb_h);
            self.data = vec![MbState::default(); rows * self.mb_w];
            self.slot = (0..self.mb_w * self.mb_h)
                .map(|a| ((a / self.mb_w % rows) * self.mb_w + a % self.mb_w) as u32)
                .collect();
            self.full = rows == self.mb_h && self.mb_h <= RING;
        }
        self.tag = [usize::MAX; RING];
        for m in &mut self.data {
            m.slice_num = u32::MAX;
        }
        if self.full {
            for (k, t) in self.tag.iter_mut().enumerate().take(self.mb_h) {
                *t = k;
            }
        }
    }

    #[inline]
    fn enter_row(&mut self, y: usize, published: usize) {
        if self.full {
            return;
        }
        for yy in y.saturating_sub(1)..=y {
            let k = yy % RING;
            let t = self.tag[k];
            if t == yy {
                continue;
            }
            if t != usize::MAX && t >= published {
                self.grow();
                return;
            }
            for m in &mut self.data[k * self.mb_w..(k + 1) * self.mb_w] {
                m.slice_num = u32::MAX;
            }
            self.tag[k] = yy;
        }
    }

    fn grow(&mut self) {
        let mut data = vec![MbState::default(); self.mb_w * self.mb_h];
        for (k, &t) in self.tag.iter().enumerate() {
            if t < self.mb_h {
                data[t * self.mb_w..(t + 1) * self.mb_w]
                    .copy_from_slice(&self.data[k * self.mb_w..(k + 1) * self.mb_w]);
            }
        }
        self.data = data;
        self.slot = (0..self.mb_w * self.mb_h).map(|a| a as u32).collect();
        self.full = true;
    }

    fn row(&self, r: usize) -> &[MbState] {
        let s = self.slot[r * self.mb_w] as usize;
        &self.data[s..s + self.mb_w]
    }
}

impl std::ops::Index<usize> for MbRing {
    type Output = MbState;
    #[inline(always)]
    fn index(&self, a: usize) -> &MbState {
        &self.data[self.slot[a] as usize]
    }
}

impl std::ops::IndexMut<usize> for MbRing {
    #[inline(always)]
    fn index_mut(&mut self, a: usize) -> &mut MbState {
        &mut self.data[self.slot[a] as usize]
    }
}

#[derive(Clone, Debug, Default)]
pub struct SliceInfo {
    pub disable_deblocking_filter_idc: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    pub ref_ids: [Vec<u32>; 2],
}

pub struct PicState {
    pub planes: PicPlanes,
    pub mbs: MbRing,
    pub slices: Vec<SliceInfo>,
    pub mb_w: usize,
    pub mb_h: usize,
    pub poc: i32,
    pub decoded_mbs: usize,
    pub frame: FrameRef,
    pub reference: bool,
    row_count: Vec<u16>,
    rows_recon: usize,
    rows_deblocked: usize,
    rows_published: usize,
}

impl PicState {
    pub fn new(frame: FrameRef) -> Self {
        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h());
        PicState {
            planes: unsafe { frame.writer() },
            mbs: MbRing::new(mb_w, mb_h),
            slices: Vec::new(),
            mb_w,
            mb_h,
            poc: frame.poc,
            decoded_mbs: 0,
            frame,
            reference: true,
            row_count: vec![0; mb_h],
            rows_recon: 0,
            rows_deblocked: 0,
            rows_published: 0,
        }
    }

    pub fn reset(&mut self, frame: FrameRef) {
        debug_assert_eq!((self.mb_w, self.mb_h), (frame.mb_w, frame.mb_h()));
        self.mbs.reset();
        self.slices.clear();
        self.planes = unsafe { frame.writer() };
        self.poc = frame.poc;
        self.decoded_mbs = 0;
        self.frame = frame;
        self.row_count.fill(0);
        self.rows_recon = 0;
        self.rows_deblocked = 0;
        self.rows_published = 0;
    }

    #[inline]
    pub fn mb_done(&mut self, addr: usize) {
        self.decoded_mbs += 1;
        let r = addr / self.mb_w;
        self.row_count[r] += 1;
        if self.row_count[r] as usize >= self.mb_w && r == self.rows_recon {
            while self.rows_recon < self.mb_h
                && self.row_count[self.rows_recon] as usize >= self.mb_w
            {
                self.rows_recon += 1;
            }
            self.flush_rows(false);
        }
    }

    pub fn flush_rows(&mut self, finished: bool) {
        let deblock_limit = if finished {
            self.mb_h
        } else {
            self.rows_recon.saturating_sub(1)
        };
        while self.rows_deblocked < deblock_limit {
            let r = self.rows_deblocked;
            for addr in r * self.mb_w..(r + 1) * self.mb_w {
                deblock::deblock_mb(self, addr, self.mb_w);
            }
            self.rows_deblocked += 1;
        }
        let publish_limit = if finished {
            self.mb_h
        } else {
            self.rows_deblocked.saturating_sub(1)
        };
        while self.rows_published < publish_limit {
            let r = self.rows_published;
            self.frame.publish(r, Some(self.mbs.row(r)));
            self.rows_published += 1;
            self.planes.published_rows(self.rows_published);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WpMode {
    Default,
    Explicit,
    Implicit,
}

struct Weights {
    mode: WpMode,
    luma_denom: i32,
    chroma_denom: i32,
    luma: [Vec<(i32, i32)>; 2],
    chroma: [Vec<[(i32, i32); 2]>; 2],
    implicit: Vec<(i32, i32)>,
    n1: usize,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct MbCur {
    pub info: MbTypeInfo,
    pub sub: [SubMbInfo; 4],
    pub rem_mode: [i8; 16],
    pub cbp: u8,
    pub ref_idx: [[i8; 4]; 2],
    pub mvd: [[[i16; 2]; 16]; 2],
}

pub(crate) struct Scratch {
    pub coef: [i32; 256],
    pub coef_c: [[i32; 64]; 2],
    pub blk_nz: u16,
    pub c_nz: [u8; 2],
    pub pred: [[u8; 256]; 2],
    pub pred_c: [[[u8; 64]; 2]; 2],
}

impl Scratch {
    pub fn new() -> Box<Self> {
        Box::new(Scratch {
            coef: [0; 256],
            coef_c: [[0; 64]; 2],
            blk_nz: 0,
            c_nz: [0; 2],
            pred: [[0; 256]; 2],
            pred_c: [[[0; 64]; 2]; 2],
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Nb {
    pub avail: bool,
    pub ref_idx: i8,
    pub mv: [i16; 2],
}

const NB_NA: Nb = Nb {
    avail: false,
    ref_idx: -1,
    mv: [0, 0],
};

pub struct SliceDecoder<'a> {
    pub sh: &'a SliceHeader,
    pub pps: &'a Pps,
    pub sps: &'a Sps,
    pub pic: &'a mut PicState,
    pub refs: &'a [Vec<RefPic>; 2],
    pub ls: &'a LevelScale,
    pub slice_num: u32,
    pub mb_w: usize,
    pub mb_h: usize,
    pub qp: i32,
    pub chroma_qp_offset: [i32; 2],
    pub mb_addr: usize,
    pub mb_x: usize,
    pub mb_y: usize,
    pub nb: [Option<usize>; 4],
    pub mv_done: u16,
    pub cur: MbCur,
    pub s: Box<Scratch>,
    weights: Weights,
    tdirect: Vec<(i32, bool)>,
    pub prev_qp_delta_nz: bool,
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn predict_block<M: Mc, const AVG: bool>(
    f: &Frame,
    mv: [i16; 2],
    px: usize,
    py: usize,
    bw: usize,
    bh: usize,
    y: (&mut [u8], usize),
    c: [&mut [u8]; 2],
    cs: usize,
) {
    const LW: usize = 24;
    let (width, height) = (f.width, f.height);
    let x0 = px as i32 + (mv[0] as i32 >> 2) - 2;
    let y0 = py as i32 + (mv[1] as i32 >> 2) - 2;
    let last = (y0 + bh as i32 + 4).clamp(0, height as i32 - 1) as usize;
    let [luma, cb, cr] = f.planes_upto(last >> 4);
    let (fx, fy) = ((mv[0] & 3) as u32, (mv[1] & 3) as u32);
    if x0 >= 0 && y0 >= 0 && x0 as usize + bw + 5 <= width && y0 as usize + bh + 5 <= height {
        let src = &luma[y0 as usize * width + x0 as usize..];
        M::luma::<AVG>(src, width, fx, fy, bw, bh, y.0, y.1);
    } else {
        let mut win = [0u8; LW * 21];
        window::<LW>(luma, width, height, x0, y0, bw + 5, bh + 5, &mut win, LW);
        M::luma::<AVG>(&win, LW, fx, fy, bw, bh, y.0, y.1);
    }
    let (cwidth, cheight) = (f.cwidth, f.cheight);
    let cx = (px / 2) as i32 + (mv[0] as i32 >> 3);
    let cy = (py / 2) as i32 + (mv[1] as i32 >> 3);
    let (cw, ch) = (bw / 2, bh / 2);
    let (fx, fy) = ((mv[0] & 7) as u32, (mv[1] & 7) as u32);
    if cx >= 0 && cy >= 0 && cx as usize + cw < cwidth && cy as usize + ch < cheight {
        let o = cy as usize * cwidth + cx as usize;
        M::chroma2::<AVG>([&cb[o..], &cr[o..]], cwidth, fx, fy, cw, ch, c, cs);
    } else {
        let mut w = [[0u8; 16 * 9]; 2];
        for (plane, w) in [cb, cr].into_iter().zip(w.iter_mut()) {
            window::<16>(plane, cwidth, cheight, cx, cy, cw + 1, ch + 1, w, 16);
        }
        M::chroma2::<AVG>([&w[0], &w[1]], 16, fx, fy, cw, ch, c, cs);
    }
}

#[inline(always)]
fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

impl<'a> SliceDecoder<'a> {
    pub fn new(
        sh: &'a SliceHeader,
        pps: &'a Pps,
        sps: &'a Sps,
        pic: &'a mut PicState,
        refs: &'a [Vec<RefPic>; 2],
        ls: &'a LevelScale,
    ) -> Result<Self> {
        let slice_num = pic.slices.len() as u32;
        let chroma_qp_offset = [
            pps.chroma_qp_index_offset,
            pps.second_chroma_qp_index_offset,
        ];
        pic.slices.push(SliceInfo {
            disable_deblocking_filter_idc: sh.disable_deblocking_filter_idc,
            alpha_offset: sh.slice_alpha_c0_offset_div2 * 2,
            beta_offset: sh.slice_beta_offset_div2 * 2,
            ref_ids: [
                refs[0].iter().map(|r| r.id()).collect(),
                refs[1].iter().map(|r| r.id()).collect(),
            ],
        });
        let weights = Self::build_weights(sh, pps, refs, pic.poc);
        let mut tdirect = Vec::new();
        if sh.slice_type == SliceType::B && !sh.direct_spatial_mv_pred && !refs[1].is_empty() {
            let poc1 = refs[1][0].poc();
            for r0 in refs[0].iter() {
                let tb = (pic.poc - r0.poc()).clamp(-128, 127);
                let td = (poc1 - r0.poc()).clamp(-128, 127);
                if td == 0 || r0.long_term {
                    tdirect.push((256, true));
                } else {
                    let tx = (16384 + (td / 2).abs()) / td;
                    tdirect.push((((tb * tx + 32) >> 6).clamp(-1024, 1023), false));
                }
            }
        }
        let (mb_w, mb_h) = (pic.mb_w, pic.mb_h);
        Ok(SliceDecoder {
            sh,
            pps,
            sps,
            pic,
            refs,
            ls,
            slice_num,
            mb_w,
            mb_h,
            qp: sh.qp(pps),
            chroma_qp_offset,
            mb_addr: 0,
            mb_x: 0,
            mb_y: 0,
            nb: [None; 4],
            mv_done: 0,
            cur: MbCur::default(),
            s: Scratch::new(),
            weights,
            tdirect,
            prev_qp_delta_nz: false,
        })
    }

    fn build_weights(
        sh: &SliceHeader,
        pps: &Pps,
        refs: &[Vec<RefPic>; 2],
        cur_poc: i32,
    ) -> Weights {
        let mut w = Weights {
            mode: WpMode::Default,
            luma_denom: 0,
            chroma_denom: 0,
            luma: [Vec::new(), Vec::new()],
            chroma: [Vec::new(), Vec::new()],
            implicit: Vec::new(),
            n1: refs[1].len(),
        };
        let explicit = (pps.weighted_pred && sh.slice_type.is_p())
            || (pps.weighted_bipred_idc == 1 && sh.slice_type.is_b());
        if explicit {
            if let Some(t) = &sh.pred_weight_table {
                w.mode = WpMode::Explicit;
                w.luma_denom = t.luma_log2_denom as i32;
                w.chroma_denom = t.chroma_log2_denom as i32;
                for (l, entries) in [&t.l0, &t.l1].into_iter().enumerate() {
                    w.luma[l] = entries
                        .iter()
                        .map(|e| (e.luma_weight, e.luma_offset))
                        .collect();
                    w.chroma[l] = entries
                        .iter()
                        .map(|e| {
                            [
                                (e.chroma_weight[0], e.chroma_offset[0]),
                                (e.chroma_weight[1], e.chroma_offset[1]),
                            ]
                        })
                        .collect();
                }
            }
        } else if pps.weighted_bipred_idc == 2 && sh.slice_type.is_b() {
            w.mode = WpMode::Implicit;
            for r0 in refs[0].iter() {
                for r1 in refs[1].iter() {
                    let tb = (cur_poc - r0.poc()).clamp(-128, 127);
                    let td = (r1.poc() - r0.poc()).clamp(-128, 127);
                    let pair = if td == 0 || r0.long_term || r1.long_term {
                        (32, 32)
                    } else {
                        let tx = (16384 + (td / 2).abs()) / td;
                        let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                        if (dsf >> 2) < -64 || (dsf >> 2) > 128 {
                            (32, 32)
                        } else {
                            (64 - (dsf >> 2), dsf >> 2)
                        }
                    };
                    w.implicit.push(pair);
                }
            }
        }
        w
    }

    pub fn start_mb(&mut self, addr: usize) -> Result<()> {
        self.mb_addr = addr;
        self.mb_x = addr % self.mb_w;
        self.mb_y = addr / self.mb_w;
        let published = self.pic.rows_published;
        ensure!(
            self.mb_y > published || (self.mb_y == 0 && published == 0),
            "macroblock {addr} lies in a finished row"
        );
        self.pic.mbs.enter_row(self.mb_y, self.pic.rows_published);
        if self.mb_x & 3 == 0 {
            self.pic.planes.prefetch_mb(self.mb_x + 4, self.mb_y);
        }
        let sn = self.slice_num;
        let mbs = &self.pic.mbs;
        let w = self.mb_w;
        let (x, y) = (self.mb_x, self.mb_y);
        let ok = |a: usize| {
            if mbs[a].slice_num == sn {
                Some(a)
            } else {
                None
            }
        };
        self.nb = [
            if x > 0 { ok(addr - 1) } else { None },
            if y > 0 { ok(addr - w) } else { None },
            if y > 0 && x + 1 < w {
                ok(addr - w + 1)
            } else {
                None
            },
            if y > 0 && x > 0 {
                ok(addr - w - 1)
            } else {
                None
            },
        ];
        self.mv_done = 0;
        self.pic.mbs[addr] = MbState {
            slice_num: sn,
            ..MbState::default()
        };
        Ok(())
    }

    #[inline]
    pub fn mb(&self) -> &MbState {
        &self.pic.mbs[self.mb_addr]
    }
    #[inline]
    pub fn mb_mut(&mut self) -> &mut MbState {
        &mut self.pic.mbs[self.mb_addr]
    }

    fn intra_nb(&self, n: usize) -> bool {
        match self.nb[n] {
            None => false,
            Some(a) => !self.pps.constrained_intra_pred || self.pic.mbs[a].kind.is_intra(),
        }
    }

    fn chroma_qp(&self, qp: i32, c: usize) -> i32 {
        QPC_TABLE[(qp + self.chroma_qp_offset[c]).clamp(0, 51) as usize] as i32
    }

    fn nc_luma(&self, raster: usize) -> i32 {
        let (bx, by) = (raster & 3, raster >> 2);
        let cur = self.mb();
        let a = if bx > 0 {
            Some(cur.nnz[raster - 1])
        } else {
            self.nb[0].map(|m| self.pic.mbs[m].nnz[by * 4 + 3])
        };
        let b = if by > 0 {
            Some(cur.nnz[raster - 4])
        } else {
            self.nb[1].map(|m| self.pic.mbs[m].nnz[12 + bx])
        };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    fn nc_chroma(&self, c: usize, raster: usize) -> i32 {
        let (bx, by) = (raster & 1, raster >> 1);
        let cur = self.mb();
        let a = if bx > 0 {
            Some(cur.nnz_c[c][raster - 1])
        } else {
            self.nb[0].map(|m| self.pic.mbs[m].nnz_c[c][by * 2 + 1])
        };
        let b = if by > 0 {
            Some(cur.nnz_c[c][raster - 2])
        } else {
            self.nb[1].map(|m| self.pic.mbs[m].nnz_c[c][2 + bx])
        };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    pub(crate) fn nb_motion(&self, list: usize, x: i32, y: i32) -> Nb {
        let (mb, bx, by) = if x < 0 {
            if y < 0 {
                (self.nb[3], 3, 3)
            } else if y > 3 {
                (None, 0, 0)
            } else {
                (self.nb[0], 3, y as usize)
            }
        } else if x > 3 {
            if y < 0 {
                (self.nb[2], 0, 3)
            } else {
                (None, 0, 0)
            }
        } else if y < 0 {
            (self.nb[1], x as usize, 3)
        } else {
            let r = (y * 4 + x) as usize;
            if self.mv_done & (1 << r) == 0 {
                return NB_NA;
            }
            (Some(self.mb_addr), x as usize, y as usize)
        };
        let Some(m) = mb else { return NB_NA };
        let st = &self.pic.mbs[m];
        let b8 = (by >> 1) * 2 + (bx >> 1);
        let r = st.ref_idx[list][b8];
        if r < 0 {
            return Nb {
                avail: true,
                ref_idx: -1,
                mv: [0, 0],
            };
        }
        Nb {
            avail: true,
            ref_idx: r,
            mv: st.mv[list][by * 4 + bx],
        }
    }

    fn nb_abc(&self, list: usize, x: usize, y: usize, w: usize) -> (Nb, Nb, Nb) {
        let (x, y, w) = (x as i32, y as i32, w as i32);
        let a = self.nb_motion(list, x - 1, y);
        let b = self.nb_motion(list, x, y - 1);
        let mut c = self.nb_motion(list, x + w, y - 1);
        if !c.avail {
            c = self.nb_motion(list, x - 1, y - 1);
        }
        (a, b, c)
    }

    #[allow(clippy::too_many_arguments)]
    fn mvp(
        &self,
        list: usize,
        x: usize,
        y: usize,
        w: usize,
        ref_idx: i8,
        shape: u8,
        part: usize,
    ) -> [i16; 2] {
        let (a, b, c) = self.nb_abc(list, x, y, w);
        if shape == 1 {
            if part == 0 && b.ref_idx == ref_idx {
                return b.mv;
            }
            if part == 1 && a.ref_idx == ref_idx {
                return a.mv;
            }
        } else if shape == 2 {
            if part == 0 && a.ref_idx == ref_idx {
                return a.mv;
            }
            if part == 1 && c.ref_idx == ref_idx {
                return c.mv;
            }
        }
        Self::median_pred(a, b, c, ref_idx)
    }

    fn median_pred(a: Nb, mut b: Nb, mut c: Nb, ref_idx: i8) -> [i16; 2] {
        if !b.avail && !c.avail && a.avail {
            b = a;
            c = a;
        }
        let ma = a.ref_idx == ref_idx;
        let mb = b.ref_idx == ref_idx;
        let mc = c.ref_idx == ref_idx;
        match (ma, mb, mc) {
            (true, false, false) => a.mv,
            (false, true, false) => b.mv,
            (false, false, true) => c.mv,
            _ => [
                median(a.mv[0], b.mv[0], c.mv[0]),
                median(a.mv[1], b.mv[1], c.mv[1]),
            ],
        }
    }

    fn set_motion(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        refs: [i8; 2],
        mvs: [[i16; 2]; 2],
    ) {
        let st = &mut self.pic.mbs[self.mb_addr];
        let mv = [0, 1].map(|l| if refs[l] >= 0 { mvs[l] } else { [0, 0] });
        if w == 4 && h == 4 {
            for l in 0..2 {
                st.mv[l] = [mv[l]; 16];
                st.ref_idx[l] = [refs[l]; 4];
            }
            self.mv_done = 0xffff;
            return;
        }
        let row_mask = ((1u16 << w) - 1) << x;
        for yy in y..y + h {
            for l in 0..2 {
                st.mv[l][yy * 4 + x..yy * 4 + x + w].fill(mv[l]);
            }
            self.mv_done |= row_mask << (yy * 4);
        }
        for l in 0..2 {
            for by in y >> 1..=(y + h - 1) >> 1 {
                for bx in x >> 1..=(x + w - 1) >> 1 {
                    st.ref_idx[l][by * 2 + bx] = refs[l];
                }
            }
        }
    }

    fn colocated(&self, m: Option<&ColMb>, raster: usize) -> ([i16; 2], i8, u32) {
        let raster = if self.sps.direct_8x8_inference {
            let (bx, by) = (raster & 3, raster >> 2);
            (if by >= 2 { 12 } else { 0 }) + if bx >= 2 { 3 } else { 0 }
        } else {
            raster
        };
        let Some(m) = m.filter(|m| !m.intra) else {
            return ([0, 0], -1, u32::MAX);
        };
        let b8 = (raster >> 3) * 2 + ((raster & 3) >> 1);
        let l = if m.ref_idx[0][b8] >= 0 { 0 } else { 1 };
        (m.mv[l][raster], m.ref_idx[l][b8], m.ref_id[l][b8])
    }

    fn spatial_direct_base(&self) -> ([i8; 2], [[i16; 2]; 2], bool) {
        let mut refs = [-1i8; 2];
        let mut nbs = [(NB_NA, NB_NA, NB_NA); 2];
        let min_pos = |x: i8, y: i8| if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) };
        for (l, nb) in nbs.iter_mut().enumerate() {
            *nb = self.nb_abc(l, 0, 0, 4);
            refs[l] = min_pos(nb.0.ref_idx, min_pos(nb.1.ref_idx, nb.2.ref_idx));
        }
        let mut mvs = [[0i16; 2]; 2];
        if refs[0] < 0 && refs[1] < 0 {
            return ([0, 0], mvs, true);
        }
        for l in 0..2 {
            if refs[l] >= 0 {
                let (a, b, c) = nbs[l];
                mvs[l] = Self::median_pred(a, b, c, refs[l]);
            }
        }
        (refs, mvs, false)
    }

    fn derive_direct(&mut self, b8_mask: u8) -> Result<()> {
        if let Some(col) = self.refs[1].first()
            && col.frame.mb_w == self.mb_w
        {
            col.frame.prefetch_motion(self.mb_y, self.mb_x + 1);
        }
        let spatial = self.sh.direct_spatial_mv_pred;
        let (base_refs, base_mvs, direct_zero) = if spatial {
            self.spatial_direct_base()
        } else {
            ([0, 0], [[0, 0]; 2], false)
        };
        let refs = self.refs;
        let col_short_term = refs[1].first().map(|r| !r.long_term).unwrap_or(false);
        let col_row = refs[1]
            .first()
            .filter(|c| c.frame.mb_w == self.mb_w && self.mb_y < c.frame.mb_h())
            .and_then(|c| c.frame.col_mb(self.mb_y, self.mb_x));
        for b8 in 0..4 {
            if b8_mask & (1 << b8) == 0 {
                continue;
            }
            let (x8, y8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            let unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
            for sub in (0..4).filter(|s| unit == 1 || *s == 0) {
                let (x, y) = (x8 + (sub & 1), y8 + (sub >> 1));
                let raster = y * 4 + x;
                let (mv_col, ref_col, id_col) = self.colocated(col_row, raster);
                let (refs, mvs) = if spatial {
                    let col_zero = col_short_term
                        && ref_col == 0
                        && (-1..=1).contains(&mv_col[0])
                        && (-1..=1).contains(&mv_col[1]);
                    let mut mvs = base_mvs;
                    for l in 0..2 {
                        if direct_zero || base_refs[l] < 0 || (base_refs[l] == 0 && col_zero) {
                            mvs[l] = [0, 0];
                        }
                    }
                    (base_refs, mvs)
                } else {
                    let ref0 = if ref_col < 0 {
                        0
                    } else {
                        match self.refs[0].iter().position(|r| r.id() == id_col) {
                            Some(i) => i as i8,
                            None => 0,
                        }
                    };
                    let (dsf, ltr) = self
                        .tdirect
                        .get(ref0 as usize)
                        .copied()
                        .unwrap_or((256, true));
                    let (mv0, mv1) = if ltr {
                        (mv_col, [0, 0])
                    } else {
                        let s = |c: i16| ((dsf * c as i32 + 128) >> 8) as i16;
                        let m0 = [s(mv_col[0]), s(mv_col[1])];
                        (m0, [m0[0] - mv_col[0], m0[1] - mv_col[1]])
                    };
                    ([ref0, 0], [mv0, mv1])
                };
                self.set_motion(x, y, unit, unit, refs, mvs);
            }
        }
        let st = &mut self.pic.mbs[self.mb_addr];
        st.direct8x8 |= b8_mask;
        Ok(())
    }

    pub fn derive_p_skip(&mut self) {
        let a = self.nb_motion(0, -1, 0);
        let b = self.nb_motion(0, 0, -1);
        let mv = if self.nb[0].is_none()
            || self.nb[1].is_none()
            || (a.ref_idx == 0 && mv_bits(a.mv) == 0)
            || (b.ref_idx == 0 && mv_bits(b.mv) == 0)
        {
            [0, 0]
        } else {
            self.mvp(0, 0, 0, 4, 0, 0, 0)
        };
        self.set_motion(0, 0, 4, 4, [0, -1], [mv, [0, 0]]);
    }

    pub fn derive_inter_motion(&mut self) -> Result<()> {
        let info = self.cur.info;
        if info.kind == MbKind::BDirect16x16 || info.kind == MbKind::BSkip {
            return self.derive_direct(0xf);
        }
        let n = [self.refs[0].len(), self.refs[1].len()];
        if info.part != Part::P8x8 {
            let shape = match info.part {
                Part::P16x8 => 1,
                Part::P8x16 => 2,
                _ => 0,
            };
            for p in 0..info.part.num_parts() {
                let (x, y, w, h) = info.part.rect(p);
                let mut refs = [-1i8; 2];
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if info.pred[p] & (1 << l) != 0 {
                        let r = self.cur.ref_idx[l][p];
                        ensure!(
                            (r as usize) < n[l],
                            "ref_idx {r} out of range (list {l}, size {})",
                            n[l]
                        );
                        let mvp = self.mvp(l, x, y, w, r, shape, p);
                        let d = self.cur.mvd[l][p * 4];
                        refs[l] = r;
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
            return Ok(());
        }
        for p in 0..4 {
            let sub = self.cur.sub[p];
            let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
            if sub.direct {
                self.derive_direct(1 << p)?;
                continue;
            }
            let mut refs = [-1i8; 2];
            for (l, r) in refs.iter_mut().enumerate() {
                if sub.pred & (1 << l) != 0 {
                    let v = self.cur.ref_idx[l][p];
                    ensure!(
                        (v as usize) < n[l],
                        "ref_idx {v} out of range (list {l}, size {})",
                        n[l]
                    );
                    *r = v;
                }
            }
            for j in 0..sub.shape.num_parts() {
                let (sx, sy, w, h) = sub.shape.rect(j);
                let (x, y) = (x8 + sx, y8 + sy);
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if refs[l] >= 0 {
                        let mvp = self.mvp(l, x, y, w, refs[l], 0, 0);
                        let d = self.cur.mvd[l][p * 4 + j];
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
        }
        Ok(())
    }

    fn weight_for(&self, refs: [i8; 2], comp: usize) -> Weight {
        self.raw_weight(refs, comp)
            .simplify(refs[0] >= 0, refs[1] >= 0)
    }

    fn raw_weight(&self, refs: [i8; 2], comp: usize) -> Weight {
        let w = &self.weights;
        match w.mode {
            WpMode::Default => Weight::Default,
            WpMode::Implicit => {
                if refs[0] >= 0 && refs[1] >= 0 {
                    let (w0, w1) = w.implicit[refs[0] as usize * w.n1 + refs[1] as usize];
                    Weight::Weighted {
                        log_wd: 5,
                        w0,
                        w1,
                        o0: 0,
                        o1: 0,
                    }
                } else {
                    Weight::Default
                }
            }
            WpMode::Explicit => {
                let get = |l: usize| -> (i32, i32) {
                    if refs[l] < 0 {
                        return (0, 0);
                    }
                    let r = refs[l] as usize;
                    if comp == 0 {
                        w.luma[l].get(r).copied().unwrap_or((1 << w.luma_denom, 0))
                    } else {
                        w.chroma[l]
                            .get(r)
                            .map(|c| c[comp - 1])
                            .unwrap_or((1 << w.chroma_denom, 0))
                    }
                };
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                let log_wd = if comp == 0 {
                    w.luma_denom
                } else {
                    w.chroma_denom
                };
                Weight::Weighted {
                    log_wd,
                    w0,
                    w1,
                    o0,
                    o1,
                }
            }
        }
    }

    #[inline(always)]
    fn mc_rect<M: Mc>(&mut self, x4: usize, y4: usize, w4: usize, h4: usize) {
        let st = &self.pic.mbs[self.mb_addr];
        let r = y4 * 4 + x4;
        let b8 = (y4 >> 1) * 2 + (x4 >> 1);
        let refs = [st.ref_idx[0][b8], st.ref_idx[1][b8]];
        let mvs = [st.mv[0][r], st.mv[1][r]];
        let (bw, bh) = (w4 * 4, h4 * 4);
        let wts = [
            self.weight_for(refs, 0),
            self.weight_for(refs, 1),
            self.weight_for(refs, 2),
        ];
        let px = self.mb_x * 16 + x4 * 4;
        let py = self.mb_y * 16 + y4 * 4;
        let use0 = refs[0] >= 0 && self.refs[0].get(refs[0] as usize).is_some();
        let use1 = refs[1] >= 0 && self.refs[1].get(refs[1] as usize).is_some();
        let ys = self.pic.planes.width;
        let cs = self.pic.planes.cwidth;
        let (cx, cy) = (px / 2, py / 2);
        if (use0 || use1) && wts.iter().all(|w| *w == Weight::Default) {
            let planes = &mut self.pic.planes;
            let mut avg = false;
            for l in 0..2 {
                if !(if l == 0 { use0 } else { use1 }) {
                    continue;
                }
                let f = &self.refs[l][refs[l] as usize].frame;
                let y = (&mut planes.y[py * ys + px..], ys);
                let c = [
                    &mut planes.cb[cy * cs + cx..],
                    &mut planes.cr[cy * cs + cx..],
                ];
                if avg {
                    predict_block::<M, true>(f, mvs[l], px, py, bw, bh, y, c, cs);
                } else {
                    predict_block::<M, false>(f, mvs[l], px, py, bw, bh, y, c, cs);
                }
                avg = true;
            }
            return;
        }
        let s = &mut *self.s;
        for l in 0..2 {
            if refs[l] < 0 {
                continue;
            }
            let Some(rp) = self.refs[l].get(refs[l] as usize) else {
                continue;
            };
            let [pc0, pc1] = &mut s.pred_c[l];
            predict_block::<M, false>(
                &rp.frame,
                mvs[l],
                px,
                py,
                bw,
                bh,
                (&mut s.pred[l][..], 16),
                [&mut pc0[..], &mut pc1[..]],
                8,
            );
        }
        M::weighted(
            &mut self.pic.planes.y[py * ys + px..],
            ys,
            if use0 { Some(&s.pred[0][..]) } else { None },
            if use1 { Some(&s.pred[1][..]) } else { None },
            16,
            bw,
            bh,
            wts[0],
        );
        for c in 0..2 {
            let wc = wts[c + 1];
            let a = if use0 {
                Some(&s.pred_c[0][c][..])
            } else {
                None
            };
            let b = if use1 {
                Some(&s.pred_c[1][c][..])
            } else {
                None
            };
            let plane = if c == 0 {
                &mut self.pic.planes.cb
            } else {
                &mut self.pic.planes.cr
            };
            M::weighted(&mut plane[cy * cs + cx..], cs, a, b, 8, bw / 2, bh / 2, wc);
        }
    }

    fn uniform_motion(&self) -> bool {
        let st = self.mb();
        (0..2).all(|l| {
            let r = st.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && uniform_mv(&st.mv[l])
        })
    }

    pub fn prefetch_refs(&self) {
        let st = self.mb();
        let (px, py) = ((self.mb_x * 16) as i32, (self.mb_y * 16) as i32);
        for l in 0..2 {
            let ri = st.ref_idx[l][0];
            if ri < 0 {
                continue;
            }
            if let Some(rp) = self.refs[l].get(ri as usize) {
                let mv = st.mv[l][0];
                rp.frame
                    .prefetch(px + (mv[0] as i32 >> 2), py + (mv[1] as i32 >> 2));
            }
        }
    }

    pub fn predict_inter(&mut self) {
        self.prefetch_refs();
        #[cfg(target_arch = "x86_64")]
        {
            if crate::h264::dec::simd::avx2() {
                return unsafe { self.predict_inter_avx2() };
            }
            self.predict_with::<crate::h264::dec::simd::Sse2>()
        }
        #[cfg(not(target_arch = "x86_64"))]
        self.predict_with::<crate::h264::dec::inter::Scalar>()
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn predict_inter_avx2(&mut self) {
        self.predict_with::<crate::h264::dec::simd::Avx2>()
    }

    #[inline(always)]
    fn predict_with<M: Mc>(&mut self) {
        let info = self.cur.info;
        let direct_unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
        match info.kind {
            MbKind::PSkip => self.mc_rect::<M>(0, 0, 4, 4),
            MbKind::BSkip | MbKind::BDirect16x16 if self.uniform_motion() => {
                self.mc_rect::<M>(0, 0, 4, 4)
            }
            MbKind::BSkip | MbKind::BDirect16x16 => {
                for y in (0..4).step_by(direct_unit) {
                    for x in (0..4).step_by(direct_unit) {
                        self.mc_rect::<M>(x, y, direct_unit, direct_unit);
                    }
                }
            }
            _ => {
                if info.part != Part::P8x8 {
                    for p in 0..info.part.num_parts() {
                        let (x, y, w, h) = info.part.rect(p);
                        self.mc_rect::<M>(x, y, w, h);
                    }
                } else {
                    for p in 0..4 {
                        let sub = self.cur.sub[p];
                        let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
                        if sub.direct {
                            for y in (0..2).step_by(direct_unit) {
                                for x in (0..2).step_by(direct_unit) {
                                    self.mc_rect::<M>(x8 + x, y8 + y, direct_unit, direct_unit);
                                }
                            }
                        } else {
                            for j in 0..sub.shape.num_parts() {
                                let (sx, sy, w, h) = sub.shape.rect(j);
                                self.mc_rect::<M>(x8 + sx, y8 + sy, w, h);
                            }
                        }
                    }
                }
            }
        }
    }

    fn intra_mode_nb(&self, n: usize, bx: usize, by: usize) -> Option<u8> {
        let m = self.nb[n]?;
        let st = &self.pic.mbs[m];
        if self.pps.constrained_intra_pred && !st.kind.is_intra() {
            return None;
        }
        Some(match st.kind {
            MbKind::I4x4 | MbKind::I8x8 => st.intra_modes[by * 4 + bx],
            _ => 2,
        })
    }

    pub fn derive_intra_modes(&mut self, is8x8: bool) {
        let step = if is8x8 { 2 } else { 1 };
        let nblk = if is8x8 { 4 } else { 16 };
        for i in 0..nblk {
            let (bx, by) = if is8x8 {
                ((i & 1) * 2, (i >> 1) * 2)
            } else {
                let (x, y) = BLK4_XY[i];
                (x as usize, y as usize)
            };
            let a = if bx > 0 {
                Some(self.mb().intra_modes[by * 4 + bx - 1])
            } else {
                self.intra_mode_nb(0, 3, by)
            };
            let b = if by > 0 {
                Some(self.mb().intra_modes[(by - 1) * 4 + bx])
            } else {
                self.intra_mode_nb(1, bx, 3)
            };
            let pred = match (a, b) {
                (Some(a), Some(b)) => a.min(b),
                _ => 2,
            };
            let rem = self.cur.rem_mode[i];
            let mode = if rem < 0 {
                pred
            } else if (rem as u8) < pred {
                rem as u8
            } else {
                rem as u8 + 1
            };
            let st = self.mb_mut();
            for yy in by..by + step {
                for xx in bx..bx + step {
                    st.intra_modes[yy * 4 + xx] = mode;
                }
            }
        }
    }

    fn add_luma_block4(&mut self, raster: usize) {
        if self.s.blk_nz & (1 << raster) == 0 {
            return;
        }
        let stride = self.pic.planes.width;
        let px = self.mb_x * 16 + (raster & 3) * 4;
        let py = self.mb_y * 16 + (raster >> 2) * 4;
        let Some(blk) = self
            .s
            .coef
            .get_mut(raster * 16..)
            .and_then(|c| c.first_chunk_mut::<16>())
        else {
            return;
        };
        transform::idct4_add(blk, &mut self.pic.planes.y[py * stride + px..], stride);
        blk.fill(0);
    }

    fn add_luma_block8(&mut self, b8: usize) {
        if self.s.blk_nz & (1 << b8) == 0 {
            return;
        }
        let stride = self.pic.planes.width;
        let px = self.mb_x * 16 + (b8 & 1) * 8;
        let py = self.mb_y * 16 + (b8 >> 1) * 8;
        let Some(blk) = self
            .s
            .coef
            .get_mut(b8 * 64..)
            .and_then(|c| c.first_chunk_mut::<64>())
        else {
            return;
        };
        transform::idct8_add(blk, &mut self.pic.planes.y[py * stride + px..], stride);
        blk.fill(0);
    }

    fn add_chroma(&mut self) {
        let stride = self.pic.planes.cwidth;
        for c in 0..2 {
            if self.s.c_nz[c] == 0 {
                continue;
            }
            for b in 0..4 {
                if self.s.c_nz[c] & (1 << b) == 0 {
                    continue;
                }
                let px = self.mb_x * 8 + (b & 1) * 4;
                let py = self.mb_y * 8 + (b >> 1) * 4;
                let Some(blk) = self.s.coef_c[c]
                    .get_mut(b * 16..)
                    .and_then(|c| c.first_chunk_mut::<16>())
                else {
                    continue;
                };
                let plane = if c == 0 {
                    &mut self.pic.planes.cb
                } else {
                    &mut self.pic.planes.cr
                };
                transform::idct4_add(blk, &mut plane[py * stride + px..], stride);
                blk.fill(0);
            }
            self.s.c_nz[c] = 0;
        }
    }

    fn luma_avail4(&self, bx: usize, by: usize) -> Avail {
        let a = self.intra_nb(0);
        let b = self.intra_nb(1);
        let c = self.intra_nb(2);
        let d = self.intra_nb(3);
        let left = if bx > 0 { true } else { a };
        let top = if by > 0 { true } else { b };
        let top_left = match (bx > 0, by > 0) {
            (true, true) => true,
            (false, true) => a,
            (true, false) => b,
            (false, false) => d,
        };
        let top_right = if by == 0 {
            if bx < 3 { b } else { c }
        } else if bx == 3 {
            false
        } else {
            RASTER_TO_BLK4[(by - 1) * 4 + bx + 1] < RASTER_TO_BLK4[by * 4 + bx]
        };
        Avail {
            left,
            top,
            top_left,
            top_right,
        }
    }

    pub fn reconstruct_intra(&mut self) {
        let kind = self.mb().kind;
        let stride = self.pic.planes.width;
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        match kind {
            MbKind::I4x4 => {
                for (blk, &(bx, by)) in BLK4_XY.iter().enumerate() {
                    let (bx, by) = (bx as usize, by as usize);
                    let _ = blk;
                    let av = self.luma_avail4(bx, by);
                    let mode = self.mb().intra_modes[by * 4 + bx];
                    let (p, yy) = self.pic.planes.y.near(stride, y0 + by * 4);
                    intra::pred4x4(p, stride, x0 + bx * 4, yy, mode, av);
                    self.add_luma_block4(by * 4 + bx);
                }
            }
            MbKind::I8x8 => {
                for b8 in 0..4 {
                    let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
                    let mut av = self.luma_avail4(bx, by);
                    av.top_right = match b8 {
                        0 => self.intra_nb(1),
                        1 => self.intra_nb(2),
                        2 => true,
                        _ => false,
                    };
                    let mode = self.mb().intra_modes[by * 4 + bx];
                    let (p, yy) = self.pic.planes.y.near(stride, y0 + by * 4);
                    intra::pred8x8(p, stride, x0 + bx * 4, yy, mode, av);
                    self.add_luma_block8(b8);
                }
            }
            MbKind::I16x16 => {
                let av = Avail {
                    left: self.intra_nb(0),
                    top: self.intra_nb(1),
                    top_left: self.intra_nb(3),
                    top_right: false,
                };
                let mode = self.cur.info.i16_mode;
                let (p, yy) = self.pic.planes.y.near(stride, y0);
                intra::pred16x16(p, stride, x0, yy, mode, av);
                for r in 0..16 {
                    self.add_luma_block4(r);
                }
            }
            _ => {}
        }
        let av = Avail {
            left: self.intra_nb(0),
            top: self.intra_nb(1),
            top_left: self.intra_nb(3),
            top_right: false,
        };
        let cs = self.pic.planes.cwidth;
        let mode = self.mb().intra_chroma_mode;
        let (cx, cy) = (self.mb_x * 8, self.mb_y * 8);
        let (p, yy) = self.pic.planes.cb.near(cs, cy);
        intra::pred_chroma(p, cs, cx, yy, mode, av);
        let (p, yy) = self.pic.planes.cr.near(cs, cy);
        intra::pred_chroma(p, cs, cx, yy, mode, av);
        self.add_chroma();
        self.s.blk_nz = 0;
    }

    pub fn reconstruct_inter_residual(&mut self) {
        if self.mb().transform_8x8 {
            for b8 in 0..4 {
                self.add_luma_block8(b8);
            }
        } else if self.s.blk_nz != 0 {
            for r in 0..16 {
                self.add_luma_block4(r);
            }
        }
        self.add_chroma();
        self.s.blk_nz = 0;
    }

    pub fn write_pcm(&mut self, samples: &[u8]) {
        let stride = self.pic.planes.width;
        let (x0, y0) = (self.mb_x * 16, self.mb_y * 16);
        for r in 0..16 {
            self.pic.planes.y[(y0 + r) * stride + x0..(y0 + r) * stride + x0 + 16]
                .copy_from_slice(&samples[r * 16..r * 16 + 16]);
        }
        let cs = self.pic.planes.cwidth;
        let (cx, cy) = (self.mb_x * 8, self.mb_y * 8);
        for c in 0..2 {
            let src = &samples[256 + c * 64..256 + c * 64 + 64];
            let plane = if c == 0 {
                &mut self.pic.planes.cb
            } else {
                &mut self.pic.planes.cr
            };
            for r in 0..8 {
                plane[(cy + r) * cs + cx..(cy + r) * cs + cx + 8]
                    .copy_from_slice(&src[r * 8..r * 8 + 8]);
            }
        }
    }

    pub fn finish_pcm(&mut self) {
        let qpc = [self.chroma_qp(0, 0) as u8, self.chroma_qp(0, 1) as u8];
        let st = self.mb_mut();
        st.kind = MbKind::IPcm;
        st.qp = 0;
        st.qpc = qpc;
        st.nnz = [16; 16];
        st.nnz_c = [[16; 4]; 2];
        st.cbf_dc = 7;
        st.cbp = 0x2f;
        st.nz_mask = 0xffff;
        st.ref_idx = [[-1; 4]; 2];
    }

    pub fn apply_qp_delta(&mut self, delta: i32) -> Result<()> {
        ensure!(
            (-26..=25).contains(&delta),
            "mb_qp_delta {delta} out of range"
        );
        self.qp = (self.qp + delta + 52) % 52;
        Ok(())
    }

    pub fn store_qp(&mut self) {
        let qp = self.qp;
        let qpc = [self.chroma_qp(qp, 0) as u8, self.chroma_qp(qp, 1) as u8];
        let st = self.mb_mut();
        st.qp = qp as u8;
        st.qpc = qpc;
    }

    fn luma_list4(&self) -> usize {
        if self.mb().kind.is_intra() { 0 } else { 3 }
    }

    pub fn put_luma4(&mut self, raster: usize, levels: &[i32; 16], ac_only: bool) {
        let qp = self.qp;
        let ls = &self.ls.ls4[self.luma_list4()][(qp % 6) as usize];
        let blk = &mut self.s.coef[raster * 16..raster * 16 + 16];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate().skip(ac_only as usize) {
            if lv != 0 {
                let p = ZIGZAG4[k] as usize;
                blk[p] = transform::scale4(lv, ls[p], qp);
                any = true;
            }
        }
        if any {
            self.s.blk_nz |= 1 << raster;
            self.pic.mbs[self.mb_addr].nz_mask |= 1 << raster;
        }
    }

    pub fn put_luma8(&mut self, b8: usize, levels: &[i32; 64]) {
        let qp = self.qp;
        let list = if self.mb().kind.is_intra() { 0 } else { 1 };
        let ls = &self.ls.ls8[list][(qp % 6) as usize];
        let blk = &mut self.s.coef[b8 * 64..b8 * 64 + 64];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate() {
            if lv != 0 {
                let p = ZIGZAG8[k] as usize;
                blk[p] = transform::scale8(lv, ls[p], qp);
                any = true;
            }
        }
        if any {
            self.s.blk_nz |= 1 << b8;
            let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            let m = (0b11u16 << (by * 4 + bx)) | (0b11u16 << ((by + 1) * 4 + bx));
            self.pic.mbs[self.mb_addr].nz_mask |= m;
        }
    }

    pub fn put_luma_dc(&mut self, levels: &[i32; 16]) {
        let mut c = [0i32; 16];
        for (k, &lv) in levels.iter().enumerate() {
            c[ZIGZAG4[k] as usize] = lv;
        }
        let qp = self.qp;
        let ls00 = self.ls.ls4[0][(qp % 6) as usize][0];
        transform::luma_dc_dequant(&mut c, qp, ls00);
        for (r, &dc) in c.iter().enumerate() {
            if dc != 0 {
                self.s.coef[r * 16] = dc;
                self.s.blk_nz |= 1 << r;
            }
        }
    }

    pub fn put_chroma_dc(&mut self, c: usize, levels: &[i32; 4]) {
        let mut v = *levels;
        let qpc = self.chroma_qp(self.qp, c);
        let list = if self.mb().kind.is_intra() {
            1 + c
        } else {
            4 + c
        };
        let ls00 = self.ls.ls4[list][(qpc % 6) as usize][0];
        transform::chroma_dc_dequant_420(&mut v, qpc, ls00);
        for (b, &dc) in v.iter().enumerate() {
            if dc != 0 {
                self.s.coef_c[c][b * 16] = dc;
                self.s.c_nz[c] |= 1 << b;
            }
        }
    }

    pub fn put_chroma_ac(&mut self, c: usize, b: usize, levels: &[i32; 16]) {
        let qpc = self.chroma_qp(self.qp, c);
        let list = if self.mb().kind.is_intra() {
            1 + c
        } else {
            4 + c
        };
        let ls = &self.ls.ls4[list][(qpc % 6) as usize];
        let blk = &mut self.s.coef_c[c][b * 16..b * 16 + 16];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate().skip(1) {
            if lv != 0 {
                let p = ZIGZAG4[k] as usize;
                blk[p] = transform::scale4(lv, ls[p], qpc);
                any = true;
            }
        }
        if any {
            self.s.c_nz[c] |= 1 << b;
        }
    }

    pub fn decode_skip(&mut self) -> Result<()> {
        let b = self.sh.slice_type.is_b();
        self.cur = MbCur::default();
        self.cur.info.kind = if b { MbKind::BSkip } else { MbKind::PSkip };
        self.mb_mut().kind = self.cur.info.kind;
        self.store_qp();
        if b {
            self.derive_direct(0xf)?;
        } else {
            ensure!(!self.refs[0].is_empty(), "P_Skip with empty reference list");
            self.derive_p_skip();
        }
        self.predict_inter();
        self.prev_qp_delta_nz = false;
        Ok(())
    }

    fn read_te(r: &mut BitReader, range: u32) -> Result<u32> {
        Ok(if range > 1 {
            r.read_ue()?
        } else {
            !r.read_bit()? as u32
        })
    }

    pub fn decode_mb_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let mb_type = r.read_ue()?;
        let st = self.sh.slice_type;
        let info = match st {
            SliceType::I | SliceType::Si => intra_mb_type(mb_type),
            SliceType::P | SliceType::Sp => {
                if mb_type < 5 {
                    p_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 5)
                }
            }
            SliceType::B => {
                if mb_type < 23 {
                    b_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 23)
                }
            }
        };
        let Some(mut info) = info else {
            return invalid(format!("mb_type {mb_type} out of range"));
        };
        self.cur = MbCur {
            info,
            ..Default::default()
        };
        if info.kind == MbKind::IPcm {
            r.byte_align();
            let pos = r.byte_pos();
            let data = r.data();
            ensure!(data.len() >= pos + 384, "truncated I_PCM macroblock");
            let Some(&samples) = data.get(pos..).and_then(|d| d.first_chunk::<384>()) else {
                return invalid("truncated I_PCM macroblock");
            };
            r.skip(384 * 8)?;
            self.finish_pcm();
            self.write_pcm(&samples);
            return Ok(());
        }
        let mut transform_8x8 = false;
        if info.kind == MbKind::I4x4 && self.pps.transform_8x8_mode && r.read_flag()? {
            transform_8x8 = true;
            info.kind = MbKind::I8x8;
            self.cur.info.kind = MbKind::I8x8;
        }
        self.mb_mut().kind = info.kind;
        self.mb_mut().transform_8x8 = transform_8x8;
        let n_ref = [self.sh.num_ref_idx_active[0], self.sh.num_ref_idx_active[1]];
        match info.kind {
            MbKind::I4x4 | MbKind::I8x8 => {
                let n = if info.kind == MbKind::I8x8 { 4 } else { 16 };
                for i in 0..n {
                    self.cur.rem_mode[i] = if r.read_flag()? {
                        -1
                    } else {
                        r.read_bits(3)? as i8
                    };
                }
                self.derive_intra_modes(info.kind == MbKind::I8x8);
                let m = r.read_ue()?;
                ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                self.mb_mut().intra_chroma_mode = m as u8;
            }
            MbKind::I16x16 => {
                let m = r.read_ue()?;
                ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                self.mb_mut().intra_chroma_mode = m as u8;
            }
            MbKind::BDirect16x16 => {}
            _ => {
                if info.part == Part::P8x8 {
                    for p in 0..4 {
                        let t = r.read_ue()?;
                        let sub = if st.is_b() {
                            b_sub_mb_type(t)
                        } else {
                            p_sub_mb_type(t)
                        };
                        let Some(sub) = sub else {
                            return invalid(format!("sub_mb_type {t} out of range"));
                        };
                        self.cur.sub[p] = sub;
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            self.cur.ref_idx[l][p] = if n_ref[l] > 1 && !info.ref0 {
                                Self::read_te(r, n_ref[l] - 1)? as i8
                            } else {
                                0
                            };
                        }
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            for j in 0..sub.shape.num_parts() {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4 + j] = [x as i16, y as i16];
                            }
                        }
                    }
                } else {
                    let np = info.part.num_parts();
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                self.cur.ref_idx[l][p] = if n_ref[l] > 1 {
                                    Self::read_te(r, n_ref[l] - 1)? as i8
                                } else {
                                    0
                                };
                            }
                        }
                    }
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4] = [x as i16, y as i16];
                            }
                        }
                    }
                }
            }
        }
        let cbp = if info.kind == MbKind::I16x16 {
            info.i16_cbp
        } else {
            let code = r.read_ue()?;
            ensure!(code < 48, "coded_block_pattern out of range");
            let (i, p) = CBP_ME[code as usize];
            if info.kind.is_intra() { i } else { p }
        };
        self.cur.cbp = cbp;
        self.mb_mut().cbp = cbp;
        if !info.kind.is_intra()
            && cbp & 15 != 0
            && self.pps.transform_8x8_mode
            && self.no_sub_8x8_lt()
        {
            let t = r.read_flag()?;
            self.mb_mut().transform_8x8 = t;
        }
        if !info.kind.is_intra() {
            self.derive_inter_motion()?;
        }
        if cbp != 0 || info.kind == MbKind::I16x16 {
            let d = r.read_se()?;
            self.apply_qp_delta(d)?;
        }
        self.store_qp();
        self.residual_cavlc(r, cbp)?;
        if info.kind.is_intra() {
            self.reconstruct_intra();
        } else {
            self.predict_inter();
            self.reconstruct_inter_residual();
        }
        Ok(())
    }

    pub fn no_sub_8x8_lt(&self) -> bool {
        let info = self.cur.info;
        match info.kind {
            MbKind::BDirect16x16 => self.sps.direct_8x8_inference,
            MbKind::Inter if info.part == Part::P8x8 => self.cur.sub.iter().all(|s| {
                if s.direct {
                    self.sps.direct_8x8_inference
                } else {
                    s.shape == SubPart::S8x8
                }
            }),
            _ => true,
        }
    }

    fn residual_cavlc(&mut self, r: &mut BitReader, cbp: u8) -> Result<()> {
        let kind = self.mb().kind;
        let t8 = self.mb().transform_8x8;
        if kind == MbKind::I16x16 {
            let nc = self.nc_luma(0);
            let mut lv = [0i32; 16];
            cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
            self.put_luma_dc(&lv);
        }
        for b8 in 0..4 {
            let (bx8, by8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            if cbp & (1 << b8) == 0 {
                continue;
            }
            if t8 {
                let mut lv8 = [0i32; 64];
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
                    self.mb_mut().nnz[raster] = tc;
                    for (i, &v) in lv.iter().enumerate() {
                        lv8[4 * i + i4] = v;
                    }
                }
                self.put_luma8(b8, &lv8);
            } else {
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = if kind == MbKind::I16x16 {
                        cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?
                    } else {
                        cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?
                    };
                    self.mb_mut().nnz[raster] = tc;
                    self.put_luma4(raster, &lv, kind == MbKind::I16x16);
                }
            }
        }
        let cbp_c = cbp >> 4;
        if cbp_c & 3 != 0 {
            for c in 0..2 {
                let mut lv = [0i32; 4];
                cavlc::residual_block(r, &mut lv, 0, 3, 4, -1)?;
                self.put_chroma_dc(c, &lv);
            }
        }
        if cbp_c & 2 != 0 {
            for c in 0..2 {
                for b in 0..4 {
                    let nc = self.nc_chroma(c, b);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?;
                    self.mb_mut().nnz_c[c][b] = tc;
                    self.put_chroma_ac(c, b, &lv);
                }
            }
        }
        Ok(())
    }

    pub fn decode_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let total = self.mb_w * self.mb_h;
        let mut addr = self.sh.first_mb_in_slice as usize;
        let intra_slice = self.sh.slice_type.is_intra();
        loop {
            ensure!(addr < total, "macroblock address {addr} out of range");
            if !intra_slice {
                let run = r.read_ue()? as usize;
                ensure!(addr + run <= total, "mb_skip_run out of range");
                for _ in 0..run {
                    self.start_mb(addr)?;
                    self.decode_skip()?;
                    self.pic.mb_done(addr);
                    addr += 1;
                }
                if run > 0 && !r.more_rbsp_data() {
                    break;
                }
                ensure!(addr < total, "macroblock address {addr} out of range");
            }
            self.start_mb(addr)?;
            self.decode_mb_cavlc(r)?;
            self.pic.mb_done(addr);
            addr += 1;
            if !r.more_rbsp_data() {
                break;
            }
        }
        Ok(())
    }
}
