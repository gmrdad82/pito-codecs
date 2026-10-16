use std::path::Path;

use super::demux::Demux;
use crate::gpu::Color;
use crate::media::{Codec, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoLook {
    pub sar: (u16, u16),
    pub color: Color,
}

fn vp9_matrix(space: u8) -> u8 {
    match space {
        1 => 5,
        2 => 1,
        3 => 6,
        4 => 7,
        5 => 9,
        7 => 0,
        _ => 2,
    }
}

pub fn video_look(path: &Path) -> Result<VideoLook> {
    let d = Demux::open(path)?;
    let track = d.video()?;
    let s = d.stream(track)?;
    let (sar, mut color) = s.look.unwrap_or(((1, 1), Color::UNKNOWN));
    if s.look.is_none()
        && let Some((matrix, full_range)) = s.color
    {
        color.matrix = matrix;
        color.full_range = full_range;
    }
    if s.codec == Some(Codec::Vp9) && !color.described() {
        let mut buf = Vec::new();
        for i in 0..d.samples(track).min(8) {
            if d.read_into(track, i, &mut buf).is_err() {
                break;
            }
            if let Some(k) = crate::vp9::keyframe_info(&buf) {
                color.matrix = vp9_matrix(k.color.color_space);
                color.full_range = k.color.full_range;
                break;
            }
        }
    }
    Ok(VideoLook { sar, color })
}
