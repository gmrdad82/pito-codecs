#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use pito_codecs::media::{Prefs, Rgba, first_frame, probe};

const FPS: u32 = 24;
const FRAMES: u32 = 120;
const WIDTH: u32 = 384;
const HEIGHT: u32 = 240;
const BLOCK: u32 = WIDTH / 8;
const RATE: usize = 48_000;
const PULSE: f64 = 2.5;
const PULSE_SAMPLES: usize = 480;
const SYNC: f64 = 0.005;
const FRAME: f64 = 1.0 / FPS as f64;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-ranges")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn stamp() -> String {
    let built = |p: &Path| {
        fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos())
    };
    let test = std::env::current_exe().map_or(0, |p| built(&p));
    format!("{} {test}", built(Path::new(env!("CARGO_BIN_EXE_pcod"))))
}

fn shared(name: &str, build: impl FnOnce(&Path)) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/pcod-ranges");
    let done = root.join(name);
    let key = stamp();
    let current = |d: &Path| fs::read_to_string(d.join(".built")).is_ok_and(|k| k == key);
    if current(&done) {
        return done;
    }
    fs::create_dir_all(&root).unwrap();
    let lock = fs::File::create(root.join(format!("{name}.lock"))).unwrap();
    lock.lock().unwrap();
    if current(&done) {
        return done;
    }
    let pid = std::process::id();
    let part = root.join(format!("{name}.{pid}.part"));
    let _ = fs::remove_dir_all(&part);
    fs::create_dir_all(&part).unwrap();
    build(&part);
    fs::write(part.join(".built"), &key).unwrap();
    if done.exists() && !current(&done) {
        let stale = root.join(format!("{name}.{pid}.stale"));
        if fs::rename(&done, &stale).is_ok() {
            let _ = fs::remove_dir_all(&stale);
        }
    }
    if fs::rename(&part, &done).is_err() {
        let _ = fs::remove_dir_all(&part);
    }
    assert!(current(&done), "{name} was not built");
    done
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

fn png_number(path: &Path) -> u32 {
    number_of(&pito_codecs::png::decode(&fs::read(path).unwrap()).unwrap())
}

fn pulse_wav(seconds: f64) -> Vec<u8> {
    wav(&pulse_pcm(seconds, PULSE))
}

fn pulse_pcm(seconds: f64, pulse: f64) -> Vec<u8> {
    let total = (seconds * RATE as f64) as usize;
    let at = (pulse * RATE as f64) as usize;
    let mut pcm = Vec::with_capacity(total * 4);
    for i in 0..total {
        let v: i16 = if (at..at + PULSE_SAMPLES).contains(&i) {
            let phase = (i - at) as f64 * 1000.0 / RATE as f64;
            ((phase * std::f64::consts::TAU).sin() * 29_000.0) as i16
        } else {
            0
        };
        pcm.extend_from_slice(&v.to_le_bytes());
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    pcm
}

fn wav(pcm: &[u8]) -> Vec<u8> {
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&(RATE as u32).to_le_bytes());
    wav.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
    wav.extend_from_slice(&4u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}

struct Sources {
    frames: PathBuf,
    mp4: PathBuf,
    webm: PathBuf,
}

fn sources() -> &'static Sources {
    static MADE: OnceLock<Sources> = OnceLock::new();
    MADE.get_or_init(|| {
        let root = shared("sources", |root| {
            let frames = root.join("frames");
            fs::create_dir_all(&frames).unwrap();
            for n in 0..FRAMES {
                let png = pito_codecs::png::encode(&numbered(n)).unwrap();
                fs::write(frames.join(format!("{n:06}.png")), png).unwrap();
            }
            let wav = root.join("pulse.wav");
            fs::write(&wav, pulse_wav(f64::from(FRAMES) / f64::from(FPS))).unwrap();
            let mp4 = root.join("numbered.mp4");
            ok(&run(&[
                "frames",
                s(&frames),
                s(&mp4),
                "--fps",
                "24",
                "--audio",
                s(&wav),
                "--route",
                "cpu",
                "--crf",
                "12",
            ]));
            let webm = root.join("numbered.webm");
            ok(&run(&[s(&mp4), s(&webm), "--crf", "20"]));
        });
        Sources {
            frames: root.join("frames"),
            mp4: root.join("numbered.mp4"),
            webm: root.join("numbered.webm"),
        }
    })
}

