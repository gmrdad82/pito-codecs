use super::*;
use crate::media::Codec;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/mkv");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tools() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| {
        Command::new(t)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn sources(seconds: u32) -> Vec<String> {
    vec![
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size=320x240:rate=30:duration={seconds}"),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("sine=frequency=440:sample_rate=48000:duration={seconds}"),
    ]
}

const H264: &[&str] = &[
    "-c:v", "libx264", "-bf", "2", "-g", "30", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "96k",
];
const VP9: &[&str] = &[
    "-c:v",
    "libvpx-vp9",
    "-g",
    "30",
    "-deadline",
    "realtime",
    "-cpu-used",
    "8",
    "-b:v",
    "300k",
    "-c:a",
    "libopus",
    "-b:a",
    "64k",
];

fn generate(name: &str, args: &[&str]) -> PathBuf {
    let path = dir().join(name);
    if !path.exists() {
        let tmp = dir().join(format!(".{}-{name}", std::process::id()));
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(sources(6))
            .args(args)
            .arg(&tmp)
            .status()
            .unwrap();
        assert!(st.success());
        if let Err(e) = std::fs::rename(&tmp, &path) {
            assert!(std::path::Path::new(&path).exists(), "{e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
    path
}

fn streamed(name: &str) -> PathBuf {
    let path = dir().join(name);
    if !path.exists() {
        let out = Command::new("ffmpeg")
            .args(["-v", "error"])
            .args(sources(6))
            .args(H264)
            .args(["-f", "matroska", "pipe:1"])
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(out.status.success());
        let tmp = dir().join(format!(".{}-{name}", std::process::id()));
        std::fs::write(&tmp, out.stdout).unwrap();
        if let Err(e) = std::fs::rename(&tmp, &path) {
            assert!(std::path::Path::new(&path).exists(), "{e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
    path
}

type Packets = Vec<Vec<(i64, u32, bool)>>;

fn ffprobe_packets(path: &Path) -> Packets {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts,size,flags",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let mut streams: Packets = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let f: Vec<&str> = line.split(',').collect();
        let s: usize = f[0].parse().unwrap();
        if streams.len() <= s {
            streams.resize(s + 1, Vec::new());
        }
        streams[s].push((
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].contains('K'),
        ));
    }
    streams
}

fn ours<S: ByteSource>(r: &Reader<S>) -> Packets {
    r.tracks
        .iter()
        .map(|t| {
            t.samples
                .iter()
                .map(|s| (s.pts, s.size, s.keyframe))
                .collect()
        })
        .collect()
}

fn decodes_clean(path: &Path) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn reader_matches_ffprobe_and_streams_and_seeks() {
    if !tools() {
        return;
    }
    let files = [
        generate("h264.mkv", H264),
        generate("vp9.webm", VP9),
        streamed("piped.mkv"),
    ];
    for path in &files {
        let r = Reader::open(path).unwrap();
        assert!(r.warnings.is_empty(), "{path:?}: {:?}", r.warnings);
        assert_eq!(ours(&r), ffprobe_packets(path), "{path:?}");
        assert_eq!(r.is_webm(), path.extension().unwrap() == "webm");
        let codecs: Vec<_> = r.tracks.iter().map(|t| t.codec()).collect();
        if r.is_webm() {
            assert_eq!(codecs, [Some(Codec::Vp9), Some(Codec::Opus)]);
        }
        let v = r.video().unwrap();
        assert_eq!((r.tracks[v].width, r.tracks[v].height), (320, 240));
        assert!((r.duration_seconds() - 6.0).abs() < 0.1);

        let mut streamed: Packets = vec![Vec::new(); r.tracks.len()];
        let mut stream = r.stream();
        let mut data = Vec::new();
        while let Some(b) = stream.next_into(&mut data).unwrap() {
            streamed[b.track].push((b.pts, data.len() as u32, b.keyframe));
        }
        assert_eq!(streamed, ours(&r), "{path:?}");

        let lazy = Reader::with_options(
            std::fs::File::open(path).unwrap(),
            ReadOptions { index: false },
        )
        .unwrap();
        for seconds in [0.0, 2.5, 4.0, 5.9] {
            let want = r.tracks[v].keyframe_at(r.tracks[v].ticks(seconds)).unwrap();
            let want = r.tracks[v].samples[want];
            for reader in [&r, &lazy] {
                let mut s = reader.stream();
                let landed = s.seek(v, seconds).unwrap();
                assert!((landed - r.tracks[v].seconds(want.pts)).abs() < 1e-9);
                let b = loop {
                    let b = s.next_into(&mut data).unwrap().unwrap();
                    if b.track == v {
                        break b;
                    }
                };
                assert_eq!((b.pts, b.keyframe), (want.pts, true), "{path:?} {seconds}");
            }
        }
    }
}

#[test]
fn writer_output_reads_back_in_ffmpeg() {
    if !tools() {
        return;
    }
    for (src, webm) in [
        (generate("h264.mkv", H264), false),
        (generate("vp9.webm", VP9), true),
    ] {
        let r = Reader::open(&src).unwrap();
        let out = dir().join(format!("ours.{}", if webm { "webm" } else { "mkv" }));
        let mut w = Writer::create_with(
            &out,
            Options {
                webm,
                ..Options::default()
            },
        )
        .unwrap();
        for t in &r.tracks {
            w.add_track(TrackConfig::from_track(t)).unwrap();
        }
        let mut order: Vec<(u64, usize, usize)> = r
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(k, t)| {
                t.samples
                    .iter()
                    .enumerate()
                    .map(move |(i, s)| (s.offset, k, i))
            })
            .collect();
        order.sort_unstable();
        for (_, k, i) in order {
            w.push(k, &r.packet(k, i).unwrap()).unwrap();
        }
        w.finish().unwrap();
        assert_eq!(ffprobe_packets(&out), ffprobe_packets(&src), "{out:?}");
        decodes_clean(&out);
        let back = Reader::open(&out).unwrap();
        assert_eq!(back.is_webm(), webm);
        assert!(!back.cues.is_empty());
        assert!((back.duration_seconds() - r.duration_seconds()).abs() < 0.05);
    }
    let h264 = Reader::open(generate("h264.mkv", H264)).unwrap();
    let mut w = Writer::new(std::io::Cursor::new(Vec::new()), Options::webm());
    assert!(
        w.add_track(TrackConfig::from_track(&h264.tracks[0]))
            .is_err()
    );
}

#[test]
fn mp4_to_mkv() {
    if !tools() {
        return;
    }
    let mp4 = dir().join("bframes.mp4");
    if !mp4.exists() {
        let tmp = dir().join(format!(".{}-bframes.mp4", std::process::id()));
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(sources(4))
            .args(H264)
            .arg(&tmp)
            .status()
            .unwrap();
        assert!(st.success());
        if let Err(e) = std::fs::rename(&tmp, &mp4) {
            assert!(std::path::Path::new(&mp4).exists(), "{e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
    let out = dir().join("from-mp4.mkv");
    remux_mp4(&mp4, &out).unwrap();
    decodes_clean(&out);
    let a = ffprobe_packets(&mp4);
    let b = ffprobe_packets(&out);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        let sizes = |p: &Vec<(i64, u32, bool)>| p.iter().map(|q| (q.1, q.2)).collect::<Vec<_>>();
        assert_eq!(sizes(x), sizes(y));
    }
}

#[test]
fn remux_cuts_keyframe_to_keyframe() {
    if !tools() {
        return;
    }
    for (src, ext) in [
        (generate("h264.mkv", H264), "mkv"),
        (generate("vp9.webm", VP9), "webm"),
    ] {
        for (snap, keys, start) in [(Snap::Outward, 3, 2.2), (Snap::Inward, 1, 3.0)] {
            let out = dir().join(format!("cut-{snap:?}.{ext}"));
            let span = remux(&src, &out, 2.2, 4.5, snap).unwrap();
            assert!((span.start - start).abs() < 0.01, "{span:?}");
            decodes_clean(&out);
            let r = Reader::open(&out).unwrap();
            assert_eq!(r.is_webm(), ext == "webm");
            let v = r.video().unwrap();
            assert_eq!(r.tracks[v].keyframes().len(), keys);
            let first = r
                .tracks
                .iter()
                .filter_map(|t| t.samples.iter().map(|s| s.pts).min())
                .min();
            assert!(first.unwrap().abs() <= 30, "{first:?}");
            assert_eq!(ffprobe_packets(&out), ours(&r));
        }
    }
}
