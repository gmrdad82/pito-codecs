const ABOUT: &str = "video, audio and image codecs in pure Rust, on the CPU and the GPU, with no ffmpeg inside: convert, cut, join, probe, measure and take stills";

const LINES: &[(&str, &str)] = &[
    (
        "pcod <input> <output> [options]",
        "convert a video, a folder of PNG frames or a still; the output's extension picks the format",
    ),
    (
        "pcod convert <input> <output> [options]",
        "the same, spelled out",
    ),
    (
        "pcod join <inputs>... <output>",
        "join clips in order into one file, copied when they match",
    ),
    ("pcod profiles", "list the profiles and what each one does"),
    (
        "pcod probe <file> [--json]",
        "show what is in a video, audio or image file",
    ),
    (
        "pcod loudness <file> [--json]",
        "measure an audio track's loudness (EBU R128)",
    ),
    (
        "pcod synth <output> --size --fps --duration",
        "write a test video: colour bars, a frame counter, a moving box and tones",
    ),
    (
        "pcod spectrum <file> <out.png>",
        "draw an audio track's spectrogram as a PNG",
    ),
    (
        "pcod decode <file> [--route cpu|gpu]",
        "decode the whole video track and report the speed",
    ),
    (
        "pcod encode <input> <output> [--codec hevc]",
        "decode, encode and mux an MP4, copying the audio",
    ),
    (
        "pcod frames <dir> <output> --fps <fps>",
        "encode a folder of PNG frames and an optional WAV into an MP4",
    ),
    (
        "pcod stills <file> --out <dir>|--sheet <png>",
        "write stills, frame ranges or one contact sheet",
    ),
    (
        "pcod filmstrip <file> <output> [--every <t>]",
        "draw tiles of a video on a cadence into one image",
    ),
    (
        "pcod skill install|status|uninstall",
        "put pcod's agent skill into Claude Code, Codex, Pi and other agents",
    ),
    (
        "pcod --help",
        "the full help; pcod <command> --help for one command",
    ),
    ("pcod --version", "print the version"),
];

pub fn text() -> String {
    let width = LINES
        .iter()
        .map(|(usage, _)| usage.len())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = format!("pcod {}: {ABOUT}\n\n", env!("CARGO_PKG_VERSION"));
    for (usage, what) in LINES {
        out.push_str(&format!("  {usage:<width$}{what}\n"));
    }
    out.push_str("\n--json prints a manifest for scripts on convert, join, probe, loudness, spectrum, stills and filmstrip.\n");
    out
}
