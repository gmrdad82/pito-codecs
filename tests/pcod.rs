#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

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

fn frames() -> &'static Path {
    static FRAMES: OnceLock<PathBuf> = OnceLock::new();
    FRAMES.get_or_init(|| {
        let folder = dir("frames");
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
    })
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
    let folder = frames().to_str().unwrap().to_string();
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
    let input = frames().to_str().unwrap().to_string();
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
    let input = frames().to_str().unwrap().to_string();
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
    let input = frames().to_str().unwrap().to_string();
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
