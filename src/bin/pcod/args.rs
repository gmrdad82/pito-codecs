use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, ValueEnum};

use super::Cli;
use super::convert::{Options, Span};
use super::extract::{
    OPUS_KBPS_HIGHEST, OPUS_KBPS_LOWEST, OPUS_KBPS_MONO, OPUS_KBPS_STEREO, RATES, Wav, is_opus,
    is_wav,
};
use super::plan::{Loudness, Request};
use super::profile::{ANIMATED_FPS, ANIMATED_WIDTH, LIGHT_WEB_CRF, Profile, VP9_LIGHT_WEB_CRF};
use super::size::Size;

pub const USAGE: &str = "pcod [OPTIONS] <INPUT> <OUTPUT>\n       pcod <COMMAND>";

pub fn help() -> String {
    format!(
        "The output's extension picks the container: mp4, mov, mkv (H.264 and AAC), webm (VP9 and Opus), gif or webp (animated, looping, {ANIMATED_WIDTH} px wide at {ANIMATED_FPS} fps by default, or the source's width and rate where smaller). --from and --to cut a window of the input, video and audio in sync, starting at 0: the first frame at or after --from to the last before --to. With no video option the cut copies whole GOPs and re-encodes only the frames at its edges where the container carries the source's codec; a video option re-encodes it all. A .wav output is the audio alone: the first audio track as 16-bit PCM, stereo at the source's rate when it is 44100 or 48000 Hz (else 48000), with --from and --to for a range, --mono, --rate and --loudness. A .opus (or .ogg) output is the audio alone as Ogg Opus at 48000 Hz, 64k stereo or 32k with --mono by default, --audio-bitrate KBPS from 16 to 510, with --from, --to, --mono and --loudness. The input is a video file or a folder of PNG frames in name order (--fps gives their rate).
Profiles: balanced (the default) and best are H.264 on the CPU at CRF 20 and 18 on x264's scale; fast is H.264 on the GPU at QP 25, on the CPU where no GPU encoder is found. For .webm, balanced is VP9 at CRF 32 and best at 28 on libvpx's scale.
Light web: --profile best --crf {LIGHT_WEB_CRF} (.webm: --crf {VP9_LIGHT_WEB_CRF})
For screen recordings: --tune animation
--size takes a width (the height follows the aspect), WxH within 1% of the aspect, or 720p, 1080p, 1440p, 2160p; pcod never stretches a picture.
Audio is AAC-LC 320k stereo at 48000 Hz, or Opus 128k in .webm; an AAC source is copied when nothing changes its audio. --audio-bitrate KBPS (32–512) sets the rate.
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
    text.parse()
        .ok()
        .filter(|r| RATES.contains(r))
        .ok_or_else(|| "use 44100 or 48000".to_string())
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
    #[arg(long, help = "For a .wav or .opus: one channel instead of two")]
    pub mono: bool,
    #[arg(long, value_parser = rate_hz, value_name = "HZ", help = "For a .wav: 44100 or 48000 [default: the source's when it is one of them, else 48000]")]
    pub rate: Option<u32>,
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
    if is_wav(&output) || is_opus(&output) {
        return wav(sub, input, joining, output, args);
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
    request.copy_video = (joining || ranged || loudness.is_some() || args.audio_bitrate.is_some())
        && !picture
        && !folder;
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
        range: ranged.then(|| Span {
            from: args.from.unwrap_or(0.0),
            to: args.to,
        }),
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
    ];
    if let Some((_, flag)) = picture.iter().find(|(set, _)| *set) {
        conflict(&if opus {
            format!("{flag} doesn't apply to .opus, which is audio alone at 48000 Hz")
        } else {
            format!("{flag} doesn't apply to .wav, which is uncompressed audio")
        });
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
            opus: opus.then(|| {
                args.audio_bitrate.unwrap_or(if args.mono {
                    OPUS_KBPS_MONO
                } else {
                    OPUS_KBPS_STEREO
                })
            }),
        }),
        range: None,
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
