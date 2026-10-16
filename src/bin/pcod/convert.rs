use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use super::Fail;
use super::manifest;
use super::pipeline::{self, Audio, Stopped};
use super::plan::{AAC_KBPS, Container, OPUS_KBPS, Request, SAMPLE_RATE, partial, same_file};
use super::profile::{ANIMATED_FPS, ANIMATED_WIDTH, Encoder, Profile};
use super::progress::Progress;
use super::quality;
use super::sha256;
use super::signals;
use super::size::Size;
use super::source::{self, Kind, Media};
use pito_codecs::media;

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

fn audio_manifest(audio: &Audio) -> manifest::Audio {
    match audio {
        Audio::Copy { codec } => manifest::Audio {
            encoder: "copy".into(),
            codec: Some(codec.clone()),
            kbps: None,
            layout: None,
            sample_rate: None,
        },
        Audio::Aac { kbps } => manifest::Audio {
            encoder: "aac".into(),
            codec: None,
            kbps: Some(*kbps),
            layout: Some("stereo".into()),
            sample_rate: Some(SAMPLE_RATE),
        },
        Audio::Opus { kbps } => manifest::Audio {
            encoder: "opus".into(),
            codec: None,
            kbps: Some(*kbps),
            layout: None,
            sample_rate: None,
        },
    }
}

fn hash(path: &Path) -> Result<String, Fail> {
    sha256::sha256_file(path).map_err(|e| format!("{}: {e}", path.display()).into())
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
    let source_hash = match (&media.kind, options.json) {
        (Kind::File, true) => hash(&media.path)?,
        _ => String::new(),
    };
    let part = partial(&request.output)?;
    *partial_path = Some(part.clone());
    let progress = Progress::new(options.progress);
    let track = media.audio.as_ref().map_or(0, |a| a.index);
    if let (Some(target), Encoder::Copy { .. }) = (request.loudness, &planned.encoder) {
        let loud = level_copy(
            &media.path,
            &part,
            track,
            target,
            request.audio_bitrate,
            &progress,
        );
        progress.clear();
        let loud = loud?;
        land(&part, &request.output, request.overwrite)?;
        *partial_path = None;
        return finish(
            options,
            &request,
            &planned,
            started,
            "CPU",
            &[],
            Some(loud),
            source_hash,
        );
    }
    let level = match request.loudness {
        Some(target) => {
            let plan = media::plan_level(
                &media.path,
                track,
                &media::Prefs::default(),
                &target_of(&target),
                level_progress(&progress),
            );
            match plan {
                Ok(p) => Some((target, p)),
                Err(media::Error::Cancelled) => {
                    progress.clear();
                    return Err(Box::new(Stopped));
                }
                Err(e) => {
                    progress.clear();
                    return Err(e.to_string().into());
                }
            }
        }
        None => None,
    };
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
        level: level.as_ref().map(|(_, p)| *p),
        profile: request.profile.name(),
        alpha: media.alpha && planned.container == Container::Webm,
        stop: &signals::STOP,
        progress: &progress,
    };
    let done = pipeline::run(&job);
    let loud = match (&done, level) {
        (Ok(_), Some((target, plan))) => {
            let after = media::measure_loudness(
                &part,
                0,
                &media::Prefs::default(),
                level_progress(&progress),
            );
            Some(after.map(|after| Loud {
                target,
                before: plan.before,
                after,
                mode: plan.mode(),
            }))
        }
        _ => None,
    };
    progress.clear();
    let done = done?;
    let loud = match loud {
        Some(Ok(l)) => Some(l),
        Some(Err(media::Error::Cancelled)) => return Err(Box::new(Stopped)),
        Some(Err(e)) => return Err(e.to_string().into()),
        None => None,
    };
    land(&part, &request.output, request.overwrite)?;
    *partial_path = None;
    finish(
        options,
        &request,
        &planned,
        started,
        &done.device,
        &done.notes,
        loud,
        source_hash,
    )
}

#[allow(clippy::too_many_arguments)]
fn finish(
    options: &Options,
    request: &Request,
    planned: &Planned,
    started: Instant,
    device: &str,
    notes: &[String],
    loud: Option<Loud>,
    source_hash: String,
) -> Result<String, Fail> {
    let seconds = started.elapsed().as_secs_f64();
    if !options.json {
        let mut lines = vec![request.output.display().to_string()];
        lines.extend(notes.iter().map(|n| format!("note: {n}")));
        if let Some(l) = &loud {
            lines.push(l.lines());
        }
        return Ok(lines.join("\n"));
    }
    let bytes = std::fs::metadata(&request.output)
        .map(|m| m.len())
        .unwrap_or(0);
    let document = manifest::Manifest {
        sources: vec![manifest::Source {
            path: request.input.display().to_string(),
            sha256: source_hash,
            loudness: loud.as_ref().map(|l| stats(&l.before)),
        }],
        params: manifest::Params {
            profile: request.profile.name().into(),
            encoder: planned.encoder.manifest(),
            quality: planned.encoder.quality(),
            width: planned.size.0,
            height: planned.size.1,
            fps: Some(f64::from(planned.fps.0) / f64::from(planned.fps.1.max(1))),
            tune: request.tune.clone(),
            loudness: loud.as_ref().map(Loud::target),
            audio: planned.audio.as_ref().map(audio_manifest),
        },
        device: device.into(),
        seconds: (seconds * 100.0).round() / 100.0,
        files: vec![manifest::Output {
            file: request.output.display().to_string(),
            sha256: hash(&request.output)?,
            bytes,
            loudness: loud.as_ref().map(|l| stats(&l.after)),
        }],
        join: None,
    };
    Ok(document.to_json())
}

