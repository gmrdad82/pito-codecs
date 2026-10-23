use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::export::{ExportOptions, Exported, Piece, export};
use super::mux::BROKEN;
use super::route::{Choice, Prefs};
use crate::gpu::{Backend, Direction, available};
use crate::media::{Codec, Error, Route};

const PIECES: usize = 8;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/edges");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

fn on_va(codec: Codec) -> bool {
    let all = available();
    let has = |d: Direction| {
        all.iter()
            .any(|&(c, dd, b)| c == codec && dd == d && b == Backend::VaApi)
    };
    has(Direction::Decode) && has(Direction::Encode)
}

fn ready(codec: Codec, x: &str) -> bool {
    if !on_va(codec) {
        eprintln!("no VA-API {codec} decoder and encoder: skipped");
        return false;
    }
    if !has_encoder(x) {
        eprintln!("no ffmpeg with {x} on PATH: skipped");
        return false;
    }
    true
}

struct Look<'a> {
    size: &'a str,
    sar: &'a str,
    color: &'a [&'a str],
}

const BT709_TV: [&str; 8] = [
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-color_range",
    "tv",
];

fn source(name: &str, codec: &[&str], look: &Look) -> PathBuf {
    let mut hash = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&(codec, look.size, look.sar, look.color), &mut hash);
    let key = std::hash::Hasher::finish(&hash);
    let path = dir().join(format!("{name}-{key:016x}.mp4"));
    crate::fixture::once(&path, |part| {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={}:rate=30:duration=12", look.size),
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=12"])
            .args(["-vf", &format!("setsar={}", look.sar)])
            .args(codec)
            .args(look.color)
            .args(["-c:a", "aac", "-b:a", "96k", "-shortest"])
            .arg(part)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    })
}

const X264: [&str; 12] = [
    "-c:v",
    "libx264",
    "-preset",
    "ultrafast",
    "-g",
    "15",
    "-keyint_min",
    "15",
    "-sc_threshold",
    "0",
    "-pix_fmt",
    "yuv420p",
];

fn h264_source(look: &Look) -> PathBuf {
    source("edges-h264", &X264, look)
}

const X265: [&str; 10] = [
    "-c:v",
    "libx265",
    "-preset",
    "ultrafast",
    "-pix_fmt",
    "yuv420p",
    "-tag:v",
    "hvc1",
    "-x265-params",
    "keyint=15:min-keyint=15:scenecut=0:open-gop=0:bframes=0:log-level=error",
];

fn hevc_source(look: &Look) -> PathBuf {
    source("edges-hevc", &X265, look)
}

fn pieces(path: &Path) -> Vec<Piece> {
    (0..PIECES)
        .map(|k| Piece {
            path: path.to_path_buf(),
            from: 1.25 * k as f64 + 0.2,
            to: 1.25 * k as f64 + 1.15,
        })
        .collect()
}

fn gpu_options(codec: Codec) -> ExportOptions {
    ExportOptions {
        video: codec,
        prefs: Prefs {
            decode: Choice::Gpu,
            encode: Choice::Gpu,
            ..Prefs::default()
        },
        ..ExportOptions::default()
    }
}

fn cut(src: &Path, codec: Codec, name: &str, broken: bool) -> (Exported, Duration) {
    let out = dir().join(name);
    let work = std::path::absolute(out.with_extension("parts")).unwrap();
    if broken {
        BROKEN.lock().unwrap().push(work.clone());
    }
    let start = Instant::now();
    let done = export(&out, &pieces(src), gpu_options(codec), |_| true);
    let wall = start.elapsed();
    if broken {
        BROKEN.lock().unwrap().retain(|w| *w != work);
    }
    (done.unwrap(), wall)
}

fn frame_tags(path: &Path) -> Vec<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("frame=sample_aspect_ratio,color_range,color_space,color_primaries,color_transfer")
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "ffprobe {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().trim_end_matches(',').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn decodes_clean(path: &Path) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    out.status.success() && out.stderr.is_empty()
}

fn per_edge(e: &Exported) -> f64 {
    e.sessions.setup.as_secs_f64() * 1000.0 / f64::from(e.sessions.edges.max(1))
}

fn report(label: &str, e: &Exported, wall: Duration) {
    eprintln!(
        "{label}: {} edges, {} sessions opened, {} reused, {} refreshed, setup {:.1} ms per edge, cut {:.0} ms",
        e.sessions.edges,
        e.sessions.opened,
        e.sessions.reused,
        e.sessions.refreshed,
        per_edge(e),
        wall.as_secs_f64() * 1000.0
    );
}

