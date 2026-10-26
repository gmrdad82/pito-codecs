#![cfg(feature = "cli")]

#[path = "../src/testref.rs"]
mod testref;

#[allow(dead_code)]
#[path = "../src/pcod/refs.rs"]
mod refs;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use pito_codecs::aac;
use pito_codecs::media::{AudioBuffer, Prefs, Rgba, probe};
use testref::{AudioCodec, Clip, Refs, Sound, VideoCodec};

const RATE: u32 = 48_000;
const SECONDS: f64 = 1.0;

fn root() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/pcod-commands");
    fs::create_dir_all(&path).unwrap();
    path
}

fn dir(name: &str) -> PathBuf {
    let path = root().join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    text(&out.stdout)
}

fn refused(out: &Output, words: &str) {
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(words), "{words:?} not in {stderr}");
    assert!(out.stdout.is_empty());
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn after<'a>(json: &'a str, marker: &str) -> &'a str {
    let at = json
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} not in {json}"));
    &json[at..]
}

const SAW_SHA256: &str = "10612187de060fefd0b166c1aa72daf38c0fd0bab4e1dba7e7a173639ffe34b9";

fn sha256sum(path: &Path) -> String {
    testref::sha256(&fs::read(path).unwrap())
}

fn digest_of(got: Option<&str>, path: &Path, json: &str) {
    let got = got.unwrap_or_else(|| panic!("no sha256 for {}: {json}", path.display()));
    assert_eq!(got.len(), 64, "{json}");
    assert!(
        got.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{json}"
    );
    assert_eq!(got, sha256sum(path), "{}: {json}", path.display());
}

fn sample(i: usize, channel: usize) -> i16 {
    (((i * 7 + channel * 300) % 2000) as i16 - 1000) * 8
}

fn wav_bytes(frames: usize) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(frames * 4);
    for i in 0..frames {
        for c in 0..2 {
            pcm.extend_from_slice(&sample(i, c).to_le_bytes());
        }
    }
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

struct Wav {
    rate: u32,
    channels: u16,
    data: Vec<u8>,
}