fn frames_at(video: &Path, times: &str, out: &Path) -> Vec<u32> {
    ok(&run(&[
        "stills",
        s(video),
        "--at",
        times,
        "--height",
        "240",
        "--route",
        "cpu",
        "--out",
        s(out),
    ]));
    let mut names: Vec<PathBuf> = fs::read_dir(out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    names.iter().map(|p| png_number(p)).collect()
}

fn pulse_and_length(video: &Path, work: &Path) -> (f64, f64) {
    let wav = work.join("audio.wav");
    ok(&run(&[s(video), s(&wav)]));
    let data = fs::read(&wav).unwrap();
    let rate = u32::from_le_bytes(data[24..28].try_into().unwrap()) as f64;
    let channels = u16::from_le_bytes(data[22..24].try_into().unwrap()) as usize;
    let samples: Vec<i16> = data[44..]
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let peak = samples.iter().map(|v| v.unsigned_abs()).max().unwrap();
    assert!(peak > 10_000, "no pulse in {}", video.display());
    let first = samples
        .iter()
        .position(|v| v.unsigned_abs() > peak / 2)
        .unwrap();
    let frames = samples.len() / channels;
    ((first / channels) as f64 / rate, frames as f64 / rate)
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

fn check_window(src: &Path, ext: &str) {
    let work = dir(&format!("window-{ext}"));
    let cut = work.join(format!("cut.{ext}"));
    let json = ok(&run(&[
        s(src),
        s(&cut),
        "--from",
        "2.0",
        "--to",
        "3.5",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").expect("a cut object")..];
    assert_eq!(number(cut_json, "frames"), 36.0, "{json}");
    assert_eq!(
        number(cut_json, "copied") + number(cut_json, "encoded"),
        36.0,
        "{json}"
    );
    assert!((number(cut_json, "from") - 2.0).abs() < 1e-6, "{json}");
    assert!((number(cut_json, "to") - 3.5).abs() < 1e-6, "{json}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert!(
        (p.duration - 1.5).abs() <= FRAME,
        "{ext}: {} s, not 1.5",
        p.duration
    );
    assert_eq!((p.width, p.height), (WIDTH, HEIGHT));
    if ext == "mp4" {
        assert_eq!(p.frames, Some(36), "{ext}: frames");
    }
    let shown = frames_at(&cut, "0,0.75,1.47", &work.join("stills"));
    assert_eq!(
        shown,
        vec![48, 66, 83],
        "{ext}: first, middle and last frame"
    );
    let (pulse, length) = pulse_and_length(&cut, &work);
    assert!(
        (pulse - (PULSE - 2.0)).abs() <= SYNC,
        "{ext}: the pulse at {pulse} s, not {}",
        PULSE - 2.0
    );
    assert!(
        (length - 1.5).abs() <= FRAME,
        "{ext}: {length} s of audio, not 1.5"
    );
}

#[test]
fn an_mp4_window_is_frame_exact_with_its_audio_in_sync() {
    check_window(&sources().mp4, "mp4");
}

#[test]
fn a_webm_window_is_frame_exact_with_its_audio_in_sync() {
    check_window(&sources().webm, "webm");
}

#[test]
fn mov_and_mkv_windows_carry_the_same_frames() {
    let src = &sources().mp4;
    for ext in ["mov", "mkv"] {
        let work = dir(&format!("window-{ext}"));
        let cut = work.join(format!("cut.{ext}"));
        ok(&run(&[s(src), s(&cut), "--from", "2", "--to", "3.5"]));
        let p = probe(&cut, &Prefs::default()).unwrap();
        assert!((p.duration - 1.5).abs() <= FRAME, "{ext}: {}", p.duration);
        if ext == "mov" {
            assert_eq!(p.frames, Some(36), "{ext}: frames");
        }
        assert_eq!(
            frames_at(&cut, "0,0.75,1.47", &work.join("stills")),
            vec![48, 66, 83],
            "{ext}"
        );
        let (pulse, _) = pulse_and_length(&cut, &work);
        assert!((pulse - 0.5).abs() <= SYNC, "{ext}: pulse at {pulse}");
    }
}

#[test]
fn a_window_between_frames_takes_the_first_frame_at_or_after_from_and_the_last_before_to() {
    let work = dir("between");
    let cut = work.join("cut.mp4");
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&cut),
        "--from",
        "2.01",
        "--to",
        "3.49",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").unwrap()..];
    assert_eq!(number(cut_json, "frames"), 35.0, "{json}");
    assert!((number(cut_json, "from") - 49.0 / 24.0).abs() < 1e-6);
    assert_eq!(
        frames_at(&cut, "0,1.44", &work.join("stills")),
        vec![49, 83]
    );
    let (pulse, _) = pulse_and_length(&cut, &work);
    assert!(
        (pulse - (PULSE - 49.0 / 24.0)).abs() <= SYNC,
        "pulse at {pulse}"
    );
}

#[test]
fn a_video_option_re_encodes_the_whole_window() {
    let work = dir("fitted");
    let cut = work.join("cut.mp4");
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&cut),
        "--from",
        "0:02",
        "--to",
        "0:03.5",
        "--size",
        "192",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").unwrap()..];
    assert_eq!(number(cut_json, "copied"), 0.0, "{json}");
    assert_eq!(number(cut_json, "encoded"), 36.0, "{json}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert_eq!((p.width, p.height), (192, 120));
    assert!((p.duration - 1.5).abs() <= FRAME);
    let stills = work.join("stills");
    ok(&run(&[
        "stills",
        s(&cut),
        "--at",
        "0",
        "--height",
        "120",
        "--route",
        "cpu",
        "--out",
        s(&stills),
    ]));
    assert_eq!(png_number(&stills.join("still-001.png")), 48);
}

#[test]
fn from_alone_runs_to_the_end_and_to_alone_starts_at_zero() {
    let work = dir("open-ends");
    let tail = work.join("tail.mp4");
    ok(&run(&[s(&sources().mp4), s(&tail), "--from", "4"]));
    let p = probe(&tail, &Prefs::default()).unwrap();
    assert!((p.duration - 1.0).abs() <= FRAME, "{}", p.duration);
    assert_eq!(frames_at(&tail, "0", &work.join("a")), vec![96]);
    let head = work.join("head.mp4");
    ok(&run(&[s(&sources().mp4), s(&head), "--to", "1"]));
    let p = probe(&head, &Prefs::default()).unwrap();
    assert!((p.duration - 1.0).abs() <= FRAME, "{}", p.duration);
    assert_eq!(frames_at(&head, "0", &work.join("b")), vec![0]);
}

#[test]
fn a_gif_and_a_webp_window_are_made_from_the_decoded_frames() {
    let work = dir("gif");
    for ext in ["gif", "webp"] {
        let out = work.join(format!("cut.{ext}"));
        let json = ok(&run(&[
            s(&sources().mp4),
            s(&out),
            "--from",
            "2",
            "--to",
            "3.5",
            "--json",
        ]));
        assert!(out.is_file(), "{ext}");
        assert_eq!(field(&json, "path"), Some(s(&sources().mp4)), "{json}");
        let shown = animated_frames(&out);
        assert_eq!(shown.len(), 18, "{ext}: 12 fps over 1.5 s");
        assert_eq!(number_of(&shown[0]), 48, "{ext}: the first frame");
        assert_eq!(number_of(&shown[9]), 66, "{ext}: the middle frame");
        assert_eq!(number_of(&shown[17]), 82, "{ext}: the last frame");
        assert!(
            shown[0].data.chunks_exact(4).all(|px| px[3] == 255),
            "{ext}: an opaque source makes opaque frames"
        );
        let cut_json = &json[json.find("\"cut\":").unwrap()..];
        assert_eq!(number(cut_json, "frames"), 36.0, "{json}");
        assert_eq!(number(cut_json, "copied"), 0.0, "{json}");
        assert_eq!(number(cut_json, "encoded"), 36.0, "{json}");
        assert!((number(cut_json, "from") - 2.0).abs() < 1e-6, "{json}");
        assert!((number(cut_json, "to") - 3.5).abs() < 1e-6, "{json}");
    }
    assert_eq!(
        listed(&work),
        ["cut.gif", "cut.webp"],
        "nothing left behind"
    );
}

#[test]
fn a_gif_window_from_the_start_and_to_the_end() {
    let work = dir("gif-ends");
    let head = work.join("head.gif");
    ok(&run(&[s(&sources().mp4), s(&head), "--to", "0.5"]));
    let shown = animated_frames(&head);
    assert_eq!(shown.len(), 6, "12 fps over 0.5 s");
    assert_eq!(number_of(&shown[0]), 0, "the first frame");
    let tail = work.join("tail.gif");
    ok(&run(&[s(&sources().mp4), s(&tail), "--from", "4.5"]));
    let shown = animated_frames(&tail);
    assert_eq!(shown.len(), 6, "12 fps over 0.5 s");
    assert_eq!(number_of(&shown[0]), 108, "the first frame");
    assert_eq!(number_of(&shown[5]), 118, "the last frame");
}

#[test]
fn an_animated_window_refuses_a_bad_range_in_one_line() {
    let work = dir("gif-refusals");
    let out = work.join("cut.gif");
    let src = s(&sources().mp4);
    refused(&run(&[src, s(&out), "--from", "9"]), "past the end");
    refused(
        &run(&[src, s(&out), "--from", "2", "--to", "1"]),
        "must come after",
    );
    refused(
        &run(&[src, s(&out), "--from", "4.99"]),
        "no frame of the video starts",
    );
    assert!(listed(&work).is_empty(), "{:?}", listed(&work));
}

#[test]
fn a_folder_of_frames_takes_the_frames_in_its_window() {
    let work = dir("folder");
    let out = work.join("cut.mp4");
    ok(&run(&[
        s(&sources().frames),
        s(&out),
        "--fps",
        "24",
        "--from",
        "1",
        "--to",
        "1.5",
    ]));
    let p = probe(&out, &Prefs::default()).unwrap();
    assert_eq!(p.frames, Some(12));
    assert_eq!(frames_at(&out, "0", &work.join("stills")), vec![24]);
}

fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_frame_range_writes_every_frame_as_numbered_pngs() {
    let work = dir("range");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(&sources().mp4),
        "--from",
        "1.0",
        "--to",
        "1.5",
        "--height",
        "120",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ]));
    let names = listed(&out);
    let want: Vec<String> = (0..12).map(|i| format!("{i:06}.png")).collect();
    assert_eq!(names, want);
    for (i, name) in names.iter().enumerate() {
        assert_eq!(png_number(&out.join(name)), 24 + i as u32, "{name}");
    }
    assert_eq!(number(&json, "count"), 12.0, "{json}");
    assert_eq!(number(&json, "fps"), 24.0, "{json}");
    assert_eq!(field(&json, "rate"), Some("24/1"), "{json}");
    assert_eq!(number(&json, "width"), 192.0, "{json}");
    assert_eq!(number(&json, "height"), 120.0, "{json}");
    assert_eq!(field(&json, "dir"), Some(s(&out)), "{json}");
    let img = pito_codecs::png::decode(&fs::read(out.join("000000.png")).unwrap()).unwrap();
    assert_eq!((img.width, img.height), (192, 120));
    let back = work.join("back.mp4");
    ok(&run(&[
        "frames",
        s(&out),
        s(&back),
        "--fps",
        "24",
        "--route",
        "cpu",
    ]));
    let p = probe(&back, &Prefs::default()).unwrap();
    assert_eq!(p.frames, Some(12));
}

#[test]
fn a_frame_range_resamples_to_the_nearest_frame() {
    let work = dir("range-fps");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(&sources().mp4),
        "--from",
        "1.0",
        "--to",
        "1.5",
        "--fps",
        "10",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ]));
    let names = listed(&out);
    assert_eq!(names.len(), 5, "{names:?}");
    let got: Vec<u32> = names.iter().map(|n| png_number(&out.join(n))).collect();
    assert_eq!(got, vec![24, 26, 29, 31, 34]);
    assert_eq!(number(&json, "count"), 5.0);
    assert_eq!(field(&json, "rate"), Some("10/1"));
}

