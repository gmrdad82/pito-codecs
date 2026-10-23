use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use pito_codecs::filter::{ColorSpec, Image, ImageMut, Kernel, Light, Scaler, Transfer};
use pito_codecs::media::{PixelFormat, Rgba};
use pito_codecs::{jpeg, png, webp};

use super::Fail;
use super::json::Json;
use super::manifest;
use super::plan::{partial, same_file};
use super::size::Size;

pub const DEFAULT_QUALITY: u8 = 75;
pub const LOSSY_EFFORT: u8 = 4;
pub const LOSSLESS_EFFORT: u8 = 6;
const GIF_TICKS: f64 = 100.0;
const WEBP_TICKS: f64 = 1000.0;
const WEBP_ZERO_DELAY: u32 = 100;
const WEBP_MAX_SIDE: u32 = 16383;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl Format {
    pub fn sniff(head: &[u8]) -> Option<Format> {
        if head.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
            Some(Format::Png)
        } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Format::Jpeg)
        } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
            Some(Format::Webp)
        } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            Some(Format::Gif)
        } else {
            None
        }
    }

    pub fn of(path: &Path) -> Option<Format> {
        let mut head = [0u8; 12];
        let mut file = File::open(path).ok()?;
        let mut filled = 0;
        while filled < head.len() {
            match file.read(&mut head[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        Format::sniff(&head[..filled])
    }

    pub fn codec(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "mjpeg",
            Format::Webp => "webp",
            Format::Gif => "gif",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Facts {
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub alpha: bool,
    pub interlaced: bool,
    pub pix_fmt: Option<&'static str>,
    pub frames: u32,
    pub seconds: Option<f64>,
    pub fps: Option<f64>,
}

pub fn still_input(path: &Path) -> bool {
    path.is_file()
        && matches!(
            Format::of(path),
            Some(Format::Png | Format::Webp | Format::Jpeg)
        )
}

pub fn probe(path: &Path) -> Result<Option<Facts>, String> {
    let Some(format) = Format::of(path) else {
        return Ok(None);
    };
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    facts(format, &data).map(Some)
}

pub fn facts(format: Format, data: &[u8]) -> Result<Facts, String> {
    let still = |width, height, alpha, pix_fmt| Facts {
        format,
        width,
        height,
        alpha,
        interlaced: false,
        pix_fmt,
        frames: 1,
        seconds: None,
        fps: None,
    };
    let timed = |still: Facts, delays: &[u32], ticks: f64, fps: Option<f64>| Facts {
        frames: delays.len() as u32,
        seconds: Some(delays.iter().map(|&d| f64::from(d)).sum::<f64>() / ticks),
        fps,
        ..still
    };
    match format {
        Format::Png => {
            let info = png::info(data).map_err(|e| e.to_string())?;
            let alpha =
                matches!(info.color, png::Color::Rgba | png::Color::GreyAlpha) || has_trns(data);
            Ok(Facts {
                interlaced: info.interlaced,
                ..still(info.width, info.height, alpha, png_pix_fmt(&info))
            })
        }
        Format::Jpeg => {
            let (width, height, pix_fmt) = jpeg_size(data)?;
            Ok(still(width, height, false, pix_fmt))
        }
        Format::Webp => {
            let info = webp::info(data).map_err(|e| e.to_string())?;
            let pix_fmt = Some(match (info.lossless, info.alpha) {
                (true, _) => "argb",
                (false, true) => "yuva420p",
                (false, false) => "yuv420p",
            });
            let first = still(info.width, info.height, info.alpha, pix_fmt);
            if !info.animated {
                return Ok(first);
            }
            let delays = webp_delays(data)?;
            let fps = rate(&delays, WEBP_TICKS);
            Ok(timed(first, &delays, WEBP_TICKS, fps))
        }
        Format::Gif => {
            let info = pito_codecs::media::scan_gif(data).map_err(|e| e.to_string())?;
            let delays: Vec<u32> = info.delays_cs.iter().map(|&d| u32::from(d)).collect();
            let (num, den) = info.fps();
            Ok(timed(
                still(info.width, info.height, info.transparent, Some("bgra")),
                &delays,
                GIF_TICKS,
                Some(f64::from(num) / f64::from(den)),
            ))
        }
    }
}

fn png_pix_fmt(info: &png::Info) -> Option<&'static str> {
    let wide = info.depth == 16;
    Some(match info.color {
        png::Color::Grey if info.depth == 1 => "monob",
        png::Color::Grey if wide => "gray16be",
        png::Color::Grey => "gray",
        png::Color::Rgb if wide => "rgb48be",
        png::Color::Rgb => "rgb24",
        png::Color::Palette => "pal8",
        png::Color::GreyAlpha if wide => "ya16be",
        png::Color::GreyAlpha => "ya8",
        png::Color::Rgba if wide => "rgba64be",
        png::Color::Rgba => "rgba",
    })
}

pub fn rate(delays: &[u32], ticks: f64) -> Option<f64> {
    let first = *delays.first()?;
    let total: u64 = delays.iter().map(|&d| u64::from(d)).sum();
    if total == 0 {
        return None;
    }
    if delays.iter().all(|&d| d == first) {
        return Some(ticks / f64::from(first));
    }
    let mean = delays.len() as f64 * ticks / total as f64;
    let whole = mean.round();
    if whole > 0.0 && (mean - whole).abs() <= mean * 0.01 {
        Some(whole)
    } else {
        Some(mean)
    }
}

fn has_trns(data: &[u8]) -> bool {
    let mut at = 8;
    while let Some(head) = data.get(at..at + 8) {
        let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        match &head[4..8] {
            b"tRNS" => return true,
            b"IDAT" | b"IEND" => return false,
            _ => {}
        }
        at = at.saturating_add(12).saturating_add(len);
    }
    false
}

pub fn jpeg_size(data: &[u8]) -> Result<(u32, u32, Option<&'static str>), String> {
    let info = jpeg::decode::info(data).map_err(|e| e.to_string())?;
    let pix_fmt = match info.sampling.as_slice() {
        _ if info.precision != 8 => None,
        [_] => Some("gray"),
        [y, cb, cr] if cb == cr && y.0 % cb.0 == 0 && y.1 % cb.1 == 0 => {
            match (y.0 / cb.0, y.1 / cb.1) {
                (1, 1) => Some("yuvj444p"),
                (2, 1) => Some("yuvj422p"),
                (2, 2) => Some("yuvj420p"),
                (1, 2) => Some("yuvj440p"),
                (4, 1) => Some("yuvj411p"),
                _ => None,
            }
        }
        _ => None,
    };
    Ok((info.width, info.height, pix_fmt))
}

pub fn webp_delays(data: &[u8]) -> Result<Vec<u32>, String> {
    let mut at = 12;
    let mut delays = Vec::new();
    while let Some(head) = data.get(at..at + 8) {
        let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
        if &head[..4] == b"ANMF" {
            let body = data
                .get(at + 8..at + 24)
                .ok_or("WebP: the file ends inside a frame")?;
            let delay = u32::from(body[12]) | u32::from(body[13]) << 8 | u32::from(body[14]) << 16;
            delays.push(if delay == 0 { WEBP_ZERO_DELAY } else { delay });
        }
        at = at.saturating_add(8).saturating_add(len + (len & 1));
    }
    if delays.is_empty() {
        return Err("WebP: an animation without frames".into());
    }
    Ok(delays)
}

pub fn json(f: &Facts) -> Json {
    Json::Obj(vec![
        ("width", f.width.into()),
        ("height", f.height.into()),
        ("fps", f.fps.into()),
        ("seconds", f.seconds.into()),
        ("codec", f.format.codec().into()),
        ("pix_fmt", f.pix_fmt.into()),
        ("audio", Json::Null),
        ("color_space", Json::Null),
        ("color_range", Json::Null),
        ("color_primaries", Json::Null),
        ("color_transfer", Json::Null),
        ("rotation", 0u32.into()),
        ("sar", Json::Null),
        ("video_index", 0u32.into()),
        ("profile", Json::Null),
        ("extradata", Json::Null),
        ("audio_index", Json::Null),
        ("audio_rate", Json::Null),
        ("audio_channels", Json::Null),
        ("audio_layout", Json::Null),
        ("audio_bits", Json::Null),
        ("audio_float", Json::Null),
        ("audio_extradata", Json::Null),
        ("alpha", f.alpha.into()),
        ("chroma", Json::Null),
        ("interlaced", f.interlaced.into()),
        ("variant", Json::Null),
        ("frames", f.frames.into()),
    ])
}

pub fn lines(f: &Facts) -> Vec<String> {
    let mut out = vec![format!("container  {}", f.format.codec().to_uppercase())];
    if let Some(s) = f.seconds {
        out.push(format!("duration   {s:.1} s"));
    }
    let mut video = format!("video      {} {}x{}", f.format.codec(), f.width, f.height);
    if let Some(fps) = f.fps {
        video.push_str(&format!(" {fps:.2} fps, {} frames", f.frames));
    }
    out.push(video);
    if f.alpha {
        out.push("alpha      yes".into());
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Still {
    pub quality: Option<u8>,
    pub lossless: bool,
}

impl Still {
    pub fn options(self) -> webp::Options {
        if self.lossless {
            webp::Options {
                effort: LOSSLESS_EFFORT,
                ..webp::Options::lossless()
            }
        } else {
            webp::Options {
                effort: LOSSY_EFFORT,
                ..webp::Options::lossy(f32::from(self.quality.unwrap_or(DEFAULT_QUALITY)))
            }
        }
    }
}

pub struct Job<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub size: Option<Size>,
    pub still: Still,
    pub overwrite: bool,
    pub json: bool,
}

pub fn load(path: &Path) -> Result<Rgba, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    match Format::sniff(&data) {
        Some(Format::Png) => png::decode(&data).map_err(|e| e.to_string()),
        Some(Format::Webp) => {
            let info = webp::info(&data).map_err(|e| e.to_string())?;
            if info.animated {
                return Err("an animated WebP is not a still image".into());
            }
            webp::decode(&data).map_err(|e| e.to_string())
        }
        Some(Format::Jpeg) => {
            let orientation = jpeg::decode::info(&data)
                .map_err(|e| e.to_string())?
                .orientation;
            let image = jpeg::decode(&data).map_err(|e| e.to_string())?;
            Ok(orient(image, orientation))
        }
        _ => Err("not a PNG, JPEG or WebP still image".into()),
    }
}

pub fn orient(image: Rgba, orientation: u8) -> Rgba {
    let (w, h) = (image.width as usize, image.height as usize);
    if !(2..=8).contains(&orientation) || image.data.len() < w * h * 4 {
        return image;
    }
    let turned = orientation >= 5;
    let (ow, oh) = if turned { (h, w) } else { (w, h) };
    let mut out = Rgba::new(ow as u32, oh as u32);
    for (i, px) in image.data.chunks_exact(4).take(w * h).enumerate() {
        let (x, y) = (i % w, i / w);
        let (ox, oy) = match orientation {
            2 => (w - 1 - x, y),
            3 => (w - 1 - x, h - 1 - y),
            4 => (x, h - 1 - y),
            5 => (y, x),
            6 => (h - 1 - y, x),
            7 => (h - 1 - y, w - 1 - x),
            _ => (y, w - 1 - x),
        };
        let at = (oy * ow + ox) * 4;
        out.data[at..at + 4].copy_from_slice(px);
    }
    out
}

pub fn resize(image: Rgba, width: u32, height: u32) -> Result<Rgba, String> {
    if (image.width, image.height) == (width, height) {
        return Ok(image);
    }
    let scaler = Scaler::new(
        image.width,
        image.height,
        width,
        height,
        PixelFormat::Yuv420p8,
        Kernel::Lanczos,
    )
    .map_err(|e| e.to_string())?
    .light(Light::Linear)
    .transfer(Transfer::Srgb)
    .color(ColorSpec::BT709, ColorSpec::BT709)
    .output(PixelFormat::Yuv420p8);
    let mut out = Rgba::new(width, height);
    scaler
        .convert(Image::Rgba(&image), ImageMut::Rgba(&mut out))
        .map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn encode(
    image: Rgba,
    size: Option<Size>,
    still: Still,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (width, height) = match size {
        Some(s) => s.resolve(image.width, image.height)?,
        None => (image.width, image.height),
    };
    if width > WEBP_MAX_SIDE || height > WEBP_MAX_SIDE {
        return Err(format!(
            "a {width}x{height} WebP is too large: each side takes at most {WEBP_MAX_SIDE} pixels"
        ));
    }
    let image = resize(image, width, height)?;
    let bytes = webp::encode(&image, &still.options()).map_err(|e| e.to_string())?;
    Ok((bytes, width, height))
}

pub fn encode_png(image: Rgba, size: Option<Size>) -> Result<(Vec<u8>, u32, u32), String> {
    let (width, height) = match size {
        Some(s) => s.resolve(image.width, image.height)?,
        None => (image.width, image.height),
    };
    let image = resize(image, width, height)?;
    let bytes = if image.data.chunks_exact(4).any(|px| px[3] != 255) {
        png::encode_rgba(&image)
    } else {
        png::encode(&image)
    }
    .map_err(|e| e.to_string())?;
    Ok((bytes, width, height))
}

pub fn is_png(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

pub fn convert(job: &Job, partial_path: &mut Option<PathBuf>) -> Result<String, Fail> {
    let started = Instant::now();
    if same_file(job.input, job.output) {
        return Err("input and output are the same file".into());
    }
    if !job.overwrite && job.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            job.output.display()
        )
        .into());
    }
    let shown = job.input.display();
    let image = load(job.input).map_err(|e| format!("{shown}: {e}"))?;
    let png_out = is_png(job.output);
    let (bytes, width, height) = if png_out {
        encode_png(image, job.size)
    } else {
        encode(image, job.size, job.still)
    }
    .map_err(|e| format!("{shown}: {e}"))?;
    let part = partial(job.output)?;
    *partial_path = Some(part.clone());
    std::fs::write(&part, &bytes).map_err(|e| format!("{}: {e}", job.output.display()))?;
    super::convert::land(&part, job.output, job.overwrite)?;
    if !job.json {
        return Ok(job.output.display().to_string());
    }
    let options = job.still.options();
    let quality =
        (!png_out && !job.still.lossless).then(|| job.still.quality.unwrap_or(DEFAULT_QUALITY));
    let mut settings = vec![("lossless", Json::from(png_out || job.still.lossless))];
    if let Some(q) = quality {
        settings.push(("quality", q.into()));
    }
    if !png_out {
        settings.push(("compression_level", options.effort.into()));
    }
    let seconds = started.elapsed().as_secs_f64();
    let document = manifest::Manifest {
        sources: vec![manifest::Source {
            path: job.input.display().to_string(),
            sha256: super::convert::hash(job.input)?,
            loudness: None,
        }],
        params: manifest::Params {
            profile: "still".into(),
            encoder: manifest::Encoder {
                encoder: if png_out { "png" } else { "webp" }.into(),
                codec: None,
                settings,
            },
            quality: quality.map(|q| manifest::Quality::Webp(u32::from(q))),
            width,
            height,
            fps: None,
            tune: None,
            loudness: None,
            audio: None,
        },
        device: "CPU".into(),
        seconds: (seconds * 100.0).round() / 100.0,
        files: vec![manifest::Output {
            file: job.output.display().to_string(),
            sha256: super::convert::hash(job.output)?,
            bytes: bytes.len() as u64,
            loudness: None,
        }],
        join: None,
        cut: None,
    };
    Ok(document.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: u32, height: u32, alpha: bool) -> Rgba {
        let mut img = Rgba::new(width, height);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            px.copy_from_slice(&[
                (x * 255 / width.max(1)) as u8,
                (y * 255 / height.max(1)) as u8,
                ((x + y) * 2) as u8,
                if alpha {
                    ((x * 3 + y) % 256) as u8
                } else {
                    255
                },
            ]);
        }
        img
    }

    fn psnr(a: &Rgba, b: &Rgba) -> f64 {
        let mse = a
            .data
            .chunks_exact(4)
            .zip(b.data.chunks_exact(4))
            .flat_map(|(p, q)| (0..3).map(move |c| (f64::from(p[c]) - f64::from(q[c])).powi(2)))
            .sum::<f64>()
            / (a.data.len() / 4 * 3) as f64;
        10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
    }

    fn jpeg(sof: u8, height: u16, width: u16, sampling: &[u8]) -> Vec<u8> {
        let mut d = vec![0xff, 0xd8, 0xff, 0xe0, 0, 16];
        d.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        d.extend_from_slice(&[0xff, 0xff, sof]);
        let len = 8 + 3 * sampling.len() as u16;
        d.extend_from_slice(&len.to_be_bytes());
        d.push(8);
        d.extend_from_slice(&height.to_be_bytes());
        d.extend_from_slice(&width.to_be_bytes());
        d.push(sampling.len() as u8);
        for (i, s) in sampling.iter().enumerate() {
            d.extend_from_slice(&[i as u8 + 1, *s, 0]);
        }
        d.extend_from_slice(&[0xff, 0xda, 0, 2, 0xff, 0xd9]);
        d
    }

    fn gif(delays: &[u16]) -> Vec<u8> {
        let frames: Vec<Rgba> = (0..delays.len())
            .map(|n| {
                let mut img = pattern(40, 30, false);
                img.data[n * 4] = img.data[n * 4].wrapping_add(90);
                img.data[(n + 40) * 4 + 1] ^= 0x7f;
                img
            })
            .collect();
        let opts = pito_codecs::gif::EncodeOptions {
            diff: false,
            ..pito_codecs::gif::EncodeOptions::default()
        };
        pito_codecs::gif::encode(&frames, delays, opts).unwrap()
    }

    #[test]
    fn sniff_names_each_format_by_its_magic() {
        assert_eq!(
            Format::sniff(&[137, 80, 78, 71, 13, 10, 26, 10]),
            Some(Format::Png)
        );
        assert_eq!(Format::sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(Format::Jpeg));
        assert_eq!(Format::sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::Webp));
        assert_eq!(Format::sniff(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(Format::sniff(b"GIF89a"), Some(Format::Gif));
        assert_eq!(Format::sniff(b"GIF87a"), Some(Format::Gif));
        assert_eq!(Format::sniff(b"\0\0\0\x18ftypmp42"), None);
        assert_eq!(Format::sniff(&[]), None);
        assert_eq!(Format::Jpeg.codec(), "mjpeg");
        assert_eq!(Format::Png.codec(), "png");
        assert_eq!(Format::Webp.codec(), "webp");
        assert_eq!(Format::Gif.codec(), "gif");
    }

    #[test]
    fn uniform_delays_give_the_exact_rate() {
        assert_eq!(rate(&[4; 10], 100.0), Some(25.0));
        assert_eq!(rate(&[10; 3], 100.0), Some(10.0));
        assert_eq!(rate(&[2; 5], 100.0), Some(50.0));
        assert_eq!(rate(&[3; 34], 100.0), Some(100.0 / 3.0));
        assert_eq!(rate(&[7; 15], 100.0), Some(100.0 / 7.0));
        assert_eq!(rate(&[40; 4], 1000.0), Some(25.0));
    }

    #[test]
    fn mixed_delays_snap_to_a_whole_rate_within_one_percent() {
        let fifteen: Vec<u32> = [7, 7, 6].repeat(5);
        assert_eq!(rate(&fifteen, 100.0), Some(15.0));
        let thirty: Vec<u32> = [3, 3, 4].repeat(10);
        assert_eq!(rate(&thirty, 100.0), Some(30.0));
        let twenty_four: Vec<u32> = [4, 4, 4, 4, 4, 5].repeat(4);
        assert_eq!(rate(&twenty_four, 100.0), Some(24.0));
        let odd = [10, 20];
        let mean = rate(&odd, 100.0).unwrap();
        assert!((mean - 2.0 * 100.0 / 30.0).abs() < 1e-9, "{mean}");
    }

    #[test]
    fn no_delays_have_no_rate() {
        assert_eq!(rate(&[], 100.0), None);
        assert_eq!(rate(&[0, 0], 100.0), None);
    }

    fn gif_facts(data: &[u8]) -> (u32, u32, Vec<u32>, bool) {
        let f = facts(Format::Gif, data).unwrap();
        let info = pito_codecs::media::scan_gif(data).unwrap();
        let delays = info.delays_cs.iter().map(|&d| u32::from(d)).collect();
        (f.width, f.height, delays, f.alpha)
    }

    #[test]
    fn a_gif_gives_its_size_delays_and_rate() {
        let data = gif(&[4, 4, 4, 4, 4]);
        let (w, h, delays, _) = gif_facts(&data);
        assert_eq!((w, h), (40, 30));
        assert_eq!(delays, vec![4; 5]);
        let f = facts(Format::Gif, &data).unwrap();
        assert_eq!(f.frames, 5);
        assert_eq!(f.fps, Some(25.0));
        assert!((f.seconds.unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(f.pix_fmt, Some("bgra"));
    }

    #[test]
    fn a_zero_gif_delay_counts_as_ten_hundredths() {
        let data = gif(&[0, 0, 0]);
        let (_, _, delays, _) = gif_facts(&data);
        assert_eq!(delays, vec![10, 10, 10]);
        let f = facts(Format::Gif, &data).unwrap();
        assert_eq!(f.fps, Some(10.0));
        assert!((f.seconds.unwrap() - 0.3).abs() < 1e-9);
        let (_, _, delays, _) = gif_facts(&gif(&[0, 2, 1, 3]));
        assert_eq!(delays, vec![10, 2, 1, 3]);
    }

    #[test]
    fn a_gif_from_pcod_at_fifteen_fps_probes_at_fifteen() {
        let delays: Vec<u16> = (0..10)
            .map(|n| pito_codecs::gif::delay_cs(n, 15, 1))
            .collect();
        let f = facts(Format::Gif, &gif(&delays)).unwrap();
        assert_eq!(f.fps, Some(15.0));
        assert_eq!(f.frames, 10);
    }

    fn two_images(second_control: &[u8]) -> Vec<u8> {
        let mut data = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        data.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0x80]);
        data.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        data.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        data.extend_from_slice(second_control);
        data.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0x81]);
        data.extend_from_slice(&[0; 12]);
        data.extend_from_slice(&[2, 2, 0x44, 0x01, 0, 0x3b]);
        data
    }

    #[test]
    fn a_gif_with_a_local_table_and_no_control_block_is_walked() {
        let data = two_images(&[0x21, 0xf9, 4, 1, 5, 0, 0, 0]);
        assert_eq!(gif_facts(&data), (1, 1, vec![10, 5], true));
        let opaque = two_images(&[0x21, 0xf9, 4, 0, 5, 0, 0, 0]);
        assert_eq!(gif_facts(&opaque), (1, 1, vec![10, 5], false));
    }

    #[test]
    fn a_control_block_spent_on_plain_text_gives_no_delay_or_alpha() {
        let mut control = vec![0x21, 0xf9, 4, 1, 5, 0, 0, 0];
        control.extend_from_slice(&[
            0x21, 0x01, 12, 0, 0, 0, 0, 1, 0, 1, 0, 1, 1, 0, 1, 1, b'A', 0,
        ]);
        assert_eq!(
            gif_facts(&two_images(&control)),
            (1, 1, vec![10, 10], false)
        );
    }

    #[test]
    fn a_short_control_block_gives_no_delay() {
        let data = two_images(&[0x21, 0xf9, 2, 1, 5, 0]);
        assert_eq!(gif_facts(&data), (1, 1, vec![10, 10], false));
    }

    #[test]
    fn a_padded_tail_after_the_last_image_still_probes() {
        let mut data = gif(&[4, 4, 4]);
        assert_eq!(data.pop(), Some(0x3b));
        data.push(0);
        let f = facts(Format::Gif, &data).unwrap();
        assert_eq!((f.frames, f.fps), (3, Some(25.0)));
    }

    #[test]
    fn a_broken_gif_is_refused() {
        let data = gif(&[4, 4, 4]);
        let refused = |d: &[u8]| facts(Format::Gif, d).unwrap_err();
        assert!(refused(&data[..data.len() / 2]).contains("truncated"));
        assert!(refused(&data[..12]).contains("not a GIF"));
        let mut zero = data.clone();
        zero[6..10].fill(0);
        assert!(refused(&zero).contains("zero canvas size"));
        let mut empty = data[..13].to_vec();
        empty.push(0x3b);
        empty[10] = 0;
        assert!(refused(&empty).contains("no frames"));
        let mut junk = data[..13].to_vec();
        junk[10] = 0;
        junk.push(0x99);
        assert!(refused(&junk).contains("unknown block 0x99"));
    }

    #[test]
    fn a_jpeg_frame_header_gives_size_and_sampling() {
        let j = jpeg(0xc0, 480, 640, &[0x22, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (640, 480, Some("yuvj420p")));
        let j = jpeg(0xc2, 9, 17, &[0x21, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (17, 9, Some("yuvj422p")));
        let j = jpeg(0xc1, 4, 4, &[0x11, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (4, 4, Some("yuvj444p")));
        let j = jpeg(0xc0, 64, 32, &[0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (32, 64, Some("gray")));
        let j = jpeg(0xc0, 8, 8, &[0x11, 0x11, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (8, 8, None));
        let mut j = jpeg(0xc1, 6, 10, &[0x22, 0x11, 0x11]);
        assert_eq!(j[25], 8);
        j[25] = 12;
        assert_eq!(jpeg_size(&j).unwrap(), (10, 6, None));
        let mut filled = jpeg(0xc0, 2, 2, &[0x11]);
        filled.splice(20..20, [0xff; 5]);
        assert_eq!(jpeg_size(&filled).unwrap(), (2, 2, Some("gray")));
        let f = facts(Format::Jpeg, &jpeg(0xc0, 48, 64, &[0x22, 0x11, 0x11])).unwrap();
        assert_eq!((f.width, f.height, f.frames, f.fps), (64, 48, 1, None));
    }

    #[test]
    fn jpeg_sampling_maps_by_its_chroma_ratio() {
        let j = jpeg(0xc0, 16, 16, &[0x12, 0x12, 0x12]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj444p")));
        let j = jpeg(0xc0, 16, 16, &[0x22, 0x12, 0x12]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj422p")));
        let j = jpeg(0xc0, 16, 16, &[0x44, 0x22, 0x22]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj420p")));
        let j = jpeg(0xc0, 16, 16, &[0x12, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj440p")));
        let j = jpeg(0xc0, 16, 16, &[0x41, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj411p")));
        let j = jpeg(0xc0, 16, 16, &[0x31, 0x21, 0x21]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, None));
        let j = jpeg(0xc0, 16, 16, &[0x22, 0x21, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, None));
    }

    #[test]
    fn probe_measures_jpegs_the_decoder_refuses() {
        for sof in [0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf] {
            let j = jpeg(sof, 30, 40, &[0x22, 0x11, 0x11]);
            assert_eq!(
                jpeg_size(&j).unwrap(),
                (40, 30, Some("yuvj420p")),
                "0x{sof:02X}"
            );
            assert!(jpeg::decode(&j).is_err(), "0x{sof:02X}");
        }
        let mut twelve = jpeg(0xc1, 30, 40, &[0x22, 0x11, 0x11]);
        twelve[25] = 12;
        assert_eq!(jpeg_size(&twelve).unwrap(), (40, 30, None));
        let i = jpeg::decode::info(&twelve).unwrap();
        assert_eq!((i.precision, i.decodable), (12, false));
        assert!(
            jpeg::decode(&twelve)
                .unwrap_err()
                .to_string()
                .contains("12-bit")
        );
        let i = jpeg::decode::info(&jpeg(0xc0, 30, 40, &[0x22, 0x11, 0x11])).unwrap();
        assert_eq!((i.precision, i.decodable), (8, true));
        let mut dac = jpeg(0xc9, 30, 40, &[0x22, 0x11, 0x11]);
        dac.splice(20..20, [0xff, 0xcc, 0, 4, 0, 0]);
        assert_eq!(jpeg_size(&dac).unwrap(), (40, 30, Some("yuvj420p")));
    }

    #[test]
    fn orient_follows_each_exif_orientation() {
        let mut img = Rgba::new(3, 2);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(&[i as u8, 0, 0, 255]);
        }
        let order = |o: u8| -> (u32, u32, Vec<u8>) {
            let r = orient(img.clone(), o);
            (
                r.width,
                r.height,
                r.data.chunks_exact(4).map(|p| p[0]).collect(),
            )
        };
        for o in [0, 1, 9] {
            assert_eq!(order(o), (3, 2, vec![0, 1, 2, 3, 4, 5]), "{o}");
        }
        assert_eq!(order(2), (3, 2, vec![2, 1, 0, 5, 4, 3]));
        assert_eq!(order(3), (3, 2, vec![5, 4, 3, 2, 1, 0]));
        assert_eq!(order(4), (3, 2, vec![3, 4, 5, 0, 1, 2]));
        assert_eq!(order(5), (2, 3, vec![0, 3, 1, 4, 2, 5]));
        assert_eq!(order(6), (2, 3, vec![3, 0, 4, 1, 5, 2]));
        assert_eq!(order(7), (2, 3, vec![5, 2, 4, 1, 3, 0]));
        assert_eq!(order(8), (2, 3, vec![2, 5, 1, 4, 0, 3]));
    }

    #[test]
    fn a_jpeg_without_a_usable_frame_header_is_refused() {
        let no_sof = [0xff, 0xd8, 0xff, 0xda, 0, 2, 0xff, 0xd9];
        assert!(jpeg_size(&no_sof).unwrap_err().contains("no frame header"));
        let j = jpeg(0xc0, 48, 64, &[0x22, 0x11, 0x11]);
        assert!(jpeg_size(&j[..22]).is_err());
        assert!(jpeg_size(&j[..2]).is_err());
        let zero = jpeg(0xc0, 0, 64, &[0x22, 0x11, 0x11]);
        assert!(jpeg_size(&zero).unwrap_err().contains("64x0"));
        let mut bad_len = j.clone();
        bad_len[4..6].copy_from_slice(&[0, 1]);
        assert!(jpeg_size(&bad_len).is_err());
        let mut no_marker = j.clone();
        no_marker[2] = 0x00;
        assert!(jpeg_size(&no_marker).is_err());
    }

    #[test]
    fn png_facts_name_the_pixel_format_and_alpha() {
        let rgba = pito_codecs::png::encode_rgba(&pattern(30, 20, true)).unwrap();
        let f = facts(Format::Png, &rgba).unwrap();
        assert_eq!((f.width, f.height, f.alpha, f.frames), (30, 20, true, 1));
        assert_eq!(f.pix_fmt, Some("rgba"));
        assert_eq!((f.fps, f.seconds), (None, None));
        let mut rgb = Vec::new();
        let mut enc = ::png::Encoder::new(&mut rgb, 30, 20);
        enc.set_color(::png::ColorType::Rgb);
        enc.set_depth(::png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&[7u8; 30 * 20 * 3])
            .unwrap();
        let f = facts(Format::Png, &rgb).unwrap();
        assert_eq!((f.alpha, f.pix_fmt), (false, Some("rgb24")));
        assert!(facts(Format::Png, &rgb[..20]).is_err());
    }

    #[test]
    fn webp_facts_cover_stills_and_animations() {
        let still_data =
            webp::encode(&pattern(30, 20, false), &webp::Options::lossy(80.0)).unwrap();
        let f = facts(Format::Webp, &still_data).unwrap();
        assert_eq!((f.width, f.height, f.frames, f.fps), (30, 20, 1, None));
        assert_eq!(f.pix_fmt, Some("yuv420p"));
        let lossless = webp::encode(&pattern(30, 20, true), &webp::Options::lossless()).unwrap();
        let f = facts(Format::Webp, &lossless).unwrap();
        assert_eq!((f.alpha, f.pix_fmt), (true, Some("argb")));
        let mut anim = webp::AnimEncoder::new(30, 20, webp::Options::lossy(70.0), 0).unwrap();
        for n in 0..4u8 {
            let mut img = pattern(30, 20, false);
            img.data[0] = n * 50;
            anim.add(&img, 40).unwrap();
        }
        let data = anim.finish().unwrap();
        assert_eq!(webp_delays(&data).unwrap(), vec![40; 4]);
        let f = facts(Format::Webp, &data).unwrap();
        assert_eq!(f.frames, 4);
        assert_eq!(f.fps, Some(25.0));
        assert!((f.seconds.unwrap() - 0.16).abs() < 1e-9);
        assert!(webp_delays(&still_data).is_err());
    }

    #[test]
    fn the_json_has_the_video_keys_with_audio_null() {
        let f = facts(Format::Gif, &gif(&[4, 4])).unwrap();
        let text = json(&f).text();
        for key in [
            "\"width\":40",
            "\"height\":30",
            "\"codec\":\"gif\"",
            "\"audio\":null",
            "\"fps\":25",
            "\"frames\":2",
            "\"video_index\":0",
            "\"pix_fmt\":\"bgra\"",
        ] {
            assert!(text.replace(' ', "").contains(key), "{key} in {text}");
        }
        let lines = lines(&f);
        assert_eq!(lines[0], "container  GIF");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("40x30 25.00 fps, 2 frames"))
        );
    }

    #[test]
    fn still_options_follow_libwebp() {
        let lossy = Still {
            quality: None,
            lossless: false,
        }
        .options();
        assert_eq!(
            (lossy.lossless, lossy.quality, lossy.effort),
            (false, 75.0, 4)
        );
        let q = Still {
            quality: Some(86),
            lossless: false,
        }
        .options();
        assert_eq!(q.quality, 86.0);
        let lossless = Still {
            quality: None,
            lossless: true,
        }
        .options();
        assert_eq!((lossless.lossless, lossless.effort), (true, 6));
    }

    #[test]
    fn lossless_round_trips_bit_exact_with_alpha() {
        let mut src = pattern(37, 23, true);
        src.data[4 * 40..4 * 41].copy_from_slice(&[200, 90, 30, 0]);
        src.data[4 * 300..4 * 301].copy_from_slice(&[17, 250, 4, 0]);
        let still = Still {
            quality: None,
            lossless: true,
        };
        let (bytes, w, h) = encode(src.clone(), None, still).unwrap();
        assert_eq!((w, h), (37, 23));
        assert!(!bytes.windows(4).any(|c| c == b"ANIM"));
        assert!(bytes.windows(4).any(|c| c == b"VP8L"));
        assert_visible_equal(&webp::decode(&bytes).unwrap(), &src);
    }

    fn assert_visible_equal(back: &Rgba, src: &Rgba) {
        assert_eq!((back.width, back.height), (src.width, src.height));
        for (i, (b, s)) in back
            .data
            .chunks_exact(4)
            .zip(src.data.chunks_exact(4))
            .enumerate()
        {
            if s[3] == 0 {
                assert_eq!(b[3], 0, "pixel {i}");
            } else {
                assert_eq!(b, s, "pixel {i}");
            }
        }
    }

    #[test]
    fn a_size_past_webps_limit_is_refused_before_the_resize() {
        let still = Still {
            quality: Some(80),
            lossless: false,
        };
        let err = encode(pattern(40, 30, false), Some(Size::Width(60000)), still).unwrap_err();
        assert!(
            err.contains("60000x45000") && err.contains("16383"),
            "{err}"
        );
        let err = encode(pattern(2, 4000, false), Some(Size::Width(10)), still).unwrap_err();
        assert!(err.contains("10x20000"), "{err}");
        let (bytes, w, h) = encode(pattern(30, 40, false), Some(Size::Height(720)), still).unwrap();
        assert_eq!((w, h), (540, 720));
        let back = webp::decode(&bytes).unwrap();
        assert_eq!((back.width, back.height), (540, 720));
    }

    #[test]
    fn resizing_an_rgba_still_leaves_no_dark_halo() {
        let mut src = Rgba::new(64, 48);
        for (i, px) in src.data.chunks_exact_mut(4).enumerate() {
            let x = i % 64;
            px.copy_from_slice(if x < 32 {
                &[0, 0, 0, 0]
            } else {
                &[240, 240, 240, 255]
            });
        }
        let small = resize(src, 32, 24).unwrap();
        for px in small.data.chunks_exact(4) {
            if px[3] > 16 {
                assert!(px[0] >= 200, "{px:?}");
            }
        }
    }

    #[test]
    fn lossy_stays_above_the_psnr_floor_and_grows_with_quality() {
        let src = pattern(96, 64, false);
        let mut sizes = Vec::new();
        for q in [80, 82, 86] {
            let still = Still {
                quality: Some(q),
                lossless: false,
            };
            let (bytes, _, _) = encode(src.clone(), None, still).unwrap();
            assert!(bytes.windows(4).any(|c| c == b"VP8 "));
            assert!(!bytes.windows(4).any(|c| c == b"ANIM"));
            let back = webp::decode(&bytes).unwrap();
            assert_eq!((back.width, back.height), (96, 64));
            let p = psnr(&src, &back);
            assert!(p > 30.0, "quality {q}: {p:.2} dB");
            sizes.push(bytes.len());
        }
        assert!(sizes[0] <= sizes[2], "{sizes:?}");
    }

    #[test]
    fn lossy_keeps_alpha() {
        let src = pattern(48, 32, true);
        let still = Still {
            quality: Some(80),
            lossless: false,
        };
        let (bytes, _, _) = encode(src.clone(), None, still).unwrap();
        assert!(bytes.windows(4).any(|c| c == b"ALPH"));
        let back = webp::decode(&bytes).unwrap();
        let worst = src
            .data
            .chunks_exact(4)
            .zip(back.data.chunks_exact(4))
            .map(|(a, b)| a[3].abs_diff(b[3]))
            .max()
            .unwrap();
        assert!(worst <= 2, "alpha off by {worst}");
    }

    #[test]
    fn size_resamples_to_an_even_height_or_the_exact_shape() {
        let still = Still {
            quality: Some(80),
            lossless: false,
        };
        let (bytes, w, h) = encode(pattern(100, 75, false), Some(Size::Width(64)), still).unwrap();
        assert_eq!((w, h), (64, 48));
        let back = webp::decode(&bytes).unwrap();
        assert_eq!((back.width, back.height), (64, 48));
        let (_, w, h) = encode(pattern(101, 33, false), Some(Size::Width(50)), still).unwrap();
        assert_eq!((w, h % 2), (50, 0));
        let (_, w, h) = encode(pattern(100, 75, false), Some(Size::Exact(40, 30)), still).unwrap();
        assert_eq!((w, h), (40, 30));
        assert!(encode(pattern(100, 75, false), Some(Size::Exact(40, 40)), still).is_err());
    }

    #[test]
    fn resize_keeps_a_flat_colour_and_skips_the_same_size() {
        let mut flat = Rgba::new(64, 48);
        for px in flat.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[200, 100, 50, 255]);
        }
        let same = resize(flat.clone(), 64, 48).unwrap();
        assert_eq!(same.data, flat.data);
        let small = resize(flat, 32, 24).unwrap();
        assert!(small.data.chunks_exact(4).all(|p| {
            p[0].abs_diff(200) <= 1
                && p[1].abs_diff(100) <= 1
                && p[2].abs_diff(50) <= 1
                && p[3] == 255
        }));
    }

    #[test]
    fn a_source_wider_than_eight_hundred_keeps_its_size() {
        let still = Still {
            quality: Some(50),
            lossless: false,
        };
        let (_, w, h) = encode(pattern(1001, 9, false), None, still).unwrap();
        assert_eq!((w, h), (1001, 9));
    }

    fn png_of(color: ::png::ColorType, depth: ::png::BitDepth, trns: bool) -> Vec<u8> {
        let (w, h) = (9u32, 2u32);
        let channels = match color {
            ::png::ColorType::Rgb => 3,
            ::png::ColorType::GrayscaleAlpha => 2,
            ::png::ColorType::Rgba => 4,
            _ => 1,
        };
        let row = (w as usize * channels * depth as usize).div_ceil(8);
        let mut out = Vec::new();
        let mut enc = ::png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        if color == ::png::ColorType::Indexed {
            enc.set_palette(vec![0u8, 0, 0, 255, 255, 255]);
        }
        if trns {
            enc.set_trns(vec![0u8]);
        }
        enc.write_header()
            .unwrap()
            .write_image_data(&vec![0u8; row * h as usize])
            .unwrap();
        out
    }

    #[test]
    fn png_facts_name_every_colour_type_and_depth() {
        use ::png::BitDepth::{Eight, One, Sixteen};
        use ::png::ColorType::{Grayscale, GrayscaleAlpha, Indexed, Rgb, Rgba};
        for (color, depth, trns, pix_fmt, alpha) in [
            (Grayscale, One, false, "monob", false),
            (Grayscale, Eight, false, "gray", false),
            (Grayscale, Sixteen, false, "gray16be", false),
            (Rgb, Sixteen, false, "rgb48be", false),
            (Indexed, Eight, false, "pal8", false),
            (Indexed, Eight, true, "pal8", true),
            (GrayscaleAlpha, Eight, false, "ya8", true),
            (GrayscaleAlpha, Sixteen, false, "ya16be", true),
            (Rgba, Sixteen, false, "rgba64be", true),
        ] {
            let data = png_of(color, depth, trns);
            let f = facts(Format::Png, &data).unwrap();
            assert_eq!(
                (f.width, f.height, f.pix_fmt, f.alpha),
                (9, 2, Some(pix_fmt), alpha),
                "{color:?} {depth:?} trns {trns}"
            );
        }
    }

    #[test]
    fn a_jpeg_sampling_ffmpeg_has_no_name_for_gives_no_pix_fmt() {
        for sampling in [[0x31, 0x11, 0x11], [0x13, 0x11, 0x11], [0x42, 0x11, 0x11]] {
            let j = jpeg(0xc0, 16, 16, &sampling);
            assert_eq!(jpeg_size(&j).unwrap(), (16, 16, None), "{sampling:02x?}");
        }
        let j = jpeg(0xc0, 16, 16, &[0x42, 0x21, 0x21]);
        assert_eq!(jpeg_size(&j).unwrap(), (16, 16, Some("yuvj420p")));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/unit-picture")
            .join(format!(
                "{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_or_an_unknown_file_sniffs_as_nothing() {
        let dir = scratch("sniff");
        assert_eq!(Format::of(&dir), None);
        assert_eq!(probe(&dir).unwrap(), None);
        assert!(!still_input(&dir));
        let text = dir.join("notes.png");
        std::fs::write(&text, b"plain text").unwrap();
        assert_eq!(Format::of(&text), None);
        assert!(!still_input(&text));
        assert_eq!(Format::of(&dir.join("missing.png")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_refuses_what_is_no_still_image() {
        let dir = scratch("load");
        let animated = dir.join("moving.gif");
        std::fs::write(&animated, gif(&[4, 4])).unwrap();
        let text = dir.join("notes.png");
        std::fs::write(&text, b"plain text").unwrap();
        for path in [&animated, &text] {
            let e = load(path).unwrap_err();
            assert_eq!(
                e,
                "not a PNG, JPEG or WebP still image",
                "{}",
                path.display()
            );
        }
        assert!(load(&dir.join("missing.png")).is_err());
        let png = dir.join("one.png");
        std::fs::write(&png, png::encode(&pattern(5, 4, false)).unwrap()).unwrap();
        assert_eq!(load(&png).unwrap(), pattern(5, 4, false));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_png_colour_and_depth_names_ffprobes_pixel_format() {
        let named = |color, depth| {
            png_pix_fmt(&png::Info {
                width: 1,
                height: 1,
                depth,
                color,
                interlaced: false,
            })
        };
        let cases = [
            (png::Color::Grey, 1, "monob"),
            (png::Color::Grey, 8, "gray"),
            (png::Color::Grey, 16, "gray16be"),
            (png::Color::Rgb, 8, "rgb24"),
            (png::Color::Rgb, 16, "rgb48be"),
            (png::Color::Palette, 8, "pal8"),
            (png::Color::GreyAlpha, 8, "ya8"),
            (png::Color::GreyAlpha, 16, "ya16be"),
            (png::Color::Rgba, 8, "rgba"),
            (png::Color::Rgba, 16, "rgba64be"),
        ];
        for (color, depth, want) in cases {
            assert_eq!(named(color, depth), Some(want), "{want}");
        }
    }

    #[test]
    fn rare_jpeg_samplings_name_their_format_or_none() {
        let j = jpeg(0xc0, 8, 8, &[0x12, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (8, 8, Some("yuvj440p")));
        let j = jpeg(0xc0, 8, 8, &[0x41, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (8, 8, Some("yuvj411p")));
        let j = jpeg(0xc0, 8, 8, &[0x31, 0x11, 0x11]);
        assert_eq!(jpeg_size(&j).unwrap(), (8, 8, None));
    }

    #[test]
    fn a_lossy_webp_with_alpha_is_yuva420p() {
        let data = webp::encode(&pattern(30, 20, true), &webp::Options::lossy(80.0)).unwrap();
        let f = facts(Format::Webp, &data).unwrap();
        assert_eq!((f.alpha, f.pix_fmt), (true, Some("yuva420p")));
    }

    #[test]
    fn trns_is_not_found_past_the_end_of_the_chunks() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0; 13 + 4]);
        assert!(!has_trns(&png));
        for cut in 8..png.len() {
            assert!(!has_trns(&png[..cut]), "{cut}");
        }
        let mut huge = png.clone();
        huge.extend_from_slice(&u32::MAX.to_be_bytes());
        huge.extend_from_slice(b"tEXt");
        assert!(!has_trns(&huge));
        let mut found = png.clone();
        found.extend_from_slice(&1u32.to_be_bytes());
        found.extend_from_slice(b"tRNS");
        assert!(has_trns(&found));
        let mut late = png;
        late.extend_from_slice(&0u32.to_be_bytes());
        late.extend_from_slice(b"IDAT");
        late.extend_from_slice(&[0; 4]);
        late.extend_from_slice(&1u32.to_be_bytes());
        late.extend_from_slice(b"tRNS");
        assert!(!has_trns(&late));
    }
}
