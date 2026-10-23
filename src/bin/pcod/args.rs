use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, ValueEnum};

use super::Cli;
use super::convert::{Options, Span};
use super::extract::{
    OPUS_KBPS_HIGHEST, OPUS_KBPS_LOWEST, OPUS_KBPS_MONO, OPUS_KBPS_STEREO, RATES, Wav, is_opus,
    is_wav,
};
use super::picture::{self, Still};
use super::plan::{AUDIO_KBPS_HIGHEST, AUDIO_KBPS_LOWEST, Loudness, Request};
use super::profile::{ANIMATED_FPS, ANIMATED_WIDTH, LIGHT_WEB_CRF, Profile, VP9_LIGHT_WEB_CRF};
use super::raw::is_raw;
use super::size::Size;
use super::tracks::Add;

pub const USAGE: &str = "pcod [OPTIONS] <INPUT> <OUTPUT>\n       pcod <COMMAND>";

pub fn help() -> String {
    format!(
        "The output's extension picks the container: mp4, mov, mkv (H.264 and AAC), webm (VP9 and Opus), gif or webp (animated, looping, {ANIMATED_WIDTH} px wide at {ANIMATED_FPS} fps by default, or the source's width and rate where smaller). --from and --to cut a window of the input, video and audio in sync, starting at 0: the first frame at or after --from to the last before --to. With no video option the cut copies whole GOPs and re-encodes only the frames at its edges where the container carries the source's codec; a video option re-encodes it all. A .wav output is the audio alone: the first audio track as 16-bit PCM, stereo at the source's rate when it is 44100 or 48000 Hz (else 48000), with --from and --to for a range, --mono, --rate and --loudness. A .s16 output, or - for stdout, is one audio stream as raw 16-bit little-endian PCM from --from to its end: --track N picks the stream as the container numbers it (the first audio stream by default), --mono mixes it to one channel and --rate resamples it (the stream's own channels and rate by default). A .opus (or .ogg) output is the audio alone as Ogg Opus at 48000 Hz, 64k stereo or 32k with --mono by default, --audio-bitrate KBPS from 16 to 510, with --from, --to, --mono and --loudness. The input is a video file, an animated GIF or a folder of PNG frames in name order (--fps gives their rate). A PNG, JPEG or WebP still to a .webp writes a still WebP (no animation) at the source's size or --size, resized with Lanczos: lossy at --quality 0 to 100 on libwebp's scale (75 by default), or --lossless (VP8L at method 6, as ffmpeg's compression_level 6; fully transparent pixels keep their alpha, not their colour, as libwebp does by default), keeping alpha; to a .png it writes a PNG. A JPEG is turned upright by its EXIF orientation first. probe also reads PNG, JPEG, WebP and GIF files; a GIF's frame rate is 100 over its delay when every delay matches, else the whole or NTSC rate its frames start on to the hundredth of a second or its mean comes within 1% of, else that mean.
Profiles: balanced (the default) and best are H.264 on the CPU at CRF 20 and 18 on x264's scale; fast is H.264 on the GPU at QP 25, on the CPU where no GPU encoder is found. For .webm, balanced is VP9 at CRF 32 and best at 28 on libvpx's scale.
Light web: --profile best --crf {LIGHT_WEB_CRF} (.webm: --crf {VP9_LIGHT_WEB_CRF})
For screen recordings: --tune animation
--size takes a width (the height follows the aspect), WxH within 1% of the aspect, or 720p, 1080p, 1440p, 2160p; pcod never stretches a picture.
Audio is AAC-LC 320k stereo at 48000 Hz, or Opus 128k in .webm; an AAC source is copied when nothing changes its audio. --audio-bitrate KBPS (32–512) sets the rate.
--no-audio writes the video alone: with no video option it is copied as it is, index first. --add-audio WAV keeps the video and the first audio track as they are and adds the WAV as another AAC track, 160k stereo at 48000 Hz (--audio-bitrate sets it), the file cut to the shorter of the video and the WAV; --title Voice,Music titles each audio track in order; .mp4 or .mov only.
An existing output is refused unless --force is given; pcod writes a hidden partial file beside the output and renames it into place at the end.
While it works, pcod draws one progress line on stderr when stderr is a terminal; never with --json, --no-progress, TERM=dumb or when stderr is a pipe or a file. --json prints one JSON manifest with each file's sha256.
Exit codes: 0 it worked, 1 refused or failed (one line on stderr starting pcod:), 2 the command line is wrong, 128 plus the signal number when a signal stopped it."
    )
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    Fast,
    Balanced,
    Best,
}

