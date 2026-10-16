use std::path::{Path, PathBuf};

use super::profile::{Encoder, GIF_MAX_FPS, Profile};
use super::size::Size;

pub const AAC_KBPS: u32 = 320;
pub const OPUS_KBPS: u32 = 128;
pub const AUDIO_KBPS_LOWEST: u32 = 32;
pub const AUDIO_KBPS_HIGHEST: u32 = 512;
pub const SAMPLE_RATE: u32 = 48_000;
pub const TUNES: [&str; 6] = [
    "film",
    "animation",
    "grain",
    "stillimage",
    "fastdecode",
    "zerolatency",
];

pub const YOUTUBE: f64 = -14.0;
pub const YOUTUBE_PEAK: f64 = -2.0;
pub const TRUE_PEAK: f64 = -1.0;
pub const QUIETEST: f64 = -70.0;
pub const LOUDEST: f64 = -5.0;
pub const PEAK_LOWEST: f64 = -9.0;
pub const PEAK_HIGHEST: f64 = 0.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Loudness {
    pub integrated: f64,
    pub true_peak: f64,
}

impl Loudness {
    pub fn parse(text: &str) -> Result<Loudness, String> {
        if text == "youtube" {
            return Ok(Loudness {
                integrated: YOUTUBE,
                true_peak: YOUTUBE_PEAK,
            });
        }
        text.parse()
            .ok()
            .filter(|v: &f64| v.is_finite())
            .map(|integrated| Loudness {
                integrated,
                true_peak: TRUE_PEAK,
            })
            .ok_or_else(|| format!("use a LUFS value such as {YOUTUBE}, or youtube"))
    }

