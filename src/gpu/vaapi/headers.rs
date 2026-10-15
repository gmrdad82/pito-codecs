use crate::gpu::Color;
use crate::syntax::bits::{BitWriter, escape_rbsp};

pub const LOG2_MAX_FRAME_NUM: u32 = 16;
pub const LOG2_MAX_POC_LSB_H264: u32 = 16;
pub const LOG2_MAX_POC_LSB_HEVC: u32 = 12;

pub struct Packed {
    pub bytes: Vec<u8>,
    pub bits: u32,
}

pub fn nal(header: &[u8], w: BitWriter) -> Packed {
    let tail_bits = w.bit_len() % 8;
    let mut rbsp = header.to_vec();
    rbsp.extend_from_slice(&w.finish());
    let mut bytes = vec![0, 0, 0, 1];
    bytes.extend_from_slice(&escape_rbsp(&rbsp));
    let mut bits = bytes.len() as u32 * 8;
    if tail_bits != 0 {
        bits -= 8 - tail_bits as u32;
    }
    Packed { bytes, bits }
}

pub fn join(parts: Vec<Packed>) -> Packed {
    let mut bytes = Vec::new();
    for p in parts {
        bytes.extend_from_slice(&p.bytes);
    }
    let bits = bytes.len() as u32 * 8;
    Packed { bytes, bits }
}

fn look(w: &mut BitWriter, sar: (u16, u16), color: Color) {
    let aspect = sar.0 != 0 && sar.1 != 0 && sar.0 != sar.1;
    w.write_bit(aspect);
    if aspect {
        w.write_bits(255, 8);
        w.write_bits(u32::from(sar.0), 16);
        w.write_bits(u32::from(sar.1), 16);
    }
    w.write_bit(false);
    let described = color.described();
    let signalled = described || color.full_range;
    w.write_bit(signalled);
    if signalled {
        w.write_bits(5, 3);
        w.write_bit(color.full_range);
        w.write_bit(described);
        if described {
            w.write_bits(u32::from(color.primaries), 8);
            w.write_bits(u32::from(color.transfer), 8);
            w.write_bits(u32::from(color.matrix), 8);
        }
    }
}

pub struct H264Sequence {
    pub profile_idc: u8,
    pub constraints: u8,
    pub level_idc: u8,
    pub width_mbs: u32,
    pub height_mbs: u32,
    pub crop_right: u32,
    pub crop_bottom: u32,
    pub max_refs: u32,
    pub reorder: u32,
    pub cabac: bool,
    pub transform_8x8: bool,
    pub sar: (u16, u16),
    pub color: Color,
}

pub fn h264_sps(s: &H264Sequence) -> Packed {
    let mut w = BitWriter::new();
    w.write_bits(u32::from(s.profile_idc), 8);
    w.write_bits(u32::from(s.constraints), 8);
    w.write_bits(u32::from(s.level_idc), 8);
    w.write_ue(0);
    if s.profile_idc == 100 {
        w.write_ue(1);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
    }
    w.write_ue(LOG2_MAX_FRAME_NUM - 4);
    w.write_ue(0);
    w.write_ue(LOG2_MAX_POC_LSB_H264 - 4);
    w.write_ue(s.max_refs);
    w.write_bit(false);
    w.write_ue(s.width_mbs - 1);
    w.write_ue(s.height_mbs - 1);
    w.write_bit(true);
    w.write_bit(true);
    let crop = s.crop_right > 0 || s.crop_bottom > 0;
    w.write_bit(crop);
    if crop {
        w.write_ue(0);
        w.write_ue(s.crop_right);
        w.write_ue(0);
        w.write_ue(s.crop_bottom);
    }
    w.write_bit(true);
    look(&mut w, s.sar, s.color);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.write_bit(true);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(15);
    w.write_ue(15);
    w.write_ue(s.reorder);
    w.write_ue(s.max_refs);
    w.rbsp_trailing();
    nal(&[0x67], w)
}

pub fn h264_pps(s: &H264Sequence) -> Packed {
    let mut w = BitWriter::new();
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(s.cabac);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bits(0, 2);
    w.write_se(0);
    w.write_se(0);
    w.write_se(0);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    if s.profile_idc == 100 {
        w.write_bit(s.transform_8x8);
        w.write_bit(false);
        w.write_se(0);
    }
    w.rbsp_trailing();
    nal(&[0x68], w)
}

pub struct H264Slice {
    pub idr: bool,
    pub reference: bool,
    pub slice_type: u32,
    pub frame_num: u32,
    pub idr_pic_id: u32,
    pub poc_lsb: u32,
    pub cabac: bool,
    pub qp_delta: i32,
}