#[test]
fn a_frame_range_takes_a_size() {
    let work = dir("range-size");
    let out = work.join("frames");
    ok(&run(&[
        "stills",
        s(&sources().mp4),
        "--from",
        "0",
        "--to",
        "0.25",
        "--size",
        "96x60",
        "--route",
        "cpu",
        "--out",
        s(&out),
    ]));
    let names = listed(&out);
    assert_eq!(names.len(), 6);
    for name in &names {
        let img = pito_codecs::png::decode(&fs::read(out.join(name)).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (96, 60), "{name}");
    }
}

#[test]
fn a_frame_range_into_a_folder_with_pngs_needs_force() {
    let work = dir("range-force");
    let out = work.join("frames");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("000099.png"), b"stale").unwrap();
    let args = |force: bool| {
        let mut a = vec![
            "stills".to_string(),
            s(&sources().mp4).into(),
            "--from".into(),
            "1".into(),
            "--to".into(),
            "1.125".into(),
            "--route".into(),
            "cpu".into(),
            "--out".into(),
            s(&out).into(),
        ];
        if force {
            a.push("--force".into());
        }
        a
    };
    let plain = args(false);
    let refs: Vec<&str> = plain.iter().map(String::as_str).collect();
    refused(&run(&refs), "already holds numbered frames");
    let forced = args(true);
    let refs: Vec<&str> = forced.iter().map(String::as_str).collect();
    ok(&run(&refs));
    assert_eq!(listed(&out), vec!["000000.png", "000001.png", "000002.png"]);
    fs::write(out.join("cover.png"), b"art").unwrap();
    refused(&run(&refs), "not numbered frames");
    assert_eq!(
        listed(&out),
        vec!["000000.png", "000001.png", "000002.png", "cover.png"]
    );
}

#[test]
fn a_range_past_a_hundred_thousand_frames_needs_force() {
    let work = dir("range-cap");
    let frames = work.join("frames");
    fs::create_dir_all(&frames).unwrap();
    let png = pito_codecs::png::encode(&numbered(1)).unwrap();
    for n in 0..101 {
        fs::write(frames.join(format!("{n:06}.png")), &png).unwrap();
    }
    let long = work.join("long.mp4");
    ok(&run(&[
        "frames",
        s(&frames),
        s(&long),
        "--fps",
        "1",
        "--route",
        "cpu",
    ]));
    let out = work.join("out");
    refused(
        &run(&[
            "stills",
            s(&long),
            "--from",
            "0",
            "--fps",
            "1000",
            "--route",
            "cpu",
            "--out",
            s(&out),
        ]),
        "use --force",
    );
    assert!(!out.join("000000.png").exists());
    refused(
        &run(&[
            "stills",
            s(&long),
            "--fps",
            "1000",
            "--route",
            "cpu",
            "--out",
            s(&out),
        ]),
        "101000 frames from 0.000 s to 101.000 s is more than 100000; use --force",
    );
    assert!(!out.join("000000.png").exists());
}

#[test]
fn fps_alone_takes_every_frame_from_the_start_to_the_end() {
    let work = dir("fps-alone");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(&sources().mp4),
        "--fps",
        "10",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ]));
    let names = listed(&out);
    let want: Vec<String> = (0..50).map(|n| format!("{n:06}.png")).collect();
    assert_eq!(names, want);
    let got: Vec<u32> = names.iter().map(|n| png_number(&out.join(n))).collect();
    let nearest: Vec<u32> = (0..50)
        .map(|k| (f64::from(k) * 2.4).round() as u32)
        .collect();
    assert_eq!(got, nearest);
    assert_eq!(number(&json, "count"), 50.0);
    assert_eq!(field(&json, "rate"), Some("10/1"));
    assert!(number(&json, "first").abs() < 1e-6, "{json}");
    assert!((number(&json, "last") - 4.9).abs() < 1e-6, "{json}");
}

