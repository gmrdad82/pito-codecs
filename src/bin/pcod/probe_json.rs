use std::path::Path;

use pito_codecs::media::{Prefs, Probe, probe};

use super::json::Json;
use super::{Fail, about};

pub fn probe_json(path: &Path) -> Result<String, Fail> {
    let p = probe(path, &Prefs::default()).map_err(|e| about(path, e))?;
    if p.width == 0 || p.height == 0 {
        return Err(about(path, "the video stream has no width and height"));
    }
    Ok(media(&p).text())
}

fn media(p: &Probe) -> Json {
    let fps = p.fps();
    let audio = p.audio.first();
    let video_index = (0..)
        .find(|i| !p.audio.iter().any(|a| a.stream == *i))
        .unwrap_or(0);
    Json::Obj(vec![
        ("width", p.width.into()),
        ("height", p.height.into()),
        ("fps", (fps.is_finite() && fps > 0.0).then_some(fps).into()),
        (
            "seconds",
            (p.duration.is_finite() && p.duration >= 0.0)
                .then_some(p.duration)
                .into(),
        ),
        ("codec", p.video_codec_name.as_str().into()),
        ("pix_fmt", pix_fmt(p).into()),
        ("audio", audio.map(|a| a.codec.as_str()).into()),
        ("color_space", Json::Null),
        ("color_range", Json::Null),
        ("color_primaries", Json::Null),
        ("color_transfer", Json::Null),
        (
            "rotation",
            ((360 - u32::from(p.rotation) % 360) % 360).into(),
        ),
        ("sar", Json::Null),
        ("video_index", video_index.into()),
        (
            "profile",
            Some(p.profile.as_str()).filter(|s| !s.is_empty()).into(),
        ),
        ("extradata", Json::Null),
        ("audio_index", audio.map(|a| a.stream).into()),
        (
            "audio_rate",
            audio.map(|a| a.rate).filter(|r| *r > 0).into(),
        ),
        (
            "audio_channels",
            audio.map(|a| a.channels).filter(|c| *c > 0).into(),
        ),
        (
            "audio_layout",
            audio.and_then(|a| layout(a.channels)).into(),
        ),
        ("audio_extradata", Json::Null),
    ])
}

fn pix_fmt(p: &Probe) -> String {
    let chroma = if p.profile.contains("4:4:4") {
        "444"
    } else if p.profile.contains("4:2:2") {
        "422"
    } else {
        "420"
    };
    match p.bit_depth {
        0..=8 => format!("yuv{chroma}p"),
        depth => format!("yuv{chroma}p{depth}le"),
    }
}

fn layout(channels: u16) -> Option<&'static str> {
    match channels {
        1 => Some("mono"),
        2 => Some("stereo"),
        6 => Some("5.1"),
        8 => Some("7.1"),
        _ => None,
    }
}
