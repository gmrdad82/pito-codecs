use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use super::Fail;
use super::json::Json;
use super::pipeline::{self, Audio, Stopped};
use super::plan::{
    AAC_KBPS, Container, OPUS_KBPS, Request, SAMPLE_RATE, loudness_json, partial, same_file,
};
use super::profile::{ANIMATED_FPS, ANIMATED_WIDTH, Encoder, Profile};
use super::progress::Progress;
use super::sha256;
use super::signals;
use super::size::Size;
use super::source::{self, Kind, Media};

pub struct Options {
    pub request: Request,
    pub json: bool,
    pub progress: bool,
}

struct Planned {
    container: Container,
    encoder: Encoder,
    size: (u32, u32),
    fps: (u32, u32),
    frames: Option<u64>,
    audio: Option<Audio>,
}

fn reduced(num: u32, den: u32) -> (u32, u32) {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a.max(1) } else { gcd(b, a % b) }
    }
    let g = gcd(num, den);
    (num / g, den / g)
}

fn audio_for(request: &Request, container: Container, codec: &str) -> Audio {
    let webm = container == Container::Webm;
    let untouched = request.loudness.is_none() && request.audio_bitrate.is_none();
    if untouched && !webm && codec == "aac" {
        return Audio::Copy {
            codec: codec.into(),
        };
    }
    if untouched && matches!(container, Container::Webm | Container::Mkv) && codec == "opus" {
        return Audio::Copy {
            codec: codec.into(),
        };
    }
    if webm {
        Audio::Opus {
            kbps: request.audio_bitrate.unwrap_or(OPUS_KBPS),
        }
    } else {
        Audio::Aac {
            kbps: request.audio_bitrate.unwrap_or(AAC_KBPS),
        }
    }
}

fn plan(request: &Request, media: &Media) -> Result<Planned, Fail> {
    let (container, encoder) = request.settings()?;
    let animated = container.animated();
    if same_file(&request.input, &request.output) {
        return Err("input and output are the same file".into());
    }
    if !request.overwrite && request.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            request.output.display()
        )
        .into());
    }
    if let (Some(target), None) = (&request.loudness, &media.audio) {
        return Err(format!(
            "the source has no audio to bring to {} LUFS",
            target.integrated
        )
        .into());
    }
    if let (Some(kbps), None) = (request.audio_bitrate, &media.audio) {
        return Err(format!("the source has no audio to encode at {kbps} kbps").into());
    }
    if request.loudness.is_some() {
        return Err("--loudness isn't available in this build of pcod".into());
    }
    if media.alpha && container == Container::Webm {
        return Err(
            "the frames carry alpha, and this build of pcod writes WebM without it; write .gif or .webp to keep it, or .mp4 to drop it onto black"
                .into(),
        );
    }
    if !media.rotation.is_multiple_of(90) {
        return Err(format!(
            "the source is rotated {}°: pcod reads quarter turns only",
            media.rotation
        )
        .into());
    }
    let copy = request.copy_video
        && media.kind == Kind::File
        && container.carries(&media.codec)
        && !(pipeline::mkv_family(&media.path)
            && matches!(container, Container::Mp4 | Container::Mov));
    let encoder = if copy {
        Encoder::Copy {
            codec: media.codec.clone(),
        }
    } else {
        encoder
    };
    let (source_width, source_height) = media.display_size();
    let size = if copy {
        (media.width, media.height)
    } else {
        match request.size {
            Some(size) => size.resolve(source_width, source_height)?,
            None if animated && source_width > ANIMATED_WIDTH => {
                Size::Width(ANIMATED_WIDTH).resolve(source_width, source_height)?
            }
            None => (source_width & !1, source_height & !1),
        }
    };
    if size.0 == 0 || size.1 == 0 {
        return Err(format!(
            "source shape {source_width}x{source_height} cannot make an even output"
        )
        .into());
    }
    if container == Container::Webm && u64::from(size.0) * u64::from(size.1) > 4096 * 2160 {
        return Err(format!(
            "WebM is VP9 on the CPU, which this build keeps to 4096x2160 pixels, not {}x{}: use --size 2160p or smaller",
            size.0, size.1
        )
        .into());
    }
    let source_fps = media.fps;
    if source_fps.0 == 0 || source_fps.1 == 0 {
        return Err("the source has no frame rate".into());
    }
    let fps = match request.fps {
        Some(f) => (f, 1),
        None if animated && media.fps_value() > f64::from(ANIMATED_FPS) => (ANIMATED_FPS, 1),
        None => reduced(source_fps.0, source_fps.1),
    };
    let frames = media.seconds.map(|s| {
        (s * f64::from(fps.0) / f64::from(fps.1))
            .round()
            .clamp(0.0, 1e15) as u64
    });
    let audio = match &media.audio {
        Some(a) if !animated => Some(audio_for(request, container, &a.codec)),
        _ => None,
    };
    Ok(Planned {
        container,
        encoder,
        size,
        fps,
        frames,
        audio,
    })
}