#[test]
fn ranges_refuse_in_one_line() {
    let work = dir("refusals");
    let src = s(&sources().mp4);
    let out = |name: &str| work.join(name).to_str().unwrap().to_string();
    refused(
        &run(&[src, &out("a.mp4"), "--from", "3", "--to", "2"]),
        "must come after",
    );
    refused(
        &run(&[src, &out("b.mp4"), "--from", "2", "--to", "2"]),
        "must come after",
    );
    refused(&run(&[src, &out("c.mp4"), "--from", "9"]), "past the end");
    refused(&run(&[src, &out("c.gif"), "--from", "9"]), "past the end");
    refused(
        &run(&[src, &out("d.wav"), "--from", "3", "--to", "2"]),
        "must come after",
    );
    refused(
        &run(&[
            "stills",
            src,
            "--from",
            "3",
            "--to",
            "1",
            "--out",
            &out("e"),
        ]),
        "must come after",
    );
    refused(
        &run(&["stills", src, "--from", "60", "--out", &out("f")]),
        "past the end",
    );
    refused(
        &run(&[
            s(&sources().frames),
            &out("g.mp4"),
            "--fps",
            "24",
            "--from",
            "6",
        ]),
        "past the end",
    );
    let names = listed(&work);
    assert!(
        names
            .iter()
            .all(|n| !n.ends_with(".mp4") && !n.ends_with(".gif")),
        "{names:?}"
    );
}

#[test]
fn range_usage_errors_exit_2_like_clap() {
    let cases: &[&[&str]] = &[
        &[
            "stills", "v.mp4", "--from", "1", "--count", "3", "--out", "d",
        ],
        &["stills", "v.mp4", "--from", "1", "--at", "2", "--out", "d"],
        &["stills", "v.mp4", "--to", "1", "--sheet", "s.png"],
        &[
            "stills", "v.mp4", "--from", "1", "--seek", "keyframe", "--out", "d",
        ],
        &["stills", "v.mp4", "--size", "640", "--out", "d"],
        &["stills", "v.mp4", "--force", "--out", "d"],
        &[
            "stills", "v.mp4", "--from", "1", "--size", "640", "--height", "100", "--out", "d",
        ],
        &["stills", "v.mp4", "--from", "1"],
        &["stills", "v.mp4", "--from", "soon", "--out", "d"],
        &["stills", "v.mp4", "--from", "1", "--fps", "0", "--out", "d"],
        &["in.mp4", "out.mp4", "--from", "1:99"],
        &["in.mp4", "out.mp4", "--to", "-1"],
        &["in.mp4", "out.mp4", "--from", "1", "--mono"],
        &["join", "a.mp4", "b.mp4", "out.mp4", "--from", "1"],
        &["join", "a.mp4", "b.mp4", "out.mp4", "--to", "2"],
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
}

const SPLICES: [u32; 4] = [0, 60, 72, FRAMES];
const KEYED_PULSE: f64 = 2.8;

struct Keyed {
    mp4: PathBuf,
    webm: PathBuf,
}

fn keyed() -> &'static Keyed {
    static MADE: OnceLock<Keyed> = OnceLock::new();
    MADE.get_or_init(|| {
        let root = shared("keyed", |root| {
            let pcm = pulse_pcm(f64::from(FRAMES) / f64::from(FPS), KEYED_PULSE);
            let per_frame = RATE / FPS as usize * 4;
            let mut mp4s = Vec::new();
            let mut webms = Vec::new();
            for (part, pair) in SPLICES.windows(2).enumerate() {
                let (a, b) = (pair[0], pair[1]);
                let frames = root.join(format!("frames-{part}"));
                fs::create_dir_all(&frames).unwrap();
                for n in a..b {
                    let png = pito_codecs::png::encode(&numbered(n)).unwrap();
                    fs::write(frames.join(format!("{:06}.png", n - a)), png).unwrap();
                }
                let wav_path = root.join(format!("part-{part}.wav"));
                let bytes = &pcm[a as usize * per_frame..b as usize * per_frame];
                fs::write(&wav_path, wav(bytes)).unwrap();
                let mp4 = root.join(format!("part-{part}.mp4"));
                ok(&run(&[
                    "frames",
                    s(&frames),
                    s(&mp4),
                    "--fps",
                    "24",
                    "--audio",
                    s(&wav_path),
                    "--route",
                    "cpu",
                    "--crf",
                    "12",
                ]));
                let webm = root.join(format!("part-{part}.webm"));
                ok(&run(&[s(&mp4), s(&webm), "--crf", "20"]));
                mp4s.push(mp4);
                webms.push(webm);
            }
            let joined = |parts: &[PathBuf], out: PathBuf| {
                let mut args: Vec<&str> = vec!["join"];
                args.extend(parts.iter().map(|p| s(p)));
                args.push(s(&out));
                ok(&run(&args));
                out
            };
            joined(&mp4s, root.join("keyed.mp4"));
            joined(&webms, root.join("keyed.webm"));
        });
        Keyed {
            mp4: root.join("keyed.mp4"),
            webm: root.join("keyed.webm"),
        }
    })
}