fn smart_cut_on_va(src: &Path, codec: Codec, tag: &str) {
    let want = frame_tags(src);
    let first = want.first().cloned().unwrap();
    assert!(want.iter().all(|t| *t == first), "{tag} source tags vary");

    let (fresh, fresh_wall) = cut(src, codec, &format!("edges-{tag}-fresh.mp4"), true);
    let (kept, kept_wall) = cut(src, codec, &format!("edges-{tag}-kept.mp4"), false);
    report(
        &format!("{tag} with a fresh session per edge"),
        &fresh,
        fresh_wall,
    );
    report(&format!("{tag} with sessions kept"), &kept, kept_wall);

    for (e, label) in [(&kept, "kept"), (&fresh, "fresh")] {
        assert!(e.smart, "{tag} {label}: {:?}", e.reasons);
        assert_eq!(e.video.route, Route::Gpu, "{tag} {label}: {:?}", e.reasons);
        assert_eq!(e.video.codec, codec);
        assert!(e.copied > 0 && e.encoded > 0, "{tag} {label}");
        assert!(
            (PIECES as u64 * 28..=PIECES as u64 * 29).contains(&e.frames),
            "{tag} {label}: {} frames",
            e.frames
        );
        assert!(e.sessions.edges as usize >= PIECES, "{tag} {label}");
        assert!(decodes_clean(&e.path), "{tag} {label} doesn't decode clean");
        let got = frame_tags(&e.path);
        assert_eq!(got.len() as u64, e.frames, "{tag} {label}");
        for (n, t) in got.iter().enumerate() {
            assert_eq!(*t, first, "{tag} {label} frame {n}");
        }
    }
    assert_eq!(kept.frames, fresh.frames);

    assert_eq!(kept.sessions.refreshed, 0, "{:?}", kept.reasons);
    let workers = kept.sessions.workers;
    assert!((1..=4).contains(&workers), "{:?}", kept.sessions);
    assert!(
        kept.sessions.opened <= 1 + 3 * workers,
        "{:?}",
        kept.sessions
    );
    assert!(
        kept.sessions.reused >= kept.sessions.edges - workers,
        "{:?}",
        kept.sessions
    );

    assert_eq!(fresh.sessions.reused, 0, "{:?}", fresh.sessions);
    assert!(fresh.sessions.refreshed > 0, "{:?}", fresh.sessions);
    assert!(
        fresh.sessions.opened > kept.sessions.opened,
        "{:?} against {:?}",
        fresh.sessions,
        kept.sessions
    );
    assert!(
        fresh.reasons.iter().any(|r| r.contains("couldn't restart")),
        "{:?}",
        fresh.reasons
    );
    assert!(
        !kept.reasons.iter().any(|r| r.contains("couldn't re")),
        "{:?}",
        kept.reasons
    );
}

fn tags_on_va(src: &Path, codec: Codec, tag: &str) {
    let want = frame_tags(src);
    let first = want.first().cloned().unwrap();
    assert!(want.iter().all(|t| *t == first), "{tag} source tags vary");
    let (e, wall) = cut(src, codec, &format!("edges-{tag}.mp4"), false);
    report(tag, &e, wall);
    assert!(e.smart, "{tag}: {:?}", e.reasons);
    assert_eq!(e.video.route, Route::Gpu, "{tag}: {:?}", e.reasons);
    assert!(e.copied > 0 && e.encoded > 0, "{tag}");
    assert_eq!(e.sessions.refreshed, 0, "{tag}: {:?}", e.reasons);
    assert!(decodes_clean(&e.path), "{tag} doesn't decode clean");
    let got = frame_tags(&e.path);
    assert_eq!(got.len() as u64, e.frames, "{tag}");
    for (n, t) in got.iter().enumerate() {
        assert_eq!(*t, first, "{tag} frame {n}");
    }
}

const FULL_709: [&str; 8] = [
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-color_range",
    "pc",
];

const SMALL: &str = "352x240";
const HEVC_SMALL: &str = "640x360";

#[test]
fn a_1080p_h264_cut_on_va_api_opens_its_sessions_once_and_keeps_the_sources_tags() {
    if !ready(Codec::H264, "libx264") {
        return;
    }
    let look = Look {
        size: "1920x1080",
        sar: "1",
        color: &BT709_TV,
    };
    smart_cut_on_va(&h264_source(&look), Codec::H264, "h264");
}

#[test]
fn a_1080p_hevc_cut_on_va_api_opens_its_sessions_once_and_keeps_the_sources_tags() {
    if !ready(Codec::Hevc, "libx265") {
        return;
    }
    let look = Look {
        size: "1920x1080",
        sar: "4/3",
        color: &BT709_TV,
    };
    smart_cut_on_va(&hevc_source(&look), Codec::Hevc, "hevc");
}

#[test]
fn an_h264_cut_of_an_untagged_source_with_no_aspect_leaves_its_edges_untagged() {
    if !ready(Codec::H264, "libx264") {
        return;
    }
    let look = Look {
        size: SMALL,
        sar: "0",
        color: &[],
    };
    tags_on_va(&h264_source(&look), Codec::H264, "h264-untagged");
}

#[test]
fn an_h264_cut_of_an_anamorphic_full_range_source_keeps_its_aspect_and_range() {
    if !ready(Codec::H264, "libx264") {
        return;
    }
    let look = Look {
        size: SMALL,
        sar: "8/9",
        color: &FULL_709,
    };
    tags_on_va(&h264_source(&look), Codec::H264, "h264-anamorphic-full");
}

#[test]
fn an_hevc_cut_of_an_untagged_source_with_no_aspect_leaves_its_edges_untagged() {
    if !ready(Codec::Hevc, "libx265") {
        return;
    }
    let look = Look {
        size: HEVC_SMALL,
        sar: "0",
        color: &[],
    };
    tags_on_va(&hevc_source(&look), Codec::Hevc, "hevc-untagged");
}

#[test]
fn an_hevc_cut_of_an_anamorphic_full_range_source_keeps_its_aspect_and_range() {
    if !ready(Codec::Hevc, "libx265") {
        return;
    }
    let look = Look {
        size: HEVC_SMALL,
        sar: "8/9",
        color: &FULL_709,
    };
    tags_on_va(&hevc_source(&look), Codec::Hevc, "hevc-anamorphic-full");
}

#[test]
fn an_export_with_no_pieces_is_refused() {
    let r = export(
        &dir().join("edges-empty.mp4"),
        &[],
        ExportOptions::default(),
        |_| true,
    );
    assert!(matches!(r, Err(Error::Invalid(_))));
}
