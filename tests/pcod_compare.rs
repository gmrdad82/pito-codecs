#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::Rgba;
use pito_codecs::png;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-compare-test")
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

fn ok(args: &[&str]) -> String {
    let out = run(args);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    text(&out.stdout)
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn clip(folder: &Path, name: &str, size: &str, seconds: &str) -> PathBuf {
    let file = folder.join(name);
    ok(&[
        "synth",
        path(&file),
        "--size",
        size,
        "--fps",
        "10",
        "--duration",
        seconds,
    ]);
    file
}

fn frames_of(folder: &Path, video: &Path, size: &str) -> PathBuf {
    let out = folder.join("frames");
    ok(&[
        "stills",
        path(video),
        "--from",
        "0",
        "--out",
        path(&out),
        "--size",
        size,
    ]);
    out
}

fn flat(folder: &Path, count: usize, value: u8) {
    fs::create_dir_all(folder).unwrap();
    for i in 0..count {
        let mut image = Rgba::new(16, 12);
        for px in image.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[value, value, value, 255]);
        }
        fs::write(
            folder.join(format!("{i:06}.png")),
            png::encode(&image).unwrap(),
        )
        .unwrap();
    }
}

fn number(json: &str, key: &str) -> f64 {
    let at = json.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
    let rest = &json[at..];
    rest[..rest.find([',', '}', ']']).unwrap()]
        .parse()
        .unwrap_or(f64::NAN)
}

#[test]
fn two_identical_clips_read_identical() {
    let d = dir("same");
    let a = clip(&d, "a.mp4", "128x96", "2");
    let out = ok(&["compare", path(&a), path(&a), "--worst", "3"]);
    assert!(out.contains("PSNR             identical"), "{out}");
    assert!(out.contains("frames compared  20"), "{out}");
    assert!(out.contains("identical frames 20"), "{out}");
    let json = ok(&["compare", path(&a), path(&a), "--json"]);
    assert!(json.contains("\"identical\":true"), "{json}");
    assert!(json.contains("\"psnr_mean\":null"), "{json}");
    assert!(json.contains("\"worst\":[]"), "{json}");
}

#[test]
fn a_changed_frame_is_the_worst_and_its_diff_is_written() {
    let d = dir("one");
    let a = clip(&d, "a.mp4", "128x96", "2");
    let frames = frames_of(&d, &a, "128x96");
    let target = frames.join("000007.png");
    let mut image = png::decode(&fs::read(&target).unwrap()).unwrap();
    for px in image.data.chunks_exact_mut(4).take(500) {
        px[0] = px[0].wrapping_add(100);
    }
    fs::write(&target, png::encode(&image).unwrap()).unwrap();
    let diffs = d.join("diffs");
    let json = ok(&[
        "compare",
        path(&a),
        path(&frames),
        "--fps",
        "10",
        "--worst",
        "2",
        "--diff",
        path(&diffs),
        "--json",
    ]);
    assert_eq!(number(&json, "identical_frames"), 19.0, "{json}");
    assert_eq!(number(&json, "worst_frame"), 7.0, "{json}");
    assert!((number(&json, "worst_time") - 0.7).abs() < 1e-9, "{json}");
    let written = fs::read(diffs.join("diff-000007.png")).unwrap();
    let diff = png::decode(&written).unwrap();
    assert_eq!((diff.width, diff.height), (128, 96));
    let lit = diff.data.chunks_exact(4).filter(|p| p[0] > 0).count();
    assert_eq!(lit, 500);
    assert!(
        diff.data
            .chunks_exact(4)
            .skip(500)
            .all(|p| p[..3] == [0, 0, 0])
    );
    assert_eq!(fs::read_dir(&diffs).unwrap().count(), 1);
    let text = ok(&[
        "compare",
        path(&a),
        path(&frames),
        "--fps",
        "10",
        "--worst",
        "1",
    ]);
    assert!(text.contains("frame 7 at 0.700 s"), "{text}");
}

#[test]
fn a_known_offset_gives_the_expected_psnr() {
    let d = dir("offset");
    flat(&d.join("a"), 4, 100);
    flat(&d.join("b"), 4, 105);
    let (a, b) = (d.join("a"), d.join("b"));
    let json = ok(&["compare", path(&a), path(&b), "--json"]);
    let expected = 10.0 * (255.0f64 * 255.0 / 25.0).log10();
    assert!(
        (number(&json, "psnr_mean") - expected).abs() < 1e-9,
        "{json}"
    );
    assert!(
        (number(&json, "psnr_worst") - expected).abs() < 1e-9,
        "{json}"
    );
    assert_eq!(number(&json, "beyond_tolerance"), 1.0, "{json}");
    assert!(json.contains("\"mean_abs_diff\":[5.0,5.0,5.0]"), "{json}");
    assert!(json.contains("\"max_abs_diff\":[5,5,5]"), "{json}");
    let loose = ok(&["compare", path(&a), path(&b), "--tolerance", "5", "--json"]);
    assert_eq!(number(&loose, "beyond_tolerance"), 0.0, "{loose}");
}

#[test]
fn a_folder_against_its_video_reads_identical_and_a_shorter_run_is_said() {
    let d = dir("folder");
    let a = clip(&d, "a.mp4", "128x96", "2");
    let frames = frames_of(&d, &a, "128x96");
    let out = ok(&["compare", path(&frames), path(&a), "--fps", "10"]);
    assert!(out.contains("PSNR             identical"), "{out}");
    fs::remove_file(frames.join("000019.png")).unwrap();
    fs::remove_file(frames.join("000018.png")).unwrap();
    let short = ok(&["compare", path(&a), path(&frames), "--fps", "10"]);
    assert!(
        short.contains("frames compared  18 (a has 20, b has 18"),
        "{short}"
    );
    let json = ok(&["compare", path(&frames), path(&a), "--fps", "10", "--json"]);
    assert_eq!(number(&json, "frames_a"), 18.0, "{json}");
    assert_eq!(number(&json, "frames_b"), 20.0, "{json}");
    assert_eq!(number(&json, "compared"), 18.0, "{json}");
}

#[test]
fn a_window_compares_the_same_frames_of_both_sides() {
    let d = dir("window");
    let a = clip(&d, "a.mp4", "128x96", "2");
    let frames = frames_of(&d, &a, "128x96");
    let json = ok(&[
        "compare",
        path(&a),
        path(&frames),
        "--fps",
        "10",
        "--from",
        "0.5",
        "--to",
        "1",
        "--json",
    ]);
    assert_eq!(number(&json, "compared"), 5.0, "{json}");
    assert_eq!(number(&json, "identical_frames"), 5.0, "{json}");
}

#[test]
fn a_size_mismatch_is_refused_naming_both_sizes() {
    let d = dir("size");
    let a = clip(&d, "a.mp4", "128x96", "1");
    let b = clip(&d, "b.mp4", "160x96", "1");
    let out = run(&["compare", path(&a), path(&b)]);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.starts_with("pcod: "), "{err}");
    assert!(err.contains("128x96") && err.contains("160x96"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn a_wrong_command_line_exits_2() {
    let d = dir("usage");
    let a = clip(&d, "a.mp4", "128x96", "1");
    let out = run(&["compare", path(&a), path(&a), "--diff", "x"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let out = run(&["compare", path(&a)]);
    assert_eq!(out.status.code(), Some(2));
}