fn check_splice(src: &Path, ext: &str) {
    let keys: Vec<u32> = pito_codecs::media::keyframes(src, &Prefs::default())
        .unwrap()
        .iter()
        .map(|t| (t * f64::from(FPS)).round() as u32)
        .collect();
    let pulse_frame = (KEYED_PULSE * f64::from(FPS)) as u32;
    let (k1, k2) = keys
        .windows(2)
        .map(|w| (w[0], w[1]))
        .find(|&(a, b)| (a..b).contains(&pulse_frame))
        .unwrap_or_else(|| panic!("{ext}: no closed GOP holds the pulse in {keys:?}"));
    assert!(k1 >= 6 && k2 + 6 <= FRAMES, "{ext}: {keys:?}");
    let (a, b) = (k1 - 6, k2 + 6);
    assert!(!keys.iter().any(|k| (a..k1).contains(k)), "{ext}: {keys:?}");
    let from = f64::from(a) / f64::from(FPS);
    let to = f64::from(b) / f64::from(FPS);
    let work = dir(&format!("splice-{ext}"));
    let cut = work.join(format!("cut.{ext}"));
    let json = ok(&run(&[
        s(src),
        s(&cut),
        "--from",
        &format!("{from}"),
        "--to",
        &format!("{to}"),
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").expect("a cut object")..];
    let frames = f64::from(b - a);
    assert_eq!(number(cut_json, "frames"), frames, "{json}");
    let (copied, encoded) = (number(cut_json, "copied"), number(cut_json, "encoded"));
    assert_eq!(copied + encoded, frames, "{ext}: {json}");
    assert!(
        copied >= f64::from(k2 - k1),
        "{ext}: the GOP {k1}..{k2} was not copied: {json}"
    );
    assert!(
        encoded >= 6.0,
        "{ext}: the head edge {a}..{k1} was not encoded: {json}"
    );
    let p = probe(&cut, &Prefs::default()).unwrap();
    let length = to - from;
    assert!(
        (p.duration - length).abs() <= FRAME,
        "{ext}: {} s, not {length}",
        p.duration
    );
    let want = [a, k1 - 1, k1, k2 - 1, k2, b - 1];
    let times: Vec<String> = want
        .iter()
        .map(|n| format!("{:.4}", (f64::from(n - a) + 0.5) / f64::from(FPS)))
        .collect();
    let shown = frames_at(&cut, &times.join(","), &work.join("stills"));
    assert_eq!(shown, want.to_vec(), "{ext}: across the splices {keys:?}");
    let (pulse, heard) = pulse_and_length(&cut, &work);
    assert!(
        (pulse - (KEYED_PULSE - from)).abs() <= SYNC,
        "{ext}: the pulse at {pulse} s in the copied GOP, not {}",
        KEYED_PULSE - from
    );
    assert!(
        (heard - length).abs() <= FRAME,
        "{ext}: {heard} s of audio, not {length}"
    );
}

#[test]
fn an_mp4_cut_copies_the_whole_gop_inside_it_and_re_encodes_its_edges() {
    check_splice(&keyed().mp4, "mp4");
}

#[test]
fn a_webm_cut_copies_the_whole_gop_inside_it_and_re_encodes_its_edges() {
    check_splice(&keyed().webm, "webm");
}

#[test]
fn a_webm_cut_into_mp4_re_encodes_h264_as_a_plain_convert_does() {
    let work = dir("webm-to-mp4");
    let cut = work.join("cut.mp4");
    let shown = ok(&run(&[
        s(&keyed().webm),
        s(&cut),
        "--from",
        "2",
        "--to",
        "3.5",
    ]));
    assert!(shown.contains("not VP9"), "{shown}");
    assert!(shown.contains("all re-encoded"), "{shown}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert_eq!(p.video_codec, Some(pito_codecs::media::Codec::H264));
    assert!((p.duration - 1.5).abs() <= FRAME, "{}", p.duration);
    assert_eq!(
        frames_at(&cut, "0,1.47", &work.join("stills")),
        vec![48, 83]
    );
    let (pulse, _) = pulse_and_length(&cut, &work);
    assert!(
        (pulse - (KEYED_PULSE - 2.0)).abs() <= SYNC,
        "pulse at {pulse}"
    );
}

#[test]
fn an_mp4_cut_into_webm_re_encodes_vp9_and_says_why() {
    let work = dir("mp4-to-webm");
    let cut = work.join("cut.webm");
    let shown = ok(&run(&[
        s(&keyed().mp4),
        s(&cut),
        "--from",
        "2",
        "--to",
        "3.5",
    ]));
    assert!(shown.contains("all re-encoded (VP9 on the"), "{shown}");
    assert!(shown.contains("h264"), "{shown}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert_eq!(p.video_codec, Some(pito_codecs::media::Codec::Vp9));
    assert_eq!(
        frames_at(&cut, "0,1.47", &work.join("stills")),
        vec![48, 83]
    );
    let (pulse, _) = pulse_and_length(&cut, &work);
    assert!(
        (pulse - (KEYED_PULSE - 2.0)).abs() <= SYNC,
        "pulse at {pulse}"
    );
}

#[test]
fn a_gif_and_a_webp_window_from_a_webm_source() {
    let work = dir("webm-animated");
    for ext in ["gif", "webp"] {
        let out = work.join(format!("cut.{ext}"));
        let json = ok(&run(&[
            s(&sources().webm),
            s(&out),
            "--from",
            "2",
            "--to",
            "3.5",
            "--json",
        ]));
        assert!(out.is_file(), "{ext}");
        let cut_json = &json[json.find("\"cut\":").unwrap()..];
        assert_eq!(number(cut_json, "frames"), 36.0, "{ext}: {json}");
        let shown = animated_frames(&out);
        assert_eq!(shown.len(), 18, "{ext}: 12 fps over 1.5 s");
        assert_eq!(number_of(&shown[0]), 48, "{ext}: the first frame");
    }
    let left: Vec<String> = listed(&work)
        .into_iter()
        .filter(|n| n != "cut.gif" && n != "cut.webp")
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

fn silent() -> &'static PathBuf {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        let frames = &sources().frames;
        shared("silent", |root| {
            ok(&run(&[
                s(frames),
                s(&root.join("silent.mp4")),
                "--fps",
                "24",
                "--crf",
                "12",
            ]));
        })
        .join("silent.mp4")
    })
}

#[test]
fn a_silent_source_cuts_to_a_silent_window() {
    let work = dir("silent-cuts");
    for ext in ["mp4", "mov", "mkv", "webm"] {
        let cut = work.join(format!("cut.{ext}"));
        ok(&run(&[s(silent()), s(&cut), "--from", "2", "--to", "3.5"]));
        let p = probe(&cut, &Prefs::default()).unwrap();
        assert!(p.audio.is_empty(), "{ext}: {:?}", p.audio);
        assert!((p.duration - 1.5).abs() <= FRAME, "{ext}: {}", p.duration);
        assert_eq!(
            frames_at(&cut, "0", &work.join(format!("stills-{ext}"))),
            vec![48],
            "{ext}"
        );
    }
}

#[test]
fn a_silent_source_refuses_audio_options_with_a_range() {
    let work = dir("silent-refusals");
    let out = work.join("cut.mp4");
    refused(
        &run(&[s(silent()), s(&out), "--from", "2", "--audio-bitrate", "96"]),
        "no audio",
    );
    refused(
        &run(&[s(silent()), s(&out), "--from", "2", "--loudness", "youtube"]),
        "no audio",
    );
    assert!(!out.exists());
}

#[test]
fn an_audio_bitrate_applies_to_the_window() {
    let work = dir("bitrate");
    let cut = work.join("cut.mp4");
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&cut),
        "--from",
        "2",
        "--to",
        "3.5",
        "--audio-bitrate",
        "96",
        "--json",
    ]));
    assert!(json.contains("\"kbps\":96"), "{json}");
    let (pulse, length) = pulse_and_length(&cut, &work);
    assert!((pulse - (PULSE - 2.0)).abs() <= SYNC, "pulse at {pulse}");
    assert!((length - 1.5).abs() <= FRAME, "{length}");
}

