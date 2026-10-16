use std::path::Path;

use pito_codecs::gpu::Color;
use pito_codecs::media::{Prefs, Probe, VideoLook, probe, video_look};

use super::json::Json;
use super::{Fail, about};

pub fn probe_json(path: &Path) -> Result<String, Fail> {
    let p = probe(path, &Prefs::default()).map_err(|e| about(path, e))?;
    if p.width == 0 || p.height == 0 {
        return Err(about(path, "the video stream has no width and height"));
    }
    Ok(media(&p, video_look(path).ok()).text())
}

fn matrix(code: u8) -> Option<&'static str> {
    Some(match code {
        0 => "gbr",
        1 => "bt709",
        4 => "fcc",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        9 => "bt2020nc",
        10 => "bt2020c",
        _ => return None,
    })
}

fn primaries(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "bt709",
        4 => "bt470m",
        5 => "bt470bg",
        6 => "smpte170m",
        7 => "smpte240m",
        9 => "bt2020",
        12 => "smpte432",
        _ => return None,
    })
}

fn transfer(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "bt709",
        4 => "gamma22",
        5 => "gamma28",
        6 => "smpte170m",
        7 => "smpte240m",
        8 => "linear",
        13 => "iec61966-2-1",
        14 => "bt2020-10",
        15 => "bt2020-12",
        16 => "smpte2084",
        18 => "arib-std-b67",
        _ => return None,
    })
}

fn range(color: &Color) -> Option<&'static str> {
    if color.full_range {
        Some("pc")
    } else if color.described() {
        Some("tv")
    } else {
        None
    }
}

fn media(p: &Probe, look: Option<VideoLook>) -> Json {
    let color = look.map(|l| l.color);
    let sar = look
        .map(|l| l.sar)
        .filter(|(n, d)| n != d && *n > 0 && *d > 0)
        .map_or(Json::Null, |(n, d)| Json::Arr(vec![n.into(), d.into()]));
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
        ("color_space", color.and_then(|c| matrix(c.matrix)).into()),
        ("color_range", color.as_ref().and_then(range).into()),
        (
            "color_primaries",
            color.and_then(|c| primaries(c.primaries)).into(),
        ),
        (
            "color_transfer",
            color.and_then(|c| transfer(c.transfer)).into(),
        ),
        (
            "rotation",
            ((360 - u32::from(p.rotation) % 360) % 360).into(),
        ),
        ("sar", sar),
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
        ("alpha", p.alpha.into()),
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
    let a = if p.alpha { "a" } else { "" };
    match p.bit_depth {
        0..=8 => format!("yuv{a}{chroma}p"),
        depth => format!("yuv{a}{chroma}p{depth}le"),
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
