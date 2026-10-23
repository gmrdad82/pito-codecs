#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::{Choice, Prefs, Rgba, Seek, audio_pcm, frames_at_seek, probe};

const BITS: u32 = 24;

fn cpu() -> Prefs {
    Prefs {
        decode: Choice::Cpu,
        encode: Choice::Cpu,
        ..Prefs::default()
    }
}

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/synth-test")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn pcod(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

fn synth(out: &Path, size: &str, fps: &str, seconds: &str, audio: Option<&str>) {
    let mut args = vec![
        "synth",
        out.to_str().unwrap(),
        "--size",
        size,
        "--fps",
        fps,
        "--duration",
        seconds,
    ];
    if let Some(a) = audio {
        args.extend(["--audio", a]);
    }
    ok(&pcod(&args));
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &json[at..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim_matches('"'))
}

fn counter(img: &Rgba) -> u64 {
    let (w, h) = (img.width, img.height);
    let top = (h * 2 / 3) & !1;
    let band = h - top;
    let cell = (w / (BITS + 2)) & !1;
    let y = (top + band / 10 + top + band / 2) / 2;
    (0..BITS).fold(0u64, |acc, k| {
        let x = cell * (k + 1) + cell / 2;
        let at = ((y * w + x) * 4) as usize;
        let px = &img.data[at..at + 3];
        let luma =
            0.2126 * f64::from(px[0]) + 0.7152 * f64::from(px[1]) + 0.0722 * f64::from(px[2]);
        (acc << 1) | u64::from(luma > 128.0)
    })
}

fn power(samples: &[f32], hz: f64, rate: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / rate;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, s) in samples.iter().enumerate() {
        re += f64::from(*s) * (w * k as f64).cos();
        im += f64::from(*s) * (w * k as f64).sin();
    }
    (re * re + im * im) / (samples.len() as f64).powi(2)
}

fn peak_hz(samples: &[f32], rate: f64) -> (f64, f64) {
    (20..=400)
        .map(|k| f64::from(k) * 5.0)
        .map(|hz| (hz, power(samples, hz, rate)))
        .fold(
            (0.0, -1.0),
            |best, now| if now.1 > best.1 { now } else { best },
        )
}

fn assert_tone(samples: &[f32], hz: f64, rate: f64, what: &str) {
    let mid = &samples[samples.len() / 4..samples.len() * 3 / 4];
    let (at, p) = peak_hz(mid, rate);
    assert!(
        (at - hz).abs() <= 5.0,
        "{what}: the peak sits at {at} Hz, not {hz}"
    );
    let want = (0.125f64 / 2.0).powi(2);
    assert!(
        (p - want).abs() < want * 0.2,
        "{what}: power {p} at {at} Hz, want {want}"
    );
}

#[test]
fn each_container_probes_at_the_asked_size_rate_length_and_tracks() {
    let d = dir("containers");
    for ext in ["mp4", "mov", "mkv", "webm"] {
        let out = d.join(format!("s.{ext}"));
        synth(
            &out,
            "320x240",
            "30",
            "1.5",
            Some("sine:440,sine:880,silence"),
        );
        let p = probe(&out, &cpu()).unwrap();
        assert_eq!((p.width, p.height), (320, 240), "{ext}");
        assert!((p.fps() - 30.0).abs() < 1e-6, "{ext}: {:?}", p.fps);
        assert!((p.duration - 1.5).abs() < 0.05, "{ext}: {}", p.duration);
        assert_eq!(p.audio.len(), 3, "{ext}");
        let pcm = audio_pcm(&out, 0, 48_000, true, &cpu()).unwrap();
        let off = pcm.samples.len().abs_diff(72_000);
        let slack = if matches!(ext, "mp4" | "mov") {
            48
        } else {
            960
        };
        assert!(
            off <= slack,
            "{ext}: {} samples for 1.5 s",
            pcm.samples.len()
        );
        let want = if ext == "webm" { "vp9" } else { "h264" };
        assert_eq!(
            p.video_codec_name.to_lowercase().replace('.', ""),
            want,
            "{ext}"
        );
    }
    let out = d.join("ntsc.mp4");
    synth(&out, "640x360", "30000/1001", "2", None);
    let p = probe(&out, &cpu()).unwrap();
    assert_eq!(p.fps, (30000, 1001));
    assert_eq!(p.frames, Some(60));
    assert!(p.audio.is_empty());

    let out = d.join("cd.mp4");
    ok(&pcod(&[
        "synth",
        out.to_str().unwrap(),
        "--size",
        "320x240",
        "--fps",
        "25",
        "--duration",
        "2",
        "--audio",
        "sine:440",
        "--rate",
        "44100",
    ]));
    let p = probe(&out, &cpu()).unwrap();
    assert_eq!(p.audio[0].rate, 44_100);
    let pcm = audio_pcm(&out, 0, 44_100, true, &cpu()).unwrap();
    assert!(
        pcm.samples.len().abs_diff(88_200) <= 48,
        "{}",
        pcm.samples.len()
    );
    assert_tone(&pcm.samples, 440.0, 44_100.0, "44.1 kHz");

    let out = d.join("quiet.mp4");
    ok(&pcod(&[
        "synth",
        out.to_str().unwrap(),
        "--size",
        "320x240",
        "--fps",
        "30",
        "--duration",
        "0.5",
        "--rate",
        "32000",
    ]));
    assert!(probe(&out, &cpu()).unwrap().audio.is_empty());
}

