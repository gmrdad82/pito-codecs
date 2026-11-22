#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::mp4::Reader;
use pito_codecs::png;

const W: u32 = 256;
const H: u32 = 144;
const FRAMES: usize = 48;
const SOLID: &str = "color=white,track=black,opacity=1,thickness=8";

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-progress")
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

fn ok(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn source(d: &Path) -> PathBuf {
    let path = d.join("source.mp4");
    ok(&run(&[
        "synth",
        "--size",
        &format!("{W}x{H}"),
        "--fps",
        "24",
        "--duration",
        "2",
        "--audio",
        "sine:440",
        s(&path),
    ]));
    path
}

fn shots(d: &Path, video: &Path) -> PathBuf {
    let out = d.join("shots");
    ok(&run(&[
        "stills",
        s(video),
        "--out",
        s(&out),
        "--from",
        "0",
        "--to",
        "100",
        "--height",
        "144",
        "--force",
    ]));
    out
}

fn lit(img: &pito_codecs::media::Rgba, x: u32, y: u32) -> bool {
    img.data[((y * img.width + x) * 4) as usize] > 128
}

fn load(folder: &Path, n: usize) -> pito_codecs::media::Rgba {
    png::decode(&fs::read(folder.join(format!("{n:06}.png"))).unwrap()).unwrap()
}

fn filled(img: &pito_codecs::media::Rgba, spec: &str) -> u32 {
    let probe = |x: u32, y: u32| u32::from(lit(img, x, y));
    match spec {
        "bar,position=top" => (0..W).map(|x| probe(x, 3)).sum(),
        "bar,position=bottom" => (0..W).map(|x| probe(x, H - 4)).sum(),
        "bar,position=left" => (0..H).map(|y| probe(3, y)).sum(),
        "bar,position=right" => (0..H).map(|y| probe(W - 4, y)).sum(),
        _ => {
            let ring = |x: u32, y: u32| x < 8 || y < 8 || x >= W - 8 || y >= H - 8;
            (0..H)
                .flat_map(|y| (0..W).map(move |x| (x, y)))
                .filter(|&(x, y)| ring(x, y) && lit(img, x, y))
                .count() as u32
        }
    }
}

fn length_of(spec: &str) -> f64 {
    match spec {
        "bar,position=top" | "bar,position=bottom" => f64::from(W),
        "bar,position=left" | "bar,position=right" => f64::from(H),
        _ => f64::from(2 * (W - 8) + 2 * (H - 8)),
    }
}

fn expected(spec: &str, n: usize) -> f64 {
    let whole = length_of(spec);
    let unit = if spec.starts_with("bar") { 1.0 } else { 8.0 };
    whole * unit * n as f64 / (FRAMES - 1) as f64
}

#[test]
fn every_mode_and_position_fills_linearly_from_empty_to_full() {
    let d = dir("fill");
    let src = source(&d);
    for spec in [
        "bar,position=top",
        "bar,position=bottom",
        "bar,position=left",
        "bar,position=right",
        "contour,start=tl",
        "contour,start=br,direction=ccw",
    ] {
        let out = d.join("out.mp4");
        let _ = fs::remove_file(&out);
        ok(&run(&[
            s(&src),
            s(&out),
            "--progress",
            &format!("{spec},{SOLID}"),
            "--no-progress",
        ]));
        let folder = shots(&d, &out);
        let key = spec.split(",direction").next().unwrap();
        let key = if key.starts_with("contour") {
            "contour"
        } else {
            key
        };
        let probe = if key == "contour" { "contour" } else { spec };
        let count = |n| f64::from(filled(&load(&folder, n), probe));
        let (first, mid, last) = (count(0), count(FRAMES / 2), count(FRAMES - 1));
        let full = expected(probe, FRAMES - 1);
        assert_eq!(first, 0.0, "{spec}: frame 0");
        let slack = if key == "contour" { 40.0 } else { 3.0 };
        assert!(
            (mid - expected(probe, FRAMES / 2)).abs() <= slack,
            "{spec}: midpoint {mid}, expected {}",
            expected(probe, FRAMES / 2)
        );
        assert!(
            (last - full).abs() <= slack,
            "{spec}: last frame {last}, expected {full}"
        );
        let quarter = count(FRAMES / 4);
        assert!(quarter > first && quarter < mid, "{spec}: grows");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_contour_starts_at_its_corner() {
    let d = dir("corner");
    let src = source(&d);
    for (spec, near) in [
        ("contour,start=tl", (4, 4)),
        ("contour,start=tr", (W - 5, 4)),
        ("contour,start=br", (W - 5, H - 5)),
        ("contour,start=bl,direction=ccw", (4, H - 5)),
    ] {
        let out = d.join("out.mp4");
        let _ = fs::remove_file(&out);
        ok(&run(&[
            s(&src),
            s(&out),
            "--progress",
            &format!("{spec},{SOLID}"),
            "--no-progress",
        ]));
        let folder = shots(&d, &out);
        let early = load(&folder, 3);
        assert!(lit(&early, near.0, near.1), "{spec}: its corner is filled");
        let far = (W - 1 - near.0, H - 1 - near.1);
        assert!(
            !lit(&early, far.0, far.1),
            "{spec}: the opposite corner waits"
        );
    }
    let _ = fs::remove_dir_all(&d);
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

#[test]
fn adding_only_the_bar_leaves_the_audio_packets_byte_identical() {
    let d = dir("audio");
    let src = source(&d);
    for extra in [&[][..], &["--profile", "fast"][..]] {
        let out = d.join("out.mp4");
        let _ = fs::remove_file(&out);
        let mut args = vec![s(&src), s(&out), "--progress", "bar", "--no-progress"];
        args.extend_from_slice(extra);
        ok(&run(&args));
        let (a, b) = (mp4_audio(&src), mp4_audio(&out));
        assert!(!a.is_empty());
        assert_eq!(a, b, "{extra:?}");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_gpu_profile_draws_the_same_bar() {
    let d = dir("fast");
    let src = source(&d);
    let out = d.join("out.mp4");
    ok(&run(&[
        s(&src),
        s(&out),
        "--progress",
        &format!("bar,position=top,{SOLID}"),
        "--profile",
        "fast",
        "--no-progress",
    ]));
    let folder = shots(&d, &out);
    let count = |n| f64::from(filled(&load(&folder, n), "bar,position=top"));
    assert_eq!(count(0), 0.0);
    assert!((count(FRAMES - 1) - f64::from(W)).abs() <= 3.0);
    assert!((count(FRAMES / 2) - expected("bar,position=top", FRAMES / 2)).abs() <= 3.0);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_cut_window_fills_over_its_own_length() {
    let d = dir("window");
    let src = source(&d);
    let out = d.join("out.mp4");
    ok(&run(&[
        s(&src),
        s(&out),
        "--progress",
        &format!("bar,position=top,{SOLID}"),
        "--from",
        "0.5",
        "--to",
        "1.5",
        "--no-progress",
    ]));
    let folder = shots(&d, &out);
    assert_eq!(filled(&load(&folder, 0), "bar,position=top"), 0);
    assert!(filled(&load(&folder, 23), "bar,position=top").abs_diff(W) <= 3);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn frames_draws_the_bar_too() {
    let d = dir("frames");
    let src = source(&d);
    let folder = d.join("png");
    ok(&run(&[
        "stills",
        s(&src),
        "--out",
        s(&folder),
        "--from",
        "0",
        "--to",
        "100",
        "--height",
        "144",
        "--force",
    ]));
    let out = d.join("out.mp4");
    ok(&run(&[
        "frames",
        s(&folder),
        s(&out),
        "--fps",
        "24",
        "--progress",
        &format!("bar,position=bottom,{SOLID}"),
    ]));
    let shots = shots(&d, &out);
    assert_eq!(filled(&load(&shots, 0), "bar,position=bottom"), 0);
    assert!(filled(&load(&shots, FRAMES - 1), "bar,position=bottom").abs_diff(W) <= 3);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_bar_is_refused_where_it_cannot_be_drawn() {
    let d = dir("refuse");
    let src = source(&d);
    for (args, part) in [
        (vec!["x.gif", "--progress", "bar"], "--progress draws on"),
        (
            vec!["x.wav", "--progress", "bar"],
            "--progress doesn't apply",
        ),
        (vec!["x.mp4", "--progress", "ring"], "bar or contour"),
        (vec!["x.mp4", "--progress", "bar,start=tl"], "contour only"),
    ] {
        let out = d.join(args[0]);
        let mut full = vec![s(&src), s(&out)];
        full.extend_from_slice(&args[1..]);
        let o = run(&full);
        assert!(!o.status.success(), "{args:?}");
        let e = String::from_utf8_lossy(&o.stderr);
        assert!(e.contains(part), "{args:?}: {e}");
        assert!(!out.exists());
    }
    let _ = fs::remove_dir_all(&d);
}
