use std::path::Path;

use super::demux::Demux;
use crate::gpu::Color;
use crate::media::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoLook {
    pub sar: (u16, u16),
    pub color: Color,
}

pub fn video_look(path: &Path) -> Result<VideoLook> {
    let d = Demux::open(path)?;
    let s = d.stream(d.video()?)?;
    let (sar, mut color) = s.look.unwrap_or(((1, 1), Color::UNKNOWN));
    if s.look.is_none()
        && let Some((matrix, full_range)) = s.color
    {
        color.matrix = matrix;
        color.full_range = full_range;
    }
    let sar = if sar.0 == 0 || sar.1 == 0 {
        (1, 1)
    } else {
        sar
    };
    Ok(VideoLook { sar, color })
}