    pub fn check(&self) -> Result<(), String> {
        if !(QUIETEST..=LOUDEST).contains(&self.integrated) {
            return Err(format!(
                "loudness {} LUFS is outside {QUIETEST} to {LOUDEST}",
                self.integrated
            ));
        }
        if !(PEAK_LOWEST..=PEAK_HIGHEST).contains(&self.true_peak) {
            return Err(format!(
                "true peak {} dBTP is outside {PEAK_LOWEST} to {PEAK_HIGHEST}",
                self.true_peak
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mov,
    Mkv,
    Webm,
    Gif,
    Webp,
}

impl Container {
    pub fn animated(self) -> bool {
        matches!(self, Container::Gif | Container::Webp)
    }

    pub fn carries(self, codec: &str) -> bool {
        let codecs: &[&str] = match self {
            Container::Mp4 | Container::Mov => &["h264", "hevc"],
            Container::Mkv => &["h264", "hevc", "vp9"],
            Container::Webm => &["vp9"],
            Container::Gif | Container::Webp => &[],
        };
        codecs.contains(&codec)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub input: PathBuf,
    pub output: PathBuf,
    pub profile: Profile,
    pub size: Option<Size>,
    pub fps: Option<u32>,
    pub quality: Option<u8>,
    pub tune: Option<String>,
    pub overwrite: bool,
    pub loudness: Option<Loudness>,
    pub audio_bitrate: Option<u32>,
    pub copy_video: bool,
    pub joins: Vec<PathBuf>,
}

impl Request {
    pub fn new(input: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Request {
        Request {
            input: input.into(),
            output: output.into(),
            profile: Profile::Balanced,
            size: None,
            fps: None,
            quality: None,
            tune: None,
            overwrite: false,
            loudness: None,
            audio_bitrate: None,
            copy_video: false,
            joins: Vec::new(),
        }
    }

    pub fn settings(&self) -> Result<(Container, Encoder), String> {
        let extension = self
            .output
            .extension()
            .and_then(|part| part.to_str())
            .unwrap_or("");
        let container = match extension.to_ascii_lowercase().as_str() {
            "mp4" => Container::Mp4,
            "mov" => Container::Mov,
            "mkv" => Container::Mkv,
            "webm" => Container::Webm,
            "gif" => Container::Gif,
            "webp" => Container::Webp,
            _ => {
                return Err(format!(
                    "unsupported output extension {extension:?}: use mp4, mov, mkv, webm, gif or webp"
                ));
            }
        };
        if let Some(fps) = self.fps {
            if !(1..=240).contains(&fps) {
                return Err(format!("fps {fps} is outside 1–240"));
            }
            if container == Container::Gif && fps > GIF_MAX_FPS {
                return Err(format!("a GIF plays at most {GIF_MAX_FPS} fps, not {fps}"));
            }
        }
        let fast_cannot =
            |format: &str| format!("fast cannot write {format}: use balanced or best");
        let encoder = match container {
            Container::Webm => self
                .profile
                .vp9_encoder(self.quality)
                .ok_or_else(|| fast_cannot("WebM"))?,
            Container::Gif => self
                .profile
                .gif_encoder()
                .ok_or_else(|| fast_cannot("GIF"))?,
            Container::Webp => self
                .profile
                .webp_encoder()
                .ok_or_else(|| fast_cannot("WebP"))?,
            Container::Mp4 | Container::Mov | Container::Mkv => self.profile.encoder(self.quality),
        };
        let extension = extension.to_ascii_lowercase();
        if container.animated() && !self.joins.is_empty() {
            return Err(format!(
                "join writes mp4, mov, mkv or webm, not .{extension}: join first, then make the .{extension} from the joined file"
            ));
        }
        if container.animated() && self.quality.is_some() {
            return Err(format!("--crf and --qp don't apply to .{extension}"));
        }
        if let Some(tune) = &self.tune {
            match encoder {
                Encoder::Vp9 { .. } | Encoder::Gif { .. } | Encoder::Webp { .. } => {
                    return Err(format!(
                        "tune is an H.264 setting and cannot be used with .{extension}"
                    ));
                }
                Encoder::H264Gpu { .. } => {
                    return Err("tune is an H.264 setting: use it with balanced or best".into());
                }
                Encoder::H264 { .. } if !TUNES.contains(&tune.as_str()) => {
                    return Err(format!("unknown tune {tune:?}: use {}", TUNES.join(", ")));
                }
                Encoder::H264 { .. } | Encoder::Copy { .. } => {}
            }
        }
        if let Some(loudness) = &self.loudness {
            loudness.check()?;
            if container.animated() {
                return Err(format!(
                    "--loudness doesn't apply to .{extension}, which carries no audio"
                ));
            }
        }
        if let Some(kbps) = self.audio_bitrate {
            if !(AUDIO_KBPS_LOWEST..=AUDIO_KBPS_HIGHEST).contains(&kbps) {
                return Err(format!(
                    "audio bitrate {kbps} kbps is outside {AUDIO_KBPS_LOWEST}–{AUDIO_KBPS_HIGHEST}"
                ));
            }
            if container.animated() {
                return Err(format!(
                    "--audio-bitrate doesn't apply to .{extension}, which carries no audio"
                ));
            }
        }
        if self.copy_video && (self.size.is_some() || self.fps.is_some()) {
            return Err(
                "a copied video keeps its size and rate, so it takes no --size or --fps".into(),
            );
        }
        if let Some(quality) = self.quality {
            let (minimum, maximum) = if matches!(encoder, Encoder::Vp9 { .. }) {
                (0, 63)
            } else {
                (1, 51)
            };
            if !(minimum..=maximum).contains(&quality) {
                return Err(format!("quality {quality} is outside {minimum}–{maximum}"));
            }
        }
        Ok((container, encoder))
    }
}

pub fn same_file(input: &Path, output: &Path) -> bool {
    if input == output
        || matches!((input.canonicalize(), output.canonicalize()), (Ok(a), Ok(b)) if a == b)
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (input.metadata(), output.metadata()) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    false
}

pub fn partial(output: &Path) -> Result<PathBuf, String> {
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "output needs a UTF-8 file name".to_string())?;
    let token = std::hash::BuildHasher::hash_one(
        &std::hash::RandomState::new(),
        (std::process::id(), std::time::SystemTime::now()),
    );
    let extension = output.extension().and_then(|e| e.to_str()).unwrap_or("out");
    Ok(output.with_file_name(format!(
        ".{name}.pcod-{}-{:08x}.partial.{extension}",
        std::process::id(),
        token as u32
    )))
}
