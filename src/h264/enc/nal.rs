use crate::syntax::bits::{BitWriter, escape_rbsp};

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

pub fn nal(nal_ref_idc: u8, nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 2);
    out.push((nal_ref_idc << 5) | nal_type);
    out.extend_from_slice(&escape_rbsp(rbsp));
    out
}

#[derive(Clone, Debug)]
pub struct Vui {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub max_num_reorder_frames: u32,
    pub max_dec_frame_buffering: u32,
    pub color: crate::media::color::Color,
}

#[derive(Clone, Debug)]
pub struct Sps {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub width_mbs: u32,
    pub height_mbs: u32,
    pub crop_right: u32,
    pub crop_bottom: u32,
    pub log2_max_frame_num: u32,
    pub log2_max_poc_lsb: u32,
    pub max_num_ref_frames: u32,
    pub vui: Vui,
}

impl Sps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(self.profile_idc as u32, 8);
        w.write_bits(self.constraint_flags as u32, 8);
        w.write_bits(self.level_idc as u32, 8);
        w.write_ue(0);
        if matches!(
            self.profile_idc,
            100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
        ) {
            w.write_ue(1);
            w.write_ue(0);
            w.write_ue(0);
            w.write_bit(false);
            w.write_bit(false);
        }
        w.write_ue(self.log2_max_frame_num - 4);
        w.write_ue(0);
        w.write_ue(self.log2_max_poc_lsb - 4);
        w.write_ue(self.max_num_ref_frames);
        w.write_bit(false);
        w.write_ue(self.width_mbs - 1);
        w.write_ue(self.height_mbs - 1);
        w.write_bit(true);
        w.write_bit(true);
        let crop = self.crop_right > 0 || self.crop_bottom > 0;
        w.write_bit(crop);
        if crop {
            w.write_ue(0);
            w.write_ue(self.crop_right / 2);
            w.write_ue(0);
            w.write_ue(self.crop_bottom / 2);
        }
        w.write_bit(true);
        let v = &self.vui;
        w.write_bit(true);
        w.write_bits(1, 8);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bits(5, 3);
        w.write_bit(v.color.full_range);
        w.write_bit(true);
        w.write_bits(v.color.primaries as u32, 8);
        w.write_bits(v.color.transfer as u32, 8);
        w.write_bits(v.color.matrix as u32, 8);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bits(v.num_units_in_tick, 32);
        w.write_bits(v.time_scale, 32);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bit(true);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(11);
        w.write_ue(11);
        w.write_ue(v.max_num_reorder_frames);
        w.write_ue(v.max_dec_frame_buffering);
        w.rbsp_trailing();
        w.finish()
    }
}

#[derive(Clone, Debug)]
pub struct Pps {
    pub cabac: bool,
    pub pic_init_qp: i32,
    pub chroma_qp_offset: i32,
    pub transform_8x8: bool,
    pub high: bool,
}

impl Pps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(self.cabac);
        w.write_bit(false);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bits(0, 2);
        w.write_se(self.pic_init_qp - 26);
        w.write_se(0);
        w.write_se(self.chroma_qp_offset);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        if self.high {
            w.write_bit(self.transform_8x8);
            w.write_bit(false);
            w.write_se(self.chroma_qp_offset);
        }
        w.rbsp_trailing();
        w.finish()
    }
}

pub fn aud_rbsp(primary_pic_type: u8) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(primary_pic_type as u32, 3);
    w.rbsp_trailing();
    w.finish()
}

