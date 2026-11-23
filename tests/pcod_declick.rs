#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::wav;

const RATE: u32 = 48_000;
const SECONDS: usize = 4;
const CLICKS: [(f64, f64, f32); 3] = [(0.5, 3.0, 0.1), (1.2, 2.0, 0.12), (2.9, 4.0, 0.1)];

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-declick")
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
    assert!(stderr.contains(words), "{words:?} not in {stderr}");
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn voice() -> Vec<f32> {
    let tau = 2.0 * std::f64::consts::PI;
    let mut state = 11u64;
    (0..RATE as usize * SECONDS)
        .map(|i| {
            let t = i as f64 / f64::from(RATE);
            let mut v = 0.0;
            for (a, b, f0) in [(0.3, 0.9, 120.0), (1.5, 2.3, 150.0), (2.6, 3.5, 110.0)] {
                if (a..b).contains(&t) {
                    let env = (((t - a) / 0.02).min(1.0)).min(((b - t) / 0.04).min(1.0));
                    let mut sum = 0.0;
                    for k in 1..=40 {
                        let k = f64::from(k);
                        sum += (tau * f0 * k * t + 0.9 * k * k).sin() / k.powf(2.0);
                    }
                    v += 0.25 * env * sum;
                }
            }
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v += 1e-4 * ((state >> 33) as f64 / (1u64 << 31) as f64 - 1.0);
            v as f32
        })
        .collect()
}

fn write(path: &Path, mono: &[f32]) {
    let mut w = wav::Writer::create(path, RATE, 2, wav::Format::Int16).unwrap();
    let stereo: Vec<f32> = mono.iter().flat_map(|&v| [v, v]).collect();
    w.write(&stereo).unwrap();
    w.finish().unwrap();
}

fn read(path: &Path) -> Vec<f32> {
    wav::decode(&fs::read(path).unwrap())
        .unwrap()
        .samples
        .chunks_exact(2)
        .map(|f| f[0])
        .collect()
}

fn dirty() -> (Vec<f32>, Vec<f32>) {
    let clean = voice();
    let mut x = clean.clone();
    for &(t, ms, amp) in &CLICKS {
        let start = (t * f64::from(RATE)) as usize;
        let len = (ms / 1000.0 * f64::from(RATE)) as usize;
        for k in 0..len {
            let p = k as f64 / len as f64;
            let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * p).cos();
            let t = k as f64 / f64::from(RATE);
            x[start + k] += amp * (hann * (2.0 * std::f64::consts::PI * 3000.0 * t).sin()) as f32;
        }
    }
    (clean, x)
}

fn peak(x: &[f32], t: f64, ms: f64) -> f32 {
    let a = (t * f64::from(RATE)) as usize;
    let b = a + (ms / 1000.0 * f64::from(RATE)) as usize;
    x[a..b].iter().fold(0.0, |m, v| m.max(v.abs()))
}

#[test]
fn a_wav_is_de_clicked_and_the_clicks_are_listed() {
    let d = dir("wav");
    let (clean, spiked) = dirty();
    write(&d.join("in.wav"), &spiked);
    let out = ok(&run(&[
        s(&d.join("in.wav")),
        s(&d.join("out.wav")),
        "--declick",
        "--declick-list",
    ]));
    assert!(out.contains("declick: 3 clicks repaired (medium)"), "{out}");
    let listed: Vec<&str> = out.lines().filter(|l| l.contains("dBFS")).collect();
    assert_eq!(listed.len(), 3, "{out}");
    assert!(listed[0].trim_start().starts_with("0:00.5"), "{out}");
    let fixed = read(&d.join("out.wav"));
    for &(t, ms, _) in &CLICKS {
        let was = peak(&clean, t, ms);
        assert!(
            (peak(&fixed, t, ms) - was).abs() < 0.03,
            "{t}: {} against {was}",
            peak(&fixed, t, ms)
        );
        assert!(peak(&spiked, t, ms) > was + 0.05);
    }
}

#[test]
fn a_clean_wav_comes_out_sample_identical() {
    let d = dir("clean");
    write(&d.join("in.wav"), &voice());
    let out = ok(&run(&[
        s(&d.join("in.wav")),
        s(&d.join("out.wav")),
        "--declick",
        "strong",
    ]));
    assert!(out.contains("declick: 0 clicks repaired (strong)"), "{out}");
    assert!(read(&d.join("out.wav")) == read(&d.join("in.wav")));
}

#[test]
fn json_carries_the_count_and_with_the_list_each_click() {
    let d = dir("json");
    write(&d.join("in.wav"), &dirty().1);
    let plain: serde_json::Value = serde_json::from_str(&ok(&run(&[
        s(&d.join("in.wav")),
        s(&d.join("a.wav")),
        "--declick",
        "light",
        "--json",
    ])))
    .unwrap();
    assert_eq!(plain["declick"]["repaired"], 3);
    assert_eq!(plain["declick"]["strength"], "light");
    assert!(plain["declick"].get("clicks").is_none());
    let listed: serde_json::Value = serde_json::from_str(&ok(&run(&[
        s(&d.join("in.wav")),
        s(&d.join("b.wav")),
        "--declick",
        "--declick-list",
        "--json",
    ])))
    .unwrap();
    let clicks = listed["declick"]["clicks"].as_array().unwrap();
    assert_eq!(clicks.len(), 3);
    assert!((clicks[1]["time"].as_f64().unwrap() - 1.2).abs() < 0.01);
    assert!(clicks[1]["level_db"].as_f64().unwrap() < 0.0);
    assert!(clicks[1]["above_db"].as_f64().unwrap() > 14.0);
}

