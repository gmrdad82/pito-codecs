use std::path::Path;

use pito_codecs::gpu::Color;
use pito_codecs::media::wav::{self, Layout};
use pito_codecs::media::{Chroma, Prefs, Probe, VideoLook, probe, video_look};

use super::json::Json;
use super::{Fail, about, picture};

pub fn probe_json(path: &Path) -> Result<String, Fail> {
    if let Some(facts) = picture::probe(path).map_err(|e| about(path, e))? {
        return Ok(picture::json(&facts).text());
    }
    let p = probe(path, &Prefs::default()).map_err(|e| about(path, e))?;
    let video = p.width > 0 && p.height > 0;
    if !video && p.audio.is_empty() {
        return Err(about(path, "the file has no video or audio stream"));
    }
    let look = if video { video_look(path).ok() } else { None };
    let pcm = wav::Reader::open(path).ok().map(|r| r.layout);
    Ok(media(&p, look, video, pcm).text())
}

fn when<T: Into<Json>>(shown: bool, value: T) -> Json {
    if shown { value.into() } else { Json::Null }
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

fn media(p: &Probe, look: Option<VideoLook>, video: bool, pcm: Option<Layout>) -> Json {
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
        ("width", when(video, p.width)),
        ("height", when(video, p.height)),
        (
            "fps",
            (video && fps.is_finite() && fps > 0.0)
                .then_some(fps)
                .into(),
        ),
        (
            "seconds",
            (p.duration.is_finite() && p.duration >= 0.0)
                .then_some(p.duration)
                .into(),
        ),
        ("codec", when(video, p.video_codec_name.as_str())),
        ("pix_fmt", when(video, pix_fmt(p))),
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
            when(video, (360 - u32::from(p.rotation) % 360) % 360),
        ),
        ("sar", sar),
        ("video_index", when(video, video_index)),
        (
            "profile",
            Some(p.profile.as_str())
                .filter(|s| video && !s.is_empty())
                .into(),
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
        ("audio_bits", pcm.map(|l| l.bits).into()),
        ("audio_float", pcm.map(|l| l.float()).into()),
        ("audio_extradata", Json::Null),
        ("alpha", p.alpha.into()),
        (
            "chroma",
            p.chroma.filter(|_| video).map(Chroma::name).into(),
        ),
        ("interlaced", when(video, p.interlaced)),
        ("variant", when(video, p.video_variant())),
        ("frames", p.frames.filter(|_| video).into()),
    ])
}

fn pix_fmt(p: &Probe) -> String {
    let bits = match p.bit_depth {
        0..=8 => String::new(),
        d => format!("{d}le"),
    };
    let chroma = match p.chroma {
        Some(Chroma::Mono) if !p.alpha => return format!("gray{bits}"),
        Some(Chroma::Yuv420) => "420",
        Some(Chroma::Yuv422) => "422",
        Some(Chroma::Yuv440) => "440",
        Some(Chroma::Yuv444) => "444",
        _ if p.profile.contains("4:4:4") => "444",
        _ if p.profile.contains("4:2:2") => "422",
        _ => "420",
    };
    let a = if p.alpha { "a" } else { "" };
    format!("yuv{a}{chroma}p{bits}")
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
