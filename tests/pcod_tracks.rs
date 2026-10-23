#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::{Prefs, Rgba, audio_pcm_range};
use pito_codecs::mp4::{
    Brand, CodecConfig, Edit, FourCc, Options, Reader, Track, TrackConfig, Writer,
};

const RATE: u32 = 48_000;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-tracks")
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

fn ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|&s| ((s * 32768.0) as i16).to_le_bytes())
        .collect();
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
    wav
}

fn tone(path: &Path, seconds: f64, hz: f64) -> Vec<f32> {
    let n = (seconds * f64::from(RATE)) as usize;
    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / f64::from(RATE);
        let v = 0.3 * (std::f64::consts::TAU * hz * t).sin()
            + 0.1 * (std::f64::consts::TAU * 2.5 * hz * t).sin();
        for side in [1.0, 0.8] {
            let q = (v * side * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            samples.push(f32::from(q) / 32768.0);
        }
    }
    fs::write(path, wav_bytes(&samples)).unwrap();
    samples
}

fn source(out: &Path) -> PathBuf {
    let folder = out.join("frames");
    fs::create_dir_all(&folder).unwrap();
    for n in 0..48u32 {
        let mut img = Rgba::new(352, 240);
        for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i as u32 % 352, i as u32 / 352);
            px.copy_from_slice(&[
                (x + n * 6) as u8,
                (y * 2) as u8,
                ((x ^ y) + n * 3) as u8,
                255,
            ]);
        }
        fs::write(
            folder.join(format!("{n:06}.png")),
            pito_codecs::png::encode(&img).unwrap(),
        )
        .unwrap();
    }
    let voice = out.join("voice.wav");
    tone(&voice, 2.0, 330.0);
    let mp4 = out.join("source.mp4");
    ok(&run(&[
        "frames",
        s(&folder),
        s(&mp4),
        "--fps",
        "24",
        "--audio",
        s(&voice),
        "--route",
        "cpu",
    ]));
    mp4
}

fn top_level(path: &Path) -> Vec<[u8; 4]> {
    let data = fs::read(path).unwrap();
    let mut kinds = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let mut size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        if size == 1 {
            size = u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap());
        } else if size == 0 {
            size = (data.len() - at) as u64;
        }
        kinds.push(kind);
        at += size.max(8) as usize;
    }
    kinds
}

fn index_first(path: &Path) {
    let kinds = top_level(path);
    let moov = kinds.iter().position(|k| k == b"moov").expect("a moov box");
    let mdat = kinds
        .iter()
        .position(|k| k == b"mdat")
        .expect("an mdat box");
    assert!(moov < mdat, "{:?}", kinds);
}

fn same_samples(a: &Reader, at: usize, b: &Reader, bt: usize, count: usize) {
    assert!(count > 0);
    for i in 0..count {
        assert_eq!(a.read(at, i).unwrap(), b.read(bt, i).unwrap(), "sample {i}");
    }
}

fn shown(track: &Track, movie_ts: u32) -> f64 {
    track
        .edits
        .iter()
        .map(|e| e.segment_duration as f64)
        .sum::<f64>()
        / f64::from(movie_ts)
}

fn ffprobe(path: &Path, entries: &str) -> Option<Vec<String>> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success());
    if out.is_none() {
        eprintln!("skip: no ffprobe to cross-check {}", path.display());
    }
    out.map(|o| {
        text(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    })
}

fn snr_db(reference: &[f32], decoded: &[f32]) -> f64 {
    let (mut s, mut e) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(decoded) {
        s += f64::from(*a).powi(2);
        e += f64::from(*a - *b).powi(2);
    }
    10.0 * (s / e.max(1e-20)).log10()
}

fn one_line_refusal(out: &Output, needle: &str) {
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.trim_end().lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("pcod: "), "{stderr}");
    assert!(stderr.contains(needle), "{stderr}");
}

