#![cfg(feature = "cli")]

#[path = "../src/testref.rs"]
mod testref;

#[allow(dead_code)]
#[path = "../src/pcod/refs.rs"]
mod refs;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::gif::{EncodeOptions, encode};
use pito_codecs::media::{Prefs, Rgba, probe};
use refs::numbered_at;
use testref::Refs;

const WIDTH: u32 = refs::GIF_SIZE.0;
const HEIGHT: u32 = refs::GIF_SIZE.1;
const DELAYS: [u16; 6] = refs::GIF_DELAYS;
const SHOWN: [u64; 6] = [7, 1, 13, 4, 25, 3];
const CLEAR: usize = 3;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-gif")
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

fn numbered(n: u32, clear: bool) -> Rgba {
    numbered_at(WIDTH, HEIGHT, n, clear)
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

fn opts() -> EncodeOptions {
    refs::gif_options()
}

fn frames() -> Vec<Rgba> {
    (0..DELAYS.len())
        .map(|i| numbered(i as u32 + 1, i == CLEAR))
        .collect()
}

fn gif_in(dir: &Path) -> PathBuf {
    let path = dir.join("in.gif");
    fs::write(&path, encode(&frames(), &DELAYS, opts()).unwrap()).unwrap();
    path
}

fn rgb(img: &Rgba) -> Vec<u8> {
    img.data
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

fn expected(rate: u64) -> Vec<usize> {
    let slot = |cs: u64| (2 * cs * rate + 100) / 200;
    let starts: Vec<u64> = SHOWN
        .iter()
        .scan(0, |at, &d| {
            let s = *at;
            *at += d;
            Some(s)
        })
        .collect();
    let ticks = slot(SHOWN.iter().sum()).max(1);
    (0..ticks)
        .map(|k| starts.iter().rposition(|&s| slot(s) <= k).unwrap())
        .collect()
}

fn pngs(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    found.sort();
    found
}

fn stills_match(rate: u64) {
    let d = dir(&format!("stills-{rate}"));
    let gif = gif_in(&d);
    let out = d.join("out");
    ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        &rate.to_string(),
        "--out",
        s(&out),
    ]));
    let want = expected(rate);
    let got = pngs(&out);
    assert_eq!(got.len(), want.len());
    let source = frames();
    for (k, (path, &i)) in got.iter().zip(&want).enumerate() {
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("{k:06}.png")
        );
        let img = pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (WIDTH, HEIGHT));
        assert!(rgb(&img) == rgb(&source[i]), "tick {k} is not frame {i}");
    }
}

#[test]
fn the_tick_model_follows_the_fps_filter() {
    assert_eq!(expected(10), vec![0, 2, 3, 4, 4]);
    assert_eq!(expected(30).len(), 16);
    assert!(expected(30).contains(&CLEAR));
    assert_eq!(expected(100).len(), 53);
}

#[test]
fn a_gif_probes_as_one_video_track() {
    let d = dir("probe");
    let p = probe(&gif_in(&d), &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height), (WIDTH, HEIGHT));
    assert_eq!(p.frames, Some(6));
    assert!((p.duration - 0.53).abs() < 1e-9, "{}", p.duration);
    assert_eq!(p.fps, (11321, 1000));
    assert_eq!(p.video_codec_name, "gif");
    assert!(p.audio.is_empty());
}

#[test]
fn stills_at_10_fps_take_the_frame_held_at_each_tick() {
    stills_match(10);
}

#[test]
fn stills_at_30_fps_take_the_frame_held_at_each_tick() {
    stills_match(30);
}