pub fn size_of(text: &str) -> Result<Size, String> {
    Size::parse(text)
}

pub fn rate_hz(text: &str) -> Result<u32, String> {
    let (lowest, highest) = (
        pito_codecs::media::LOWEST_RATE,
        pito_codecs::media::HIGHEST_RATE,
    );
    text.parse()
        .ok()
        .filter(|r| (lowest..=highest).contains(r))
        .ok_or_else(|| format!("use 44100 or 48000 for a .wav, {lowest} to {highest} for raw PCM"))
}

pub fn loudness_of(text: &str) -> Result<Loudness, String> {
    Loudness::parse(text)
}

pub fn loudness_with(
    loudness: Option<Loudness>,
    true_peak: Option<f64>,
    linear: bool,
) -> Option<Loudness> {
    loudness.map(|mut l| {
        if let Some(tp) = true_peak {
            l.true_peak = tp;
        }
        l.linear = linear;
        l
    })
}

#[derive(clap::Args, Clone, Default)]
pub struct ConvertArgs {
    #[arg(long, value_enum, help = "fast, balanced (the default) or best")]
    pub profile: Option<ProfileArg>,
    #[arg(long, value_parser = size_of, help = "A width, WxH within 1% of the aspect, or 720p, 1080p, 1440p, 2160p")]
    pub size: Option<Size>,
    #[arg(
        long,
        value_parser = rate_of,
        help = "The frame rate, 1 to 240: an integer, a ratio such as 24000/1001, or a decimal (23.976, 29.97, 47.952 and 59.94 are 24000/1001, 30000/1001, 48000/1001 and 60000/1001; for a folder of PNG frames, their rate)"
    )]
    pub fps: Option<(u32, u32)>,
    #[arg(
        long,
        conflicts_with = "qp",
        help = "Quality for balanced and best, on x264's (or libvpx's) scale"
    )]
    pub crf: Option<u8>,
    #[arg(long, help = "Quality for fast")]
    pub qp: Option<u8>,
    #[arg(
        long,
        help = "film, animation, grain, stillimage, fastdecode or zerolatency"
    )]
    pub tune: Option<String>,
    #[arg(long, allow_hyphen_values = true, value_parser = loudness_of, help = "Level the audio to LUFS (-70 to -5) or youtube")]
    pub loudness: Option<Loudness>,
    #[arg(
        long,
        allow_hyphen_values = true,
        requires = "loudness",
        help = "The true-peak ceiling in dBTP [default: -1]"
    )]
    pub true_peak: Option<f64>,
    #[arg(
        long,
        requires = "loudness",
        help = "Keep the dynamics: gain to the target, then only a true-peak limiter at the ceiling (may land under the target)"
    )]
    pub linear: bool,
    #[arg(long, help = "The audio bitrate in kbps, 32 to 512 (.opus: 16 to 510)")]
    pub audio_bitrate: Option<u32>,
    #[arg(long, value_parser = super::time_of, help = "Start at this time (seconds, m:ss or h:mm:ss): the first frame at or after it")]
    pub from: Option<f64>,
    #[arg(long, value_parser = super::time_of, help = "Stop before this time [default: the end]")]
    pub to: Option<f64>,
    #[arg(long, help = "For a .wav, .opus, .s16 or -: one channel")]
    pub mono: bool,
    #[arg(long, value_parser = rate_hz, value_name = "HZ", help = "For a .wav: 44100 or 48000 [default: the source's when it is one of them, else 48000]; for .s16 or -: 8000 to 768000 [default: the stream's]")]
    pub rate: Option<u32>,
    #[arg(
        long,
        value_parser = clap::value_parser!(u8).range(0..=100),
        help = "For a still .webp: lossy quality on libwebp's scale, 0 to 100 [default: 75]"
    )]
    pub quality: Option<u8>,
    #[arg(
        long,
        conflicts_with = "quality",
        help = "For a still .webp: lossless (VP8L at method 6, as ffmpeg's compression_level 6)"
    )]
    pub lossless: bool,
    #[arg(long, value_parser = super::stills::crop_of, help = "For one PNG to a PNG: write only this rectangle, W:H:X:Y")]
    pub crop: Option<super::stills::Crop>,
    #[arg(
        long,
        help = "Write no audio track; with no video option the video is copied as it is, index first"
    )]
    pub no_audio: bool,
    #[arg(
        long,
        value_name = "WAV",
        conflicts_with = "no_audio",
        help = "Keep the video and the first audio track as they are and add this WAV as another AAC track (160 kbps, --audio-bitrate sets it), cut to the shorter of the video and the WAV; .mp4 or .mov"
    )]
    pub add_audio: Option<PathBuf>,
    #[arg(
        long,
        value_name = "TITLES",
        value_delimiter = ',',
        requires = "add_audio",
        help = "With --add-audio: a title for each audio track, in order, separated by commas"
    )]
    pub title: Vec<String>,
    #[arg(
        long,
        value_name = "N",
        help = "For .s16 or -: the audio stream, numbered as the container numbers its streams (ffmpeg's -map 0:N) [default: the first audio stream]"
    )]
    pub track: Option<usize>,
    #[arg(long, help = "Replace an existing output")]
    pub force: bool,
    #[arg(long, help = "Print a JSON manifest instead of the plain result")]
    pub json: bool,
    #[arg(long, help = "Draw no progress line")]
    pub no_progress: bool,
}