pub fn h264_slice(s: &H264Slice) -> Packed {
    let mut w = BitWriter::new();
    w.write_ue(0);
    w.write_ue(s.slice_type);
    w.write_ue(0);
    w.write_bits(s.frame_num, LOG2_MAX_FRAME_NUM);
    if s.idr {
        w.write_ue(s.idr_pic_id);
    }
    w.write_bits(s.poc_lsb, LOG2_MAX_POC_LSB_H264);
    let (p, b) = (s.slice_type == 0, s.slice_type == 1);
    if b {
        w.write_bit(true);
    }
    if p || b {
        w.write_bit(false);
        w.write_bit(false);
        if b {
            w.write_bit(false);
        }
    }
    if s.reference {
        if s.idr {
            w.write_bit(false);
            w.write_bit(false);
        } else {
            w.write_bit(false);
        }
    }
    if s.cabac && (p || b) {
        w.write_ue(0);
    }
    w.write_se(s.qp_delta);
    w.write_ue(0);
    w.write_se(0);
    w.write_se(0);
    let ref_idc = if s.idr {
        3
    } else if s.reference {
        2
    } else {
        0
    };
    let kind = if s.idr { 5 } else { 1 };
    nal(&[(ref_idc << 5) | kind], w)
}

pub struct HevcSequence {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub width: u32,
    pub height: u32,
    pub crop_right: u32,
    pub crop_bottom: u32,
    pub bit_depth: u32,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub depth_inter: u32,
    pub depth_intra: u32,
    pub amp: bool,
    pub sao: bool,
    pub temporal_mvp: bool,
    pub strong_intra: bool,
    pub cu_qp_delta: bool,
    pub fps_num: u32,
    pub fps_den: u32,
    pub sar: (u16, u16),
    pub color: Color,
}

fn profile_tier_level(w: &mut BitWriter, s: &HevcSequence) {
    w.write_bits(0, 2);
    w.write_bit(false);
    w.write_bits(u32::from(s.profile_idc), 5);
    let mut compat = 1u32 << (31 - u32::from(s.profile_idc));
    if s.profile_idc == 1 {
        compat |= 1 << (31 - 2);
    }
    w.write_bits(compat, 32);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.write_bits(0, 32);
    w.write_bits(0, 12);
    w.write_bits(u32::from(s.level_idc), 8);
}

pub fn hevc_vps(s: &HevcSequence) -> Packed {
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bit(true);
    w.write_bit(true);
    w.write_bits(0, 6);
    w.write_bits(0, 3);
    w.write_bit(true);
    w.write_bits(0xffff, 16);
    profile_tier_level(&mut w, s);
    w.write_bit(false);
    w.write_ue(1);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bits(0, 6);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(false);
    w.rbsp_trailing();
    nal(&[32 << 1, 1], w)
}

pub fn hevc_sps(s: &HevcSequence) -> Packed {
    let mut w = BitWriter::new();
    w.write_bits(0, 4);
    w.write_bits(0, 3);
    w.write_bit(true);
    profile_tier_level(&mut w, s);
    w.write_ue(0);
    w.write_ue(1);
    w.write_ue(s.width);
    w.write_ue(s.height);
    let crop = s.crop_right > 0 || s.crop_bottom > 0;
    w.write_bit(crop);
    if crop {
        w.write_ue(0);
        w.write_ue(s.crop_right);
        w.write_ue(0);
        w.write_ue(s.crop_bottom);
    }
    w.write_ue(s.bit_depth - 8);
    w.write_ue(s.bit_depth - 8);
    w.write_ue(LOG2_MAX_POC_LSB_HEVC - 4);
    w.write_bit(false);
    w.write_ue(1);
    w.write_ue(0);
    w.write_ue(0);
    w.write_ue(s.log2_min_cb - 3);
    w.write_ue(s.log2_ctb - s.log2_min_cb);
    w.write_ue(s.log2_min_tb - 2);
    w.write_ue(s.log2_max_tb - s.log2_min_tb);
    w.write_ue(s.depth_inter);
    w.write_ue(s.depth_intra);
    w.write_bit(false);
    w.write_bit(s.amp);
    w.write_bit(s.sao);
    w.write_bit(false);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(s.temporal_mvp);
    w.write_bit(s.strong_intra);
    w.write_bit(true);
    look(&mut w, s.sar, s.color);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.write_bits(s.fps_den, 32);
    w.write_bits(s.fps_num, 32);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.rbsp_trailing();
    nal(&[33 << 1, 1], w)
}