#[test]
fn two_runs_write_the_same_bytes() {
    let d = dir("same");
    for ext in ["mp4", "mov", "webm", "mkv"] {
        let a = d.join(format!("a.{ext}"));
        let b = d.join(format!("b.{ext}"));
        synth(&a, "320x240", "25", "1", Some("sine:1000,silence"));
        synth(&b, "320x240", "25", "1", Some("sine:1000,silence"));
        assert!(fs::read(&a).unwrap() == fs::read(&b).unwrap(), "{ext}");
    }
}

#[test]
fn frame_n_shows_counter_n() {
    let d = dir("counter");
    for ext in ["mp4", "mov", "mkv", "webm"] {
        let out = d.join(format!("c.{ext}"));
        synth(&out, "320x240", "30", "2", None);
        let picks = [0u64, 1, 2, 17, 31, 44, 59];
        let times: Vec<f64> = picks.iter().map(|n| *n as f64 / 30.0).collect();
        let stills = frames_at_seek(&out, &times, 240, Seek::Exact, &cpu()).unwrap();
        let read: Vec<u64> = stills.iter().map(counter).collect();
        assert_eq!(read, picks, "{ext}");
        assert!(stills.windows(2).all(|p| p[0].data != p[1].data), "{ext}");
    }
}

#[test]
fn the_sine_tracks_play_their_frequency_at_minus_18_dbfs() {
    let d = dir("sine");
    for ext in ["mp4", "mkv", "webm"] {
        let out = d.join(format!("t.{ext}"));
        synth(
            &out,
            "320x240",
            "30",
            "1",
            Some("sine:440,sine:880,silence"),
        );
        let prefs = cpu();
        let a = audio_pcm(&out, 0, 48_000, true, &prefs).unwrap();
        let b = audio_pcm(&out, 1, 48_000, true, &prefs).unwrap();
        let s = audio_pcm(&out, 2, 48_000, true, &prefs).unwrap();
        assert_tone(&a.samples, 440.0, 48_000.0, ext);
        assert_tone(&b.samples, 880.0, 48_000.0, ext);
        let peak = a.samples.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((f64::from(peak) - 0.125).abs() < 0.015, "{ext}: {peak}");
        assert!(s.samples.iter().all(|v| v.abs() < 1e-3), "{ext}");
        let json = pcod(&["loudness", out.to_str().unwrap(), "--json"]);
        ok(&json);
        let json = text(&json.stdout);
        let max: f64 = field(&json, "max_volume").unwrap().parse().unwrap();
        assert!((max + 18.1).abs() < 0.6, "{ext}: {json}");
    }
}

fn float_wav(path: &Path, amplitude: f64, seconds: f64) {
    let rate = 48_000u32;
    let frames = (seconds * f64::from(rate)) as u32;
    let mut data = Vec::with_capacity(frames as usize * 8);
    for k in 0..frames {
        let v = (amplitude
            * (std::f64::consts::TAU * 1000.0 * f64::from(k) / f64::from(rate)).sin())
            as f32;
        data.extend_from_slice(&v.to_le_bytes());
        data.extend_from_slice(&v.to_le_bytes());
    }
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&3u16.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 8).to_le_bytes());
    w.extend_from_slice(&8u16.to_le_bytes());
    w.extend_from_slice(&32u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data.len() as u32).to_le_bytes());
    w.extend_from_slice(&data);
    fs::write(path, w).unwrap();
}

