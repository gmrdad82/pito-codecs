#![deny(unsafe_op_in_unsafe_fn)]

use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::{cmp, mem, ptr, slice};

use crate::av1::cpu::pod::{AsBytes, FromZeroes};

use crate::av1::cpu::align::{Align16, AlignedVec64};
use crate::av1::cpu::cpu::CpuFlags;
use crate::av1::cpu::disjoint_mut::{
    DisjointMut, DisjointMutArcSlice, DisjointMutGuard, DisjointMutSlice,
};
use crate::av1::cpu::env::{fix_mv_precision, get_gmv_2d, get_poc_diff};
use crate::av1::cpu::error::Rav1dResult;
use crate::av1::cpu::ffi_safe::FFISafe;
use crate::av1::cpu::include::common::intops::{apply_sign, iclip};
use crate::av1::cpu::include::dav1d::headers::{
    Rav1dFrameHeader, Rav1dSequenceHeader, Rav1dWarpedMotionType,
};
use crate::av1::cpu::internal::Bxy;
use crate::av1::cpu::intra_edge::EdgeFlags;
use crate::av1::cpu::levels::{BlockSize, Mv, UnalignedMv};
use crate::av1::cpu::wrap_fn_ptr::wrap_fn_ptr;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
#[repr(C, packed)]
pub struct RefMvsTemporalBlock {
    pub mv: UnalignedMv,
    pub r#ref: i8,
}
const _: () = assert!(mem::size_of::<RefMvsTemporalBlock>() == 5);

#[derive(Clone, Copy, Eq)]
#[repr(C, align(2))]
pub struct RefMvsRefPair {
    pub r#ref: [i8; 2],
}

unsafe impl crate::av1::cpu::pod::FromZeroes for RefMvsRefPair {}

unsafe impl crate::av1::cpu::pod::AsBytes for RefMvsRefPair {}
const _: () = assert!(mem::size_of::<RefMvsRefPair>() == 2);

impl PartialEq for RefMvsRefPair {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl From<[i8; 2]> for RefMvsRefPair {
    fn from(from: [i8; 2]) -> Self {
        RefMvsRefPair { r#ref: from }
    }
}

#[derive(Clone, Copy, Default, Eq)]
#[repr(C)]
#[repr(align(4))]
pub struct RefMvsMvPair {
    pub mv: [Mv; 2],
}

unsafe impl crate::av1::cpu::pod::FromZeroes for RefMvsMvPair {}

unsafe impl crate::av1::cpu::pod::AsBytes for RefMvsMvPair {}
const _: () = assert!(mem::size_of::<RefMvsMvPair>() == 8);

impl PartialEq for RefMvsMvPair {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

#[derive(Clone, Copy)]
#[repr(C, align(4))]
pub struct RefMvsBlock {
    pub mv: RefMvsMvPair,
    pub r#ref: RefMvsRefPair,
    pub bs: BlockSize,
    pub mf: u8,
}

unsafe impl crate::av1::cpu::pod::FromZeroes for RefMvsBlock {}
const _: () = assert!(mem::size_of::<RefMvsBlock>() == 12);

#[repr(C)]
pub(crate) struct AsmRefMvsFrame<'a> {
    _lifetime: PhantomData<&'a ()>,

    _frm_hdr: *const (),
    pub iw4: i32,
    pub ih4: i32,
    pub iw8: i32,
    pub ih8: i32,
    pub sbsz: i32,
    pub use_ref_frame_mvs: i32,
    pub sign_bias: [u8; 7],
    pub mfmv_sign: [u8; 7],
    pub pocdiff: [i8; 7],
    pub mfmv_ref: [u8; 3],
    pub mfmv_ref2cur: [i32; 3],
    pub mfmv_ref2ref: [[i32; 7]; 3],
    pub n_mfmvs: i32,
    pub n_blocks: i32,
    pub rp: *mut RefMvsTemporalBlock,
    pub rp_ref: *const *mut RefMvsTemporalBlock,
    pub rp_proj: *mut RefMvsTemporalBlock,
    pub rp_stride: isize,
    pub r: *mut RefMvsBlock,
    pub n_tile_threads: i32,
    pub n_frame_threads: i32,
}

const R_PAD: usize = 1;

#[derive(Default)]
pub struct RefMvsFrame {
    pub iw4: i32,
    pub ih4: i32,
    pub iw8: i32,
    pub ih8: i32,
    pub sbsz: i32,
    pub use_ref_frame_mvs: i32,
    pub sign_bias: [u8; 7],
    pub mfmv_sign: [u8; 7],
    pub pocdiff: [i8; 7],
    pub mfmv_ref: [u8; 3],
    pub mfmv_ref2cur: [i32; 3],
    pub mfmv_ref2ref: [[i32; 7]; 3],
    pub n_mfmvs: i32,
    pub n_blocks: u32,

    pub rp_proj: DisjointMut<AlignedVec64<RefMvsTemporalBlock>>,
    pub rp_stride: u32,
    pub r: DisjointMut<AlignedVec64<RefMvsBlock>>,
    pub n_tile_threads: u32,
    pub n_frame_threads: u32,
}

#[derive(Default)]
#[repr(C)]
pub struct RefmvsTileRange {
    pub start: i32,
    pub end: i32,
}

pub struct RefmvsTile {
    pub r: [usize; 37],

    pub rp_proj: usize,

