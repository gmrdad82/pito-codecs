use std::path::{Path, PathBuf};

use pito_codecs::gpu::Color;
use pito_codecs::media::{self, Prefs};
use pito_codecs::png;

use super::Fail;

#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    pub codec: String,
    pub index: usize,
    pub stream: usize,
    pub rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    File,
    Frames(Vec<PathBuf>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    pub path: PathBuf,
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub seconds: Option<f64>,
    pub frames: Option<u64>,
    pub codec: String,
    pub bit_depth: u8,
    pub alpha: bool,
    pub color: Color,
    pub rotation: u16,
    pub sar: (u16, u16),
    pub audio: Option<Sound>,
}

impl Media {
    pub fn anamorphic(&self) -> bool {
        self.sar.0 != self.sar.1 && self.sar.0 > 0 && self.sar.1 > 0
    }

    pub fn display_size(&self) -> (u32, u32) {
        let width = if self.anamorphic() {
            let scaled = u128::from(self.width) * u128::from(self.sar.0);
            let pairs = (scaled + u128::from(self.sar.1)) / (2 * u128::from(self.sar.1));
            u32::try_from(2 * pairs).unwrap_or(u32::MAX - 1)
        } else {
            self.width
        };
        if self.rotation % 180 == 90 {
            (self.height, width)
        } else {
            (width, self.height)
        }
    }

    pub fn fps_value(&self) -> f64 {
        self.fps.0 as f64 / self.fps.1.max(1) as f64
    }
}

pub fn listed(dir: &Path) -> Result<Vec<PathBuf>, Fail> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry?.path();
        let is_png = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if is_png && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(format!("{}: no .png files", dir.display()).into());
    }
    Ok(files)
}

pub fn frames(dir: &Path, fps: Option<(u32, u32)>) -> Result<Media, Fail> {
    let files = listed(dir)?;
    let first = std::fs::read(&files[0]).map_err(|e| format!("{}: {e}", files[0].display()))?;
    let info = png::info(&first).map_err(|e| format!("{}: {e}", files[0].display()))?;
    let rate = fps.unwrap_or((30, 1));
    let count = files.len() as u64;
    Ok(Media {
        path: dir.to_path_buf(),
        width: info.width,
        height: info.height,
        fps: rate,
        seconds: Some(count as f64 * f64::from(rate.1) / f64::from(rate.0.max(1))),
        frames: Some(count),
        codec: "png".into(),
        bit_depth: info.depth.max(8),
        alpha: matches!(info.color, png::Color::Rgba | png::Color::GreyAlpha),
        color: Color {
            primaries: 1,
            transfer: 13,
            matrix: 0,
            full_range: true,
        },
        rotation: 0,
        sar: (1, 1),
        audio: None,
        kind: Kind::Frames(files),
    })
}

pub fn file(path: &Path) -> Result<Media, Fail> {
    let at = |e: &dyn std::fmt::Display| -> Fail {
        let shown = path.display().to_string();
        let text = e.to_string();
        if text.contains(&shown) {
            text.into()
        } else {
            format!("{shown}: {text}").into()
        }
    };
    let p = media::probe(path, &Prefs::default()).map_err(|e| at(&e))?;
    if p.width == 0 || p.height == 0 {
        return Err(at(&"the video stream has no width and height"));
    }
    let look = media::video_look(path).ok();
    let audio = p.audio.first().map(|a| Sound {
        codec: a.codec.to_ascii_lowercase(),
        index: a.index,
        stream: a.stream,
        rate: a.rate,
        channels: a.channels,
    });
    Ok(Media {
        path: path.to_path_buf(),
        kind: Kind::File,
        width: p.width,
        height: p.height,
        fps: p.fps,
        seconds: (p.duration.is_finite() && p.duration > 0.0).then_some(p.duration),
        frames: p.frames,
        codec: p.video_codec_name.to_ascii_lowercase(),
        bit_depth: p.bit_depth.max(8),
        alpha: p.alpha,
        color: look.map_or(Color::UNKNOWN, |l| l.color),
        rotation: p.rotation % 360,
        sar: look.map_or((1, 1), |l| l.sar),
        audio,
    })
}

pub fn open(path: &Path, fps: Option<(u32, u32)>) -> Result<Media, Fail> {
    if path.is_dir() {
        frames(path, fps)
    } else {
        file(path)
    }
}
