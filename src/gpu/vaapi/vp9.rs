use std::collections::VecDeque;

use super::ffi::{self, VAProfile, VASurfaceID};
use super::va::Session;
use super::{Accel, LAG, Picture};
use crate::gpu::biplanar::Geometry;
use crate::gpu::{Backend, DecoderBackend, GpuFrame};
use crate::media::{Error, Frame, Packet, Result};
use crate::vp9::decoder::segment_quant;
use crate::vp9::header::{
    FrameHeader, HeaderState, KEY_FRAME, RefInfo, parse_uncompressed, split_superframe,
};
use crate::vp9::loopfilter::LfFrame;
use crate::vp9::tables::{ALTREF_FRAME, GOLDEN_FRAME, LAST_FRAME, SEG_LVL_REF_FRAME, SEG_LVL_SKIP};

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PictureParams {
    pub frame_width: u16,
    pub frame_height: u16,
    pub reference_frames: [VASurfaceID; 8],
    pub pic_fields: u32,
    pub filter_level: u8,
    pub sharpness_level: u8,
    pub log2_tile_rows: u8,
    pub log2_tile_columns: u8,
    pub frame_header_length_in_bytes: u8,
    pub first_partition_size: u16,
    pub mb_segment_tree_probs: [u8; 7],
    pub segment_pred_probs: [u8; 3],
    pub profile: u8,
    pub bit_depth: u8,
    pub va_reserved: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SegmentParams {
    pub segment_flags: u16,
    pub filter_level: [[u8; 2]; 4],
    pub luma_ac_quant_scale: i16,
    pub luma_dc_quant_scale: i16,
    pub chroma_ac_quant_scale: i16,
    pub chroma_dc_quant_scale: i16,
    pub va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SliceParams {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub seg_param: [SegmentParams; 8],
    pub va_reserved: [u32; 4],
}

pub struct Vp9Decoder {
    session: Option<Session>,
    size: (u32, u32),
    bits: u8,
    profile: u8,
    st: HeaderState,
    slots: [Option<(usize, RefInfo)>; 8],
    queue: VecDeque<(usize, i64)>,
    name: String,
    gpu: bool,
}

fn backend(e: impl std::fmt::Display) -> Error {
    Error::Backend(e.to_string())
}

impl Vp9Decoder {
    pub fn new() -> Result<Self> {
        let display = super::va::probe().map_err(Error::Backend)?;
        let name = format!("VA-API VP9 ({})", display.vendor());
        super::check_profile(crate::media::Codec::Vp9, crate::gpu::Direction::Decode)?;
        Ok(Self {
            session: None,
            size: (0, 0),
            bits: 8,
            profile: 0,
            st: HeaderState::default(),
            slots: [None; 8],
            queue: VecDeque::new(),
            name,
            gpu: false,
        })
    }

    fn open(&mut self, h: &FrameHeader) -> Result<()> {
        let c = &h.color;
        if !(c.subsampling_x && c.subsampling_y) || !matches!(c.bit_depth, 8 | 10) {
            return Err(Error::Unsupported(format!(
                "VP9 profile {} at {}-bit on the GPU",
                h.profile, c.bit_depth
            )));
        }
        let profiles: &[VAProfile] = match h.profile {
            0 => &[ffi::VAProfileVP9Profile0],
            2 => &[ffi::VAProfileVP9Profile2],
            p => return Err(Error::Unsupported(format!("VP9 profile {p} on the GPU"))),
        };
        self.drain_all()?;
        self.session = None;
        self.slots = [None; 8];
        let size = (h.width, h.height);
        let geometry = Geometry {
            crop: (0, 0, h.width, h.height),
            bits: c.bit_depth as u32,
        };
        let session = Session::new(profiles, size, 8 + 2 + LAG, geometry).map_err(backend)?;
        self.name = format!("VA-API VP9 ({})", session.vendor());
        self.session = Some(session);
        self.size = size;
        self.bits = c.bit_depth;
        self.profile = h.profile;
        Ok(())
    }

    fn free_surface(&self) -> Result<usize> {
        let s = self
            .session
            .as_ref()
            .ok_or_else(|| Error::Invalid("VP9 stream does not start with a key frame".into()))?;
        (0..s.surfaces().len())
            .find(|&i| {
                !s.held(i)
                    && !self.slots.iter().flatten().any(|&(j, _)| j == i)
                    && !self.queue.iter().any(|&(j, _)| j == i)
            })
            .ok_or_else(|| Error::Backend("no free VA-API surface for VP9".into()))
    }

    fn read(&mut self, index: usize, pts: i64) -> Result<Picture> {
        let s = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Invalid("no VA-API session".into()))?;
        s.gpu = self.gpu;
        Ok(s.read(index).map_err(backend)?.at(pts))
    }

    fn drain(&mut self, keep: usize) -> Result<Vec<Picture>> {
        let mut out = Vec::new();
        while self.queue.len() > keep {
            let Some((index, pts)) = self.queue.pop_front() else {
                break;
            };
            out.push(self.read(index, pts)?);
        }
        Ok(out)
    }

    fn drain_all(&mut self) -> Result<Vec<Picture>> {
        self.drain(0)
    }

    fn decode_frame(&mut self, data: &[u8], pts: i64, out: &mut Vec<Picture>) -> Result<()> {
        let refs: [Option<RefInfo>; 8] = std::array::from_fn(|i| self.slots[i].map(|s| s.1));
        let h = parse_uncompressed(data, &mut self.st, &refs).map_err(Error::from)?;
        if h.show_existing_frame {
            let (index, _) = self.slots[h.frame_to_show_map_idx as usize]
                .ok_or_else(|| Error::Invalid("VP9 show_existing_frame of an empty slot".into()))?;
            out.extend(self.drain_all()?);
            out.push(self.read(index, pts)?);
            return Ok(());
        }
        let fresh = h.frame_type == KEY_FRAME || h.intra_only;
        if self.session.is_none()
            || (fresh
                && ((h.width, h.height) != self.size
                    || h.color.bit_depth != self.bits
                    || h.profile != self.profile))
        {
            if !fresh {
                return Err(Error::Invalid(
                    "VP9 stream does not start with a key frame".into(),
                ));
            }
            out.extend(self.drain_all()?);
            self.open(&h)?;
        }
        if (h.width, h.height) != self.size {
            return Err(Error::Unsupported(
                "VP9 reference scaling on the GPU".into(),
            ));
        }
        let target = self.free_surface()?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Error::Invalid("no VA-API session".into()))?;
        let ids = session.surfaces().to_vec();
        let mut pic = PictureParams {
            frame_width: h.width as u16,
            frame_height: h.height as u16,
            reference_frames: std::array::from_fn(|i| {
                self.slots[i].map_or(ffi::VA_INVALID_SURFACE, |(j, _)| ids[j])
            }),
            ..Default::default()
        };
        let seg = &self.st.seg;
        let bias = |r: i8| h.ref_frame_sign_bias[r as usize] as u32;
        let fields = [
            (h.color.subsampling_x as u32, 0),
            (h.color.subsampling_y as u32, 1),
            ((h.frame_type != KEY_FRAME) as u32, 2),
            (h.show_frame as u32, 3),
            (h.error_resilient_mode as u32, 4),
            (h.intra_only as u32, 5),
            ((!h.frame_is_intra && h.allow_high_precision_mv) as u32, 6),
            (h.interp_filter as u32 & 7, 7),
            (h.frame_parallel_decoding_mode as u32, 10),
            (h.reset_frame_context as u32 & 3, 11),
            (h.refresh_frame_context as u32, 13),
            (h.frame_context_idx as u32 & 3, 14),
            (seg.enabled as u32, 16),
            (seg.temporal_update as u32, 17),
            (seg.update_map as u32, 18),
            (h.ref_frame_idx[0] as u32 & 7, 19),
            (bias(LAST_FRAME), 22),
            (h.ref_frame_idx[1] as u32 & 7, 23),
            (bias(GOLDEN_FRAME), 26),
            (h.ref_frame_idx[2] as u32 & 7, 27),
            (bias(ALTREF_FRAME), 30),
            (h.lossless as u32, 31),
        ];
        pic.pic_fields = fields.iter().fold(0, |a, &(v, s)| a | (v << s));
        pic.filter_level = self.st.lf.level;
        pic.sharpness_level = self.st.lf.sharpness;
        pic.log2_tile_rows = h.tile_rows_log2 as u8;
        pic.log2_tile_columns = h.tile_cols_log2 as u8;
        pic.frame_header_length_in_bytes = h.uncompressed_size as u8;
        pic.first_partition_size = h.header_size_in_bytes as u16;
        pic.mb_segment_tree_probs = seg.tree_probs;
        pic.segment_pred_probs = if seg.temporal_update {
            seg.pred_probs
        } else {
            [255; 3]
        };
        pic.profile = h.profile;
        pic.bit_depth = h.color.bit_depth;
        let lf = LfFrame::new(
            &self.st.lf,
            seg,
            h.mi_rows as usize,
            h.mi_cols as usize,
            h.color.subsampling_x,
            h.color.subsampling_y,
            h.color.bit_depth,
        );
        let q = segment_quant(&h, seg);
        let mut slice = SliceParams {
            slice_data_size: data.len() as u32,
            ..Default::default()
        };
        for (s, p) in slice.seg_param.iter_mut().enumerate() {
            let flags = (seg.feature_active(s as u8, SEG_LVL_REF_FRAME) as u16)
                | ((seg.feature_data[s][SEG_LVL_REF_FRAME] as u16 & 3) << 1)
                | ((seg.feature_active(s as u8, SEG_LVL_SKIP) as u16) << 3);
            p.segment_flags = flags;
            p.filter_level = lf.lvl[s];
            p.luma_dc_quant_scale = q[s][0][0] as i16;
            p.luma_ac_quant_scale = q[s][0][1] as i16;
            p.chroma_dc_quant_scale = q[s][1][0] as i16;
            p.chroma_ac_quant_scale = q[s][1][1] as i16;
        }
        session
            .decode_picture(
                target,
                &pic,
                None::<&PictureParams>,
                &[(slice, data.to_vec())],
            )
            .map_err(backend)?;
        for i in 0..8 {
            if (h.refresh_frame_flags >> i) & 1 == 1 {
                self.slots[i] = Some((
                    target,
                    RefInfo {
                        width: h.width,
                        height: h.height,
                    },
                ));
            }
        }
        if h.show_frame {
            if let Some(s) = self.session.as_mut() {
                s.gpu = self.gpu;
                s.prefetch(target);
            }
            self.queue.push_back((target, pts));
            out.extend(self.drain(LAG)?);
        }
        Ok(())
    }

    pub fn decode(&mut self, packet: &Packet, gpu: bool) -> Result<Vec<Picture>> {
        self.gpu = gpu;
        let mut out = Vec::new();
        for f in split_superframe(&packet.data) {
            if !f.is_empty() {
                self.decode_frame(f, packet.pts, &mut out)?;
            }
        }
        Ok(out)
    }

    pub fn flush(&mut self, gpu: bool) -> Result<Vec<Picture>> {
        self.gpu = gpu;
        self.drain_all()
    }

    pub fn reset(&mut self) {
        self.queue.clear();
        self.slots = [None; 8];
        self.st = HeaderState::default();
    }
}

