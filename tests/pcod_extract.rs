#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use pito_codecs::media::Rgba;

const FPS: u32 = 24;
const FRAMES: u32 = 240;
const WIDTH: u32 = 384;
const HEIGHT: u32 = 240;
const BLOCK: u32 = WIDTH / 8;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/pcod-extract")
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
    text(&out.stdout)
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn refused(out: &Output, words: &str) {
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(words), "{words:?} not in {stderr}");
    assert!(out.stdout.is_empty());
}

fn numbered(n: u32) -> Rgba {
    let mut img = Rgba::new(WIDTH, HEIGHT);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let bit = (i as u32 % WIDTH) / BLOCK;
        let v = if n >> bit & 1 == 1 { 255 } else { 0 };
        px.copy_from_slice(&[v, v, v, 255]);
    }
    img
}

fn number_of(img: &Rgba) -> u32 {
    let y = img.height / 2;
    let block = img.width / 8;
    (0..8).fold(0, |n, bit| {
        let x = bit * block + block / 2;
        let at = (y * img.width + x) as usize * 4;
        if img.data[at] > 128 { n | 1 << bit } else { n }
    })
}

fn decoded(path: &Path) -> Rgba {
    let data = fs::read(path).unwrap();
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => pito_codecs::png::decode(&data).unwrap(),
        Some("webp") => pito_codecs::webp::decode(&data).unwrap(),
        Some("jpg") => pito_codecs::jpeg::decode(&data).unwrap(),
        other => panic!("no decoder for {other:?}"),
    }
}

fn psnr(a: &Rgba, b: &Rgba) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let (mut sum, mut n) = (0.0, 0.0);
    for (x, y) in a.data.chunks_exact(4).zip(b.data.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(x[c]) - f64::from(y[c]);
            sum += d * d;
            n += 1.0;
        }
    }
    if sum == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (255.0 * 255.0 / (sum / n)).log10()
}

fn source() -> &'static Path {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        let root = root();
        fs::create_dir_all(&root).unwrap();
        let lock = fs::File::create(root.join("source.lock")).unwrap();
        lock.lock().unwrap();
        let video = root.join("ten.mp4");
        if video.exists() {
            return video;
        }
        let pid = std::process::id();
        let frames = root.join(format!("frames.{pid}"));
        let part = root.join(format!("ten.{pid}.mp4"));
        let _ = fs::remove_dir_all(&frames);
        fs::create_dir_all(&frames).unwrap();
        for n in 0..FRAMES {
            let png = pito_codecs::png::encode(&numbered(n)).unwrap();
            fs::write(frames.join(format!("{n:06}.png")), png).unwrap();
        }
        ok(&run(&[
            "frames",
            s(&frames),
            s(&part),
            "--fps",
            "24",
            "--route",
            "cpu",
            "--crf",
            "12",
        ]));
        fs::rename(&part, &video).unwrap();
        let _ = fs::remove_dir_all(&frames);
        video
    })
}

fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn extract(name: &str, extra: &[&str]) -> (PathBuf, Vec<u32>, String) {
    let work = dir(name);
    let out = work.join("frames");
    let mut args = vec![
        "stills",
        s(source()),
        "--height",
        "120",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ];
    args.extend_from_slice(extra);
    let json = ok(&run(&args));
    let names = listed(&out);
    for (i, name) in names.iter().enumerate() {
        assert!(name.starts_with(&format!("{i:06}.")), "{names:?}");
    }
    let got = names
        .iter()
        .map(|n| number_of(&decoded(&out.join(n))))
        .collect();
    (out, got, json)
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn number(json: &str, key: &str) -> f64 {
    field(json, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {json}"))
}

fn times(json: &str) -> Vec<f64> {
    let files = &json[json.find("\"files\":[").expect("files")..];
    files
        .split("\"time\":")
        .skip(1)
        .map(|t| {
            let end = t.find([',', '}']).unwrap();
            t[..end].parse().unwrap()
        })
        .collect()
}

fn frames(range: impl Iterator<Item = u32>) -> Vec<u32> {
    range.collect()
}

#[test]
fn every_two_seconds_over_the_whole_video() {
    let (_, got, json) = extract("every-2s", &["--every", "2s"]);
    assert_eq!(got, vec![0, 48, 96, 144, 192]);
    assert_eq!(number(&json, "count"), 5.0, "{json}");
    assert_eq!(number(&json, "every"), 2.0, "{json}");
    assert_eq!(field(&json, "rate"), Some("null"), "{json}");
    assert_eq!(field(&json, "format"), Some("png"), "{json}");
    assert_eq!(field(&json, "quality"), Some("null"), "{json}");
    let t = times(&json);
    assert_eq!(t.len(), 5, "{json}");
    for (i, at) in t.iter().enumerate() {
        assert!((at - 2.0 * i as f64).abs() < 1e-3, "{t:?}");
    }
    assert!(json.contains("000004.png"), "{json}");
}

#[test]
fn every_half_second_over_the_whole_video() {
    let (_, got, json) = extract("every-500ms", &["--every", "500ms"]);
    assert_eq!(got, frames((0..20).map(|k| k * 12)));
    assert_eq!(number(&json, "fps"), 2.0, "{json}");
}

#[test]
fn every_clock_second_over_a_window_that_runs_from_zero() {
    let (_, got, _) = extract("every-clock", &["--every", "0:01", "--to", "3"]);
    assert_eq!(got, vec![0, 24, 48]);
}

#[test]
fn three_per_second_over_the_whole_video() {
    let (_, got, json) = extract("fps-3", &["--fps", "3"]);
    assert_eq!(got, frames((0..30).map(|k| k * 8)));
    assert_eq!(field(&json, "rate"), Some("3/1"), "{json}");
    assert_eq!(field(&json, "every"), Some("null"), "{json}");
}

#[test]
fn every_frame_of_the_whole_video() {
    let (_, got, json) = extract("all", &["--from", "0"]);
    assert_eq!(got, frames(0..FRAMES));
    assert_eq!(number(&json, "count"), f64::from(FRAMES));
    let t = times(&json);
    assert_eq!(t.len(), FRAMES as usize);
    for (i, at) in t.iter().enumerate() {
        assert!((at - i as f64 / f64::from(FPS)).abs() < 1e-3, "{i}: {at}");
    }
}

#[test]
fn a_window_takes_every_interval_from_its_start() {
    let (_, got, _) = extract("win-2s", &["--from", "2.5", "--to", "6.5", "--every", "2s"]);
    assert_eq!(got, vec![60, 108]);
    let (_, got, _) = extract(
        "win-500ms",
        &["--from", "1", "--to", "2", "--every", "500ms"],
    );
    assert_eq!(got, vec![24, 36]);
}

#[test]
fn a_window_takes_three_per_second() {
    let (_, got, _) = extract("win-fps-3", &["--from", "1", "--to", "2", "--fps", "3"]);
    assert_eq!(got, vec![24, 32, 40]);
}

#[test]
fn a_window_to_the_end_takes_every_last_frame() {
    let (_, got, _) = extract("win-tail", &["--from", "9.5"]);
    assert_eq!(got, frames(228..FRAMES));
}

#[test]
fn an_interval_longer_than_the_video_takes_its_first_frame() {
    let (_, got, _) = extract("every-20s", &["--every", "20s"]);
    assert_eq!(got, vec![0]);
}

#[test]
fn webp_frames_open_with_the_right_numbers() {
    let (out, got, json) = extract(
        "webp",
        &["--every", "2s", "--format", "webp", "--quality", "90"],
    );
    assert_eq!(got, vec![0, 48, 96, 144, 192]);
    assert_eq!(
        listed(&out),
        (0..5).map(|i| format!("{i:06}.webp")).collect::<Vec<_>>()
    );
    assert_eq!(field(&json, "format"), Some("webp"), "{json}");
    assert_eq!(number(&json, "quality"), 90.0, "{json}");
    let img = decoded(&out.join("000000.webp"));
    assert_eq!((img.width, img.height), (192, 120));
}

#[test]
fn a_lower_webp_quality_writes_smaller_files() {
    let (low, _, _) = extract(
        "webp-low",
        &["--every", "5s", "--format", "webp", "--quality", "5"],
    );
    let (high, _, _) = extract(
        "webp-high",
        &["--every", "5s", "--format", "webp", "--quality", "100"],
    );
    let size = |d: &Path| fs::metadata(d.join("000001.webp")).unwrap().len();
    assert!(
        size(&low) < size(&high),
        "{} vs {}",
        size(&low),
        size(&high)
    );
}

#[test]
fn jpeg_frames_open_with_the_right_numbers_close_to_the_png_frames() {
    let (png, _, _) = extract("jpeg-ref", &["--every", "2s"]);
    for format in ["jpeg", "jpg"] {
        let (out, got, json) = extract(
            &format!("jpeg-{format}"),
            &["--every", "2s", "--format", format, "--quality", "80"],
        );
        assert_eq!(got, vec![0, 48, 96, 144, 192]);
        assert_eq!(
            listed(&out),
            (0..5).map(|i| format!("{i:06}.jpg")).collect::<Vec<_>>()
        );
        assert_eq!(field(&json, "format"), Some("jpeg"), "{json}");
        assert_eq!(number(&json, "quality"), 80.0, "{json}");
        for i in 0..5 {
            let name = format!("{i:06}");
            let ours = decoded(&out.join(format!("{name}.jpg")));
            let lossless = decoded(&png.join(format!("{name}.png")));
            assert_eq!((ours.width, ours.height), (192, 120));
            let db = psnr(&ours, &lossless);
            assert!(db > 30.0, "{name}: {db:.2} dB");
        }
    }
}

#[test]
fn a_lower_jpeg_quality_writes_smaller_files() {
    let (low, _, _) = extract(
        "jpeg-low",
        &["--every", "5s", "--format", "jpeg", "--quality", "5"],
    );
    let (high, _, _) = extract(
        "jpeg-high",
        &["--every", "5s", "--format", "jpeg", "--quality", "100"],
    );
    let size = |d: &Path| fs::metadata(d.join("000001.jpg")).unwrap().len();
    assert!(
        size(&low) < size(&high),
        "{} vs {}",
        size(&low),
        size(&high)
    );
}

#[test]
fn png_says_once_that_it_ignores_quality() {
    let work = dir("png-quality");
    let out = work.join("frames");
    let done = run(&[
        "stills",
        s(source()),
        "--every",
        "5s",
        "--quality",
        "50",
        "--route",
        "cpu",
        "--out",
        s(&out),
    ]);
    ok(&done);
    let stderr = text(&done.stderr);
    assert_eq!(stderr, "pcod: png is lossless, so --quality is ignored\n");
    assert_eq!(listed(&out), vec!["000000.png", "000001.png"]);
}

#[test]
fn stills_at_times_take_the_format_and_the_sheet_stays_png() {
    let work = dir("at-webp");
    let out = work.join("stills");
    let sheet = work.join("sheet.png");
    ok(&run(&[
        "stills",
        s(source()),
        "--at",
        "1,2",
        "--height",
        "240",
        "--format",
        "webp",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--sheet",
        s(&sheet),
    ]));
    assert_eq!(listed(&out), vec!["still-001.webp", "still-002.webp"]);
    assert_eq!(number_of(&decoded(&out.join("still-001.webp"))), 24);
    assert_eq!(number_of(&decoded(&out.join("still-002.webp"))), 48);
    assert!(fs::read(&sheet).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn a_webp_run_replaces_numbered_png_frames_only_with_force() {
    let work = dir("webp-force");
    let out = work.join("frames");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("000000.png"), b"stale").unwrap();
    let args = |force: bool| {
        let mut a = vec![
            "stills",
            s(source()),
            "--every",
            "5s",
            "--format",
            "webp",
            "--route",
            "cpu",
            "--out",
            s(&out),
        ];
        if force {
            a.push("--force");
        }
        run(&a)
    };
    refused(&args(false), "already holds numbered frames");
    ok(&args(true));
    assert_eq!(listed(&out), vec!["000000.webp", "000001.webp"]);
}

#[test]
fn a_cadence_past_the_end_is_refused_in_one_line() {
    let work = dir("past-end");
    refused(
        &run(&[
            "stills",
            s(source()),
            "--from",
            "30",
            "--every",
            "2s",
            "--out",
            s(&work.join("f")),
        ]),
        "past the end",
    );
    refused(
        &run(&[
            "stills",
            s(source()),
            "--from",
            "5",
            "--to",
            "4",
            "--fps",
            "3",
            "--out",
            s(&work.join("g")),
        ]),
        "must come after",
    );
}

#[test]
fn bad_cadence_and_quality_are_usage_errors_like_clap() {
    let cases: &[(&[&str], &str)] = &[
        (
            &["--every", "0", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "0ms", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "-1", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "-2s", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "soon", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "2x", "--out", "d"],
            "is not an interval above zero",
        ),
        (
            &["--every", "2s", "--fps", "3", "--out", "d"],
            "cannot be used with",
        ),
        (
            &["--fps", "3", "--every", "2s", "--out", "d"],
            "cannot be used with",
        ),
        (
            &["--every", "2s", "--count", "3", "--out", "d"],
            "cannot be used with",
        ),
        (
            &["--every", "2s", "--at", "1", "--out", "d"],
            "cannot be used with",
        ),
        (
            &["--every", "2s", "--sheet", "s.png"],
            "cannot be used with",
        ),
        (
            &["--fps", "3", "--seek", "keyframe", "--out", "d"],
            "cannot be used with",
        ),
        (&["--every", "2s"], "required arguments were not provided"),
        (&["--quality", "0", "--out", "d"], "0 is not in 1..=100"),
        (&["--quality", "101", "--out", "d"], "101 is not in 1..=100"),
        (&["--quality", "high", "--out", "d"], "invalid value"),
        (
            &["--quality", "50", "--sheet", "s.png"],
            "required arguments were not provided",
        ),
        (&["--format", "gif", "--out", "d"], "invalid value 'gif'"),
        (
            &["--size", "640", "--out", "d"],
            "required arguments were not provided",
        ),
    ];
    for (extra, words) in cases {
        let mut args = vec!["stills", "v.mp4"];
        args.extend_from_slice(extra);
        let out = run(&args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        let first = stderr.lines().next().unwrap_or("");
        assert!(first.starts_with("error: "), "{args:?}: {stderr}");
        assert!(
            stderr.contains(words),
            "{args:?}: {words:?} not in {stderr}"
        );
        assert!(
            stderr.ends_with("\n\nFor more information, try '--help'.\n"),
            "{args:?}: {stderr}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn a_window_ending_on_a_slot_leaves_that_slot_out() {
    let (_, got, json) = extract("win-edge", &["--from", "2", "--to", "6", "--every", "2s"]);
    assert_eq!(got, vec![48, 96]);
    assert_eq!(number(&json, "count"), 2.0, "{json}");
}

#[test]
fn every_with_a_size_scales_each_frame_to_it() {
    let work = dir("every-size");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(source()),
        "--every",
        "5s",
        "--size",
        "96x60",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ]));
    assert_eq!(number(&json, "count"), 2.0, "{json}");
    let names = listed(&out);
    assert_eq!(names, vec!["000000.png", "000001.png"]);
    let got: Vec<u32> = names
        .iter()
        .map(|n| {
            let img = decoded(&out.join(n));
            assert_eq!((img.width, img.height), (96, 60));
            number_of(&img)
        })
        .collect();
    assert_eq!(got, vec![0, 120]);
}

#[test]
fn the_plain_line_names_the_count_format_and_interval() {
    let work = dir("plain-line");
    let out = work.join("frames");
    let line = ok(&run(&[
        "stills",
        s(source()),
        "--every",
        "2s",
        "--height",
        "120",
        "--route",
        "cpu",
        "--out",
        s(&out),
    ]));
    let first = line.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("5 png frames, 192x120 one every 2.000 s, 0.000 s to 8.000 s, in "),
        "{line}"
    );
    assert_eq!(line.lines().count(), 2, "{line}");
}

#[test]
fn alpha_webm_frames_keep_their_alpha_and_jpeg_flattens_onto_black() {
    let work = dir("alpha-webm");
    let frames = work.join("frames");
    fs::create_dir_all(&frames).unwrap();
    for n in 0..20u32 {
        let mut img = Rgba::new(352, 240);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let x = i as u32 % 352;
            px.copy_from_slice(&[230, 230, 230, if x < 176 { 0 } else { 255 }]);
        }
        let png = pito_codecs::png::encode_rgba(&img).unwrap();
        fs::write(frames.join(format!("{n:04}.png")), png).unwrap();
    }
    let webm = work.join("clip.webm");
    ok(&run(&[s(&frames), s(&webm), "--fps", "10"]));
    let take = |format: &str| {
        let out = work.join(format);
        ok(&run(&[
            "stills",
            s(&webm),
            "--every",
            "1s",
            "--size",
            "352x240",
            "--format",
            format,
            "--route",
            "cpu",
            "--out",
            s(&out),
        ]));
        let names = listed(&out);
        assert_eq!(names.len(), 2, "{names:?}");
        out.join(&names[1])
    };
    let at = |img: &Rgba, x: u32| {
        let i = ((img.height / 2 * img.width + x) * 4) as usize;
        [
            img.data[i],
            img.data[i + 1],
            img.data[i + 2],
            img.data[i + 3],
        ]
    };
    for format in ["png", "webp"] {
        let img = decoded(&take(format));
        assert_eq!(
            at(&img, 40)[3],
            0,
            "{format}: the see-through half stays see-through"
        );
        assert!(at(&img, 300)[3] > 250, "{format}");
        assert!(at(&img, 300)[0] > 200, "{format}");
    }
    let jpeg = take("jpeg");
    assert_eq!(&fs::read(&jpeg).unwrap()[..2], &[0xff, 0xd8]);
    let img = decoded(&jpeg);
    let left = at(&img, 40);
    assert!(
        left[..3].iter().all(|&c| c < 12),
        "flattened onto black: {left:?}"
    );
    let right = at(&img, 300);
    assert!(right[..3].iter().all(|&c| c > 200), "{right:?}");
}