#[test]
fn a_window_with_no_frame_start_in_it_is_refused() {
    let work = dir("no-frame");
    let out = work.join("cut.mp4");
    refused(
        &run(&[s(&sources().mp4), s(&out), "--from", "4.99"]),
        "no frame starts",
    );
    refused(
        &run(&[s(&sources().mp4), s(&out), "--from", "5"]),
        "past the end",
    );
    assert!(!out.exists());
}

#[cfg(unix)]
#[test]
fn a_frame_range_into_an_unwritable_folder_fails_in_one_line_and_does_not_hang() {
    use std::os::unix::fs::PermissionsExt;
    let work = dir("range-unwritable");
    let out = work.join("frames");
    fs::create_dir_all(&out).unwrap();
    fs::set_permissions(&out, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(out.join("probe"), b"x").is_ok() {
        let _ = fs::remove_file(out.join("probe"));
        fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let args: Vec<String> = [
        "stills",
        s(&sources().mp4),
        "--from",
        "0",
        "--route",
        "cpu",
        "--out",
        s(&out),
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let _ = tx.send(run(&refs));
    });
    let done = rx.recv_timeout(std::time::Duration::from_secs(120));
    fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
    let done = done.expect("pcod stills hung on an unwritable folder");
    refused(&done, ".png: ");
    let stderr = text(&done.stderr);
    let named = stderr
        .strip_prefix(&format!("pcod: {}/", out.display()))
        .and_then(|rest| rest.split_once(".png: "))
        .map(|(stem, _)| stem.len() == 6 && stem.bytes().all(|b| b.is_ascii_digit()));
    assert_eq!(named, Some(true), "{stderr}");
    assert!(listed(&out).is_empty(), "{:?}", listed(&out));
}

fn animated_frames(path: &Path) -> Vec<Rgba> {
    let data = fs::read(path).unwrap();
    if path.extension().is_some_and(|e| e == "gif") {
        pito_codecs::gif::decode(&data)
            .unwrap()
            .frames
            .into_iter()
            .map(|(img, _)| img)
            .collect()
    } else {
        pito_codecs::webp::decode_animation(&data)
            .unwrap()
            .frames
            .into_iter()
            .map(|f| f.image)
            .collect()
    }
}

fn translucent(n: u32) -> Rgba {
    let mut img = numbered(n);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        if i as u32 / WIDTH >= HEIGHT * 3 / 4 {
            px[3] = 0;
        }
    }
    img
}

fn alpha_source() -> &'static PathBuf {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        shared("alpha", |root| {
            let frames = root.join("frames");
            fs::create_dir_all(&frames).unwrap();
            for n in 0..FRAMES {
                let png = rgba_png(&translucent(n));
                fs::write(frames.join(format!("{n:06}.png")), png).unwrap();
            }
            ok(&run(&[
                s(&frames),
                s(&root.join("alpha.webm")),
                "--fps",
                "24",
                "--crf",
                "20",
            ]));
        })
        .join("alpha.webm")
    })
}

fn see_through(img: &Rgba) -> (u8, u8) {
    let at = |y: u32| img.data[((y * img.width + img.width / 2) * 4 + 3) as usize];
    (at(img.height / 2), at(img.height * 7 / 8))
}

