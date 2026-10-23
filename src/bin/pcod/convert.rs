use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use super::Fail;
use super::manifest;
use super::picture::{self, Still};
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
    pub wav: Option<super::extract::Wav>,
    pub add: Option<super::tracks::Add>,
    pub range: Option<Span>,
    pub still: Option<Still>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub from: f64,
    pub to: Option<f64>,
}

struct Origin {
    path: PathBuf,
    cut: manifest::Cut,
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
    encoded_audio(request, container)
}

fn encoded_audio(request: &Request, container: Container) -> Audio {
    if container == Container::Webm {
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
        Some(f) => reduced(f.0, f.1),
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

pub fn hash(path: &Path) -> Result<String, Fail> {
    sha256::sha256_file(path).map_err(|e| format!("{}: {e}", path.display()).into())
}

pub fn land(partial_path: &Path, output: &Path, overwrite: bool) -> Result<(), Fail> {
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
    let mut media = source::open(&request.input, request.fps)?;
    let mut window = None;
    let mut cut = None;
    if options.range.is_some() && media.kind == Kind::Gif {
        return Err("a GIF converts whole: --from and --to cut video files".into());
    }
    if let Some(span) = options.range {
        let to = span.to.unwrap_or(f64::INFINITY);
        if to <= span.from {
            return Err(format!("--to {to} must come after --from {}", span.from).into());
        }
        if let Some(length) = media.seconds
            && span.from >= length
        {
            return Err(format!(
                "--from {} is at or past the end of the video ({length:.3} s)",
                span.from
            )
            .into());
        }
        let (container, _) = request.settings()?;
        if media.kind == Kind::File && !media.alpha && !container.animated() {
            return ranged(options, &media, span, partial_path);
        }
        if media.kind == Kind::File {
            let frames = windowed(&mut media, span.from, to)?;
            window = Some(frames);
            cut = Some(Origin {
                path: media.path.clone(),
                cut: manifest::Cut {
                    from: frames.0 as f64 * f64::from(media.fps.1) / f64::from(media.fps.0.max(1)),
                    to: frames.1 as f64 * f64::from(media.fps.1) / f64::from(media.fps.0.max(1)),
                    frames: (frames.1 - frames.0) as u64,
                    copied: 0,
                    encoded: (frames.1 - frames.0) as u64,
                },
            });
        } else {
            media = sliced(media, span.from, to)?;
        }
    }
    let origin = cut.as_ref();
    let mut request = request.clone();
    if window.is_some() {
        request.copy_video = false;
    }
    if matches!(media.kind, Kind::Frames(_)) {
        request.fps = None;
    }
    let mut planned = plan(&request, &media)?;
    if request.no_audio {
        planned.audio = None;
    }
    if window.is_some()
        && let Some(Audio::Copy { .. }) = planned.audio
    {
        planned.audio = Some(encoded_audio(&request, planned.container));
    }
    let source_hash = match (&media.kind, options.json, origin) {
        (Kind::File, true, Some(o)) => hash(&o.path)?,
        (Kind::File | Kind::Gif, true, None) => hash(&media.path)?,
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
        loud.held()?;
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
            origin,
        );
    }
    let level = match request.loudness {
        Some(target) => {
            let encoded = match &planned.audio {
                Some(Audio::Aac { kbps }) => Some((media::Codec::Aac, *kbps)),
                Some(Audio::Opus { kbps }) => Some((media::Codec::Opus, *kbps)),
                _ => None,
            };
            let plan = media::plan_level_styled(
                &media.path,
                track,
                &media::Prefs::default(),
                &pipeline::window_span(window, media.fps).map_or(
                    media::AudioSpan::WHOLE,
                    |(from, to)| media::AudioSpan {
                        from,
                        to,
                        ..media::AudioSpan::WHOLE
                    },
                ),
                &target_of(&target),
                media::LevelStyle {
                    linear: target.linear,
                    encoded,
                },
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
        window,
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
                calibration: plan.calibration,
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
    if let Some(l) = &loud {
        l.held()?;
    }
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
        origin,
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
    origin: Option<&Origin>,
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
            path: origin
                .map_or(&request.input, |o| &o.path)
                .display()
                .to_string(),
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
        cut: origin.map(|o| o.cut.clone()),
    };
    Ok(document.to_json())
}

pub fn target_of(l: &super::plan::Loudness) -> media::LevelTarget {
    media::LevelTarget {
        integrated: l.integrated,
        true_peak: l.true_peak,
    }
}

pub fn stats(s: &media::LoudnessStats) -> manifest::Measured {
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

pub struct Loud {
    pub target: super::plan::Loudness,
    pub before: media::LoudnessStats,
    pub after: media::LoudnessStats,
    pub mode: media::LevelMode,
    pub calibration: Option<media::Calibration>,
}

pub fn ceiling_held(peak: f64, ceiling: f64) -> Result<(), Fail> {
    if peak > ceiling + 0.005 {
        return Err(format!(
            "the encoded audio peaks at {peak:.2} dBTP, over the {ceiling} dBTP ceiling, so nothing was written; ask for a lower --true-peak or a higher --audio-bitrate"
        )
        .into());
    }
    Ok(())
}

impl Loud {
    pub fn held(&self) -> Result<(), Fail> {
        ceiling_held(self.after.true_peak, self.target.true_peak)
    }

    pub fn lines(&self) -> String {
        let m = |s: &media::LoudnessStats| {
            format!(
                "{:.2} LUFS, LRA {:.2} LU, true peak {:.2} dBTP",
                s.integrated, s.range, s.true_peak
            )
        };
        let mode = match (self.target.linear, self.mode) {
            (true, media::LevelMode::Limited) => "linear gain, peaks limited",
            (_, mode) => mode_name(mode),
        };
        let short = self
            .calibration
            .and_then(|c| c.shortfall(&target_of(&self.target)))
            .map_or(String::new(), |s| format!("\nloudness short: {s}"));
        format!(
            "loudness before: {}\nloudness after: {} ({}, target {} LUFS, ceiling {} dBTP){short}",
            m(&self.before),
            m(&self.after),
            mode,
            self.target.integrated,
            self.target.true_peak
        )
    }

    pub fn target(&self) -> manifest::Target {
        manifest::Target {
            integrated: self.target.integrated,
            true_peak: self.target.true_peak,
            mode: mode_name(self.mode).into(),
            linear: self.target.linear,
            calibration: self.calibration.map(|c| manifest::Calibrated {
                passes: c.passes,
                reached: c.reached,
                landed: c.landed,
                short: c.shortfall(&target_of(&self.target)),
            }),
        }
    }
}

pub fn level_progress(progress: &Progress) -> impl FnMut(&media::LevelProgress) -> bool + '_ {
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
    let style = media::LevelStyle {
        linear: target.linear,
        encoded: None,
    };
    let done = match media::level_with(input, part, &opts, style, level_progress(progress)) {
        Ok(d) => d,
        Err(media::Error::Cancelled) => return Err(Box::new(Stopped)),
        Err(e) => return Err(e.to_string().into()),
    };
    Ok(Loud {
        target,
        before: done.before,
        after: done.after,
        mode: done.mode,
        calibration: done.calibration,
    })
}

fn crf_of(rate: media::Rate) -> u8 {
    match rate {
        media::Rate::Crf(c) | media::Rate::Qp(c) => c,
        _ => 20,
    }
}

fn join_options(
    request: &Request,
    container: Container,
    display: (u32, u32),
    encode: bool,
) -> Result<media::JoinOptions, Fail> {
    let size = match request.size {
        Some(size) => Some(size.resolve(display.0, display.1)?),
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
    Ok(media::JoinOptions {
        encode,
        size,
        fps: request.fps,
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
    })
}

fn windowed(media: &mut Media, from: f64, to: f64) -> Result<(i64, i64), Fail> {
    let first = media::first_frame_at(from, media.fps).max(0);
    let total = media
        .seconds
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| media::first_frame_at(s, media.fps))
        .ok_or_else(|| format!("{} has no known length to cut", media.path.display()))?;
    let end = if to.is_finite() {
        media::first_frame_at(to, media.fps).min(total)
    } else {
        total
    };
    if end <= first {
        return Err(format!("no frame of the video starts between {from} s and {to} s").into());
    }
    media.frames = Some((end - first) as u64);
    media.seconds =
        Some((end - first) as f64 * f64::from(media.fps.1) / f64::from(media.fps.0.max(1)));
    Ok((first, end))
}

fn sliced(mut media: Media, from: f64, to: f64) -> Result<Media, Fail> {
    let Kind::Frames(files) = &media.kind else {
        return Ok(media);
    };
    let count = i64::try_from(files.len()).unwrap_or(i64::MAX);
    let first = media::first_frame_at(from, media.fps).clamp(0, count) as usize;
    let end = media::first_frame_at(to, media.fps).clamp(0, count) as usize;
    if end <= first {
        return Err(format!("no frame of the folder starts between {from} s and {to} s").into());
    }
    let kept = files[first..end].to_vec();
    media.frames = Some(kept.len() as u64);
    media.seconds =
        Some(kept.len() as f64 * f64::from(media.fps.1) / f64::from(media.fps.0.max(1)));
    media.kind = Kind::Frames(kept);
    Ok(media)
}

fn cut_to(
    input: &Path,
    (from, to): (f64, f64),
    out: &Path,
    opts: media::JoinOptions,
    progress: &Progress,
) -> Result<media::Cut, Fail> {
    progress.step("cut", 1, 1, Some(1000));
    media::cut(input, from, to, out, opts, |p| {
        progress.frame((p.done.clamp(0.0, 1.0) * 1000.0) as u64);
        !signals::STOP.load(std::sync::atomic::Ordering::Relaxed)
    })
    .map_err(|e| match e {
        media::Error::Cancelled => Box::new(Stopped) as Fail,
        e => e.to_string().into(),
    })
}

fn cut_prefs(media: &Media) -> media::Prefs {
    let encode = if media.codec == "hevc" {
        media::Choice::Auto
    } else {
        media::Choice::Cpu
    };
    media::Prefs {
        encode,
        ..media::Prefs::default()
    }
}

fn cut_encoder(made: &media::Cut, crf: u8) -> (manifest::Encoder, Option<manifest::Quality>) {
    let codec = made.video.to_string().to_ascii_lowercase().replace('.', "");
    if made.encoded == 0 {
        return (Encoder::Copy { codec }.manifest(), None);
    }
    let crf = if made.smart {
        crf.saturating_sub(2)
    } else {
        crf
    };
    let (encoder, settings, quality) = match (made.video, made.route) {
        (media::Codec::Vp9, _) => {
            let vp9 = (u32::from(crf) * 7 / 4).min(63);
            (
                "vp9",
                vec![("crf", vp9.into())],
                manifest::Quality::Crf(vp9),
            )
        }
        (media::Codec::H264, media::Route::Gpu) => (
            "h264_gpu",
            vec![("qp", crf.into())],
            manifest::Quality::Qp(u32::from(crf)),
        ),
        (media::Codec::H264, _) => (
            "h264",
            vec![
                (
                    "preset",
                    if made.smart { "speed" } else { "balanced" }.into(),
                ),
                ("crf", crf.into()),
            ],
            manifest::Quality::Crf(u32::from(crf)),
        ),
        (media::Codec::Hevc, _) => (
            "hevc_gpu",
            vec![("qp", crf.into())],
            manifest::Quality::Qp(u32::from(crf)),
        ),
        _ => return (Encoder::Copy { codec }.manifest(), None),
    };
    (
        manifest::Encoder {
            encoder: encoder.into(),
            codec: None,
            settings,
        },
        Some(quality),
    )
}

fn cut_tune(request: &Request, container: Container) -> Result<Option<String>, Fail> {
    match request.tune.as_deref() {
        None => Ok(None),
        Some("animation") if container != Container::Webm => Ok(Some("animation".into())),
        Some(tune) => Err(format!(
            "--tune {tune} doesn't apply to a --from/--to cut, only --tune animation does, into mp4, mov or mkv, and it re-encodes the whole window"
        )
        .into()),
    }
}

fn cut_reported_tune(tune: Option<String>, made: &media::Cut) -> Option<String> {
    let tabled = matches!(made.video, media::Codec::H264 | media::Codec::Hevc);
    tune.filter(|_| made.encoded > 0 && tabled)
}

fn cut_line(c: &media::Cut) -> String {
    let span = format!("cut {:.3} s to {:.3} s, {} frames", c.from, c.to, c.frames);
    let route = match c.route {
        media::Route::Gpu => "GPU",
        _ => "CPU",
    };
    if c.encoded == 0 {
        format!("{span}, all copied, no re-encode")
    } else if c.copied == 0 {
        format!("{span}, all re-encoded ({} on the {route})", c.video)
    } else {
        format!(
            "{span}: {} copied, {} re-encoded at the edges ({} on the {route})",
            c.copied, c.encoded, c.video
        )
    }
}

fn cut_manifest(c: &media::Cut) -> manifest::Cut {
    manifest::Cut {
        from: c.from,
        to: c.to,
        frames: c.frames,
        copied: c.copied,
        encoded: c.encoded,
    }
}

fn ranged(
    options: &Options,
    media: &Media,
    span: Span,
    partial_path: &mut Option<PathBuf>,
) -> Result<String, Fail> {
    let started = Instant::now();
    let request = &options.request;
    let (container, _) = request.settings()?;
    let tune = cut_tune(request, container)?;
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
    let window = (span.from, span.to.unwrap_or(f64::INFINITY));
    let mut opts = join_options(
        request,
        container,
        media.display_size(),
        !request.copy_video,
    )?;
    if !opts.encode {
        opts.prefs = cut_prefs(media);
    }
    let crf = opts.crf;
    let part = partial(&request.output)?;
    *partial_path = Some(part.clone());
    let progress = Progress::new(options.progress);
    let made = match cut_to(&request.input, window, &part, opts, &progress) {
        Ok(m) => m,
        Err(e) => {
            progress.clear();
            return Err(e);
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
            progress.clear();
            let loud = loud?;
            loud.held()?;
            Some(loud)
        }
        None => None,
    };
    progress.clear();
    land(&part, &request.output, request.overwrite)?;
    *partial_path = None;
    let seconds = started.elapsed().as_secs_f64();
    if !options.json {
        let mut lines = vec![request.output.display().to_string(), cut_line(&made)];
        lines.extend(made.reasons.iter().map(|n| format!("note: {n}")));
        if let Some(l) = &loud {
            lines.push(l.lines());
        }
        return Ok(lines.join("\n"));
    }
    let (encoder, quality) = cut_encoder(&made, crf);
    let audio = match made.audio {
        media::JoinAudio::Silent => None,
        media::JoinAudio::Copied => Some(Audio::Copy {
            codec: if container == Container::Webm {
                "opus".into()
            } else {
                "aac".into()
            },
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
        sources: vec![manifest::Source {
            path: request.input.display().to_string(),
            sha256: hash(&request.input)?,
            loudness: loud.as_ref().map(|l| stats(&l.before)),
        }],
        params: manifest::Params {
            profile: request.profile.name().into(),
            quality,
            encoder,
            width: made.size.0,
            height: made.size.1,
            fps: Some(f64::from(made.fps.0) / f64::from(made.fps.1.max(1))),
            tune: cut_reported_tune(tune, &made),
            loudness: loud.as_ref().map(Loud::target),
            audio: audio.as_ref().map(audio_manifest),
        },
        device: match made.route {
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
        join: None,
        cut: Some(cut_manifest(&made)),
    };
    Ok(document.to_json())
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
    let profile = request.profile.name();
    let webm = container == Container::Webm;
    let opts = join_options(
        request,
        container,
        first.display_size(),
        !request.copy_video,
    )?;
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
            let loud = loud?;
            loud.held()?;
            Some(loud)
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
        cut: None,
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
    let raw = options.wav.as_ref().is_some_and(|w| w.raw);
    let outcome = if let Some(still) = options.still {
        picture::convert(
            &picture::Job {
                input: &options.request.input,
                output: &options.request.output,
                size: options.request.size,
                still,
                overwrite: options.request.overwrite,
                json: options.json,
            },
            &mut partial_path,
        )
    } else if let Some(wav) = options.wav.as_ref().filter(|w| w.raw) {
        super::raw::run(&options, wav, &mut partial_path, &signals::STOP)
    } else if let Some(wav) = &options.wav {
        super::extract::run(&options, wav, &mut partial_path)
    } else if let Some(add) = &options.add {
        super::tracks::run(&options, add, &mut partial_path)
    } else if options.request.joins.is_empty() {
        convert(&options, &mut partial_path)
    } else {
        join(&options, &mut partial_path)
    };
    if let Some(p) = &partial_path {
        let _ = std::fs::remove_file(p);
    }
    match outcome {
        Ok(text) if raw && text.is_empty() => ExitCode::SUCCESS,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pito_codecs::gpu::Color;

    fn folder(count: usize) -> Media {
        Media {
            path: PathBuf::from("frames"),
            kind: Kind::Frames(
                (0..count)
                    .map(|k| PathBuf::from(format!("{k:06}.png")))
                    .collect(),
            ),
            width: 64,
            height: 64,
            fps: (24, 1),
            seconds: Some(count as f64 / 24.0),
            frames: Some(count as u64),
            codec: "png".into(),
            bit_depth: 8,
            alpha: false,
            color: Color::UNKNOWN,
            rotation: 0,
            sar: (1, 1),
            audio: None,
        }
    }

    #[test]
    fn an_alpha_window_keeps_the_frames_that_start_in_it() {
        let mut m = folder(120);
        m.kind = Kind::File;
        assert_eq!(windowed(&mut m, 2.0, 3.5).unwrap(), (48, 84));
        assert_eq!(m.frames, Some(36));
        assert!((m.seconds.unwrap() - 1.5).abs() < 1e-9);
        let mut m = folder(120);
        assert_eq!(windowed(&mut m, 2.01, f64::INFINITY).unwrap(), (49, 120));
        let mut m = folder(120);
        assert_eq!(windowed(&mut m, 4.0, 9.0).unwrap(), (96, 120));
        let mut m = folder(120);
        assert!(windowed(&mut m, 2.01, 2.04).is_err());
        let mut m = folder(120);
        m.seconds = None;
        let e = windowed(&mut m, 1.0, 2.0).unwrap_err().to_string();
        assert!(e.contains("no known length"), "{e}");
    }

    fn names(media: &Media) -> Vec<String> {
        let Kind::Frames(files) = &media.kind else {
            panic!("not a folder");
        };
        files.iter().map(|p| p.display().to_string()).collect()
    }

    fn made(video: media::Codec, route: media::Route, encoded: u64) -> media::Cut {
        media::Cut {
            path: PathBuf::from("out.mp4"),
            from: 2.0,
            to: 3.5,
            size: (384, 240),
            fps: (24, 1),
            frames: 36,
            copied: 36 - encoded,
            encoded,
            smart: true,
            video,
            route,
            audio: media::JoinAudio::Silent,
            reasons: Vec::new(),
        }
    }

    fn setting(e: &manifest::Encoder, key: &str) -> String {
        let found = e.settings.iter().find(|(k, _)| *k == key);
        let mut o = String::new();
        if let Some((_, v)) = found {
            v.write(&mut o);
        }
        o
    }

    #[test]
    fn the_manifest_names_the_encoder_that_made_the_edges() {
        let (e, q) = cut_encoder(&made(media::Codec::Hevc, media::Route::Gpu, 0), 12);
        assert_eq!(e.encoder, "copy");
        assert_eq!(e.codec.as_deref(), Some("hevc"));
        assert!(e.settings.is_empty());
        assert_eq!(q, None);
        let (e, q) = cut_encoder(&made(media::Codec::Hevc, media::Route::Gpu, 12), 12);
        assert_eq!(e.encoder, "hevc_gpu");
        assert_eq!(q, Some(manifest::Quality::Qp(10)));
        let (e, q) = cut_encoder(&made(media::Codec::H264, media::Route::Gpu, 36), 20);
        assert_eq!(e.encoder, "h264_gpu");
        assert_eq!(q, Some(manifest::Quality::Qp(18)));
        let (e, q) = cut_encoder(&made(media::Codec::Av1, media::Route::Cpu, 36), 20);
        assert_eq!(e.encoder, "copy");
        assert_eq!(q, None);
    }

    #[test]
    fn smart_edges_report_the_speed_preset_and_the_edge_crf() {
        let (e, q) = cut_encoder(&made(media::Codec::H264, media::Route::Cpu, 12), 18);
        assert_eq!(e.encoder, "h264");
        assert_eq!(setting(&e, "preset"), "\"speed\"");
        assert_eq!(setting(&e, "crf"), "16");
        assert_eq!(q, Some(manifest::Quality::Crf(16)));
        let (e, q) = cut_encoder(&made(media::Codec::Vp9, media::Route::Cpu, 12), 18);
        assert_eq!(e.encoder, "vp9");
        assert_eq!(setting(&e, "crf"), "28");
        assert_eq!(q, Some(manifest::Quality::Crf(28)));
        let (_, q) = cut_encoder(&made(media::Codec::H264, media::Route::Cpu, 12), 1);
        assert_eq!(
            q,
            Some(manifest::Quality::Crf(0)),
            "the edge crf stops at 0"
        );
    }

    #[test]
    fn a_whole_window_reports_the_balanced_preset_and_the_crf_it_was_given() {
        let whole = |video| media::Cut {
            copied: 0,
            smart: false,
            ..made(video, media::Route::Cpu, 36)
        };
        let (e, q) = cut_encoder(&whole(media::Codec::H264), 18);
        assert_eq!(setting(&e, "preset"), "\"balanced\"");
        assert_eq!(setting(&e, "crf"), "18");
        assert_eq!(q, Some(manifest::Quality::Crf(18)));
        let (e, q) = cut_encoder(&whole(media::Codec::Vp9), 40);
        assert_eq!(setting(&e, "crf"), "63", "VP9's scale stops at 63");
        assert_eq!(q, Some(manifest::Quality::Crf(63)));
    }

    #[test]
    fn a_cut_takes_only_the_animation_tune_and_only_into_h264() {
        let mut request = Request::new("in.mp4", "out.mp4");
        assert_eq!(cut_tune(&request, Container::Mp4).unwrap(), None);
        request.tune = Some("animation".into());
        assert_eq!(
            cut_tune(&request, Container::Mp4).unwrap().as_deref(),
            Some("animation")
        );
        let e = cut_tune(&request, Container::Webm).unwrap_err().to_string();
        assert!(e.contains("--tune animation"), "{e}");
        for tune in ["film", "grain", "stillimage", "fastdecode", "zerolatency"] {
            request.tune = Some(tune.into());
            let e = cut_tune(&request, Container::Mp4).unwrap_err().to_string();
            assert!(e.starts_with(&format!("--tune {tune} ")), "{e}");
            assert_eq!(e.lines().count(), 1, "{e}");
        }
    }

    #[test]
    fn the_tune_is_reported_for_every_edge_the_animation_table_set() {
        let tune = || Some("animation".to_string());
        for video in [media::Codec::H264, media::Codec::Hevc] {
            let edges = made(video, media::Route::Gpu, 12);
            assert_eq!(
                cut_reported_tune(tune(), &edges).as_deref(),
                Some("animation"),
                "{video}"
            );
            let copied = made(video, media::Route::Gpu, 0);
            assert_eq!(cut_reported_tune(tune(), &copied), None, "{video}");
            assert_eq!(cut_reported_tune(None, &edges), None, "{video}");
        }
        let vp9 = made(media::Codec::Vp9, media::Route::Cpu, 12);
        assert_eq!(cut_reported_tune(tune(), &vp9), None);
    }

    #[test]
    fn a_folder_keeps_the_frames_that_start_in_the_window() {
        let m = sliced(folder(48), 0.25, 0.5).unwrap();
        assert_eq!(
            names(&m),
            (6..12).map(|k| format!("{k:06}.png")).collect::<Vec<_>>()
        );
        assert_eq!(m.frames, Some(6));
        assert!((m.seconds.unwrap() - 0.25).abs() < 1e-9);
        let m = sliced(folder(48), 0.26, 0.5).unwrap();
        assert_eq!(names(&m).first().map(String::as_str), Some("000007.png"));
    }

    #[test]
    fn a_folder_window_stops_at_the_last_frame() {
        let m = sliced(folder(48), 1.5, 9.0).unwrap();
        assert_eq!(m.frames, Some(12));
        assert_eq!(names(&m).last().map(String::as_str), Some("000047.png"));
    }

    #[test]
    fn a_folder_window_past_the_end_is_refused() {
        assert!(sliced(folder(48), 2.0, 3.0).is_err());
        assert!(sliced(folder(48), 0.01, 0.02).is_err());
    }

    #[test]
    fn a_file_passes_through_untouched() {
        let m = Media {
            kind: Kind::File,
            ..folder(1)
        };
        assert_eq!(sliced(m.clone(), 1.0, 2.0).unwrap(), m);
    }
}
