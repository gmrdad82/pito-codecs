#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::{Prefs, measure_loudness};
use pito_codecs::mkv;
use pito_codecs::mp4::Reader;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-audio-copy")
        .join(name);
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
    text(&out.stdout)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn source(dir: &Path, name: &str, seconds: &str) -> PathBuf {
    let path = dir.join(name);
    ok(&run(&[
        "synth",
        "--size",
        "128x96",
        "--fps",
        "24",
        "--duration",
        seconds,
        "--audio",
        "sine:440",
        s(&path),
    ]));
    path
}

fn frames(dir: &Path, seconds: &str) -> PathBuf {
    let folder = dir.join("frames");
    let video = source(dir, "picture.mp4", seconds);
    ok(&run(&[
        "stills",
        s(&video),
        "--from",
        "0",
        "--to",
        seconds,
        "--out",
        s(&folder),
    ]));
    folder
}

fn render(folder: &Path, out: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["frames", s(folder), s(out), "--fps", "24", "--route", "cpu"];
    args.extend_from_slice(extra);
    run(&args)
}

fn mp4_audio(path: &Path) -> Vec<Vec<u8>> {
    let r = Reader::open(path).unwrap();
    let a = r.audio()[0];
    (0..r.tracks[a].samples.len())
        .map(|i| {
            let mut d = Vec::new();
            r.read_into(a, i, &mut d).unwrap();
            d
        })
        .collect()
}

fn mkv_audio(path: &Path) -> Vec<Vec<u8>> {
    let r = mkv::Reader::open(path).unwrap();
    let a = r.audio()[0];
    (0..r.tracks[a].samples.len())
        .map(|i| r.read(a, i).unwrap())
        .collect()
}

fn loud(path: &Path) -> (f64, f64) {
    let m = measure_loudness(path, 0, &Prefs::default(), |_| true).unwrap();
    (m.integrated, m.true_peak)
}

#[test]
fn the_audio_packets_are_the_sources_own_bytes() {
    let d = dir("same");
    let folder = frames(&d, "2");
    let src = source(&d, "film.mp4", "2");
    let out = d.join("out.mp4");
    let said = ok(&render(&folder, &out, &["--audio-copy", s(&src)]));
    assert!(said.contains("copied as it is, no re-encode"), "{said}");
    let (a, b) = (mp4_audio(&src), mp4_audio(&out));
    assert!(!a.is_empty());
    assert_eq!(a.len(), b.len());
    assert_eq!(a, b);
    assert_eq!(loud(&src), loud(&out));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_longer_track_is_cut_to_the_videos_length() {
    let d = dir("cut");
    let folder = frames(&d, "1");
    let src = source(&d, "film.mp4", "3");
    let out = d.join("out.mp4");
    ok(&render(&folder, &out, &["--audio-copy", s(&src)]));
    let (a, b) = (mp4_audio(&src), mp4_audio(&out));
    assert!(b.len() < a.len());
    assert!(b.len() >= 46);
    assert_eq!(a[..b.len()], b[..]);
    let r = Reader::open(&out).unwrap();
    let t = &r.tracks[r.audio()[0]];
    assert!(t.seconds(t.samples.last().unwrap().pts + t.edit_offset) < 1.0);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_shorter_track_ends_and_the_video_runs_on() {
    let d = dir("pad");
    let folder = frames(&d, "2");
    let src = source(&d, "film.mp4", "1");
    let out = d.join("out.mp4");
    let said = ok(&render(&folder, &out, &["--audio-copy", s(&src)]));
    assert!(said.contains("note    the audio ends at"), "{said}");
    assert_eq!(mp4_audio(&src), mp4_audio(&out));
    let r = Reader::open(&out).unwrap();
    let v = &r.tracks[r.video().unwrap()];
    assert_eq!(v.samples.len(), 48);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn matroska_aac_is_copied_too() {
    let d = dir("mkv");
    let folder = frames(&d, "2");
    let src = source(&d, "film.mkv", "2");
    let out = d.join("out.mp4");
    ok(&render(&folder, &out, &["--audio-copy", s(&src)]));
    let (a, b) = (mkv_audio(&src), mp4_audio(&out));
    assert!(!a.is_empty());
    assert_eq!(a[..b.len()], b[..]);
    assert!(b.len() + 2 >= a.len());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_refusals_say_why() {
    let d = dir("refuse");
    let folder = frames(&d, "1");
    let src = source(&d, "film.mp4", "1");
    let wav = d.join("a.wav");
    let out = d.join("out.mp4");
    let both = render(
        &folder,
        &out,
        &["--audio-copy", s(&src), "--audio", s(&wav)],
    );
    assert_ne!(both.status.code(), Some(0));
    assert!(text(&both.stderr).contains("cannot be used with '--audio"));
    let leveled = render(
        &folder,
        &out,
        &["--audio-copy", s(&src), "--loudness", "youtube"],
    );
    assert_ne!(leveled.status.code(), Some(0));
    assert!(text(&leveled.stderr).contains("cannot be used with '--loudness"));
    let opus = source(&d, "film.webm", "1");
    let refused = render(&folder, &out, &["--audio-copy", s(&opus)]);
    assert_eq!(refused.status.code(), Some(1));
    let e = text(&refused.stderr);
    assert!(
        e.contains("--audio-copy copies opus as it is, and .mp4 doesn't carry opus"),
        "{e}"
    );
    assert_eq!(e.trim().lines().count(), 1, "{e}");
    let silent = d.join("silent.mp4");
    ok(&run(&[
        "synth",
        "--size",
        "128x96",
        "--fps",
        "24",
        "--duration",
        "1",
        s(&silent),
    ]));
    let none = render(&folder, &out, &["--audio-copy", s(&silent)]);
    assert_eq!(none.status.code(), Some(1));
    assert!(text(&none.stderr).contains("no audio track to copy"));
    assert!(!out.exists());
    let _ = fs::remove_dir_all(&d);
}