#[test]
fn loudness_json_gives_max_volume_and_the_duration() {
    let d = dir("peak");
    let wav = d.join("minus6.wav");
    float_wav(&wav, 10f64.powf(-6.0 / 20.0), 2.0);
    let out = pcod(&["loudness", wav.to_str().unwrap(), "--json"]);
    ok(&out);
    let json = text(&out.stdout);
    assert_eq!(field(&json, "max_volume"), Some("-6.0"), "{json}");
    let peak: f64 = field(&json, "sample_peak").unwrap().parse().unwrap();
    assert!((peak + 6.0).abs() < 0.01, "{json}");
    let true_peak: f64 = field(&json, "true_peak").unwrap().parse().unwrap();
    assert!(true_peak >= peak - 0.01, "{json}");
    let duration: f64 = field(&json, "duration").unwrap().parse().unwrap();
    assert!((duration - 2.0).abs() < 0.01, "{json}");

    let quiet = d.join("silent.wav");
    float_wav(&quiet, 0.0, 1.0);
    let out = pcod(&["loudness", quiet.to_str().unwrap(), "--json"]);
    ok(&out);
    assert_eq!(field(&text(&out.stdout), "max_volume"), Some("-91.0"));

    let hot = d.join("clipped.wav");
    float_wav(&hot, 1.5, 1.0);
    let out = pcod(&["loudness", hot.to_str().unwrap(), "--json"]);
    ok(&out);
    assert_eq!(field(&text(&out.stdout), "max_volume"), Some("0.0"));
}

