#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]

use std::ffi::{c_char, c_int, c_uint, c_void};

pub type VADisplay = *mut c_void;
pub type VAStatus = c_int;
pub type VAGenericID = c_uint;
pub type VAConfigID = VAGenericID;
pub type VAContextID = VAGenericID;
pub type VASurfaceID = VAGenericID;
pub type VABufferID = VAGenericID;
pub type VAImageID = VAGenericID;
pub type VAProfile = c_int;
pub type VAEntrypoint = c_int;
pub type VABufferType = c_int;
pub type VAConfigAttribType = c_int;
pub type VAMessageCallback =
    Option<unsafe extern "C" fn(user_context: *mut c_void, message: *const c_char)>;

pub const VA_STATUS_SUCCESS: VAStatus = 0;
pub const VA_STATUS_ERROR_UNSUPPORTED_PROFILE: VAStatus = 0x0c;
pub const VA_STATUS_ERROR_UNSUPPORTED_ENTRYPOINT: VAStatus = 0x0d;
pub const VA_STATUS_ERROR_UNSUPPORTED_RT_FORMAT: VAStatus = 0x0e;
pub const VA_STATUS_ERROR_RESOLUTION_NOT_SUPPORTED: VAStatus = 0x13;
pub const VA_STATUS_ERROR_UNIMPLEMENTED: VAStatus = 0x14;
pub const VA_STATUS_ERROR_DECODING_ERROR: VAStatus = 0x17;
pub const VA_STATUS_ERROR_TIMEDOUT: VAStatus = 0x26;

pub const VA_INVALID_ID: VAGenericID = 0xffff_ffff;
pub const VA_INVALID_SURFACE: VASurfaceID = VA_INVALID_ID;

pub const VAProfileH264Main: VAProfile = 6;
pub const VAProfileH264High: VAProfile = 7;
pub const VAProfileH264ConstrainedBaseline: VAProfile = 13;
pub const VAProfileHEVCMain: VAProfile = 17;
pub const VAProfileHEVCMain10: VAProfile = 18;
pub const VAProfileVP9Profile0: VAProfile = 19;
pub const VAProfileVP9Profile2: VAProfile = 21;
pub const VAEntrypointVLD: VAEntrypoint = 1;

pub const VAConfigAttribRTFormat: VAConfigAttribType = 0;
pub const VA_RT_FORMAT_YUV420: c_uint = 0x0000_0001;
pub const VA_RT_FORMAT_YUV420_10: c_uint = 0x0000_0100;
pub const VA_RT_FORMAT_RGB32: c_uint = 0x0002_0000;

pub const VA_PROGRESSIVE: c_int = 0x1;

pub const VAPictureParameterBufferType: VABufferType = 0;
pub const VAIQMatrixBufferType: VABufferType = 1;
pub const VASliceParameterBufferType: VABufferType = 4;
pub const VASliceDataBufferType: VABufferType = 5;

pub const VA_SLICE_DATA_FLAG_ALL: u32 = 0x00;

pub const VA_PICTURE_H264_INVALID: u32 = 0x0000_0001;
pub const VA_PICTURE_H264_SHORT_TERM_REFERENCE: u32 = 0x0000_0008;
pub const VA_PICTURE_H264_LONG_TERM_REFERENCE: u32 = 0x0000_0010;

pub const VA_FOURCC_NV12: u32 = 0x3231_564E;
pub const VA_FOURCC_P010: u32 = 0x3031_3050;
pub const VA_FOURCC_RGBA: u32 = 0x4142_4752;

pub const VA_PICTURE_HEVC_INVALID: u32 = 0x0000_0001;
pub const VA_PICTURE_HEVC_LONG_TERM_REFERENCE: u32 = 0x0000_0008;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE: u32 = 0x0000_0010;
pub const VA_PICTURE_HEVC_RPS_ST_CURR_AFTER: u32 = 0x0000_0020;
pub const VA_PICTURE_HEVC_RPS_LT_CURR: u32 = 0x0000_0040;