#[test]
fn a_one_frame_gif_with_no_delay_holds_for_the_default_delay() {
    let d = dir("one");
    let gif = d.join("one.gif");
    let frame = numbered(5, false);
    fs::write(
        &gif,
        encode(std::slice::from_ref(&frame), &[0], opts()).unwrap(),
    )
    .unwrap();
    let p = probe(&gif, &Prefs::default()).unwrap();
    assert_eq!(p.frames, Some(1));
    assert!((p.duration - 0.1).abs() < 1e-9);
    let out = d.join("out");
    ok(&run(&["stills", s(&gif), "--fps", "30", "--out", s(&out)]));
    let got = pngs(&out);
    assert_eq!(got.len(), 3);
    for path in got {
        let img = pito_codecs::png::decode(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(rgb(&img), rgb(&frame));
    }
}

#[test]
fn a_gif_encodes_to_an_mp4_at_the_asked_rate() {
    let d = dir("mp4");
    let gif = gif_in(&d);
    let mp4 = d.join("out.mp4");
    ok(&run(&[
        s(&gif),
        s(&mp4),
        "--fps",
        "30",
        "--profile",
        "best",
        "--crf",
        "23",
        "--tune",
        "animation",
    ]));
    let p = probe(&mp4, &Prefs::default()).unwrap();
    assert_eq!(p.frames, Some(16));
    assert_eq!(p.fps, (30, 1));
    assert!((p.duration - 16.0 / 30.0).abs() < 0.002, "{}", p.duration);
    assert_eq!((p.width, p.height), (WIDTH, HEIGHT));
    let numbers = decoded_numbers(&mp4, &d.join("back"), HEIGHT);
    let want: Vec<u32> = expected(30).iter().map(|&i| i as u32 + 1).collect();
    assert_eq!(numbers, want);
}

fn decoded_numbers(mp4: &Path, back: &Path, height: u32) -> Vec<u32> {
    ok(&run(&[
        "stills",
        s(mp4),
        "--from",
        "0",
        "--height",
        &height.to_string(),
        "--out",
        s(back),
    ]));
    pngs(back)
        .iter()
        .map(|p| number_of(&pito_codecs::png::decode(&fs::read(p).unwrap()).unwrap()))
        .collect()
}

#[test]
fn a_gif_without_fps_keeps_every_frame_and_ends_on_the_last() {
    let d = dir("mp4-rate");
    let gif = gif_in(&d);
    let mp4 = d.join("out.mp4");
    ok(&run(&[s(&gif), s(&mp4)]));
    let p = probe(&mp4, &Prefs::default()).unwrap();
    assert_eq!(p.fps, (100, 1));
    assert_eq!(p.frames, Some(53));
    assert!((p.duration - 0.53).abs() < 0.002, "{}", p.duration);
    let numbers = decoded_numbers(&mp4, &d.join("back"), HEIGHT);
    let want: Vec<u32> = expected(100).iter().map(|&i| i as u32 + 1).collect();
    assert_eq!(numbers, want);
    for n in 1..=DELAYS.len() as u32 {
        assert!(numbers.contains(&n), "frame {n} is missing");
    }
    assert_eq!(numbers.last(), Some(&(DELAYS.len() as u32)));
}

#[test]
fn a_gif_from_our_encoder_converts_without_fps_at_its_own_rate() {
    for (rate, count) in [(15u32, 22u32), (12, 17), (30, 25)] {
        let d = dir(&format!("mp4-own-{rate}"));
        let source: Vec<Rgba> = (1..=count).map(|n| numbered(n, false)).collect();
        let delays: Vec<u16> = (0..u64::from(count))
            .map(|n| pito_codecs::gif::delay_cs(n, rate, 1))
            .collect();
        let gif = d.join("in.gif");
        fs::write(&gif, encode(&source, &delays, opts()).unwrap()).unwrap();
        let mp4 = d.join("out.mp4");
        ok(&run(&[s(&gif), s(&mp4)]));
        let p = probe(&mp4, &Prefs::default()).unwrap();
        assert_eq!(p.fps, (rate, 1), "{rate}");
        assert_eq!(p.frames, Some(u64::from(count)), "{rate}");
        let numbers = decoded_numbers(&mp4, &d.join("back"), HEIGHT);
        assert_eq!(numbers, (1..=count).collect::<Vec<_>>(), "{rate}");
        let out = d.join("stills");
        let said = ok(&run(&["stills", s(&gif), "--from", "0", "--out", s(&out)]));
        assert_eq!(pngs(&out).len(), count as usize, "{rate}");
        assert!(
            said.starts_with(&format!(
                "{count} png frames, {WIDTH}x{HEIGHT} at {rate} fps"
            )),
            "{said}"
        );
    }
}

#[test]
fn a_short_frame_before_a_long_one_survives_a_convert_without_fps() {
    let d = dir("mp4-short");
    let gif = d.join("short.gif");
    fs::write(
        &gif,
        encode(&[numbered(1, false), numbered(2, false)], &[2, 100], opts()).unwrap(),
    )
    .unwrap();
    let mp4 = d.join("out.mp4");
    ok(&run(&[s(&gif), s(&mp4)]));
    let p = probe(&mp4, &Prefs::default()).unwrap();
    assert_eq!(p.fps, (50, 1));
    assert_eq!(p.frames, Some(51));
    let numbers = decoded_numbers(&mp4, &d.join("back"), HEIGHT);
    assert_eq!(numbers[0], 1);
    assert!(numbers[1..].iter().all(|&n| n == 2), "{numbers:?}");
    assert_eq!(numbers.len(), 51);
}

#[test]
fn an_odd_canvas_converts_to_an_even_mp4_and_stills_keep_its_size() {
    let (w, h) = (386, 241);
    let d = dir("odd");
    let gif = d.join("odd.gif");
    let source: Vec<Rgba> = (1..=3).map(|n| numbered_at(w, h, n, false)).collect();
    fs::write(&gif, encode(&source, &[10, 10, 10], opts()).unwrap()).unwrap();
    let mp4 = d.join("out.mp4");
    ok(&run(&[s(&gif), s(&mp4), "--fps", "10"]));
    let p = probe(&mp4, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height), (w, h - 1));
    assert_eq!(p.frames, Some(3));
    assert_eq!(decoded_numbers(&mp4, &d.join("back"), h - 1), vec![1, 2, 3]);
    let out = d.join("out");
    ok(&run(&["stills", s(&gif), "--fps", "10", "--out", s(&out)]));
    let got = pngs(&out);
    assert_eq!(got.len(), 3);
    for (path, want) in got.iter().zip(&source) {
        let img = pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (w, h));
        assert!(rgb(&img) == rgb(want));
    }
}