    pub tile_col: RefmvsTileRange,
    pub tile_row: RefmvsTileRange,
}

impl Default for RefmvsTile {
    fn default() -> Self {
        Self {
            r: [Default::default(); 37],
            rp_proj: Default::default(),
            tile_col: Default::default(),
            tile_row: Default::default(),
        }
    }
}

#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct RefMvsCandidate {
    pub mv: RefMvsMvPair,
    pub weight: i32,
}

wrap_fn_ptr!(pub(crate) unsafe extern "C" fn load_tmvs(
    rf: &AsmRefMvsFrame,
    tile_row_idx: i32,
    col_start8: i32,
    col_end8: i32,
    row_start8: i32,
    row_end8: i32,
    _rp_proj: *const FFISafe<DisjointMut<AlignedVec64<RefMvsTemporalBlock>>>,
    _rp_ref: *const FFISafe<[Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7]>,
) -> ());

impl load_tmvs::Fn {
    pub fn call(
        &self,
        rf: &RefMvsFrame,
        rp: &Option<DisjointMutArcSlice<RefMvsTemporalBlock>>,
        rp_ref: &[Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7],
        tile_row_idx: i32,
        col_start8: i32,
        col_end8: i32,
        row_start8: i32,
        row_end8: i32,
    ) {
        let RefMvsFrame {
            iw4,
            ih4,
            iw8,
            ih8,
            sbsz,
            use_ref_frame_mvs,
            sign_bias,
            mfmv_sign,
            pocdiff,
            mfmv_ref,
            mfmv_ref2cur,
            mfmv_ref2ref,
            n_mfmvs,
            n_blocks,
            ref rp_proj,
            rp_stride,
            ref r,
            n_tile_threads,
            n_frame_threads,
        } = *rf;
        fn mvs_to_dav1d(
            mvs: &Option<DisjointMutArcSlice<RefMvsTemporalBlock>>,
        ) -> *mut RefMvsTemporalBlock {
            mvs.as_ref()
                .map(|rp| rp.inner.as_mut_ptr())
                .unwrap_or_else(ptr::null_mut)
        }
        let rp_ref_dav1d = rp_ref.each_ref().map(mvs_to_dav1d);
        let rf_dav1d = AsmRefMvsFrame {
            _lifetime: PhantomData,
            _frm_hdr: ptr::null(),
            iw4,
            ih4,
            iw8,
            ih8,
            sbsz,
            use_ref_frame_mvs,
            sign_bias,
            mfmv_sign,
            pocdiff,
            mfmv_ref,
            mfmv_ref2cur,
            mfmv_ref2ref,
            n_mfmvs,
            n_blocks: n_blocks as _,
            rp: mvs_to_dav1d(rp),
            rp_ref: rp_ref_dav1d.as_ptr(),
            rp_proj: rp_proj.as_mut_ptr(),
            rp_stride: rp_stride as _,
            r: r.as_mut_ptr(),
            n_tile_threads: n_tile_threads as _,
            n_frame_threads: n_frame_threads as _,
        };

        let rp_proj = FFISafe::new(&rf.rp_proj);
        let rp_ref = FFISafe::new(rp_ref);
        let rf = &rf_dav1d;

        unsafe {
            self.get()(
                rf,
                tile_row_idx,
                col_start8,
                col_end8,
                row_start8,
                row_end8,
                rp_proj,
                rp_ref,
            )
        };
    }
}

wrap_fn_ptr!(pub unsafe extern "C" fn save_tmvs(
    rp_ptr: *mut RefMvsTemporalBlock,
    stride: isize,
    rr: &[*const RefMvsBlock; 31],
    ref_sign: &[u8; 7],
    col_end8: i32,
    row_end8: i32,
    col_start8: i32,
    row_start8: i32,
    _r: *const FFISafe<DisjointMut<AlignedVec64<RefMvsBlock>>>,
    _ri: &[usize; 31],
    _rp: *const FFISafe<DisjointMutArcSlice<RefMvsTemporalBlock>>,
) -> ());

impl save_tmvs::Fn {
    pub fn call(
        &self,
        rt: &RefmvsTile,
        rf: &RefMvsFrame,
        rp: &Option<DisjointMutArcSlice<RefMvsTemporalBlock>>,
        col_start8: i32,
        col_end8: i32,
        row_start8: i32,
        row_end8: i32,
    ) {
        assert!(row_start8 >= 0);
        assert!((row_end8 - row_start8) as u32 <= 16);

        let rp = &*rp.as_ref().unwrap();

        let row_end8 = cmp::min(row_end8, rf.ih8);
        let col_end8 = cmp::min(col_end8, rf.iw8);
        let stride = rf.rp_stride as usize;
        let ref_sign = &rf.mfmv_sign;
        let ri = <&[_; 31]>::try_from(&rt.r[6..]).unwrap();

        let rr = &ri.map(|ri| {
            if ri > rf.r.len() - R_PAD {
                return ptr::null();
            }

            const _: () = assert!(mem::size_of::<RefMvsBlock>() * (1 + R_PAD) > 16);

            unsafe { rf.r.as_mut_ptr().cast_const().add(ri) }
        });

        let rp_offset = row_start8 as usize * stride;
        assert!(rp_offset <= rp.inner.len());

        let rp_ptr = unsafe { rp.inner.as_mut_ptr().add(rp_offset) };
        let stride = stride as isize;
        let r = FFISafe::new(&rf.r);
        let rp = FFISafe::new(rp);

        unsafe {
            self.get()(
                rp_ptr, stride, rr, ref_sign, col_end8, row_end8, col_start8, row_start8, r, ri, rp,
            )
        };
    }
}

wrap_fn_ptr!(pub unsafe extern "C" fn splat_mv(
    rr: *mut *mut RefMvsBlock,
    rmv: &Align16<RefMvsBlock>,
    bx4: i32,
    bw4: i32,
    bh4: i32,
) -> ());

impl splat_mv::Fn {
    pub fn call(
        &self,
        rf: &RefMvsFrame,
        rt: &RefmvsTile,
        rmv: &Align16<RefMvsBlock>,
        b4: Bxy,
        bw4: usize,
        bh4: usize,
    ) {
        let offset = (b4.y as usize & 31) + 5;
        let len = bh4;
        let bx4 = b4.x as usize;

        type Guard<'a> = DisjointMutGuard<'a, AlignedVec64<RefMvsBlock>, [RefMvsBlock]>;

        let mut r_guards = [const { MaybeUninit::uninit() }; 37];
        let mut r_ptrs = [MaybeUninit::uninit(); 37];

        let r_indices = &rt.r[offset..][..len];

        let r_guards = &mut r_guards[offset..][..len];

        let r_ptrs = &mut r_ptrs[offset..][..len];

        for i in 0..len {
            let ri = r_indices[i];
            if ri < rf.r.len() - R_PAD {
                let guard = rf.r.index_mut((ri + bx4.., ..bw4));
                r_guards[i].write(guard);

                let guard = unsafe { r_guards[i].assume_init_mut() };

                let ptr = unsafe { guard.as_mut_ptr().sub(bx4) };
                r_ptrs[i].write(ptr);
            } else {
                r_ptrs[i].write(ptr::null_mut());
            }
        }

        unsafe fn slice_assume_init_mut<T>(slice: &mut [MaybeUninit<T>]) -> &mut [T] {
            unsafe { &mut *(ptr::from_mut(slice) as *mut [T]) }
        }

        let r_ptrs = unsafe { slice_assume_init_mut(r_ptrs) };

        let rr = r_ptrs.as_mut_ptr();
        let bx4 = b4.x as _;
        let bw4 = bw4 as _;
        let bh4 = bh4 as _;

        unsafe { self.get()(rr, rmv, bx4, bw4, bh4) };

        if mem::needs_drop::<Guard>() {
            for i in 0..len {
                let ptr = r_ptrs[i];
                if ptr.is_null() {
                    continue;
                }

                unsafe { r_guards[i].assume_init_drop() };
            }
        }
    }
}

pub struct Rav1dRefmvsDSPContext {
    pub load_tmvs: load_tmvs::Fn,
    pub save_tmvs: save_tmvs::Fn,
    pub splat_mv: splat_mv::Fn,
}

fn add_spatial_candidate(
    mvstack: &mut [RefMvsCandidate],
    cnt: &mut usize,
    weight: i32,
    b: RefMvsBlock,
    r#ref: RefMvsRefPair,
    gmv: &[Mv; 2],
    have_newmv_match: &mut i32,
    have_refmv_match: &mut i32,
) {
    if b.mv.mv[0].is_invalid() {
        return;
    }

    let mf_odd = b.mf & 1 != 0;
    if r#ref.r#ref[1] == -1 {
        for n in 0..2 {
            if b.r#ref.r#ref[n] == r#ref.r#ref[0] {
                let cand_mv = if mf_odd && gmv[0] != Mv::INVALID {
                    gmv[0]
                } else {
                    b.mv.mv[n]
                };

                *have_refmv_match = 1;
                *have_newmv_match |= b.mf as i32 >> 1;

                let last = *cnt;
                for cand in &mut mvstack[..last] {
                    if cand.mv.mv[0] == cand_mv {
                        cand.weight += weight;
                        return;
                    }
                }

                if last < 8 {
                    let cand = &mut mvstack[last];
                    cand.mv.mv[0] = cand_mv;
                    cand.weight = weight;
                    *cnt = last + 1;
                }
                return;
            }
        }
    } else if b.r#ref == r#ref {
        let cand_mv = RefMvsMvPair {
            mv: [
                if mf_odd && gmv[0] != Mv::INVALID {
                    gmv[0]
                } else {
                    b.mv.mv[0]
                },
                if mf_odd && gmv[1] != Mv::INVALID {
                    gmv[1]
                } else {
                    b.mv.mv[1]
                },
            ],
        };

        *have_refmv_match = 1;
        *have_newmv_match |= b.mf as i32 >> 1;

        let last = *cnt;
        for cand in &mut mvstack[..last] {
            if cand.mv == cand_mv {
                cand.weight += weight;
                return;
            }
        }

        if last < 8 {
            let cand = &mut mvstack[last];
            cand.mv = cand_mv;
            cand.weight = weight;
            *cnt = last + 1;
        }
    }
}