fn word(text: &Path) -> bool {
    let s = text.to_string_lossy();
    !s.contains(['.', '/', '\\']) && !text.exists()
}

fn command_for(sub: Option<&str>) -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    match sub {
        Some(name) => cmd
            .find_subcommand(name)
            .cloned()
            .unwrap_or_else(Cli::command),
        None => cmd,
    }
}

pub fn usage_error(sub: Option<&str>, kind: ErrorKind, message: impl std::fmt::Display) -> ! {
    command_for(sub).error(kind, message).exit()
}

fn missing(sub: Option<&str>, names: &[&str]) -> ! {
    usage_error(
        sub,
        ErrorKind::MissingRequiredArgument,
        format!(
            "the following required arguments were not provided:\n  {}",
            names.join("\n  ")
        ),
    )
}

pub fn bare(input: Option<PathBuf>, output: Option<PathBuf>, args: ConvertArgs) -> Options {
    match (input, output) {
        (None, _) => missing(None, &["<INPUT>", "<OUTPUT>"]),
        (Some(input), None) if word(&input) => usage_error(
            None,
            ErrorKind::InvalidSubcommand,
            format!("unrecognized subcommand '{}'", input.display()),
        ),
        (Some(_), None) => missing(None, &["<OUTPUT>"]),
        (Some(input), Some(output)) => options(None, input, Vec::new(), output, args),
    }
}

