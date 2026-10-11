use super::ffi::*;
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::process::Command;

type Layout = (&'static str, usize, Vec<(&'static str, usize)>);

macro_rules! layouts {
    ($($t:ident { $($f:ident),* $(,)? })*) => {
        vec![$((stringify!($t), size_of::<$t>(), vec![$((stringify!($f), offset_of!($t, $f))),*])),*]
    };
}

#[test]
fn ffi_layouts_match_the_libva_headers() {
    if !Path::new("/usr/include/va/va.h").exists() {
        eprintln!("skipped: no libva headers");
        return;
    }
    let layouts: Vec<Layout> = layouts! {
        VAConfigAttrib { value }
        VAPictureH264 { picture_id, frame_idx, flags, TopFieldOrderCnt, va_reserved }
        VAPictureParameterBufferH264 { ReferenceFrames, picture_width_in_mbs_minus1, num_ref_frames, seq_fields, pic_init_qp_minus26, pic_fields, frame_num, va_reserved }
        VAIQMatrixBufferH264 { ScalingList8x8, va_reserved }
        VASliceParameterBufferH264 { slice_data_bit_offset, slice_type, RefPicList0, RefPicList1, luma_log2_weight_denom, luma_weight_l0, chroma_weight_l0_flag, chroma_offset_l1, va_reserved }
        VAPictureHEVC { pic_order_cnt, flags }
        VAPictureParameterBufferHEVC { ReferenceFrames, pic_width_in_luma_samples, pic_fields, column_width_minus1, row_height_minus1, slice_parsing_fields, st_rps_bits, va_reserved }
        VASliceParameterBufferHEVC { slice_data_byte_offset, RefPicList, LongSliceFlags, delta_luma_weight_l0, ChromaOffsetL1, five_minus_max_num_merge_cand, num_entry_point_offsets, slice_data_num_emu_prevn_bytes, va_reserved }
        VAIQMatrixBufferHEVC { ScalingList16x16, ScalingListDC16x16, va_reserved }
        VAImageFormat { bits_per_pixel, va_reserved }
        VAImage { format, buf, width, data_size, pitches, offsets, va_reserved }
        VAEncSequenceParameterBufferH264 { intra_period, picture_width_in_mbs, seq_fields, bit_depth_luma_minus8, offset_for_non_ref_pic, offset_for_ref_frame, frame_cropping_flag, frame_crop_left_offset, vui_parameters_present_flag, vui_fields, aspect_ratio_idc, sar_width, time_scale, va_reserved }
        VAEncPictureParameterBufferH264 { ReferenceFrames, coded_buf, pic_parameter_set_id, last_picture, frame_num, pic_init_qp, chroma_qp_index_offset, pic_fields, va_reserved }
        VAEncSliceParameterBufferH264 { num_macroblocks, macroblock_info, slice_type, idr_pic_id, pic_order_cnt_lsb, delta_pic_order_cnt_bottom, direct_spatial_mv_pred_flag, RefPicList0, luma_log2_weight_denom, luma_weight_l0, chroma_offset_l1, cabac_init_idc, slice_qp_delta, slice_beta_offset_div2, va_reserved }
        VAEncSequenceParameterBufferHEVC { general_tier_flag, intra_period, bits_per_second, pic_width_in_luma_samples, seq_fields, log2_min_luma_coding_block_size_minus3, max_transform_hierarchy_depth_intra, pcm_sample_bit_depth_luma_minus1, vui_parameters_present_flag, vui_fields, aspect_ratio_idc, vui_time_scale, min_spatial_segmentation_idc, scc_fields, va_reserved }
        VAEncPictureParameterBufferHEVC { reference_frames, coded_buf, collocated_ref_pic_index, column_width_minus1, row_height_minus1, slice_pic_parameter_set_id, nal_unit_type, pic_fields, hierarchical_level_plus1, scc_fields, va_reserved }
        VAEncSliceParameterBufferHEVC { slice_type, ref_pic_list0, ref_pic_list1, luma_log2_weight_denom, chroma_offset_l1, max_num_merge_cand, slice_tc_offset_div2, slice_fields, pred_weight_table_bit_length, va_reserved }
        VAEncMiscParameterRateControl { rc_flags, target_frame_size, va_reserved }
        VAEncMiscParameterFrameRate { framerate_flags, va_reserved }
        VAEncMiscParameterHRD { buffer_size, va_reserved }
        VACodedBufferSegment { status, buf, next, va_reserved }
        VAEncPackedHeaderParameterBuffer { bit_length, has_emulation_bytes, va_reserved }
        VARectangle { y, width, height }
        VADRMPRIMESurfaceDescriptor { width, num_objects, objects, num_layers, layers }
        VAProcPipelineParameterBuffer { surface_region, surface_color_standard, output_region, output_background_color, filters, num_filters, forward_references, rotation_state, blend_state, additional_outputs, input_surface_flag, input_color_properties, output_color_properties, processing_mode, output_hdr_metadata, va_reserved }
    };
    let mut c = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include <va/va.h>\n#include <va/va_dec_hevc.h>\n#include <va/va_enc_h264.h>\n#include <va/va_enc_hevc.h>\n#include <va/va_vpp.h>\n#include <va/va_drmcommon.h>\nint main(void){\n",
    );
    let mut expected = String::new();
    for (t, size, fields) in &layouts {
        c.push_str(&format!("printf(\"{t} %zu\\n\", sizeof({t}));\n"));
        expected.push_str(&format!("{t} {size}\n"));
        for (f, off) in fields {
            c.push_str(&format!(
                "printf(\"{t}.{f} %zu\\n\", offsetof({t}, {f}));\n"
            ));
            expected.push_str(&format!("{t}.{f} {off}\n"));
        }
    }
    c.push_str("return 0;}\n");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/abi");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("layouts.c");
    let bin = dir.join("layouts");
    std::fs::write(&src, c).unwrap();
    let Ok(built) = Command::new("cc").arg(&src).arg("-o").arg(&bin).status() else {
        eprintln!("skipped: no C compiler");
        return;
    };
    assert!(built.success(), "the layout program did not build");
    let out = Command::new(&bin).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
}