pub fn sei_user_data_rbsp(text: &str) -> Vec<u8> {
    const UUID: [u8; 16] = [
        0x7a, 0x3c, 0x1f, 0x52, 0xd4, 0x0b, 0x4e, 0x8e, 0x9d, 0x61, 0x2f, 0xc4, 0x93, 0x05, 0xb7,
        0x1a,
    ];
    let mut payload = UUID.to_vec();
    payload.extend_from_slice(text.as_bytes());
    let mut w = BitWriter::new();
    w.write_bits(5, 8);
    let mut size = payload.len();
    while size >= 255 {
        w.write_bits(255, 8);
        size -= 255;
    }
    w.write_bits(size as u32, 8);
    w.write_bytes(&payload);
    w.rbsp_trailing();
    w.finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliceType {
    P = 0,
    B = 1,
    I = 2,
}

#[derive(Clone, Debug)]
pub struct SliceHeader {
    pub slice_type: SliceType,
    pub nal_ref_idc: u8,
    pub idr: bool,
    pub idr_pic_id: u32,
    pub frame_num: u32,
    pub log2_max_frame_num: u32,
    pub poc_lsb: u32,
    pub log2_max_poc_lsb: u32,
    pub l0_modification: Option<u32>,
    pub cabac: bool,
    pub slice_qp: i32,
    pub pic_init_qp: i32,
    pub disable_deblocking: u32,
    pub alpha_offset_div2: i32,
    pub beta_offset_div2: i32,
}

impl SliceHeader {
    pub fn write(&self, w: &mut BitWriter) {
        w.write_ue(0);
        w.write_ue(self.slice_type as u32);
        w.write_ue(0);
        w.write_bits(self.frame_num, self.log2_max_frame_num);
        if self.idr {
            w.write_ue(self.idr_pic_id);
        }
        w.write_bits(self.poc_lsb, self.log2_max_poc_lsb);
        if self.slice_type == SliceType::B {
            w.write_bit(true);
        }
        if self.slice_type != SliceType::I {
            w.write_bit(false);
            match self.l0_modification {
                Some(d) => {
                    w.write_bit(true);
                    w.write_ue(0);
                    w.write_ue(d);
                    w.write_ue(3);
                }
                None => w.write_bit(false),
            }
            if self.slice_type == SliceType::B {
                w.write_bit(false);
            }
        }
        if self.nal_ref_idc != 0 {
            if self.idr {
                w.write_bit(false);
                w.write_bit(false);
            } else {
                w.write_bit(false);
            }
        }
        if self.cabac && self.slice_type != SliceType::I {
            w.write_ue(0);
        }
        w.write_se(self.slice_qp - self.pic_init_qp);
        w.write_ue(self.disable_deblocking);
        if self.disable_deblocking != 1 {
            w.write_se(self.alpha_offset_div2);
            w.write_se(self.beta_offset_div2);
        }
    }
}

pub fn avcc(sps_nal: &[u8], pps_nal: &[u8], profile_idc: u8, constraint: u8, level: u8) -> Vec<u8> {
    let mut v = vec![1, profile_idc, constraint, level, 0xFF, 0xE1];
    v.extend_from_slice(&(sps_nal.len() as u16).to_be_bytes());
    v.extend_from_slice(sps_nal);
    v.push(1);
    v.extend_from_slice(&(pps_nal.len() as u16).to_be_bytes());
    v.extend_from_slice(pps_nal);
    if matches!(profile_idc, 100 | 110 | 122 | 144) {
        v.push(0xFC | 1);
        v.push(0xF8);
        v.push(0xF8);
        v.push(0);
    }
    v
}

const LEVELS: [(u8, u64, u64, u64, u64); 17] = [
    (10, 1485, 99, 396, 64),
    (11, 3000, 396, 900, 192),
    (12, 6000, 396, 2376, 384),
    (13, 11880, 396, 2376, 768),
    (20, 11880, 396, 2376, 2000),
    (21, 19800, 792, 4752, 4000),
    (22, 20250, 1620, 8100, 4000),
    (30, 40500, 1620, 8100, 10000),
    (31, 108000, 3600, 18000, 14000),
    (32, 216000, 5120, 20480, 20000),
    (40, 245760, 8192, 32768, 20000),
    (41, 245760, 8192, 32768, 50000),
    (42, 522240, 8704, 34816, 50000),
    (50, 589824, 22080, 110400, 135000),
    (51, 983040, 36864, 184320, 240000),
    (52, 2073600, 36864, 184320, 240000),
    (62, 16711680, 139264, 696320, 800000),
];

fn br_factor(high: bool) -> u64 {
    if high { 1250 } else { 1000 }
}

pub fn pick_level(
    width_mbs: u32,
    height_mbs: u32,
    fps: f64,
    dpb_frames: u32,
    kbps: Option<u32>,
    high: bool,
) -> u8 {
    let fs = (width_mbs * height_mbs) as u64;
    let mbps = (fs as f64 * fps).ceil() as u64;
    for &(l, max_mbps, max_fs, max_dpb, max_br) in &LEVELS {
        let br_ok = kbps.is_none_or(|k| (k as u64) * 1000 <= max_br * br_factor(high));
        let dim_ok =
            (width_mbs as u64).pow(2) <= 8 * max_fs && (height_mbs as u64).pow(2) <= 8 * max_fs;
        if fs <= max_fs && mbps <= max_mbps && fs * dpb_frames as u64 <= max_dpb && br_ok && dim_ok
        {
            return l;
        }
    }
    62
}

pub fn max_kbps(high: bool) -> u32 {
    (LEVELS[LEVELS.len() - 1].4 * br_factor(high) / 1000) as u32
}
