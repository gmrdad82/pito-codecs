use std::path::{Path, PathBuf};

use clap::error::ErrorKind;
use clap::{CommandFactory, ValueEnum};

use super::Cli;
use super::convert::Options;
use super::plan::{Loudness, Request};
use super::profile::{ANIMATED_FPS, ANIMATED_WIDTH, LIGHT_WEB_CRF, Profile, VP9_LIGHT_WEB_CRF};
use super::size::Size;

pub const USAGE: &str = "pcod [OPTIONS] <INPUT> <OUTPUT>\n       pcod <COMMAND>";

pub fn help() -> String {
    format!(
        "The output's extension picks the container: mp4, mov, mkv (H.264 and AAC), webm (VP9 and Opus), gif or webp (animated, looping, {ANIMATED_WIDTH} px wide at {ANIMATED_FPS} fps by default, or the source's width and rate where smaller). The input is a video file or a folder of PNG frames in name order (--fps gives their rate).
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

pub fn loudness_of(text: &str) -> Result<Loudness, String> {
    Loudness::parse(text)
}

#[derive(clap::Args, Clone, Default)]
pub struct ConvertArgs {
    #[arg(long, value_enum, help = "fast, balanced (the default) or best")]
    pub profile: Option<ProfileArg>,
    #[arg(long, value_parser = size_of, help = "A width, WxH within 1% of the aspect, or 720p, 1080p, 1440p, 2160p")]
    pub size: Option<Size>,
    #[arg(
        long,
        help = "The frame rate, 1 to 240 (for a folder of PNG frames, their rate)"
    )]
    pub fps: Option<u32>,
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
    #[arg(long, help = "The audio bitrate in kbps, 32 to 512")]
    pub audio_bitrate: Option<u32>,
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
    let mut loudness = args.loudness;
    if let (Some(tp), Some(l)) = (args.true_peak, loudness.as_mut()) {
        l.true_peak = tp;
    }
    let mut request = Request::new(input, output);
    request.joins = joins;
    request.profile = profile;
    request.size = args.size;
    request.fps = args.fps;
    request.quality = args.crf.or(args.qp);
    request.tune = args.tune;
    request.overwrite = args.force;
    request.copy_video =
        (joining || loudness.is_some() || args.audio_bitrate.is_some()) && !picture && !folder;
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
        progress: !args.no_progress && !args.json && drawable(),
    }
}

fn drawable() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal() && std::env::var_os("TERM").is_none_or(|t| t != "dumb")
}