pub fn hevc_pps(s: &HevcSequence) -> Packed {
    let mut w = BitWriter::new();
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bits(0, 3);
    w.write_bit(false);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(0);
    w.write_se(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(s.cu_qp_delta);
    if s.cu_qp_delta {
        w.write_ue(0);
    }
    w.write_se(0);
    w.write_se(0);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(true);
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bit(false);
    w.rbsp_trailing();
    nal(&[34 << 1, 1], w)
}

pub struct HevcSlice {
    pub idr: bool,
    pub poc: u32,
    pub ref_delta: u32,
    pub sao: bool,
    pub temporal_mvp: bool,
    pub qp_delta: i32,
}

pub fn hevc_slice(s: &HevcSlice) -> Packed {
    let mut w = BitWriter::new();
    w.write_bit(true);
    if s.idr {
        w.write_bit(false);
    }
    w.write_ue(0);
    w.write_ue(if s.idr { 2 } else { 1 });
    if !s.idr {
        w.write_bits(
            s.poc & ((1 << LOG2_MAX_POC_LSB_HEVC) - 1),
            LOG2_MAX_POC_LSB_HEVC,
        );
        w.write_bit(false);
        w.write_ue(1);
        w.write_ue(0);
        w.write_ue(s.ref_delta.saturating_sub(1));
        w.write_bit(true);
        if s.temporal_mvp {
            w.write_bit(true);
        }
    }
    if s.sao {
        w.write_bit(true);
        w.write_bit(true);
    }
    if !s.idr {
        w.write_bit(false);
        w.write_ue(0);
    }
    w.write_se(s.qp_delta);
    w.write_bit(true);
    w.write_bit(true);
    w.align_zero();
    let kind: u8 = if s.idr { 19 } else { 1 };
    nal(&[kind << 1, 1], w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::bits::unescape_rbsp;

    fn rbsp(p: &Packed, header: usize) -> Vec<u8> {
        unescape_rbsp(&p.bytes[4 + header..])
    }

    #[test]
    fn the_sps_carries_the_sources_aspect_and_colour() {
        let color = Color {
            primaries: 6,
            transfer: 6,
            matrix: 6,
            full_range: true,
        };
        for (sar, color) in [((4, 3), color), ((1, 1), Color::UNKNOWN)] {
            let h264 = H264Sequence {
                profile_idc: 100,
                constraints: 0,
                level_idc: 40,
                width_mbs: 120,
                height_mbs: 68,
                crop_right: 0,
                crop_bottom: 4,
                max_refs: 1,
                reorder: 0,
                cabac: true,
                transform_8x8: true,
                sar,
                color,
            };
            let sps = crate::syntax::h264::params::Sps::parse(&rbsp(&h264_sps(&h264), 1)).unwrap();
            let vui = sps.vui.unwrap();
            assert_eq!(vui.sar, if sar == (1, 1) { (0, 0) } else { sar });
            assert_eq!(
                (
                    vui.colour_primaries,
                    vui.transfer_characteristics,
                    vui.matrix_coefficients,
                    vui.full_range
                ),
                (
                    color.primaries,
                    color.transfer,
                    color.matrix,
                    color.full_range
                )
            );
            let hevc = HevcSequence {
                profile_idc: 1,
                level_idc: 120,
                width: 1920,
                height: 1088,
                crop_right: 0,
                crop_bottom: 4,
                bit_depth: 8,
                log2_min_cb: 3,
                log2_ctb: 5,
                log2_min_tb: 2,
                log2_max_tb: 5,
                depth_inter: 2,
                depth_intra: 2,
                amp: true,
                sao: true,
                temporal_mvp: true,
                strong_intra: true,
                cu_qp_delta: false,
                fps_num: 60000,
                fps_den: 1001,
                sar,
                color,
            };
            let sps = crate::syntax::hevc::params::Sps::parse(&rbsp(&hevc_sps(&hevc), 2)).unwrap();
            let vui = sps.vui.unwrap();
            assert_eq!(vui.sar, if sar == (1, 1) { (0, 0) } else { sar });
            assert_eq!(
                (
                    vui.colour_primaries,
                    vui.transfer_characteristics,
                    vui.matrix_coefficients,
                    vui.full_range
                ),
                (
                    color.primaries,
                    color.transfer,
                    color.matrix,
                    color.full_range
                )
            );
            assert_eq!(vui.timing, Some((1001, 60000)));
        }
    }
}