pub fn options(
    sub: Option<&str>,
    input: PathBuf,
    joins: Vec<PathBuf>,
    output: PathBuf,
    args: ConvertArgs,
) -> Options {
    let joining = sub == Some("join");
    if std::iter::once(&input)
        .chain(&joins)
        .chain(std::iter::once(&output))
        .any(|p| p.to_str().is_none())
    {
        usage_error(
            sub,
            ErrorKind::InvalidUtf8,
            "invalid UTF-8 was detected in one or more arguments",
        );
    }
    if let Some(crop) = args.crop {
        return png_crop(sub, input, joining, output, crop, args);
    }
    if is_raw(&output) {
        return raw(sub, input, joining, output, args);
    }
    if args.track.is_some() {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--track fits a .s16 or - output",
        );
    }
    if is_wav(&output) || is_opus(&output) {
        return wav(sub, input, joining, output, args);
    }
    if !joining && (is_webp(&output) || picture::is_png(&output)) && picture::still_input(&input) {
        return still(sub, input, output, args);
    }
    if args.quality.is_some() || args.lossless {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--quality and --lossless fit a still .webp made from a .png, .jpg or .webp",
        );
    }
    if let Some(wav) = args.add_audio.clone() {
        return added(sub, input, joining, output, wav, args);
    }
    let ranged = args.from.is_some() || args.to.is_some();
    if joining && ranged {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "join takes whole clips: cut each one with --from and --to first, then join them",
        );
    }
    if args.mono || args.rate.is_some() {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--mono fits a .wav or .opus output, --rate a .wav",
        );
    }
    if args.no_audio && (joining || ranged) {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--no-audio takes one whole file: cut or join first, then drop the audio",
        );
    }
    if args.no_audio && (args.loudness.is_some() || args.audio_bitrate.is_some()) {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--no-audio writes no audio for --loudness or --audio-bitrate to change",
        );
    }
    let picture = args.profile.is_some()
        || args.size.is_some()
        || args.fps.is_some()
        || args.crf.is_some()
        || args.qp.is_some()
        || args.tune.is_some();
    let profile = match args.profile.unwrap_or(ProfileArg::Balanced) {
        ProfileArg::Fast => Profile::Fast,
        ProfileArg::Balanced => Profile::Balanced,
        ProfileArg::Best => Profile::Best,
    };
    let folder = input.is_dir();
    if folder && args.fps.is_none() {
        missing(sub, &["--fps <FPS>"]);
    }
    let loudness = loudness_with(args.loudness, args.true_peak, args.linear);
    let mut request = Request::new(input, output);
    request.joins = joins;
    request.profile = profile;
    request.size = args.size;
    request.fps = args.fps;
    request.quality = args.crf.or(args.qp);
    request.tune = args.tune;
    request.overwrite = args.force;
    request.copy_video =
        (joining || ranged || loudness.is_some() || args.audio_bitrate.is_some() || args.no_audio)
            && !picture
            && !folder;
    request.no_audio = args.no_audio;
    request.loudness = loudness;
    request.audio_bitrate = args.audio_bitrate;
    let mut checked = request.clone();
    if folder {
        checked.fps = None;
        checked.copy_video = false;
    }
    if let Err(e) = checked.settings() {
        usage_error(sub, ErrorKind::ValueValidation, e);
    }
    if profile == Profile::Fast && args.crf.is_some() {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--crf fits balanced or best; use --qp with fast",
        );
    }
    if profile != Profile::Fast && args.qp.is_some() {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--qp fits fast; use --crf with balanced or best",
        );
    }
    Options {
        request,
        json: args.json,
        progress: super::progress::enabled(args.json, args.no_progress),
        wav: None,
        add: None,
        range: ranged.then(|| Span {
            from: args.from.unwrap_or(0.0),
            to: args.to,
        }),
        still: None,
        crop: None,
    }
}

fn is_webp(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("webp"))
}

