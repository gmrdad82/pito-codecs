use std::collections::VecDeque;
use std::sync::Arc;

use super::Picture;
use crate::media::{Error, Result};
use crate::syntax::bits::unescape_rbsp;
use crate::syntax::h264::dpb::{Dpb, DpbEntry, Output, OutputMeta, RefMark};
use crate::syntax::h264::params::{Pps, Sps};
use crate::syntax::h264::slice::{
    NalHeader, Poc, PocState, SliceHeader, SliceType, is_new_picture, nal_type,
};

use super::Accel;
use super::ffi::{
    self, VAIQMatrixBufferH264, VAPictureH264, VAPictureParameterBufferH264,
    VASliceParameterBufferH264, pic_bits, seq_bits,
};
use crate::gpu::nal::Framing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    pub index: usize,
    pub top: i32,
    pub bottom: i32,
    pub seq: u64,
}

const UNASSIGNED: usize = usize::MAX;

struct Pending {
    target: usize,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    first: SliceHeader,
    poc: Poc,
    pts: i64,
    slices: Vec<(VASliceParameterBufferH264, Vec<u8>)>,
}

pub struct Front<A: Accel> {
    accel: A,
    framing: Framing,
    mbs: (u32, u32),
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    dpb: Dpb<Surface>,
    poc_state: PocState,
    prev_ref_frame_num: u32,
    active_sps: Option<Arc<Sps>>,
    pending: Option<Pending>,
    ready: VecDeque<(usize, i64)>,
    seq: u64,
}

fn decode_err(e: impl std::fmt::Display) -> Error {
    Error::Backend(e.to_string())
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(format!("the hardware decoder does not take {what}"))
}

impl<A: Accel> Front<A> {
    pub fn new(
        accel: A,
        framing: Framing,
        mbs: (u32, u32),
        parameter_sets: &[Vec<u8>],
    ) -> Result<Self> {
        let mut f = Self {
            accel,
            framing,
            mbs,
            spss: vec![None; 32],
            ppss: vec![None; 256],
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            prev_ref_frame_num: 0,
            active_sps: None,
            pending: None,
            ready: VecDeque::new(),
            seq: 0,
        };
        f.load_parameter_sets(parameter_sets)?;
        Ok(f)
    }

    fn load_parameter_sets(&mut self, sets: &[Vec<u8>]) -> Result<()> {
        let mut frames = Vec::new();
        for nal in sets {
            self.handle_nal(nal, 0, &mut frames)?;
        }
        Ok(())
    }

    pub fn accel_mut(&mut self) -> &mut A {
        &mut self.accel
    }