#[test]
fn bad_sizes_durations_and_audio_are_refused_in_one_line() {
    let d = dir("refused");
    let out = d.join("x.mp4");
    let o = out.to_str().unwrap();
    for (args, says) in [
        (
            vec!["--size", "321x240", "--fps", "30", "--duration", "1"],
            "even",
        ),
        (
            vec!["--size", "320x0", "--fps", "30", "--duration", "1"],
            "even",
        ),
        (
            vec!["--size", "0x0", "--fps", "30", "--duration", "1"],
            "even",
        ),
        (
            vec!["--size", "64x48", "--fps", "30", "--duration", "1"],
            "128x96",
        ),
        (
            vec!["--size", "320x240", "--fps", "30", "--duration", "0"],
            "duration",
        ),
        (
            vec![
                "--size",
                "320x240",
                "--fps",
                "30",
                "--duration",
                "1",
                "--audio",
                "sine:440,noise",
            ],
            "unknown --audio entry 'noise'",
        ),
        (
            vec![
                "--size",
                "320x240",
                "--fps",
                "30",
                "--duration",
                "1",
                "--audio",
                "sine:30000",
            ],
            "frequency",
        ),
    ] {
        let mut full = vec!["synth", o];
        full.extend(args.iter().copied());
        let r = pcod(&full);
        assert_eq!(r.status.code(), Some(1), "{args:?}");
        let err = text(&r.stderr);
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(err.starts_with("pcod: ") && err.contains(says), "{err}");
        assert!(!out.exists());
    }
    let webm = d.join("x.webm");
    let r = pcod(&[
        "synth",
        webm.to_str().unwrap(),
        "--size",
        "320x240",
        "--fps",
        "30",
        "--duration",
        "1",
        "--audio",
        "sine:440",
        "--rate",
        "44100",
    ]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("48000"));
    let r = pcod(&[
        "synth",
        o,
        "--size",
        "320x240",
        "--fps",
        "0",
        "--duration",
        "1",
    ]);
    assert_eq!(r.status.code(), Some(2), "{}", text(&r.stderr));
    assert!(text(&r.stderr).contains("For more information, try '--help'."));
    let r = pcod(&[
        "synth",
        o,
        "--size",
        "320x240",
        "--fps",
        "241",
        "--duration",
        "1",
    ]);
    assert_eq!(r.status.code(), Some(1));
    let err = text(&r.stderr);
    assert!(
        err.lines().count() == 1 && err.contains("frame rate"),
        "{err}"
    );
    assert!(!out.exists());
    let r = pcod(&[
        "synth",
        o,
        "--size",
        "big",
        "--fps",
        "30",
        "--duration",
        "1",
    ]);
    assert_eq!(r.status.code(), Some(2));
    assert!(text(&r.stderr).contains("For more information, try '--help'."));

    synth(&out, "320x240", "30", "0.5", None);
    let r = pcod(&[
        "synth",
        o,
        "--size",
        "320x240",
        "--fps",
        "30",
        "--duration",
        "0.5",
    ]);
    assert_eq!(r.status.code(), Some(1));
    assert!(text(&r.stderr).contains("--force"));
    ok(&pcod(&[
        "synth",
        o,
        "--size",
        "320x240",
        "--fps",
        "30",
        "--duration",
        "0.5",
        "--force",
    ]));
    let leftovers: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("partial"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn a_slice_and_a_join_of_synth_parts_need_no_ffmpeg() {
    let d = dir("cut-join");
    for ext in ["mp4", "mkv", "webm"] {
        let a = d.join(format!("a.{ext}"));
        let b = d.join(format!("b.{ext}"));
        let c = d.join(format!("c.{ext}"));
        synth(&a, "352x240", "30", "3", Some("sine:440"));
        synth(&b, "352x240", "30", "2", Some("sine:660"));
        synth(&c, "352x240", "30", "1", Some("sine:880"));
        let cut = d.join(format!("cut.{ext}"));
        ok(&pcod(&[
            a.to_str().unwrap(),
            cut.to_str().unwrap(),
            "--from",
            "1",
            "--to",
            "2",
        ]));
        let p = probe(&cut, &cpu()).unwrap();
        assert!((p.duration - 1.0).abs() < 0.1, "{ext}: {}", p.duration);
        assert_eq!((p.width, p.height), (352, 240));
        assert_eq!(p.audio.len(), 1, "{ext}");
        let first = frames_at_seek(&cut, &[0.0], 240, Seek::Exact, &cpu()).unwrap();
        assert_eq!(counter(&first[0]), 30, "{ext}");

        let joined = d.join(format!("joined.{ext}"));
        ok(&pcod(&[
            "join",
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            c.to_str().unwrap(),
            joined.to_str().unwrap(),
        ]));
        let p = probe(&joined, &cpu()).unwrap();
        assert!((p.duration - 6.0).abs() < 0.1, "{ext}: {}", p.duration);
        assert_eq!(p.audio.len(), 1, "{ext}");
        let at = frames_at_seek(
            &joined,
            &[89.0 / 30.0, 3.0, 4.0, 149.0 / 30.0, 5.0, 179.0 / 30.0],
            240,
            Seek::Exact,
            &cpu(),
        )
        .unwrap();
        let read: Vec<u64> = at.iter().map(counter).collect();
        assert_eq!(read, vec![89, 0, 30, 59, 0, 29], "{ext}");
        let pcm = audio_pcm(&joined, 0, 48_000, true, &cpu()).unwrap().samples;
        assert!(
            pcm.len().abs_diff(288_000) <= 2_000,
            "{ext}: {} samples for 6 s",
            pcm.len()
        );
        for (span, hz, others) in [
            (24_000..72_000, 440.0, [660.0, 880.0]),
            (156_000..216_000, 660.0, [440.0, 880.0]),
            (246_000..282_000, 880.0, [440.0, 660.0]),
        ] {
            let part = &pcm[span.clone()];
            for other in others {
                assert!(
                    power(part, hz, 48_000.0) > 100.0 * power(part, other, 48_000.0),
                    "{ext}: {span:?} plays {hz} Hz, not {other} Hz"
                );
            }
        }
    }
}

#[cfg(unix)]
fn interrupt(child: &std::process::Child) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let pid = i32::try_from(child.id()).unwrap();
    assert_eq!(unsafe { kill(pid, 2) }, 0);
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_synth_and_leaves_no_file_behind() {
    let d = dir("ctrl-c");
    let out = d.join("long.mp4");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args(["synth", out.to_str().unwrap(), "--size", "352x240"])
        .args(["--fps", "30", "--duration", "21600", "--audio", "sine:440"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while fs::read_dir(&d).unwrap().count() == 0 {
        assert!(started.elapsed().as_secs() < 60, "no partial file appeared");
        assert!(child.try_wait().unwrap().is_none(), "pcod ended early");
        std::thread::yield_now();
    }
    interrupt(&child);
    let done = child.wait_with_output().unwrap();
    let stderr = text(&done.stderr);
    assert_eq!(done.status.code(), Some(130), "{stderr}");
    assert_eq!(stderr, "pcod: interrupted by SIGINT\n");
    assert!(done.stdout.is_empty());
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
}