fn still(sub: Option<&str>, input: PathBuf, output: PathBuf, args: ConvertArgs) -> Options {
    let video = [
        (args.profile.is_some(), "--profile"),
        (args.fps.is_some(), "--fps"),
        (args.crf.is_some(), "--crf"),
        (args.qp.is_some(), "--qp"),
        (args.tune.is_some(), "--tune"),
        (args.loudness.is_some(), "--loudness"),
        (args.audio_bitrate.is_some(), "--audio-bitrate"),
        (args.from.is_some(), "--from"),
        (args.to.is_some(), "--to"),
        (args.mono, "--mono"),
        (args.rate.is_some(), "--rate"),
        (args.no_audio, "--no-audio"),
        (args.add_audio.is_some(), "--add-audio"),
    ];
    if let Some((_, flag)) = video.iter().find(|(set, _)| *set) {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            format!("{flag} doesn't apply to a still image output"),
        );
    }
    if picture::is_png(&output) && (args.quality.is_some() || args.lossless) {
        usage_error(
            sub,
            ErrorKind::ArgumentConflict,
            "--quality and --lossless fit a .webp; a .png still is always lossless",
        );
    }
    let mut request = Request::new(input, output);
    request.size = args.size;
    request.overwrite = args.force;
    Options {
        request,
        json: args.json,
        progress: super::progress::enabled(args.json, args.no_progress),
        wav: None,
        add: None,
        range: None,
        still: Some(Still {
            quality: args.quality,
            lossless: args.lossless,
        }),
        crop: None,
    }
}

fn wav(
    sub: Option<&str>,
    input: PathBuf,
    joining: bool,
    output: PathBuf,
    args: ConvertArgs,
) -> Options {
    let conflict = |message: &str| usage_error(sub, ErrorKind::ArgumentConflict, message);
    let opus = is_opus(&output);
    let kind = if opus { ".opus" } else { ".wav" };
    if joining {
        conflict(&format!(
            "join writes mp4, mov, mkv or webm, not {kind}: join first, then take the {kind} from the joined file"
        ));
    }
    if input.is_dir() {
        conflict(&format!(
            "a {kind} takes its audio from a video or audio file, not a folder of frames"
        ));
    }
    let picture = [
        (args.profile.is_some(), "--profile"),
        (args.size.is_some(), "--size"),
        (args.fps.is_some(), "--fps"),
        (args.crf.is_some(), "--crf"),
        (args.qp.is_some(), "--qp"),
        (args.tune.is_some(), "--tune"),
        (args.audio_bitrate.is_some() && !opus, "--audio-bitrate"),
        (args.rate.is_some() && opus, "--rate"),
        (args.quality.is_some(), "--quality"),
        (args.lossless, "--lossless"),
        (args.no_audio, "--no-audio"),
        (args.add_audio.is_some(), "--add-audio"),
    ];
    if let Some((_, flag)) = picture.iter().find(|(set, _)| *set) {
        conflict(&if opus {
            format!("{flag} doesn't apply to .opus, which is audio alone at 48000 Hz")
        } else {
            format!("{flag} doesn't apply to .wav, which is uncompressed audio")
        });
    }
    if !opus
        && let Some(rate) = args.rate
        && !RATES.contains(&rate)
    {
        usage_error(
            sub,
            ErrorKind::ValueValidation,
            format!("a .wav takes --rate 44100 or 48000, not {rate}"),
        );
    }
    if opus
        && let Some(kbps) = args.audio_bitrate
        && !(OPUS_KBPS_LOWEST..=OPUS_KBPS_HIGHEST).contains(&kbps)
    {
        usage_error(
            sub,
            ErrorKind::ValueValidation,
            format!(
                "an .opus bitrate of {kbps} kbps is outside {OPUS_KBPS_LOWEST}–{OPUS_KBPS_HIGHEST}"
            ),
        );
    }
    let from = args.from.unwrap_or(0.0);
    let loudness = loudness_with(args.loudness, args.true_peak, args.linear);
    if let Some(l) = &loudness
        && let Err(e) = l.check()
    {
        usage_error(sub, ErrorKind::ValueValidation, e);
    }
    let mut request = Request::new(input, output);
    request.overwrite = args.force;
    request.loudness = loudness;
    Options {
        request,
        json: args.json,
        progress: super::progress::enabled(args.json, args.no_progress),
        wav: Some(Wav {
            from,
            to: args.to,
            mono: args.mono,
            rate: args.rate,
            track: None,
            raw: false,
            opus: opus.then(|| {
                args.audio_bitrate.unwrap_or(if args.mono {
                    OPUS_KBPS_MONO
                } else {
                    OPUS_KBPS_STEREO
                })
            }),
        }),
        add: None,
        range: None,
        still: None,
        crop: None,
    }
}

