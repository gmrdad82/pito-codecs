#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::{Prefs, Rgba, probe};

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
