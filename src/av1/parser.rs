use super::helpers;
use super::reader::{Reader, Result};

pub const TOTAL_REFS_PER_FRAME: usize = 8;
pub const NUM_REF_FRAMES: usize = 8;
pub const REFS_PER_FRAME: usize = 7;
pub const MAX_SEGMENTS: usize = 8;
pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_REF_FRAME: usize = 5;
pub const SEG_LVL_MAX: usize = 8;
pub const MAX_TILE_COLS: usize = 64;
pub const MAX_TILE_ROWS: usize = 64;
pub const CDEF_MAX: usize = 1 << 3;
pub const MAX_NUM_PLANES: usize = 3;
pub const MAX_NUM_Y_POINTS: usize = 16;
pub const MAX_NUM_CB_POINTS: usize = 16;
pub const MAX_NUM_CR_POINTS: usize = 16;
pub const MAX_NUM_POS_LUMA: usize = 25;
pub const MAX_NUM_OPERATING_POINTS: usize = 32;
pub const SELECT_SCREEN_CONTENT_TOOLS: u32 = 2;
pub const SELECT_INTEGER_MV: u32 = 2;
pub const PRIMARY_REF_NONE: u32 = 7;
pub const SUPERRES_DENOM_BITS: u32 = 3;
pub const SUPERRES_DENOM_MIN: u32 = 9;
pub const SUPERRES_NUM: u32 = 8;
pub const MAX_TILE_WIDTH: u32 = 4096;
pub const MAX_TILE_AREA: u32 = 4096 * 2304;
pub const RESTORATION_TILESIZE_MAX: u16 = 256;
pub const WARPEDMODEL_PREC_BITS: u32 = 16;
pub const WARP_PARAM_REDUCE_BITS: u32 = 6;
pub const GM_ABS_ALPHA_BITS: u32 = 12;
pub const GM_ALPHA_PREC_BITS: u32 = 15;
pub const GM_ABS_TRANS_ONLY_BITS: u32 = 9;
pub const GM_TRANS_ONLY_PREC_BITS: u32 = 3;
pub const GM_ABS_TRANS_BITS: u32 = 12;
pub const GM_TRANS_PREC_BITS: u32 = 6;

pub const LAST_FRAME: usize = 1;
pub const LAST2_FRAME: usize = 2;
pub const LAST3_FRAME: usize = 3;
pub const GOLDEN_FRAME: usize = 4;
pub const BWDREF_FRAME: usize = 5;
pub const ALTREF2_FRAME: usize = 6;
pub const ALTREF_FRAME: usize = 7;

pub const IDENTITY: u8 = 0;
pub const TRANSLATION: u8 = 1;
pub const ROTZOOM: u8 = 2;
pub const AFFINE: u8 = 3;

pub const RESTORE_NONE: u8 = 0;
pub const RESTORE_WIENER: u8 = 1;
pub const RESTORE_SGRPROJ: u8 = 2;
pub const RESTORE_SWITCHABLE: u8 = 3;

pub const ONLY_4X4: u8 = 0;
pub const TX_MODE_LARGEST: u8 = 1;
pub const TX_MODE_SELECT: u8 = 2;

pub const INTERP_SWITCHABLE: u8 = 4;

const FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
const FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
const FEATURE_MAX: [i64; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];

const ALL_FRAMES: u32 = (1 << NUM_REF_FRAMES) - 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ObuType {
    #[default]
    Reserved,
    SequenceHeader,
    TemporalDelimiter,
    FrameHeader,
    TileGroup,
    Metadata,
    Frame,
    RedundantFrameHeader,
    TileList,
    Padding,
}

