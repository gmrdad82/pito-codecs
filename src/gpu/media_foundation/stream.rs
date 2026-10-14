use super::device::SurfaceFormat;
use crate::gpu::nal;
use crate::media::{Codec, Error, Result};
use crate::syntax::bits::unescape_rbsp;
use crate::syntax::{h264, hevc};

pub const MAX_SIDE: u32 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    pub format: SurfaceFormat,
    pub coded: (u32, u32),
    pub crop: (u32, u32, u32, u32),
}

fn sps_rbsp(codec: Codec, sets: &[Vec<u8>]) -> Option<Vec<u8>> {
    let header = if codec == Codec::Hevc { 2 } else { 1 };
    sets.iter()
        .rev()
        .find(|n| nal::is_parameter_set(codec, n) && is_sps(codec, n))
        .and_then(|n| Some(unescape_rbsp(n.get(header..)?)))
}

pub fn is_sps(codec: Codec, nal: &[u8]) -> bool {
    let t = nal::nal_type(codec, nal);
    match codec {
        Codec::Hevc => t == 33,
        _ => t == 7,
    }
}

fn unsupported(e: impl std::fmt::Display) -> Error {
    Error::Unsupported(e.to_string())
}

pub fn plan(codec: Codec, sets: &[Vec<u8>]) -> Result<Plan> {
    let rbsp = sps_rbsp(codec, sets)
        .ok_or_else(|| Error::Invalid(format!("no {codec:?} SPS in the parameter sets")))?;
    let p = match codec {
        Codec::H264 => {
            let sps = h264::params::Sps::parse(&rbsp).map_err(|e| Error::Invalid(e.to_string()))?;
            if !sps.frame_mbs_only {
                return Err(unsupported("interlaced H.264 on Media Foundation"));
            }
            if sps.chroma_format_idc != 1 || sps.bit_depth_luma != 8 || sps.bit_depth_chroma != 8 {
                return Err(unsupported(format!(
                    "H.264 chroma format {} at {}-bit on Media Foundation",
                    sps.chroma_format_idc, sps.bit_depth_luma
                )));
            }
            if !matches!(sps.profile_idc, 66 | 77 | 100) {
                return Err(unsupported(format!(
                    "H.264 profile_idc {} on Media Foundation",
                    sps.profile_idc
                )));
            }
            Plan {
                format: SurfaceFormat::Nv12,
                coded: (sps.width(), sps.height()),
                crop: sps.crop_rect(),
            }
        }
        Codec::Hevc => {
            let sps = hevc::params::Sps::parse(&rbsp).map_err(|e| Error::Invalid(e.to_string()))?;
            let bits = sps.bit_depth_luma;
            if sps.chroma_format_idc != 1 || sps.bit_depth_chroma != bits || sps.range_extension {
                return Err(unsupported(format!(
                    "HEVC chroma format {} at {bits}-bit on Media Foundation",
                    sps.chroma_format_idc
                )));
            }
            let format = match (sps.profile_idc, bits) {
                (1..=3, 8) => SurfaceFormat::Nv12,
                (2, 10) => SurfaceFormat::P010,
                (p, b) => {
                    return Err(unsupported(format!(
                        "HEVC profile {p} at {b}-bit on Media Foundation"
                    )));
                }
            };
            Plan {
                format,
                coded: (sps.width, sps.height),
                crop: sps.crop_rect(),
            }
        }
        Codec::Vp9 => return Err(unsupported("VP9 on Media Foundation")),
        Codec::Aac | Codec::Opus | Codec::Mp3 => {
            return Err(unsupported(format!("{codec:?} is not a video codec")));
        }
    };
    let (cx, cy, w, h) = p.crop;
    if w == 0
        || h == 0
        || w > MAX_SIDE
        || h > MAX_SIDE
        || cx.saturating_add(w) > p.coded.0
        || cy.saturating_add(h) > p.coded.1
    {
        return Err(unsupported(format!(
            "a {w}x{h} picture at {cx},{cy} in {}x{}",
            p.coded.0, p.coded.1
        )));
    }
    Ok(p)
}

pub fn merge_sets(codec: Codec, old: &[Vec<u8>], new: &[&[u8]]) -> Vec<Vec<u8>> {
    let kinds: Vec<u8> = new.iter().map(|n| nal::nal_type(codec, n)).collect();
    let mut sets: Vec<Vec<u8>> = old
        .iter()
        .filter(|n| !kinds.contains(&nal::nal_type(codec, n)))
        .cloned()
        .chain(new.iter().map(|n| n.to_vec()))
        .collect();
    sets.sort_by_key(|n| nal::nal_type(codec, n));
    sets
}

pub fn annexb(nals: &[&[u8]], headers: Option<&[Vec<u8>]>, out: &mut Vec<u8>) {
    out.clear();
    let heads = headers.into_iter().flatten().map(Vec::as_slice);
    nal::put_start_codes(heads.chain(nals.iter().copied()), out);
}
