use std::collections::VecDeque;
use std::ops::Range;

use super::backend;
use super::ffi::{self, VASurfaceID};
use super::va::Session;
use super::{Accel, LAG, Picture};
use crate::av1::parser::{
    FrameHeaderObu, FrameType, MAX_SEGMENTS, NUM_REF_FRAMES, ObuAction, ObuType, Parser,
    SEG_LVL_MAX, SequenceHeaderObu, Tile,
};
use crate::gpu::biplanar::Geometry;
use crate::gpu::{Backend, DecoderBackend, GpuFrame};
use crate::media::{Error, Frame, Packet, Result};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Segmentation {
    pub segment_info_fields: u32,
    pub feature_data: [[i16; 8]; 8],
    pub feature_mask: [u8; 8],
    pub va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FilmGrain {
    pub film_grain_info_fields: u32,
    pub grain_seed: u16,
    pub num_y_points: u8,
    pub point_y_value: [u8; 14],
    pub point_y_scaling: [u8; 14],
    pub num_cb_points: u8,
    pub point_cb_value: [u8; 10],
    pub point_cb_scaling: [u8; 10],
    pub num_cr_points: u8,
    pub point_cr_value: [u8; 10],
    pub point_cr_scaling: [u8; 10],
    pub ar_coeffs_y: [i8; 24],
    pub ar_coeffs_cb: [i8; 25],
    pub ar_coeffs_cr: [i8; 25],
    pub cb_mult: u8,
    pub cb_luma_mult: u8,
    pub cb_offset: u16,
    pub cr_mult: u8,
    pub cr_luma_mult: u8,
    pub cr_offset: u16,
    pub va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct WarpedMotion {
    pub wmtype: u32,
    pub wmmat: [i32; 8],
    pub invalid: u8,
    pub va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PictureParams {
    pub profile: u8,
    pub order_hint_bits_minus_1: u8,
    pub bit_depth_idx: u8,
    pub matrix_coefficients: u8,
    pub seq_info_fields: u32,
    pub current_frame: VASurfaceID,
    pub current_display_picture: VASurfaceID,
    pub anchor_frames_num: u8,
    pub anchor_frames_list: usize,
    pub frame_width_minus1: u16,
    pub frame_height_minus1: u16,
    pub output_frame_width_in_tiles_minus_1: u16,
    pub output_frame_height_in_tiles_minus_1: u16,
    pub ref_frame_map: [VASurfaceID; 8],
    pub ref_frame_idx: [u8; 7],
    pub primary_ref_frame: u8,
    pub order_hint: u8,
    pub seg_info: Segmentation,
    pub film_grain_info: FilmGrain,
    pub tile_cols: u8,
    pub tile_rows: u8,
    pub width_in_sbs_minus_1: [u16; 63],
    pub height_in_sbs_minus_1: [u16; 63],
    pub tile_count_minus_1: u16,
    pub context_update_tile_id: u16,
    pub pic_info_fields: u32,
    pub superres_scale_denominator: u8,
    pub interp_filter: u8,
    pub filter_level: [u8; 2],
    pub filter_level_u: u8,
    pub filter_level_v: u8,
    pub loop_filter_info_fields: u8,
    pub ref_deltas: [i8; 8],
    pub mode_deltas: [i8; 2],
    pub base_qindex: u8,
    pub y_dc_delta_q: i8,
    pub u_dc_delta_q: i8,
    pub u_ac_delta_q: i8,
    pub v_dc_delta_q: i8,
    pub v_ac_delta_q: i8,
    pub qmatrix_fields: u16,
    pub mode_control_fields: u32,
    pub cdef_damping_minus_3: u8,
    pub cdef_bits: u8,
    pub cdef_y_strengths: [u8; 8],
    pub cdef_uv_strengths: [u8; 8],
    pub loop_restoration_fields: u16,
    pub wm: [WarpedMotion; 7],
    pub va_reserved: [u32; 8],
}

impl Default for PictureParams {
    fn default() -> Self {
        Self {
            profile: 0,
            order_hint_bits_minus_1: 0,
            bit_depth_idx: 0,
            matrix_coefficients: 0,
            seq_info_fields: 0,
            current_frame: ffi::VA_INVALID_SURFACE,
            current_display_picture: ffi::VA_INVALID_SURFACE,
            anchor_frames_num: 0,
            anchor_frames_list: 0,
            frame_width_minus1: 0,
            frame_height_minus1: 0,
            output_frame_width_in_tiles_minus_1: 0,
            output_frame_height_in_tiles_minus_1: 0,
            ref_frame_map: [ffi::VA_INVALID_SURFACE; 8],
            ref_frame_idx: [0; 7],
            primary_ref_frame: 0,
            order_hint: 0,
            seg_info: Segmentation::default(),
            film_grain_info: FilmGrain::default(),
            tile_cols: 0,
            tile_rows: 0,
            width_in_sbs_minus_1: [0; 63],
            height_in_sbs_minus_1: [0; 63],
            tile_count_minus_1: 0,
            context_update_tile_id: 0,
            pic_info_fields: 0,
            superres_scale_denominator: 8,
            interp_filter: 0,
            filter_level: [0; 2],
            filter_level_u: 0,
            filter_level_v: 0,
            loop_filter_info_fields: 0,
            ref_deltas: [0; 8],
            mode_deltas: [0; 2],
            base_qindex: 0,
            y_dc_delta_q: 0,
            u_dc_delta_q: 0,
            u_ac_delta_q: 0,
            v_dc_delta_q: 0,
            v_ac_delta_q: 0,
            qmatrix_fields: 0,
            mode_control_fields: 0,
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_strengths: [0; 8],
            cdef_uv_strengths: [0; 8],
            loop_restoration_fields: 0,
            wm: [WarpedMotion::default(); 7],
            va_reserved: [0; 8],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SliceParams {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub tile_row: u16,
    pub tile_column: u16,
    pub tg_start: u16,
    pub tg_end: u16,
    pub anchor_frame_idx: u8,
    pub tile_idx_in_tile_list: u16,
    pub va_reserved: [u32; 4],
}

fn bits(fields: &[(bool, u32)]) -> u32 {
    fields.iter().fold(0, |a, &(v, s)| a | (u32::from(v) << s))
}

fn film_grain(fh: &FrameHeaderObu) -> FilmGrain {
    let fg = &fh.film_grain_params;
    let mut out = FilmGrain {
        film_grain_info_fields: u32::from(fg.apply_grain)
            | (u32::from(fg.chroma_scaling_from_luma) << 1)
            | ((u32::from(fg.grain_scaling_minus_8) & 3) << 2)
            | ((fg.ar_coeff_lag & 3) << 4)
            | ((u32::from(fg.ar_coeff_shift_minus_6) & 3) << 6)
            | ((u32::from(fg.grain_scale_shift) & 3) << 8)
            | (u32::from(fg.overlap_flag) << 10)
            | (u32::from(fg.clip_to_restricted_range) << 11),
        grain_seed: fg.grain_seed,
        num_y_points: fg.num_y_points,
        num_cb_points: fg.num_cb_points,
        num_cr_points: fg.num_cr_points,
        cb_mult: fg.cb_mult,
        cb_luma_mult: fg.cb_luma_mult,
        cb_offset: fg.cb_offset,
        cr_mult: fg.cr_mult,
        cr_luma_mult: fg.cr_luma_mult,
        cr_offset: fg.cr_offset,
        ..Default::default()
    };
    out.point_y_value.copy_from_slice(&fg.point_y_value[..14]);
    out.point_y_scaling
        .copy_from_slice(&fg.point_y_scaling[..14]);
    out.point_cb_value.copy_from_slice(&fg.point_cb_value[..10]);
    out.point_cb_scaling
        .copy_from_slice(&fg.point_cb_scaling[..10]);
    out.point_cr_value.copy_from_slice(&fg.point_cr_value[..10]);
    out.point_cr_scaling
        .copy_from_slice(&fg.point_cr_scaling[..10]);
    let centre = |v: u8| (i16::from(v) - 128) as i8;
    for (d, &s) in out.ar_coeffs_y.iter_mut().zip(&fg.ar_coeffs_y_plus_128) {
        *d = centre(s);
    }
    for (d, &s) in out.ar_coeffs_cb.iter_mut().zip(&fg.ar_coeffs_cb_plus_128) {
        *d = centre(s);
    }
    for (d, &s) in out.ar_coeffs_cr.iter_mut().zip(&fg.ar_coeffs_cr_plus_128) {
        *d = centre(s);
    }
    out
}

fn cdef_strength(pri: u32, sec: u32) -> u8 {
    let sec = if sec == 4 { 3 } else { sec };
    (((pri & 0xf) << 2) | (sec & 3)) as u8
}

pub fn picture_params(
    seq: &SequenceHeaderObu,
    fh: &FrameHeaderObu,
    (current_frame, current_display_picture): (VASurfaceID, VASurfaceID),
    ref_frame_map: [VASurfaceID; NUM_REF_FRAMES],
    grain: bool,
) -> PictureParams {
    let cc = &seq.color_config;
    let mut p = PictureParams {
        profile: seq.seq_profile,
        order_hint_bits_minus_1: seq.order_hint_bits_minus_1.max(0) as u8,
        bit_depth_idx: match seq.bit_depth {
            10 => 1,
            12 => 2,
            _ => 0,
        },
        matrix_coefficients: cc.matrix_coefficients,
        current_frame,
        current_display_picture,
        frame_width_minus1: fh.upscaled_width.saturating_sub(1) as u16,
        frame_height_minus1: fh.frame_height.saturating_sub(1) as u16,
        primary_ref_frame: fh.primary_ref_frame as u8,
        order_hint: fh.order_hint as u8,
        ref_frame_idx: fh.ref_frame_idx,
        ..Default::default()
    };
    p.seq_info_fields = bits(&[
        (seq.still_picture, 0),
        (seq.use_128x128_superblock, 1),
        (seq.enable_filter_intra, 2),
        (seq.enable_intra_edge_filter, 3),
        (seq.enable_interintra_compound, 4),
        (seq.enable_masked_compound, 5),
        (seq.enable_dual_filter, 6),
        (seq.enable_order_hint, 7),
        (seq.enable_jnt_comp, 8),
        (seq.enable_cdef, 9),
        (cc.mono_chrome, 10),
        (cc.color_range, 11),
        (cc.subsampling_x, 12),
        (cc.subsampling_y, 13),
        (cc.chroma_sample_position != 0, 14),
        (seq.film_grain_params_present, 15),
    ]);
    if !(fh.frame_type == FrameType::KeyFrame && fh.show_frame) {
        p.ref_frame_map = ref_frame_map;
    }
    let seg = &fh.segmentation_params;
    p.seg_info.segment_info_fields = bits(&[
        (seg.segmentation_enabled, 0),
        (seg.segmentation_update_map, 1),
        (seg.segmentation_temporal_update, 2),
        (seg.segmentation_update_data, 3),
    ]);
    p.seg_info.feature_data = seg.feature_data;
    for i in 0..MAX_SEGMENTS {
        p.seg_info.feature_mask[i] = (0..SEG_LVL_MAX)
            .filter(|&j| seg.feature_enabled[i][j])
            .fold(0u8, |m, j| m | (1 << j));
    }
    if grain && fh.film_grain_params.apply_grain {
        p.film_grain_info = film_grain(fh);
    }
    let ti = &fh.tile_info;
    p.tile_cols = ti.tile_cols as u8;
    p.tile_rows = ti.tile_rows as u8;
    for (d, &s) in p
        .width_in_sbs_minus_1
        .iter_mut()
        .zip(&ti.width_in_sbs_minus_1)
    {
        *d = s as u16;
    }
    for (d, &s) in p
        .height_in_sbs_minus_1
        .iter_mut()
        .zip(&ti.height_in_sbs_minus_1)
    {
        *d = s as u16;
    }
    p.context_update_tile_id = ti.context_update_tile_id as u16;
    p.pic_info_fields = (fh.frame_type as u32 & 3)
        | bits(&[
            (fh.show_frame, 2),
            (fh.showable_frame, 3),
            (fh.error_resilient_mode, 4),
            (fh.disable_cdf_update, 5),
            (fh.allow_screen_content_tools != 0, 6),
            (fh.force_integer_mv != 0, 7),
            (fh.allow_intrabc, 8),
            (fh.use_superres, 9),
            (fh.allow_high_precision_mv, 10),
            (fh.is_motion_mode_switchable, 11),
            (fh.use_ref_frame_mvs, 12),
            (fh.disable_frame_end_update_cdf, 13),
            (ti.uniform_tile_spacing_flag, 14),
            (fh.allow_warped_motion, 15),
        ]);
    p.superres_scale_denominator = fh.superres_denom as u8;
    p.interp_filter = fh.interpolation_filter;
    let lf = &fh.loop_filter_params;
    p.filter_level = [lf.loop_filter_level[0], lf.loop_filter_level[1]];
    p.filter_level_u = lf.loop_filter_level[2];
    p.filter_level_v = lf.loop_filter_level[3];
    p.loop_filter_info_fields = (lf.loop_filter_sharpness & 7)
        | (u8::from(lf.loop_filter_delta_enabled) << 3)
        | (u8::from(lf.loop_filter_delta_update) << 4);
    p.ref_deltas = lf.loop_filter_ref_deltas;
    p.mode_deltas = lf.loop_filter_mode_deltas;
    let q = &fh.quantization_params;
    p.base_qindex = q.base_q_idx as u8;
    p.y_dc_delta_q = q.delta_q_y_dc as i8;
    p.u_dc_delta_q = q.delta_q_u_dc as i8;
    p.u_ac_delta_q = q.delta_q_u_ac as i8;
    p.v_dc_delta_q = q.delta_q_v_dc as i8;
    p.v_ac_delta_q = q.delta_q_v_ac as i8;
    p.qmatrix_fields = u16::from(q.using_qmatrix)
        | ((q.qm_y as u16 & 0xf) << 1)
        | ((q.qm_u as u16 & 0xf) << 5)
        | ((q.qm_v as u16 & 0xf) << 9);
    p.mode_control_fields = u32::from(q.delta_q_present)
        | ((q.delta_q_res & 3) << 1)
        | (u32::from(lf.delta_lf_present) << 3)
        | ((u32::from(lf.delta_lf_res) & 3) << 4)
        | (u32::from(lf.delta_lf_multi) << 6)
        | ((u32::from(fh.tx_mode) & 3) << 7)
        | (u32::from(fh.reference_select) << 9)
        | (u32::from(fh.reduced_tx_set) << 10)
        | (u32::from(fh.skip_mode_present) << 11);
    let cdef = &fh.cdef_params;
    p.cdef_damping_minus_3 = cdef.cdef_damping.saturating_sub(3) as u8;
    p.cdef_bits = cdef.cdef_bits as u8;
    for i in 0..(1usize << cdef.cdef_bits).min(8) {
        p.cdef_y_strengths[i] =
            cdef_strength(cdef.cdef_y_pri_strength[i], cdef.cdef_y_sec_strength[i]);
        p.cdef_uv_strengths[i] =
            cdef_strength(cdef.cdef_uv_pri_strength[i], cdef.cdef_uv_sec_strength[i]);
    }
    let lr = &fh.loop_restoration_params;
    p.loop_restoration_fields = (u16::from(lr.frame_restoration_type[0]) & 3)
        | ((u16::from(lr.frame_restoration_type[1]) & 3) << 2)
        | ((u16::from(lr.frame_restoration_type[2]) & 3) << 4)
        | ((u16::from(lr.lr_unit_shift) & 3) << 6)
        | ((u16::from(lr.lr_uv_shift) & 1) << 8);
    let gm = &fh.global_motion_params;
    for (i, wm) in p.wm.iter_mut().enumerate() {
        let r = i + 1;
        wm.wmtype = u32::from(gm.gm_type[r]);
        wm.wmmat[..6].copy_from_slice(&gm.gm_params[r]);
        wm.invalid = u8::from(!gm.warp_valid[r]);
    }
    p
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot {
    recon: usize,
    display: usize,
}

struct Group {
    tiles: Range<usize>,
    data: Range<usize>,
    start: u16,
    end: u16,
}

pub struct Av1Decoder {
    session: Option<Session>,
    parser: Parser,
    size: (u32, u32),
    bits: u8,
    slots: [Option<Slot>; NUM_REF_FRAMES],
    queue: VecDeque<(usize, i64)>,
    current: Option<(FrameHeaderObu, Slot)>,
    found: Vec<Tile>,
    params: Vec<SliceParams>,
    groups: Vec<Group>,
    name: String,
    gpu: bool,
    stills: bool,
    grain: bool,
    display: Option<super::Device>,
    held: usize,
}

fn invalid(e: String) -> Error {
    Error::Invalid(format!("AV1: {e}"))
}

pub fn config_obus(extradata: &[u8]) -> &[u8] {
    match extradata {
        [0x81, _, _, _, rest @ ..] => rest,
        _ => extradata,
    }
}

impl Av1Decoder {
    pub fn on(display: Option<super::Device>, held: usize, extradata: &[u8]) -> Result<Self> {
        let vendor = match &display {
            Some(d) => d.vendor().to_string(),
            None => super::va::probe()
                .map_err(Error::Backend)?
                .vendor()
                .to_string(),
        };
        let supported = match &display {
            Some(d) => super::supports(d, ffi::VAProfileAV1Profile0, crate::gpu::Direction::Decode),
            None => super::va::probe().is_ok_and(|d| {
                super::supports(&d, ffi::VAProfileAV1Profile0, crate::gpu::Direction::Decode)
            }),
        };
        if !supported {
            return Err(Error::Unsupported(format!(
                "this VA-API driver ({vendor}) has no AV1 Decode"
            )));
        }
        let mut d = Self {
            session: None,
            parser: Parser::default(),
            size: (0, 0),
            bits: 8,
            slots: [None; NUM_REF_FRAMES],
            queue: VecDeque::new(),
            current: None,
            found: Vec::new(),
            params: Vec::new(),
            groups: Vec::new(),
            name: format!("VA-API AV1 ({vendor})"),
            gpu: false,
            stills: false,
            grain: true,
            display,
            held,
        };
        let obus = config_obus(extradata);
        if !obus.is_empty() {
            let mut out = Vec::new();
            d.decode_unit(obus, 0, &mut out)?;
        }
        Ok(d)
    }

    #[cfg(test)]
    pub fn grain(&mut self, on: bool) {
        self.grain = on;
    }

    fn open(&mut self, seq: &SequenceHeaderObu, size: (u32, u32)) -> Result<()> {
        let cc = &seq.color_config;
        if seq.seq_profile != 0
            || cc.mono_chrome
            || !(cc.subsampling_x && cc.subsampling_y)
            || !matches!(seq.bit_depth, 8 | 10)
        {
            return Err(Error::Unsupported(format!(
                "AV1 profile {} at {}-bit{} on the GPU",
                seq.seq_profile,
                seq.bit_depth,
                if cc.mono_chrome { " monochrome" } else { "" }
            )));
        }
        super::large_enough(size.0, size.1)?;
        self.session = None;
        self.slots = [None; NUM_REF_FRAMES];
        let geometry = Geometry {
            crop: (0, 0, size.0, size.1),
            bits: u32::from(seq.bit_depth),
        };
        let extra = if seq.film_grain_params_present {
            NUM_REF_FRAMES + 2
        } else {
            0
        };
        let mut session = Session::new(
            self.display.as_ref(),
            &[ffi::VAProfileAV1Profile0],
            size,
            NUM_REF_FRAMES + 2 + LAG + extra + self.held,
            geometry,
        )
        .map_err(backend)?;
        if self.stills {
            session.direct();
        }
        self.name = format!("VA-API AV1 ({})", session.vendor());
        self.session = Some(session);
        self.size = size;
        self.bits = seq.bit_depth;
        Ok(())
    }

    fn free_surface(&self, besides: Option<usize>) -> Result<usize> {
        let s = self
            .session
            .as_ref()
            .ok_or_else(|| Error::Invalid("AV1 stream does not start with a key frame".into()))?;
        (0..s.surfaces().len())
            .find(|&i| {
                Some(i) != besides
                    && !s.held(i)
                    && !self
                        .slots
                        .iter()
                        .flatten()
                        .any(|s| s.recon == i || s.display == i)
                    && !self.queue.iter().any(|&(j, _)| j == i)
            })
            .ok_or_else(|| Error::Backend("no free VA-API surface for AV1".into()))
    }

    fn read(&mut self, index: usize, pts: i64) -> Result<Picture> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Invalid("no VA-API session".into()))?;
        s.gpu = self.gpu;
        Ok(s.read(index).map_err(backend)?.at(pts))
    }

    fn drain(&mut self, keep: usize, out: &mut Vec<Picture>) -> Result<()> {
        while self.queue.len() > keep {
            let Some((index, pts)) = self.queue.pop_front() else {
                break;
            };
            out.push(self.read(index, pts)?);
        }
        Ok(())
    }

    fn show(&mut self, index: usize, pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        if let Some(s) = self.session.as_mut() {
            s.gpu = self.gpu;
            s.prefetch(index);
        }
        self.queue.push_back((index, pts));
        self.drain(LAG, out)
    }

    fn show_existing(
        &mut self,
        fh: &FrameHeaderObu,
        pts: i64,
        out: &mut Vec<Picture>,
    ) -> Result<()> {
        let slot = self.slots[fh.frame_to_show_map_idx as usize & 7]
            .ok_or_else(|| Error::Invalid("AV1 show_existing_frame of an empty slot".into()))?;
        if fh.frame_type == FrameType::KeyFrame {
            self.parser.ref_frame_update(fh).map_err(invalid)?;
            self.slots = [Some(slot); NUM_REF_FRAMES];
        }
        self.show(slot.display, pts, out)
    }

    fn begin(&mut self, fh: FrameHeaderObu, out: &mut Vec<Picture>) -> Result<()> {
        let seq = self.parser.sequence().map_err(invalid)?.clone();
        let size = (fh.upscaled_width, fh.frame_height);
        let fresh = fh.frame_type == FrameType::KeyFrame;
        if self.session.is_none() || (fresh && (size != self.size || seq.bit_depth != self.bits)) {
            if !fresh {
                return Err(Error::Invalid(
                    "AV1 stream does not start with a key frame".into(),
                ));
            }
            self.drain(0, out)?;
            self.open(&seq, size)?;
        }
        if size != self.size {
            return Err(Error::Unsupported(
                "AV1 reference scaling on the GPU".into(),
            ));
        }
        let recon = self.free_surface(None)?;
        let display = if self.grain && fh.film_grain_params.apply_grain {
            self.free_surface(Some(recon))?
        } else {
            recon
        };
        self.found.clear();
        self.groups.clear();
        self.current = Some((fh, Slot { recon, display }));
        Ok(())
    }

    fn tile_group(&mut self, data: &[u8], at: usize) -> Result<bool> {
        if self.current.is_none() {
            return Err(Error::Invalid(
                "AV1 tile group without a frame header".into(),
            ));
        }
        let first = self.found.len();
        let tg = self
            .parser
            .parse_tile_group_obu(data, &mut self.found)
            .map_err(invalid)?;
        self.groups.push(Group {
            tiles: first..self.found.len(),
            data: at..at + data.len(),
            start: tg.tg_start as u16,
            end: tg.tg_end as u16,
        });
        Ok(tg.last)
    }

    fn submit(&mut self, data: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        let Some((fh, slot)) = self.current.take() else {
            return Ok(());
        };
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Invalid("no VA-API session".into()))?;
        let ids = session.surfaces();
        let id = |i: usize| ids.get(i).copied().unwrap_or(ffi::VA_INVALID_SURFACE);
        let refs =
            std::array::from_fn(|i| self.slots[i].map_or(ffi::VA_INVALID_SURFACE, |s| id(s.recon)));
        let seq = self.parser.sequence().map_err(invalid)?;
        let grained = slot.display != slot.recon;
        let pic = picture_params(
            seq,
            &fh,
            (
                id(slot.recon),
                if grained {
                    id(slot.display)
                } else {
                    ffi::VA_INVALID_SURFACE
                },
            ),
            refs,
            grained,
        );
        self.params.clear();
        for g in &self.groups {
            for t in &self.found[g.tiles.clone()] {
                self.params.push(SliceParams {
                    slice_data_size: t.tile_size,
                    slice_data_offset: t.tile_offset,
                    tile_row: t.tile_row as u16,
                    tile_column: t.tile_col as u16,
                    tg_start: g.start,
                    tg_end: g.end,
                    ..Default::default()
                });
            }
        }
        let mut groups: Vec<(&[SliceParams], &[u8])> = Vec::with_capacity(self.groups.len());
        for g in &self.groups {
            let params = self
                .params
                .get(g.tiles.clone())
                .ok_or_else(|| Error::Invalid("AV1 tile list".into()))?;
            let bytes = data
                .get(g.data.clone())
                .ok_or_else(|| Error::Invalid("AV1 tile group past the packet".into()))?;
            groups.push((params, bytes));
        }
        session
            .decode_groups(slot.recon, &pic, &groups)
            .map_err(backend)?;
        self.parser.ref_frame_update(&fh).map_err(invalid)?;
        for i in 0..NUM_REF_FRAMES {
            if (fh.refresh_frame_flags >> i) & 1 == 1 {
                self.slots[i] = Some(slot);
            }
        }
        if fh.show_frame {
            self.show(slot.display, pts, out)?;
        }
        Ok(())
    }

    fn decode_unit(&mut self, data: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        let mut pos = 0;
        while pos < data.len() {
            let rest = &data[pos..];
            let obu = match self.parser.read_obu(rest).map_err(invalid)? {
                ObuAction::Process(obu) => obu,
                ObuAction::Drop(n) => {
                    pos += n.max(1);
                    continue;
                }
            };
            let at = pos + obu.bytes_used - obu.data.len();
            match obu.header.obu_type {
                ObuType::SequenceHeader => {
                    self.parser
                        .parse_sequence_header_obu(&obu)
                        .map_err(invalid)?;
                }
                ObuType::TemporalDelimiter => self.parser.parse_temporal_delimiter_obu(),
                ObuType::FrameHeader | ObuType::Frame => {
                    if self.current.is_some() {
                        return Err(Error::Invalid(
                            "AV1 frame header before the last tile group".into(),
                        ));
                    }
                    let fh = self.parser.parse_frame_header_obu(&obu).map_err(invalid)?;
                    if fh.show_existing_frame {
                        self.show_existing(&fh, pts, out)?;
                    } else {
                        let header_bytes = fh.header_bytes;
                        self.begin(fh, out)?;
                        if obu.header.obu_type == ObuType::Frame {
                            let tiles = obu.data.get(header_bytes..).unwrap_or_default();
                            if self.tile_group(tiles, at + header_bytes)? {
                                self.submit(data, pts, out)?;
                            }
                        }
                    }
                }
                ObuType::TileGroup => {
                    if self.tile_group(obu.data, at)? {
                        self.submit(data, pts, out)?;
                    }
                }
                ObuType::TileList => {
                    return Err(Error::Unsupported("AV1 large-scale tiles".into()));
                }
                _ => {}
            }
            pos += obu.bytes_used.max(1);
        }
        if self.current.take().is_some() {
            return Err(Error::Invalid(
                "AV1 temporal unit ends inside a frame".into(),
            ));
        }
        Ok(())
    }

    pub fn decode(&mut self, packet: &Packet, gpu: bool) -> Result<Vec<Picture>> {
        self.gpu = gpu;
        let mut out = Vec::new();
        self.decode_unit(&packet.data, packet.pts, &mut out)?;
        Ok(out)
    }

    pub fn flush(&mut self, gpu: bool) -> Result<Vec<Picture>> {
        self.gpu = gpu;
        let mut out = Vec::new();
        self.drain(0, &mut out)?;
        Ok(out)
    }

    pub fn reset(&mut self) {
        self.queue.clear();
        self.slots = [None; NUM_REF_FRAMES];
        self.current = None;
        self.parser.reset();
    }
}

impl DecoderBackend for Av1Decoder {
    fn backend(&self) -> Backend {
        Backend::VaApi
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        super::frames(Av1Decoder::decode(self, packet, false)?)
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        super::frames(Av1Decoder::flush(self, false)?)
    }

    fn reset(&mut self) {
        Av1Decoder::reset(self)
    }

    fn stills(&mut self) {
        self.stills = true;
        if let Some(s) = &mut self.session {
            s.direct();
        }
    }

    fn decode_gpu(&mut self, packet: &Packet) -> Result<Vec<GpuFrame>> {
        Ok(super::gpu_frames(Av1Decoder::decode(self, packet, true)?))
    }

    fn flush_gpu(&mut self) -> Result<Vec<GpuFrame>> {
        Ok(super::gpu_frames(Av1Decoder::flush(self, true)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        if data.len() < 32 || &data[..4] != b"DKIF" {
            return out;
        }
        let mut pos = u16::from_le_bytes([data[6], data[7]]) as usize;
        while pos + 12 <= data.len() {
            let size = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 12;
            if pos + size > data.len() {
                break;
            }
            out.push(&data[pos..pos + size]);
            pos += size;
        }
        out
    }

    fn planes(f: &Frame) -> Vec<u8> {
        let bps = f.format.bytes_per_sample();
        f.planes
            .iter()
            .flat_map(|p| {
                (0..p.height).flat_map(move |y| {
                    p.data[y * p.stride..y * p.stride + p.width * bps]
                        .iter()
                        .copied()
                })
            })
            .collect()
    }

    fn psnr(got: &[u8], want: &[u8], ten: bool) -> f64 {
        let (sum, n, peak) = if ten {
            let s: f64 = got
                .chunks(2)
                .zip(want.chunks(2))
                .map(|(a, b)| {
                    let d = f64::from(u16::from_le_bytes([a[0], a[1]]))
                        - f64::from(u16::from_le_bytes([b[0], b[1]]));
                    d * d
                })
                .sum();
            (s, got.len() / 2, 1023.0)
        } else {
            let s: f64 = got
                .iter()
                .zip(want)
                .map(|(&a, &b)| {
                    let d = f64::from(a) - f64::from(b);
                    d * d
                })
                .sum();
            (s, got.len(), 255.0)
        };
        if sum == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (peak * peak / (sum / n as f64)).log10()
    }

    pub struct Outcome {
        pub frames: usize,
        pub exact: usize,
        pub worst: f64,
    }

    pub fn compare(ivf: &Path, yuv: &Path, grain: bool) -> Outcome {
        let data = std::fs::read(ivf).unwrap();
        let reference = std::fs::read(yuv).unwrap();
        let mut dec = Av1Decoder::on(None, super::super::HELD, &[]).unwrap();
        dec.grain(grain);
        let mut frames = Vec::new();
        for (i, c) in ivf_frames(&data).iter().enumerate() {
            let packet = Packet {
                data: c.to_vec(),
                pts: i as i64,
                dts: i as i64,
                keyframe: false,
            };
            for p in dec
                .decode(&packet, false)
                .unwrap_or_else(|e| panic!("{}: unit {i}: {e}", ivf.display()))
            {
                frames.push(p.into_frame().unwrap());
            }
        }
        for p in dec.flush(false).unwrap() {
            frames.push(p.into_frame().unwrap());
        }
        let mut out = Outcome {
            frames: frames.len(),
            exact: 0,
            worst: f64::INFINITY,
        };
        let mut at = 0;
        for f in &frames {
            let got = planes(f);
            let want = reference
                .get(at..at + got.len())
                .unwrap_or_else(|| panic!("{}: more frames than the reference", ivf.display()));
            at += got.len();
            if got == want {
                out.exact += 1;
            } else {
                out.worst = out
                    .worst
                    .min(psnr(&got, want, f.format.bytes_per_sample() == 2));
            }
        }
        assert_eq!(at, reference.len(), "{}: frame count", ivf.display());
        out
    }

    fn has_av1() -> bool {
        super::super::va::probe().is_ok_and(|d| {
            super::super::supports(&d, ffi::VAProfileAV1Profile0, crate::gpu::Direction::Decode)
        })
    }

    fn ffmpeg(args: &[&str]) -> bool {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    #[ignore]
    fn streams_in_tmp_match_dav1d() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/av1");
        let list = std::env::var("PITO_AV1_STREAMS").unwrap_or_default();
        for spec in list.split(',').filter(|s| !s.is_empty()) {
            let (name, reference, grain) = match spec.split(':').collect::<Vec<_>>()[..] {
                [n, r, g] => (n, r, g == "1"),
                [n, r] => (n, r, true),
                [n] => (n, n, true),
                _ => continue,
            };
            let o = compare(
                &dir.join(format!("{name}.ivf")),
                &dir.join(format!("{reference}.yuv")),
                grain,
            );
            eprintln!(
                "{spec}: {} frames, {} bit-exact, worst PSNR {:.2} dB",
                o.frames, o.exact, o.worst
            );
        }
    }

    #[test]
    #[ignore]
    fn speed_in_tmp() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/av1");
        let name = std::env::var("PITO_AV1_BENCH").unwrap_or_else(|_| "s1080_8".into());
        let gpu = std::env::var_os("PITO_AV1_GPU").is_some();
        let first = std::env::var_os("PITO_AV1_FIRST").is_some();
        let data = std::fs::read(dir.join(format!("{name}.ivf"))).unwrap();
        let units = ivf_frames(&data);
        let start = std::time::Instant::now();
        let mut dec = Av1Decoder::on(None, super::super::HELD, &[]).unwrap();
        let mut n = 0;
        let mut sink = 0u64;
        let mut take = |pictures: Vec<Picture>| {
            for p in pictures {
                n += 1;
                match p {
                    Picture::Cpu(f) => sink += u64::from(f.planes[0].data[0]),
                    Picture::Gpu(g) => sink += u64::from(g.width()),
                }
            }
        };
        for (i, c) in units.iter().enumerate() {
            let packet = Packet {
                data: c.to_vec(),
                pts: i as i64,
                dts: i as i64,
                keyframe: false,
            };
            take(dec.decode(&packet, gpu).unwrap());
            if first {
                break;
            }
        }
        take(dec.flush(gpu).unwrap());
        let secs = start.elapsed().as_secs_f64();
        eprintln!(
            "AV1BENCH {name} {} frames {:.2} ms {:.1} fps ({sink})",
            n,
            secs * 1e3,
            n as f64 / secs
        );
    }

    #[test]
    fn vaapi_av1_is_bit_exact_against_dav1d() {
        if !has_av1() {
            eprintln!("SKIPPED: no VA-API AV1 decoder");
            return;
        }
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/gpu");
        std::fs::create_dir_all(&dir).unwrap();
        let ivf = dir.join("av1_test.ivf");
        let yuv = dir.join("av1_test.yuv");
        let (i, y) = (ivf.to_str().unwrap(), yuv.to_str().unwrap());
        if !ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=352x288:rate=30,noise=alls=10:allf=t",
            "-frames:v",
            "24",
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-crf",
            "40",
            "-tile-columns",
            "1",
            "-f",
            "ivf",
            i,
        ]) || !ffmpeg(&[
            "-c:v", "libdav1d", "-i", i, "-f", "rawvideo", "-pix_fmt", "yuv420p", y,
        ]) {
            eprintln!("SKIPPED: no ffmpeg with libaom-av1 and libdav1d");
            return;
        }
        let o = compare(&ivf, &yuv, true);
        assert_eq!(o.exact, o.frames, "worst PSNR {:.2} dB", o.worst);
        assert!(o.frames >= 24);
        let data = std::fs::read(&ivf).unwrap();
        let mut dec = Av1Decoder::on(None, super::super::HELD, &[]).unwrap();
        let mut held = std::collections::VecDeque::new();
        let mut n = 0;
        for (i, c) in ivf_frames(&data).iter().enumerate() {
            let packet = Packet {
                data: c.to_vec(),
                pts: i as i64,
                dts: i as i64,
                keyframe: false,
            };
            for p in dec.decode(&packet, true).unwrap() {
                n += 1;
                held.push_back(p);
                if held.len() > 12 {
                    held.pop_front();
                }
            }
        }
        n += dec.flush(true).unwrap().len();
        assert_eq!(n, o.frames);
    }

    #[test]
    fn layouts_match_va_dec_av1_h() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<Segmentation>(), 156);
        assert_eq!(size_of::<FilmGrain>(), 176);
        assert_eq!(offset_of!(FilmGrain, ar_coeffs_y), 77);
        assert_eq!(offset_of!(FilmGrain, cb_offset), 154);
        assert_eq!(size_of::<WarpedMotion>(), 56);
        assert_eq!(size_of::<SliceParams>(), 40);
        assert_eq!(offset_of!(SliceParams, tile_idx_in_tile_list), 22);
        assert_eq!(offset_of!(PictureParams, anchor_frames_list), 24);
        assert_eq!(offset_of!(PictureParams, ref_frame_map), 40);
        assert_eq!(offset_of!(PictureParams, seg_info), 84);
        assert_eq!(offset_of!(PictureParams, film_grain_info), 240);
        assert_eq!(offset_of!(PictureParams, pic_info_fields), 676);
        assert_eq!(offset_of!(PictureParams, qmatrix_fields), 704);
        assert_eq!(offset_of!(PictureParams, mode_control_fields), 708);
        assert_eq!(offset_of!(PictureParams, loop_restoration_fields), 730);
        assert_eq!(offset_of!(PictureParams, wm), 732);
        assert_eq!(size_of::<PictureParams>(), 1160);
    }
}