#[test]
fn frames_drawn_as_changes_over_the_last_come_out_whole_in_stills() {
    let d = dir("diff");
    let gif = d.join("diff.gif");
    let source: Vec<Rgba> = (0..DELAYS.len())
        .map(|i| numbered(i as u32 + 1, false))
        .collect();
    let diffed = EncodeOptions {
        diff: true,
        ..opts()
    };
    fs::write(&gif, encode(&source, &DELAYS, diffed).unwrap()).unwrap();
    let out = d.join("out");
    ok(&run(&["stills", s(&gif), "--fps", "30", "--out", s(&out)]));
    let want = expected(30);
    let got = pngs(&out);
    assert_eq!(got.len(), want.len());
    for (k, (path, &i)) in got.iter().zip(&want).enumerate() {
        let img = pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap();
        assert!(rgb(&img) == rgb(&source[i]), "tick {k} is not frame {i}");
    }
}

#[test]
fn the_tick_model_matches_ffmpegs_fps_filter() {
    let r = Refs::open(refs::AREA);
    let gif = refs::ticks_gif().unwrap();
    r.assert_digest(refs::GIF_TICKS, &testref::sha256(&gif));
    let source: Vec<Rgba> = (0..DELAYS.len())
        .map(|i| numbered(i as u32 + 1, false))
        .collect();
    assert!(gif == encode(&source, &DELAYS, opts()).unwrap());
    for rate in refs::GIF_RATES {
        let numbers = refs::numbers_of_probes(&r.bytes(&refs::ticks_name(rate)));
        assert_eq!(numbers, expected(rate), "fps={rate}");
    }
}

#[test]
fn a_truncated_gif_is_refused_in_one_line() {
    let d = dir("truncated");
    let whole = encode(&frames(), &DELAYS, opts()).unwrap();
    let gif = d.join("cut.gif");
    fs::write(&gif, &whole[..whole.len() - 3]).unwrap();
    refused(
        &run(&["stills", s(&gif), "--fps", "10", "--out", s(&d.join("out"))]),
        "truncated GIF",
    );
    refused(&run(&[s(&gif), s(&d.join("out.mp4"))]), "truncated GIF");
    assert!(!d.join("out.mp4").exists());
}