fn read_wav(path: &Path) -> Wav {
    let b = fs::read(path).unwrap();
    assert_eq!(&b[0..4], b"RIFF");
    assert_eq!(&b[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes([b[34], b[35]]), 16);
    assert_eq!(&b[36..40], b"data");
    let len = u32::from_le_bytes([b[40], b[41], b[42], b[43]]) as usize;
    assert_eq!(len, b.len() - 44);
    assert_eq!(
        u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize,
        len + 36
    );
    Wav {
        rate: u32::from_le_bytes([b[24], b[25], b[26], b[27]]),
        channels: u16::from_le_bytes([b[22], b[23]]),
        data: b[44..].to_vec(),
    }
}

fn clip(frames: u64) -> Clip {
    Clip::new((352, 240), (30, 1), frames)
}

fn adts(path: &Path) -> PathBuf {
    let mut e = aac::Encoder::new(aac::EncoderConfig {
        rate: RATE,
        channels: 2,
        bitrate: 64_000,
    })
    .unwrap();
    let tone = Sound::Sine {
        hz: 440.0,
        amp: 0.25,
    };
    let buf = AudioBuffer {
        samples: testref::pcm(tone, RATE, 2, 0, RATE as usize / 2),
        rate: RATE,
        channels: 2,
    };
    let mut packets = e.encode(&buf).unwrap();
    packets.extend(e.flush());
    let mut out = Vec::new();
    for p in packets {
        let len = p.data.len() + 7;
        out.extend_from_slice(&[
            0xff,
            0xf1,
            0x4c,
            0x80 | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc,
        ]);
        out.extend_from_slice(&p.data);
    }
    testref::write_atomic(path, &out).unwrap();
    path.to_path_buf()
}

fn write_frames(folder: &Path) {
    let _ = fs::remove_dir_all(folder);
    fs::create_dir_all(folder).unwrap();
    for n in 0..10u32 {
        let mut img = Rgba::new(352, 240);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % 352, i as u32 / 352);
            px.copy_from_slice(&[(x + n * 9) as u8, (y * 2) as u8, (x ^ y) as u8, 255]);
        }
        fs::write(
            folder.join(format!("{n:06}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
}

struct Media {
    wav: PathBuf,
    frames: PathBuf,
    mp4: PathBuf,
    silent: PathBuf,
}

fn media() -> &'static Media {
    static MEDIA: OnceLock<Media> = OnceLock::new();
    MEDIA.get_or_init(|| {
        let shared = root().join("shared");
        fs::create_dir_all(&shared).unwrap();
        let pid = std::process::id();
        let wav = shared.join("saw.wav");
        if !wav.exists() {
            let part = shared.join(format!("{pid}.saw.wav"));
            fs::write(&part, wav_bytes((SECONDS * f64::from(RATE)) as usize)).unwrap();
            if fs::rename(&part, &wav).is_err() {
                let _ = fs::remove_file(&part);
            }
        }
        let frames = shared.join("frames");
        if !frames.exists() {
            let part = shared.join(format!("{pid}.frames"));
            write_frames(&part);
            if fs::rename(&part, &frames).is_err() {
                let _ = fs::remove_dir_all(&part);
            }
        }
        Media {
            wav,
            frames,
            mp4: clip(30).make(refs::AREA, "commands-tone.mp4"),
            silent: clip(30)
                .video_only()
                .make(refs::AREA, "commands-silent.mp4"),
        }
    })
}

#[test]
fn probe_names_every_container_family_in_text_and_json() {
    let m = media();
    let r = Refs::open(refs::AREA);
    let mut cases: Vec<(PathBuf, &str, &str, &str)> = vec![(m.wav.clone(), "WAV", "null", "pcm")];
    for (file, container, codec) in refs::TONES {
        cases.push((r.path(file), container, "null", codec));
    }
    let m4a = clip(30).audio_only().make(refs::AREA, "commands-tone.m4a");
    cases.push((m4a, "MP4", "null", "aac"));
    let opus = dir("probe-opus").join("saw.opus");
    ok(&run(&[s(&m.wav), s(&opus)]));
    cases.push((opus, "Ogg", "null", "opus"));
    cases.push((m.mp4.clone(), "MP4", "h264", "aac"));
    for (name, container) in [
        ("commands-tone.mov", "QuickTime"),
        ("commands-tone.mkv", "Matroska"),
    ] {
        cases.push((clip(30).make(refs::AREA, name), container, "h264", "aac"));
    }
    let webm = Clip {
        video: Some(VideoCodec::Vp9),
        audio: Some(AudioCodec::Opus),
        ..clip(15)
    }
    .make(refs::AREA, "commands-tone.webm");
    cases.push((webm, "Matroska", "vp9", "opus"));
    for (path, container, video, audio) in cases {
        let shown = ok(&run(&["probe", s(&path)]));
        let name = path.display();
        assert!(
            shown.starts_with(&format!("container  {container}\nduration   ")),
            "{name}: {shown}"
        );
        assert_eq!(
            shown.contains("\nvideo      "),
            video != "null",
            "{name}: {shown}"
        );
        if video != "null" {
            assert!(
                shown.contains(&format!("\nvideo      {video} "))
                    && shown.contains(" 352x240 30.00 fps"),
                "{name}: {shown}"
            );
        }
        assert!(
            shown
                .lines()
                .any(|l| l.starts_with("audio      ") && l.contains(" 2 ch 48000 Hz")),
            "{name}: {shown}"
        );
        let json = ok(&run(&["probe", s(&path), "--json"]));
        assert_eq!(json.lines().count(), 1, "{name}: {json}");
        assert_eq!(field(&json, "codec"), Some(video), "{name}: {json}");
        assert_eq!(field(&json, "audio"), Some(audio), "{name}: {json}");
        assert_eq!(field(&json, "audio_channels"), Some("2"), "{name}: {json}");
        assert_eq!(field(&json, "audio_rate"), Some("48000"), "{name}: {json}");
        let seconds: f64 = field(&json, "seconds").unwrap().parse().unwrap();
        let tone = refs::TONE_FRAMES as f64 / f64::from(refs::TONE_RATE);
        let long = if refs::TONES.iter().any(|(f, _, _)| path.ends_with(f)) {
            tone - 0.01..=tone + 0.06
        } else {
            0.45..=1.05
        };
        assert!(long.contains(&seconds), "{name}: {json}");
    }
}

#[test]
fn probe_refuses_what_it_cannot_read_in_one_line() {
    let folder = dir("probe-refused");
    let junk = folder.join("junk.mp4");
    fs::write(&junk, b"not a video").unwrap();
    let missing = folder.join("missing.mp4");
    let adts = adts(&folder.join("tone.aac"));
    assert_eq!(&fs::read(&adts).unwrap()[..2], &[0xff, 0xf1]);
    refused(&run(&["probe", s(&adts)]), s(&adts));
    refused(&run(&["probe", s(&adts), "--json"]), s(&adts));
    for args in [
        vec!["probe", s(&junk)],
        vec!["probe", s(&junk), "--json"],
        vec!["probe", s(&missing)],
    ] {
        let out = run(&args);
        refused(&out, args[1]);
    }
}

fn manifest_files_match(json: &str, output: &Path) {
    let files = after(json, "\"files\":");
    assert_eq!(field(files, "file"), Some(s(output)), "{json}");
    digest_of(field(files, "sha256"), output, json);
    assert_eq!(
        field(files, "bytes"),
        Some(fs::metadata(output).unwrap().len().to_string().as_str()),
        "{json}"
    );
}

#[test]
fn convert_profiles_and_quality_flags_land_in_the_manifest() {
    let m = media();
    let folder = dir("profiles");
    let frames = s(&m.frames);
    let cases: [(&[&str], &str, &str, &str); 6] = [
        (&[], "balanced", "crf", "20"),
        (&["--profile", "best"], "best", "crf", "18"),
        (
            &["--profile", "balanced", "--crf", "28"],
            "balanced",
            "crf",
            "28",
        ),
        (
            &["--profile", "best", "--tune", "animation"],
            "best",
            "crf",
            "18",
        ),
        (&["--profile", "fast"], "fast", "qp", "25"),
        (&["--profile", "fast", "--qp", "30"], "fast", "qp", "30"),
    ];
    for (k, (flags, profile, key, value)) in cases.into_iter().enumerate() {
        let output = folder.join(format!("{k}.mp4"));
        let mut args = vec![frames, s(&output), "--fps", "10", "--json"];
        args.extend_from_slice(flags);
        let json = ok(&run(&args));
        assert_eq!(json.lines().count(), 1, "{flags:?}: {json}");
        assert_eq!(field(&json, "name"), Some("pcod"), "{json}");
        assert_eq!(
            field(&json, "version"),
            Some(env!("CARGO_PKG_VERSION")),
            "{json}"
        );
        let params = after(&json, "\"params\":");
        assert_eq!(field(params, "profile"), Some(profile), "{flags:?}: {json}");
        assert_eq!(
            field(after(params, "\"quality\":"), key),
            Some(value),
            "{flags:?}: {json}"
        );
        if flags.contains(&"animation") {
            assert_eq!(field(params, "tune"), Some("animation"), "{json}");
        }
        manifest_files_match(&json, &output);
        let p = probe(&output, &Prefs::default()).unwrap();
        assert_eq!(
            (p.video_codec_name.as_str(), p.width, p.height),
            ("h264", 352, 240)
        );
        assert_eq!(p.fps, (10, 1));
    }
    let plain = folder.join("plain.webm");
    let out = run(&[frames, s(&plain), "--fps", "10"]);
    assert_eq!(ok(&out).trim(), s(&plain));
    assert_eq!(
        probe(&plain, &Prefs::default()).unwrap().video_codec_name,
        "vp9"
    );
}

#[test]
fn no_progress_line_is_drawn_when_stderr_is_not_a_terminal() {
    let m = media();
    let folder = dir("progress");
    for (k, env) in [
        None,
        Some(("TERM", "dumb")),
        Some(("TERM", "xterm-256color")),
    ]
    .into_iter()
    .enumerate()
    {
        let output = folder.join(format!("{k}.mp4"));
        let mut c = Command::new(env!("CARGO_BIN_EXE_pcod"));
        c.args([s(&m.frames), s(&output), "--fps", "10"]);
        if let Some((key, value)) = env {
            c.env(key, value);
        }
        let out = c.output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(out.stderr.is_empty(), "{env:?}: {:?}", text(&out.stderr));
        assert_eq!(text(&out.stdout).trim(), s(&output));
    }
}

#[test]
fn extract_writes_the_exact_pcm_of_its_window() {
    let m = media();
    let folder = dir("extract");
    let source = read_wav(&m.wav);

    let whole = folder.join("whole.wav");
    assert_eq!(ok(&run(&[s(&m.wav), s(&whole)])).trim(), s(&whole));
    let got = read_wav(&whole);
    assert_eq!((got.rate, got.channels), (48_000, 2));
    assert_eq!(got.data, source.data);

    let window = folder.join("window.wav");
    ok(&run(&[
        s(&m.wav),
        s(&window),
        "--from",
        "0.25",
        "--to",
        "0.75",
    ]));
    let got = read_wav(&window);
    assert_eq!((got.rate, got.channels), (48_000, 2));
    assert_eq!(got.data, source.data[12_000 * 4..36_000 * 4]);

    let tail = folder.join("tail.wav");
    ok(&run(&[s(&m.wav), s(&tail), "--from", "0:00.5"]));
    assert_eq!(read_wav(&tail).data, source.data[24_000 * 4..]);

    let mono = folder.join("mono.wav");
    ok(&run(&[s(&m.wav), s(&mono), "--mono", "--to", "0.5"]));
    let got = read_wav(&mono);
    assert_eq!((got.rate, got.channels), (48_000, 1));
    assert_eq!(got.data.len(), 24_000 * 2);
    for (i, pair) in got.data.chunks_exact(2).enumerate() {
        let mono = i32::from(i16::from_le_bytes([pair[0], pair[1]]));
        let want = (i32::from(sample(i, 0)) + i32::from(sample(i, 1))) / 2;
        assert!((mono - want).abs() <= 1, "frame {i}: {mono} vs {want}");
    }

    let resampled = folder.join("44k.wav");
    ok(&run(&[s(&m.wav), s(&resampled), "--rate", "44100"]));
    let got = read_wav(&resampled);
    assert_eq!((got.rate, got.channels), (44_100, 2));
    let frames = got.data.len() / 4;
    assert!((44_000..=44_200).contains(&frames), "{frames}");
    for channel in 0..2 {
        let (was, now) = (rms(&source.data, channel), rms(&got.data, channel));
        assert!(was > 1000.0, "{was}");
        assert!(
            (now / was - 1.0).abs() < 0.05,
            "channel {channel}: {now} vs {was}"
        );
    }
}

fn rms(stereo: &[u8], channel: usize) -> f64 {
    let values: Vec<f64> = stereo
        .chunks_exact(4)
        .map(|f| f64::from(i16::from_le_bytes([f[channel * 2], f[channel * 2 + 1]])))
        .collect();
    (values.iter().map(|v| v * v).sum::<f64>() / values.len().max(1) as f64).sqrt()
}

#[test]
fn extract_levels_to_a_loudness_target_and_reports_it() {
    let m = media();
    let folder = dir("extract-loud");
    let output = folder.join("loud.wav");
    let shown = ok(&run(&[s(&m.wav), s(&output), "--loudness", "-23"]));
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 3, "{shown}");
    assert_eq!(lines[0], s(&output));
    assert!(lines[1].starts_with("loudness before: "), "{shown}");
    assert!(
        lines[2].starts_with("loudness after: ") && lines[2].contains("target -23 LUFS"),
        "{shown}"
    );
    let measured = ok(&run(&["loudness", s(&output), "--json"]));
    let integrated: f64 = field(&measured, "integrated").unwrap().parse().unwrap();
    assert!((integrated + 23.0).abs() < 1.0, "{measured}");
    let peak: f64 = field(&measured, "true_peak").unwrap().parse().unwrap();
    assert!(peak <= -0.9, "{measured}");
}

#[test]
fn extract_json_manifests_hash_the_source_and_the_output() {
    let m = media();
    let folder = dir("extract-json");
    for (name, flags, profile, layout, rate, kbps) in [
        ("a.wav", &[][..], "wav", "stereo", "48000", "null"),
        (
            "b.wav",
            &["--mono", "--rate", "44100"][..],
            "wav",
            "mono",
            "44100",
            "null",
        ),
        ("c.opus", &["--mono"][..], "opus", "mono", "48000", "32"),
        (
            "d.opus",
            &["--audio-bitrate", "96"][..],
            "opus",
            "stereo",
            "48000",
            "96",
        ),
    ] {
        let output = folder.join(name);
        let mut args = vec![s(&m.wav), s(&output), "--json"];
        args.extend_from_slice(flags);
        let json = ok(&run(&args));
        let source = after(&json, "\"source\":");
        assert_eq!(field(source, "path"), Some(s(&m.wav)), "{json}");
        assert_eq!(field(source, "sha256"), Some(SAW_SHA256), "{json}");
        let params = after(&json, "\"params\":");
        assert_eq!(field(params, "profile"), Some(profile), "{json}");
        let audio = after(params, "\"audio\":");
        assert_eq!(field(audio, "layout"), Some(layout), "{json}");
        assert_eq!(field(audio, "sample_rate"), Some(rate), "{json}");
        assert_eq!(field(audio, "kbps").unwrap_or("null"), kbps, "{json}");
        assert_eq!(field(&json, "device"), Some("CPU"), "{json}");
        manifest_files_match(&json, &output);
        if name == "a.wav" {
            assert_eq!(fs::read(&output).unwrap(), fs::read(&m.wav).unwrap());
            let files = after(&json, "\"files\":");
            assert_eq!(field(files, "sha256"), Some(SAW_SHA256), "{json}");
        }
    }
}

#[test]
fn the_shared_source_is_the_wav_its_known_digest_was_taken_from() {
    let m = media();
    let bytes = fs::read(&m.wav).unwrap();
    assert_eq!(bytes, wav_bytes((SECONDS * f64::from(RATE)) as usize));
    assert_eq!(bytes.len(), 192_044);
    assert_eq!(sha256sum(&m.wav), SAW_SHA256);
}

#[test]
fn extract_refuses_bad_windows_and_sources_in_one_line() {
    let m = media();
    let folder = dir("extract-refused");
    let output = folder.join("out.wav");
    refused(
        &run(&[s(&m.wav), s(&output), "--from", "0.5", "--to", "0.5"]),
        "--to 0.5 must come after --from 0.5",
    );
    refused(
        &run(&[s(&m.wav), s(&output), "--from", "2"]),
        "--from 2 is at or past the end of the audio (1.000 s)",
    );
    refused(&run(&[s(&m.wav), s(&m.wav)]), "the output is the input");
    refused(&run(&[s(&m.silent), s(&output)]), "the source has no audio");
    assert!(!output.exists());
    fs::write(&output, b"old").unwrap();
    refused(&run(&[s(&m.wav), s(&output)]), "use --force to replace it");
    assert_eq!(fs::read(&output).unwrap(), b"old");
    ok(&run(&[s(&m.wav), s(&output), "--force"]));
    assert_eq!(read_wav(&output).channels, 2);
    let leftovers: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn loudness_measures_a_track_in_text_and_json() {
    let m = media();
    let shown = ok(&run(&["loudness", s(&m.wav)]));
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines.len(), 7, "{shown}");
    assert_eq!(
        lines[0],
        "audio           track 0, pcm 2 ch 48000 Hz, 1.00 s"
    );
    for (line, head, unit) in [
        (lines[1], "integrated      ", " LUFS"),
        (lines[2], "range           ", " LU"),
        (lines[3], "true peak       ", " dBTP"),
        (lines[4], "sample peak     ", " dBFS"),
        (lines[5], "short-term max  ", " LUFS"),
        (lines[6], "momentary max   ", " LUFS"),
    ] {
        assert!(line.starts_with(head) && line.ends_with(unit), "{line}");
    }
    let json = ok(&run(&["loudness", s(&m.wav), "--json"]));
    for (key, want) in [
        ("track", "0"),
        ("codec", "pcm"),
        ("channels", "2"),
        ("sample_rate", "48000"),
    ] {
        assert_eq!(field(&json, key), Some(want), "{json}");
    }
    let peak: f64 = field(&json, "sample_peak").unwrap().parse().unwrap();
    let loudest = (0..48_000)
        .map(|i| sample(i, 0).unsigned_abs())
        .max()
        .unwrap();
    let want = 20.0 * (f64::from(loudest) / 32768.0).log10();
    assert!((peak - want).abs() < 0.1, "{peak} vs {want}: {json}");
    refused(
        &run(&["loudness", s(&m.wav), "--track", "3"]),
        "there is no audio track 3; the file has 1",
    );
    refused(&run(&["loudness", s(&m.silent)]), "the file has no audio");
}

#[test]
fn decode_reports_its_route_and_every_frame() {
    let mp4 = &media().mp4;
    let shown = ok(&run(&[
        "decode",
        s(mp4),
        "--route",
        "cpu",
        "--threads",
        "2",
    ]));
    let lines: Vec<&str> = shown.lines().collect();
    assert!(lines.contains(&"route   CPU (2 threads)"), "{shown}");
    let last = lines.last().unwrap();
    assert!(
        last.starts_with("decoded 30 frames in ") && last.ends_with(" fps"),
        "{shown}"
    );
    let shown = ok(&run(&["decode", s(mp4), "--route", "cpu"]));
    assert!(
        shown
            .lines()
            .any(|l| l.starts_with("decoded 30 frames in ")),
        "{shown}"
    );
    let folder = dir("decode");
    let junk = folder.join("junk.mp4");
    fs::write(&junk, b"nothing to see").unwrap();
    refused(&run(&["decode", s(&junk), "--route", "cpu"]), s(&junk));
    refused(
        &run(&["decode", s(&media().wav), "--route", "cpu"]),
        s(&media().wav),
    );
}

#[test]
fn encode_re_encodes_the_video_and_copies_the_audio() {
    let mp4 = &media().mp4;
    let folder = dir("encode");
    for (name, codec) in [("h264.mp4", "h264"), ("hevc.mp4", "hevc")] {
        let output = folder.join(name);
        let shown = ok(&run(&[
            "encode",
            s(mp4),
            s(&output),
            "--route",
            "cpu",
            "--codec",
            codec,
            "--crf",
            "30",
            "--threads",
            "2",
        ]));
        let lines: Vec<&str> = shown.lines().collect();
        assert!(lines.contains(&"decode  CPU (2 threads)"), "{shown}");
        assert!(lines.contains(&"encode  H.264 CPU (2 threads)"), "{shown}");
        if codec == "hevc" {
            assert!(lines.iter().any(|l| l.starts_with("note    ")), "{shown}");
        }
        assert!(
            lines.iter().any(|l| l.starts_with("encoded 30 frames in ")),
            "{shown}"
        );
        assert_eq!(
            lines
                .last()
                .map(|l| l.starts_with(&format!("wrote   {} (", output.display()))),
            Some(true),
            "{shown}"
        );
        let p = probe(&output, &Prefs::default()).unwrap();
        assert_eq!(
            (p.video_codec_name.as_str(), p.width, p.height),
            ("h264", 352, 240)
        );
        assert_eq!(p.fps, (30, 1));
        assert_eq!(p.frames, Some(30));
        assert_eq!(p.audio.len(), 1);
        assert_eq!(p.audio[0].codec.to_ascii_lowercase(), "aac");
    }
    refused(
        &run(&[
            "encode",
            s(&media().wav),
            s(&folder.join("x.mp4")),
            "--route",
            "cpu",
        ]),
        s(&media().wav),
    );
}

#[test]
fn every_subcommand_answers_a_bad_command_line_like_clap() {
    let wav = s(&media().wav).to_string();
    let frames = s(&media().frames).to_string();
    let usage = dir("usage");
    let out_wav = s(&usage.join("out.wav")).to_string();
    let out_mp4 = s(&usage.join("out.mp4")).to_string();
    let cases: Vec<Vec<&str>> = vec![
        vec!["probe"],
        vec!["probe", "a.mp4", "--x"],
        vec!["loudness"],
        vec!["loudness", "a.wav", "--track", "x"],
        vec!["spectrum", "a.wav", "out.png", "--height", "100"],
        vec!["decode"],
        vec!["decode", "a.mp4", "--route", "npu"],
        vec!["decode", "a.mp4", "--threads", "-1"],
        vec!["encode", "a.mp4"],
        vec!["encode", "a.mp4", "b.mp4", "--codec", "av1"],
        vec!["encode", "a.mp4", "b.mp4", "--crf", "x"],
        vec!["frames", "dir", "out.mp4"],
        vec![
            "frames",
            "dir",
            "out.mp4",
            "--fps",
            "10",
            "--audio-bitrate",
            "10",
        ],
        vec![
            "frames",
            "dir",
            "out.mp4",
            "--fps",
            "10",
            "--loudness",
            "-14",
        ],
        vec!["stills", "a.mp4"],
        vec!["stills", "a.mp4", "--out", "o", "--seek", "fuzzy"],
        vec!["profiles", "extra"],
        vec!["convert", "a.mp4"],
        vec!["convert", "a.mp4", "b.mp4", "--crf", "60"],
        vec!["join", "a.mp4"],
        vec!["join", "a.mp4", "b.mp4", "out.mp4", "--from", "1"],
        vec!["join", "a.mp4", "b.mp4", "out.wav"],
        vec!["a.mp4", "b.mp4", "--mono"],
        vec!["a.mp4", "b.mp4", "--rate", "48000"],
        vec!["a.mp4", "b.mp4", "--true-peak", "-1"],
        vec!["a.mp4", "b.mp4", "--linear"],
        vec!["a.mp4", "b.wav", "--rate", "22050"],
        vec!["a.mp4", "b.wav", "--crf", "20"],
        vec!["a.mp4", "b.wav", "--audio-bitrate", "128"],
        vec!["a.mp4", "b.opus", "--size", "720p"],
        vec!["a.mp4", "b.wav", "--from", "soon"],
        vec![&wav, &out_wav, "--loudness", "-90"],
        vec![&frames, &out_wav],
        vec![&frames, &out_mp4],
    ];
    for args in &cases {
        let out = run(args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        assert!(
            stderr.ends_with("\n\nFor more information, try '--help'.\n"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("\x1b["), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let usage = match args[0] {
            sub @ ("probe" | "loudness" | "spectrum" | "decode" | "encode" | "frames"
            | "stills" | "profiles" | "convert" | "join") => format!("Usage: pcod {sub}"),
            _ => "Usage: pcod [OPTIONS] <INPUT> <OUTPUT>".to_string(),
        };
        match stderr.find("Usage: ") {
            Some(at) => assert!(stderr[at..].starts_with(&usage), "{args:?}: {stderr}"),
            None => assert!(
                stderr.starts_with("error: invalid value "),
                "{args:?}: {stderr}"
            ),
        }
    }
    let out = run(&["join", "a.mp4"]);
    assert!(
        text(&out.stderr).contains("Usage: pcod join"),
        "{}",
        text(&out.stderr)
    );
    let out = run(&["a.mp4", "b.mp4", "--mono"]);
    assert_eq!(
        text(&out.stderr),
        "error: --mono fits a .wav or .opus output, --rate a .wav\n\nUsage: pcod [OPTIONS] <INPUT> <OUTPUT>\n       pcod <COMMAND>\n\nFor more information, try '--help'.\n"
    );
}

#[test]
fn join_through_pcod_reports_its_method_and_hashes_every_clip() {
    let m = media();
    let (mp4, silent) = (&m.mp4, &m.silent);
    let folder = dir("join");
    let copied = folder.join("copied.mkv");
    let shown = ok(&run(&["join", s(mp4), s(mp4), s(&copied)]));
    assert_eq!(
        shown,
        format!(
            "{}\njoined 2 clips by stream copy, no re-encode: every clip is h264 352x240 at 30 fps with aac stereo at 48000 Hz\n",
            copied.display()
        )
    );
    let encoded = folder.join("encoded.mp4");
    let json = ok(&run(&["join", s(mp4), s(silent), s(&encoded), "--json"]));
    let sources = after(&json, "\"sources\":");
    digest_of(field(sources, "sha256"), mp4, &json);
    let (at, _) = sources
        .match_indices("\"path\":")
        .nth(1)
        .expect("two sources");
    let second = &sources[at..];
    assert_eq!(field(second, "path"), Some(s(silent)), "{json}");
    digest_of(field(second, "sha256"), silent, &json);
    let join = after(&json, "\"join\":");
    assert_eq!(field(join, "method"), Some("encode"), "{json}");
    assert_eq!(field(join, "clips"), Some("2"), "{json}");
    assert_eq!(
        field(join, "reason"),
        Some(
            format!(
                "{} has no audio and {} has",
                silent.display(),
                mp4.display()
            )
            .as_str()
        ),
        "{json}"
    );
    manifest_files_match(&json, &encoded);
    refused(
        &run(&["join", s(mp4), s(&m.frames), s(&folder.join("x.mp4"))]),
        "join takes video files, not a folder of frames",
    );
    refused(
        &run(&["join", s(mp4), s(&copied), s(&copied)]),
        "is both a clip and the output",
    );
    refused(
        &run(&["join", s(mp4), s(mp4), s(&copied)]),
        "use --force to replace it",
    );
}

#[test]
fn frames_with_loudness_measures_the_mp4_it_lands() {
    let out = dir("frames-loudness");
    let folder = out.join("frames");
    write_frames(&folder);
    let wav = out.join("voice.wav");
    fs::write(&wav, wav_bytes(RATE as usize)).unwrap();
    let mp4 = out.join("out.mp4");
    let args = [
        "frames",
        s(&folder),
        s(&mp4),
        "--fps",
        "24",
        "--audio",
        s(&wav),
        "--loudness",
        "-16",
        "--route",
        "cpu",
    ];
    let said = ok(&run(&args));
    let after = said
        .lines()
        .find(|l| l.starts_with("after   "))
        .unwrap_or_else(|| panic!("{said}"));
    assert!(after.ends_with("dBTP (the MP4's AAC)"), "{after}");
    let peak: f64 = after
        .split("true peak ")
        .nth(1)
        .and_then(|t| t.split(' ').next())
        .and_then(|t| t.parse().ok())
        .unwrap();
    assert!(peak <= -1.0 + 0.005, "{after}");
    assert!(
        said.lines().last().unwrap().starts_with("wrote   "),
        "{said}"
    );
    let first = fs::read(&mp4).unwrap();
    assert!(!first.is_empty());
    let leftovers: Vec<_> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n != "frames" && n != "voice.wav" && n != "out.mp4")
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    refused(&run(&args), "already exists; use --force to replace it");
    assert_eq!(fs::read(&mp4).unwrap(), first);
    let mut forced = args.to_vec();
    forced.push("--force");
    ok(&run(&forced));
}
