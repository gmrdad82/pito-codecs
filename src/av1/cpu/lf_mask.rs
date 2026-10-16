use std::cmp;
use std::ffi::c_int;
use std::mem::MaybeUninit;

use crate::av1::cpu::pod::FromBytes;
use crate::av1::cpu::sync::RwLock;

use crate::av1::cpu::align::{Align16, AlignedVec2, ArrayDefault};
use crate::av1::cpu::ctx::CaseSet;
use crate::av1::cpu::disjoint_mut::DisjointMut;
use crate::av1::cpu::include::common::intops::{clip, iclip};
use crate::av1::cpu::include::dav1d::headers::{
    Rav1dFrameHeader, Rav1dLoopfilterModeRefDeltas, Rav1dPixelLayout, Rav1dRestorationType,
};
use crate::av1::cpu::internal::Bxy;
use crate::av1::cpu::levels::{BlockSize, SegmentId, TxfmSize};
use crate::av1::cpu::relaxed_atomic::RelaxedAtomic;
use crate::av1::cpu::tables::DAV1D_TXFM_DIMENSIONS;

#[repr(C)]
pub struct Av1FilterLUT {
    pub e: [u8; 64],
    pub i: [u8; 64],
    pub sharp: [u64; 2],
}

impl Default for Av1FilterLUT {
    fn default() -> Self {
        Self {
            e: [0; 64],
            i: [0; 64],
            sharp: Default::default(),
        }
    }
}

impl ArrayDefault for Av1FilterLUT {
    fn default() -> Self {
        Default::default()
    }
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Av1RestorationUnit {
    pub r#type: Rav1dRestorationType,
    pub filter_h: [i8; 3],
    pub filter_v: [i8; 3],
    pub sgr_weights: [i8; 2],
}

#[derive(Default)]
#[repr(C)]
pub struct Av1Filter {
    pub filter_y: [[[[RelaxedAtomic<u16>; 2]; 3]; 32]; 2],
    pub filter_uv: [[[[RelaxedAtomic<u16>; 2]; 2]; 32]; 2],

    pub cdef_idx: [RelaxedAtomic<i8>; 4],