fn audio_json(audio: &Option<Audio>) -> Json {
    match audio {
        None => Json::Null,
        Some(Audio::Copy { codec }) => {
            Json::map([("encoder", "copy".into()), ("codec", codec.as_str().into())])
        }
        Some(Audio::Aac { kbps }) => Json::map([
            ("encoder", "aac".into()),
            ("kbps", (*kbps).into()),
            ("layout", "stereo".into()),
            ("sample_rate", SAMPLE_RATE.into()),
        ]),
        Some(Audio::Opus { kbps }) => {
            Json::map([("encoder", "opus".into()), ("kbps", (*kbps).into())])
        }
    }
}

fn encoder_words(encoder: &Encoder) -> Vec<String> {
    let w = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match encoder {
        Encoder::H264 { preset, crf } => w(&[
            "encode",
            "h264",
            "cpu",
            &format!("preset {}", preset.name()),
            &format!("crf {crf}"),
        ]),
        Encoder::H264Gpu { qp } => w(&["encode", "h264", "gpu", &format!("qp {qp}")]),
        Encoder::Vp9 { crf } => w(&["encode", "vp9", &format!("crf {crf}")]),
        Encoder::Gif { tolerance } => w(&["encode", "gif", &format!("tolerance {tolerance}")]),
        Encoder::Webp { quality, effort } => w(&[
            "encode",
            "webp",
            &format!("quality {quality}"),
            &format!("effort {effort}"),
        ]),
        Encoder::Copy { codec } => w(&["copy", codec]),
    }
}

fn audio_words(audio: &Audio) -> Vec<String> {
    match audio {
        Audio::Copy { codec } => vec!["copy".into(), codec.clone()],
        Audio::Aac { kbps } => vec![
            "encode".into(),
            "aac".into(),
            format!("{kbps}k"),
            "stereo".into(),
            SAMPLE_RATE.to_string(),
        ],
        Audio::Opus { kbps } => vec!["encode".into(), "opus".into(), format!("{kbps}k")],
    }
}

fn pipeline_steps(media: &Media, planned: &Planned) -> Json {
    let mut steps: Vec<Vec<String>> = Vec::new();
    if !matches!(planned.encoder, Encoder::Copy { .. }) {
        steps.push(match &media.kind {
            Kind::File => vec!["decode".to_string(), media.codec.clone()],
            Kind::Frames(files) => vec!["read".into(), format!("{} png", files.len())],
        });
        if planned.size != media.display_size() {
            steps.push(vec![
                "scale".into(),
                format!("{}x{}", planned.size.0, planned.size.1),
                "lanczos".into(),
            ]);
        }
    }
    steps.push(encoder_words(&planned.encoder));
    if let Some(a) = &planned.audio {
        steps.push(audio_words(a));
    }
    Json::List(
        steps
            .into_iter()
            .map(|w| Json::List(w.into_iter().map(Json::Str).collect()))
            .collect(),
    )
}