pub const VA_PADDING_LOW: usize = 4;
pub const VA_PADDING_MEDIUM: usize = 8;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAConfigAttrib {
    pub type_: VAConfigAttribType,
    pub value: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VAPictureH264 {
    pub picture_id: VASurfaceID,
    pub frame_idx: u32,
    pub flags: u32,
    pub TopFieldOrderCnt: i32,
    pub BottomFieldOrderCnt: i32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

impl VAPictureH264 {
    pub const INVALID: Self = Self {
        picture_id: VA_INVALID_SURFACE,
        frame_idx: 0,
        flags: VA_PICTURE_H264_INVALID,
        TopFieldOrderCnt: 0,
        BottomFieldOrderCnt: 0,
        va_reserved: [0; VA_PADDING_LOW],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16],
    pub picture_width_in_mbs_minus1: u16,
    pub picture_height_in_mbs_minus1: u16,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames: u8,
    pub seq_fields: u32,
    pub num_slice_groups_minus1: u8,
    pub slice_group_map_type: u8,
    pub slice_group_change_rate_minus1: u16,
    pub pic_init_qp_minus26: i8,
    pub pic_init_qs_minus26: i8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: u32,
    pub frame_num: u16,
    pub va_reserved: [u32; VA_PADDING_MEDIUM],
}

pub mod seq_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const RESIDUAL_COLOUR_TRANSFORM: u32 = 2;
    pub const GAPS_IN_FRAME_NUM_ALLOWED: u32 = 3;
    pub const FRAME_MBS_ONLY: u32 = 4;
    pub const MB_ADAPTIVE_FRAME_FIELD: u32 = 5;
    pub const DIRECT_8X8_INFERENCE: u32 = 6;
    pub const MIN_LUMA_BI_PRED_SIZE_8X8: u32 = 7;
    pub const LOG2_MAX_FRAME_NUM_MINUS4: u32 = 8;
    pub const PIC_ORDER_CNT_TYPE: u32 = 12;
    pub const LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4: u32 = 14;
    pub const DELTA_PIC_ORDER_ALWAYS_ZERO: u32 = 18;
}

pub mod pic_bits {
    pub const ENTROPY_CODING_MODE: u32 = 0;
    pub const WEIGHTED_PRED: u32 = 1;
    pub const WEIGHTED_BIPRED_IDC: u32 = 2;
    pub const TRANSFORM_8X8_MODE: u32 = 4;
    pub const FIELD_PIC: u32 = 5;
    pub const CONSTRAINED_INTRA_PRED: u32 = 6;
    pub const PIC_ORDER_PRESENT: u32 = 7;
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 8;
    pub const REDUNDANT_PIC_CNT_PRESENT: u32 = 9;
    pub const REFERENCE_PIC: u32 = 10;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAIQMatrixBufferH264 {
    pub ScalingList4x4: [[u8; 16]; 6],
    pub ScalingList8x8: [[u8; 64]; 2],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VASliceParameterBufferH264 {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub slice_data_bit_offset: u16,
    pub first_mb_in_slice: u16,
    pub slice_type: u8,
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub RefPicList0: [VAPictureH264; 32],
    pub RefPicList1: [VAPictureH264; 32],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i16; 32],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i16; 2]; 32],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i16; 2]; 32],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VAPictureHEVC {
    pub picture_id: VASurfaceID,
    pub pic_order_cnt: i32,
    pub flags: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

impl VAPictureHEVC {
    pub const INVALID: Self = Self {
        picture_id: VA_INVALID_SURFACE,
        pic_order_cnt: 0,
        flags: VA_PICTURE_HEVC_INVALID,
        va_reserved: [0; VA_PADDING_LOW],
    };
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAPictureParameterBufferHEVC {
    pub CurrPic: VAPictureHEVC,
    pub ReferenceFrames: [VAPictureHEVC; 15],
    pub pic_width_in_luma_samples: u16,
    pub pic_height_in_luma_samples: u16,
    pub pic_fields: u32,
    pub sps_max_dec_pic_buffering_minus1: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub pcm_sample_bit_depth_luma_minus1: u8,
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_transform_block_size_minus2: u8,
    pub log2_diff_max_min_transform_block_size: u8,
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub init_qp_minus26: i8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u16; 19],
    pub row_height_minus1: [u16; 21],
    pub slice_parsing_fields: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pic_sps: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub num_extra_slice_header_bits: u8,
    pub st_rps_bits: u32,
    pub va_reserved: [u32; VA_PADDING_MEDIUM],
}

pub mod hevc_pic_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const SEPARATE_COLOUR_PLANE: u32 = 2;
    pub const PCM_ENABLED: u32 = 3;
    pub const SCALING_LIST_ENABLED: u32 = 4;
    pub const TRANSFORM_SKIP_ENABLED: u32 = 5;
    pub const AMP_ENABLED: u32 = 6;
    pub const STRONG_INTRA_SMOOTHING_ENABLED: u32 = 7;
    pub const SIGN_DATA_HIDING_ENABLED: u32 = 8;
    pub const CONSTRAINED_INTRA_PRED: u32 = 9;
    pub const CU_QP_DELTA_ENABLED: u32 = 10;
    pub const WEIGHTED_PRED: u32 = 11;
    pub const WEIGHTED_BIPRED: u32 = 12;
    pub const TRANSQUANT_BYPASS_ENABLED: u32 = 13;
    pub const TILES_ENABLED: u32 = 14;
    pub const ENTROPY_CODING_SYNC_ENABLED: u32 = 15;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 16;
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED: u32 = 17;
    pub const PCM_LOOP_FILTER_DISABLED: u32 = 18;
    pub const NO_PIC_REORDERING: u32 = 19;
    pub const NO_BI_PRED: u32 = 20;
}