    pub fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let nals = self.framing.split(sample).map_err(decode_err)?;
        let mut frames = Vec::new();
        for nal in nals {
            self.handle_nal(nal, pts, &mut frames)?;
        }
        self.submit_pending(&mut frames)?;
        Ok(frames)
    }

    pub fn flush(&mut self) -> Result<Vec<Picture>> {
        let mut frames = Vec::new();
        self.submit_pending(&mut frames)?;
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.read_out(outs, &mut frames)?;
        self.drain(0, &mut frames)?;
        Ok(frames)
    }

    pub fn reset(&mut self) {
        self.ready.clear();
        self.dpb = Dpb::new();
        self.poc_state = PocState::default();
        self.prev_ref_frame_num = 0;
        self.active_sps = None;
        self.pending = None;
    }

    fn read_out(&mut self, outs: Vec<Output<Surface>>, frames: &mut Vec<Picture>) -> Result<()> {
        for o in outs {
            self.accel.prefetch(o.frame.index);
            self.ready.push_back((o.frame.index, o.meta.pts));
        }
        self.drain(super::LAG, frames)
    }

    fn drain(&mut self, keep: usize, frames: &mut Vec<Picture>) -> Result<()> {
        while self.ready.len() > keep {
            let Some((index, pts)) = self.ready.pop_front() else {
                break;
            };
            let picture = self.accel.read(index).map_err(decode_err)?;
            frames.push(picture.at(pts));
        }
        Ok(())
    }

    fn busy(&self, i: usize) -> bool {
        self.dpb.entries.iter().any(|e| e.frame.index == i)
            || self.ready.iter().any(|r| r.0 == i)
            || self.accel.held(i)
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64, frames: &mut Vec<Picture>) -> Result<()> {
        let Some(&first) = nal.first() else {
            return Ok(());
        };
        let rest = nal.get(1..).unwrap_or_default();
        let hdr = NalHeader::parse(first).map_err(decode_err)?;
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let sps = Sps::parse(&unescape_rbsp(rest)).map_err(decode_err)?;
                let slot = self
                    .spss
                    .get_mut(sps.id as usize)
                    .ok_or_else(|| decode_err("SPS id out of range"))?;
                *slot = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let spss: Vec<Option<Sps>> = self
                    .spss
                    .iter()
                    .map(|s| s.as_ref().map(|s| (**s).clone()))
                    .collect();
                let pps = Pps::parse(&unescape_rbsp(rest), &spss).map_err(decode_err)?;
                let slot = self
                    .ppss
                    .get_mut(pps.id as usize)
                    .ok_or_else(|| decode_err("PPS id out of range"))?;
                *slot = Some(Arc::new(pps));
            }
            nal_type::SLICE | nal_type::IDR => self.handle_slice(nal, hdr, pts, frames)?,
            nal_type::SLICE_DPA | nal_type::SLICE_DPB | nal_type::SLICE_DPC => {
                return Err(unsupported("data partitioning"));
            }
            nal_type::END_SEQ | nal_type::END_STREAM => self.submit_pending(frames)?,
            _ => {}
        }
        Ok(())
    }

    fn handle_slice(
        &mut self,
        nal: &[u8],
        hdr: NalHeader,
        pts: i64,
        frames: &mut Vec<Picture>,
    ) -> Result<()> {
        let payload = nal.get(1..).unwrap_or_default();
        let mut result = None;
        let mut found: Option<(Arc<Pps>, Arc<Sps>)> = None;
        for full in [false, true] {
            let Some(rbsp) = super::header_rbsp(payload, full) else {
                break;
            };
            found = None;
            let (ppss, spss) = (&self.ppss, &self.spss);
            let parsed = SliceHeader::parse(&rbsp, hdr, |id| {
                let missing = |what: String| crate::syntax::error::Error::MissingParameterSet(what);
                let pps = ppss
                    .get(id as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| missing(format!("PPS {id}")))?;
                let sps = spss
                    .get(pps.sps_id as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| missing(format!("SPS {}", pps.sps_id)))?;
                found = Some((pps.clone(), sps.clone()));
                Ok((&**pps, &**sps))
            });

            let done = parsed.is_ok();
            result = Some(parsed.map(|(sh, _, _)| sh));
            if done {
                break;
            }
        }
        let sh = result
            .ok_or_else(|| decode_err("empty slice"))?
            .map_err(decode_err)?;
        let Some((pps, sps)) = found else {
            return Err(decode_err("slice without a PPS"));
        };
        sps.check_supported().map_err(decode_err)?;
        if pps.num_slice_groups > 1 {
            return Err(unsupported("slice groups (FMO)"));
        }
        if sh.field_pic {
            return Err(unsupported("field pictures"));
        }
        if matches!(sh.slice_type, SliceType::Sp | SliceType::Si) {
            return Err(unsupported("SP / SI slices"));
        }
        if sh.redundant_pic_cnt > 0 {
            return Ok(());
        }
        let new_pic = match &self.pending {
            None => true,
            Some(p) => {
                is_new_picture(&p.first, &sh, &sps)
                    || sh.first_mb_in_slice == 0
                    || !Arc::ptr_eq(&p.sps, &sps)
            }
        };
        if new_pic {
            self.submit_pending(frames)?;
            if sh.first_mb_in_slice != 0 && self.active_sps.is_none() {
                return Err(decode_err(
                    "stream does not start with the first slice of a picture",
                ));
            }
            self.start_picture(&sh, &sps, &pps, pts, frames)?;
        }
        let Some(pending) = self.pending.as_ref() else {
            return Err(decode_err("slice without a started picture"));
        };
        if !Arc::ptr_eq(&pending.pps, &pps) {
            return Err(unsupported("a picture whose slices use different PPSs"));
        }
        let surfaces = self.accel.surfaces();
        let lists = self
            .dpb
            .build_ref_lists(&sh, pending.poc.frame(), sps.max_frame_num(), |e| {
                (va_ref(e, surfaces), e.mark != RefMark::Unused)
            })
            .map_err(decode_err)?;
        if !sh.slice_type.is_intra()
            && (lists[0].is_empty() || (sh.slice_type.is_b() && lists[1].is_empty()))
        {
            return Err(decode_err(
                "no reference pictures available for an inter slice",
            ));
        }
        if lists.iter().flatten().any(|(_, is_ref)| !is_ref) {
            return Err(decode_err("a reference picture is missing"));
        }
        let refs = lists.map(|l| l.into_iter().map(|(p, _)| p).collect::<Vec<_>>());
        let params = slice_params(&sh, nal.len(), &refs)?;
        if let Some(p) = self.pending.as_mut() {
            p.slices.push((params, nal.to_vec()));
        }
        Ok(())
    }

    fn start_picture(
        &mut self,
        sh: &SliceHeader,
        sps: &Arc<Sps>,
        pps: &Arc<Pps>,
        pts: i64,
        frames: &mut Vec<Picture>,
    ) -> Result<()> {
        if (sps.pic_width_in_mbs, sps.frame_height_in_mbs()) != self.mbs {
            return Err(unsupported("a picture size change"));
        }
        if sps.max_dpb_frames() + super::LAG >= self.accel.surfaces().len() {
            return Err(unsupported("a DPB larger than the decoder's surfaces"));
        }
        let changed = self
            .active_sps
            .as_ref()
            .is_some_and(|a| a.max_dpb_frames() != sps.max_dpb_frames());
        let mut outs = Vec::new();
        if changed {
            self.dpb.flush(&mut outs);
            self.dpb.entries.clear();
        }
        self.active_sps = Some(sps.clone());
        self.dpb.capacity = sps.max_dpb_frames();
        self.dpb.max_reorder = sps.max_num_reorder_frames().min(self.dpb.capacity);
        if sh.idr {
            self.dpb.idr(sh.no_output_of_prior_pics, &mut outs);
            self.prev_ref_frame_num = 0;
        } else {
            let max = sps.max_frame_num();
            if sh.frame_num != self.prev_ref_frame_num
                && sh.frame_num != (self.prev_ref_frame_num + 1) % max
            {
                self.fill_frame_num_gap(sh, sps, pts)?;
            }
        }
        self.read_out(outs, frames)?;
        let poc = self.poc_state.compute(sh, sps);
        let target = (0..self.accel.surfaces().len())
            .find(|&i| !self.busy(i))
            .ok_or_else(|| decode_err("no free surface"))?;
        self.pending = Some(Pending {
            target,
            sps: sps.clone(),
            pps: pps.clone(),
            first: sh.clone(),
            poc,
            pts,
            slices: Vec::new(),
        });
        Ok(())
    }

    fn fill_frame_num_gap(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pts: i64) -> Result<()> {
        let max = sps.max_frame_num();
        let last = self
            .dpb
            .entries
            .iter()
            .filter(|e| !e.non_existing)
            .max_by_key(|e| e.frame.seq)
            .map(|e| e.frame.index);
        let meta = Arc::new(output_meta(pts));
        let poc_state = &mut self.poc_state;
        let mut make = |_| {
            (
                Surface {
                    index: UNASSIGNED,
                    top: 0,
                    bottom: 0,
                    seq: 0,
                },
                0,
                meta.clone(),
            )
        };
        let mut update = |frame_num| poc_state.update_gap_frame(frame_num, sps);
        self.dpb.fill_frame_num_gap(
            self.prev_ref_frame_num,
            sh.frame_num,
            max,
            sps.max_num_ref_frames as usize,
            &mut make,
            &mut update,
        );
        self.prev_ref_frame_num = (sh.frame_num + max - 1) % max;
        let mut used: Vec<usize> = self
            .dpb
            .entries
            .iter()
            .map(|e| e.frame.index)
            .filter(|&i| i != UNASSIGNED)
            .collect();
        used.extend(self.ready.iter().map(|r| r.0));
        used.extend((0..self.accel.surfaces().len()).filter(|&i| self.accel.held(i)));
        let mut free = (0..self.accel.surfaces().len()).filter(|i| !used.contains(i));
        let mut fills = Vec::new();
        for e in self
            .dpb
            .entries
            .iter_mut()
            .filter(|e| e.frame.index == UNASSIGNED)
        {
            let index = free
                .next()
                .ok_or_else(|| decode_err("no free surface for a frame_num gap"))?;
            self.seq += 1;
            e.frame = Surface {
                index,
                top: 0,
                bottom: 0,
                seq: self.seq,
            };
            fills.push(index);
        }
        for index in fills {
            self.accel.fill(index, last).map_err(decode_err)?;
        }
        Ok(())
    }

    fn submit_pending(&mut self, frames: &mut Vec<Picture>) -> Result<()> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        if p.slices.is_empty() {
            return Err(decode_err("a picture without slices"));
        }
        let surfaces = self.accel.surfaces();
        let target_id = surfaces
            .get(p.target)
            .copied()
            .ok_or_else(|| decode_err("surface out of range"))?;
        if self
            .dpb
            .entries
            .iter()
            .filter(|e| e.mark != RefMark::Unused)
            .count()
            > 16
        {
            return Err(decode_err("more than 16 reference frames"));
        }
        let mut pic = picture_params(&p.sps, &p.pps, &p.first);
        pic.CurrPic = VAPictureH264 {
            picture_id: target_id,
            frame_idx: p.first.frame_num,
            flags: 0,
            TopFieldOrderCnt: p.poc.top,
            BottomFieldOrderCnt: p.poc.bottom,
            va_reserved: [0; ffi::VA_PADDING_LOW],
        };
        for (slot, e) in pic.ReferenceFrames.iter_mut().zip(
            self.dpb
                .entries
                .iter()
                .filter(|e| e.mark != RefMark::Unused),
        ) {
            *slot = va_ref(e, surfaces);
        }
        let iq = iq_matrix(&p.pps);
        self.accel
            .decode_h264(p.target, &pic, &iq, &p.slices)
            .map_err(decode_err)?;

        self.poc_state.update(&p.first, &p.poc);
        let mmco5 = p.first.has_mmco5();
        if p.first.nal_ref_idc != 0 {
            self.prev_ref_frame_num = if mmco5 { 0 } else { p.first.frame_num };
        }
        let temp = if mmco5 { p.poc.frame() } else { 0 };
        self.seq += 1;
        let surface = Surface {
            index: p.target,
            top: p.poc.top.wrapping_sub(temp),
            bottom: p.poc.bottom.wrapping_sub(temp),
            seq: self.seq,
        };
        let fpoc = if mmco5 { 0 } else { p.poc.frame() };
        let meta = Arc::new(output_meta(p.pts));
        let mut outs = Vec::new();
        self.dpb.store_picture(
            &p.first,
            surface,
            fpoc,
            p.sps.max_frame_num(),
            p.sps.max_num_ref_frames as usize,
            meta,
            &mut outs,
        );
        self.read_out(outs, frames)
    }
}