fn target_of(l: &super::plan::Loudness) -> media::LevelTarget {
    media::LevelTarget {
        integrated: l.integrated,
        true_peak: l.true_peak,
    }
}

fn stats(s: &media::LoudnessStats) -> manifest::Measured {
    manifest::Measured {
        integrated: s.integrated,
        range: s.range,
        true_peak: s.true_peak,
        threshold: s.threshold,
    }
}

fn mode_name(mode: media::LevelMode) -> &'static str {
    match mode {
        media::LevelMode::Linear => "linear",
        media::LevelMode::Limited => "limited",
    }
}

struct Loud {
    target: super::plan::Loudness,
    before: media::LoudnessStats,
    after: media::LoudnessStats,
    mode: media::LevelMode,
}

impl Loud {
    fn lines(&self) -> String {
        let m = |s: &media::LoudnessStats| {
            format!(
                "{:.2} LUFS, LRA {:.2} LU, true peak {:.2} dBTP",
                s.integrated, s.range, s.true_peak
            )
        };
        format!(
            "loudness before: {}\nloudness after: {} ({}, target {} LUFS, ceiling {} dBTP)",
            m(&self.before),
            m(&self.after),
            mode_name(self.mode),
            self.target.integrated,
            self.target.true_peak
        )
    }

    fn target(&self) -> manifest::Target {
        manifest::Target {
            integrated: self.target.integrated,
            true_peak: self.target.true_peak,
            mode: mode_name(self.mode).into(),
        }
    }
}