fn scan_row(
    mvstack: &mut [RefMvsCandidate],
    cnt: &mut usize,
    r#ref: RefMvsRefPair,
    gmv: &[Mv; 2],
    r: &DisjointMut<AlignedVec64<RefMvsBlock>>,
    b_offset: usize,
    bw4: i32,
    w4: i32,
    max_rows: i32,
    step: i32,
    have_newmv_match: &mut i32,
    have_refmv_match: &mut i32,
) -> i32 {
    let mut cand_b = *r.index(b_offset);
    let first_cand_bs = cand_b.bs;
    let first_cand_b_dim = first_cand_bs.dimensions();
    let mut cand_bw4 = first_cand_b_dim[0] as i32;
    let mut len = cmp::max(step, cmp::min(bw4, cand_bw4));

    if bw4 <= cand_bw4 {
        let weight = if bw4 == 1 {
            2
        } else {
            cmp::max(2, cmp::min(2 * max_rows, first_cand_b_dim[1] as i32))
        };
        add_spatial_candidate(
            mvstack,
            cnt,
            len * weight,
            cand_b,
            r#ref,
            gmv,
            have_newmv_match,
            have_refmv_match,
        );
        return weight >> 1;
    }

    let mut x = 0;
    loop {
        add_spatial_candidate(
            mvstack,
            cnt,
            len * 2,
            cand_b,
            r#ref,
            gmv,
            have_newmv_match,
            have_refmv_match,
        );
        x += len;
        if x >= w4 {
            return 1;
        }
        cand_b = *r.index(b_offset + x as usize);
        cand_bw4 = cand_b.bs.dimensions()[0] as i32;
        assert!(cand_bw4 < bw4);
        len = cmp::max(step, cand_bw4);
    }
}

fn scan_col(
    mvstack: &mut [RefMvsCandidate],
    cnt: &mut usize,
    r#ref: RefMvsRefPair,
    gmv: &[Mv; 2],
    r: &DisjointMut<AlignedVec64<RefMvsBlock>>,
    b: &[usize],
    bh4: i32,
    h4: i32,
    bx4: i32,
    max_cols: i32,
    step: i32,
    have_newmv_match: &mut i32,
    have_refmv_match: &mut i32,
) -> i32 {
    let mut cand_b = *r.index(b[0] + bx4 as usize);
    let first_cand_bs = cand_b.bs;
    let first_cand_b_dim = first_cand_bs.dimensions();
    let mut cand_bh4 = first_cand_b_dim[1] as i32;
    let mut len = cmp::max(step, cmp::min(bh4, cand_bh4));

    if bh4 <= cand_bh4 {
        let weight = if bh4 == 1 {
            2
        } else {
            cmp::max(2, cmp::min(2 * max_cols, first_cand_b_dim[0] as i32))
        };
        add_spatial_candidate(
            mvstack,
            cnt,
            len * weight,
            cand_b,
            r#ref,
            gmv,
            have_newmv_match,
            have_refmv_match,
        );
        return weight >> 1;
    }

    let mut y = 0;
    loop {
        add_spatial_candidate(
            mvstack,
            cnt,
            len * 2,
            cand_b,
            r#ref,
            gmv,
            have_newmv_match,
            have_refmv_match,
        );
        y += len;
        if y >= h4 {
            return 1;
        }
        cand_b = *r.index(b[y as usize] + bx4 as usize);
        cand_bh4 = cand_b.bs.dimensions()[1] as i32;
        assert!(cand_bh4 < bh4);
        len = cmp::max(step, cand_bh4);
    }
}

#[inline]
fn mv_projection(mv: Mv, num: i32, den: i32) -> Mv {
    static DIV_MULT: [u16; 32] = [
        0, 16384, 8192, 5461, 4096, 3276, 2730, 2340, 2048, 1820, 1638, 1489, 1365, 1260, 1170,
        1092, 1024, 963, 910, 862, 819, 780, 744, 712, 682, 655, 630, 606, 585, 564, 546, 528,
    ];
    assert!(den > 0 && den < 32);
    assert!(num > -32 && num < 32);
    let frac = num * DIV_MULT[den as usize] as i32;
    let y = mv.y as i32 * frac;
    let x = mv.x as i32 * frac;

    let max = (1 << 14) - 1;
    return Mv {
        y: iclip(y + 8192 + (y >> 31) >> 14, -max, max) as i16,
        x: iclip(x + 8192 + (x >> 31) >> 14, -max, max) as i16,
    };
}