fn va_ref(e: &DpbEntry<Surface>, surfaces: &[ffi::VASurfaceID]) -> VAPictureH264 {
    let long = e.mark == RefMark::Long;
    VAPictureH264 {
        picture_id: surfaces
            .get(e.frame.index)
            .copied()
            .unwrap_or(ffi::VA_INVALID_SURFACE),
        frame_idx: if long {
            e.long_term_frame_idx
        } else {
            e.frame_num
        },
        flags: if long {
            ffi::VA_PICTURE_H264_LONG_TERM_REFERENCE
        } else {
            ffi::VA_PICTURE_H264_SHORT_TERM_REFERENCE
        },
        TopFieldOrderCnt: e.frame.top,
        BottomFieldOrderCnt: e.frame.bottom,
        va_reserved: [0; ffi::VA_PADDING_LOW],
    }
}

fn output_meta(pts: i64) -> OutputMeta {
    OutputMeta {
        pts,
        ..Default::default()
    }
}

fn flag(on: bool, bit: u32) -> u32 {
    u32::from(on) << bit
}

pub fn picture_params(sps: &Sps, pps: &Pps, sh: &SliceHeader) -> VAPictureParameterBufferH264 {
    let seq_fields = (sps.chroma_format_idc & 3) << seq_bits::CHROMA_FORMAT_IDC
        | flag(
            sps.separate_colour_plane,
            seq_bits::RESIDUAL_COLOUR_TRANSFORM,
        )
        | flag(
            sps.gaps_in_frame_num_allowed,
            seq_bits::GAPS_IN_FRAME_NUM_ALLOWED,
        )
        | flag(sps.frame_mbs_only, seq_bits::FRAME_MBS_ONLY)
        | flag(
            sps.mb_adaptive_frame_field,
            seq_bits::MB_ADAPTIVE_FRAME_FIELD,
        )
        | flag(sps.direct_8x8_inference, seq_bits::DIRECT_8X8_INFERENCE)
        | flag(sps.level_idc >= 31, seq_bits::MIN_LUMA_BI_PRED_SIZE_8X8)
        | (sps.log2_max_frame_num.saturating_sub(4) & 15) << seq_bits::LOG2_MAX_FRAME_NUM_MINUS4
        | (sps.pic_order_cnt_type & 3) << seq_bits::PIC_ORDER_CNT_TYPE
        | (sps.log2_max_poc_lsb.saturating_sub(4) & 15)
            << seq_bits::LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4
        | flag(
            sps.delta_pic_order_always_zero,
            seq_bits::DELTA_PIC_ORDER_ALWAYS_ZERO,
        );
    let pic_fields = flag(pps.entropy_coding_mode, pic_bits::ENTROPY_CODING_MODE)
        | flag(pps.weighted_pred, pic_bits::WEIGHTED_PRED)
        | (pps.weighted_bipred_idc & 3) << pic_bits::WEIGHTED_BIPRED_IDC
        | flag(pps.transform_8x8_mode, pic_bits::TRANSFORM_8X8_MODE)
        | flag(sh.field_pic, pic_bits::FIELD_PIC)
        | flag(pps.constrained_intra_pred, pic_bits::CONSTRAINED_INTRA_PRED)
        | flag(
            pps.bottom_field_pic_order_in_frame_present,
            pic_bits::PIC_ORDER_PRESENT,
        )
        | flag(
            pps.deblocking_filter_control_present,
            pic_bits::DEBLOCKING_FILTER_CONTROL_PRESENT,
        )
        | flag(
            pps.redundant_pic_cnt_present,
            pic_bits::REDUNDANT_PIC_CNT_PRESENT,
        )
        | flag(sh.nal_ref_idc != 0, pic_bits::REFERENCE_PIC);
    let narrow = |v: u32| u16::try_from(v.saturating_sub(1)).unwrap_or(u16::MAX);
    let small = |v: i32| i8::try_from(v).unwrap_or(0);
    VAPictureParameterBufferH264 {
        CurrPic: VAPictureH264::INVALID,
        ReferenceFrames: [VAPictureH264::INVALID; 16],
        picture_width_in_mbs_minus1: narrow(sps.pic_width_in_mbs),
        picture_height_in_mbs_minus1: narrow(sps.frame_height_in_mbs()),
        bit_depth_luma_minus8: u8::try_from(sps.bit_depth_luma.saturating_sub(8)).unwrap_or(0),
        bit_depth_chroma_minus8: u8::try_from(sps.bit_depth_chroma.saturating_sub(8)).unwrap_or(0),
        num_ref_frames: u8::try_from(sps.max_num_ref_frames).unwrap_or(16),
        seq_fields,
        num_slice_groups_minus1: 0,
        slice_group_map_type: 0,
        slice_group_change_rate_minus1: 0,
        pic_init_qp_minus26: small(pps.pic_init_qp - 26),
        pic_init_qs_minus26: small(pps.pic_init_qs - 26),
        chroma_qp_index_offset: small(pps.chroma_qp_index_offset),
        second_chroma_qp_index_offset: small(pps.second_chroma_qp_index_offset),
        pic_fields,
        frame_num: u16::try_from(sh.frame_num).unwrap_or(0),
        va_reserved: [0; ffi::VA_PADDING_MEDIUM],
    }
}