#[test]
fn a_gif_with_no_frames_is_refused_in_one_line() {
    let d = dir("empty");
    let gif = d.join("empty.gif");
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&[16, 0, 16, 0, 0x80, 0, 0, 0, 0, 0, 255, 255, 255, 0x3B]);
    fs::write(&gif, &bytes).unwrap();
    refused(&run(&[s(&gif), s(&d.join("out.mp4"))]), "no frames");
    refused(
        &run(&["stills", s(&gif), "--fps", "10", "--out", s(&d.join("out"))]),
        "no frames",
    );
}

#[test]
fn a_gif_range_convert_is_refused() {
    let d = dir("range");
    let gif = gif_in(&d);
    refused(
        &run(&[s(&gif), s(&d.join("out.mp4")), "--from", "0.1"]),
        "a GIF converts whole",
    );
}

#[test]
fn gif_stills_take_a_window_write_a_manifest_and_refuse_video_only_flags() {
    let d = dir("window");
    let gif = gif_in(&d);
    let out = d.join("out");
    let said = ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "10",
        "--from",
        "0.1",
        "--to",
        "0.4",
        "--out",
        s(&out),
    ]));
    assert!(said.starts_with("3 png frames,"), "{said}");
    let numbers: Vec<u32> = pngs(&out)
        .iter()
        .map(|p| number_of(&pito_codecs::png::decode(&fs::read(p).unwrap()).unwrap()))
        .collect();
    let want: Vec<u32> = expected(10)[1..4].iter().map(|&i| i as u32 + 1).collect();
    assert_eq!(numbers, want);

    let json = d.join("json");
    let said = ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "10",
        "--from",
        "0.1",
        "--to",
        "0.4",
        "--out",
        s(&json),
        "--json",
    ]));
    let v: serde_json::Value = serde_json::from_str(said.trim()).unwrap();
    assert_eq!(v["count"], 3);
    assert_eq!(v["rate"], "10/1");
    assert_eq!(
        (v["width"].as_u64(), v["height"].as_u64()),
        (Some(u64::from(WIDTH)), Some(u64::from(HEIGHT)))
    );
    assert!((v["first"].as_f64().unwrap() - 0.1).abs() < 1e-6, "{v}");
    assert!((v["last"].as_f64().unwrap() - 0.3).abs() < 1e-6, "{v}");
    assert_eq!(pngs(&json).len(), 3);

    let none = d.join("none");
    refused(
        &run(&[
            "stills",
            s(&gif),
            "--fps",
            "10",
            "--from",
            "5",
            "--out",
            s(&none),
        ]),
        "no frame starts between",
    );
    for flags in [["--count", "4"], ["--at", "0.2"]] {
        let r = run(&["stills", s(&gif), flags[0], flags[1], "--out", s(&none)]);
        let stderr = text(&r.stderr);
        assert_ne!(r.status.code(), Some(0), "{flags:?}: {stderr}");
        assert!(
            stderr.starts_with("pcod: ") || stderr.starts_with("error: "),
            "{stderr}"
        );
        assert!(r.stdout.is_empty());
    }
    assert!(!none.exists() || pngs(&none).is_empty());
}

#[test]
fn a_gif_padded_after_its_last_image_probes_converts_and_gives_stills() {
    let d = dir("padded");
    let mut bytes = encode(&frames(), &DELAYS, opts()).unwrap();
    assert_eq!(bytes.pop(), Some(0x3B));
    bytes.push(0);
    let gif = d.join("padded.gif");
    fs::write(&gif, &bytes).unwrap();
    let said = ok(&run(&["probe", "--json", s(&gif)]));
    let v: serde_json::Value = serde_json::from_str(said.trim()).unwrap();
    assert_eq!(v["frames"], DELAYS.len(), "{v}");
    assert_eq!(
        (v["width"].as_u64(), v["alpha"].as_bool()),
        (Some(u64::from(WIDTH)), Some(true)),
        "{v}"
    );
    let seconds = SHOWN.iter().sum::<u64>() as f64 / 100.0;
    assert!(
        (v["seconds"].as_f64().unwrap() - seconds).abs() < 1e-6,
        "{v}"
    );
    let made = ok(&run(&[s(&gif), s(&d.join("out.mp4")), "--json"]));
    let m: serde_json::Value = serde_json::from_str(made.trim()).unwrap();
    assert_eq!(
        m["source"]["sha256"].as_str().map(str::len),
        Some(64),
        "{m}"
    );
    assert!(
        m["source"]["path"]
            .as_str()
            .unwrap()
            .ends_with("padded.gif"),
        "{m}"
    );
    let out = d.join("stills");
    ok(&run(&["stills", s(&gif), "--fps", "10", "--out", s(&out)]));
    assert_eq!(pngs(&out).len(), expected(10).len());
}