fn add_temporal_candidate(
    rf: &RefMvsFrame,
    mvstack: &mut [RefMvsCandidate],
    cnt: &mut usize,
    rb: RefMvsTemporalBlock,
    r#ref: RefMvsRefPair,
    globalmv: Option<(&mut i32, &[Mv; 2])>,
    frame_hdr: &Rav1dFrameHeader,
) {
    let rb_mv = rb.mv.into_aligned();
    if rb_mv.is_invalid() {
        return;
    }

    let mut mv = mv_projection(
        rb_mv,
        rf.pocdiff[r#ref.r#ref[0] as usize - 1] as i32,
        rb.r#ref as i32,
    );
    fix_mv_precision(frame_hdr, &mut mv);

    let last = *cnt;
    if r#ref.r#ref[1] == -1 {
        if let Some((globalmv_ctx, gmv)) = globalmv {
            *globalmv_ctx = ((mv.x - gmv[0].x).abs() | (mv.y - gmv[0].y).abs() >= 16) as i32;
        }

        for cand in &mut mvstack[..last] {
            if cand.mv.mv[0] == mv {
                cand.weight += 2;
                return;
            }
        }
        if last < 8 {
            let cand = &mut mvstack[last];
            cand.mv.mv[0] = mv;
            cand.weight = 2;
            *cnt = last + 1;
        }
    } else {
        let mut mvp = RefMvsMvPair {
            mv: [
                mv,
                mv_projection(
                    rb_mv,
                    rf.pocdiff[r#ref.r#ref[1] as usize - 1] as i32,
                    rb.r#ref as i32,
                ),
            ],
        };
        fix_mv_precision(frame_hdr, &mut mvp.mv[1]);

        for cand in &mut mvstack[..last] {
            if cand.mv == mvp {
                cand.weight += 2;
                return;
            }
        }
        if last < 8 {
            let cand = &mut mvstack[last];
            cand.mv = mvp;
            cand.weight = 2;
            *cnt = last + 1;
        }
    };
}

fn add_compound_extended_candidate(
    same: &mut [RefMvsCandidate],
    same_count: &mut [usize; 4],
    cand_b: RefMvsBlock,
    sign0: u8,
    sign1: u8,
    r#ref: RefMvsRefPair,
    sign_bias: &[u8; 7],
) {
    let (same, diff) = same.split_at_mut(2);
    let (same_count, diff_count) = same_count.split_at_mut(2);

    for n in 0..2 {
        let cand_ref = cand_b.r#ref.r#ref[n];

        if cand_ref <= 0 {
            break;
        }

        let sign_bias = sign_bias[cand_ref as usize - 1];
        let mut cand_mv = cand_b.mv.mv[n];
        if cand_ref == r#ref.r#ref[0] {
            if same_count[0] < 2 {
                same[same_count[0]].mv.mv[0] = cand_mv;
                same_count[0] += 1;
            }
            if diff_count[1] < 2 {
                if (sign1 ^ sign_bias) != 0 {
                    cand_mv = -cand_mv;
                }
                diff[diff_count[1]].mv.mv[1] = cand_mv;
                diff_count[1] += 1;
            }
        } else if cand_ref == r#ref.r#ref[1] {
            if same_count[1] < 2 {
                same[same_count[1]].mv.mv[1] = cand_mv;
                same_count[1] += 1;
            }
            if diff_count[0] < 2 {
                if (sign0 ^ sign_bias) != 0 {
                    cand_mv = -cand_mv;
                }
                diff[diff_count[0]].mv.mv[0] = cand_mv;
                diff_count[0] += 1;
            }
        } else {
            let i_cand_mv = -cand_mv;

            if diff_count[0] < 2 {
                diff[diff_count[0]].mv.mv[0] = if (sign0 ^ sign_bias) != 0 {
                    i_cand_mv
                } else {
                    cand_mv
                };
                diff_count[0] += 1;
            }

            if diff_count[1] < 2 {
                diff[diff_count[1]].mv.mv[1] = if (sign1 ^ sign_bias) != 0 {
                    i_cand_mv
                } else {
                    cand_mv
                };
                diff_count[1] += 1;
            }
        }
    }
}

fn add_single_extended_candidate(
    mvstack: &mut [RefMvsCandidate; 8],
    cnt: &mut usize,
    cand_b: RefMvsBlock,
    sign: u8,
    sign_bias: &[u8; 7],
) {
    for n in 0..2 {
        let cand_ref = cand_b.r#ref.r#ref[n];

        if cand_ref <= 0 {
            break;
        }

        let mut cand_mv = cand_b.mv.mv[n];
        if (sign ^ sign_bias[cand_ref as usize - 1]) != 0 {
            cand_mv = -cand_mv;
        }

        let last = *cnt;
        let mut broke_early = false;
        for cand in &mut mvstack[..last] {
            if cand_mv == cand.mv.mv[0] {
                broke_early = true;
                break;
            }
        }
        if !broke_early {
            mvstack[last].mv.mv[0] = cand_mv;
            mvstack[last].weight = 2;
            *cnt = last + 1;
        }
    }
}