    pub noskip_mask: [[RelaxedAtomic<u16>; 2]; 16],
}

#[derive(Default)]
#[repr(C)]
pub struct Av1Restoration {
    pub lr: [[RwLock<Av1RestorationUnit>; 4]; 3],
}

fn decomp_tx(
    txa: &mut [[[[MaybeUninit<u8>; 32]; 32]; 2]; 2],
    from: TxfmSize,
    depth: usize,
    y_off: u8,
    x_off: u8,
    tx_masks: &[u16; 2],
) {
    debug_assert!(depth <= 2);
    let t_dim = &DAV1D_TXFM_DIMENSIONS[from as usize];

    let y0 = (y_off * t_dim.h) as usize;
    let x0 = (x_off * t_dim.w) as usize;

    let is_split = if from == TxfmSize::S4x4 || depth > 1 {
        false
    } else {
        (tx_masks[depth] >> (y_off * 4 + x_off)) & 1 != 0
    };
    if is_split {
        let sub = t_dim.sub;

        decomp_tx(txa, sub, depth + 1, y_off * 2 + 0, x_off * 2 + 0, tx_masks);
        if t_dim.w >= t_dim.h {
            decomp_tx(txa, sub, depth + 1, y_off * 2 + 0, x_off * 2 + 1, tx_masks);
        }
        if t_dim.h >= t_dim.w {
            decomp_tx(txa, sub, depth + 1, y_off * 2 + 1, x_off * 2 + 0, tx_masks);
            if t_dim.w >= t_dim.h {
                decomp_tx(txa, sub, depth + 1, y_off * 2 + 1, x_off * 2 + 1, tx_masks);
            }
        }
    } else {
        let lw = cmp::min(2, t_dim.lw);
        let lh = cmp::min(2, t_dim.lh);

        debug_assert!(t_dim.w == 1 << t_dim.lw && t_dim.w <= 16);
        CaseSet::<16, false>::one((), t_dim.w as usize, x0, |case, ()| {
            for y in 0..t_dim.h as usize {
                case.set(&mut txa[0][0][y0 + y], MaybeUninit::new(lw));
                case.set(&mut txa[1][0][y0 + y], MaybeUninit::new(lh));
                txa[0][1][y0 + y][x0].write(t_dim.w);
            }
        });
        CaseSet::<16, false>::one((), t_dim.w as usize, x0, |case, ()| {
            case.set(&mut txa[1][1][y0], MaybeUninit::new(t_dim.h));
        });
    };
}

#[inline]
fn mask_edges_inter(
    masks: &[[[[RelaxedAtomic<u16>; 2]; 3]; 32]; 2],
    by4: usize,
    bx4: usize,
    w4: usize,
    h4: usize,
    skip: bool,
    max_tx: TxfmSize,
    tx_masks: &[u16; 2],
    a: &mut [u8],
    l: &mut [u8],
) {
    let t_dim = &DAV1D_TXFM_DIMENSIONS[max_tx as usize];

    let mut txa = Align16([[[[MaybeUninit::uninit(); 32]; 32]; 2]; 2]);

    for (y_off, _) in (0..h4).step_by(t_dim.h as usize).enumerate() {
        for (x_off, _) in (0..w4).step_by(t_dim.w as usize).enumerate() {
            decomp_tx(&mut txa.0, max_tx, 0, y_off as u8, x_off as u8, tx_masks);
        }
    }

    for y in 0..h4 {
        let mask = 1u32 << (by4 + y);
        let sidx = (mask >= 0x10000) as usize;
        let smask = mask >> (sidx << 4);

        let txa_y = unsafe { txa[0][0][y][0].assume_init() };
        masks[0][bx4][cmp::min(txa_y, l[y]) as usize][sidx].update(|it| it | smask as u16);
    }

    for x in 0..w4 {
        let mask = 1u32 << (bx4 + x);
        let sidx = (mask >= 0x10000) as usize;
        let smask = mask >> (sidx << 4);

        let txa_x = unsafe { txa[1][0][0][x].assume_init() };
        masks[1][by4][cmp::min(txa_x, a[x]) as usize][sidx].update(|it| it | smask as u16);
    }
    if !skip {
        for y in 0..h4 {
            let mask = 1u32 << (by4 + y);
            let sidx = (mask >= 0x10000) as usize;
            let smask = mask >> (sidx << 4);

            let mut ltx = unsafe { txa[0][0][y][0].assume_init() };

            let step = unsafe { txa[0][1][y][0].assume_init() } as usize;
            let mut x = step;
            while x < w4 {
                let rtx = unsafe { txa[0][0][y][x].assume_init() };
                masks[0][bx4 + x][cmp::min(rtx, ltx) as usize][sidx].update(|it| it | smask as u16);
                ltx = rtx;

                let step = unsafe { txa[0][1][y][x].assume_init() } as usize;
                x += step;
            }
        }

        for x in 0..w4 {
            let mask = 1u32 << (bx4 + x);
            let sidx = (mask >= 0x10000) as usize;
            let smask = mask >> (sidx << 4);

            let mut ttx = unsafe { txa[1][0][0][x].assume_init() };

            let step = unsafe { txa[1][1][0][x].assume_init() } as usize;
            let mut y = step;
            while y < h4 {
                let btx = unsafe { txa[1][0][y][x].assume_init() };
                masks[1][by4 + y][cmp::min(ttx, btx) as usize][sidx].update(|it| it | smask as u16);
                ttx = btx;

                let step = unsafe { txa[1][1][y][x].assume_init() } as usize;
                y += step;
            }
        }
    }

    for y in 0..h4 {
        l[y] = unsafe { txa[0][0][y][w4 - 1].assume_init() };
    }

    let txa_slice =
        unsafe { &*(&txa[1][0][h4 - 1][..w4] as *const [MaybeUninit<u8>] as *const [u8]) };
    a[..w4].copy_from_slice(txa_slice);
}

#[inline]
fn mask_edges_intra(
    masks: &[[[[RelaxedAtomic<u16>; 2]; 3]; 32]; 2],
    by4: usize,
    bx4: usize,
    w4: usize,
    h4: usize,
    tx: TxfmSize,
    a: &mut [u8],
    l: &mut [u8],
) {
    let t_dim = &DAV1D_TXFM_DIMENSIONS[tx as usize];
    let twl4 = t_dim.lw;
    let thl4 = t_dim.lh;
    let twl4c = cmp::min(2, twl4);
    let thl4c = cmp::min(2, thl4);

    for y in 0..h4 {
        let mask = 1u32 << (by4 + y);
        let sidx = (mask >= 0x10000) as usize;
        let smask = mask >> (sidx << 4);
        masks[0][bx4][cmp::min(twl4c, l[y]) as usize][sidx].update(|it| it | smask as u16);
    }

    for x in 0..w4 {
        let mask = 1u32 << (bx4 + x);
        let sidx = (mask >= 0x10000) as usize;
        let smask = mask >> (sidx << 4);

        masks[1][by4][cmp::min(thl4c, a[x]) as usize][sidx].update(|it| it | smask as u16);
    }

    let hstep = t_dim.w as usize;
    let t = 1u32 << by4;
    let inner = (((t as u64) << h4) - (t as u64)) as u32;
    let inner = [inner as u16, (inner >> 16) as u16];
    for x in (hstep..w4).step_by(hstep) {
        if inner[0] != 0 {
            masks[0][bx4 + x][twl4c as usize][0].update(|it| it | inner[0]);
        }
        if inner[1] != 0 {
            masks[0][bx4 + x][twl4c as usize][1].update(|it| it | inner[1]);
        }
    }

    let vstep = t_dim.h as usize;
    let t = 1u32 << bx4;
    let inner = (((t as u64) << w4) - (t as u64)) as u32;
    let inner = [inner as u16, (inner >> 16) as u16];
    for y in (vstep..h4).step_by(vstep) {
        if inner[0] != 0 {
            masks[1][by4 + y][thl4c as usize][0].update(|it| it | inner[0]);
        }
        if inner[1] != 0 {
            masks[1][by4 + y][thl4c as usize][1].update(|it| it | inner[1]);
        }
    }

    CaseSet::<32, true>::many(
        [(a, thl4c), (l, twl4c)],
        [w4 as usize, h4 as usize],
        [0, 0],
        |case, (dir, tl4c)| {
            case.set(dir, tl4c);
        },
    );
}

fn mask_edges_chroma(
    masks: &[[[[RelaxedAtomic<u16>; 2]; 2]; 32]; 2],
    cby4: usize,
    cbx4: usize,
    cw4: usize,
    ch4: usize,
    skip_inter: bool,
    tx: TxfmSize,
    a: &mut [u8],
    l: &mut [u8],
    ss_hor: usize,
    ss_ver: usize,
) {
    let t_dim = &DAV1D_TXFM_DIMENSIONS[tx as usize];
    let twl4 = t_dim.lw;
    let thl4 = t_dim.lh;
    let twl4c = (twl4 != 0) as u8;
    let thl4c = (thl4 != 0) as u8;
    let vbits = 4 - ss_ver;
    let hbits = 4 - ss_hor;
    let vmask = 16 >> ss_ver;
    let hmask = 16 >> ss_hor;
    let vmax = 1u32 << vmask;
    let hmax = 1u32 << hmask;

    for y in 0..ch4 {
        let mask = 1u32 << (cby4 + y);
        let sidx = (mask >= vmax) as usize;
        let smask = mask >> (sidx << vbits);
        masks[0][cbx4][cmp::min(twl4c, l[y]) as usize][sidx].update(|it| it | smask as u16);
    }

    for x in 0..cw4 {
        let mask = 1u32 << (cbx4 + x);
        let sidx = (mask >= hmax) as usize;
        let smask = mask >> (sidx << hbits);
        masks[1][cby4][cmp::min(thl4c, a[x]) as usize][sidx].update(|it| it | smask as u16);
    }

    if !skip_inter {
        let hstep = t_dim.w as usize;
        let t = 1u32 << cby4;
        let inner = (((t as u64) << ch4) - (t as u64)) as u32;
        let inner = [(inner & ((1 << vmask) - 1)) as u16, (inner >> vmask) as u16];
        for x in (hstep..cw4).step_by(hstep) {
            if inner[0] != 0 {
                masks[0][cbx4 + x][twl4c as usize][0].update(|it| it | inner[0]);
            }
            if inner[1] != 0 {
                masks[0][cbx4 + x][twl4c as usize][1].update(|it| it | inner[1]);
            }
        }

        let vstep = t_dim.h as usize;
        let t = 1u32 << cbx4;
        let inner = (((t as u64) << cw4) - (t as u64)) as u32;
        let inner = [(inner & ((1 << hmask) - 1)) as u16, (inner >> hmask) as u16];
        for y in (vstep..ch4).step_by(vstep) {
            if inner[0] != 0 {
                masks[1][cby4 + y][thl4c as usize][0].update(|it| it | inner[0]);
            }
            if inner[1] != 0 {
                masks[1][cby4 + y][thl4c as usize][1].update(|it| it | inner[1]);
            }
        }
    }

    CaseSet::<32, true>::many(
        [(a, thl4c), (l, twl4c)],
        [cw4 as usize, ch4 as usize],
        [0, 0],
        |case, (dir, tl4c)| {
            case.set(dir, tl4c);
        },
    );
}

#[inline(never)]
pub(crate) fn rav1d_create_lf_mask_intra(
    lflvl: &Av1Filter,
    level_cache: &DisjointMut<AlignedVec2<u8>>,
    b4_stride: isize,
    filter_level: &Align16<[[[u8; 2]; 8]; 4]>,
    b: Bxy,
    iw: c_int,
    ih: c_int,
    bs: BlockSize,
    ytx: TxfmSize,
    uvtx: TxfmSize,
    layout: Rav1dPixelLayout,
    ay: &mut [u8],
    ly: &mut [u8],
    aluv: Option<(&mut [u8], &mut [u8])>,
) {
    let b4_stride = b4_stride as usize;
    let [bx, by, iw, ih] = [b.x, b.y, iw, ih].map(|it| it as usize);

    let b_dim = bs.dimensions();
    let b_dim = b_dim.map(|it| it as usize);
    let bw4 = cmp::min(iw - bx, b_dim[0]);
    let bh4 = cmp::min(ih - by, b_dim[1]);
    let bx4 = bx & 31;
    let by4 = by & 31;

    let filter_level_yuv = filter_level.0.map(|a| a[0][0]);
    let [filter_level_y, filter_level_uv] = *<[u16; 2]>::ref_from(&filter_level_yuv).unwrap();

    if bw4 != 0 && bh4 != 0 {
        let mut level_cache_off = by * b4_stride + bx;
        for _y in 0..bh4 {
            for x in 0..bw4 {
                let idx = 4 * (level_cache_off + x);

                let lvl = &mut *level_cache.index_mut((idx + 0.., ..2));
                *u16::mut_from(lvl).unwrap() = filter_level_y;
            }
            level_cache_off += b4_stride;
        }

        mask_edges_intra(&lflvl.filter_y, by4, bx4, bw4, bh4, ytx, ay, ly);
    }

    let (auv, luv) = match aluv {
        None => return,
        Some(aluv) => aluv,
    };

    let ss_ver = (layout == Rav1dPixelLayout::I420) as usize;
    let ss_hor = (layout != Rav1dPixelLayout::I444) as usize;
    let cbw4 = cmp::min(
        (iw + ss_hor >> ss_hor) - (bx >> ss_hor),
        (b_dim[0] + ss_hor) >> ss_hor,
    );
    let cbh4 = cmp::min(
        (ih + ss_ver >> ss_ver) - (by >> ss_ver),
        (b_dim[1] + ss_ver) >> ss_ver,
    );

    if cbw4 == 0 || cbh4 == 0 {
        return;
    }

    let cbx4 = bx4 >> ss_hor;
    let cby4 = by4 >> ss_ver;

    let mut level_cache_off = (by >> ss_ver) * b4_stride + (bx >> ss_hor);
    for _y in 0..cbh4 {
        for x in 0..cbw4 {
            let idx = 4 * (level_cache_off + x);

            let lvl = &mut *level_cache.index_mut((idx + 2.., ..2));
            *u16::mut_from(lvl).unwrap() = filter_level_uv;
        }
        level_cache_off += b4_stride;
    }

    mask_edges_chroma(
        &lflvl.filter_uv,
        cby4,
        cbx4,
        cbw4,
        cbh4,
        false,
        uvtx,
        auv,
        luv,
        ss_hor,
        ss_ver,
    );
}

#[inline(never)]
pub(crate) fn rav1d_create_lf_mask_inter(
    lflvl: &Av1Filter,
    level_cache: &DisjointMut<AlignedVec2<u8>>,
    b4_stride: isize,
    filter_level: &Align16<[[[u8; 2]; 8]; 4]>,
    r#ref: usize,
    is_gmv: bool,
    b: Bxy,
    iw: c_int,
    ih: c_int,
    skip: bool,
    bs: BlockSize,
    max_ytx: TxfmSize,
    tx_masks: &[u16; 2],
    uvtx: TxfmSize,
    layout: Rav1dPixelLayout,
    ay: &mut [u8],
    ly: &mut [u8],
    aluv: Option<(&mut [u8], &mut [u8])>,
) {
    let b4_stride = b4_stride as usize;
    let is_gmv = is_gmv as usize;
    let [bx, by, iw, ih] = [b.x, b.y, iw, ih].map(|it| it as usize);

    let b_dim = bs.dimensions();
    let b_dim = b_dim.map(|it| it as usize);
    let bw4 = cmp::min(iw - bx, b_dim[0]);
    let bh4 = cmp::min(ih - by, b_dim[1]);
    let bx4 = bx & 31;
    let by4 = by & 31;

    let filter_level_yuv = filter_level.0.map(|a| a[r#ref][is_gmv]);
    let [filter_level_y, filter_level_uv] = *<[u16; 2]>::ref_from(&filter_level_yuv).unwrap();

    if bw4 != 0 && bh4 != 0 {
        let mut level_cache_off = by * b4_stride + bx;
        for _y in 0..bh4 {
            for x in 0..bw4 {
                let idx = 4 * (level_cache_off + x);

                let lvl = &mut *level_cache.index_mut((idx + 0.., ..2));
                *u16::mut_from(lvl).unwrap() = filter_level_y;
            }
            level_cache_off += b4_stride;
        }

        mask_edges_inter(
            &lflvl.filter_y,
            by4,
            bx4,
            bw4,
            bh4,
            skip,
            max_ytx,
            tx_masks,
            ay,
            ly,
        );
    }

    let (auv, luv) = match aluv {
        None => return,
        Some(aluv) => aluv,
    };

    let ss_ver = (layout == Rav1dPixelLayout::I420) as usize;
    let ss_hor = (layout != Rav1dPixelLayout::I444) as usize;
    let cbw4 = cmp::min(
        (iw + ss_hor >> ss_hor) - (bx >> ss_hor),
        (b_dim[0] + ss_hor) >> ss_hor,
    );
    let cbh4 = cmp::min(
        (ih + ss_ver >> ss_ver) - (by >> ss_ver),
        (b_dim[1] + ss_ver) >> ss_ver,
    );

    if cbw4 == 0 || cbh4 == 0 {
        return;
    }

    let cbx4 = bx4 >> ss_hor;
    let cby4 = by4 >> ss_ver;

    let mut level_cache_off = (by >> ss_ver) * b4_stride + (bx >> ss_hor);
    for _y in 0..cbh4 {
        for x in 0..cbw4 {
            let idx = 4 * (level_cache_off + x);

            let lvl = &mut *level_cache.index_mut((idx + 2.., ..2));
            *u16::mut_from(lvl).unwrap() = filter_level_uv;
        }
        level_cache_off += b4_stride;
    }

    mask_edges_chroma(
        &lflvl.filter_uv,
        cby4,
        cbx4,
        cbw4,
        cbh4,
        skip,
        uvtx,
        auv,
        luv,
        ss_hor,
        ss_ver,
    );
}

pub fn rav1d_calc_eih(lim_lut: &mut Av1FilterLUT, filter_sharpness: u8) {
    let sharp = filter_sharpness;
    for level in 0..64 {
        let mut limit = level;

        if sharp > 0 {
            limit >>= sharp + 3 >> 2;
            limit = cmp::min(limit, 9 - sharp);
        }
        limit = cmp::max(limit, 1);

        lim_lut.i[level as usize] = limit;
        lim_lut.e[level as usize] = 2 * (level + 2) + limit;
    }
    let sharp = sharp as u64;
    lim_lut.sharp[0] = sharp + 3 >> 2;
    lim_lut.sharp[1] = if sharp != 0 { 9 - sharp } else { 0xff };
}

fn calc_lf_value(
    lflvl_values: &mut [[u8; 2]; 8],
    base_lvl: u8,
    lf_delta: i8,
    seg_delta: i8,
    mr_delta: Option<&Rav1dLoopfilterModeRefDeltas>,
) {
    let base = iclip(
        iclip(base_lvl as c_int + lf_delta as c_int, 0, 63) + seg_delta as c_int,
        0,
        63,
    );

    if let Some(mr_delta) = mr_delta {
        let sh = (base >= 32) as c_int;
        lflvl_values[0] = [clip(base + mr_delta.ref_delta[0] as i32 * (1 << sh), 0, 63); 2];
        for r in 1..8 {
            for m in 0..2 {
                let delta = mr_delta.mode_delta[m] + mr_delta.ref_delta[r];
                lflvl_values[r][m] = clip(base + delta as i32 * (1 << sh), 0, 63);
            }
        }
    } else {
        *lflvl_values = [[base as u8; 2]; 8];
    }
}

#[inline]
fn calc_lf_value_chroma(
    lflvl_values: &mut [[u8; 2]; 8],
    base_lvl: u8,
    lf_delta: i8,
    seg_delta: i8,
    mr_delta: Option<&Rav1dLoopfilterModeRefDeltas>,
) {
    if base_lvl == 0 {
        *lflvl_values = Default::default();
    } else {
        calc_lf_value(lflvl_values, base_lvl, lf_delta, seg_delta, mr_delta);
    };
}

pub(crate) fn rav1d_calc_lf_values(
    lflvl_values: &mut [Align16<[[[u8; 2]; 8]; 4]>; SegmentId::COUNT],
    hdr: &Rav1dFrameHeader,
    lf_delta: &[i8; 4],
) {
    let n_seg = if hdr.segmentation.enabled != 0 {
        SegmentId::COUNT
    } else {
        1
    };

    if hdr.loopfilter.level_y == [0; 2] {
        lflvl_values[..n_seg].fill_with(Default::default);
        return;
    }

    let mr_deltas = if hdr.loopfilter.mode_ref_delta_enabled != 0 {
        Some(&hdr.loopfilter.mode_ref_deltas)
    } else {
        None
    };
    for s in 0..n_seg {
        let segd = if hdr.segmentation.enabled != 0 {
            Some(&hdr.segmentation.seg_data.d[s])
        } else {
            None
        };

        calc_lf_value(
            &mut lflvl_values[s][0],
            hdr.loopfilter.level_y[0],
            lf_delta[0],
            segd.map(|segd| segd.delta_lf_y_v).unwrap_or(0),
            mr_deltas,
        );
        calc_lf_value(
            &mut lflvl_values[s][1],
            hdr.loopfilter.level_y[1],
            lf_delta[if hdr.delta.lf.multi != 0 { 1 } else { 0 }],
            segd.map(|segd| segd.delta_lf_y_h).unwrap_or(0),
            mr_deltas,
        );
        calc_lf_value_chroma(
            &mut lflvl_values[s][2],
            hdr.loopfilter.level_u,
            lf_delta[if hdr.delta.lf.multi != 0 { 2 } else { 0 }],
            segd.map(|segd| segd.delta_lf_u).unwrap_or(0),
            mr_deltas,
        );
        calc_lf_value_chroma(
            &mut lflvl_values[s][3],
            hdr.loopfilter.level_v,
            lf_delta[if hdr.delta.lf.multi != 0 { 3 } else { 0 }],
            segd.map(|segd| segd.delta_lf_v).unwrap_or(0),
            mr_deltas,
        );
    }
}