#[test]
fn an_alpha_webm_window_keeps_its_alpha() {
    let src = alpha_source();
    assert!(probe(src, &Prefs::default()).unwrap().alpha);
    let work = dir("alpha-webm");
    let cut = work.join("cut.webm");
    let json = ok(&run(&[
        s(src),
        s(&cut),
        "--from",
        "2",
        "--to",
        "3.5",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").expect("a cut object")..];
    assert_eq!(number(cut_json, "frames"), 36.0, "{json}");
    assert_eq!(number(cut_json, "encoded"), 36.0, "{json}");
    assert!((number(cut_json, "from") - 2.0).abs() < 1e-6, "{json}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert!(p.alpha, "the cut lost its alpha");
    assert!(
        (p.duration - 1.5).abs() <= FRAME,
        "{} s, not 1.5",
        p.duration
    );
    let first = first_frame(&cut, 0, &Prefs::default()).unwrap();
    assert_eq!(number_of(&first), 48, "the first frame");
    let (solid, clear) = see_through(&first);
    assert!(solid > 224 && clear < 32, "alpha {solid} and {clear}");
    assert_eq!(
        frames_at(&cut, "0,0.75,1.47", &work.join("stills")),
        vec![48, 66, 83],
        "first, middle and last frame"
    );
    assert_eq!(
        frames_at(&cut, "0.02,0.77", &work.join("between")),
        vec![48, 66],
        "a still between two frames shows the frame on screen"
    );
    assert_eq!(listed(&work), ["between", "cut.webm", "stills"]);
}

#[test]
fn an_alpha_webm_window_keeps_its_alpha_in_a_gif_and_a_webp() {
    let work = dir("alpha-animated");
    for ext in ["gif", "webp"] {
        let out = work.join(format!("cut.{ext}"));
        ok(&run(&[
            s(alpha_source()),
            s(&out),
            "--from",
            "2",
            "--to",
            "3.5",
        ]));
        let shown = animated_frames(&out);
        assert_eq!(shown.len(), 18, "{ext}: 12 fps over 1.5 s");
        assert_eq!(number_of(&shown[0]), 48, "{ext}: the first frame");
        assert_eq!(number_of(&shown[17]), 82, "{ext}: the last frame");
        let (solid, clear) = see_through(&shown[0]);
        assert!(
            solid > 224 && clear < 32,
            "{ext}: alpha {solid} and {clear}"
        );
    }
    assert_eq!(listed(&work), ["cut.gif", "cut.webp"]);
}

#[test]
fn a_window_levels_its_own_audio() {
    for (src, ext) in [(&sources().mp4, "mp4"), (&sources().webm, "webm")] {
        let work = dir(&format!("loudness-{ext}"));
        let cut = work.join(format!("cut.{ext}"));
        let json = ok(&run(&[
            s(src),
            s(&cut),
            "--from",
            "2",
            "--to",
            "3.5",
            "--loudness",
            "youtube",
            "--json",
        ]));
        assert!(json.contains("\"loudness\""), "{ext}: {json}");
        assert_eq!(listed(&work), [format!("cut.{ext}")], "{ext}");
        let p = probe(&cut, &Prefs::default()).unwrap();
        assert!((p.duration - 1.5).abs() <= FRAME, "{ext}: {}", p.duration);
        assert_eq!(
            frames_at(&cut, "0", &work.join("stills")),
            vec![48],
            "{ext}"
        );
        let (pulse, length) = pulse_and_length(&cut, &work);
        assert!(
            (pulse - (PULSE - 2.0)).abs() <= SYNC,
            "{ext}: pulse at {pulse}"
        );
        assert!((length - 1.5).abs() <= FRAME, "{ext}: {length}");
    }
}

fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for byte in parts.iter().flat_map(|p| p.iter()) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                crc >> 1 ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn rgba_png(img: &Rgba) -> Vec<u8> {
    let row = img.width as usize * 4;
    let raw: Vec<u8> = img
        .data
        .chunks_exact(row)
        .flat_map(|line| std::iter::once(0).chain(line.iter().copied()))
        .collect();
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        let len = block.len() as u16;
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (a, b) = raw.iter().fold((1u32, 0u32), |(a, b), &v| {
        let a = (a + u32::from(v)) % 65_521;
        (a, (b + a) % 65_521)
    });
    z.extend_from_slice(&(b << 16 | a).to_be_bytes());
    let mut head = Vec::new();
    head.extend_from_slice(&img.width.to_be_bytes());
    head.extend_from_slice(&img.height.to_be_bytes());
    head.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    for (kind, data) in [(b"IHDR", &head), (b"IDAT", &z), (b"IEND", &Vec::new())] {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    }
    png
}

#[test]
fn the_rgba_png_writer_round_trips_through_the_decoder() {
    let img = translucent(5);
    let back = pito_codecs::png::decode(&rgba_png(&img)).unwrap();
    assert_eq!((back.width, back.height), (WIDTH, HEIGHT));
    assert!(back.data == img.data, "the PNG did not round-trip");
}

fn mux_webm(video: &Path, audio: &Path, out: &Path, squeeze: f64) {
    use pito_codecs::mkv;
    let vr = mkv::Reader::open(video).unwrap();
    let ar = mkv::Reader::open(audio).unwrap();
    let (vt, at) = (vr.video().unwrap(), ar.audio()[0]);
    let (vtrack, atrack) = (&vr.tracks[vt], &ar.tracks[at]);
    let mut w = mkv::Writer::create_with(out, mkv::Options::webm()).unwrap();
    let mut vcfg = mkv::TrackConfig::from_track(vtrack);
    vcfg.default_duration = Some(1_000_000_000 / u64::from(FPS));
    let v = w.add_track(vcfg).unwrap();
    let a = w.add_track(mkv::TrackConfig::from_track(atrack)).unwrap();
    let (vn, an) = (vtrack.samples.len(), atrack.samples.len());
    let (mut vi, mut ai) = (0, 0);
    let mut alpha = Vec::new();
    while vi < vn || ai < an {
        let vs = (vi < vn).then(|| vtrack.seconds(vtrack.samples[vi].pts) / squeeze);
        let sound = (ai < an).then(|| atrack.seconds(atrack.samples[ai].pts));
        match (vs, sound) {
            (Some(t), s) if s.is_none_or(|s| t <= s) => {
                let mut p = vr.packet(vt, vi).unwrap();
                p.pts = vtrack.ticks(t);
                p.dts = p.pts;
                let has = vr.read_alpha(vt, vi, &mut alpha).unwrap();
                w.push_alpha(v, &p, has.then_some(alpha.as_slice()))
                    .unwrap();
                vi += 1;
            }
            _ => {
                w.push(a, &ar.packet(at, ai).unwrap()).unwrap();
                ai += 1;
            }
        }
    }
    w.finish().unwrap();
}

fn alpha_with_opus() -> &'static PathBuf {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        let (video, audio) = (alpha_source(), &sources().webm);
        shared("alpha-opus", |root| {
            mux_webm(video, audio, &root.join("alpha-opus.webm"), 1.0);
        })
        .join("alpha-opus.webm")
    })
}

#[test]
fn the_alpha_fixture_carries_its_alpha_and_an_opus_track() {
    let p = probe(alpha_with_opus(), &Prefs::default()).unwrap();
    assert!(p.alpha, "the fixture lost its alpha");
    assert_eq!(p.audio.len(), 1, "{:?}", p.audio);
    assert!(
        p.audio[0].codec.eq_ignore_ascii_case("opus"),
        "{:?}",
        p.audio
    );
    assert!((p.duration - 5.0).abs() <= FRAME, "{} s", p.duration);
}

#[test]
fn an_alpha_webm_window_with_an_opus_track_keeps_its_alpha_and_its_audio_in_sync() {
    let work = dir("alpha-opus-cut");
    let cut = work.join("cut.webm");
    let json = ok(&run(&[
        s(alpha_with_opus()),
        s(&cut),
        "--from",
        "2",
        "--to",
        "3.5",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").expect("a cut object")..];
    assert_eq!(number(cut_json, "frames"), 36.0, "{json}");
    let p = probe(&cut, &Prefs::default()).unwrap();
    assert!(p.alpha, "the cut lost its alpha");
    assert_eq!(p.audio.len(), 1, "the cut lost its audio: {:?}", p.audio);
    assert!((p.duration - 1.5).abs() <= FRAME, "{} s", p.duration);
    let first = first_frame(&cut, 0, &Prefs::default()).unwrap();
    assert_eq!(number_of(&first), 48, "the first frame");
    let (solid, clear) = see_through(&first);
    assert!(solid > 224 && clear < 32, "alpha {solid} and {clear}");
    let (pulse, length) = pulse_and_length(&cut, &work);
    assert!((pulse - (PULSE - 2.0)).abs() <= SYNC, "pulse at {pulse}");
    assert!((length - 1.5).abs() <= FRAME, "{length}");
    assert_eq!(listed(&work), ["audio.wav", "cut.webm"]);
}

#[test]
fn a_frame_range_of_an_alpha_webm_writes_rgba_pngs_with_the_alpha() {
    let work = dir("alpha-range");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(alpha_source()),
        "--from",
        "2",
        "--to",
        "2.25",
        "--height",
        "240",
        "--out",
        s(&out),
        "--json",
    ]));
    assert_eq!(number(&json, "count"), 6.0, "{json}");
    assert_eq!(number(&json, "expected"), 6.0, "{json}");
    let names = listed(&out);
    assert_eq!(names.len(), 6, "{names:?}");
    for (i, name) in names.iter().enumerate() {
        let data = fs::read(out.join(name)).unwrap();
        assert_eq!(data[25], 6, "{name}: an RGBA PNG");
        let img = pito_codecs::png::decode(&data).unwrap();
        assert_eq!((img.width, img.height), (WIDTH, HEIGHT), "{name}");
        assert_eq!(number_of(&img), 48 + i as u32, "{name}");
        let (solid, clear) = see_through(&img);
        assert!(
            solid > 224 && clear < 32,
            "{name}: alpha {solid} and {clear}"
        );
    }
    let small = work.join("small");
    let json = ok(&run(&[
        "stills",
        s(alpha_source()),
        "--from",
        "2",
        "--to",
        "2.25",
        "--height",
        "120",
        "--out",
        s(&small),
        "--json",
    ]));
    assert_eq!(number(&json, "count"), 6.0, "{json}");
    for (i, name) in listed(&small).iter().enumerate() {
        let data = fs::read(small.join(name)).unwrap();
        assert_eq!(data[25], 6, "{name}: a scaled RGBA PNG");
        let img = pito_codecs::png::decode(&data).unwrap();
        assert_eq!((img.width, img.height), (WIDTH / 2, HEIGHT / 2), "{name}");
        assert_eq!(number_of(&img), 48 + i as u32, "{name}");
        let (solid, clear) = see_through(&img);
        assert!(
            solid > 224 && clear < 32,
            "{name}: scaled alpha {solid} and {clear}"
        );
    }
    let plain = work.join("plain");
    ok(&run(&[
        "stills",
        s(&sources().mp4),
        "--from",
        "2",
        "--to",
        "2.1",
        "--out",
        s(&plain),
    ]));
    let data = fs::read(plain.join("000000.png")).unwrap();
    assert_eq!(data[25], 2, "an opaque source still writes RGB PNGs");
}

fn short_stream() -> &'static PathBuf {
    static MADE: OnceLock<PathBuf> = OnceLock::new();
    MADE.get_or_init(|| {
        let src = &sources().webm;
        shared("short", |root| {
            mux_webm(src, src, &root.join("short.webm"), 2.0);
        })
        .join("short.webm")
    })
}