fn added(
    sub: Option<&str>,
    input: PathBuf,
    joining: bool,
    output: PathBuf,
    wav: PathBuf,
    args: ConvertArgs,
) -> Options {
    let conflict = |message: &str| usage_error(sub, ErrorKind::ArgumentConflict, message);
    if joining {
        conflict("--add-audio works on one file, not join: join first, then add the track");
    }
    if input.is_dir() {
        conflict("--add-audio copies the video of a file, not a folder of frames");
    }
    if wav.to_str().is_none() {
        usage_error(
            sub,
            ErrorKind::InvalidUtf8,
            "invalid UTF-8 was detected in one or more arguments",
        );
    }
    let changes = [
        (args.profile.is_some(), "--profile"),
        (args.size.is_some(), "--size"),
        (args.fps.is_some(), "--fps"),
        (args.crf.is_some(), "--crf"),
        (args.qp.is_some(), "--qp"),
        (args.tune.is_some(), "--tune"),
        (args.loudness.is_some(), "--loudness"),
        (args.from.is_some(), "--from"),
        (args.to.is_some(), "--to"),
        (args.mono, "--mono"),
        (args.rate.is_some(), "--rate"),
    ];
    if let Some((_, flag)) = changes.iter().find(|(set, _)| *set) {
        conflict(&format!(
            "{flag} doesn't apply with --add-audio, which copies the video and the first audio track as they are"
        ));
    }
    if let Some(kbps) = args.audio_bitrate
        && !(AUDIO_KBPS_LOWEST..=AUDIO_KBPS_HIGHEST).contains(&kbps)
    {
        usage_error(
            sub,
            ErrorKind::ValueValidation,
            format!(
                "audio bitrate {kbps} kbps is outside {AUDIO_KBPS_LOWEST}–{AUDIO_KBPS_HIGHEST}"
            ),
        );
    }
    let mut request = Request::new(input, output);
    request.overwrite = args.force;
    request.audio_bitrate = args.audio_bitrate;
    Options {
        request,
        json: args.json,
        progress: super::progress::enabled(args.json, args.no_progress),
        wav: None,
        add: Some(Add {
            wav,
            titles: args.title,
        }),
        range: None,
        still: None,
        crop: None,
    }
}

pub fn crop_conflict(
    input: &Path,
    joining: bool,
    output: &Path,
    args: &ConvertArgs,
) -> Option<String> {
    if joining {
        return Some("--crop cuts one PNG; join takes clips".into());
    }
    if input.is_dir() || !picture::is_png(input) || !picture::is_png(output) {
        return Some(
            "--crop here cuts one PNG into a PNG (pcod in.png out.png --crop W:H:X:Y); for video frames use pcod stills --crop"
                .into(),
        );
    }
    let ConvertArgs {
        profile,
        size,
        fps,
        crf,
        qp,
        tune,
        loudness,
        true_peak,
        linear,
        audio_bitrate,
        from,
        to,
        mono,
        rate,
        quality,
        lossless,
        track,
        no_audio,
        add_audio,
        title,
        crop: _,
        force: _,
        json: _,
        no_progress: _,
    } = args;
    let flags = [
        (profile.is_some(), "--profile"),
        (size.is_some(), "--size"),
        (fps.is_some(), "--fps"),
        (crf.is_some(), "--crf"),
        (qp.is_some(), "--qp"),
        (tune.is_some(), "--tune"),
        (loudness.is_some(), "--loudness"),
        (true_peak.is_some(), "--true-peak"),
        (*linear, "--linear"),
        (audio_bitrate.is_some(), "--audio-bitrate"),
        (from.is_some(), "--from"),
        (to.is_some(), "--to"),
        (*mono, "--mono"),
        (rate.is_some(), "--rate"),
        (quality.is_some(), "--quality"),
        (*lossless, "--lossless"),
        (track.is_some(), "--track"),
        (*no_audio, "--no-audio"),
        (add_audio.is_some(), "--add-audio"),
        (!title.is_empty(), "--title"),
    ];
    flags
        .iter()
        .find(|(set, _)| *set)
        .map(|(_, flag)| format!("{flag} doesn't apply to a PNG crop"))
}

