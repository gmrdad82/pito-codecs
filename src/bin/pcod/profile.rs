use super::json::Json;
use super::manifest;

pub const FAST_QP: u8 = 25;
pub const BALANCED_CRF: u8 = 20;
pub const BEST_CRF: u8 = 18;
pub const LIGHT_WEB_CRF: u8 = 23;
pub const VP9_BALANCED_CRF: u8 = 32;
pub const VP9_BEST_CRF: u8 = 28;
pub const VP9_LIGHT_WEB_CRF: u8 = 40;
pub const ANIMATED_WIDTH: u32 = 800;
pub const ANIMATED_FPS: u32 = 12;
pub const GIF_MAX_FPS: u32 = 50;
pub const GIF_BALANCED_TOLERANCE: u8 = 12;
pub const GIF_BEST_TOLERANCE: u8 = 4;
pub const WEBP_QUALITY: u8 = 75;
pub const WEBP_BALANCED_EFFORT: u8 = 4;
pub const WEBP_BEST_EFFORT: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Fast,
    Balanced,
    Best,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Balanced,
    Quality,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Encoder {
    H264 { preset: Preset, crf: u8 },
    H264Gpu { qp: u8 },
    Vp9 { crf: u8 },
    Gif { tolerance: u8 },
    Webp { quality: u8, effort: u8 },
    Copy { codec: String },
}

impl Profile {
    pub const ALL: [Profile; 3] = [Profile::Fast, Profile::Balanced, Profile::Best];

    pub fn name(self) -> &'static str {
        match self {
            Profile::Fast => "fast",
            Profile::Balanced => "balanced",
            Profile::Best => "best",
        }
    }

    pub fn encoder(self, quality: Option<u8>) -> Encoder {
        match self {
            Profile::Fast => Encoder::H264Gpu {
                qp: quality.unwrap_or(FAST_QP),
            },
            Profile::Balanced => Encoder::H264 {
                preset: Preset::Balanced,
                crf: quality.unwrap_or(BALANCED_CRF),
            },
            Profile::Best => Encoder::H264 {
                preset: Preset::Quality,
                crf: quality.unwrap_or(BEST_CRF),
            },
        }
    }

    pub fn vp9_encoder(self, quality: Option<u8>) -> Option<Encoder> {
        match self {
            Profile::Fast => None,
            Profile::Balanced => Some(Encoder::Vp9 {
                crf: quality.unwrap_or(VP9_BALANCED_CRF),
            }),
            Profile::Best => Some(Encoder::Vp9 {
                crf: quality.unwrap_or(VP9_BEST_CRF),
            }),
        }
    }

    pub fn gif_encoder(self) -> Option<Encoder> {
        match self {
            Profile::Fast => None,
            Profile::Balanced => Some(Encoder::Gif {
                tolerance: GIF_BALANCED_TOLERANCE,
            }),
            Profile::Best => Some(Encoder::Gif {
                tolerance: GIF_BEST_TOLERANCE,
            }),
        }
    }

    pub fn webp_encoder(self) -> Option<Encoder> {
        let effort = match self {
            Profile::Fast => return None,
            Profile::Balanced => WEBP_BALANCED_EFFORT,
            Profile::Best => WEBP_BEST_EFFORT,
        };
        Some(Encoder::Webp {
            quality: WEBP_QUALITY,
            effort,
        })
    }
}

impl Preset {
    pub fn name(self) -> &'static str {
        match self {
            Preset::Balanced => "balanced",
            Preset::Quality => "quality",
        }
    }
}

impl Encoder {
    pub fn quality(&self) -> Option<manifest::Quality> {
        match self {
            Encoder::H264 { crf, .. } | Encoder::Vp9 { crf } => {
                Some(manifest::Quality::Crf(u32::from(*crf)))
            }
            Encoder::H264Gpu { qp } => Some(manifest::Quality::Qp(u32::from(*qp))),
            Encoder::Webp { quality, .. } => Some(manifest::Quality::Webp(u32::from(*quality))),
            _ => None,
        }
    }

    pub fn manifest(&self) -> manifest::Encoder {
        let (encoder, codec, settings): (&str, Option<String>, Vec<(&'static str, Json)>) =
            match self {
                Encoder::H264 { preset, crf } => (
                    "h264",
                    None,
                    vec![("preset", preset.name().into()), ("crf", (*crf).into())],
                ),
                Encoder::H264Gpu { qp } => ("h264_gpu", None, vec![("qp", (*qp).into())]),
                Encoder::Vp9 { crf } => ("vp9", None, vec![("crf", (*crf).into())]),
                Encoder::Gif { tolerance } => {
                    ("gif", None, vec![("tolerance", (*tolerance).into())])
                }
                Encoder::Webp { quality, effort } => (
                    "webp",
                    None,
                    vec![
                        ("quality", (*quality).into()),
                        ("compression_level", (*effort).into()),
                    ],
                ),
                Encoder::Copy { codec } => ("copy", Some(codec.clone()), Vec::new()),
            };
        manifest::Encoder {
            encoder: encoder.into(),
            codec,
            settings,
        }
    }
}