pub mod hevc_slice_parsing_bits {
    pub const LISTS_MODIFICATION_PRESENT: u32 = 0;
    pub const LONG_TERM_REF_PICS_PRESENT: u32 = 1;
    pub const SPS_TEMPORAL_MVP_ENABLED: u32 = 2;
    pub const CABAC_INIT_PRESENT: u32 = 3;
    pub const OUTPUT_FLAG_PRESENT: u32 = 4;
    pub const DEPENDENT_SLICE_SEGMENTS_ENABLED: u32 = 5;
    pub const PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT: u32 = 6;
    pub const SAMPLE_ADAPTIVE_OFFSET_ENABLED: u32 = 7;
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED: u32 = 8;
    pub const PPS_DISABLE_DEBLOCKING_FILTER: u32 = 9;
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u32 = 10;
    pub const RAP_PIC: u32 = 11;
    pub const IDR_PIC: u32 = 12;
    pub const INTRA_PIC: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VASliceParameterBufferHEVC {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub slice_data_byte_offset: u32,
    pub slice_segment_address: u32,
    pub RefPicList: [[u8; 15]; 2],
    pub LongSliceFlags: u32,
    pub collocated_ref_idx: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    pub slice_beta_offset_div2: i8,
    pub slice_tc_offset_div2: i8,
    pub luma_log2_weight_denom: u8,
    pub delta_chroma_log2_weight_denom: i8,
    pub delta_luma_weight_l0: [i8; 15],
    pub luma_offset_l0: [i8; 15],
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    pub ChromaOffsetL0: [[i8; 2]; 15],
    pub delta_luma_weight_l1: [i8; 15],
    pub luma_offset_l1: [i8; 15],
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    pub ChromaOffsetL1: [[i8; 2]; 15],
    pub five_minus_max_num_merge_cand: u8,
    pub num_entry_point_offsets: u16,
    pub entry_offset_to_subset_array: u16,
    pub slice_data_num_emu_prevn_bytes: u16,
    pub va_reserved: [u32; VA_PADDING_LOW - 2],
}

pub mod hevc_slice_bits {
    pub const LAST_SLICE_OF_PIC: u32 = 0;
    pub const DEPENDENT_SLICE_SEGMENT: u32 = 1;
    pub const SLICE_TYPE: u32 = 2;
    pub const COLOR_PLANE_ID: u32 = 4;
    pub const SLICE_SAO_LUMA: u32 = 6;
    pub const SLICE_SAO_CHROMA: u32 = 7;
    pub const MVD_L1_ZERO: u32 = 8;
    pub const CABAC_INIT: u32 = 9;
    pub const SLICE_TEMPORAL_MVP_ENABLED: u32 = 10;
    pub const SLICE_DEBLOCKING_FILTER_DISABLED: u32 = 11;
    pub const COLLOCATED_FROM_L0: u32 = 12;
    pub const SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAIQMatrixBufferHEVC {
    pub ScalingList4x4: [[u8; 16]; 6],
    pub ScalingList8x8: [[u8; 64]; 6],
    pub ScalingList16x16: [[u8; 64]; 6],
    pub ScalingList32x32: [[u8; 64]; 2],
    pub ScalingListDC16x16: [u8; 6],
    pub ScalingListDC32x32: [u8; 2],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAImageFormat {
    pub fourcc: u32,
    pub byte_order: u32,
    pub bits_per_pixel: u32,
    pub depth: u32,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
    pub alpha_mask: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VAImage {
    pub image_id: VAImageID,
    pub format: VAImageFormat,
    pub buf: VABufferID,
    pub width: u16,
    pub height: u16,
    pub data_size: u32,
    pub num_planes: u32,
    pub pitches: [u32; 3],
    pub offsets: [u32; 3],
    pub num_palette_entries: i32,
    pub entry_bytes: i32,
    pub component_order: [i8; 4],
    pub va_reserved: [u32; VA_PADDING_LOW],
}

pub type vaInitialize =
    unsafe extern "C" fn(dpy: VADisplay, major: *mut c_int, minor: *mut c_int) -> VAStatus;
pub type vaTerminate = unsafe extern "C" fn(dpy: VADisplay) -> VAStatus;
pub type vaErrorStr = unsafe extern "C" fn(status: VAStatus) -> *const c_char;
pub type vaQueryVendorString = unsafe extern "C" fn(dpy: VADisplay) -> *const c_char;
pub type vaSetInfoCallback = unsafe extern "C" fn(
    dpy: VADisplay,
    callback: VAMessageCallback,
    user_context: *mut c_void,
) -> VAMessageCallback;
pub type vaSetErrorCallback = unsafe extern "C" fn(
    dpy: VADisplay,
    callback: VAMessageCallback,
    user_context: *mut c_void,
) -> VAMessageCallback;
pub type vaGetConfigAttributes = unsafe extern "C" fn(
    dpy: VADisplay,
    profile: VAProfile,
    entrypoint: VAEntrypoint,
    attrib_list: *mut VAConfigAttrib,
    num_attribs: c_int,
) -> VAStatus;
pub type vaCreateConfig = unsafe extern "C" fn(
    dpy: VADisplay,
    profile: VAProfile,
    entrypoint: VAEntrypoint,
    attrib_list: *mut VAConfigAttrib,
    num_attribs: c_int,
    config_id: *mut VAConfigID,
) -> VAStatus;
pub type vaDestroyConfig = unsafe extern "C" fn(dpy: VADisplay, config_id: VAConfigID) -> VAStatus;
pub type vaCreateSurfaces = unsafe extern "C" fn(
    dpy: VADisplay,
    format: c_uint,
    width: c_uint,
    height: c_uint,
    surfaces: *mut VASurfaceID,
    num_surfaces: c_uint,
    attrib_list: *mut c_void,
    num_attribs: c_uint,
) -> VAStatus;
pub type vaDestroySurfaces = unsafe extern "C" fn(
    dpy: VADisplay,
    surfaces: *mut VASurfaceID,
    num_surfaces: c_int,
) -> VAStatus;
pub type vaCreateContext = unsafe extern "C" fn(
    dpy: VADisplay,
    config_id: VAConfigID,
    picture_width: c_int,
    picture_height: c_int,
    flag: c_int,
    render_targets: *mut VASurfaceID,
    num_render_targets: c_int,
    context: *mut VAContextID,
) -> VAStatus;
pub type vaDestroyContext = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID) -> VAStatus;
pub type vaCreateBuffer = unsafe extern "C" fn(
    dpy: VADisplay,
    context: VAContextID,
    type_: VABufferType,
    size: c_uint,
    num_elements: c_uint,
    data: *mut c_void,
    buf_id: *mut VABufferID,
) -> VAStatus;
pub type vaDestroyBuffer = unsafe extern "C" fn(dpy: VADisplay, buffer_id: VABufferID) -> VAStatus;
pub type vaBeginPicture = unsafe extern "C" fn(
    dpy: VADisplay,
    context: VAContextID,
    render_target: VASurfaceID,
) -> VAStatus;
pub type vaRenderPicture = unsafe extern "C" fn(
    dpy: VADisplay,
    context: VAContextID,
    buffers: *mut VABufferID,
    num_buffers: c_int,
) -> VAStatus;
pub type vaEndPicture = unsafe extern "C" fn(dpy: VADisplay, context: VAContextID) -> VAStatus;
pub type vaSyncSurface =
    unsafe extern "C" fn(dpy: VADisplay, render_target: VASurfaceID) -> VAStatus;
pub type vaSyncSurface2 =
    unsafe extern "C" fn(dpy: VADisplay, surface: VASurfaceID, timeout_ns: u64) -> VAStatus;
pub type vaCreateImage = unsafe extern "C" fn(
    dpy: VADisplay,
    format: *mut VAImageFormat,
    width: c_int,
    height: c_int,
    image: *mut VAImage,
) -> VAStatus;
pub type vaDestroyImage = unsafe extern "C" fn(dpy: VADisplay, image: VAImageID) -> VAStatus;
pub type vaGetImage = unsafe extern "C" fn(
    dpy: VADisplay,
    surface: VASurfaceID,
    x: c_int,
    y: c_int,
    width: c_uint,
    height: c_uint,
    image: VAImageID,
) -> VAStatus;
pub type vaPutImage = unsafe extern "C" fn(
    dpy: VADisplay,
    surface: VASurfaceID,
    image: VAImageID,
    src_x: c_int,
    src_y: c_int,
    src_width: c_uint,
    src_height: c_uint,
    dest_x: c_int,
    dest_y: c_int,
    dest_width: c_uint,
    dest_height: c_uint,
) -> VAStatus;
pub type vaMapBuffer =
    unsafe extern "C" fn(dpy: VADisplay, buf_id: VABufferID, pbuf: *mut *mut c_void) -> VAStatus;
pub type vaUnmapBuffer = unsafe extern "C" fn(dpy: VADisplay, buf_id: VABufferID) -> VAStatus;
pub type vaGetDisplayDRM = unsafe extern "C" fn(fd: c_int) -> VADisplay;

pub const VAEntrypointEncSlice: VAEntrypoint = 6;
pub const VAEntrypointEncSliceLP: VAEntrypoint = 8;

pub const VAConfigAttribRateControl: VAConfigAttribType = 5;
pub const VAConfigAttribEncPackedHeaders: VAConfigAttribType = 10;
pub const VAConfigAttribEncMaxRefFrames: VAConfigAttribType = 13;
pub const VAConfigAttribMaxPictureWidth: VAConfigAttribType = 18;
pub const VAConfigAttribMaxPictureHeight: VAConfigAttribType = 19;
pub const VAConfigAttribEncHEVCFeatures: VAConfigAttribType = 50;
pub const VAConfigAttribEncHEVCBlockSizes: VAConfigAttribType = 51;
pub const VA_ATTRIB_NOT_SUPPORTED: u32 = 0x8000_0000;

pub const VA_RC_CBR: u32 = 0x0000_0002;
pub const VA_RC_VBR: u32 = 0x0000_0004;
pub const VA_RC_CQP: u32 = 0x0000_0010;
pub const VA_ENC_PACKED_HEADER_NONE: u32 = 0;

pub const VAEncCodedBufferType: VABufferType = 21;
pub const VAEncSequenceParameterBufferType: VABufferType = 22;
pub const VAEncPictureParameterBufferType: VABufferType = 23;
pub const VAEncSliceParameterBufferType: VABufferType = 24;
pub const VAEncMiscParameterBufferType: VABufferType = 27;

pub const VAEncMiscParameterTypeFrameRate: u32 = 0;
pub const VAEncMiscParameterTypeRateControl: u32 = 1;
pub const VAEncMiscParameterTypeHRD: u32 = 5;

pub const VA_PADDING_HIGH: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncSequenceParameterBufferH264 {
    pub seq_parameter_set_id: u8,
    pub level_idc: u8,
    pub intra_period: u32,
    pub intra_idr_period: u32,
    pub ip_period: u32,
    pub bits_per_second: u32,
    pub max_num_ref_frames: u32,
    pub picture_width_in_mbs: u16,
    pub picture_height_in_mbs: u16,
    pub seq_fields: u32,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub num_ref_frames_in_pic_order_cnt_cycle: u8,
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub offset_for_ref_frame: [i32; 256],
    pub frame_cropping_flag: u8,
    pub frame_crop_left_offset: u32,
    pub frame_crop_right_offset: u32,
    pub frame_crop_top_offset: u32,
    pub frame_crop_bottom_offset: u32,
    pub vui_parameters_present_flag: u8,
    pub vui_fields: u32,
    pub aspect_ratio_idc: u8,
    pub sar_width: u32,
    pub sar_height: u32,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

pub mod enc_seq_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const FRAME_MBS_ONLY: u32 = 2;
    pub const DIRECT_8X8_INFERENCE: u32 = 5;
    pub const LOG2_MAX_FRAME_NUM_MINUS4: u32 = 6;
    pub const PIC_ORDER_CNT_TYPE: u32 = 10;
    pub const LOG2_MAX_PIC_ORDER_CNT_LSB_MINUS4: u32 = 12;
}

pub mod enc_vui_bits {
    pub const ASPECT_RATIO_INFO_PRESENT: u32 = 0;
    pub const TIMING_INFO_PRESENT: u32 = 1;
    pub const BITSTREAM_RESTRICTION: u32 = 2;
    pub const LOG2_MAX_MV_LENGTH_HORIZONTAL: u32 = 3;
    pub const LOG2_MAX_MV_LENGTH_VERTICAL: u32 = 8;
    pub const FIXED_FRAME_RATE: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncPictureParameterBufferH264 {
    pub CurrPic: VAPictureH264,
    pub ReferenceFrames: [VAPictureH264; 16],
    pub coded_buf: VABufferID,
    pub pic_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub last_picture: u8,
    pub frame_num: u16,
    pub pic_init_qp: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub chroma_qp_index_offset: i8,
    pub second_chroma_qp_index_offset: i8,
    pub pic_fields: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

pub mod enc_pic_bits {
    pub const IDR_PIC: u32 = 0;
    pub const REFERENCE_PIC: u32 = 1;
    pub const ENTROPY_CODING_MODE: u32 = 3;
    pub const TRANSFORM_8X8_MODE: u32 = 8;
    pub const DEBLOCKING_FILTER_CONTROL_PRESENT: u32 = 9;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncSliceParameterBufferH264 {
    pub macroblock_address: u32,
    pub num_macroblocks: u32,
    pub macroblock_info: VABufferID,
    pub slice_type: u8,
    pub pic_parameter_set_id: u8,
    pub idr_pic_id: u16,
    pub pic_order_cnt_lsb: u16,
    pub delta_pic_order_cnt_bottom: i32,
    pub delta_pic_order_cnt: [i32; 2],
    pub direct_spatial_mv_pred_flag: u8,
    pub num_ref_idx_active_override_flag: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub RefPicList0: [VAPictureH264; 32],
    pub RefPicList1: [VAPictureH264; 32],
    pub luma_log2_weight_denom: u8,
    pub chroma_log2_weight_denom: u8,
    pub luma_weight_l0_flag: u8,
    pub luma_weight_l0: [i16; 32],
    pub luma_offset_l0: [i16; 32],
    pub chroma_weight_l0_flag: u8,
    pub chroma_weight_l0: [[i16; 2]; 32],
    pub chroma_offset_l0: [[i16; 2]; 32],
    pub luma_weight_l1_flag: u8,
    pub luma_weight_l1: [i16; 32],
    pub luma_offset_l1: [i16; 32],
    pub chroma_weight_l1_flag: u8,
    pub chroma_weight_l1: [[i16; 2]; 32],
    pub chroma_offset_l1: [[i16; 2]; 32],
    pub cabac_init_idc: u8,
    pub slice_qp_delta: i8,
    pub disable_deblocking_filter_idc: u8,
    pub slice_alpha_c0_offset_div2: i8,
    pub slice_beta_offset_div2: i8,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncSequenceParameterBufferHEVC {
    pub general_profile_idc: u8,
    pub general_level_idc: u8,
    pub general_tier_flag: u8,
    pub intra_period: u32,
    pub intra_idr_period: u32,
    pub ip_period: u32,
    pub bits_per_second: u32,
    pub pic_width_in_luma_samples: u16,
    pub pic_height_in_luma_samples: u16,
    pub seq_fields: u32,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_transform_block_size_minus2: u8,
    pub log2_diff_max_min_transform_block_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub pcm_sample_bit_depth_luma_minus1: u32,
    pub pcm_sample_bit_depth_chroma_minus1: u32,
    pub log2_min_pcm_luma_coding_block_size_minus3: u32,
    pub log2_max_pcm_luma_coding_block_size_minus3: u32,
    pub vui_parameters_present_flag: u8,
    pub vui_fields: u32,
    pub aspect_ratio_idc: u8,
    pub sar_width: u32,
    pub sar_height: u32,
    pub vui_num_units_in_tick: u32,
    pub vui_time_scale: u32,
    pub min_spatial_segmentation_idc: u16,
    pub max_bytes_per_pic_denom: u8,
    pub max_bits_per_min_cu_denom: u8,
    pub scc_fields: u32,
    pub va_reserved: [u32; VA_PADDING_MEDIUM - 1],
}

pub mod enc_hevc_seq_bits {
    pub const CHROMA_FORMAT_IDC: u32 = 0;
    pub const BIT_DEPTH_LUMA_MINUS8: u32 = 3;
    pub const BIT_DEPTH_CHROMA_MINUS8: u32 = 6;
    pub const STRONG_INTRA_SMOOTHING: u32 = 10;
    pub const AMP: u32 = 11;
    pub const SAO: u32 = 12;
    pub const TEMPORAL_MVP: u32 = 15;
    pub const LOW_DELAY_SEQ: u32 = 16;
}

pub mod enc_hevc_vui_bits {
    pub const ASPECT_RATIO_INFO_PRESENT: u32 = 0;
    pub const TIMING_INFO_PRESENT: u32 = 3;
    pub const BITSTREAM_RESTRICTION: u32 = 4;
    pub const MOTION_VECTORS_OVER_PIC_BOUNDARIES: u32 = 6;
    pub const LOG2_MAX_MV_LENGTH_HORIZONTAL: u32 = 8;
    pub const LOG2_MAX_MV_LENGTH_VERTICAL: u32 = 13;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncPictureParameterBufferHEVC {
    pub decoded_curr_pic: VAPictureHEVC,
    pub reference_frames: [VAPictureHEVC; 15],
    pub coded_buf: VABufferID,
    pub collocated_ref_pic_index: u8,
    pub last_picture: u8,
    pub pic_init_qp: u8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u8; 19],
    pub row_height_minus1: [u8; 21],
    pub log2_parallel_merge_level_minus2: u8,
    pub ctu_max_bitsize_allowed: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub slice_pic_parameter_set_id: u8,
    pub nal_unit_type: u8,
    pub pic_fields: u32,
    pub hierarchical_level_plus1: u8,
    pub va_byte_reserved: u8,
    pub scc_fields: u16,
    pub va_reserved: [u32; VA_PADDING_HIGH - 1],
}

pub mod enc_hevc_pic_bits {
    pub const IDR_PIC: u32 = 0;
    pub const CODING_TYPE: u32 = 1;
    pub const REFERENCE_PIC: u32 = 4;
    pub const CU_QP_DELTA_ENABLED: u32 = 9;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 16;
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAEncSliceParameterBufferHEVC {
    pub slice_segment_address: u32,
    pub num_ctu_in_slice: u32,
    pub slice_type: u8,
    pub slice_pic_parameter_set_id: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub ref_pic_list0: [VAPictureHEVC; 15],
    pub ref_pic_list1: [VAPictureHEVC; 15],
    pub luma_log2_weight_denom: u8,
    pub delta_chroma_log2_weight_denom: i8,
    pub delta_luma_weight_l0: [i8; 15],
    pub luma_offset_l0: [i8; 15],
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    pub chroma_offset_l0: [[i8; 2]; 15],
    pub delta_luma_weight_l1: [i8; 15],
    pub luma_offset_l1: [i8; 15],
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    pub chroma_offset_l1: [[i8; 2]; 15],
    pub max_num_merge_cand: u8,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    pub slice_beta_offset_div2: i8,
    pub slice_tc_offset_div2: i8,
    pub slice_fields: u32,
    pub pred_weight_table_bit_offset: u32,
    pub pred_weight_table_bit_length: u32,
    pub va_reserved: [u32; VA_PADDING_MEDIUM - 2],
}

pub mod enc_hevc_slice_bits {
    pub const LAST_SLICE_OF_PIC: u32 = 0;
    pub const SLICE_SAO_LUMA: u32 = 5;
    pub const SLICE_SAO_CHROMA: u32 = 6;
    pub const NUM_REF_IDX_ACTIVE_OVERRIDE: u32 = 7;
    pub const SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED: u32 = 12;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VAEncMiscParameterRateControl {
    pub bits_per_second: u32,
    pub target_percentage: u32,
    pub window_size: u32,
    pub initial_qp: u32,
    pub min_qp: u32,
    pub basic_unit_size: u32,
    pub rc_flags: u32,
    pub ICQ_quality_factor: u32,
    pub max_qp: u32,
    pub quality_factor: u32,
    pub target_frame_size: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VAEncMiscParameterFrameRate {
    pub framerate: u32,
    pub framerate_flags: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VAEncMiscParameterHRD {
    pub initial_buffer_fullness: u32,
    pub buffer_size: u32,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VACodedBufferSegment {
    pub size: u32,
    pub bit_offset: u32,
    pub status: u32,
    pub reserved: u32,
    pub buf: *mut c_void,
    pub next: *mut c_void,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

pub const VA_ENC_PACKED_HEADER_SEQUENCE: u32 = 0x1;
pub const VA_ENC_PACKED_HEADER_PICTURE: u32 = 0x2;
pub const VA_ENC_PACKED_HEADER_SLICE: u32 = 0x4;
pub const VAEncPackedHeaderSequence: u32 = 1;
pub const VAEncPackedHeaderSlice: u32 = 3;
pub const VAEncPackedHeaderParameterBufferType: VABufferType = 25;
pub const VAEncPackedHeaderDataBufferType: VABufferType = 26;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VAEncPackedHeaderParameterBuffer {
    pub type_: u32,
    pub bit_length: u32,
    pub has_emulation_bytes: u8,
    pub va_reserved: [u32; VA_PADDING_LOW],
}

pub type vaDeriveImage =
    unsafe extern "C" fn(dpy: VADisplay, surface: VASurfaceID, image: *mut VAImage) -> VAStatus;

pub const VAProfileNone: VAProfile = -1;
pub const VAEntrypointVideoProc: VAEntrypoint = 10;
pub const VAProcPipelineParameterBufferType: VABufferType = 41;
pub const VAProcColorStandardBT601: c_int = 1;
pub const VAProcColorStandardBT709: c_int = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VARectangle {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VAProcColorProperties {
    pub chroma_sample_location: u8,
    pub color_range: u8,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub reserved: [u8; 3],
}

#[cfg(target_pointer_width = "64")]
pub const PROC_RESERVED: usize = 16;
#[cfg(not(target_pointer_width = "64"))]
pub const PROC_RESERVED: usize = 19;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VAProcPipelineParameterBuffer {
    pub surface: VASurfaceID,
    pub surface_region: *const VARectangle,
    pub surface_color_standard: c_int,
    pub output_region: *const VARectangle,
    pub output_background_color: u32,
    pub output_color_standard: c_int,
    pub pipeline_flags: u32,
    pub filter_flags: u32,
    pub filters: *mut VABufferID,
    pub num_filters: u32,
    pub forward_references: *mut VASurfaceID,
    pub num_forward_references: u32,
    pub backward_references: *mut VASurfaceID,
    pub num_backward_references: u32,
    pub rotation_state: u32,
    pub blend_state: *const c_void,
    pub mirror_state: u32,
    pub additional_outputs: *mut VASurfaceID,
    pub num_additional_outputs: u32,
    pub input_surface_flag: u32,
    pub output_surface_flag: u32,
    pub input_color_properties: VAProcColorProperties,
    pub output_color_properties: VAProcColorProperties,
    pub processing_mode: c_int,
    pub output_hdr_metadata: *mut c_void,
    pub va_reserved: [u32; PROC_RESERVED],
}

pub const VA_BLEND_PREMULTIPLIED_ALPHA: u32 = 0x0002;
pub const VA_CHROMA_SITING_VERTICAL_CENTER: u8 = 0x02;
pub const VA_CHROMA_SITING_HORIZONTAL_CENTER: u8 = 0x08;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VABlendState {
    pub flags: u32,
    pub global_alpha: f32,
    pub min_luma: f32,
    pub max_luma: f32,
}

pub const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 0x4000_0000;
pub const VA_SURFACE_ATTRIB_SETTABLE: u32 = 0x0000_0002;
pub const VASurfaceAttribPixelFormat: c_int = 1;
pub const VASurfaceAttribMemoryType: c_int = 6;
pub const VASurfaceAttribExternalBufferDescriptor: c_int = 7;
pub const VAGenericValueTypeInteger: c_int = 1;
pub const VAGenericValueTypePointer: c_int = 3;
pub const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
pub const DRM_FORMAT_P010: u32 = u32::from_le_bytes(*b"P010");
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub union VAGenericValueUnion {
    pub i: c_int,
    pub f: f32,
    pub p: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VAGenericValue {
    pub type_: c_int,
    pub value: VAGenericValueUnion,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VASurfaceAttrib {
    pub type_: c_int,
    pub flags: u32,
    pub value: VAGenericValue,
}

pub const VA_EXPORT_SURFACE_READ_ONLY: u32 = 0x0001;
pub const VA_EXPORT_SURFACE_SEPARATE_LAYERS: u32 = 0x0004;
pub const VA_EXPORT_SURFACE_COMPOSED_LAYERS: u32 = 0x0008;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VADRMPRIMEObject {
    pub fd: c_int,
    pub size: u32,
    pub drm_format_modifier: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VADRMPRIMELayer {
    pub drm_format: u32,
    pub num_planes: u32,
    pub object_index: [u32; 4],
    pub offset: [u32; 4],
    pub pitch: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VADRMPRIMESurfaceDescriptor {
    pub fourcc: u32,
    pub width: u32,
    pub height: u32,
    pub num_objects: u32,
    pub objects: [VADRMPRIMEObject; 4],
    pub num_layers: u32,
    pub layers: [VADRMPRIMELayer; 4],
}

pub type vaExportSurfaceHandle = unsafe extern "C" fn(
    dpy: VADisplay,
    surface: VASurfaceID,
    mem_type: u32,
    flags: u32,
    descriptor: *mut c_void,
) -> VAStatus;
