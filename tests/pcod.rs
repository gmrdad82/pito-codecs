#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::{Prefs, Rgba, audio_pcm_range, probe};

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-test")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn pcod() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_pcod"));
    c.env("NO_COLOR", "1");
    c
}

fn run(args: &[&str]) -> Output {
    pcod().args(args).output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

fn frames(name: &str) -> PathBuf {
    {
        let folder = dir(&format!("{name}-frames"));
        for n in 0..20u32 {
            let mut img = Rgba::new(320, 240);
            for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
                let (x, y) = (i as u32 % 320, i as u32 / 320);
                px.copy_from_slice(&[
                    (x + n * 8) as u8,
                    (y * 2) as u8,
                    ((x ^ y) + n * 4) as u8,
                    if x < 40 { 0 } else { 255 },
                ]);
            }
            let png = pito_codecs::png::encode(&img).unwrap();
            fs::write(folder.join(format!("{n:04}.png")), png).unwrap();
        }
        folder
    }
}

fn partials(folder: &Path) -> Vec<String> {
    fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial"))
        .collect()
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

#[test]
fn version_help_and_profiles() {
    let out = run(&["--version"]);
    ok(&out);
    assert_eq!(
        text(&out.stdout),
        format!("pcod {}\n", env!("CARGO_PKG_VERSION"))
    );
    for args in [&["--help"][..], &["-h"]] {
        let out = run(args);
        ok(&out);
        assert!(text(&out.stdout).contains("--profile best --crf 23"));
    }
    let out = run(&["profiles"]);
    ok(&out);
    assert!(text(&out.stdout).contains("balanced: H.264"));
}

#[test]
fn usage_errors_exit_2_like_clap() {
    let cases: &[&[&str]] = &[
        &[],
        &["tui"],
        &["--nope"],
        &["in.mp4", "out.mp4", "--x"],
        &["in.mp4", "out.mp4", "--size"],
        &["in", "out.gif", "--loudness", "-14"],
        &["in", "out.webp", "--audio-bitrate", "320"],
        &["in", "out.mp4", "--audio-bitrate", "600"],
        &["in", "out.mp4", "--true-peak", "-2"],
        &["in", "out.mp4", "--loudness", "loud"],
        &["in", "out.mp4", "--loudness", "-3"],
        &["in", "out.mp4", "--qp", "25"],
        &["in", "out.mp4", "--profile", "fast", "--tune", "animation"],
        &["in", "out.avi"],
        &["in", "out.mp4", "--crf", "52"],
        &["in", "out.mp4", "--crf", "300"],
        &["in", "out.mp4", "--profile", "fast", "--crf", "20"],
        &["in", "out.mp4", "--crf", "20", "--qp", "25"],
        &["in", "out.mp4", "--fps", "0"],
        &["in", "out.mp4", "--fps", "many"],
        &["in", "out.mp4", "--size", "321x180"],
        &["probe", "--version"],
        &["join", "a.mp4", "out.mp4"],
        &["join", "a.mp4", "b.mp4", "out.gif"],
        &["in.wav", "out.opus", "--rate", "44100"],
        &["in.wav", "out.opus", "--audio-bitrate", "600"],
        &["in.wav", "out.opus", "--audio-bitrate", "15"],
        &["in.wav", "out.ogg", "--crf", "20"],
        &["spectrum", "in.wav"],
        &["spectrum", "in.wav", "out.png", "--floor-db", "3"],
        &["spectrum", "in.wav", "out.png", "--max-hz", "50"],
        &["spectrum", "in.wav", "out.png", "--width", "100"],
    ];
    for args in cases {
        let out = run(args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        assert!(
            stderr.ends_with("\n\nFor more information, try '--help'.\n"),
            "{args:?}: {stderr}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let out = run(&["tui"]);
    assert_eq!(
        text(&out.stderr),
        "error: unrecognized subcommand 'tui'\n\nUsage: pcod [OPTIONS] <INPUT> <OUTPUT>\n       pcod <COMMAND>\n\nFor more information, try '--help'.\n"
    );
    let folder = frames("usage").to_str().unwrap().to_string();
    let out = run(&[&folder, "out.mp4"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("--fps <FPS>"));
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_exits_2() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let out = pcod()
        .arg(OsStr::from_bytes(b"\xff.mp4"))
        .arg("out.mp4")
        .output()
        .unwrap();
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn each_extension_gets_its_container_and_the_json_manifest() {
    let folder = dir("containers");
    let input = frames("containers").to_str().unwrap().to_string();
    for (name, codec) in [
        ("a.mp4", "h264"),
        ("a.mov", "h264"),
        ("a.mkv", "h264"),
        ("a.gif", ""),
        ("a.webp", ""),
    ] {
        let output = folder.join(name);
        let out = run(&[&input, output.to_str().unwrap(), "--fps", "10", "--json"]);
        ok(&out);
        let json = text(&out.stdout);
        assert_eq!(json.lines().count(), 1, "{json}");
        assert_eq!(field(&json, "name"), Some("pcod"), "{json}");
        let bytes = fs::metadata(&output).unwrap().len();
        assert_eq!(field(&json, "bytes"), Some(bytes.to_string().as_str()));
        let sha = json
            .rsplit("\"sha256\":\"")
            .next()
            .map(|s| &s[..64])
            .unwrap_or("");
        assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()), "{json}");
        if !codec.is_empty() {
            let p = probe(&output, &Prefs::default()).unwrap();
            assert_eq!(p.video_codec_name, codec, "{name}");
            assert_eq!((p.width, p.height), (320, 240), "{name}");
        }
        assert!(partials(&folder).is_empty());
    }
    let gif = fs::read(folder.join("a.gif")).unwrap();
    assert!(gif.starts_with(b"GIF89a"));
    assert!(gif.windows(11).any(|w| w == b"NETSCAPE2.0"));
    let webp = fs::read(folder.join("a.webp")).unwrap();
    assert_eq!(&webp[8..12], b"WEBP");
}

#[test]
fn an_existing_output_needs_force() {
    let folder = dir("force");
    let input = frames("force").to_str().unwrap().to_string();
    let output = folder.join("out.mp4");
    fs::write(&output, b"old").unwrap();
    let out = run(&[&input, output.to_str().unwrap(), "--fps", "10"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert!(
        stderr.trim_end().ends_with("use --force to replace it"),
        "{stderr}"
    );
    assert_eq!(fs::read(&output).unwrap(), b"old");
    let out = run(&[&input, output.to_str().unwrap(), "--fps", "10", "--force"]);
    ok(&out);
    assert_eq!(text(&out.stdout).trim(), output.to_str().unwrap());
    assert_ne!(fs::read(&output).unwrap(), b"old");
    let copy = folder.join("again.mp4");
    let out = run(&[
        output.to_str().unwrap(),
        output.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("same file"));
    let out = run(&[output.to_str().unwrap(), copy.to_str().unwrap()]);
    ok(&out);
    assert!(partials(&folder).is_empty());
}

#[cfg(unix)]
#[test]
fn a_signal_stops_the_conversion_and_leaves_nothing() {
    let folder = dir("signal");
    let input = frames("signal").to_str().unwrap().to_string();
    let output = folder.join("big.mp4");
    let child = pcod()
        .args([
            &input,
            output.to_str().unwrap(),
            "--fps",
            "1",
            "--size",
            "2160p",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while partials(&folder).is_empty() && started.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = i32::try_from(child.id()).unwrap();
    unsafe {
        kill(pid, 15);
    }
    let out = child.wait_with_output().unwrap();
    let stderr = text(&out.stderr);
    if out.status.code() == Some(0) {
        return;
    }
    assert_eq!(out.status.code(), Some(143), "{stderr}");
    assert!(stderr.contains("SIGTERM"), "{stderr}");
    assert!(!output.exists());
    assert!(partials(&folder).is_empty(), "{:?}", partials(&folder));
}

#[cfg(target_os = "linux")]
fn capped(cap: u64, args: &[&str]) -> Output {
    Command::new("prlimit")
        .arg(format!("--as={cap}"))
        .arg("--core=1:1")
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_pcod"))
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn six_k_frames_encode_under_an_address_space_cap_or_refuse_in_one_line() {
    const W: u32 = 6344;
    const H: u32 = 3968;
    const COUNT: u32 = 40;
    let folder = dir("6k-frames");
    let mut img = Rgba::new(W, H);
    for n in 0..COUNT {
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % W, i as u32 / W);
            let moving = (x + W - n * 97) % W < 800 && (1000..1800).contains(&y);
            px.copy_from_slice(&if moving {
                [255 - n as u8, 40, 200, 255]
            } else {
                [
                    (x * 255 / W) as u8,
                    (y * 255 / H) as u8,
                    ((x ^ y) >> 4) as u8,
                    255,
                ]
            });
        }
        let png = pito_codecs::png::encode(&img).unwrap();
        fs::write(folder.join(format!("{n:06}.png")), png).unwrap();
    }
    drop(img);
    let out = dir("6k");
    let mp4 = out.join("6k.mp4");
    let args = [
        "frames",
        folder.to_str().unwrap(),
        mp4.to_str().unwrap(),
        "--fps",
        "60",
        "--crf",
        "14",
        "--route",
        "cpu",
    ];
    let tight = capped(2_000_000_000, &args);
    let stderr = text(&tight.stderr);
    assert_eq!(tight.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("address-space limit"), "{stderr}");
    assert!(!mp4.exists());
    let made = capped(8_000_000_000, &args);
    ok(&made);
    let p = probe(&mp4, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height, p.frames), (W, H, Some(COUNT as u64)));
}

fn pulse_at(file: &Path) -> Option<(usize, usize)> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args([
            "-map",
            "0:a",
            "-af",
            "aresample=async=1:first_pts=0",
            "-ac",
            "1",
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .ok()?;
    assert!(out.status.success(), "{}", text(&out.stderr));
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let peak = samples.iter().fold(0f32, |m, v| m.max(v.abs()));
    let at = samples.iter().position(|v| v.abs() > peak / 2.0).unwrap();
    Some((at, samples.len()))
}

#[test]
fn ntsc_frames_keep_the_exact_rate_and_the_audio_in_sync() {
    const COUNT: u32 = 1000;
    const RATE: u32 = 48_000;
    let folder = dir("ntsc-frames");
    for n in 0..COUNT {
        let mut img = Rgba::new(64, 64);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let x = i as u32 % 64;
            px.copy_from_slice(&[(x * 4 + n) as u8, n as u8, 128, 255]);
        }
        fs::write(
            folder.join(format!("{n:06}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
    for (fps, num) in [("23.976", 24_000usize), ("47.95", 48_000)] {
        let last = (COUNT as usize - 1) * 1001 * RATE as usize / num;
        let total = COUNT as usize * 1001 * RATE as usize / num;
        let mut pcm = Vec::with_capacity((total + RATE as usize) * 4);
        for i in 0..total + RATE as usize {
            let v: i16 = if (last..last + 96).contains(&i) {
                29_000
            } else {
                0
            };
            pcm.extend_from_slice(&v.to_le_bytes());
            pcm.extend_from_slice(&v.to_le_bytes());
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
        let out = dir(&format!("ntsc-{num}"));
        let audio = out.join("pulse.wav");
        fs::write(&audio, wav).unwrap();
        let mp4 = out.join("ntsc.mp4");
        ok(&run(&[
            "frames",
            folder.to_str().unwrap(),
            mp4.to_str().unwrap(),
            "--fps",
            fps,
            "--audio",
            audio.to_str().unwrap(),
            "--route",
            "cpu",
        ]));
        let reader = pito_codecs::mp4::Reader::open(&mp4).unwrap();
        let t = &reader.tracks[reader.video().unwrap()];
        let ticks: u64 = t.samples.iter().map(|s| u64::from(s.duration)).sum();
        assert_eq!(t.samples.len(), COUNT as usize);
        assert_eq!(
            u128::from(ticks) * num as u128,
            u128::from(COUNT) * 1001 * u128::from(t.timescale)
        );
        let Some(pulse) = pulse_at(&mp4) else {
            continue;
        };
        assert_eq!(pulse, (last, total), "{fps}");
        for ext in ["mkv", "webm"] {
            let copy = out.join(format!("ntsc.{ext}"));
            ok(&run(&[mp4.to_str().unwrap(), copy.to_str().unwrap()]));
            let (at, _) = pulse_at(&copy).unwrap();
            assert!(
                at.abs_diff(last) <= 2,
                "{fps} {ext}: pulse at {at}, frame at {last}"
            );
        }
    }
}

fn wav(bits: u16, float: bool, extensible: bool, rf64: bool) -> Vec<u8> {
    const RATE: u32 = 48_000;
    let align = 2 * bits / 8;
    let data = vec![0u8; RATE as usize / 2 * usize::from(align)];
    let tag: u16 = if float { 3 } else { 1 };
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&(if extensible { 0xfffe } else { tag }).to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&RATE.to_le_bytes());
    fmt.extend_from_slice(&(RATE * u32::from(align)).to_le_bytes());
    fmt.extend_from_slice(&align.to_le_bytes());
    fmt.extend_from_slice(&bits.to_le_bytes());
    if extensible {
        fmt.extend_from_slice(&22u16.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(&3u32.to_le_bytes());
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&[0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
    }
    let mut body = Vec::new();
    if rf64 {
        body.extend_from_slice(b"ds64");
        body.extend_from_slice(&28u32.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes());
        body.extend_from_slice(&(data.len() as u64).to_le_bytes());
        body.extend_from_slice(&(RATE as u64 / 2).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
    } else if extensible {
        body.extend_from_slice(b"JUNK");
        body.extend_from_slice(&28u32.to_le_bytes());
        body.extend_from_slice(&[0; 28]);
    }
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    body.extend_from_slice(&fmt);
    body.extend_from_slice(b"data");
    let size = if rf64 { u32::MAX } else { data.len() as u32 };
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(&data);
    let mut file = Vec::new();
    file.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
    let riff = if rf64 {
        u32::MAX
    } else {
        4 + body.len() as u32
    };
    file.extend_from_slice(&riff.to_le_bytes());
    file.extend_from_slice(b"WAVE");
    file.extend_from_slice(&body);
    file
}

#[test]
fn audio_only_wavs_probe_as_audio() {
    let out = dir("probe-wav");
    let shapes = [
        (16, false, false, false),
        (16, false, true, false),
        (24, false, false, false),
        (24, false, true, false),
        (32, true, false, false),
        (32, true, true, false),
        (24, false, true, true),
    ];
    for (bits, float, extensible, rf64) in shapes {
        let name = format!("{bits}-{float}-{extensible}-{rf64}");
        let path = out.join(format!("{name}.wav"));
        fs::write(&path, wav(bits, float, extensible, rf64)).unwrap();
        let probed = run(&["probe", path.to_str().unwrap(), "--json"]);
        ok(&probed);
        let json = text(&probed.stdout);
        let bits = bits.to_string();
        let float = float.to_string();
        for (key, want) in [
            ("width", "null"),
            ("height", "null"),
            ("fps", "null"),
            ("codec", "null"),
            ("video_index", "null"),
            ("audio", "pcm"),
            ("audio_rate", "48000"),
            ("audio_channels", "2"),
            ("audio_bits", bits.as_str()),
            ("audio_float", float.as_str()),
            ("seconds", "0.5"),
            ("pix_fmt", "null"),
            ("chroma", "null"),
            ("interlaced", "null"),
            ("variant", "null"),
            ("frames", "null"),
        ] {
            assert_eq!(field(&json, key), Some(want), "{name} {key}: {json}");
        }
        let shown = run(&["probe", path.to_str().unwrap()]);
        ok(&shown);
        let shown = text(&shown.stdout);
        assert!(shown.starts_with("container  WAV"), "{name}: {shown}");
        assert!(!shown.contains("video"), "{name}: {shown}");
    }
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

type Shape = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

#[test]
fn probe_json_and_text_name_the_video_variant() {
    if !has_encoder("libx264") {
        eprintln!("no ffmpeg with libx264 on PATH: skipped");
        return;
    }
    let out = dir("probe-variant");
    let shapes: [Shape; 4] = [
        (
            "high422-10",
            &["-pix_fmt", "yuv422p10le", "-profile:v", "high422"],
            "4:2:2",
            "false",
            "10-bit 4:2:2 H.264",
            "yuv422p10le",
        ),
        (
            "high422-8",
            &["-pix_fmt", "yuv422p", "-profile:v", "high422"],
            "4:2:2",
            "false",
            "8-bit 4:2:2 H.264",
            "yuv422p",
        ),
        (
            "high",
            &["-pix_fmt", "yuv420p", "-profile:v", "high"],
            "4:2:0",
            "false",
            "8-bit 4:2:0 H.264",
            "yuv420p",
        ),
        (
            "interlaced",
            &[
                "-pix_fmt",
                "yuv420p",
                "-flags",
                "+ildct+ilme",
                "-x264opts",
                "tff=1",
            ],
            "4:2:0",
            "true",
            "interlaced 8-bit 4:2:0 H.264",
            "yuv420p",
        ),
    ];
    for (name, args, chroma, interlaced, variant, pix_fmt) in shapes {
        let path = out.join(format!("{name}.mp4"));
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=s=352x240:r=30:d=0.5")
            .args(["-c:v", "libx264"])
            .args(args)
            .arg(&path)
            .status()
            .unwrap();
        assert!(made.success(), "{name}");
        let probed = run(&["probe", path.to_str().unwrap(), "--json"]);
        ok(&probed);
        let json = text(&probed.stdout);
        for (key, want) in [
            ("codec", "h264"),
            ("chroma", chroma),
            ("interlaced", interlaced),
            ("variant", variant),
            ("pix_fmt", pix_fmt),
            ("frames", "15"),
        ] {
            assert_eq!(field(&json, key), Some(want), "{name} {key}: {json}");
        }
        let shown = run(&["probe", path.to_str().unwrap()]);
        ok(&shown);
        let shown = text(&shown.stdout);
        assert!(
            shown.lines().any(|l| l == format!("variant    {variant}")),
            "{name}: {shown}"
        );
    }
}

fn voice(path: &Path, seconds: f64) -> Vec<f32> {
    const RATE: u32 = 48_000;
    let n = (seconds * f64::from(RATE)) as usize;
    let mut seed = 0x1234_5678u32;
    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / f64::from(RATE);
        let env = if (i / 12_000) % 4 == 3 { 0.2 } else { 1.0 };
        for f in [440.0, 660.0] {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (f64::from(seed >> 9) / f64::from(1u32 << 23) - 0.5) * 0.05;
            let v = 0.3 * (std::f64::consts::TAU * f * t).sin()
                + 0.1 * (std::f64::consts::TAU * 3150.0 * t).sin()
                + noise;
            let q = (v * env * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            samples.push(f32::from(q) / 32768.0);
        }
    }
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|&s| ((s * 32768.0) as i16).to_le_bytes())
        .collect();
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
    fs::write(path, wav).unwrap();
    samples
}

fn snr_db(reference: &[f32], decoded: &[f32]) -> f64 {
    let (mut s, mut e) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(decoded) {
        s += f64::from(*a).powi(2);
        e += f64::from(*a - *b).powi(2);
    }
    10.0 * (s / e.max(1e-20)).log10()
}

#[test]
fn opus_output_round_trips_through_our_decoder() {
    let out = dir("opus");
    let wav = out.join("voice.wav");
    let stereo = voice(&wav, 2.0);
    let mono: Vec<f32> = stereo
        .chunks_exact(2)
        .map(|f| (f[0] + f[1]) / 2.0)
        .collect();
    let cases: [(&str, &[&str], usize, Option<f64>); 3] = [
        ("stereo.opus", &[], 2, Some(10.0)),
        ("mono.ogg", &["--mono"], 1, Some(10.0)),
        ("voice.opus", &["--mono", "--audio-bitrate", "16"], 1, None),
    ];
    let mut sizes = Vec::new();
    for (name, flags, channels, floor) in cases {
        let path = out.join(name);
        let mut args = vec![wav.to_str().unwrap(), path.to_str().unwrap()];
        args.extend_from_slice(flags);
        let result = run(&args);
        ok(&result);
        assert_eq!(text(&result.stdout).trim(), path.to_str().unwrap());
        let data = fs::read(&path).unwrap();
        sizes.push(data.len());
        let ogg = pito_codecs::opus::ogg::OggOpus::parse(&data).unwrap();
        assert!(ogg.warnings.is_empty(), "{name}: {:?}", ogg.warnings);
        assert_eq!(ogg.head.channels, channels, "{name}");
        assert_eq!(ogg.head.input_sample_rate, 48_000, "{name}");
        let decoded = ogg.decode().unwrap();
        assert_eq!((decoded.rate, decoded.channels), (48_000, channels as u16));
        assert_eq!(decoded.frames(), 96_000, "{name}");
        let reference = if channels == 2 { &stereo } else { &mono };
        let snr = snr_db(reference, &decoded.samples);
        eprintln!("{name}: {} bytes, snr {snr:.2} dB", data.len());
        if let Some(floor) = floor {
            assert!(snr > floor, "{name}: snr {snr:.2} dB, floor {floor} dB");
        }
        let p = probe(&path, &Prefs::default()).unwrap();
        assert_eq!(p.audio.len(), 1);
        assert_eq!(p.audio[0].codec, "opus");
        assert!((p.duration - 2.0).abs() < 1e-6, "{name}: {}", p.duration);
        let ch = channels as u16;
        let prefs = Prefs::default();
        let whole = audio_pcm_range(&path, 0, 0.0, f64::INFINITY, 48_000, ch, &prefs).unwrap();
        assert_eq!(whole.samples, decoded.samples, "{name}");
        let part = audio_pcm_range(&path, 0, 0.5, 1.25, 48_000, ch, &prefs).unwrap();
        assert_eq!(
            part.samples,
            decoded.samples[24_000 * channels..60_000 * channels],
            "{name}"
        );
        let tail = audio_pcm_range(&path, 0, 1.5, 9.0, 48_000, ch, &prefs).unwrap();
        assert_eq!(tail.samples, decoded.samples[72_000 * channels..], "{name}");
        assert!(audio_pcm_range(&path, 1, 0.0, 1.0, 48_000, ch, &prefs).is_err());
    }
    assert!(sizes[2] < sizes[1] && sizes[1] < sizes[0], "{sizes:?}");
    let again = run(&[
        wav.to_str().unwrap(),
        out.join("stereo.opus").to_str().unwrap(),
    ]);
    assert_eq!(again.status.code(), Some(1));
}

fn tone_power(samples: &[f32], stride: usize, offset: usize, hz: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / 48_000.0;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0f64, 0f64);
    let n = samples.len() / stride;
    for i in n / 8..n - n / 8 {
        let s0 = f64::from(samples[i * stride + offset]) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

#[test]
fn opus_output_keeps_the_voice_band() {
    let out = dir("opus-band");
    let wav = out.join("voice.wav");
    let stereo = voice(&wav, 2.0);
    let mono: Vec<f32> = stereo
        .chunks_exact(2)
        .map(|f| (f[0] + f[1]) / 2.0)
        .collect();
    let cases: [(&str, &[&str], usize); 3] = [
        ("mono16.opus", &["--mono", "--audio-bitrate", "16"], 1),
        ("stereo24.opus", &["--audio-bitrate", "24"], 2),
        ("mono24.opus", &["--mono", "--audio-bitrate", "24"], 1),
    ];
    let mut wrong = Vec::new();
    for (name, flags, channels) in cases {
        let path = out.join(name);
        let mut args = vec![wav.to_str().unwrap(), path.to_str().unwrap()];
        args.extend_from_slice(flags);
        ok(&run(&args));
        let data = fs::read(&path).unwrap();
        let ogg = pito_codecs::opus::ogg::OggOpus::parse(&data).unwrap();
        assert!(ogg.warnings.is_empty(), "{name}: {:?}", ogg.warnings);
        assert_eq!(ogg.head.channels, channels, "{name}");
        for i in 0..ogg.packets.len() {
            let toc = ogg.packet(i)[0];
            assert!(toc >> 3 >= 16, "{name}: packet {i} toc {toc:#04x}");
            assert_eq!(
                toc & 4 != 0,
                channels == 2,
                "{name}: packet {i} toc {toc:#04x}"
            );
        }
        let decoded = ogg.decode().unwrap();
        assert_eq!((decoded.rate, decoded.channels), (48_000, channels as u16));
        assert_eq!(decoded.frames(), 96_000, "{name}");
        let p = probe(&path, &Prefs::default()).unwrap();
        assert!((p.duration - 2.0).abs() < 1e-6, "{name}: {}", p.duration);
        for ch in 0..channels {
            let (reference, stride, tones, absent): (&[f32], usize, &[f64], Option<f64>) =
                match (channels, ch) {
                    (1, _) => (&mono, 1, &[440.0, 660.0, 3150.0], None),
                    (_, 0) => (&stereo, 2, &[440.0, 3150.0], Some(660.0)),
                    _ => (&stereo, 2, &[660.0, 3150.0], Some(440.0)),
                };
            for &hz in tones {
                let want = tone_power(reference, stride, ch, hz);
                let got = tone_power(&decoded.samples, channels, ch, hz);
                let db = 10.0 * (got / want).log10();
                eprintln!("{name}: channel {ch} {hz} Hz {db:+.2} dB");
                if db.abs() >= 1.0 {
                    wrong.push(format!("{name}: channel {ch} {hz} Hz off by {db:.2} dB"));
                }
            }
            if let Some(hz) = absent {
                let leak = tone_power(&decoded.samples, channels, ch, hz)
                    / tone_power(&decoded.samples, channels, ch, tones[0]);
                let db = 10.0 * leak.log10();
                eprintln!("{name}: channel {ch} {hz} Hz leak {db:+.2} dB");
                if db >= -20.0 {
                    wrong.push(format!(
                        "{name}: channel {ch} carries {hz} Hz at {db:.2} dB"
                    ));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn spectrum_writes_a_png_of_the_asked_size() {
    let out = dir("spectrum");
    let wav = out.join("voice.wav");
    voice(&wav, 1.5);
    let png_path = out.join("voice.png");
    let result = run(&[
        "spectrum",
        wav.to_str().unwrap(),
        png_path.to_str().unwrap(),
        "--json",
    ]);
    ok(&result);
    let json = text(&result.stdout);
    assert_eq!(field(&json, "width"), Some("1200"));
    assert_eq!(field(&json, "height"), Some("360"));
    assert_eq!(field(&json, "window"), Some("2048"));
    assert_eq!(field(&json, "max_hz"), Some("12000"));
    assert_eq!(field(&json, "floor_db"), Some("-90"));
    let img = pito_codecs::png::decode(&fs::read(&png_path).unwrap()).unwrap();
    assert_eq!((img.width, img.height), (1200, 360));
    let opus = out.join("voice.opus");
    ok(&run(&[wav.to_str().unwrap(), opus.to_str().unwrap()]));
    let small = out.join("small.png");
    ok(&run(&[
        "spectrum",
        opus.to_str().unwrap(),
        small.to_str().unwrap(),
        "--width",
        "640",
        "--height",
        "200",
        "--max-hz",
        "8000",
        "--floor-db",
        "-60",
    ]));
    let img = pito_codecs::png::decode(&fs::read(&small).unwrap()).unwrap();
    assert_eq!((img.width, img.height), (640, 200));
    let wrong = run(&[
        "spectrum",
        wav.to_str().unwrap(),
        out.join("voice.jpg").to_str().unwrap(),
    ]);
    assert_eq!(wrong.status.code(), Some(1));
    assert!(text(&wrong.stderr).starts_with("pcod: "));
    let track = run(&[
        "spectrum",
        wav.to_str().unwrap(),
        out.join("track.png").to_str().unwrap(),
        "--track",
        "3",
    ]);
    assert_eq!(track.status.code(), Some(1));
    assert!(text(&track.stderr).contains("no audio track 3"));
    let missing = run(&[
        "spectrum",
        out.join("gone.wav").to_str().unwrap(),
        out.join("gone.png").to_str().unwrap(),
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).starts_with("pcod: "));
    assert!(!out.join("track.png").exists() && !out.join("gone.png").exists());
    assert!(partials(&out).is_empty(), "{:?}", partials(&out));
}

#[test]
fn spectrum_keeps_an_existing_output_unless_forced() {
    let out = dir("spectrum-force");
    let wav = out.join("voice.wav");
    voice(&wav, 1.0);
    let png_path = out.join("cover.png");
    fs::write(&png_path, b"keep me").unwrap();
    let args = [
        "spectrum",
        wav.to_str().unwrap(),
        png_path.to_str().unwrap(),
    ];
    let refused = run(&args);
    assert_eq!(refused.status.code(), Some(1));
    let err = text(&refused.stderr);
    assert!(
        err.starts_with("pcod: ") && err.contains("already exists"),
        "{err}"
    );
    assert!(err.contains("--force"), "{err}");
    assert_eq!(fs::read(&png_path).unwrap(), b"keep me");
    let disguised = out.join("audio.png");
    fs::copy(&wav, &disguised).unwrap();
    let d = disguised.to_str().unwrap();
    let same = run(&["spectrum", d, d, "--force"]);
    assert_eq!(same.status.code(), Some(1));
    assert!(text(&same.stderr).contains("the output is the input"));
    assert_eq!(fs::read(&disguised).unwrap(), fs::read(&wav).unwrap());
    let mut forced = args.to_vec();
    forced.push("--force");
    ok(&run(&forced));
    let img = pito_codecs::png::decode(&fs::read(&png_path).unwrap()).unwrap();
    assert_eq!((img.width, img.height), (1200, 360));
    assert!(partials(&out).is_empty(), "{:?}", partials(&out));
}

fn ogg_page(body: &[u8]) -> Vec<u8> {
    let mut page = Vec::new();
    page.extend_from_slice(b"OggS");
    page.extend_from_slice(&[0, 2]);
    page.extend_from_slice(&[0; 8]);
    page.extend_from_slice(&7u32.to_le_bytes());
    page.extend_from_slice(&[0; 8]);
    page.push(1);
    page.push(body.len() as u8);
    page.extend_from_slice(body);
    page
}

#[test]
fn ogg_that_is_not_opus_is_refused_by_name() {
    let out = dir("ogg-other");
    let mut speex = b"Speex   ".to_vec();
    speex.resize(30, 0);
    let mut flac = b"\x7fFLAC".to_vec();
    flac.resize(30, 0);
    let cases = [
        (
            "talk.ogg",
            ogg_page(&speex),
            "Ogg Speex audio is not supported",
        ),
        (
            "talk.oga",
            ogg_page(&flac),
            "Ogg FLAC audio is not supported",
        ),
        (
            "odd.ogg",
            ogg_page(&[9; 30]),
            "unknown Ogg audio is not supported",
        ),
    ];
    for (name, data, want) in cases {
        let path = out.join(name);
        fs::write(&path, data).unwrap();
        let err = probe(&path, &Prefs::default()).unwrap_err().to_string();
        assert!(err.contains(want), "{name}: {err}");
        let err = audio_pcm_range(&path, 0, 0.0, 1.0, 48_000, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains(want), "{name}: {err}");
        for args in [
            vec!["probe", path.to_str().unwrap()],
            vec![
                "spectrum",
                path.to_str().unwrap(),
                out.join("s.png").to_str().unwrap(),
            ],
            vec![path.to_str().unwrap(), out.join("o.wav").to_str().unwrap()],
        ] {
            let r = run(&args);
            assert_eq!(r.status.code(), Some(1), "{args:?}");
            let stderr = text(&r.stderr);
            assert!(stderr.contains(want), "{args:?}: {stderr}");
            assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
        }
    }
}

fn wav_body(data: &[u8]) -> (u16, u32, Vec<i16>) {
    assert_eq!(&data[..4], b"RIFF");
    assert_eq!(&data[8..12], b"WAVE");
    let mut pos = 12;
    let (mut channels, mut rate) = (0, 0);
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &data[pos + 8..(pos + 8 + len).min(data.len())];
        if id == b"fmt " {
            channels = u16::from_le_bytes([body[2], body[3]]);
            rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
        } else if id == b"data" {
            let pcm = body
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();
            return (channels, rate, pcm);
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

#[test]
fn ogg_and_webm_vorbis_extract_to_wav_and_convert_to_opus() {
    if !has_encoder("libvorbis") {
        eprintln!("skipped Vorbis through pcod: ffmpeg has no libvorbis encoder");
        return;
    }
    let out = dir("vorbis");
    for ext in ["ogg", "webm"] {
        let src = out.join(format!("tone.{ext}"));
        let made = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("aevalsrc=0.3*sin(440*2*PI*t)|0.2*sin(660*2*PI*t):s=44100:d=0.7:c=stereo")
            .args(["-c:a", "libvorbis", "-q:a", "3"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success(), "{ext}");
        let wav = out.join(format!("{ext}.wav"));
        ok(&run(&[src.to_str().unwrap(), wav.to_str().unwrap()]));
        let (channels, rate, ours) = wav_body(&fs::read(&wav).unwrap());
        assert_eq!((channels, rate), (2, 44_100), "{ext}");
        assert_eq!(ours.len(), 30_870 * 2, "{ext}");
        let theirs: Vec<i16> = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&src)
            .args(["-f", "s16le", "-"])
            .output()
            .unwrap()
            .stdout
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let n = ours.len().min(theirs.len());
        assert!(
            n + 256 * 2 >= ours.len(),
            "{ext}: ffmpeg gave {}",
            theirs.len()
        );
        let worst = ours[..n]
            .iter()
            .zip(&theirs[..n])
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap();
        eprintln!("{ext}: worst 16-bit difference {worst} over {n} samples");
        assert!(worst <= 1, "{ext}: worst 16-bit difference {worst}");
        let opus = out.join(format!("{ext}.opus"));
        ok(&run(&[src.to_str().unwrap(), opus.to_str().unwrap()]));
        let ogg = pito_codecs::opus::ogg::OggOpus::parse(&fs::read(&opus).unwrap()).unwrap();
        let decoded = ogg.decode().unwrap();
        assert_eq!(decoded.channels, 2, "{ext}");
        assert!(
            decoded.frames().abs_diff(33_600) <= 1,
            "{ext}: {} frames",
            decoded.frames()
        );
        assert!(decoded.samples.iter().any(|&v| v.abs() > 0.05), "{ext}");
    }
}

#[test]
fn an_id3_tagged_flac_is_labelled_flac() {
    let out = dir("id3-flac");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flac/s16_stereo_44k.flac");
    let mut data = b"ID3\x03\x00\x00\x00\x00\x01\x05".to_vec();
    data.extend(std::iter::repeat_n(0u8, 133));
    data.extend_from_slice(&fs::read(&fixture).unwrap());
    for (name, label) in [
        ("tagged.flac", "container  FLAC"),
        ("tagged.FLAC", "container  FLAC"),
    ] {
        let path = out.join(name);
        fs::write(&path, &data).unwrap();
        let shown = run(&["probe", path.to_str().unwrap()]);
        ok(&shown);
        let shown = text(&shown.stdout);
        assert!(shown.starts_with(label), "{name}: {shown}");
        assert!(
            shown.contains("audio      flac 2 ch 44100 Hz"),
            "{name}: {shown}"
        );
    }
    let plain = run(&["probe", fixture.to_str().unwrap()]);
    ok(&plain);
    assert!(text(&plain.stdout).starts_with("container  FLAC"));
}

#[test]
fn frames_refuses_an_existing_output_without_force_and_replaces_it_with() {
    let folder = dir("frames-force");
    let input = frames("frames-force").to_str().unwrap().to_string();
    let output = folder.join("out.mp4");
    let path = output.to_str().unwrap();
    fs::write(&output, b"old").unwrap();
    let args = ["frames", &input, path, "--fps", "10", "--route", "cpu"];
    let out = run(&args);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert_eq!(
        stderr,
        format!("pcod: {path} already exists; use --force to replace it\n")
    );
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
    assert_eq!(fs::read(&output).unwrap(), b"old");
    let forced = run(&[&args[..], &["--force"]].concat());
    ok(&forced);
    assert_ne!(fs::read(&output).unwrap(), b"old");
    let p = probe(&output, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height, p.frames), (320, 240, Some(20)));
    assert!(partials(&folder).is_empty());
    let fresh = folder.join("fresh.mp4");
    ok(&run(&[
        "frames",
        &input,
        fresh.to_str().unwrap(),
        "--fps",
        "10",
        "--route",
        "cpu",
    ]));
    assert!(fresh.exists());
    assert!(partials(&folder).is_empty());
}

#[test]
fn frames_that_fail_leave_neither_output_nor_partial() {
    let folder = dir("frames-fail");
    let input = frames("frames-fail");
    fs::write(input.join("0005.png"), b"not a png").unwrap();
    let output = folder.join("out.mp4");
    let out = run(&[
        "frames",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--fps",
        "10",
        "--route",
        "cpu",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).starts_with("pcod: "));
    assert!(!output.exists());
    assert!(partials(&folder).is_empty());
    fs::write(&output, b"old").unwrap();
    let forced = run(&[
        "frames",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--fps",
        "10",
        "--route",
        "cpu",
        "--force",
    ]);
    assert_eq!(forced.status.code(), Some(1));
    assert!(
        !text(&forced.stdout).contains("wrote"),
        "{}",
        text(&forced.stdout)
    );
    assert_eq!(fs::read(&output).unwrap(), b"old");
    assert!(partials(&folder).is_empty());
}

#[test]
fn frames_crf_0_is_near_lossless_against_the_source() {
    use pito_codecs::filter::{ColorSpec, Order, Packed, packed_to_yuv};
    use pito_codecs::media::{Frame, PixelFormat, TimeBase};
    let folder = dir("frames-crf0");
    let input = frames("frames-crf0");
    let output = folder.join("crf0.mp4");
    ok(&run(&[
        "frames",
        input.to_str().unwrap(),
        output.to_str().unwrap(),
        "--fps",
        "10",
        "--crf",
        "0",
        "--route",
        "cpu",
    ]));
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success(), "{}", text(&decoded.stderr));
    let (w, h) = (320usize, 240usize);
    let picture = w * h * 3 / 2;
    assert_eq!(decoded.stdout.len(), picture * 20);
    let mut squared = 0f64;
    let mut identical = true;
    for n in 0..20 {
        let png = fs::read(input.join(format!("{n:04}.png"))).unwrap();
        let rgba = pito_codecs::png::decode(&png).unwrap();
        let mut want = Frame::new(PixelFormat::Yuv420p8, 320, 240, 0, TimeBase::new(1, 1));
        let source = Packed::new(rgba.width, rgba.height, Order::Rgbx, &rgba.data);
        packed_to_yuv(source, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut want).unwrap();
        let luma = &want.planes[0];
        let got = &decoded.stdout[n * picture..n * picture + w * h];
        for y in 0..h {
            for x in 0..w {
                let d = f64::from(luma.data[y * luma.stride + x]) - f64::from(got[y * w + x]);
                identical &= d == 0.0;
                squared += d * d;
            }
        }
    }
    let mse = squared / (w * h * 20) as f64;
    let psnr = if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    };
    eprintln!("frames --crf 0 luma PSNR: {psnr:.2} dB");
    assert!(psnr > 54.0, "{psnr}");
    assert!(
        !identical,
        "--crf 0 came back bit-exact; the help says it is not"
    );
}

fn ramp_wav(seconds: usize) -> (Vec<u8>, Vec<i16>) {
    let frames = 48_000 * seconds;
    let samples: Vec<i16> = (0..frames)
        .flat_map(|i| {
            let t = i as f64 / 48_000.0;
            let l = ((t * 440.0 * std::f64::consts::TAU).sin() * 12_000.0) as i16;
            let r = ((t * 150.0 * std::f64::consts::TAU).sin() * 9_000.0) as i16;
            [l, r]
        })
        .collect();
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut file = Vec::with_capacity(44 + data.len());
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&2u16.to_le_bytes());
    file.extend_from_slice(&48_000u32.to_le_bytes());
    file.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    file.extend_from_slice(&4u16.to_le_bytes());
    file.extend_from_slice(&16u16.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(&data);
    (file, samples)
}

fn le(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[test]
fn raw_s16_goes_to_a_file_or_stdout_from_any_start() {
    let out = dir("raw-s16");
    let input = out.join("ramp.wav");
    let (file, samples) = ramp_wav(2);
    fs::write(&input, file).unwrap();
    let input_s = input.to_str().unwrap();

    let piped = run(&[input_s, "-", "--from", "1"]);
    ok(&piped);
    assert!(piped.stderr.is_empty(), "{}", text(&piped.stderr));
    assert_eq!(
        le(&piped.stdout),
        samples[48_000 * 2..],
        "stereo at 48000 Hz is the source as it is"
    );

    let whole = run(&[input_s, "-", "--track", "0"]);
    ok(&whole);
    assert_eq!(le(&whole.stdout), samples);

    let target = out.join("voice.s16");
    let target_s = target.to_str().unwrap();
    let written = run(&[input_s, target_s, "--mono", "--rate", "16000"]);
    ok(&written);
    assert_eq!(text(&written.stdout).trim(), target_s);
    let mono = le(&fs::read(&target).unwrap());
    assert!(mono.len().abs_diff(32_000) <= 160, "{} samples", mono.len());
    let own = audio_pcm_range(&input, 0, 0.0, f64::INFINITY, 16_000, 1, &Prefs::default()).unwrap();
    let own: Vec<i16> = own
        .samples
        .iter()
        .map(|&s| pito_codecs::media::to_s16(s))
        .collect();
    let n = mono.len().min(own.len()) - 160;
    let worst = mono[..n]
        .iter()
        .zip(&own[..n])
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap();
    assert!(
        worst <= 8,
        "the streamed mono differs from the in-memory one by {worst}"
    );
    assert!(partials(&out).is_empty());

    let again = run(&[input_s, target_s, "--mono"]);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).contains("already exists"));
    ok(&run(&[input_s, target_s, "--mono", "--force"]));
    assert_eq!(le(&fs::read(&target).unwrap()).len(), 96_000);
}

#[test]
fn raw_s16_refusals_are_one_line_and_bad_flags_exit_2() {
    let out = dir("raw-s16-refused");
    let input = out.join("ramp.wav");
    fs::write(&input, ramp_wav(1).0).unwrap();
    let input_s = input.to_str().unwrap();
    for (args, said) in [
        (&["--track", "3"][..], "no audio stream 3"),
        (&["--from", "5"], "past the end"),
    ] {
        let mut all = vec![input_s, "-"];
        all.extend_from_slice(args);
        let got = run(&all);
        let stderr = text(&got.stderr);
        assert_eq!(got.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(got.stdout.is_empty(), "{args:?}");
        assert!(
            stderr.starts_with("pcod: ") && stderr.contains(said),
            "{args:?}: {stderr}"
        );
        assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
    }
    let same = out.join("ramp.s16");
    fs::copy(&input, &same).unwrap();
    let same_s = same.to_str().unwrap();
    let got = run(&[same_s, same_s, "--force"]);
    assert_eq!(got.status.code(), Some(1), "{}", text(&got.stderr));
    assert!(text(&got.stderr).contains("the output is the input"));
    assert_eq!(fs::read(&same).unwrap(), fs::read(&input).unwrap());
    let missing = run(&["nothing-here.mkv", "-"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("nothing-here.mkv"));
    let folder = run(&[out.to_str().unwrap(), "-"]);
    let stderr = text(&folder.stderr);
    assert_eq!(folder.status.code(), Some(2), "{stderr}");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("not a folder of frames"), "{stderr}");
    assert!(folder.stdout.is_empty());
    let cases: &[&[&str]] = &[
        &["in.mkv", "out.s16", "--to", "3"],
        &["in.mkv", "out.s16", "--json"],
        &["in.mkv", "-", "--loudness", "-16"],
        &["in.mkv", "-", "--crf", "20"],
        &["in.mkv", "-", "--rate", "0"],
        &["in.mkv", "-", "--rate", "16"],
        &["in.mkv", "-", "--rate", "7999"],
        &["in.mkv", "-", "--track", "x"],
        &["in.mkv", "out.mp4", "--track", "1"],
        &["in.mkv", "out.wav", "--track", "1"],
        &["in.mkv", "out.wav", "--rate", "16000"],
        &["join", "a.mkv", "b.mkv", "out.s16"],
    ];
    for args in cases {
        let got = run(args);
        let stderr = text(&got.stderr);
        assert_eq!(got.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        assert!(got.stdout.is_empty(), "{args:?}");
    }
    for (flag, value) in [
        ("--quality", Some("80")),
        ("--lossless", None),
        ("--no-audio", None),
        ("--add-audio", Some("bed.wav")),
    ] {
        for target in ["out.s16", "-"] {
            let mut args = vec!["in.mkv", target, flag];
            args.extend(value);
            let got = run(&args);
            let stderr = text(&got.stderr);
            assert_eq!(got.status.code(), Some(2), "{args:?}: {stderr}");
            assert!(
                stderr.starts_with(&format!("error: {flag} doesn't apply to raw PCM")),
                "{args:?}: {stderr}"
            );
            assert!(got.stdout.is_empty(), "{args:?}");
        }
    }
}

#[test]
fn raw_s16_track_picks_the_stream_the_container_numbers() {
    if !has_encoder("mpeg4") {
        eprintln!("no ffmpeg with mpeg4 on PATH: the raw --track test is skipped");
        return;
    }
    let out = dir("raw-s16-track");
    let input = out.join("two-tracks.mkv");
    let made = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=s=352x240:r=30:d=2"])
        .args(["-f", "lavfi", "-i", "sine=f=300:r=48000:d=2"])
        .args(["-f", "lavfi", "-i", "sine=f=1200:r=44100:d=2"])
        .args(["-map", "0:v", "-map", "1:a", "-map", "2:a"])
        .args(["-c:v", "mpeg4", "-c:a", "aac", "-b:a", "128k"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(made.success());
    let input_s = input.to_str().unwrap();
    let decode = |stream: usize| {
        let span = pito_codecs::media::S16Span {
            stream,
            from: 0.5,
            rate: 16_000,
            mono: true,
        };
        let mut all = Vec::new();
        pito_codecs::media::decode_s16(&input, &span, &Prefs::default(), |c| {
            all.extend_from_slice(c);
            Ok(())
        })
        .unwrap();
        all
    };
    for stream in [1usize, 2] {
        let n = stream.to_string();
        let got = run(&[
            input_s, "-", "--track", &n, "--from", "0.5", "--mono", "--rate", "16000",
        ]);
        ok(&got);
        let got = le(&got.stdout);
        assert!(
            got.len().abs_diff(24_000) <= 800,
            "stream {stream}: {} samples",
            got.len()
        );
        assert_eq!(got, decode(stream), "stream {stream}");
    }
    assert_ne!(decode(1), decode(2));
    let first = run(&[input_s, "-", "--from", "0.5", "--mono", "--rate", "16000"]);
    ok(&first);
    assert_eq!(
        le(&first.stdout),
        decode(1),
        "no --track takes the first audio stream"
    );
    let video = run(&[input_s, "-", "--track", "0"]);
    assert_eq!(video.status.code(), Some(1));
    assert!(
        text(&video.stderr).contains("no audio stream 0"),
        "{}",
        text(&video.stderr)
    );
}

const MKV_RATES: [(&str, (u32, u32)); 6] = [
    ("23.976", (24000, 1001)),
    ("24", (24, 1)),
    ("25", (25, 1)),
    ("29.97", (30000, 1001)),
    ("30", (30, 1)),
    ("60", (60, 1)),
];

fn counted_frames(root: &Path, count: u32, shade: u32) -> PathBuf {
    let folder = root.join(format!("frames-{count}-{shade}"));
    fs::create_dir_all(&folder).unwrap();
    for n in 0..count {
        let mut img = Rgba::new(64, 64);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let x = i as u32 % 64;
            px.copy_from_slice(&[(x * 4 + n * 3) as u8, (n * 5 + shade) as u8, 96, 255]);
        }
        fs::write(
            folder.join(format!("{n:04}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
    folder
}

fn encoded(folder: &Path, out: &Path, fps: &str) {
    ok(&run(&[
        folder.to_str().unwrap(),
        out.to_str().unwrap(),
        "--fps",
        fps,
    ]));
}

fn without_default_duration(src: &Path, out: &Path) {
    let r = pito_codecs::mkv::Reader::open(src).unwrap();
    let v = r.video().unwrap();
    let options = pito_codecs::mkv::Options {
        webm: r.is_webm(),
        ..pito_codecs::mkv::Options::default()
    };
    let mut w = pito_codecs::mkv::Writer::create_with(out, options).unwrap();
    let mut cfg = pito_codecs::mkv::TrackConfig::from_track(&r.tracks[v]);
    cfg.default_duration = None;
    let t = w.add_track(cfg).unwrap();
    for (i, s) in r.tracks[v].samples.iter().enumerate() {
        let data = r.read(v, i).unwrap();
        w.push_block(t, &data, s.pts, None, s.keyframe, None)
            .unwrap();
    }
    w.finish().unwrap();
    let copy = pito_codecs::mkv::Reader::open(out).unwrap();
    assert_eq!(copy.tracks[copy.video().unwrap()].default_duration, None);
}

#[test]
fn short_webm_and_mkv_parts_probe_at_their_exact_rates() {
    let root = dir("mkv-rates");
    for count in [6u32, 12, 60] {
        let folder = counted_frames(&root, count, 0);
        for (fps, want) in MKV_RATES {
            for ext in ["webm", "mkv"] {
                let part = root.join(format!("{count}-{fps}.{ext}"));
                encoded(&folder, &part, fps);
                let r = pito_codecs::mkv::Reader::open(&part).unwrap();
                let t = &r.tracks[r.video().unwrap()];
                assert!(t.default_duration.is_some());
                assert_eq!(t.samples.len(), count as usize, "{count} at {fps} {ext}");
                let p = probe(&part, &Prefs::default()).unwrap();
                assert_eq!(p.fps, want, "{count} frames at {fps} in {ext}");
                let ambiguous = count == 6 && fps == "29.97";
                if ambiguous {
                    continue;
                }
                let bare = root.join(format!("{count}-{fps}-bare.{ext}"));
                without_default_duration(&part, &bare);
                let p = probe(&bare, &Prefs::default()).unwrap();
                assert_eq!(
                    p.fps, want,
                    "{count} frames at {fps} in {ext}, block times only"
                );
                assert_eq!(p.frames, Some(u64::from(count)));
            }
        }
    }
}

fn joined(parts: &[&Path], out: &Path) -> String {
    let mut args = vec!["join"];
    args.extend(parts.iter().map(|p| p.to_str().unwrap()));
    args.push(out.to_str().unwrap());
    let done = run(&args);
    ok(&done);
    text(&done.stdout)
}

#[test]
fn three_short_24_fps_webm_parts_join_by_copy() {
    let root = dir("mkv-join-copy");
    let mut parts = Vec::new();
    let mut bare = Vec::new();
    for k in 0..3u32 {
        let folder = counted_frames(&root, 12, k * 40);
        let part = root.join(format!("part-{k}.webm"));
        encoded(&folder, &part, "24");
        let stripped = root.join(format!("bare-{k}.webm"));
        without_default_duration(&part, &stripped);
        parts.push(part);
        bare.push(stripped);
    }
    for (name, set) in [("ours", &parts), ("bare", &bare)] {
        let out = root.join(format!("{name}-joined.webm"));
        let refs: Vec<&Path> = set.iter().map(PathBuf::as_path).collect();
        let said = joined(&refs, &out);
        assert!(
            said.contains("by stream copy, no re-encode"),
            "{name}: {said}"
        );
        let p = probe(&out, &Prefs::default()).unwrap();
        assert_eq!(p.fps, (24, 1), "{name}");
        assert!((p.duration - 1.5).abs() < 0.01, "{name}: {}", p.duration);
        let r = pito_codecs::mkv::Reader::open(&out).unwrap();
        let t = &r.tracks[r.video().unwrap()];
        assert_eq!(t.samples.len(), 36, "{name}");
        let pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
        assert!(pts.windows(2).all(|w| w[1] > w[0]), "{name}: {pts:?}");
        let parted: Vec<Vec<u8>> = set
            .iter()
            .flat_map(|p| {
                let r = pito_codecs::mkv::Reader::open(p).unwrap();
                let v = r.video().unwrap();
                (0..r.tracks[v].samples.len())
                    .map(|i| r.read(v, i).unwrap())
                    .collect::<Vec<_>>()
            })
            .collect();
        let v = r.video().unwrap();
        let whole: Vec<Vec<u8>> = (0..t.samples.len())
            .map(|i| r.read(v, i).unwrap())
            .collect();
        assert_eq!(whole, parted, "{name}: the copy keeps every frame's bytes");
    }
}

#[test]
fn webm_parts_at_24_and_25_fps_re_encode_and_say_why() {
    let root = dir("mkv-join-mixed");
    let folder = counted_frames(&root, 12, 0);
    let slow = root.join("slow.webm");
    let fast = root.join("fast.webm");
    encoded(&folder, &slow, "24");
    encoded(&folder, &fast, "25");
    let out = root.join("mixed.webm");
    let said = joined(&[&slow, &fast], &out);
    assert!(said.contains("by re-encoding"), "{said}");
    assert!(
        said.contains(&format!(
            "{} runs at 25 fps, not 24 fps like {}",
            fast.display(),
            slow.display()
        )),
        "{said}"
    );
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!(p.fps, (24, 1));
    assert!(
        (p.duration - (0.5 + 12.0 / 25.0)).abs() < 0.05,
        "{}",
        p.duration
    );
}

#[test]
fn bare_twelve_frame_webm_parts_at_close_rates_re_encode_and_say_why() {
    let root = dir("mkv-join-close");
    let folder = counted_frames(&root, 12, 0);
    for (slow, fast, said_slow, said_fast) in [
        ("23.976", "24", "23.98 fps", "24 fps"),
        ("29.97", "30", "29.97 fps", "30 fps"),
    ] {
        let mut bare = Vec::new();
        for fps in [slow, fast] {
            let part = root.join(format!("{fps}.webm"));
            encoded(&folder, &part, fps);
            let stripped = root.join(format!("bare-{fps}.webm"));
            without_default_duration(&part, &stripped);
            bare.push(stripped);
        }
        let out = root.join(format!("{slow}-{fast}.webm"));
        let said = joined(&[&bare[0], &bare[1]], &out);
        assert!(said.contains("by re-encoding"), "{slow}/{fast}: {said}");
        assert!(
            said.contains(&format!(
                "{} runs at {said_fast}, not {said_slow} like {}",
                bare[1].display(),
                bare[0].display()
            )),
            "{slow}/{fast}: {said}"
        );
    }
}

#[test]
fn a_bare_six_frame_29_97_part_joins_a_29_97_part_by_copy() {
    let root = dir("mkv-join-six");
    let first = root.join("first.webm");
    encoded(&counted_frames(&root, 6, 0), &first, "29.97");
    let second = root.join("second.webm");
    encoded(&counted_frames(&root, 6, 40), &second, "29.97");
    let bare = root.join("second-bare.webm");
    without_default_duration(&second, &bare);
    assert_eq!(probe(&bare, &Prefs::default()).unwrap().fps, (30, 1));
    let out = root.join("joined.webm");
    let said = joined(&[&first, &bare], &out);
    assert!(said.contains("by stream copy, no re-encode"), "{said}");
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!(p.fps, (30000, 1001));
    let frame = 1001.0 / 30000.0;
    assert!((p.duration - 12.0 * frame).abs() < frame, "{}", p.duration);
    let r = pito_codecs::mkv::Reader::open(&out).unwrap();
    let t = &r.tracks[r.video().unwrap()];
    assert_eq!(t.samples.len(), 12);
    let mut pts: Vec<f64> = t.samples.iter().map(|s| t.seconds(s.pts)).collect();
    pts.sort_by(f64::total_cmp);
    for (i, w) in pts.windows(2).enumerate() {
        let step = w[1] - w[0];
        assert!((step - frame).abs() < 0.0015, "step {i}: {step}");
    }
}

fn hum_wav(amplitude: f64, seconds: usize, clicks: bool) -> Vec<u8> {
    const RATE: u32 = 48_000;
    let mut pcm = Vec::new();
    for n in 0..seconds * RATE as usize {
        let hum =
            amplitude * (2.0 * std::f64::consts::PI * 100.0 * n as f64 / f64::from(RATE)).sin();
        let s = if clicks && n % (RATE as usize / 2) < 4 {
            0.9
        } else {
            hum
        };
        let v = (s * 32767.0).round() as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
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

#[test]
fn calibrated_levelling_that_lands_short_says_so_in_text_and_json() {
    let work = dir("loudness-short");
    let input = work.join("hum.wav");
    fs::write(&input, hum_wav(0.1, 10, false)).unwrap();
    let out = work.join("out.wav");
    let args = [
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--loudness",
        "-5",
        "--true-peak",
        "-9",
    ];
    let shown = run(&args);
    ok(&shown);
    let shown = text(&shown.stdout);
    let short: Vec<&str> = shown
        .lines()
        .filter(|l| l.starts_with("loudness short: "))
        .collect();
    assert_eq!(short.len(), 1, "{shown}");
    assert!(short[0].contains("-5 LUFS target"), "{shown}");
    assert!(short[0].contains("-9.30 dBTP"), "{shown}");
    let json = run(&[&args[..], &["--json", "--force"]].concat());
    ok(&json);
    let v: serde_json::Value = serde_json::from_str(&text(&json.stdout)).unwrap();
    let c = &v["params"]["loudness"]["calibration"];
    assert_eq!(c["reached"], false, "{v}");
    let passes = c["passes"].as_u64().unwrap();
    assert!(passes >= 1, "{v}");
    assert!(c["landed"].as_f64().unwrap() < -5.1, "{v}");
    assert!(
        c["short"]
            .as_str()
            .unwrap()
            .contains(&format!("after {passes} passes")),
        "{v}"
    );
    assert!(
        v["files"][0]["loudness"]["true_peak"].as_f64().unwrap() <= -9.0,
        "{v}"
    );
}

#[test]
fn calibrated_levelling_that_reaches_the_target_reports_its_passes_without_a_short_line() {
    let work = dir("loudness-reached");
    let input = work.join("hum.wav");
    fs::write(&input, hum_wav(0.05, 10, true)).unwrap();
    let out = work.join("out.wav");
    let args = [
        input.to_str().unwrap(),
        out.to_str().unwrap(),
        "--loudness",
        "-14",
        "--true-peak",
        "-1",
    ];
    let shown = run(&args);
    ok(&shown);
    assert!(
        !text(&shown.stdout).contains("loudness short"),
        "{}",
        text(&shown.stdout)
    );
    let json = run(&[&args[..], &["--json", "--force"]].concat());
    ok(&json);
    let v: serde_json::Value = serde_json::from_str(&text(&json.stdout)).unwrap();
    assert_eq!(v["params"]["loudness"]["mode"], "limited", "{v}");
    let c = &v["params"]["loudness"]["calibration"];
    assert_eq!(c["reached"], true, "{v}");
    assert!(c["short"].is_null(), "{v}");
    assert!(c["passes"].as_u64().unwrap() >= 1, "{v}");
    let measured = run(&["loudness", out.to_str().unwrap(), "--json"]);
    ok(&measured);
    let m: serde_json::Value = serde_json::from_str(&text(&measured.stdout)).unwrap();
    let integrated = m["integrated"].as_f64().unwrap();
    assert!(
        (integrated + 14.0).abs() <= 0.1,
        "the written file reaches the target: {m}"
    );
    assert!(
        m["true_peak"].as_f64().unwrap() <= -1.0,
        "the written file stays under the ceiling: {m}"
    );
}

#[cfg(unix)]
fn long_wav(name: &str) -> (PathBuf, PathBuf) {
    let out = dir(name);
    let input = out.join("long.wav");
    fs::write(&input, ramp_wav(20).0).unwrap();
    (out, input)
}

#[cfg(unix)]
fn raw_child(input: &Path) -> std::process::Child {
    pcod()
        .args([input.to_str().unwrap(), "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn raw_s16_ends_quietly_when_the_reader_hangs_up() {
    use std::io::Read;
    let (out, input) = long_wav("raw-s16-hang-up");
    let mut child = raw_child(&input);
    let mut some = vec![0u8; 16 << 10];
    child.stdout.take().unwrap().read_exact(&mut some).unwrap();
    assert_eq!(le(&some), ramp_wav(1).1[..8 << 10]);
    let done = child.wait_with_output().unwrap();
    assert_eq!(done.status.code(), Some(0), "{}", text(&done.stderr));
    assert!(done.stderr.is_empty(), "{}", text(&done.stderr));
    assert!(partials(&out).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn raw_s16_to_a_full_device_fails_in_one_line() {
    let (_, input) = long_wav("raw-s16-full");
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    let done = pcod()
        .args([input.to_str().unwrap(), "-"])
        .stdout(full)
        .output()
        .unwrap();
    let stderr = text(&done.stderr);
    assert_eq!(done.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_raw_s16_mid_stream() {
    use std::io::Read;
    let (out, input) = long_wav("raw-s16-ctrl-c");
    let mut child = raw_child(&input);
    let mut stdout = child.stdout.take().unwrap();
    let mut some = vec![0u8; 4 << 10];
    stdout.read_exact(&mut some).unwrap();
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = i32::try_from(child.id()).unwrap();
    assert_eq!(unsafe { kill(pid, 2) }, 0);
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).unwrap();
    let done = child.wait_with_output().unwrap();
    let stderr = text(&done.stderr);
    assert_eq!(done.status.code(), Some(130), "{stderr}");
    assert_eq!(stderr, "pcod: interrupted by SIGINT\n");
    assert!(
        some.len() + rest.len() < 20 * 48_000 * 4,
        "the whole stream came out"
    );
    assert!(partials(&out).is_empty());
}