pub fn iq_matrix(pps: &Pps) -> VAIQMatrixBufferH264 {
    VAIQMatrixBufferH264 {
        ScalingList4x4: pps.scaling.m4,
        ScalingList8x8: [pps.scaling.m8[0], pps.scaling.m8[1]],
        va_reserved: [0; ffi::VA_PADDING_LOW],
    }
}

pub fn slice_params(
    sh: &SliceHeader,
    nal_len: usize,
    refs: &[Vec<VAPictureH264>; 2],
) -> Result<VASliceParameterBufferH264> {
    let bit_offset = sh
        .header_bits
        .checked_add(8)
        .and_then(|b| u16::try_from(b).ok())
        .ok_or_else(|| decode_err("slice header too long"))?;
    let mut s = VASliceParameterBufferH264 {
        slice_data_size: u32::try_from(nal_len).map_err(|_| decode_err("slice too large"))?,
        slice_data_offset: 0,
        slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
        slice_data_bit_offset: bit_offset,
        first_mb_in_slice: u16::try_from(sh.first_mb_in_slice)
            .map_err(|_| decode_err("first_mb_in_slice out of range"))?,
        slice_type: (sh.slice_type_raw % 5) as u8,
        direct_spatial_mv_pred_flag: u8::from(sh.direct_spatial_mv_pred),
        num_ref_idx_l0_active_minus1: u8::try_from(sh.num_ref_idx_active[0].saturating_sub(1))
            .unwrap_or(31)
            .min(31),
        num_ref_idx_l1_active_minus1: u8::try_from(sh.num_ref_idx_active[1].saturating_sub(1))
            .unwrap_or(31)
            .min(31),
        cabac_init_idc: u8::try_from(sh.cabac_init_idc).unwrap_or(0),
        slice_qp_delta: i8::try_from(sh.slice_qp_delta)
            .map_err(|_| decode_err("slice_qp_delta out of range"))?,
        disable_deblocking_filter_idc: u8::try_from(sh.disable_deblocking_filter_idc).unwrap_or(0),
        slice_alpha_c0_offset_div2: i8::try_from(sh.slice_alpha_c0_offset_div2).unwrap_or(0),
        slice_beta_offset_div2: i8::try_from(sh.slice_beta_offset_div2).unwrap_or(0),
        RefPicList0: [VAPictureH264::INVALID; 32],
        RefPicList1: [VAPictureH264::INVALID; 32],
        luma_log2_weight_denom: 0,
        chroma_log2_weight_denom: 0,
        luma_weight_l0_flag: 0,
        luma_weight_l0: [0; 32],
        luma_offset_l0: [0; 32],
        chroma_weight_l0_flag: 0,
        chroma_weight_l0: [[0; 2]; 32],
        chroma_offset_l0: [[0; 2]; 32],
        luma_weight_l1_flag: 0,
        luma_weight_l1: [0; 32],
        luma_offset_l1: [0; 32],
        chroma_weight_l1_flag: 0,
        chroma_weight_l1: [[0; 2]; 32],
        chroma_offset_l1: [[0; 2]; 32],
        va_reserved: [0; ffi::VA_PADDING_LOW],
    };
    for (dst, src) in s.RefPicList0.iter_mut().zip(&refs[0]) {
        *dst = *src;
    }
    for (dst, src) in s.RefPicList1.iter_mut().zip(&refs[1]) {
        *dst = *src;
    }
    if let Some(w) = &sh.pred_weight_table {
        s.luma_log2_weight_denom = u8::try_from(w.luma_log2_denom).unwrap_or(0);
        s.chroma_log2_weight_denom = u8::try_from(w.chroma_log2_denom).unwrap_or(0);
        let w16 = |v: i32| i16::try_from(v).unwrap_or(0);
        let lists = [
            (
                &w.l0,
                &mut s.luma_weight_l0,
                &mut s.luma_offset_l0,
                &mut s.chroma_weight_l0,
                &mut s.chroma_offset_l0,
                &mut s.luma_weight_l0_flag,
                &mut s.chroma_weight_l0_flag,
            ),
            (
                &w.l1,
                &mut s.luma_weight_l1,
                &mut s.luma_offset_l1,
                &mut s.chroma_weight_l1,
                &mut s.chroma_offset_l1,
                &mut s.luma_weight_l1_flag,
                &mut s.chroma_weight_l1_flag,
            ),
        ];
        for (entries, lw, lo, cw, co, lflag, cflag) in lists {
            if entries.is_empty() {
                continue;
            }
            *lflag = 1;
            *cflag = 1;
            for (i, e) in entries.iter().take(32).enumerate() {
                lw[i] = w16(e.luma_weight);
                lo[i] = w16(e.luma_offset);
                for c in 0..2 {
                    cw[i][c] = w16(e.chroma_weight[c]);
                    co[i][c] = w16(e.chroma_offset[c]);
                }
            }
        }
    }
    Ok(s)
}