impl DecoderBackend for Vp9Decoder {
    fn backend(&self) -> Backend {
        Backend::VaApi
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        super::frames(Vp9Decoder::decode(self, packet, false)?)
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        super::frames(Vp9Decoder::flush(self, false)?)
    }

    fn reset(&mut self) {
        Vp9Decoder::reset(self)
    }

    fn decode_gpu(&mut self, packet: &Packet) -> Result<Vec<GpuFrame>> {
        Ok(super::gpu_frames(Vp9Decoder::decode(self, packet, true)?))
    }

    fn flush_gpu(&mut self) -> Result<Vec<GpuFrame>> {
        Ok(super::gpu_frames(Vp9Decoder::flush(self, true)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_match_va_dec_vp9_h() {
        assert_eq!(std::mem::size_of::<PictureParams>(), 92);
        assert_eq!(std::mem::offset_of!(PictureParams, pic_fields), 36);
        assert_eq!(
            std::mem::offset_of!(PictureParams, first_partition_size),
            46
        );
        assert_eq!(std::mem::offset_of!(PictureParams, profile), 58);
        assert_eq!(std::mem::size_of::<SegmentParams>(), 36);
        assert_eq!(std::mem::offset_of!(SegmentParams, luma_ac_quant_scale), 10);
        assert_eq!(std::mem::size_of::<SliceParams>(), 316);
    }
}