#[test]
fn a_frame_range_stops_at_the_last_real_frame_of_a_stream_that_ends_short() {
    let p = probe(short_stream(), &Prefs::default()).unwrap();
    assert!(
        (p.duration - 2.5).abs() <= FRAME,
        "the probe reads the video's own end: {}",
        p.duration
    );
    let work = dir("short-range");
    let out = work.join("frames");
    let json = ok(&run(&[
        "stills",
        s(short_stream()),
        "--from",
        "0",
        "--route",
        "cpu",
        "--out",
        s(&out),
        "--json",
    ]));
    let count = number(&json, "count");
    assert!(count < number(&json, "expected"), "{json}");
    assert!((59.0..=62.0).contains(&count), "{json}");
    assert_eq!(listed(&out).len() as f64, count, "{json}");
    let last = listed(&out).last().cloned().unwrap();
    let shown = png_number(&out.join(last));
    assert!(
        (FRAMES - 3..FRAMES).contains(&shown),
        "the last PNG shows frame {shown}, not one of the stream's last frames"
    );
    let text_out = ok(&run(&[
        "stills",
        s(short_stream()),
        "--from",
        "0",
        "--route",
        "cpu",
        "--out",
        s(&work.join("again")),
    ]));
    assert!(
        text_out.contains("note: the stream ends before its probed length"),
        "{text_out}"
    );
}

#[test]
fn a_range_past_the_last_real_frame_is_refused_and_leaves_no_folder() {
    let work = dir("short-past");
    let out = work.join("frames");
    refused(
        &run(&[
            "stills",
            s(short_stream()),
            "--from",
            "3",
            "--to",
            "4",
            "--route",
            "cpu",
            "--out",
            s(&out),
        ]),
        "--from 3 is at or past the end of the video",
    );
    assert!(!out.exists(), "the refusal left its --out folder behind");
    let kept = work.join("kept");
    fs::create_dir_all(&kept).unwrap();
    refused(
        &run(&[
            "stills",
            s(short_stream()),
            "--from",
            "3",
            "--to",
            "4",
            "--route",
            "cpu",
            "--out",
            s(&kept),
        ]),
        "--from 3 is at or past the end of the video",
    );
    assert!(kept.is_dir(), "a folder it didn't make stays");
}

#[test]
fn a_range_refuses_a_tune_the_cut_cannot_honour() {
    let work = dir("cut-tune");
    let out = work.join("cut.mp4");
    refused(
        &run(&[
            s(&sources().mp4),
            s(&out),
            "--from",
            "2",
            "--to",
            "3.5",
            "--tune",
            "film",
        ]),
        "--tune film doesn't apply to a --from/--to cut",
    );
    assert!(!out.exists());
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&out),
        "--from",
        "2.02",
        "--to",
        "3.5",
        "--tune",
        "animation",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").unwrap()..];
    assert!(number(cut_json, "encoded") > 0.0, "{json}");
    assert!(json.contains("\"tune\":\"animation\""), "{json}");
    let copied = work.join("copied.mp4");
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&copied),
        "--from",
        "2",
        "--to",
        "4",
        "--tune",
        "animation",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").unwrap()..];
    assert_eq!(
        number(cut_json, "copied"),
        0.0,
        "a tune re-encodes the whole window: {json}"
    );
    assert!(number(cut_json, "encoded") > 0.0, "{json}");
    assert!(json.contains("\"tune\":\"animation\""), "{json}");
}

#[test]
fn the_cut_manifest_reports_the_edge_settings_it_used() {
    let work = dir("cut-manifest");
    let smart = ok(&run(&[
        s(&sources().mp4),
        s(&work.join("smart.mp4")),
        "--from",
        "1.5",
        "--to",
        "4.5",
        "--json",
    ]));
    let smart_cut = &smart[smart.find("\"cut\":").unwrap()..];
    assert!(number(smart_cut, "encoded") > 0.0, "{smart}");
    assert!(number(smart_cut, "copied") > 0.0, "{smart}");
    assert!(
        smart.contains("\"preset\":\"speed\""),
        "smart edges: {smart}"
    );
    assert!(!smart.contains("\"tune\":\""), "{smart}");
    let whole = ok(&run(&[
        s(&sources().mp4),
        s(&work.join("whole.mp4")),
        "--from",
        "1.5",
        "--to",
        "4.5",
        "--crf",
        "20",
        "--json",
    ]));
    let whole_cut = &whole[whole.find("\"cut\":").unwrap()..];
    assert_eq!(number(whole_cut, "copied"), 0.0, "{whole}");
    assert!(whole.contains("\"preset\":\"balanced\""), "{whole}");
    assert_eq!(
        number(&smart, "crf"),
        number(&whole, "crf") - 2.0,
        "smart edges run the default CRF less 2: {smart} {whole}"
    );
}

#[test]
fn a_range_with_a_profile_re_encodes_the_whole_window_at_its_crf() {
    let work = dir("cut-profile");
    let json = ok(&run(&[
        s(&sources().mp4),
        s(&work.join("best.mp4")),
        "--from",
        "2.02",
        "--to",
        "3.5",
        "--profile",
        "best",
        "--json",
    ]));
    let cut_json = &json[json.find("\"cut\":").unwrap()..];
    assert_eq!(number(cut_json, "copied"), 0.0, "{json}");
    assert!(number(cut_json, "encoded") > 0.0, "{json}");
    assert!(json.contains("\"preset\":\"balanced\""), "{json}");
    assert!(
        json.contains("\"crf\":16"),
        "best maps 18 to crf 16: {json}"
    );
}