fn no_partials(folder: &Path) {
    let left: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn no_audio_copies_the_video_byte_for_byte_with_the_index_first() {
    let out = dir("no-audio");
    let src = source(&out);
    let copy = out.join("video.mp4");
    let done = run(&[s(&src), s(&copy), "--no-audio"]);
    ok(&done);
    assert_eq!(text(&done.stdout).trim(), s(&copy));
    let a = Reader::open(&src).unwrap();
    let b = Reader::open(&copy).unwrap();
    assert_eq!(a.audio().len(), 1);
    assert!(b.audio().is_empty());
    assert_eq!(b.tracks.len(), 1);
    let (va, vb) = (a.video().unwrap(), b.video().unwrap());
    assert_eq!(a.tracks[va].samples.len(), 48);
    assert_eq!(b.tracks[vb].samples.len(), 48);
    same_samples(&a, va, &b, vb, 48);
    index_first(&copy);
    if let Some(types) = ffprobe(&copy, "stream=codec_type") {
        assert_eq!(types, ["video"]);
    }

    let encoded = out.join("encoded.mp4");
    ok(&run(&[s(&src), s(&encoded), "--no-audio", "--crf", "30"]));
    let c = Reader::open(&encoded).unwrap();
    assert!(c.audio().is_empty());
    let vc = c.video().unwrap();
    assert_eq!(c.tracks[vc].samples.len(), 48);
    assert_ne!(c.read(vc, 0).unwrap(), a.read(va, 0).unwrap());
    index_first(&encoded);
    no_partials(&out);
}

#[test]
fn add_audio_keeps_the_first_track_and_adds_a_titled_bed_cut_to_the_video() {
    let out = dir("add-longer");
    let src = source(&out);
    let bed = out.join("music.wav");
    let music = tone(&bed, 3.0, 523.0);
    let mixed = out.join("mixed.mp4");
    let done = run(&[
        s(&src),
        s(&mixed),
        "--add-audio",
        s(&bed),
        "--title",
        "Voice,Music",
    ]);
    ok(&done);
    let stdout = text(&done.stdout);
    assert!(stdout.starts_with(s(&mixed)), "{stdout}");
    assert!(stdout.contains("160 kbps"), "{stdout}");
    assert!(stdout.contains("the video is shorter"), "{stdout}");
    index_first(&mixed);
    let a = Reader::open(&src).unwrap();
    let b = Reader::open(&mixed).unwrap();
    let (va, vb) = (a.video().unwrap(), b.video().unwrap());
    same_samples(&a, va, &b, vb, 48);
    assert_eq!(b.tracks[vb].samples.len(), 48);
    let audio = b.audio();
    assert_eq!(audio.len(), 2);
    let names: Vec<&str> = audio.iter().map(|&i| b.tracks[i].name.as_str()).collect();
    assert_eq!(names, ["Voice", "Music"]);
    let (aa, ab) = (a.audio()[0], audio[0]);
    let kept = b.tracks[ab].samples.len();
    assert!(kept + 2 >= a.tracks[aa].samples.len(), "{kept}");
    same_samples(&a, aa, &b, ab, kept);
    assert!(
        (b.duration_seconds() - 2.0).abs() < 0.01,
        "{}",
        b.duration_seconds()
    );
    for &t in &audio {
        let d = shown(&b.tracks[t], b.timescale);
        assert!((d - 2.0).abs() < 0.03, "track {t}: {d}");
    }
    let bed_track = &b.tracks[audio[1]];
    assert_eq!(bed_track.sample_rate(), RATE);
    assert_eq!(bed_track.channels(), 2);
    let decoded =
        audio_pcm_range(&mixed, 1, 0.0, f64::INFINITY, RATE, 2, &Prefs::default()).unwrap();
    let frames = decoded.frames();
    assert!((96_000..96_000 + 1024).contains(&frames), "{frames}");
    let n = frames.min(96_000) * 2;
    let snr = snr_db(&music[..n], &decoded.samples[..n]);
    assert!(snr > 40.0, "snr {snr:.2} dB");
    if let Some(titles) = ffprobe(&mixed, "stream_tags=name") {
        assert_eq!(titles, ["Voice", "Music"]);
    }
    if let Some(d) = ffprobe(&mixed, "format=duration") {
        let d: f64 = d[0].parse().unwrap();
        assert!((d - 2.0).abs() < 0.05, "{d}");
    }
    no_partials(&out);
}

#[test]
fn add_audio_cuts_the_video_to_a_shorter_wav() {
    let out = dir("add-shorter");
    let src = source(&out);
    let bed = out.join("short.wav");
    let music = tone(&bed, 1.0, 440.0);
    let mixed = out.join("short.mov");
    let done = run(&[
        s(&src),
        s(&mixed),
        "--add-audio",
        s(&bed),
        "--audio-bitrate",
        "96",
    ]);
    ok(&done);
    let stdout = text(&done.stdout);
    assert!(stdout.contains("96 kbps"), "{stdout}");
    assert!(stdout.contains("the WAV is shorter"), "{stdout}");
    index_first(&mixed);
    let a = Reader::open(&src).unwrap();
    let b = Reader::open(&mixed).unwrap();
    assert!(b.quicktime);
    let (va, vb) = (a.video().unwrap(), b.video().unwrap());
    let vt = &b.tracks[vb];
    let presented = vt
        .samples
        .iter()
        .filter(|x| vt.seconds(x.pts + vt.edit_offset) < 1.0 - 1e-9)
        .count();
    assert_eq!(presented, 24);
    assert!((shown(vt, b.timescale) - 1.0).abs() < 0.01);
    same_samples(&a, va, &b, vb, vt.samples.len());
    let audio = b.audio();
    assert_eq!(audio.len(), 2);
    assert!(audio.iter().all(|&i| b.tracks[i].name == "SoundHandler"));
    same_samples(
        &a,
        a.audio()[0],
        &b,
        audio[0],
        b.tracks[audio[0]].samples.len(),
    );
    assert!(b.tracks[audio[0]].samples.len() < a.tracks[a.audio()[0]].samples.len());
    assert!(
        (b.duration_seconds() - 1.0).abs() < 0.01,
        "{}",
        b.duration_seconds()
    );
    for &t in &audio {
        let d = shown(&b.tracks[t], b.timescale);
        assert!((d - 1.0).abs() < 0.03, "track {t}: {d}");
    }
    let decoded =
        audio_pcm_range(&mixed, 1, 0.0, f64::INFINITY, RATE, 2, &Prefs::default()).unwrap();
    assert!(
        (48_000..48_000 + 1024).contains(&decoded.frames()),
        "{}",
        decoded.frames()
    );
    let n = decoded.frames().min(48_000) * 2;
    let snr = snr_db(&music[..n], &decoded.samples[..n]);
    assert!(snr > 25.0, "snr {snr:.2} dB");
    if let Some(d) = ffprobe(&mixed, "format=duration") {
        let d: f64 = d[0].parse().unwrap();
        assert!((d - 1.0).abs() < 0.05, "{d}");
    }
}

#[test]
fn add_audio_refuses_bad_input_in_one_line_and_leaves_nothing() {
    let out = dir("add-bad");
    let src = source(&out);
    let bed = out.join("bed.wav");
    tone(&bed, 1.0, 440.0);
    let empty = out.join("empty.wav");
    fs::write(&empty, wav_bytes(&[])).unwrap();
    let missing = out.join("missing.wav");
    let target = out.join("out.mp4");

    let r = run(&[s(&src), s(&target), "--add-audio", s(&missing)]);
    one_line_refusal(&r, "missing.wav");
    let r = run(&[s(&src), s(&target), "--add-audio", s(&empty)]);
    one_line_refusal(&r, "empty.wav");
    one_line_refusal(&r, "holds no audio frames");
    let r = run(&[
        s(&src),
        s(&target),
        "--add-audio",
        s(&bed),
        "--title",
        "Voice,Music,Extra",
    ]);
    one_line_refusal(&r, "--title gives 3 titles for 2 audio tracks");
    let r = run(&[
        s(&src),
        s(&target),
        "--add-audio",
        s(&bed),
        "--title",
        "Voice,",
    ]);
    one_line_refusal(&r, "--title has an empty title");
    let webm = out.join("out.webm");
    let r = run(&[s(&src), s(&webm), "--add-audio", s(&bed)]);
    one_line_refusal(&r, "--add-audio writes .mp4 or .mov, not .webm");
    assert!(!webm.exists());
    assert!(!target.exists());
    no_partials(&out);

    let silent = out.join("silent.mp4");
    ok(&run(&[s(&src), s(&silent), "--no-audio"]));
    let r = run(&[
        s(&silent),
        s(&target),
        "--add-audio",
        s(&bed),
        "--title",
        "Voice,Music",
    ]);
    one_line_refusal(&r, "--title gives 2 titles for 1 audio track");
    ok(&run(&[
        s(&silent),
        s(&target),
        "--add-audio",
        s(&bed),
        "--title",
        "Music",
    ]));
    let b = Reader::open(&target).unwrap();
    assert_eq!(b.audio().len(), 1);
    assert_eq!(b.tracks[b.audio()[0]].name, "Music");
    let again = run(&[s(&silent), s(&target), "--add-audio", s(&bed)]);
    one_line_refusal(&again, "already exists");
    ok(&run(&[
        s(&silent),
        s(&target),
        "--add-audio",
        s(&bed),
        "--force",
    ]));
}

#[test]
fn track_flags_that_cannot_go_together_exit_2() {
    let out = dir("add-usage");
    let src = out.join("in.mp4");
    fs::write(&src, b"not read").unwrap();
    let bed = out.join("bed.wav");
    let target = out.join("out.mp4");
    let cases: [&[&str]; 8] = [
        &["--title", "Voice"],
        &["--add-audio", s(&bed), "--no-audio"],
        &["--add-audio", s(&bed), "--size", "720p"],
        &["--add-audio", s(&bed), "--from", "1"],
        &["--add-audio", s(&bed), "--audio-bitrate", "8"],
        &["--no-audio", "--loudness", "-16"],
        &["--no-audio", "--to", "1"],
        &["--no-audio", "--audio-bitrate", "128"],
    ];
    for flags in cases {
        let mut args = vec![s(&src), s(&target)];
        args.extend_from_slice(flags);
        let r = run(&args);
        let stderr = text(&r.stderr);
        assert_eq!(r.status.code(), Some(2), "{flags:?}: {stderr}");
        assert!(stderr.starts_with("error: "), "{flags:?}: {stderr}");
    }
    let wav = out.join("out.wav");
    for flag in [["--no-audio", ""], ["--add-audio", s(&bed)]] {
        let mut args = vec![s(&src), s(&wav), flag[0]];
        if !flag[1].is_empty() {
            args.push(flag[1]);
        }
        let r = run(&args);
        assert_eq!(r.status.code(), Some(2), "{flag:?}: {}", text(&r.stderr));
    }
    assert!(!target.exists() && !wav.exists());
    let other = out.join("b.mp4");
    let usage: [Vec<&str>; 2] = [
        vec![
            "join",
            s(&src),
            s(&other),
            s(&target),
            "--add-audio",
            s(&bed),
        ],
        vec![s(&out), s(&target), "--add-audio", s(&bed)],
    ];
    let said = [
        "--add-audio works on one file, not join",
        "--add-audio copies the video of a file, not a folder of frames",
    ];
    for (args, words) in usage.iter().zip(said) {
        let r = run(args);
        let stderr = text(&r.stderr);
        assert_eq!(r.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
        assert!(stderr.contains(words), "{words:?} not in {stderr}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bad = out.join(std::ffi::OsStr::from_bytes(b"bed\xff.wav"));
        let r = Command::new(env!("CARGO_BIN_EXE_pcod"))
            .env("NO_COLOR", "1")
            .arg(&src)
            .arg(&target)
            .arg("--add-audio")
            .arg(&bad)
            .output()
            .unwrap();
        let stderr = text(&r.stderr);
        assert_eq!(r.status.code(), Some(2), "{stderr}");
        assert!(
            stderr.starts_with("error: invalid UTF-8 was detected"),
            "{stderr}"
        );
    }
    assert!(!target.exists());
}

#[test]
fn add_audio_on_two_audio_tracks_keeps_the_first_says_so_and_writes_a_manifest() {
    let out = dir("add-twice");
    let src = source(&out);
    let bed = out.join("music.wav");
    tone(&bed, 3.0, 523.0);
    let both = out.join("both.mp4");
    ok(&run(&[s(&src), s(&both), "--add-audio", s(&bed)]));
    let again = out.join("again.mp4");
    let done = run(&[
        s(&both),
        s(&again),
        "--add-audio",
        s(&bed),
        "--title",
        "Voice,Bed",
    ]);
    ok(&done);
    let stdout = text(&done.stdout);
    assert!(stdout.contains("1 more audio track left out"), "{stdout}");
    let a = Reader::open(&both).unwrap();
    let b = Reader::open(&again).unwrap();
    assert_eq!(b.audio().len(), 2);
    let names: Vec<&str> = b
        .audio()
        .iter()
        .map(|&i| b.tracks[i].name.as_str())
        .collect();
    assert_eq!(names, ["Voice", "Bed"]);
    same_samples(
        &a,
        a.audio()[0],
        &b,
        b.audio()[0],
        b.tracks[b.audio()[0]].samples.len(),
    );

    let json = out.join("json.mp4");
    let done = run(&[s(&src), s(&json), "--add-audio", s(&bed), "--json"]);
    ok(&done);
    let v: serde_json::Value = serde_json::from_slice(&done.stdout).unwrap();
    let sources = v["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    assert!(
        sources[1]["path"].as_str().unwrap().ends_with("music.wav"),
        "{v}"
    );
    assert_eq!(sources[1]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(v["params"]["audio"]["kbps"], 160);
    assert_eq!(v["params"]["profile"], "copy");
    let file = &v["files"][0];
    assert_eq!(
        file["bytes"].as_u64().unwrap(),
        fs::metadata(&json).unwrap().len()
    );
    no_partials(&out);
}

#[test]
fn add_audio_refuses_to_copy_pcm_audio_into_an_mp4_and_keeps_it_in_a_mov() {
    let out = dir("add-pcm");
    let src = source(&out);
    let mov = out.join("pcm.mov");
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(&src)
        .args(["-c:v", "copy", "-c:a", "pcm_s16le"])
        .arg(&mov)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skip: no ffmpeg to make a PCM MOV");
        return;
    }
    let bed = out.join("bed.wav");
    tone(&bed, 1.0, 440.0);
    let mp4 = out.join("out.mp4");
    let r = run(&[s(&mov), s(&mp4), "--add-audio", s(&bed)]);
    one_line_refusal(&r, ".mp4 doesn't carry pcm: write a .mov");
    assert!(!mp4.exists());
    let kept = out.join("out.mov");
    ok(&run(&[s(&mov), s(&kept), "--add-audio", s(&bed)]));
    let b = Reader::open(&kept).unwrap();
    assert_eq!(b.audio().len(), 2);
    no_partials(&out);
}

struct Rewrite {
    brand: Brand,
    video: bool,
    video_frames: bool,
    lead_ms: u64,
    unknown_video: bool,
    audio: bool,
    more_audio: usize,
    second_entry_at: Option<usize>,
}

const REWRITE: Rewrite = Rewrite {
    brand: Brand::Mp4,
    video: true,
    video_frames: true,
    lead_ms: 0,
    unknown_video: false,
    audio: true,
    more_audio: 0,
    second_entry_at: None,
};

fn rewrite(src: &Path, out: &Path, how: Rewrite) {
    let r = Reader::open(src).unwrap();
    let mut w = Writer::create_with(
        out,
        Options {
            brand: how.brand,
            reserve: 0,
        },
    )
    .unwrap();
    w.set_movie_timescale(1000);
    let mut lanes = Vec::new();
    let mut switch = None;
    if how.video {
        let v = r.video().unwrap();
        let t = &r.tracks[v];
        let mut cfg = TrackConfig::from_track(t).unwrap();
        if how.lead_ms > 0 {
            let end = t
                .samples
                .iter()
                .map(|s| s.pts + i64::from(s.duration))
                .max()
                .unwrap();
            cfg.edits = vec![
                Edit {
                    segment_duration: how.lead_ms,
                    media_time: -1,
                    media_rate: 0x10000,
                },
                Edit {
                    segment_duration: (t.seconds(end) * 1000.0).round() as u64,
                    media_time: 0,
                    media_rate: 0x10000,
                },
            ];
        }
        if how.unknown_video {
            cfg.entry.format = FourCc(*b"xvid");
            cfg.entry.codec = CodecConfig::Other(FourCc(*b"xvid"));
        }
        let second = cfg.entry.clone();
        let o = w.add_track(cfg).unwrap();
        if let Some(at) = how.second_entry_at {
            switch = Some((o, at, w.add_entry(o, second).unwrap()));
        }
        if how.video_frames {
            lanes.push((v, o));
        }
    }
    if how.audio {
        let a = r.audio()[0];
        for _ in 0..=how.more_audio {
            let o = w
                .add_track(TrackConfig::from_track(&r.tracks[a]).unwrap())
                .unwrap();
            lanes.push((a, o));
        }
    }
    let mut data = Vec::new();
    for (from, to) in lanes {
        for (n, s) in r.tracks[from].samples.iter().enumerate() {
            if let Some((track, at, entry)) = switch
                && track == to
                && at == n
            {
                w.use_entry(to, entry).unwrap();
            }
            r.read_into(from, n, &mut data).unwrap();
            w.push_sample(to, &data, s.pts, s.dts, s.duration, s.keyframe)
                .unwrap();
        }
    }
    w.finish().unwrap();
}

#[test]
fn add_audio_refuses_inputs_it_cannot_copy_in_one_line() {
    let out = dir("add-inputs");
    let src = source(&out);
    let bed = out.join("bed.wav");
    tone(&bed, 1.0, 440.0);
    let target = out.join("out.mp4");

    let r = run(&[s(&src), s(&src), "--add-audio", s(&bed)]);
    one_line_refusal(&r, "input and output are the same file");
    let r = run(&[s(&src), s(&target), "--add-audio", s(&target)]);
    one_line_refusal(&r, "input and output are the same file");

    let mkv = out.join("in.mkv");
    fs::write(&mkv, [0x1a, 0x45, 0xdf, 0xa3, 0x9f, 0x42, 0x86, 0x81]).unwrap();
    let r = run(&[s(&mkv), s(&target), "--add-audio", s(&bed)]);
    one_line_refusal(
        &r,
        "--add-audio copies the tracks of an MP4 or MOV, not Matroska",
    );

    let m4a = out.join("voice.m4a");
    rewrite(
        &src,
        &m4a,
        Rewrite {
            video: false,
            ..REWRITE
        },
    );
    let r = run(&[s(&m4a), s(&target), "--add-audio", s(&bed)]);
    one_line_refusal(&r, "voice.m4a: no video track to keep");

    let hollow = out.join("hollow.mp4");
    rewrite(
        &src,
        &hollow,
        Rewrite {
            video_frames: false,
            ..REWRITE
        },
    );
    let r = run(&[s(&hollow), s(&target), "--add-audio", s(&bed)]);
    one_line_refusal(&r, "hollow.mp4: the video track has no frames");

    let late = out.join("late.mp4");
    rewrite(
        &src,
        &late,
        Rewrite {
            lead_ms: 1000,
            ..REWRITE
        },
    );
    assert!(Reader::open(&late).unwrap().tracks[0].edit_offset > 0);
    let short = out.join("short.wav");
    tone(&short, 0.5, 440.0);
    let r = run(&[s(&late), s(&target), "--add-audio", s(&short)]);
    one_line_refusal(&r, "short.wav: the WAV ends before the video's first frame");
    let long = out.join("long.wav");
    tone(&long, 2.0, 440.0);
    ok(&run(&[s(&late), s(&target), "--add-audio", s(&long)]));
    let kept = Reader::open(&target).unwrap();
    let vt = &kept.tracks[kept.video().unwrap()];
    assert_eq!(vt.samples.len(), 24);
    assert!(vt.edit_offset > 0);
    fs::remove_file(&target).unwrap();
    no_partials(&out);
}

#[test]
fn add_audio_names_a_video_codec_it_does_not_know_as_unknown() {
    let out = dir("add-unknown");
    let src = source(&out);
    let bed = out.join("bed.wav");
    tone(&bed, 2.0, 440.0);
    let mov = out.join("odd.mov");
    rewrite(
        &src,
        &mov,
        Rewrite {
            brand: Brand::Mov,
            unknown_video: true,
            ..REWRITE
        },
    );
    let r = Reader::open(&mov).unwrap();
    assert_eq!(r.tracks[r.video().unwrap()].codec(), None);
    let target = out.join("out.mov");
    let done = run(&[s(&mov), s(&target), "--add-audio", s(&bed), "--json"]);
    ok(&done);
    let v: serde_json::Value = serde_json::from_slice(&done.stdout).unwrap();
    assert_eq!(v["params"]["encoder"]["encoder"], "copy", "{v}");
    assert_eq!(v["params"]["encoder"]["codec"], "unknown", "{v}");
    let b = Reader::open(&target).unwrap();
    let vt = &b.tracks[b.video().unwrap()];
    assert_eq!(vt.entries[0].format, FourCc(*b"xvid"));
    assert_eq!(vt.samples.len(), 48);
    assert_eq!(b.audio().len(), 2);
    let mp4 = out.join("out.mp4");
    let r = run(&[s(&mov), s(&mp4), "--add-audio", s(&bed)]);
    one_line_refusal(
        &r,
        "--add-audio copies its video track as it is, and .mp4 doesn't carry xvid: write a .mov",
    );
    assert!(!mp4.exists());
}

#[test]
fn add_audio_keeps_every_sample_entry_and_which_sample_uses_which() {
    let out = dir("add-entries");
    let src = source(&out);
    let two = out.join("two.mp4");
    rewrite(
        &src,
        &two,
        Rewrite {
            second_entry_at: Some(20),
            ..REWRITE
        },
    );
    let a = Reader::open(&two).unwrap();
    let at = &a.tracks[a.video().unwrap()];
    assert_eq!(at.entries.len(), 2);
    let want: Vec<u32> = at.samples.iter().map(|s| s.entry).collect();
    assert_eq!(want.iter().filter(|&&e| e == 0).count(), 20);
    assert_eq!(want.iter().filter(|&&e| e == 1).count(), 28);
    let bed = out.join("bed.wav");
    tone(&bed, 2.0, 440.0);
    let target = out.join("out.mp4");
    ok(&run(&[s(&two), s(&target), "--add-audio", s(&bed)]));
    let b = Reader::open(&target).unwrap();
    let bt = &b.tracks[b.video().unwrap()];
    assert_eq!(bt.entries, at.entries);
    let got: Vec<u32> = bt.samples.iter().map(|s| s.entry).collect();
    assert_eq!(got, want);
    same_samples(&a, a.video().unwrap(), &b, b.video().unwrap(), 48);
    let back = out.join("back");
    ok(&run(&[
        "stills",
        s(&target),
        "--from",
        "0",
        "--out",
        s(&back),
        "--route",
        "cpu",
    ]));
    assert_eq!(fs::read_dir(&back).unwrap().count(), 48);
    no_partials(&out);
}

#[test]
fn add_audio_on_three_audio_tracks_says_two_are_left_out() {
    let out = dir("add-three");
    let src = source(&out);
    let three = out.join("three.mp4");
    rewrite(
        &src,
        &three,
        Rewrite {
            more_audio: 2,
            ..REWRITE
        },
    );
    assert_eq!(Reader::open(&three).unwrap().audio().len(), 3);
    let bed = out.join("bed.wav");
    tone(&bed, 2.0, 440.0);
    let target = out.join("out.mp4");
    let done = run(&[s(&three), s(&target), "--add-audio", s(&bed)]);
    ok(&done);
    let stdout = text(&done.stdout);
    assert!(
        stdout.contains(
            "note: the video and the first audio track copied, 2 more audio tracks left out, bed.wav encoded as AAC"
        ),
        "{stdout}"
    );
    let b = Reader::open(&target).unwrap();
    assert_eq!(b.audio().len(), 2);

    let json = out.join("json.mp4");
    let done = run(&[s(&three), s(&json), "--add-audio", s(&bed), "--json"]);
    ok(&done);
    let v: serde_json::Value = serde_json::from_slice(&done.stdout).unwrap();
    let sources = v["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2, "{v}");
    assert!(sources[0]["path"].as_str().unwrap().ends_with("three.mp4"));
    assert!(sources[1]["path"].as_str().unwrap().ends_with("bed.wav"));
    assert_eq!(v["params"]["profile"], "copy");
    assert_eq!(v["params"]["audio"]["encoder"], "aac");
    assert_eq!(Reader::open(&json).unwrap().audio().len(), 2);
    no_partials(&out);
}