fn png_crop(
    sub: Option<&str>,
    input: PathBuf,
    joining: bool,
    output: PathBuf,
    crop: super::stills::Crop,
    args: ConvertArgs,
) -> Options {
    if let Some(message) = crop_conflict(&input, joining, &output, &args) {
        usage_error(sub, ErrorKind::ArgumentConflict, message);
    }
    let mut request = Request::new(input, output);
    request.overwrite = args.force;
    Options {
        request,
        json: args.json,
        progress: false,
        wav: None,
        add: None,
        range: None,
        still: None,
        crop: Some(crop),
    }
}

pub fn rate_of(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("'{text}' is not a frame rate such as 60, 30000/1001 or 29.97");
    let (num, den) = if let Some((n, d)) = text.split_once('/') {
        (
            n.trim().parse::<u32>().map_err(|_| bad())?,
            d.trim().parse::<u32>().map_err(|_| bad())?,
        )
    } else if let Some((whole, frac)) = text.split_once('.') {
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let den = 10u32.pow(frac.len() as u32);
        let whole = whole.parse::<u32>().map_err(|_| bad())?;
        let frac = frac.parse::<u32>().map_err(|_| bad())?;
        let num = whole
            .checked_mul(den)
            .and_then(|w| w.checked_add(frac))
            .ok_or_else(bad)?;
        let ntsc = f64::from(num) / f64::from(den) * 1.001;
        if !num.is_multiple_of(den) && ntsc >= 1.0 && (ntsc - ntsc.round()).abs() < 0.005 {
            (ntsc.round() as u32 * 1000, 1001)
        } else {
            (num, den)
        }
    } else {
        (text.trim().parse::<u32>().map_err(|_| bad())?, 1)
    };
    if num == 0 || den == 0 || num / den > 1000 {
        return Err(bad());
    }
    let g = gcd(num, den);
    Ok((num / g, den / g))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

fn raw(
    sub: Option<&str>,
    input: PathBuf,
    joining: bool,
    output: PathBuf,
    args: ConvertArgs,
) -> Options {
    let conflict = |message: &str| usage_error(sub, ErrorKind::ArgumentConflict, message);
    if joining {
        conflict(
            "join writes mp4, mov, mkv or webm, not raw PCM: join first, then take the PCM from the joined file",
        );
    }
    if input.is_dir() {
        conflict("raw PCM takes its audio from a video or audio file, not a folder of frames");
    }
    let other = [
        (args.profile.is_some(), "--profile"),
        (args.size.is_some(), "--size"),
        (args.fps.is_some(), "--fps"),
        (args.crf.is_some(), "--crf"),
        (args.qp.is_some(), "--qp"),
        (args.tune.is_some(), "--tune"),
        (args.audio_bitrate.is_some(), "--audio-bitrate"),
        (args.loudness.is_some(), "--loudness"),
        (args.to.is_some(), "--to"),
        (args.json, "--json"),
        (args.quality.is_some(), "--quality"),
        (args.lossless, "--lossless"),
        (args.no_audio, "--no-audio"),
        (args.add_audio.is_some(), "--add-audio"),
    ];
    if let Some((_, flag)) = other.iter().find(|(set, _)| *set) {
        conflict(&format!(
            "{flag} doesn't apply to raw PCM, which is 16-bit little-endian samples from --from to the end"
        ));
    }
    let mut request = Request::new(input, output);
    request.overwrite = args.force;
    Options {
        request,
        json: false,
        progress: false,
        wav: Some(Wav {
            from: args.from.unwrap_or(0.0),
            to: None,
            mono: args.mono,
            rate: args.rate,
            opus: None,
            track: args.track,
            raw: true,
        }),
        add: None,
        range: None,
        still: None,
        crop: None,
    }
}

pub fn every_of(text: &str) -> Result<f64, String> {
    let bad = || format!("'{text}' is not an interval above zero such as 2s, 500ms, 1.5 or 0:01");
    let trimmed = text.trim();
    let seconds = if let Some(ms) = trimmed.strip_suffix("ms") {
        ms.trim().parse::<f64>().map_err(|_| bad())? / 1000.0
    } else {
        let plain = trimmed.strip_suffix('s').unwrap_or(trimmed);
        super::time_of(plain).map_err(|_| bad())?
    };
    if seconds.is_finite() && seconds > 0.0 {
        Ok(seconds)
    } else {
        Err(bad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(args: ConvertArgs) -> Option<String> {
        crop_conflict(Path::new("in.png"), false, Path::new("out.png"), &args)
    }

    #[test]
    fn a_png_crop_names_every_flag_it_ignores() {
        assert_eq!(conflict(ConvertArgs::default()), None);
        let quiet = ConvertArgs {
            force: true,
            json: true,
            no_progress: true,
            ..ConvertArgs::default()
        };
        assert_eq!(conflict(quiet), None);
        let peak = ConvertArgs {
            true_peak: Some(-2.0),
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(peak).as_deref(),
            Some("--true-peak doesn't apply to a PNG crop")
        );
        let linear = ConvertArgs {
            linear: true,
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(linear).as_deref(),
            Some("--linear doesn't apply to a PNG crop")
        );
        let mono = ConvertArgs {
            mono: true,
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(mono).as_deref(),
            Some("--mono doesn't apply to a PNG crop")
        );
        let track = ConvertArgs {
            track: Some(1),
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(track).as_deref(),
            Some("--track doesn't apply to a PNG crop")
        );
        let bare = ConvertArgs {
            no_audio: true,
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(bare).as_deref(),
            Some("--no-audio doesn't apply to a PNG crop")
        );
        let added = ConvertArgs {
            add_audio: Some(PathBuf::from("bed.wav")),
            title: vec!["Voice".into()],
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(added).as_deref(),
            Some("--add-audio doesn't apply to a PNG crop")
        );
        let titled = ConvertArgs {
            title: vec!["Voice".into()],
            ..ConvertArgs::default()
        };
        assert_eq!(
            conflict(titled).as_deref(),
            Some("--title doesn't apply to a PNG crop")
        );
    }

    #[test]
    fn a_png_crop_wants_a_png_on_both_sides() {
        let args = ConvertArgs::default();
        let e = crop_conflict(Path::new("in.mp4"), false, Path::new("out.png"), &args).unwrap();
        assert!(e.contains("cuts one PNG into a PNG"), "{e}");
        let e = crop_conflict(Path::new("in.png"), true, Path::new("out.png"), &args).unwrap();
        assert!(e.contains("join takes clips"), "{e}");
    }

    #[test]
    fn an_interval_reads_seconds_milliseconds_and_clock_times() {
        assert_eq!(every_of("2s").unwrap(), 2.0);
        assert_eq!(every_of("500ms").unwrap(), 0.5);
        assert_eq!(every_of("1.5").unwrap(), 1.5);
        assert_eq!(every_of("2.5s").unwrap(), 2.5);
        assert_eq!(every_of("0:01").unwrap(), 1.0);
        assert_eq!(every_of("1:30").unwrap(), 90.0);
        assert_eq!(every_of("1:00:00").unwrap(), 3600.0);
        assert_eq!(every_of(" 250 ms ").unwrap(), 0.25);
    }

    #[test]
    fn an_interval_refuses_zero_negative_and_garbage() {
        for bad in [
            "0", "0s", "0ms", "-1", "-2s", "-500ms", "", "s", "ms", "soon", "2x", "1:99", "inf",
            "NaN", "1e400",
        ] {
            let e = every_of(bad).unwrap_err();
            assert!(e.contains("is not an interval above zero"), "{bad}: {e}");
            assert!(e.contains(&format!("'{bad}'")), "{bad}: {e}");
            assert!(!e.contains('\n'), "{bad}: {e}");
        }
    }
}