fn level_progress(progress: &Progress) -> impl FnMut(&media::LevelProgress) -> bool + '_ {
    let mut stage = None;
    move |p: &media::LevelProgress| {
        if stage != Some(p.stage) {
            stage = Some(p.stage);
            let name = match p.stage {
                media::LevelStage::Measuring => "measure",
                media::LevelStage::Calibrating => "calibrate",
                media::LevelStage::Levelling => "level",
            };
            progress.step(name, 1, 1, Some(1000));
        }
        progress.frame((p.done.clamp(0.0, 1.0) * 1000.0) as u64);
        !signals::STOP.load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn level_copy(
    input: &Path,
    part: &Path,
    track: usize,
    target: super::plan::Loudness,
    kbps: Option<u32>,
    progress: &Progress,
) -> Result<Loud, Fail> {
    let opts = media::LevelOptions {
        target: target_of(&target),
        audio_kbps: kbps,
        track,
        prefs: media::Prefs::default(),
    };
    let done = match media::level(input, part, &opts, level_progress(progress)) {
        Ok(d) => d,
        Err(media::Error::Cancelled) => return Err(Box::new(Stopped)),
        Err(e) => return Err(e.to_string().into()),
    };
    Ok(Loud {
        target,
        before: done.before,
        after: done.after,
        mode: done.mode,
    })
}

fn crf_of(rate: media::Rate) -> u8 {
    match rate {
        media::Rate::Crf(c) | media::Rate::Qp(c) => c,
        _ => 20,
    }
}

fn join(options: &Options, partial_path: &mut Option<PathBuf>) -> Result<String, Fail> {
    let started = Instant::now();
    let request = &options.request;
    let (container, _) = request.settings()?;
    let inputs: Vec<PathBuf> = std::iter::once(request.input.clone())
        .chain(request.joins.iter().cloned())
        .collect();
    if let Some(p) = inputs.iter().find(|p| p.is_dir()) {
        return Err(format!(
            "{}: join takes video files, not a folder of frames",
            p.display()
        )
        .into());
    }
    if let Some(p) = inputs.iter().find(|p| same_file(p, &request.output)) {
        return Err(format!(
            "{} is both a clip and the output: input and output are the same file",
            p.display()
        )
        .into());
    }
    if !request.overwrite && request.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            request.output.display()
        )
        .into());
    }
    let first = source::file(&inputs[0])?;
    let size = match request.size {
        Some(size) => {
            let (w, h) = first.display_size();
            Some(size.resolve(w, h)?)
        }
        None => None,
    };
    let profile = request.profile.name();
    let animation = request.tune.as_deref() == Some("animation");
    let webm = container == Container::Webm;
    let crf = if webm {
        let vp9 = crf_of(quality::vp9_rate(profile, request.quality.map(f32::from)));
        ((f32::from(vp9) * 4.0 / 7.0).round() as u8).clamp(1, 51)
    } else {
        crf_of(quality::h264_rate(
            profile,
            request.quality.map(f32::from),
            animation,
        ))
    };
    let choice = if request.profile == Profile::Fast {
        media::Choice::Gpu
    } else {
        media::Choice::Cpu
    };
    let opts = media::JoinOptions {
        encode: !request.copy_video,
        size,
        fps: request.fps.map(|f| (f, 1)),
        video: if webm {
            media::Codec::Vp9
        } else {
            media::Codec::H264
        },
        crf,
        audio_kbps: request.audio_bitrate,
        prefs: media::Prefs {
            encode: choice,
            ..media::Prefs::default()
        },
    };
    let part = partial(&request.output)?;
    *partial_path = Some(part.clone());
    let progress = Progress::new(options.progress);
    progress.step("join", 1, 1, Some(1000));
    let joined = media::join(&inputs, &part, opts, |p| {
        progress.frame((p.done.clamp(0.0, 1.0) * 1000.0) as u64);
        !signals::STOP.load(std::sync::atomic::Ordering::Relaxed)
    });
    let joined = match joined {
        Ok(j) => j,
        Err(e) => {
            progress.clear();
            return match e {
                media::Error::Cancelled => Err(Box::new(Stopped)),
                e => Err(e.to_string().into()),
            };
        }
    };
    let mut part = part;
    let loud = match request.loudness {
        Some(target) => {
            let levelled = partial(&request.output)?;
            *partial_path = Some(levelled.clone());
            let loud = level_copy(
                &part,
                &levelled,
                0,
                target,
                request.audio_bitrate,
                &progress,
            );
            let _ = std::fs::remove_file(&part);
            part = levelled;
            Some(loud?)
        }
        None => None,
    };
    progress.clear();
    land(&part, &request.output, request.overwrite)?;
    *partial_path = None;
    let seconds = started.elapsed().as_secs_f64();
    let fps = f64::from(joined.fps.0) / f64::from(joined.fps.1.max(1));
    let line = match joined.method {
        media::JoinMethod::Copy => format!(
            "joined {} clips by stream copy, no re-encode: {}",
            joined.clips, joined.reason
        ),
        media::JoinMethod::Encode => format!(
            "joined {} clips by re-encoding to {}x{} at {} fps ({}): {}",
            joined.clips,
            joined.size.0,
            joined.size.1,
            if (fps - fps.round()).abs() < 0.005 {
                format!("{fps:.0}")
            } else {
                format!("{fps:.2}")
            },
            profile,
            joined.reason
        ),
    };
    if !options.json {
        let mut text = format!("{}\n{line}", request.output.display());
        if let Some(l) = &loud {
            text.push('\n');
            text.push_str(&l.lines());
        }
        return Ok(text);
    }
    let sources = inputs
        .iter()
        .map(|p| {
            Ok(manifest::Source {
                path: p.display().to_string(),
                sha256: hash(p)?,
                loudness: None,
            })
        })
        .collect::<Result<Vec<_>, Fail>>()?;
    let codec = joined
        .video
        .to_string()
        .to_ascii_lowercase()
        .replace('.', "");
    let encoder = match joined.method {
        media::JoinMethod::Copy => Encoder::Copy { codec },
        media::JoinMethod::Encode => {
            if webm {
                request
                    .profile
                    .vp9_encoder(request.quality)
                    .unwrap_or(Encoder::Copy { codec })
            } else {
                request.profile.encoder(request.quality)
            }
        }
    };
    let audio = match joined.audio {
        media::JoinAudio::Silent => None,
        media::JoinAudio::Copied => Some(Audio::Copy {
            codec: if webm { "opus".into() } else { "aac".into() },
        }),
        media::JoinAudio::Encoded {
            codec: media::Codec::Opus,
            kbps,
        } => Some(Audio::Opus { kbps }),
        media::JoinAudio::Encoded { kbps, .. } => Some(Audio::Aac { kbps }),
    };
    let bytes = std::fs::metadata(&request.output)
        .map(|m| m.len())
        .unwrap_or(0);
    let document = manifest::Manifest {
        sources,
        params: manifest::Params {
            profile: profile.into(),
            quality: if matches!(encoder, Encoder::Copy { .. }) {
                None
            } else {
                encoder.quality()
            },
            encoder: encoder.manifest(),
            width: joined.size.0,
            height: joined.size.1,
            fps: Some(fps),
            tune: request.tune.clone(),
            loudness: loud.as_ref().map(Loud::target),
            audio: audio.as_ref().map(audio_manifest),
        },
        device: match joined.route {
            media::Route::Gpu => "GPU".into(),
            _ => "CPU".into(),
        },
        seconds: (seconds * 100.0).round() / 100.0,
        files: vec![manifest::Output {
            file: request.output.display().to_string(),
            sha256: hash(&request.output)?,
            bytes,
            loudness: loud.as_ref().map(|l| stats(&l.after)),
        }],
        join: Some(manifest::Join {
            method: joined.method.name().into(),
            reason: joined.reason.clone(),
            clips: joined.clips,
            loudness: loud.as_ref().map(|l| stats(&l.before)),
        }),
    };
    Ok(document.to_json())
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
    let outcome = if options.request.joins.is_empty() {
        convert(&options, &mut partial_path)
    } else {
        join(&options, &mut partial_path)
    };
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