impl ObuType {
    fn from_bits(v: u32) -> Self {
        match v {
            1 => ObuType::SequenceHeader,
            2 => ObuType::TemporalDelimiter,
            3 => ObuType::FrameHeader,
            4 => ObuType::TileGroup,
            5 => ObuType::Metadata,
            6 => ObuType::Frame,
            7 => ObuType::RedundantFrameHeader,
            8 => ObuType::TileList,
            15 => ObuType::Padding,
            _ => ObuType::Reserved,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameType {
    #[default]
    KeyFrame = 0,
    InterFrame = 1,
    IntraOnlyFrame = 2,
    SwitchFrame = 3,
}

impl FrameType {
    fn from_bits(v: u32) -> Self {
        match v & 3 {
            0 => FrameType::KeyFrame,
            1 => FrameType::InterFrame,
            2 => FrameType::IntraOnlyFrame,
            _ => FrameType::SwitchFrame,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ObuHeader {
    pub obu_type: ObuType,
    pub extension_flag: bool,
    pub has_size_field: bool,
    pub temporal_id: u32,
    pub spatial_id: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Obu<'a> {
    pub header: ObuHeader,
    pub data: &'a [u8],
    pub bytes_used: usize,
}

pub enum ObuAction<'a> {
    Process(Obu<'a>),
    Drop(usize),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tile {
    pub tile_offset: u32,
    pub tile_size: u32,
    pub tile_row: u32,
    pub tile_col: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileGroupObu {
    pub tile_start_and_end_present_flag: bool,
    pub tg_start: u32,
    pub tg_end: u32,
    pub last: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OperatingPoint {
    pub seq_level_idx: u8,
    pub seq_tier: u8,
    pub idc: u16,
    pub decoder_model_present_for_this_op: bool,
    pub decoder_buffer_delay: u32,
    pub encoder_buffer_delay: u32,
    pub low_delay_mode_flag: bool,
    pub initial_display_delay_present_for_this_op: bool,
    pub initial_display_delay_minus_1: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimingInfo {
    pub num_units_in_display_tick: u32,
    pub time_scale: u32,
    pub equal_picture_interval: bool,
    pub num_ticks_per_picture_minus_1: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecoderModelInfo {
    pub buffer_delay_length_minus_1: u8,
    pub num_units_in_decoding_tick: u32,
    pub buffer_removal_time_length_minus_1: u8,
    pub frame_presentation_time_length_minus_1: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorConfig {
    pub high_bitdepth: bool,
    pub twelve_bit: bool,
    pub mono_chrome: bool,
    pub color_description_present_flag: bool,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
    pub chroma_sample_position: u8,
    pub separate_uv_delta_q: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SequenceHeaderObu {
    pub obu_header: ObuHeader,
    pub seq_profile: u8,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,
    pub frame_width_bits_minus_1: u8,
    pub frame_height_bits_minus_1: u8,
    pub max_frame_width_minus_1: u16,
    pub max_frame_height_minus_1: u16,
    pub frame_id_numbers_present_flag: bool,
    pub delta_frame_id_length_minus_2: u32,
    pub additional_frame_id_length_minus_1: u32,
    pub use_128x128_superblock: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_interintra_compound: bool,
    pub enable_masked_compound: bool,
    pub enable_warped_motion: bool,
    pub enable_order_hint: bool,
    pub enable_dual_filter: bool,
    pub enable_jnt_comp: bool,
    pub enable_ref_frame_mvs: bool,
    pub seq_choose_screen_content_tools: bool,
    pub seq_force_screen_content_tools: u32,
    pub seq_choose_integer_mv: bool,
    pub seq_force_integer_mv: u32,
    pub order_hint_bits_minus_1: i32,
    pub order_hint_bits: i32,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub film_grain_params_present: bool,
    pub operating_points_cnt_minus_1: u32,
    pub operating_points: [OperatingPoint; MAX_NUM_OPERATING_POINTS],
    pub decoder_model_info_present_flag: bool,
    pub decoder_model_info: DecoderModelInfo,
    pub initial_display_delay_present_flag: bool,
    pub timing_info_present_flag: bool,
    pub timing_info: TimingInfo,
    pub color_config: ColorConfig,
    pub bit_depth: u8,
    pub num_planes: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameHeaderObu {
    pub obu_header: ObuHeader,
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: u8,
    pub frame_presentation_time: u32,
    pub display_frame_id: u32,
    pub frame_type: FrameType,
    pub show_frame: bool,
    pub showable_frame: bool,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub allow_screen_content_tools: u32,
    pub force_integer_mv: u32,
    pub current_frame_id: u32,
    pub frame_size_override_flag: bool,
    pub order_hint: u32,
    pub primary_ref_frame: u32,
    pub buffer_removal_time_present_flag: bool,
    pub buffer_removal_time: [u32; MAX_NUM_OPERATING_POINTS],
    pub refresh_frame_flags: u32,
    pub ref_order_hint: [u32; NUM_REF_FRAMES],
    pub allow_intrabc: bool,
    pub frame_refs_short_signaling: bool,
    pub last_frame_idx: u8,
    pub gold_frame_idx: u8,
    pub ref_frame_idx: [u8; REFS_PER_FRAME],
    pub allow_high_precision_mv: bool,
    pub is_motion_mode_switchable: bool,
    pub use_ref_frame_mvs: bool,
    pub disable_frame_end_update_cdf: bool,
    pub allow_warped_motion: bool,
    pub reduced_tx_set: bool,
    pub render_and_frame_size_different: bool,
    pub use_superres: bool,
    pub is_filter_switchable: bool,
    pub interpolation_filter: u8,
    pub loop_filter_params: LoopFilterParams,
    pub quantization_params: QuantizationParams,
    pub segmentation_params: SegmentationParams,
    pub tile_info: TileInfo,
    pub cdef_params: CdefParams,
    pub loop_restoration_params: LoopRestorationParams,
    pub skip_mode_present: bool,
    pub reference_select: bool,
    pub global_motion_params: GlobalMotionParams,
    pub film_grain_params: FilmGrainParams,
    pub superres_denom: u32,
    pub frame_is_intra: bool,
    pub order_hints: [u32; NUM_REF_FRAMES],
    pub ref_frame_sign_bias: [bool; NUM_REF_FRAMES],
    pub coded_lossless: bool,
    pub all_lossless: bool,
    pub lossless_array: [bool; MAX_SEGMENTS],
    pub seg_qm_level: [[u32; MAX_SEGMENTS]; 3],
    pub upscaled_width: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub tx_mode: u8,
    pub skip_mode_frame: [u32; 2],
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub header_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopFilterParams {
    pub loop_filter_level: [u8; 4],
    pub loop_filter_sharpness: u8,
    pub loop_filter_delta_enabled: bool,
    pub loop_filter_delta_update: bool,
    pub loop_filter_ref_deltas: [i8; TOTAL_REFS_PER_FRAME],
    pub loop_filter_mode_deltas: [i8; 2],
    pub delta_lf_present: bool,
    pub delta_lf_res: u8,
    pub delta_lf_multi: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuantizationParams {
    pub base_q_idx: u32,
    pub diff_uv_delta: bool,
    pub using_qmatrix: bool,
    pub qm_y: u32,
    pub qm_u: u32,
    pub qm_v: u32,
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentationParams {
    pub segmentation_enabled: bool,
    pub segmentation_update_map: bool,
    pub segmentation_temporal_update: bool,
    pub segmentation_update_data: bool,
    pub feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub feature_data: [[i16; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub seg_id_pre_skip: bool,
    pub last_active_seg_id: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileInfo {
    pub uniform_tile_spacing_flag: bool,
    pub width_in_sbs_minus_1: [u32; MAX_TILE_COLS],
    pub height_in_sbs_minus_1: [u32; MAX_TILE_ROWS],
    pub context_update_tile_id: u32,
    pub mi_col_starts: [u32; MAX_TILE_COLS + 1],
    pub mi_row_starts: [u32; MAX_TILE_ROWS + 1],
    pub tile_cols_log2: u32,
    pub tile_cols: u32,
    pub tile_rows_log2: u32,
    pub tile_rows: u32,
    pub tile_size_bytes: u32,
}

impl Default for TileInfo {
    fn default() -> Self {
        Self {
            uniform_tile_spacing_flag: false,
            width_in_sbs_minus_1: [0; MAX_TILE_COLS],
            height_in_sbs_minus_1: [0; MAX_TILE_ROWS],
            context_update_tile_id: 0,
            mi_col_starts: [0; MAX_TILE_COLS + 1],
            mi_row_starts: [0; MAX_TILE_ROWS + 1],
            tile_cols_log2: 0,
            tile_cols: 0,
            tile_rows_log2: 0,
            tile_rows: 0,
            tile_size_bytes: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CdefParams {
    pub cdef_damping: u32,
    pub cdef_bits: u32,
    pub cdef_y_pri_strength: [u32; CDEF_MAX],
    pub cdef_y_sec_strength: [u32; CDEF_MAX],
    pub cdef_uv_pri_strength: [u32; CDEF_MAX],
    pub cdef_uv_sec_strength: [u32; CDEF_MAX],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopRestorationParams {
    pub lr_unit_shift: u8,
    pub lr_uv_shift: u8,
    pub frame_restoration_type: [u8; MAX_NUM_PLANES],
    pub loop_restoration_size: [u16; MAX_NUM_PLANES],
    pub uses_lr: bool,
    pub uses_chroma_lr: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalMotionParams {
    pub is_global: [bool; NUM_REF_FRAMES],
    pub is_rot_zoom: [bool; NUM_REF_FRAMES],
    pub is_translation: [bool; NUM_REF_FRAMES],
    pub gm_params: [[i32; 6]; NUM_REF_FRAMES],
    pub warp_valid: [bool; NUM_REF_FRAMES],
    pub gm_type: [u8; NUM_REF_FRAMES],
}

const DEFAULT_WARP: [i32; 6] = [
    0,
    0,
    1 << WARPEDMODEL_PREC_BITS,
    0,
    0,
    1 << WARPEDMODEL_PREC_BITS,
];

impl Default for GlobalMotionParams {
    fn default() -> Self {
        Self {
            is_global: [false; NUM_REF_FRAMES],
            is_rot_zoom: [false; NUM_REF_FRAMES],
            is_translation: [false; NUM_REF_FRAMES],
            gm_params: [DEFAULT_WARP; NUM_REF_FRAMES],
            warp_valid: [true; NUM_REF_FRAMES],
            gm_type: [IDENTITY; NUM_REF_FRAMES],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilmGrainParams {
    pub apply_grain: bool,
    pub grain_seed: u16,
    pub update_grain: bool,
    pub film_grain_params_ref_idx: u8,
    pub num_y_points: u8,
    pub point_y_value: [u8; MAX_NUM_Y_POINTS],
    pub point_y_scaling: [u8; MAX_NUM_Y_POINTS],
    pub chroma_scaling_from_luma: bool,
    pub num_cb_points: u8,
    pub point_cb_value: [u8; MAX_NUM_CB_POINTS],
    pub point_cb_scaling: [u8; MAX_NUM_CB_POINTS],
    pub num_cr_points: u8,
    pub point_cr_value: [u8; MAX_NUM_CR_POINTS],
    pub point_cr_scaling: [u8; MAX_NUM_CR_POINTS],
    pub grain_scaling_minus_8: u8,
    pub ar_coeff_lag: u32,
    pub ar_coeffs_y_plus_128: [u8; MAX_NUM_POS_LUMA],
    pub ar_coeffs_cb_plus_128: [u8; MAX_NUM_POS_LUMA],
    pub ar_coeffs_cr_plus_128: [u8; MAX_NUM_POS_LUMA],
    pub ar_coeff_shift_minus_6: u8,
    pub grain_scale_shift: u8,
    pub cb_mult: u8,
    pub cb_luma_mult: u8,
    pub cb_offset: u16,
    pub cr_mult: u8,
    pub cr_luma_mult: u8,
    pub cr_offset: u16,
    pub overlap_flag: bool,
    pub clip_to_restricted_range: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReferenceFrameInfo {
    ref_valid: bool,
    ref_frame_id: u32,
    ref_upscaled_width: u32,
    ref_frame_width: u32,
    ref_frame_height: u32,
    ref_render_width: u32,
    ref_render_height: u32,
    ref_mi_cols: u32,
    ref_mi_rows: u32,
    ref_frame_type: FrameType,
    ref_order_hint: u32,
    segmentation_params: SegmentationParams,
    global_motion_params: GlobalMotionParams,
    loop_filter_params: LoopFilterParams,
    film_grain_params: FilmGrainParams,
    display_frame_id: u32,
    showable_frame: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Parser {
    operating_point_idc: u16,
    seen_frame_header: bool,
    is_first_frame: bool,
    ref_info: [ReferenceFrameInfo; NUM_REF_FRAMES],
    mi_cols: u32,
    mi_rows: u32,
    prev_frame_id: u32,
    current_frame_id: u32,
    tile: TileInfo,
    pub last_frame_header: Option<FrameHeaderObu>,
    pub sequence_header: Option<SequenceHeaderObu>,
}

impl Parser {
    pub fn sequence(&self) -> Result<&SequenceHeaderObu> {
        self.sequence_header
            .as_ref()
            .ok_or_else(|| "no sequence header parsed yet".to_string())
    }

    pub fn num_tiles(&self) -> u32 {
        self.tile.tile_cols * self.tile.tile_rows
    }

    fn compute_image_size(&mut self, fh: &mut FrameHeaderObu) {
        fh.mi_cols = 2 * ((fh.frame_width + 7) >> 3);
        fh.mi_rows = 2 * ((fh.frame_height + 7) >> 3);
        self.mi_cols = fh.mi_cols;
        self.mi_rows = fh.mi_rows;
    }

    fn parse_superres_params(
        fh: &mut FrameHeaderObu,
        r: &mut Reader,
        enable_superres: bool,
    ) -> Result<()> {
        fh.use_superres = enable_superres && r.read_bit()?;
        fh.superres_denom = if fh.use_superres {
            r.read_bits(SUPERRES_DENOM_BITS)? + SUPERRES_DENOM_MIN
        } else {
            SUPERRES_NUM
        };
        fh.upscaled_width = fh.frame_width;
        fh.frame_width =
            (fh.upscaled_width * SUPERRES_NUM + (fh.superres_denom / 2)) / fh.superres_denom;
        Ok(())
    }

    fn set_frame_refs(&self, fh: &mut FrameHeaderObu) -> Result<()> {
        let seq = self.sequence()?;
        let mut ref_frame_idx = [-1i32; REFS_PER_FRAME];
        ref_frame_idx[0] = fh.last_frame_idx.into();
        ref_frame_idx[GOLDEN_FRAME - LAST_FRAME] = fh.gold_frame_idx.into();
        let mut used_frame = [false; NUM_REF_FRAMES];
        used_frame[fh.last_frame_idx as usize & 7] = true;
        used_frame[fh.gold_frame_idx as usize & 7] = true;
        let cur_frame_hint = 1i32 << (seq.order_hint_bits - 1).clamp(0, 30);
        let mut shifted_order_hints = [0; NUM_REF_FRAMES];
        for i in 0..NUM_REF_FRAMES {
            shifted_order_hints[i] = cur_frame_hint
                + helpers::get_relative_dist(
                    seq.enable_order_hint,
                    seq.order_hint_bits,
                    self.ref_info[i].ref_order_hint as i32,
                    fh.order_hint as i32,
                );
        }
        let mut latest_order_hint = shifted_order_hints[fh.last_frame_idx as usize & 7];
        if latest_order_hint >= cur_frame_hint {
            return Err("last_order_hint is not before the current frame".into());
        }
        let mut earliest_order_hint = shifted_order_hints[fh.gold_frame_idx as usize & 7];
        if earliest_order_hint >= cur_frame_hint {
            return Err("gold_order_hint is not before the current frame".into());
        }
        let found = helpers::find_latest_backward(
            &shifted_order_hints,
            &used_frame,
            cur_frame_hint,
            &mut latest_order_hint,
        );
        if found >= 0 {
            ref_frame_idx[ALTREF_FRAME - LAST_FRAME] = found;
            used_frame[found as usize] = true;
        }
        let found = helpers::find_earliest_backward(
            &shifted_order_hints,
            &used_frame,
            cur_frame_hint,
            &mut earliest_order_hint,
        );
        if found >= 0 {
            ref_frame_idx[BWDREF_FRAME - LAST_FRAME] = found;
            used_frame[found as usize] = true;
        }
        let found = helpers::find_earliest_backward(
            &shifted_order_hints,
            &used_frame,
            cur_frame_hint,
            &mut earliest_order_hint,
        );
        if found >= 0 {
            ref_frame_idx[ALTREF2_FRAME - LAST_FRAME] = found;
            used_frame[found as usize] = true;
        }
        const REF_FRAME_LIST: [usize; 5] = [
            LAST2_FRAME - LAST_FRAME,
            LAST3_FRAME - LAST_FRAME,
            BWDREF_FRAME - LAST_FRAME,
            ALTREF2_FRAME - LAST_FRAME,
            ALTREF_FRAME - LAST_FRAME,
        ];
        for ref_frame in REF_FRAME_LIST {
            if ref_frame_idx[ref_frame] < 0 {
                let found = helpers::find_latest_forward(
                    &shifted_order_hints,
                    &used_frame,
                    cur_frame_hint,
                    &mut latest_order_hint,
                );
                if found >= 0 {
                    ref_frame_idx[ref_frame] = found;
                    used_frame[found as usize] = true;
                }
            }
        }
        let mut earliest = 0u8;
        earliest_order_hint = shifted_order_hints[0];
        for i in 1..NUM_REF_FRAMES {
            let hint = shifted_order_hints[i];
            if hint < earliest_order_hint {
                earliest = i as u8;
                earliest_order_hint = hint;
            }
        }
        for (dest, src) in fh.ref_frame_idx.iter_mut().zip(ref_frame_idx) {
            *dest = if src < 0 { earliest } else { src as u8 };
        }
        Ok(())
    }

    fn parse_frame_size(&mut self, fh: &mut FrameHeaderObu, r: &mut Reader) -> Result<()> {
        let seq = self.sequence()?;
        if fh.frame_size_override_flag {
            fh.frame_width = r.read_bits(u32::from(seq.frame_width_bits_minus_1) + 1)? + 1;
            fh.frame_height = r.read_bits(u32::from(seq.frame_height_bits_minus_1) + 1)? + 1;
        } else {
            fh.frame_width = u32::from(seq.max_frame_width_minus_1) + 1;
            fh.frame_height = u32::from(seq.max_frame_height_minus_1) + 1;
        }
        let enable_superres = seq.enable_superres;
        Self::parse_superres_params(fh, r, enable_superres)?;
        self.compute_image_size(fh);
        Ok(())
    }

    fn parse_render_size(fh: &mut FrameHeaderObu, r: &mut Reader) -> Result<()> {
        fh.render_and_frame_size_different = r.read_bit()?;
        if fh.render_and_frame_size_different {
            fh.render_width = r.read_bits(16)? + 1;
            fh.render_height = r.read_bits(16)? + 1;
        } else {
            fh.render_width = fh.upscaled_width;
            fh.render_height = fh.frame_height;
        }
        Ok(())
    }

    fn frame_size_with_refs(&mut self, fh: &mut FrameHeaderObu, r: &mut Reader) -> Result<()> {
        let mut found_ref = false;
        for i in 0..REFS_PER_FRAME {
            found_ref = r.read_bit()?;
            if found_ref {
                let rf = &self.ref_info[fh.ref_frame_idx[i] as usize & 7];
                fh.upscaled_width = rf.ref_upscaled_width;
                fh.frame_width = fh.upscaled_width;
                fh.frame_height = rf.ref_frame_height;
                fh.render_width = rf.ref_render_width;
                fh.render_height = rf.ref_render_height;
                break;
            }
        }
        if !found_ref {
            self.parse_frame_size(fh, r)?;
            Self::parse_render_size(fh, r)?;
        } else {
            let enable_superres = self.sequence()?.enable_superres;
            Self::parse_superres_params(fh, r, enable_superres)?;
            self.compute_image_size(fh);
        }
        Ok(())
    }

    fn skip_and_check_trailing_bits(r: &mut Reader, obu: &Obu) -> Result<()> {
        if obu.data.is_empty()
            || matches!(
                obu.header.obu_type,
                ObuType::TileList | ObuType::TileGroup | ObuType::Frame
            )
        {
            return Ok(());
        }
        let num_trailing = (obu.data.len() * 8)
            .checked_sub(r.position())
            .ok_or_else(|| "OBU overrun".to_string())?;
        r.read_trailing_bits(num_trailing)
    }

    fn parse_obu_header(r: &mut Reader) -> Result<ObuHeader> {
        if r.read_bit()? {
            return Err("obu_forbidden_bit is set".into());
        }
        let mut header = ObuHeader {
            obu_type: ObuType::from_bits(r.read_bits(4)?),
            extension_flag: r.read_bit()?,
            has_size_field: r.read_bit()?,
            temporal_id: 0,
            spatial_id: 0,
        };
        r.read_bit()?;
        if header.extension_flag {
            header.temporal_id = r.read_bits(3)?;
            header.spatial_id = r.read_bits(2)?;
            r.read_bits(3)?;
        }
        Ok(header)
    }

    pub fn read_obu<'a>(&self, data: &'a [u8]) -> Result<ObuAction<'a>> {
        if data.is_empty() {
            return Err("empty OBU".into());
        }
        let mut reader = Reader::new(data);
        let header = Self::parse_obu_header(&mut reader)?;
        let start = reader.position() / 8;
        let obu_size = if header.has_size_field {
            reader.read_leb128()? as usize
        } else {
            data.len() - start
        };
        let start = reader.position() / 8;
        let end = start
            .checked_add(obu_size)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| format!("OBU of {obu_size} bytes past the end of the data"))?;
        if header.obu_type != ObuType::SequenceHeader
            && header.obu_type != ObuType::TemporalDelimiter
            && self.operating_point_idc != 0
            && header.extension_flag
        {
            let in_temporal_layer = (self.operating_point_idc >> header.temporal_id) & 1 != 0;
            let in_spatial_layer = (self.operating_point_idc >> (header.spatial_id + 8)) & 1 != 0;
            if !in_temporal_layer || !in_spatial_layer {
                return Ok(ObuAction::Drop(end));
            }
        }
        Ok(ObuAction::Process(Obu {
            header,
            data: &data[start..end],
            bytes_used: end,
        }))
    }

    fn parse_color_config(s: &mut SequenceHeaderObu, r: &mut Reader) -> Result<()> {
        let cc = &mut s.color_config;
        cc.high_bitdepth = r.read_bit()?;
        if s.seq_profile == 2 && cc.high_bitdepth {
            cc.twelve_bit = r.read_bit()?;
            s.bit_depth = if cc.twelve_bit { 12 } else { 10 };
        } else {
            s.bit_depth = if cc.high_bitdepth { 10 } else { 8 };
        }
        cc.mono_chrome = if s.seq_profile == 1 {
            false
        } else {
            r.read_bit()?
        };
        s.num_planes = if cc.mono_chrome { 1 } else { 3 };
        cc.color_description_present_flag = r.read_bit()?;
        if cc.color_description_present_flag {
            cc.color_primaries = r.read_bits(8)? as u8;
            cc.transfer_characteristics = r.read_bits(8)? as u8;
            cc.matrix_coefficients = r.read_bits(8)? as u8;
        } else {
            cc.color_primaries = 2;
            cc.transfer_characteristics = 2;
            cc.matrix_coefficients = 2;
        }
        if cc.mono_chrome {
            cc.color_range = r.read_bit()?;
            cc.subsampling_x = true;
            cc.subsampling_y = true;
            cc.chroma_sample_position = 0;
            cc.separate_uv_delta_q = false;
            return Ok(());
        } else if cc.color_primaries == 1
            && cc.transfer_characteristics == 13
            && cc.matrix_coefficients == 0
        {
            cc.color_range = true;
            cc.subsampling_x = false;
            cc.subsampling_y = false;
        } else {
            cc.color_range = r.read_bit()?;
            if s.seq_profile == 0 {
                cc.subsampling_x = true;
                cc.subsampling_y = true;
            } else if s.seq_profile == 1 {
                cc.subsampling_x = false;
                cc.subsampling_y = false;
            } else if s.bit_depth == 12 {
                cc.subsampling_x = r.read_bit()?;
                cc.subsampling_y = cc.subsampling_x && r.read_bit()?;
            } else {
                cc.subsampling_x = true;
                cc.subsampling_y = false;
            }
            if cc.subsampling_x && cc.subsampling_y {
                cc.chroma_sample_position = r.read_bits(2)? as u8;
            }
        }
        cc.separate_uv_delta_q = r.read_bit()?;
        Ok(())
    }

    fn parse_timing_info(ti: &mut TimingInfo, r: &mut Reader) -> Result<()> {
        ti.num_units_in_display_tick = r.read_bits(32)?;
        ti.time_scale = r.read_bits(32)?;
        ti.equal_picture_interval = r.read_bit()?;
        if ti.equal_picture_interval {
            ti.num_ticks_per_picture_minus_1 = r.read_uvlc()?;
        }
        Ok(())
    }

    fn parse_decoder_model_info(dmi: &mut DecoderModelInfo, r: &mut Reader) -> Result<()> {
        dmi.buffer_delay_length_minus_1 = r.read_bits(5)? as u8;
        dmi.num_units_in_decoding_tick = r.read_bits(32)?;
        dmi.buffer_removal_time_length_minus_1 = r.read_bits(5)? as u8;
        dmi.frame_presentation_time_length_minus_1 = r.read_bits(5)?;
        Ok(())
    }

    pub fn parse_temporal_delimiter_obu(&mut self) {
        self.seen_frame_header = false;
    }

    pub fn parse_sequence_header_obu(&mut self, obu: &Obu) -> Result<&SequenceHeaderObu> {
        let mut s = SequenceHeaderObu {
            obu_header: obu.header,
            ..Default::default()
        };
        let mut r = Reader::new(obu.data);
        s.seq_profile = r.read_bits(3)? as u8;
        if s.seq_profile > 2 {
            return Err(format!("AV1 seq_profile {}", s.seq_profile));
        }
        s.still_picture = r.read_bit()?;
        s.reduced_still_picture_header = r.read_bit()?;
        if s.reduced_still_picture_header {
            s.operating_points[0].seq_level_idx = r.read_bits(5)? as u8;
        } else {
            s.timing_info_present_flag = r.read_bit()?;
            if s.timing_info_present_flag {
                Self::parse_timing_info(&mut s.timing_info, &mut r)?;
                s.decoder_model_info_present_flag = r.read_bit()?;
                if s.decoder_model_info_present_flag {
                    Self::parse_decoder_model_info(&mut s.decoder_model_info, &mut r)?;
                }
            }
            s.initial_display_delay_present_flag = r.read_bit()?;
            s.operating_points_cnt_minus_1 = r.read_bits(5)?;
            for i in 0..=s.operating_points_cnt_minus_1 as usize {
                let n = u32::from(s.decoder_model_info.buffer_delay_length_minus_1) + 1;
                let op = &mut s.operating_points[i];
                op.idc = r.read_bits(12)? as u16;
                op.seq_level_idx = r.read_bits(5)? as u8;
                op.seq_tier = if op.seq_level_idx > 7 {
                    r.read_bit()? as u8
                } else {
                    0
                };
                if s.decoder_model_info_present_flag {
                    op.decoder_model_present_for_this_op = r.read_bit()?;
                    if op.decoder_model_present_for_this_op {
                        op.decoder_buffer_delay = r.read_bits(n)?;
                        op.encoder_buffer_delay = r.read_bits(n)?;
                        op.low_delay_mode_flag = r.read_bit()?;
                    }
                }
                if s.initial_display_delay_present_flag {
                    op.initial_display_delay_present_for_this_op = r.read_bit()?;
                    if op.initial_display_delay_present_for_this_op {
                        op.initial_display_delay_minus_1 = r.read_bits(4)?;
                    }
                }
            }
        }
        s.frame_width_bits_minus_1 = r.read_bits(4)? as u8;
        s.frame_height_bits_minus_1 = r.read_bits(4)? as u8;
        s.max_frame_width_minus_1 = r.read_bits(u32::from(s.frame_width_bits_minus_1) + 1)? as u16;
        s.max_frame_height_minus_1 =
            r.read_bits(u32::from(s.frame_height_bits_minus_1) + 1)? as u16;
        s.frame_id_numbers_present_flag = !s.reduced_still_picture_header && r.read_bit()?;
        if s.frame_id_numbers_present_flag {
            s.delta_frame_id_length_minus_2 = r.read_bits(4)?;
            s.additional_frame_id_length_minus_1 = r.read_bits(3)?;
            let frame_id_length =
                s.additional_frame_id_length_minus_1 + s.delta_frame_id_length_minus_2 + 3;
            if frame_id_length > 16 {
                return Err(format!("frame_id_length {frame_id_length}"));
            }
        }
        s.use_128x128_superblock = r.read_bit()?;
        s.enable_filter_intra = r.read_bit()?;
        s.enable_intra_edge_filter = r.read_bit()?;
        if s.reduced_still_picture_header {
            s.seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS;
            s.seq_force_integer_mv = SELECT_INTEGER_MV;
            s.order_hint_bits = 0;
            s.order_hint_bits_minus_1 = -1;
        } else {
            s.enable_interintra_compound = r.read_bit()?;
            s.enable_masked_compound = r.read_bit()?;
            s.enable_warped_motion = r.read_bit()?;
            s.enable_dual_filter = r.read_bit()?;
            s.enable_order_hint = r.read_bit()?;
            if s.enable_order_hint {
                s.enable_jnt_comp = r.read_bit()?;
                s.enable_ref_frame_mvs = r.read_bit()?;
            }
            s.seq_choose_screen_content_tools = r.read_bit()?;
            s.seq_force_screen_content_tools = if s.seq_choose_screen_content_tools {
                SELECT_SCREEN_CONTENT_TOOLS
            } else {
                r.read_bit()? as u32
            };
            if s.seq_force_screen_content_tools > 0 {
                s.seq_choose_integer_mv = r.read_bit()?;
                s.seq_force_integer_mv = if s.seq_choose_integer_mv {
                    SELECT_INTEGER_MV
                } else {
                    r.read_bit()? as u32
                };
            } else {
                s.seq_force_integer_mv = SELECT_INTEGER_MV;
            }
            if s.enable_order_hint {
                s.order_hint_bits_minus_1 = r.read_bits(3)? as i32;
                s.order_hint_bits = s.order_hint_bits_minus_1 + 1;
            } else {
                s.order_hint_bits_minus_1 = -1;
                s.order_hint_bits = 0;
            }
        }
        s.enable_superres = r.read_bit()?;
        s.enable_cdef = r.read_bit()?;
        s.enable_restoration = r.read_bit()?;
        Self::parse_color_config(&mut s, &mut r)?;
        s.film_grain_params_present = r.read_bit()?;
        Self::skip_and_check_trailing_bits(&mut r, obu)?;
        self.operating_point_idc = s.operating_points[0].idc;
        Ok(self.sequence_header.insert(s))
    }

    fn load_reference_frame(&self, fh: &mut FrameHeaderObu) -> Result<()> {
        let rf = &self.ref_info[fh.frame_to_show_map_idx as usize & 7];
        let film_grain_params_present = self.sequence()?.film_grain_params_present;
        fh.frame_type = rf.ref_frame_type;
        fh.upscaled_width = rf.ref_upscaled_width;
        fh.frame_width = rf.ref_frame_width;
        fh.frame_height = rf.ref_frame_height;
        fh.render_width = rf.ref_render_width;
        fh.render_height = rf.ref_render_height;
        fh.order_hint = rf.ref_order_hint;
        fh.showable_frame = rf.showable_frame;
        if fh.frame_type == FrameType::KeyFrame {
            fh.current_frame_id = rf.ref_frame_id;
            fh.mi_cols = rf.ref_mi_cols;
            fh.mi_rows = rf.ref_mi_rows;
            fh.global_motion_params = rf.global_motion_params;
            if film_grain_params_present {
                fh.film_grain_params = rf.film_grain_params;
            }
            fh.loop_filter_params = rf.loop_filter_params;
            fh.segmentation_params = rf.segmentation_params;
        }
        Ok(())
    }

    fn setup_past_independence(fh: &mut FrameHeaderObu) {
        fh.segmentation_params.feature_enabled = Default::default();
        fh.segmentation_params.feature_data = Default::default();
        for i in LAST_FRAME..=ALTREF_FRAME {
            fh.global_motion_params.gm_type[i] = IDENTITY;
        }
        fh.loop_filter_params.loop_filter_delta_enabled = true;
        fh.loop_filter_params.loop_filter_ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
        fh.loop_filter_params.loop_filter_mode_deltas = Default::default();
    }

    fn parse_tile_info(&mut self, r: &mut Reader, ti: &mut TileInfo) -> Result<()> {
        let seq = self.sequence()?;
        let sb_shift = if seq.use_128x128_superblock { 5 } else { 4 };
        let sb_cols = (self.mi_cols + (1 << sb_shift) - 1) >> sb_shift;
        let sb_rows = (self.mi_rows + (1 << sb_shift) - 1) >> sb_shift;
        let sb_size = sb_shift + 2;
        let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
        let mut max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
        let min_log2_tile_cols = helpers::tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = helpers::tile_log2(1, sb_cols.min(MAX_TILE_COLS as u32));
        let max_log2_tile_rows = helpers::tile_log2(1, sb_rows.min(MAX_TILE_ROWS as u32));
        let min_log2_tiles = min_log2_tile_cols.max(helpers::tile_log2(
            max_tile_area_sb,
            sb_rows.saturating_mul(sb_cols),
        ));
        let t = &mut self.tile;
        ti.uniform_tile_spacing_flag = r.read_bit()?;
        if ti.uniform_tile_spacing_flag {
            t.tile_cols_log2 = min_log2_tile_cols;
            while t.tile_cols_log2 < max_log2_tile_cols && r.read_bit()? {
                t.tile_cols_log2 += 1;
            }
            let tile_width_sb = (sb_cols + (1 << t.tile_cols_log2) - 1) >> t.tile_cols_log2;
            let mut i = 0;
            let mut start_sb = 0;
            while start_sb < sb_cols {
                if i >= MAX_TILE_COLS {
                    return Err("too many tile columns".into());
                }
                t.mi_col_starts[i] = start_sb << sb_shift;
                i += 1;
                start_sb += tile_width_sb.max(1);
            }
            t.mi_col_starts[i] = self.mi_cols;
            t.tile_cols = i as u32;
            for j in 0..i {
                ti.width_in_sbs_minus_1[j] =
                    ((t.mi_col_starts[j + 1] - t.mi_col_starts[j] + (1 << sb_shift) - 1)
                        >> sb_shift)
                        .saturating_sub(1);
            }
            t.tile_rows_log2 = min_log2_tiles.saturating_sub(t.tile_cols_log2);
            while t.tile_rows_log2 < max_log2_tile_rows && r.read_bit()? {
                t.tile_rows_log2 += 1;
            }
            let tile_height_sb = (sb_rows + (1 << t.tile_rows_log2) - 1) >> t.tile_rows_log2;
            let mut i = 0;
            let mut start_sb = 0;
            while start_sb < sb_rows {
                if i >= MAX_TILE_ROWS {
                    return Err("too many tile rows".into());
                }
                t.mi_row_starts[i] = start_sb << sb_shift;
                i += 1;
                start_sb += tile_height_sb.max(1);
            }
            t.mi_row_starts[i] = self.mi_rows;
            t.tile_rows = i as u32;
            for j in 0..i {
                ti.height_in_sbs_minus_1[j] =
                    ((t.mi_row_starts[j + 1] - t.mi_row_starts[j] + (1 << sb_shift) - 1)
                        >> sb_shift)
                        .saturating_sub(1);
            }
        } else {
            let mut widest_tile_sb = 0;
            let mut start_sb = 0;
            let mut i = 0;
            while start_sb < sb_cols {
                if i >= MAX_TILE_COLS {
                    return Err("too many tile columns".into());
                }
                t.mi_col_starts[i] = start_sb << sb_shift;
                let max_width = (sb_cols - start_sb).min(max_tile_width_sb);
                ti.width_in_sbs_minus_1[i] = r.read_ns(max_width)?;
                let size_sb = ti.width_in_sbs_minus_1[i] + 1;
                widest_tile_sb = widest_tile_sb.max(size_sb);
                start_sb += size_sb;
                i += 1;
            }
            t.mi_col_starts[i] = self.mi_cols;
            t.tile_cols = i as u32;
            t.tile_cols_log2 = helpers::tile_log2(1, t.tile_cols);
            max_tile_area_sb = if min_log2_tiles > 0 {
                (sb_rows * sb_cols) >> (min_log2_tiles + 1)
            } else {
                sb_rows * sb_cols
            };
            let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb.max(1)).max(1);
            let mut start_sb = 0;
            let mut i = 0;
            while start_sb < sb_rows {
                if i >= MAX_TILE_ROWS {
                    return Err("too many tile rows".into());
                }
                t.mi_row_starts[i] = start_sb << sb_shift;
                let max_height = (sb_rows - start_sb).min(max_tile_height_sb);
                ti.height_in_sbs_minus_1[i] = r.read_ns(max_height)?;
                start_sb += ti.height_in_sbs_minus_1[i] + 1;
                i += 1;
            }
            t.mi_row_starts[i] = self.mi_rows;
            t.tile_rows = i as u32;
            t.tile_rows_log2 = helpers::tile_log2(1, t.tile_rows);
        }
        if t.tile_cols_log2 > 0 || t.tile_rows_log2 > 0 {
            ti.context_update_tile_id = r.read_bits(t.tile_rows_log2 + t.tile_cols_log2)?;
            if ti.context_update_tile_id >= t.tile_rows * t.tile_cols {
                return Err(format!(
                    "context_update_tile_id {}",
                    ti.context_update_tile_id
                ));
            }
            t.tile_size_bytes = r.read_bits(2)? + 1;
        } else {
            ti.context_update_tile_id = 0;
        }
        ti.mi_col_starts = t.mi_col_starts;
        ti.mi_row_starts = t.mi_row_starts;
        ti.tile_cols_log2 = t.tile_cols_log2;
        ti.tile_cols = t.tile_cols;
        ti.tile_rows_log2 = t.tile_rows_log2;
        ti.tile_rows = t.tile_rows;
        ti.tile_size_bytes = t.tile_size_bytes;
        Ok(())
    }

    fn parse_quantization_params(
        r: &mut Reader,
        q: &mut QuantizationParams,
        num_planes: u32,
        separate_uv_delta_q: bool,
    ) -> Result<()> {
        q.base_q_idx = r.read_bits(8)?;
        q.delta_q_y_dc = r.read_delta_q()?;
        if num_planes > 1 {
            q.diff_uv_delta = separate_uv_delta_q && r.read_bit()?;
            q.delta_q_u_dc = r.read_delta_q()?;
            q.delta_q_u_ac = r.read_delta_q()?;
            if q.diff_uv_delta {
                q.delta_q_v_dc = r.read_delta_q()?;
                q.delta_q_v_ac = r.read_delta_q()?;
            } else {
                q.delta_q_v_dc = q.delta_q_u_dc;
                q.delta_q_v_ac = q.delta_q_u_ac;
            }
        } else {
            q.diff_uv_delta = false;
            q.delta_q_u_dc = 0;
            q.delta_q_u_ac = 0;
            q.delta_q_v_dc = 0;
            q.delta_q_v_ac = 0;
        }
        q.using_qmatrix = r.read_bit()?;
        if q.using_qmatrix {
            q.qm_y = r.read_bits(4)?;
            q.qm_u = r.read_bits(4)?;
            q.qm_v = if separate_uv_delta_q {
                r.read_bits(4)?
            } else {
                q.qm_u
            };
        }
        Ok(())
    }

    fn parse_delta_q_params(r: &mut Reader, q: &mut QuantizationParams) -> Result<()> {
        q.delta_q_res = 0;
        q.delta_q_present = q.base_q_idx > 0 && r.read_bit()?;
        if q.delta_q_present {
            q.delta_q_res = r.read_bits(2)?;
        }
        Ok(())
    }

    fn parse_delta_lf_params(
        r: &mut Reader,
        lf: &mut LoopFilterParams,
        delta_q_present: bool,
        allow_intrabc: bool,
    ) -> Result<()> {
        lf.delta_lf_present = false;
        lf.delta_lf_res = 0;
        lf.delta_lf_multi = false;
        if delta_q_present {
            lf.delta_lf_present = !allow_intrabc && r.read_bit()?;
            if lf.delta_lf_present {
                lf.delta_lf_res = r.read_bits(2)? as u8;
                lf.delta_lf_multi = r.read_bit()?;
            }
        }
        Ok(())
    }

    fn parse_segmentation_params(&self, r: &mut Reader, fh: &mut FrameHeaderObu) -> Result<()> {
        let s = &mut fh.segmentation_params;
        s.segmentation_enabled = r.read_bit()?;
        s.segmentation_update_map = false;
        s.segmentation_temporal_update = false;
        s.segmentation_update_data = false;
        if s.segmentation_enabled {
            if fh.primary_ref_frame == PRIMARY_REF_NONE {
                s.segmentation_update_map = true;
                s.segmentation_temporal_update = false;
                s.segmentation_update_data = true;
            } else {
                s.segmentation_update_map = r.read_bit()?;
                if s.segmentation_update_map {
                    s.segmentation_temporal_update = r.read_bit()?;
                }
                s.segmentation_update_data = r.read_bit()?;
            }
            if s.segmentation_update_data {
                for i in 0..MAX_SEGMENTS {
                    for j in 0..SEG_LVL_MAX {
                        let feature_enabled = r.read_bit()?;
                        s.feature_enabled[i][j] = feature_enabled;
                        let mut clipped = 0;
                        if feature_enabled {
                            let bits = FEATURE_BITS[j];
                            let limit = FEATURE_MAX[j];
                            clipped = if FEATURE_SIGNED[j] {
                                helpers::clip3(-limit, limit, i64::from(r.read_su(1 + bits)?))
                            } else {
                                helpers::clip3(0, limit, i64::from(r.read_bits(bits)?))
                            };
                        }
                        s.feature_data[i][j] = clipped as i16;
                    }
                }
            } else {
                let prev_frame = &self.ref_info
                    [fh.ref_frame_idx[fh.primary_ref_frame as usize % REFS_PER_FRAME] as usize & 7];
                s.feature_enabled = prev_frame.segmentation_params.feature_enabled;
                s.feature_data = prev_frame.segmentation_params.feature_data;
            }
        } else {
            s.feature_enabled = Default::default();
            s.feature_data = Default::default();
        }
        s.seg_id_pre_skip = false;
        s.last_active_seg_id = 0;
        for i in 0..MAX_SEGMENTS {
            for j in 0..SEG_LVL_MAX {
                if s.feature_enabled[i][j] {
                    s.last_active_seg_id = i as u8;
                    if j >= SEG_LVL_REF_FRAME {
                        s.seg_id_pre_skip = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_loop_filter_parameters(
        r: &mut Reader,
        fh: &mut FrameHeaderObu,
        num_planes: u32,
    ) -> Result<()> {
        let lf = &mut fh.loop_filter_params;
        if fh.coded_lossless || fh.allow_intrabc {
            lf.loop_filter_level[0] = 0;
            lf.loop_filter_level[1] = 0;
            lf.loop_filter_ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
            lf.loop_filter_mode_deltas = Default::default();
            return Ok(());
        }
        lf.loop_filter_level[0] = r.read_bits(6)? as u8;
        lf.loop_filter_level[1] = r.read_bits(6)? as u8;
        if num_planes > 1 && (lf.loop_filter_level[0] > 0 || lf.loop_filter_level[1] > 0) {
            lf.loop_filter_level[2] = r.read_bits(6)? as u8;
            lf.loop_filter_level[3] = r.read_bits(6)? as u8;
        }
        lf.loop_filter_sharpness = r.read_bits(3)? as u8;
        lf.loop_filter_delta_enabled = r.read_bit()?;
        lf.loop_filter_delta_update = false;
        if lf.loop_filter_delta_enabled {
            lf.loop_filter_delta_update = r.read_bit()?;
            if lf.loop_filter_delta_update {
                for i in 0..TOTAL_REFS_PER_FRAME {
                    if r.read_bit()? {
                        lf.loop_filter_ref_deltas[i] = r.read_su(7)? as i8;
                    }
                }
                for i in 0..2 {
                    if r.read_bit()? {
                        lf.loop_filter_mode_deltas[i] = r.read_su(7)? as i8;
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_cdef_params(
        r: &mut Reader,
        fh: &mut FrameHeaderObu,
        enable_cdef: bool,
        num_planes: u32,
    ) -> Result<()> {
        let cdef = &mut fh.cdef_params;
        if fh.coded_lossless || fh.allow_intrabc || !enable_cdef {
            *cdef = CdefParams {
                cdef_damping: 3,
                ..Default::default()
            };
            return Ok(());
        }
        cdef.cdef_damping = r.read_bits(2)? + 3;
        cdef.cdef_bits = r.read_bits(2)?;
        for i in 0..(1usize << cdef.cdef_bits) {
            cdef.cdef_y_pri_strength[i] = r.read_bits(4)?;
            cdef.cdef_y_sec_strength[i] = r.read_bits(2)?;
            if cdef.cdef_y_sec_strength[i] == 3 {
                cdef.cdef_y_sec_strength[i] += 1;
            }
            if num_planes > 1 {
                cdef.cdef_uv_pri_strength[i] = r.read_bits(4)?;
                cdef.cdef_uv_sec_strength[i] = r.read_bits(2)?;
                if cdef.cdef_uv_sec_strength[i] == 3 {
                    cdef.cdef_uv_sec_strength[i] += 1;
                }
            }
        }
        Ok(())
    }

    fn parse_loop_restoration_params(
        r: &mut Reader,
        fh: &mut FrameHeaderObu,
        seq: &SequenceHeaderObu,
    ) -> Result<()> {
        let lr = &mut fh.loop_restoration_params;
        *lr = LoopRestorationParams::default();
        if fh.all_lossless || fh.allow_intrabc || !seq.enable_restoration {
            return Ok(());
        }
        const REMAP_LR_TYPE: [u8; 4] = [
            RESTORE_NONE,
            RESTORE_SWITCHABLE,
            RESTORE_WIENER,
            RESTORE_SGRPROJ,
        ];
        for i in 0..seq.num_planes.min(MAX_NUM_PLANES as u32) as usize {
            lr.frame_restoration_type[i] = REMAP_LR_TYPE[r.read_bits(2)? as usize & 3];
            if lr.frame_restoration_type[i] != RESTORE_NONE {
                lr.uses_lr = true;
                if i > 0 {
                    lr.uses_chroma_lr = true;
                }
            }
        }
        if lr.uses_lr {
            if seq.use_128x128_superblock {
                lr.lr_unit_shift = r.read_bits(1)? as u8 + 1;
            } else {
                lr.lr_unit_shift = r.read_bits(1)? as u8;
                if lr.lr_unit_shift > 0 {
                    lr.lr_unit_shift += r.read_bits(1)? as u8;
                }
            }
            lr.loop_restoration_size[0] = RESTORATION_TILESIZE_MAX >> (2 - lr.lr_unit_shift);
            let cc = &seq.color_config;
            lr.lr_uv_shift = if cc.subsampling_x && cc.subsampling_y && lr.uses_chroma_lr {
                r.read_bits(1)? as u8
            } else {
                0
            };
            lr.loop_restoration_size[1] = lr.loop_restoration_size[0] >> lr.lr_uv_shift;
            lr.loop_restoration_size[2] = lr.loop_restoration_size[0] >> lr.lr_uv_shift;
        }
        Ok(())
    }

    fn read_tx_mode(r: &mut Reader, fh: &mut FrameHeaderObu) -> Result<()> {
        fh.tx_mode = if fh.coded_lossless {
            ONLY_4X4
        } else if r.read_bit()? {
            TX_MODE_SELECT
        } else {
            TX_MODE_LARGEST
        };
        Ok(())
    }

    fn parse_skip_mode_params(
        &self,
        r: &mut Reader,
        fh: &mut FrameHeaderObu,
        enable_order_hint: bool,
        order_hint_bits: i32,
    ) -> Result<()> {
        let dist = |a: u32, b: i32| {
            helpers::get_relative_dist(enable_order_hint, order_hint_bits, a as i32, b)
        };
        let mut skip_mode_allowed = false;
        if !fh.frame_is_intra && fh.reference_select && enable_order_hint {
            let order_hint = fh.order_hint as i32;
            let mut forward_idx = -1i32;
            let mut backward_idx = -1i32;
            let mut forward_hint = 0;
            let mut backward_hint = 0;
            for i in 0..REFS_PER_FRAME {
                let ref_hint = self.ref_info[fh.ref_frame_idx[i] as usize & 7].ref_order_hint;
                if dist(ref_hint, order_hint) < 0 {
                    if forward_idx < 0 || dist(ref_hint, forward_hint) > 0 {
                        forward_idx = i as i32;
                        forward_hint = ref_hint as i32;
                    }
                } else if dist(ref_hint, order_hint) > 0
                    && (backward_idx < 0 || dist(ref_hint, backward_hint) < 0)
                {
                    backward_idx = i as i32;
                    backward_hint = ref_hint as i32;
                }
            }
            if forward_idx < 0 {
                skip_mode_allowed = false;
            } else if backward_idx >= 0 {
                skip_mode_allowed = true;
                fh.skip_mode_frame[0] = (LAST_FRAME as i32 + forward_idx.min(backward_idx)) as u32;
                fh.skip_mode_frame[1] = (LAST_FRAME as i32 + forward_idx.max(backward_idx)) as u32;
            } else {
                let mut second_forward_idx = -1i32;
                let mut second_forward_hint = 0;
                for i in 0..REFS_PER_FRAME {
                    let ref_hint = self.ref_info[fh.ref_frame_idx[i] as usize & 7].ref_order_hint;
                    if dist(ref_hint, forward_hint) < 0
                        && (second_forward_idx < 0 || dist(ref_hint, second_forward_hint) > 0)
                    {
                        second_forward_idx = i as i32;
                        second_forward_hint = ref_hint as i32;
                    }
                }
                if second_forward_idx >= 0 {
                    skip_mode_allowed = true;
                    fh.skip_mode_frame[0] =
                        (LAST_FRAME as i32 + forward_idx.min(second_forward_idx)) as u32;
                    fh.skip_mode_frame[1] =
                        (LAST_FRAME as i32 + forward_idx.max(second_forward_idx)) as u32;
                }
            }
        }
        fh.skip_mode_present = skip_mode_allowed && r.read_bit()?;
        Ok(())
    }

    fn get_qindex(fh: &FrameHeaderObu, segment_id: usize) -> i64 {
        let base_q_idx = i64::from(fh.quantization_params.base_q_idx);
        let seg = &fh.segmentation_params;
        if seg.segmentation_enabled && seg.feature_enabled[segment_id][SEG_LVL_ALT_Q] {
            let data = i64::from(seg.feature_data[segment_id][SEG_LVL_ALT_Q]);
            helpers::clip3(0, 255, base_q_idx + data)
        } else {
            base_q_idx
        }
    }

    fn setup_shear(warp_params: &[i32; 6]) -> bool {
        if *warp_params == DEFAULT_WARP {
            return true;
        }
        let w = warp_params.map(i64::from);
        let alpha0 = helpers::clip3(-32768, 32767, w[2] - (1 << WARPEDMODEL_PREC_BITS));
        let beta0 = helpers::clip3(-32768, 32767, w[3]);
        let Some((div_shift, div_factor)) = helpers::resolve_divisor(w[2]) else {
            return false;
        };
        let v = (w[4] << WARPEDMODEL_PREC_BITS) * div_factor;
        let gamma0 = helpers::clip3(-32768, 32767, helpers::round2signed(v, div_shift));
        let w34 = w[3] * w[4];
        let delta0 = helpers::clip3(
            -32768,
            32767,
            w[5] - helpers::round2signed(w34 * div_factor, div_shift)
                - (1 << WARPEDMODEL_PREC_BITS),
        );
        let reduce =
            |x: i64| helpers::round2signed(x, WARP_PARAM_REDUCE_BITS) << WARP_PARAM_REDUCE_BITS;
        let (alpha, beta, gamma, delta) = (
            reduce(alpha0),
            reduce(beta0),
            reduce(gamma0),
            reduce(delta0),
        );
        4 * alpha.abs() + 7 * beta.abs() < (1 << WARPEDMODEL_PREC_BITS)
            && 4 * gamma.abs() + 4 * delta.abs() < (1 << WARPEDMODEL_PREC_BITS)
    }

    fn read_global_param(
        r: &mut Reader,
        type_: u8,
        ref_frame: usize,
        idx: usize,
        allow_high_precision_mv: bool,
        prev_gm_params: &[[i32; 6]; NUM_REF_FRAMES],
        gm_params: &mut [[i32; 6]; NUM_REF_FRAMES],
    ) -> Result<()> {
        let mut abs_bits = GM_ABS_ALPHA_BITS;
        let mut prec_bits = GM_ALPHA_PREC_BITS;
        if idx < 2 {
            if type_ == TRANSLATION {
                abs_bits = GM_ABS_TRANS_ONLY_BITS - !allow_high_precision_mv as u32;
                prec_bits = GM_TRANS_ONLY_PREC_BITS - !allow_high_precision_mv as u32;
            } else {
                abs_bits = GM_ABS_TRANS_BITS;
                prec_bits = GM_TRANS_PREC_BITS;
            }
        }
        let prec_diff = WARPEDMODEL_PREC_BITS - prec_bits;
        let (round, sub) = if idx % 3 == 2 {
            (1 << WARPEDMODEL_PREC_BITS, 1 << prec_bits)
        } else {
            (0, 0)
        };
        let mx = 1 << abs_bits;
        let reference = (prev_gm_params[ref_frame][idx] >> prec_diff) - sub;
        gm_params[ref_frame][idx] =
            (r.decode_signed_subexp_with_ref(-mx, mx + 1, reference)? << prec_diff) + round;
        Ok(())
    }

    fn parse_global_motion_params(&self, r: &mut Reader, fh: &mut FrameHeaderObu) -> Result<()> {
        let gm = &mut fh.global_motion_params;
        *gm = GlobalMotionParams::default();
        if fh.frame_is_intra {
            return Ok(());
        }
        let prev_gm_params = if fh.primary_ref_frame == PRIMARY_REF_NONE {
            [DEFAULT_WARP; NUM_REF_FRAMES]
        } else {
            let prev_frame = fh.ref_frame_idx[fh.primary_ref_frame as usize % REFS_PER_FRAME];
            self.ref_info[prev_frame as usize & 7]
                .global_motion_params
                .gm_params
        };
        for ref_frame in LAST_FRAME..=ALTREF_FRAME {
            gm.is_global[ref_frame] = r.read_bit()?;
            let type_ = if gm.is_global[ref_frame] {
                gm.is_rot_zoom[ref_frame] = r.read_bit()?;
                if gm.is_rot_zoom[ref_frame] {
                    ROTZOOM
                } else {
                    gm.is_translation[ref_frame] = r.read_bit()?;
                    if gm.is_translation[ref_frame] {
                        TRANSLATION
                    } else {
                        AFFINE
                    }
                }
            } else {
                IDENTITY
            };
            gm.gm_type[ref_frame] = type_;
            let hp = fh.allow_high_precision_mv;
            if type_ >= ROTZOOM {
                Self::read_global_param(
                    r,
                    type_,
                    ref_frame,
                    2,
                    hp,
                    &prev_gm_params,
                    &mut gm.gm_params,
                )?;
                Self::read_global_param(
                    r,
                    type_,
                    ref_frame,
                    3,
                    hp,
                    &prev_gm_params,
                    &mut gm.gm_params,
                )?;
                if type_ == AFFINE {
                    Self::read_global_param(
                        r,
                        type_,
                        ref_frame,
                        4,
                        hp,
                        &prev_gm_params,
                        &mut gm.gm_params,
                    )?;
                    Self::read_global_param(
                        r,
                        type_,
                        ref_frame,
                        5,
                        hp,
                        &prev_gm_params,
                        &mut gm.gm_params,
                    )?;
                } else {
                    gm.gm_params[ref_frame][4] = -gm.gm_params[ref_frame][3];
                    gm.gm_params[ref_frame][5] = gm.gm_params[ref_frame][2];
                }
            }
            if type_ >= TRANSLATION {
                Self::read_global_param(
                    r,
                    type_,
                    ref_frame,
                    0,
                    hp,
                    &prev_gm_params,
                    &mut gm.gm_params,
                )?;
                Self::read_global_param(
                    r,
                    type_,
                    ref_frame,
                    1,
                    hp,
                    &prev_gm_params,
                    &mut gm.gm_params,
                )?;
            }
            gm.warp_valid[ref_frame] = Self::setup_shear(&gm.gm_params[ref_frame]);
        }
        Ok(())
    }

    fn parse_film_grain_parameters(
        &self,
        r: &mut Reader,
        fh: &mut FrameHeaderObu,
        seq: &SequenceHeaderObu,
    ) -> Result<()> {
        let fg = &mut fh.film_grain_params;
        if !seq.film_grain_params_present || (!fh.show_frame && !fh.showable_frame) {
            *fg = Default::default();
            return Ok(());
        }
        fg.apply_grain = r.read_bit()?;
        if !fg.apply_grain {
            *fg = Default::default();
            return Ok(());
        }
        fg.grain_seed = r.read_bits(16)? as u16;
        fg.update_grain = if fh.frame_type == FrameType::InterFrame {
            r.read_bit()?
        } else {
            true
        };
        if !fg.update_grain {
            let film_grain_params_ref_idx = r.read_bits(3)? as u8;
            if !fh.ref_frame_idx.contains(&film_grain_params_ref_idx) {
                return Err("film_grain_params_ref_idx is not a reference of this frame".into());
            }
            let grain_seed = fg.grain_seed;
            *fg = self.ref_info[film_grain_params_ref_idx as usize & 7].film_grain_params;
            fg.grain_seed = grain_seed;
            fg.film_grain_params_ref_idx = film_grain_params_ref_idx;
            fg.update_grain = false;
            return Ok(());
        }
        let cc = &seq.color_config;
        fg.num_y_points = r.read_bits(4)? as u8;
        if fg.num_y_points > 14 {
            return Err(format!("num_y_points {}", fg.num_y_points));
        }
        for i in 0..fg.num_y_points as usize {
            fg.point_y_value[i] = r.read_bits(8)? as u8;
            if i > 0 && fg.point_y_value[i - 1] >= fg.point_y_value[i] {
                return Err("point_y_value is not increasing".into());
            }
            fg.point_y_scaling[i] = r.read_bits(8)? as u8;
        }
        fg.chroma_scaling_from_luma = !cc.mono_chrome && r.read_bit()?;
        if cc.mono_chrome
            || fg.chroma_scaling_from_luma
            || (cc.subsampling_x && cc.subsampling_y && fg.num_y_points == 0)
        {
            fg.num_cb_points = 0;
            fg.num_cr_points = 0;
        } else {
            fg.num_cb_points = r.read_bits(4)? as u8;
            if fg.num_cb_points > 10 {
                return Err(format!("num_cb_points {}", fg.num_cb_points));
            }
            for i in 0..fg.num_cb_points as usize {
                fg.point_cb_value[i] = r.read_bits(8)? as u8;
                if i > 0 && fg.point_cb_value[i - 1] >= fg.point_cb_value[i] {
                    return Err("point_cb_value is not increasing".into());
                }
                fg.point_cb_scaling[i] = r.read_bits(8)? as u8;
            }
            fg.num_cr_points = r.read_bits(4)? as u8;
            if fg.num_cr_points > 10 {
                return Err(format!("num_cr_points {}", fg.num_cr_points));
            }
            for i in 0..fg.num_cr_points as usize {
                fg.point_cr_value[i] = r.read_bits(8)? as u8;
                if i > 0 && fg.point_cr_value[i - 1] >= fg.point_cr_value[i] {
                    return Err("point_cr_value is not increasing".into());
                }
                fg.point_cr_scaling[i] = r.read_bits(8)? as u8;
            }
        }
        fg.grain_scaling_minus_8 = r.read_bits(2)? as u8;
        fg.ar_coeff_lag = r.read_bits(2)?;
        let num_pos_luma = (2 * fg.ar_coeff_lag * (fg.ar_coeff_lag + 1)) as usize;
        let num_pos_chroma = if fg.num_y_points > 0 {
            for i in 0..num_pos_luma {
                fg.ar_coeffs_y_plus_128[i] = r.read_bits(8)? as u8;
            }
            num_pos_luma + 1
        } else {
            num_pos_luma
        };
        if fg.chroma_scaling_from_luma || fg.num_cb_points > 0 {
            for i in 0..num_pos_chroma {
                fg.ar_coeffs_cb_plus_128[i] = r.read_bits(8)? as u8;
            }
        }
        if fg.chroma_scaling_from_luma || fg.num_cr_points > 0 {
            for i in 0..num_pos_chroma {
                fg.ar_coeffs_cr_plus_128[i] = r.read_bits(8)? as u8;
            }
        }
        fg.ar_coeff_shift_minus_6 = r.read_bits(2)? as u8;
        fg.grain_scale_shift = r.read_bits(2)? as u8;
        if fg.num_cb_points > 0 {
            fg.cb_mult = r.read_bits(8)? as u8;
            fg.cb_luma_mult = r.read_bits(8)? as u8;
            fg.cb_offset = r.read_bits(9)? as u16;
        }
        if fg.num_cr_points > 0 {
            fg.cr_mult = r.read_bits(8)? as u8;
            fg.cr_luma_mult = r.read_bits(8)? as u8;
            fg.cr_offset = r.read_bits(9)? as u16;
        }
        fg.overlap_flag = r.read_bit()?;
        fg.clip_to_restricted_range = r.read_bit()?;
        Ok(())
    }

    fn parse_uncompressed_frame_header(&mut self, obu: &Obu) -> Result<FrameHeaderObu> {
        let mut r = Reader::new(obu.data);
        let mut fh = FrameHeaderObu {
            obu_header: obu.header,
            ..Default::default()
        };
        let seq = self.sequence()?.clone();
        let id_len = if seq.frame_id_numbers_present_flag {
            seq.additional_frame_id_length_minus_1 + seq.delta_frame_id_length_minus_2 + 3
        } else {
            0
        };
        let order_hint_bits = seq.order_hint_bits.max(0) as u32;
        let dmi = &seq.decoder_model_info;
        if seq.reduced_still_picture_header {
            fh.show_existing_frame = false;
            fh.frame_type = FrameType::KeyFrame;
            fh.frame_is_intra = true;
            fh.show_frame = true;
            fh.showable_frame = false;
        } else {
            fh.show_existing_frame = r.read_bit()?;
            if obu.header.obu_type == ObuType::Frame && fh.show_existing_frame {
                return Err("an OBU_FRAME with show_existing_frame".into());
            }
            if fh.show_existing_frame {
                fh.frame_to_show_map_idx = r.read_bits(3)? as u8;
                if seq.decoder_model_info_present_flag && !seq.timing_info.equal_picture_interval {
                    fh.frame_presentation_time =
                        r.read_bits(dmi.frame_presentation_time_length_minus_1 + 1)?;
                }
                let ref_frame = &self.ref_info[fh.frame_to_show_map_idx as usize];
                fh.refresh_frame_flags = 0;
                if seq.frame_id_numbers_present_flag {
                    fh.display_frame_id = r.read_bits(id_len)?;
                    if ref_frame.display_frame_id != fh.display_frame_id || !ref_frame.ref_valid {
                        return Err("display_frame_id does not match".into());
                    }
                }
                if !ref_frame.ref_valid {
                    return Err("show_existing_frame of an empty slot".into());
                }
                self.load_reference_frame(&mut fh)?;
                if fh.frame_type == FrameType::KeyFrame {
                    fh.refresh_frame_flags = ALL_FRAMES;
                }
                if seq.film_grain_params_present {
                    fh.film_grain_params =
                        self.ref_info[fh.frame_to_show_map_idx as usize].film_grain_params;
                }
                fh.header_bytes = r.position() / 8;
                return Ok(fh);
            }
            fh.frame_type = FrameType::from_bits(r.read_bits(2)?);
            fh.frame_is_intra = matches!(
                fh.frame_type,
                FrameType::IntraOnlyFrame | FrameType::KeyFrame
            );
            fh.show_frame = r.read_bit()?;
            if fh.show_frame
                && seq.decoder_model_info_present_flag
                && !seq.timing_info.equal_picture_interval
            {
                fh.frame_presentation_time =
                    r.read_bits(dmi.frame_presentation_time_length_minus_1 + 1)?;
            }
            fh.showable_frame = if fh.show_frame {
                fh.frame_type != FrameType::KeyFrame
            } else {
                r.read_bit()?
            };
            fh.error_resilient_mode = if fh.frame_type == FrameType::SwitchFrame
                || (fh.frame_type == FrameType::KeyFrame && fh.show_frame)
            {
                true
            } else {
                r.read_bit()?
            };
        }
        if fh.frame_type == FrameType::KeyFrame && fh.show_frame {
            for i in 0..NUM_REF_FRAMES {
                self.ref_info[i].ref_valid = false;
                self.ref_info[i].ref_order_hint = 0;
            }
            for i in 0..REFS_PER_FRAME {
                fh.order_hints[LAST_FRAME + i] = 0;
            }
        }
        fh.disable_cdf_update = r.read_bit()?;
        fh.allow_screen_content_tools =
            if seq.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS {
                r.read_bit()? as u32
            } else {
                seq.seq_force_screen_content_tools
            };
        fh.force_integer_mv = if fh.allow_screen_content_tools > 0 {
            if seq.seq_force_integer_mv == SELECT_INTEGER_MV {
                r.read_bit()? as u32
            } else {
                seq.seq_force_integer_mv
            }
        } else {
            0
        };
        if fh.frame_is_intra {
            fh.force_integer_mv = 1;
        }
        if seq.frame_id_numbers_present_flag {
            self.prev_frame_id = self.current_frame_id;
            self.current_frame_id = r.read_bits(id_len)?;
            fh.current_frame_id = self.current_frame_id;
            let diff_len = seq.delta_frame_id_length_minus_2 + 2;
            let shifted_diff_len = 1u32 << diff_len;
            let shifted_id_len = 1u32 << id_len;
            for i in 0..NUM_REF_FRAMES {
                let id = self.ref_info[i].ref_frame_id;
                let cur = self.current_frame_id;
                if cur > shifted_diff_len {
                    if id > cur || id < cur - shifted_diff_len {
                        self.ref_info[i].ref_valid = false;
                    }
                } else if id > cur && id < shifted_id_len + cur - shifted_diff_len {
                    self.ref_info[i].ref_valid = false;
                }
            }
        } else {
            self.current_frame_id = 0;
            self.prev_frame_id = 0;
            fh.current_frame_id = 0;
        }
        fh.frame_size_override_flag = if fh.frame_type == FrameType::SwitchFrame {
            true
        } else if seq.reduced_still_picture_header {
            false
        } else {
            r.read_bit()?
        };
        fh.order_hint = r.read_bits(order_hint_bits)?;
        fh.primary_ref_frame = if fh.frame_is_intra || fh.error_resilient_mode {
            PRIMARY_REF_NONE
        } else {
            r.read_bits(3)?
        };
        if seq.decoder_model_info_present_flag {
            fh.buffer_removal_time_present_flag = r.read_bit()?;
            if fh.buffer_removal_time_present_flag {
                for op_num in 0..=seq.operating_points_cnt_minus_1 as usize {
                    let op = &seq.operating_points[op_num];
                    if op.decoder_model_present_for_this_op {
                        let in_temporal_layer = (op.idc >> fh.obu_header.temporal_id) & 1 != 0;
                        let in_spatial_layer = (op.idc >> (fh.obu_header.spatial_id + 8)) & 1 != 0;
                        if op.idc == 0 || (in_temporal_layer && in_spatial_layer) {
                            fh.buffer_removal_time[op_num] =
                                r.read_bits(u32::from(dmi.buffer_removal_time_length_minus_1) + 1)?;
                        }
                    }
                }
            }
        }
        fh.allow_high_precision_mv = false;
        fh.use_ref_frame_mvs = false;
        fh.allow_intrabc = false;
        fh.refresh_frame_flags = if fh.frame_type == FrameType::SwitchFrame
            || (fh.frame_type == FrameType::KeyFrame && fh.show_frame)
        {
            ALL_FRAMES
        } else {
            r.read_bits(8)?
        };
        if (!fh.frame_is_intra || fh.refresh_frame_flags != ALL_FRAMES)
            && fh.error_resilient_mode
            && seq.enable_order_hint
        {
            for i in 0..NUM_REF_FRAMES {
                fh.ref_order_hint[i] = r.read_bits(order_hint_bits)?;
                if fh.ref_order_hint[i] != self.ref_info[i].ref_order_hint {
                    self.ref_info[i].ref_valid = false;
                    self.ref_info[i].ref_order_hint = fh.ref_order_hint[i];
                }
            }
        }
        if fh.frame_is_intra {
            self.parse_frame_size(&mut fh, &mut r)?;
            Self::parse_render_size(&mut fh, &mut r)?;
            if fh.allow_screen_content_tools > 0 && fh.upscaled_width == fh.frame_width {
                fh.allow_intrabc = r.read_bit()?;
            }
        } else {
            fh.frame_refs_short_signaling = seq.enable_order_hint && r.read_bit()?;
            if fh.frame_refs_short_signaling {
                fh.last_frame_idx = r.read_bits(3)? as u8;
                fh.gold_frame_idx = r.read_bits(3)? as u8;
                self.set_frame_refs(&mut fh)?;
            }
            for i in 0..REFS_PER_FRAME {
                if !fh.frame_refs_short_signaling {
                    fh.ref_frame_idx[i] = r.read_bits(3)? as u8;
                }
                if seq.frame_id_numbers_present_flag {
                    let delta_frame_id = r.read_bits(seq.delta_frame_id_length_minus_2 + 2)? + 1;
                    let shifted_id_len = 1u32 << id_len;
                    let expected =
                        (self.current_frame_id + shifted_id_len - delta_frame_id) % shifted_id_len;
                    let actual = self.ref_info[fh.ref_frame_idx[i] as usize].ref_frame_id;
                    if expected != actual {
                        return Err(format!("frame id {actual}, expected {expected}"));
                    }
                }
            }
            if fh.frame_size_override_flag && !fh.error_resilient_mode {
                self.frame_size_with_refs(&mut fh, &mut r)?;
            } else {
                self.parse_frame_size(&mut fh, &mut r)?;
                Self::parse_render_size(&mut fh, &mut r)?;
            }
            fh.allow_high_precision_mv = fh.force_integer_mv == 0 && r.read_bit()?;
            fh.is_filter_switchable = r.read_bit()?;
            fh.interpolation_filter = if fh.is_filter_switchable {
                INTERP_SWITCHABLE
            } else {
                r.read_bits(2)? as u8
            };
            fh.is_motion_mode_switchable = r.read_bit()?;
            fh.use_ref_frame_mvs =
                !fh.error_resilient_mode && seq.enable_ref_frame_mvs && r.read_bit()?;
            for i in 0..REFS_PER_FRAME {
                let ref_frame = LAST_FRAME + i;
                let hint = self.ref_info[fh.ref_frame_idx[i] as usize].ref_order_hint;
                fh.order_hints[ref_frame] = hint;
                fh.ref_frame_sign_bias[ref_frame] = seq.enable_order_hint
                    && helpers::get_relative_dist(
                        seq.enable_order_hint,
                        seq.order_hint_bits,
                        hint as i32,
                        fh.order_hint as i32,
                    ) > 0;
            }
        }
        fh.disable_frame_end_update_cdf =
            seq.reduced_still_picture_header || fh.disable_cdf_update || r.read_bit()?;
        if fh.primary_ref_frame == PRIMARY_REF_NONE {
            Self::setup_past_independence(&mut fh);
        } else {
            let prev_frame = &self.ref_info
                [fh.ref_frame_idx[fh.primary_ref_frame as usize % REFS_PER_FRAME] as usize];
            if !prev_frame.ref_valid {
                return Err("the primary reference frame is missing".into());
            }
            fh.loop_filter_params.loop_filter_ref_deltas =
                prev_frame.loop_filter_params.loop_filter_ref_deltas;
            fh.loop_filter_params.loop_filter_mode_deltas =
                prev_frame.loop_filter_params.loop_filter_mode_deltas;
            fh.segmentation_params.feature_enabled = prev_frame.segmentation_params.feature_enabled;
            fh.segmentation_params.feature_data = prev_frame.segmentation_params.feature_data;
        }
        let mut tile_info = TileInfo::default();
        self.parse_tile_info(&mut r, &mut tile_info)?;
        fh.tile_info = tile_info;
        Self::parse_quantization_params(
            &mut r,
            &mut fh.quantization_params,
            seq.num_planes,
            seq.color_config.separate_uv_delta_q,
        )?;
        self.parse_segmentation_params(&mut r, &mut fh)?;
        Self::parse_delta_q_params(&mut r, &mut fh.quantization_params)?;
        Self::parse_delta_lf_params(
            &mut r,
            &mut fh.loop_filter_params,
            fh.quantization_params.delta_q_present,
            fh.allow_intrabc,
        )?;
        fh.coded_lossless = true;
        for segment_id in 0..MAX_SEGMENTS {
            let q_index = Self::get_qindex(&fh, segment_id);
            let q = &fh.quantization_params;
            fh.lossless_array[segment_id] = q_index == 0
                && q.delta_q_y_dc == 0
                && q.delta_q_u_ac == 0
                && q.delta_q_u_dc == 0
                && q.delta_q_v_ac == 0
                && q.delta_q_v_dc == 0;
            if !fh.lossless_array[segment_id] {
                fh.coded_lossless = false;
            }
            if q.using_qmatrix {
                fh.seg_qm_level[0][segment_id] = if fh.lossless_array[segment_id] {
                    15
                } else {
                    q.qm_y
                };
                fh.seg_qm_level[1][segment_id] = if fh.lossless_array[segment_id] {
                    15
                } else {
                    q.qm_u
                };
                fh.seg_qm_level[2][segment_id] = if fh.lossless_array[segment_id] {
                    15
                } else {
                    q.qm_v
                };
            }
        }
        fh.all_lossless = fh.coded_lossless && fh.frame_width == fh.upscaled_width;
        Self::parse_loop_filter_parameters(&mut r, &mut fh, seq.num_planes)?;
        Self::parse_cdef_params(&mut r, &mut fh, seq.enable_cdef, seq.num_planes)?;
        Self::parse_loop_restoration_params(&mut r, &mut fh, &seq)?;
        Self::read_tx_mode(&mut r, &mut fh)?;
        fh.reference_select = !fh.frame_is_intra && r.read_bit()?;
        self.parse_skip_mode_params(&mut r, &mut fh, seq.enable_order_hint, seq.order_hint_bits)?;
        fh.allow_warped_motion = !fh.frame_is_intra
            && !fh.error_resilient_mode
            && seq.enable_warped_motion
            && r.read_bit()?;
        fh.reduced_tx_set = r.read_bit()?;
        self.parse_global_motion_params(&mut r, &mut fh)?;
        self.parse_film_grain_parameters(&mut r, &mut fh, &seq)?;
        Self::skip_and_check_trailing_bits(&mut r, obu)?;
        if obu.header.obu_type == ObuType::Frame {
            r.byte_alignment()?;
        }
        fh.header_bytes = r.position() / 8;
        Ok(fh)
    }

    pub fn parse_tile_group_obu(
        &mut self,
        data: &[u8],
        tiles: &mut Vec<Tile>,
    ) -> Result<TileGroupObu> {
        let mut tg = TileGroupObu::default();
        let mut r = Reader::new(data);
        let num_tiles = self.num_tiles();
        if num_tiles == 0 {
            return Err("a tile group before a frame header".into());
        }
        if num_tiles > 1 {
            tg.tile_start_and_end_present_flag = r.read_bit()?;
        }
        if num_tiles == 1 || !tg.tile_start_and_end_present_flag {
            tg.tg_start = 0;
            tg.tg_end = num_tiles - 1;
        } else {
            let tile_bits = self.tile.tile_cols_log2 + self.tile.tile_rows_log2;
            tg.tg_start = r.read_bits(tile_bits)?;
            tg.tg_end = r.read_bits(tile_bits)?;
        }
        if tg.tg_end < tg.tg_start || tg.tg_end >= num_tiles {
            return Err(format!("tile group {}..{}", tg.tg_start, tg.tg_end));
        }
        r.byte_alignment()?;
        let mut sz = (data.len() - r.position() / 8) as u64;
        let tile_size_bytes = self.tile.tile_size_bytes;
        for tile_num in tg.tg_start..=tg.tg_end {
            let tile_size = if tile_num == tg.tg_end {
                sz
            } else {
                let size = u64::from(r.read_le(tile_size_bytes)?) + 1;
                sz = sz
                    .checked_sub(size + u64::from(tile_size_bytes))
                    .ok_or_else(|| "tile larger than its tile group".to_string())?;
                size
            };
            tiles.push(Tile {
                tile_offset: (r.position() / 8) as u32,
                tile_size: tile_size as u32,
                tile_row: tile_num / self.tile.tile_cols,
                tile_col: tile_num % self.tile.tile_cols,
            });
            if tile_num < tg.tg_end {
                r.skip_bits(tile_size as usize * 8)?;
            }
        }
        tg.last = tg.tg_end == num_tiles - 1;
        if tg.last {
            self.seen_frame_header = false;
        }
        Ok(tg)
    }

    pub fn parse_frame_header_obu(&mut self, obu: &Obu) -> Result<FrameHeaderObu> {
        if self.seen_frame_header {
            let mut header = self
                .last_frame_header
                .clone()
                .ok_or_else(|| "no previous frame header to copy".to_string())?;
            header.header_bytes = obu.data.len();
            return Ok(header);
        }
        self.seen_frame_header = true;
        let header = self.parse_uncompressed_frame_header(obu)?;
        if header.show_existing_frame {
            self.last_frame_header = None;
            self.seen_frame_header = false;
        } else {
            self.last_frame_header = Some(header.clone());
        }
        self.is_first_frame = false;
        Ok(header)
    }

    pub fn ref_frame_update(&mut self, fh: &FrameHeaderObu) -> Result<()> {
        if fh.show_existing_frame && fh.frame_type != FrameType::KeyFrame {
            return Ok(());
        }
        if fh.frame_type == FrameType::IntraOnlyFrame && fh.refresh_frame_flags == ALL_FRAMES {
            return Err("an intra-only frame refreshes every reference".into());
        }
        let film_grain_params_present = self.sequence()?.film_grain_params_present;
        for (i, ref_info) in self.ref_info.iter_mut().enumerate() {
            if (fh.refresh_frame_flags >> i) & 1 != 0 {
                ref_info.ref_valid = true;
                ref_info.ref_frame_id = fh.current_frame_id;
                ref_info.ref_frame_type = fh.frame_type;
                ref_info.ref_upscaled_width = fh.upscaled_width;
                ref_info.ref_frame_width = fh.frame_width;
                ref_info.ref_frame_height = fh.frame_height;
                ref_info.ref_render_width = fh.render_width;
                ref_info.ref_render_height = fh.render_height;
                ref_info.ref_order_hint = fh.order_hint;
                ref_info.ref_mi_cols = fh.mi_cols;
                ref_info.ref_mi_rows = fh.mi_rows;
                ref_info.segmentation_params = fh.segmentation_params;
                ref_info.global_motion_params = fh.global_motion_params;
                ref_info.loop_filter_params = fh.loop_filter_params;
                ref_info.display_frame_id = fh.display_frame_id;
                ref_info.showable_frame = fh.showable_frame && !fh.show_existing_frame;
                if film_grain_params_present {
                    ref_info.film_grain_params = fh.film_grain_params;
                }
            }
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        let sequence_header = self.sequence_header.take();
        let operating_point_idc = self.operating_point_idc;
        *self = Self {
            sequence_header,
            operating_point_idc,
            ..Default::default()
        };
    }
}