#[test]
fn a_gif_range_past_a_hundred_thousand_frames_needs_force() {
    let d = dir("cap");
    let two = [numbered_at(16, 16, 1, false), numbered_at(16, 16, 2, false)];
    let gif = d.join("long.gif");
    fs::write(&gif, encode(&two, &[60_000, 60_000], opts()).unwrap()).unwrap();
    let out = d.join("out");
    refused(
        &run(&["stills", s(&gif), "--fps", "100", "--out", s(&out)]),
        "120000 frames from 0.000 s is more than 100000; use --force",
    );
    assert!(!out.exists());
    let blocked = d.join("file");
    fs::write(&blocked, b"not a folder").unwrap();
    let inside = blocked.join("out");
    let forced = run(&[
        "stills",
        s(&gif),
        "--fps",
        "100",
        "--force",
        "--out",
        s(&inside),
    ]);
    refused(&forced, s(&inside));
    assert!(!text(&forced.stderr).contains("--force"));
    let small = d.join("small");
    let said = ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "100",
        "--to",
        "0.05",
        "--height",
        "8",
        "--force",
        "--out",
        s(&small),
    ]));
    assert!(said.starts_with("5 png frames, 8x8 at 100 fps"), "{said}");
    let written = pngs(&small);
    assert_eq!(written.len(), 5);
    let first = pito_codecs::png::decode(&fs::read(&written[0]).unwrap()).unwrap();
    assert_eq!((first.width, first.height), (8, 8));
}

#[test]
fn a_gif_takes_every_and_format_as_a_video_does() {
    let d = dir("every-format");
    let gif = gif_in(&d);
    let by_fps = d.join("fps");
    ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "10",
        "--out",
        s(&by_fps),
    ]));
    let by_every = d.join("every");
    let json = ok(&run(&[
        "stills",
        s(&gif),
        "--every",
        "100ms",
        "--out",
        s(&by_every),
        "--json",
    ]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["every"], 0.1, "{v}");
    assert_eq!(v["format"], "png", "{v}");
    assert_eq!(v["quality"], serde_json::Value::Null, "{v}");
    assert_eq!(v["count"], expected(10).len(), "{v}");
    assert_eq!(v["expected"], expected(10).len(), "{v}");
    assert_eq!(v["rate"], serde_json::Value::Null, "{v}");
    assert!(v["first_png"].is_number(), "{v}");
    let listed = v["files"].as_array().unwrap();
    let written = pngs(&by_every);
    assert_eq!(listed.len(), written.len(), "{v}");
    assert_eq!(listed[0]["path"], s(&written[0]), "{v}");
    assert_eq!(
        listed[listed.len() - 1]["path"],
        s(&written[written.len() - 1])
    );
    for (k, f) in listed.iter().enumerate() {
        assert_eq!(f["index"], k, "{f}");
        let at = k as f64 / 10.0;
        assert!((f["time"].as_f64().unwrap() - at).abs() < 1e-6, "{f}");
        assert!((f["slot"].as_f64().unwrap() - at).abs() < 1e-6, "{f}");
    }

    let mp4 = d.join("same.mp4");
    ok(&run(&[s(&gif), s(&mp4), "--fps", "30"]));
    let keys = |json: &str| {
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        let file: Vec<String> = v["files"][0].as_object().unwrap().keys().cloned().collect();
        (keys, file, v["rate"].clone(), v["every"].clone())
    };
    for flags in [["--every", "100ms"], ["--fps", "10"]] {
        let from_gif = d.join(format!("gif{}", flags[1]));
        let from_mp4 = d.join(format!("mp4{}", flags[1]));
        let (gif_json, mp4_json) = (
            ok(&run(&[
                "stills",
                s(&gif),
                flags[0],
                flags[1],
                "--out",
                s(&from_gif),
                "--json",
            ])),
            ok(&run(&[
                "stills",
                s(&mp4),
                flags[0],
                flags[1],
                "--out",
                s(&from_mp4),
                "--json",
            ])),
        );
        assert_eq!(keys(&gif_json), keys(&mp4_json), "{gif_json}\n{mp4_json}");
    }

    let (a, b) = (pngs(&by_fps), pngs(&by_every));
    assert_eq!(a.len(), expected(10).len());
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.file_name(), y.file_name());
        assert_eq!(fs::read(x).unwrap(), fs::read(y).unwrap());
    }

    let said = ok(&run(&[
        "stills",
        s(&gif),
        "--every",
        "0.1",
        "--force",
        "--out",
        s(&by_every),
    ]));
    assert!(
        said.starts_with(&format!(
            "{} png frames, {WIDTH}x{HEIGHT} one every 0.100 s, 0.000 s to ",
            expected(10).len()
        )),
        "{said}"
    );
    let jpeg = d.join("jpeg");
    let json = ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "10",
        "--format",
        "jpeg",
        "--quality",
        "70",
        "--out",
        s(&jpeg),
        "--json",
    ]));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["format"], "jpeg", "{v}");
    assert_eq!(v["quality"], 70, "{v}");
    assert_eq!(v["every"], serde_json::Value::Null, "{v}");
    assert_eq!(v["rate"], "10/1", "{v}");
    assert_eq!(
        fs::read_dir(&jpeg)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().unwrap() == "jpg")
            .count(),
        expected(10).len()
    );

    let webp = d.join("webp");
    let said = ok(&run(&[
        "stills",
        s(&gif),
        "--fps",
        "10",
        "--format",
        "webp",
        "--quality",
        "90",
        "--out",
        s(&webp),
    ]));
    assert!(
        said.starts_with(&format!(
            "{} webp frames, {WIDTH}x{HEIGHT} at 10 fps",
            expected(10).len()
        )),
        "{said}"
    );
    let mut files: Vec<PathBuf> = fs::read_dir(&webp)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    assert_eq!(files.len(), expected(10).len());
    for (k, (path, want)) in files.iter().zip(expected(10)).enumerate() {
        assert_eq!(path.file_name().unwrap(), &*format!("{k:06}.webp"));
        let img = pito_codecs::webp::decode(&fs::read(path).unwrap()).unwrap();
        assert_eq!(number_of(&img), want as u32 + 1, "{k}");
    }
}