fn hash(path: &Path) -> Result<String, Fail> {
    sha256::sha256_file(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn source_hashes(media: &Media) -> Result<Json, Fail> {
    match &media.kind {
        Kind::File => Ok(hash(&media.path)?.into()),
        Kind::Frames(_) => Ok(Json::Null),
    }
}

fn land(partial_path: &Path, output: &Path, overwrite: bool) -> Result<(), Fail> {
    if overwrite {
        return std::fs::rename(partial_path, output)
            .map_err(|e| format!("{}: {e}", output.display()).into());
    }
    match std::fs::hard_link(partial_path, output) {
        Ok(()) => {
            let _ = std::fs::remove_file(partial_path);
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(format!(
            "{} already exists; use --force to replace it",
            output.display()
        )
        .into()),
        Err(_) if !output.exists() => std::fs::rename(partial_path, output)
            .map_err(|e| format!("{}: {e}", output.display()).into()),
        Err(e) => Err(format!("{}: {e}", output.display()).into()),
    }
}

fn convert(options: &Options, partial_path: &mut Option<PathBuf>) -> Result<String, Fail> {
    let started = Instant::now();
    let request = &options.request;
    let media = source::open(&request.input, request.fps)?;
    let mut request = request.clone();
    if matches!(media.kind, Kind::Frames(_)) {
        request.fps = None;
    }
    let planned = plan(&request, &media)?;
    let source_hash = if options.json {
        source_hashes(&media)?
    } else {
        Json::Null
    };
    let part = partial(&request.output)?;
    *partial_path = Some(part.clone());
    let progress = Progress::new(options.progress);
    let job = pipeline::Job {
        media: &media,
        container: planned.container,
        encoder: &planned.encoder,
        size: planned.size,
        fps: planned.fps,
        frames: planned.frames,
        audio: planned.audio.clone(),
        partial: &part,
        tune: request.tune.as_deref(),
        stop: &signals::STOP,
        progress: &progress,
    };
    let done = pipeline::run(&job);
    progress.clear();
    let done = done?;
    land(&part, &request.output, request.overwrite)?;
    *partial_path = None;
    let seconds = started.elapsed().as_secs_f64();
    if !options.json {
        let mut lines = vec![request.output.display().to_string()];
        lines.extend(done.notes.iter().map(|n| format!("note: {n}")));
        return Ok(lines.join("\n"));
    }
    let bytes = std::fs::metadata(&request.output)
        .map(|m| m.len())
        .unwrap_or(0);
    let mut document = Json::map([
        (
            "tool",
            Json::map([
                ("name", "pcod".into()),
                ("version", env!("CARGO_PKG_VERSION").into()),
            ]),
        ),
        (
            "source",
            Json::map([
                ("path", request.input.as_path().into()),
                ("sha256", source_hash),
                ("loudness", Json::Null),
            ]),
        ),
        (
            "params",
            Json::map([
                ("profile", request.profile.name().into()),
                ("encoder", planned.encoder.json()),
                ("quality", planned.encoder.quality()),
                (
                    "size",
                    Json::map([
                        ("width", planned.size.0.into()),
                        ("height", planned.size.1.into()),
                    ]),
                ),
                (
                    "fps",
                    (f64::from(planned.fps.0) / f64::from(planned.fps.1.max(1))).into(),
                ),
                ("tune", request.tune.clone().into()),
                (
                    "loudness",
                    loudness_json(request.loudness.as_ref(), "linear"),
                ),
                ("audio", audio_json(&planned.audio)),
            ]),
        ),
        ("device", done.device.as_str().into()),
        ("seconds", ((seconds * 100.0).round() / 100.0).into()),
        (
            "files",
            Json::List(vec![Json::map([
                ("file", request.output.as_path().into()),
                ("sha256", hash(&request.output)?.into()),
                ("bytes", bytes.into()),
                ("args", pipeline_steps(&media, &planned)),
                ("loudness", Json::Null),
            ])]),
        ),
    ]);
    let mut notes = done.notes.clone();
    notes.extend(done.decode_notes.iter().cloned());
    if !notes.is_empty() {
        document.set("notes", notes.into());
    }
    Ok(document.text())
}

pub fn print(text: &str) -> ExitCode {
    match writeln!(std::io::stdout().lock(), "{text}") {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => refuse(&format!("cannot write to stdout: {e}")),
    }
}

pub fn refuse(message: &str) -> ExitCode {
    let line = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let _ = writeln!(std::io::stderr(), "pcod: {line}");
    ExitCode::FAILURE
}

pub fn run(options: Options) -> ExitCode {
    signals::catch();
    let mut partial_path = None;
    let outcome = convert(&options, &mut partial_path);
    if let Some(p) = &partial_path {
        let _ = std::fs::remove_file(p);
    }
    match outcome {
        Ok(text) => print(&text),
        Err(e) => {
            if let Some((code, message)) = signals::caught() {
                let _ = writeln!(std::io::stderr(), "pcod: {message}");
                return ExitCode::from(code);
            }
            if e.downcast_ref::<Stopped>().is_some() {
                return refuse("stopped before it finished");
            }
            refuse(&e.to_string())
        }
    }
}

pub fn profiles() -> String {
    let mut lines = Vec::new();
    for profile in Profile::ALL {
        lines.push(match profile.encoder(None) {
            Encoder::H264 { preset, crf } => format!(
                "{}: H.264 on the CPU, preset {}, CRF {crf} on x264's scale",
                profile.name(),
                preset.name()
            ),
            Encoder::H264Gpu { qp } => format!(
                "{}: H.264 on the GPU (VA-API, NVENC, VideoToolbox), QP {qp}; on the CPU where no GPU encoder is found",
                profile.name()
            ),
            _ => profile.name().to_string(),
        });
    }
    for profile in Profile::ALL {
        if let Some(Encoder::Vp9 { crf }) = profile.vp9_encoder(None) {
            lines.push(format!(
                "{} .webm: VP9, CRF {crf} on libvpx's scale",
                profile.name()
            ));
        }
    }
    lines.push(format!(
        "balanced, best .gif: a palette from the clip, loops forever, keeps alpha as on or off; without alpha each frame stores only what changed by more than {} levels ({} with best)",
        super::profile::GIF_BALANCED_TOLERANCE,
        super::profile::GIF_BEST_TOLERANCE
    ));
    for profile in Profile::ALL {
        if let Some(Encoder::Webp { quality, effort }) = profile.webp_encoder() {
            lines.push(format!(
                "{} .webp: lossy quality {quality}, effort {effort}, loops forever, keeps alpha",
                profile.name()
            ));
        }
    }
    lines.push(format!(
        ".gif and .webp: {ANIMATED_WIDTH} px wide at {ANIMATED_FPS} fps by default, or the source's width and rate where smaller; --size and --fps set them"
    ));
    lines.push(format!(
        "light web: --profile best --crf {} (.webm: --crf {})",
        super::profile::LIGHT_WEB_CRF,
        super::profile::VP9_LIGHT_WEB_CRF
    ));
    lines.push(format!(
        "--loudness youtube: {} LUFS under a {} dBTP true-peak ceiling, two EBU R128 passes; with no video option the video is copied where the container takes it",
        super::plan::YOUTUBE,
        super::plan::YOUTUBE_PEAK
    ));
    lines.push(format!(
        "audio: AAC-LC {AAC_KBPS}k stereo at {SAMPLE_RATE} Hz (an AAC source is copied when nothing changes its audio), Opus {OPUS_KBPS}k in .webm; --audio-bitrate {}–{} sets the rate",
        super::plan::AUDIO_KBPS_LOWEST,
        super::plan::AUDIO_KBPS_HIGHEST
    ));
    lines.join("\n")
}