#[test]
fn it_chains_with_loudness_declicking_first() {
    let d = dir("chain");
    write(&d.join("in.wav"), &dirty().1);
    let v: serde_json::Value = serde_json::from_str(&ok(&run(&[
        s(&d.join("in.wav")),
        s(&d.join("out.wav")),
        "--declick",
        "--loudness",
        "-14",
        "--json",
    ])))
    .unwrap();
    assert_eq!(v["declick"]["repaired"], 3);
    let after = v["files"][0]["loudness"]["integrated"].as_f64().unwrap();
    assert!((after + 14.0).abs() < 0.5, "{after}");
    let before = v["source"]["loudness"]["integrated"].as_f64().unwrap();
    assert!(before < -14.0 + 20.0);
}

#[test]
fn a_video_keeps_its_picture_and_has_its_audio_repaired() {
    let d = dir("video");
    let video = d.join("in.mp4");
    ok(&run(&[
        "synth",
        s(&video),
        "--size",
        "320x240",
        "--fps",
        "24",
        "--duration",
        "2",
        "--audio",
        "sine:440",
    ]));
    let out = ok(&run(&[
        s(&video),
        s(&d.join("out.mp4")),
        "--declick",
        "--declick-list",
    ]));
    assert!(out.contains("declick: 0 clicks repaired"), "{out}");
    let probe = text(&run(&["probe", s(&d.join("out.mp4"))]).stdout);
    assert!(probe.contains("320"), "{probe}");
    assert!(probe.to_lowercase().contains("aac"), "{probe}");
    assert!(!d.read_dir().unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".declick.")
    }));
}

#[test]
fn track_picks_the_stream_and_a_missing_one_is_refused() {
    let d = dir("track");
    write(&d.join("in.wav"), &dirty().1);
    let out = run(&[
        s(&d.join("in.wav")),
        s(&d.join("o.wav")),
        "--declick",
        "--track",
        "9",
    ]);
    refused(&out, "no audio stream 9");
    let first = run(&[
        s(&d.join("in.wav")),
        s(&d.join("p.wav")),
        "--declick",
        "--track",
        "0",
    ]);
    assert!(ok(&first).contains("3 clicks repaired"));
}

#[test]
fn declick_is_refused_where_it_cannot_apply() {
    let d = dir("refuse");
    write(&d.join("in.wav"), &voice());
    let i = s(&d.join("in.wav")).to_string();
    for (args, words) in [
        (
            vec![i.as_str(), s(&d.join("a.gif")), "--declick"],
            "--declick",
        ),
        (
            vec![i.as_str(), s(&d.join("a.s16")), "--declick"],
            "--declick",
        ),
        (
            vec![i.as_str(), s(&d.join("a.wav")), "--declick-list"],
            "--declick",
        ),
        (
            vec![i.as_str(), s(&d.join("a.wav")), "--declick", "harsh"],
            "harsh",
        ),
    ] {
        let out = run(&args);
        assert_ne!(out.status.code(), Some(0), "{args:?}");
        assert!(
            text(&out.stderr).contains(words),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn a_chosen_track_of_a_video_is_repaired() {
    let d = dir("tracks");
    let video = d.join("in.mp4");
    ok(&run(&[
        "synth",
        s(&video),
        "--size",
        "320x240",
        "--fps",
        "24",
        "--duration",
        "4",
        "--audio",
        "sine:440",
    ]));
    let (clean, spiked) = dirty();
    write(&d.join("voice.wav"), &spiked);
    let two = d.join("two.mp4");
    ok(&run(&[
        s(&video),
        s(&two),
        "--add-audio",
        s(&d.join("voice.wav")),
    ]));
    let out = ok(&run(&[
        s(&two),
        s(&d.join("out.mp4")),
        "--declick",
        "--track",
        "2",
        "--declick-list",
    ]));
    assert!(out.contains("3 clicks repaired"), "{out}");
    let pcm = |file: &Path, track: &str, name: &str| -> Vec<f32> {
        let raw = d.join(name);
        ok(&run(&[
            s(file),
            s(&raw),
            "--track",
            track,
            "--mono",
            "--rate",
            "48000",
        ]));
        fs::read(raw)
            .unwrap()
            .chunks_exact(2)
            .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
            .collect()
    };
    let before = pcm(&two, "2", "before.s16");
    let after = pcm(&d.join("out.mp4"), "1", "after.s16");
    for &(t, ms, _) in &CLICKS {
        let was = peak(&clean, t, ms);
        assert!(peak(&before, t, ms) > was + 0.04, "{t}");
        assert!(
            peak(&after, t, ms) < was + 0.03,
            "{t}: {}",
            peak(&after, t, ms)
        );
    }
}