pub(crate) fn rav1d_refmvs_find(
    rt: &RefmvsTile,
    rf: &RefMvsFrame,
    mvstack: &mut [RefMvsCandidate; 8],
    cnt: &mut usize,
    ctx: &mut i32,
    r#ref: RefMvsRefPair,
    bs: BlockSize,
    edge_flags: EdgeFlags,
    by4: i32,
    bx4: i32,
    frame_hdr: &Rav1dFrameHeader,
) {
    let b_dim = bs.dimensions();
    let bw4 = b_dim[0] as i32;
    let w4 = cmp::min(cmp::min(bw4, 16), rt.tile_col.end - bx4);
    let bh4 = b_dim[1] as i32;
    let h4 = cmp::min(cmp::min(bh4, 16), rt.tile_row.end - by4);
    let mut gmv = [Mv::default(); 2];
    let mut tgmv = [Mv::default(); 2];

    *cnt = 0;
    assert!(
        r#ref.r#ref[0] >= 0 && r#ref.r#ref[0] <= 8 && r#ref.r#ref[1] >= -1 && r#ref.r#ref[1] <= 8
    );
    if r#ref.r#ref[0] > 0 {
        tgmv[0] = get_gmv_2d(
            &frame_hdr.gmv[r#ref.r#ref[0] as usize - 1],
            bx4,
            by4,
            bw4,
            bh4,
            frame_hdr,
        );

        gmv[0] = if frame_hdr.gmv[r#ref.r#ref[0] as usize - 1].r#type
            > Rav1dWarpedMotionType::Translation
        {
            tgmv[0]
        } else {
            Mv::INVALID
        };
    } else {
        tgmv[0] = Mv::ZERO;
        gmv[0] = Mv::INVALID;
    }
    if r#ref.r#ref[1] > 0 {
        tgmv[1] = get_gmv_2d(
            &frame_hdr.gmv[r#ref.r#ref[1] as usize - 1],
            bx4,
            by4,
            bw4,
            bh4,
            frame_hdr,
        );
        gmv[1] = if frame_hdr.gmv[r#ref.r#ref[1] as usize - 1].r#type
            > Rav1dWarpedMotionType::Translation
        {
            tgmv[1]
        } else {
            Mv::INVALID
        };
    }

    let mut have_newmv = 0;
    let mut have_col_mvs = 0;
    let mut have_row_mvs = 0;
    let max_rows;
    let n_rows;
    let b_top;
    let b_top_offset;
    if by4 > rt.tile_row.start {
        max_rows = cmp::min(by4 - rt.tile_row.start + 1 >> 1, 2 + (bh4 > 1) as i32) as u32;
        let i = rt.r[(by4 as usize & 31) + 5 - 1] + bx4 as usize;

        b_top_offset = match i {
            0 => 0,
            _ => 1,
        };
        b_top = i - b_top_offset;
        n_rows = scan_row(
            mvstack,
            cnt,
            r#ref,
            &gmv,
            &rf.r,
            b_top + b_top_offset,
            bw4,
            w4,
            max_rows as i32,
            if bw4 >= 16 { 4 } else { 1 },
            &mut have_newmv,
            &mut have_row_mvs,
        ) as u32;
    } else {
        max_rows = 0;
        n_rows = !0;
        b_top = Default::default();
        b_top_offset = 0;
    }

    let max_cols;
    let n_cols;
    let b_left;
    if bx4 > rt.tile_col.start {
        max_cols = cmp::min(bx4 - rt.tile_col.start + 1 >> 1, 2 + (bw4 > 1) as i32) as u32;
        b_left = &rt.r[(by4 as usize & 31) + 5..];
        n_cols = scan_col(
            mvstack,
            cnt,
            r#ref,
            &gmv,
            &rf.r,
            b_left,
            bh4,
            h4,
            bx4 - 1,
            max_cols as i32,
            if bh4 >= 16 { 4 } else { 1 },
            &mut have_newmv,
            &mut have_col_mvs,
        ) as u32;
    } else {
        max_cols = 0;
        n_cols = !0;
        b_left = Default::default();
    }

    if n_rows != !0
        && edge_flags.contains(EdgeFlags::I444_TOP_HAS_RIGHT)
        && cmp::max(bw4, bh4) <= 16
        && bw4 + bx4 < rt.tile_col.end
    {
        add_spatial_candidate(
            mvstack,
            cnt,
            4,
            *rf.r.index(b_top + bw4 as usize + b_top_offset),
            r#ref,
            &gmv,
            &mut have_newmv,
            &mut have_row_mvs,
        );
    }

    let nearest_match = have_col_mvs + have_row_mvs;
    let nearest_cnt = *cnt;
    for cand in &mut mvstack[..nearest_cnt] {
        cand.weight += 640;
    }

    let mut globalmv_ctx = frame_hdr.use_ref_frame_mvs as i32;
    if rf.use_ref_frame_mvs != 0 {
        let stride = rf.rp_stride as usize;
        let by8 = by4 >> 1;
        let bx8 = bx4 >> 1;
        let rbi = rt.rp_proj + (by8 as usize & 15) * stride + bx8 as usize;
        let step_h = if bw4 >= 16 { 2 } else { 1 };
        let step_v = if bh4 >= 16 { 2 } else { 1 };
        let w8 = cmp::min(w4 + 1 >> 1, 8) as usize;
        let h8 = cmp::min(h4 + 1 >> 1, 8) as usize;
        for y in (0..h8).step_by(step_v) {
            for x in (0..w8).step_by(step_h) {
                add_temporal_candidate(
                    rf,
                    mvstack,
                    cnt,
                    *rf.rp_proj.index(rbi + y * stride + x),
                    r#ref,
                    if x | y == 0 {
                        Some((&mut globalmv_ctx, &tgmv))
                    } else {
                        None
                    },
                    frame_hdr,
                );
            }
        }
        if cmp::min(bw4, bh4) >= 2 && cmp::max(bw4, bh4) < 16 {
            let bh8 = bh4 >> 1;
            let bw8 = bw4 >> 1;
            let rbi = rbi + bh8 as usize * stride;
            let has_bottom = by8 + bh8 < cmp::min(rt.tile_row.end >> 1, (by8 & !7) + 8);
            if has_bottom && bx8 - 1 >= cmp::max(rt.tile_col.start >> 1, bx8 & !7) {
                let rb = *rf.rp_proj.index(rbi - 1);
                add_temporal_candidate(rf, mvstack, cnt, rb, r#ref, None, frame_hdr);
            }
            if bx8 + bw8 < cmp::min(rt.tile_col.end >> 1, (bx8 & !7) + 8) {
                if has_bottom {
                    let rb = *rf.rp_proj.index(rbi + bw8 as usize);
                    add_temporal_candidate(rf, mvstack, cnt, rb, r#ref, None, frame_hdr);
                }
                if (by8 + bh8 - 1) < cmp::min(rt.tile_row.end >> 1, (by8 & !7) + 8) {
                    let rb = *rf.rp_proj.index(rbi + bw8 as usize - stride);
                    add_temporal_candidate(rf, mvstack, cnt, rb, r#ref, None, frame_hdr);
                }
            }
        }
    }
    assert!(*cnt <= 8);

    let mut have_dummy_newmv_match = 0;
    if n_rows | n_cols != !0 {
        add_spatial_candidate(
            mvstack,
            cnt,
            4,
            *rf.r.index(b_top + b_top_offset - 1),
            r#ref,
            &gmv,
            &mut have_dummy_newmv_match,
            &mut have_row_mvs,
        );
    }

    let mut n_rows = n_rows;
    let mut n_cols = n_cols;
    for n in 2..=3 {
        if n > n_rows && n <= max_rows {
            let ri = rt.r[(((by4 & 31) - 2 * n as i32 + 1 | 1) + 5) as usize] + (bx4 as usize | 1);
            n_rows = n_rows.wrapping_add(scan_row(
                mvstack,
                cnt,
                r#ref,
                &gmv,
                &rf.r,
                ri,
                bw4,
                w4,
                (1 + max_rows - n) as _,
                if bw4 >= 16 { 4 } else { 2 },
                &mut have_dummy_newmv_match,
                &mut have_row_mvs,
            ) as u32);
        }
        if n > n_cols && n <= max_cols {
            n_cols = n_cols.wrapping_add(scan_col(
                mvstack,
                cnt,
                r#ref,
                &gmv,
                &rf.r,
                &rt.r[(by4 as usize & 31 | 1) + 5..],
                bh4,
                h4,
                bx4 - n as i32 * 2 + 1 | 1,
                (1 + max_cols - n) as _,
                if bh4 >= 16 { 4 } else { 2 },
                &mut have_dummy_newmv_match,
                &mut have_col_mvs,
            ) as u32);
        }
    }
    assert!(*cnt <= 8);

    let ref_match_count = have_col_mvs + have_row_mvs;

    let (refmv_ctx, newmv_ctx) = match nearest_match {
        0 => (cmp::min(2, ref_match_count), (ref_match_count > 0) as i32),
        1 => (cmp::min(ref_match_count * 3, 4), 3 - have_newmv),
        2 => (5, 5 - have_newmv),
        _ => (0, 0),
    };

    mvstack[..nearest_cnt].sort_by_key(|cand| -cand.weight);
    mvstack[nearest_cnt..*cnt].sort_by_key(|cand| -cand.weight);

    if r#ref.r#ref[1] > 0 {
        if *cnt < 2 {
            let sign0 = rf.sign_bias[r#ref.r#ref[0] as usize - 1];
            let sign1 = rf.sign_bias[r#ref.r#ref[1] as usize - 1];
            let sz4 = cmp::min(w4, h4);
            let cur_cnt = *cnt;
            let same = &mut mvstack[cur_cnt..];
            let mut same_count = [0; 4];

            if n_rows != !0 {
                let mut x = 0;
                while x < sz4 {
                    let cand_b = *rf.r.index(b_top + x as usize + b_top_offset);
                    add_compound_extended_candidate(
                        same,
                        &mut same_count,
                        cand_b,
                        sign0,
                        sign1,
                        r#ref,
                        &rf.sign_bias,
                    );
                    x += cand_b.bs.dimensions()[0] as i32;
                }
            }

            if n_cols != !0 {
                let mut y = 0;
                while y < sz4 {
                    let cand_b = *rf.r.index(b_left[y as usize] + bx4 as usize - 1);
                    add_compound_extended_candidate(
                        same,
                        &mut same_count,
                        cand_b,
                        sign0,
                        sign1,
                        r#ref,
                        &rf.sign_bias,
                    );
                    y += cand_b.bs.dimensions()[1] as i32;
                }
            }

            let (same, diff) = same.split_at_mut(2);
            let diff = &diff[..];
            let diff_count = &same_count[2..];

            for n in 0..2 {
                let mut m = same_count[n];

                if m >= 2 {
                    continue;
                }

                let l = diff_count[n];
                if l != 0 {
                    same[m].mv.mv[n] = diff[0].mv.mv[n];
                    m += 1;
                    if m == 2 {
                        continue;
                    }
                    if l == 2 {
                        same[1].mv.mv[n] = diff[1].mv.mv[n];
                        continue;
                    }
                }
                for cand in &mut same[m..2] {
                    cand.mv.mv[n] = tgmv[n];
                }
            }

            let n = *cnt;
            let same = &mvstack[cur_cnt..];
            if n == 1 && mvstack[0].mv == same[0].mv {
                mvstack[1].mv = mvstack[2].mv;
            }
            for cand in &mut mvstack[n..2] {
                cand.weight = 2;
            }
            *cnt = 2;
        }

        let left = -(bx4 + bw4 + 4) * 4 * 8;
        let right = (rf.iw4 - bx4 + 4) * 4 * 8;
        let top = -(by4 + bh4 + 4) * 4 * 8;
        let bottom = (rf.ih4 - by4 + 4) * 4 * 8;

        let n_refmvs = *cnt;

        for cand in &mut mvstack[..n_refmvs] {
            let mv = &mut cand.mv.mv;
            mv[0].x = iclip(mv[0].x as i32, left, right) as i16;
            mv[0].y = iclip(mv[0].y as i32, top, bottom) as i16;
            mv[1].x = iclip(mv[1].x as i32, left, right) as i16;
            mv[1].y = iclip(mv[1].y as i32, top, bottom) as i16;
        }

        *ctx = match refmv_ctx >> 1 {
            0 => cmp::min(newmv_ctx, 1),
            1 => 1 + cmp::min(newmv_ctx, 3),
            2 => iclip(3 + newmv_ctx, 4, 7),
            _ => *ctx,
        };

        return;
    } else if *cnt < 2 && r#ref.r#ref[0] > 0 {
        let sign = rf.sign_bias[r#ref.r#ref[0] as usize - 1];
        let sz4 = cmp::min(w4, h4);

        if n_rows != !0 {
            let mut x = 0;
            while x < sz4 && *cnt < 2 {
                let cand_b = *rf.r.index(b_top + x as usize + b_top_offset);
                add_single_extended_candidate(mvstack, cnt, cand_b, sign, &rf.sign_bias);
                x += cand_b.bs.dimensions()[0] as i32;
            }
        }

        if n_cols != !0 {
            let mut y = 0;
            while y < sz4 && *cnt < 2 {
                let cand_b = *rf.r.index(b_left[y as usize] + bx4 as usize - 1);
                add_single_extended_candidate(mvstack, cnt, cand_b, sign, &rf.sign_bias);
                y += cand_b.bs.dimensions()[1] as i32;
            }
        }
    }
    assert!(*cnt <= 8);

    let n_refmvs = *cnt;
    if n_refmvs != 0 {
        let left = -(bx4 + bw4 + 4) * 4 * 8;
        let right = (rf.iw4 - bx4 + 4) * 4 * 8;
        let top = -(by4 + bh4 + 4) * 4 * 8;
        let bottom = (rf.ih4 - by4 + 4) * 4 * 8;

        for cand in &mut mvstack[..n_refmvs] {
            let mv = &mut cand.mv.mv;
            mv[0].x = iclip(mv[0].x as i32, left, right) as i16;
            mv[0].y = iclip(mv[0].y as i32, top, bottom) as i16;
        }
    }

    for cand in &mut mvstack[cmp::min(*cnt, 2)..2] {
        cand.mv.mv[0] = tgmv[0];
    }

    *ctx = refmv_ctx << 4 | globalmv_ctx << 3 | newmv_ctx;
}

pub(crate) fn rav1d_refmvs_tile_sbrow_init(
    rf: &RefMvsFrame,
    tile_col_start4: i32,
    tile_col_end4: i32,
    tile_row_start4: i32,
    tile_row_end4: i32,
    sby: i32,
    mut tile_row_idx: i32,
    pass: i32,
) -> RefmvsTile {
    if rf.n_tile_threads == 1 {
        tile_row_idx = 0;
    }
    let rp_stride = rf.rp_stride as usize;
    let r_stride = rp_stride * 2;
    let rp_proj = 16 * rp_stride * tile_row_idx as usize;
    let pass_off = if rf.n_frame_threads > 1 && pass == 2 {
        35 * 2 * rf.n_blocks as usize
    } else {
        0
    };
    let mut r = 35 * r_stride * tile_row_idx as usize + pass_off;
    let sbsz = rf.sbsz;
    let off = sbsz * sby & 16;
    let invalid_r = usize::MAX;
    let mut rr = [invalid_r; 37];

    for i in 0..sbsz {
        rr[(off + 5 + i) as usize] = r;
        r += r_stride;
    }
    rr[(off + 0) as usize] = r;
    r += r_stride;
    rr[(off + 1) as usize] = invalid_r;
    rr[(off + 2) as usize] = r;
    r += r_stride;
    rr[(off + 3) as usize] = invalid_r;
    rr[(off + 4) as usize] = r;
    if sby & 1 != 0 {
        for i in [0, 2, 4] {
            rr.swap((off + i) as usize, (off + sbsz + i) as usize);
        }
    }

    RefmvsTile {
        r: rr,
        rp_proj,
        tile_col: RefmvsTileRange {
            start: tile_col_start4,
            end: cmp::min(tile_col_end4, rf.iw4),
        },
        tile_row: RefmvsTileRange {
            start: tile_row_start4,
            end: cmp::min(tile_row_end4, rf.ih4),
        },
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn load_tmvs_c(
    rf: &AsmRefMvsFrame,
    tile_row_idx: i32,
    col_start8: i32,
    col_end8: i32,
    row_start8: i32,
    row_end8: i32,
    rp_proj: *const FFISafe<DisjointMut<AlignedVec64<RefMvsTemporalBlock>>>,
    rp_ref: *const FFISafe<[Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7]>,
) {
    let rp_proj = unsafe { FFISafe::get(rp_proj) };

    let rp_ref = unsafe { FFISafe::get(rp_ref) };
    load_tmvs_rust(
        rf,
        tile_row_idx,
        col_start8,
        col_end8,
        row_start8,
        row_end8,
        rp_proj,
        rp_ref,
    )
}

fn load_tmvs_rust(
    rf: &AsmRefMvsFrame,
    mut tile_row_idx: i32,
    col_start8: i32,
    col_end8: i32,
    row_start8: i32,
    mut row_end8: i32,
    rp_proj: &DisjointMut<AlignedVec64<RefMvsTemporalBlock>>,
    rp_ref: &[Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7],
) {
    if rf.n_tile_threads == 1 {
        tile_row_idx = 0;
    }
    assert!(row_start8 >= 0);
    assert!((row_end8 - row_start8) as u32 <= 16);
    row_end8 = cmp::min(row_end8, rf.ih8);
    let col_start8i = cmp::max(col_start8 - 8, 0);
    let col_end8i = cmp::min(col_end8 + 8, rf.iw8);
    let stride = rf.rp_stride as usize;
    let rp_proj_offset = 16 * stride * tile_row_idx as usize;
    for y in row_start8..row_end8 {
        let offset = rp_proj_offset + (y & 15) as usize * stride;
        for rp_proj in
            &mut *rp_proj.index_mut(offset + col_start8 as usize..offset + col_end8 as usize)
        {
            rp_proj.mv = Mv::INVALID.into_unaligned();
        }
    }
    for n in 0..rf.n_mfmvs {
        let ref2cur = rf.mfmv_ref2cur[n as usize];
        if ref2cur == i32::MIN {
            continue;
        }
        let r#ref = rf.mfmv_ref[n as usize];
        let ref_sign = r#ref as i32 - 4;
        let r = &*rp_ref[r#ref as usize].as_ref().unwrap().inner;
        for y in row_start8..row_end8 {
            let y_sb_align = y & !7;
            let y_proj_start = cmp::max(y_sb_align, row_start8);
            let y_proj_end = cmp::min(y_sb_align + 8, row_end8);
            let mut x = col_start8i;
            while x < col_end8i {
                let mut rbi = y as usize * stride + x as usize;
                let mut rb = *r.index(rbi);
                if rb.r#ref == 0 {
                    x += 1;
                    continue;
                }
                let ref2ref = rf.mfmv_ref2ref[n as usize][(rb.r#ref - 1) as usize];
                if ref2ref == 0 {
                    x += 1;
                    continue;
                }
                let offset = mv_projection(rb.mv.into_aligned(), ref2cur, ref2ref);
                let mut pos_x =
                    x + apply_sign((offset.x as i32).abs() >> 6, offset.x as i32 ^ ref_sign);
                let pos_y =
                    y + apply_sign((offset.y as i32).abs() >> 6, offset.y as i32 ^ ref_sign);
                if pos_y >= y_proj_start && pos_y < y_proj_end {
                    let pos = (pos_y & 15) as usize * stride;
                    loop {
                        let x_sb_align = x & !7;
                        if pos_x >= cmp::max(x_sb_align - 8, col_start8)
                            && pos_x < cmp::min(x_sb_align + 16, col_end8)
                        {
                            *rp_proj.index_mut(
                                rp_proj_offset + (pos as isize + pos_x as isize) as usize,
                            ) = RefMvsTemporalBlock {
                                mv: rb.mv,
                                r#ref: ref2ref as i8,
                            };
                        }
                        x += 1;
                        if x >= col_end8i {
                            break;
                        }
                        let prev_rb = rb;
                        rbi += 1;
                        rb = *r.index(rbi);
                        if rb != prev_rb {
                            break;
                        }
                        pos_x += 1;
                    }
                } else {
                    loop {
                        x += 1;
                        if x >= col_end8i {
                            break;
                        }
                        let prev_rb = rb;
                        rbi += 1;
                        rb = *r.index(rbi);
                        if rb != prev_rb {
                            break;
                        }
                    }
                }
            }
        }
    }
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn save_tmvs_c(
    _rp: *mut RefMvsTemporalBlock,
    stride: isize,
    _rr: &[*const RefMvsBlock; 31],
    ref_sign: &[u8; 7],
    col_end8: i32,
    row_end8: i32,
    col_start8: i32,
    row_start8: i32,
    r: *const FFISafe<DisjointMut<AlignedVec64<RefMvsBlock>>>,
    ri: &[usize; 31],
    rp: *const FFISafe<DisjointMutArcSlice<RefMvsTemporalBlock>>,
) {
    let stride = stride as usize;
    let [col_end8, row_end8, col_start8, row_start8] =
        [col_end8, row_end8, col_start8, row_start8].map(|it| it as usize);

    let r = unsafe { FFISafe::get(r) };

    let rp = unsafe { FFISafe::get(rp) };
    let rp = &*rp.inner;
    save_tmvs_rust(
        stride, ref_sign, col_end8, row_end8, col_start8, row_start8, r, ri, rp,
    )
}

fn save_tmvs_rust(
    stride: usize,
    ref_sign: &[u8; 7],
    col_end8: usize,
    row_end8: usize,
    col_start8: usize,
    row_start8: usize,
    r: &DisjointMut<AlignedVec64<RefMvsBlock>>,
    ri: &[usize; 31],
    rp: &DisjointMutSlice<RefMvsTemporalBlock>,
) {
    for y in row_start8..row_end8 {
        let b = ri[(y & 15) * 2];
        let mut x = col_start8;
        while x < col_end8 {
            let cand_b = *r.index(b + x * 2 + 1);
            let bw8 = cand_b.bs.dimensions()[0] + 1 >> 1;
            let block = |i: usize| {
                let mv = cand_b.mv.mv[i];
                let r#ref = cand_b.r#ref.r#ref[i];
                if r#ref > 0 && ref_sign[r#ref as usize - 1] != 0 && mv.y.abs() | mv.x.abs() < 4096
                {
                    Some(RefMvsTemporalBlock {
                        mv: mv.into_unaligned(),
                        r#ref,
                    })
                } else {
                    None
                }
            };
            let block = block(1).or_else(|| block(0)).unwrap_or_default();
            let offset = y * stride + x;
            rp.index_mut(offset..offset + bw8 as usize).fill(block);
            x += bw8 as usize;
        }
    }
}

pub(crate) fn rav1d_refmvs_init_frame(
    rf: &mut RefMvsFrame,
    seq_hdr: &Rav1dSequenceHeader,
    frm_hdr: &Rav1dFrameHeader,
    ref_poc: &[u32; 7],
    ref_ref_poc: &[[u32; 7]; 7],
    rp_ref: &[Option<DisjointMutArcSlice<RefMvsTemporalBlock>>; 7],
    n_tile_threads: u32,
    n_frame_threads: u32,
) -> Rav1dResult {
    let rp_stride = ((frm_hdr.size.width[0] + 127 & !127) >> 3) as u32;
    let n_tile_rows = if n_tile_threads > 1 {
        frm_hdr.tiling.rows as u32
    } else {
        1
    };
    let n_blocks = rp_stride * n_tile_rows;

    rf.sbsz = 16 << seq_hdr.sb128;
    rf.iw8 = frm_hdr.size.width[0] + 7 >> 3;
    rf.ih8 = frm_hdr.size.height + 7 >> 3;
    rf.iw4 = rf.iw8 << 1;
    rf.ih4 = rf.ih8 << 1;
    rf.rp_stride = rp_stride;
    rf.n_tile_threads = n_tile_threads;
    rf.n_frame_threads = n_frame_threads;

    if n_blocks != rf.n_blocks {
        let r_sz = 35 * 2 * n_blocks as usize * (1 + (n_frame_threads > 1) as usize) + R_PAD;
        let rp_proj_sz = 16 * n_blocks as usize;

        rf.r.resize(r_sz, FromZeroes::new_zeroed());
        rf.rp_proj.resize(rp_proj_sz, Default::default());
        rf.n_blocks = n_blocks;
    }

    let poc = frm_hdr.frame_offset as u32;
    for i in 0..7 {
        let poc_diff = get_poc_diff(seq_hdr.order_hint_n_bits, ref_poc[i] as i32, poc as i32);
        rf.sign_bias[i] = (poc_diff > 0) as u8;
        rf.mfmv_sign[i] = (poc_diff < 0) as u8;
        rf.pocdiff[i] = iclip(
            get_poc_diff(seq_hdr.order_hint_n_bits, poc as i32, ref_poc[i] as i32),
            -31,
            31,
        ) as i8;
    }

    rf.n_mfmvs = 0;
    if frm_hdr.use_ref_frame_mvs != 0 && seq_hdr.order_hint_n_bits != 0 {
        let mut total = 2;
        if rp_ref[0].is_some() && ref_ref_poc[0][6] != ref_poc[3] {
            rf.mfmv_ref[rf.n_mfmvs as usize] = 0;
            rf.n_mfmvs += 1;
            total = 3;
        }
        if rp_ref[4].is_some()
            && get_poc_diff(
                seq_hdr.order_hint_n_bits,
                ref_poc[4] as i32,
                frm_hdr.frame_offset as i32,
            ) > 0
        {
            rf.mfmv_ref[rf.n_mfmvs as usize] = 4;
            rf.n_mfmvs += 1;
        }
        if rp_ref[5].is_some()
            && get_poc_diff(
                seq_hdr.order_hint_n_bits,
                ref_poc[5] as i32,
                frm_hdr.frame_offset as i32,
            ) > 0
        {
            rf.mfmv_ref[rf.n_mfmvs as usize] = 5;
            rf.n_mfmvs += 1;
        }
        if rf.n_mfmvs < total
            && rp_ref[6].is_some()
            && get_poc_diff(
                seq_hdr.order_hint_n_bits,
                ref_poc[6] as i32,
                frm_hdr.frame_offset as i32,
            ) > 0
        {
            rf.mfmv_ref[rf.n_mfmvs as usize] = 6;
            rf.n_mfmvs += 1;
        }
        if rf.n_mfmvs < total && rp_ref[1].is_some() {
            rf.mfmv_ref[rf.n_mfmvs as usize] = 1;
            rf.n_mfmvs += 1;
        }

        for n in 0..rf.n_mfmvs as usize {
            let rpoc = ref_poc[rf.mfmv_ref[n] as usize];
            let diff1 = get_poc_diff(
                seq_hdr.order_hint_n_bits,
                rpoc as i32,
                frm_hdr.frame_offset as i32,
            );
            if diff1.abs() > 31 {
                rf.mfmv_ref2cur[n] = i32::MIN;
            } else {
                rf.mfmv_ref2cur[n] = if rf.mfmv_ref[n] < 4 { -diff1 } else { diff1 };
                for m in 0..7 {
                    let rrpoc = ref_ref_poc[rf.mfmv_ref[n] as usize][m];
                    let diff2 = get_poc_diff(seq_hdr.order_hint_n_bits, rpoc as i32, rrpoc as i32);

                    rf.mfmv_ref2ref[n][m] = if diff2 as u32 > 31 { 0 } else { diff2 };
                }
            }
        }
    }
    rf.use_ref_frame_mvs = (rf.n_mfmvs > 0) as i32;

    Ok(())
}

#[deny(unsafe_op_in_unsafe_fn)]
unsafe extern "C" fn splat_mv_c(
    rr: *mut *mut RefMvsBlock,
    rmv: &Align16<RefMvsBlock>,
    bx4: i32,
    bw4: i32,
    bh4: i32,
) {
    let [bx4, bw4, bh4] = [bx4, bw4, bh4].map(|it| it as usize);

    let rr = unsafe { slice::from_raw_parts_mut(rr, bh4) };
    let rr = rr
        .into_iter()
        .map(|&mut r| unsafe { slice::from_raw_parts_mut(r.add(bx4), bw4) });
    splat_mv_rust(rr, rmv)
}

fn splat_mv_rust<'a>(rr: impl Iterator<Item = &'a mut [RefMvsBlock]>, rmv: &Align16<RefMvsBlock>) {
    let rmv = rmv.0;
    for r in rr {
        r.fill_with(|| rmv)
    }
}

impl Rav1dRefmvsDSPContext {
    pub const fn default() -> Self {
        Self {
            load_tmvs: load_tmvs::Fn::new(load_tmvs_c),
            save_tmvs: save_tmvs::Fn::new(save_tmvs_c),
            splat_mv: splat_mv::Fn::new(splat_mv_c),
        }
    }

    #[inline(always)]
    const fn init(self, flags: CpuFlags) -> Self {
        #[allow(unreachable_code)]
        {
            let _ = flags;
            self
        }
    }

    pub const fn new(flags: CpuFlags) -> Self {
        Self::default().init(flags)
    }
}

impl Default for Rav1dRefmvsDSPContext {
    fn default() -> Self {
        Self::default()
    }
}