#[cfg(unix)]
#[test]
fn a_gif_range_into_a_folder_it_cannot_write_leaves_no_frame() {
    use std::os::unix::fs::PermissionsExt;
    let d = dir("read-only");
    let gif = d.join("long.gif");
    let long: Vec<Rgba> = (0..80).map(|n| numbered_at(64, 48, n, false)).collect();
    fs::write(&gif, encode(&long, &[4; 80], opts()).unwrap()).unwrap();
    let shut = d.join("shut");
    fs::create_dir(&shut).unwrap();
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(shut.join("probe"), b"x").is_ok() {
        let _ = fs::remove_file(shut.join("probe"));
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skip: this user writes into a read-only folder");
        return;
    }
    let r = run(&["stills", s(&gif), "--fps", "30", "--out", s(&shut)]);
    fs::set_permissions(&shut, fs::Permissions::from_mode(0o755)).unwrap();
    refused(&r, &shut.join("0000").display().to_string());
    assert!(text(&r.stderr).contains(".png: "), "{}", text(&r.stderr));
    assert_eq!(fs::read_dir(&shut).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_gif_range_that_cannot_print_its_summary_says_why_in_one_line() {
    let d = dir("stdout-full");
    let gif = gif_in(&d);
    let out = d.join("out");
    for json in [false, true] {
        let mut args = vec![
            "stills",
            s(&gif),
            "--fps",
            "10",
            "--force",
            "--out",
            s(&out),
        ];
        if json {
            args.push("--json");
        }
        let r = Command::new(env!("CARGO_BIN_EXE_pcod"))
            .env("NO_COLOR", "1")
            .args(&args)
            .stdout(fs::File::create("/dev/full").unwrap())
            .output()
            .unwrap();
        let stderr = text(&r.stderr);
        assert_eq!(r.status.code(), Some(1), "{stderr}");
        assert!(stderr.starts_with("pcod: "), "{stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert!(stderr.contains("No space left on device"), "{stderr}");
    }
}
