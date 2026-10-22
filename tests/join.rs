#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]

use std::path::{Path, PathBuf};
use std::process::Command;

use pito_codecs::media::{JoinMethod, JoinOptions, Prefs, join, probe};

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg(out: &Path, args: &[&str]) -> bool {
    if out.exists() {
        return true;
    }
    let part = dir().join(format!(
        "{}.{}",
        std::process::id(),
        out.file_name().unwrap().to_str().unwrap()
    ));
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&part)
        .status()
        .is_ok_and(|s| s.success());
    if made && std::fs::rename(&part, out).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    made
}

fn output(args: &[&str], path: &Path, tail: &[&str]) -> Vec<u8> {
    let out = Command::new(args[0])
        .args(&args[1..])
        .arg(path)
        .args(tail)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn seconds(path: &Path) -> f64 {
    let text = output(
        &[
            "ffprobe",
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ],
        path,
        &[],
    );
    String::from_utf8_lossy(&text).trim().parse().unwrap()
}

fn video_samples(path: &Path) -> Vec<Vec<u8>> {
    let r = pito_codecs::mp4::Reader::open(path).unwrap();
    let v = r.video().unwrap();
    (0..r.tracks[v].samples.len())
        .map(|i| {
            let mut d = Vec::new();
            r.read_into(v, i, &mut d).unwrap();
            d
        })
        .collect()
}

#[test]
fn a_join_copies_matching_clips_and_letterboxes_the_rest_with_silence() {
    let part = dir().join("join-part.mp4");
    let tall = dir().join("join-tall.mp4");
    let made = ffmpeg(
        &part,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=48000:d=2",
            "-ac",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ],
    ) && ffmpeg(
        &tall,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=360x640:r=30:d=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    if !made {
        return;
    }
    let copied = dir().join("join-copied.mp4");
    let three = [part.clone(), part.clone(), part.clone()];
    let j = join(&three, &copied, JoinOptions::default(), |_| true).unwrap();
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    let one = video_samples(&part);
    let all = video_samples(&copied);
    assert_eq!(all.len(), one.len() * 3);
    assert!(all.chunks(one.len()).all(|c| c == one.as_slice()));
    let (whole, single) = (seconds(&copied), seconds(&part));
    assert!((whole - 3.0 * single).abs() < 0.05, "{whole} vs {single}");

    let mixed = dir().join("join-mixed.mp4");
    let j = join(&[part, tall], &mixed, JoinOptions::default(), |_| true).unwrap();
    assert_eq!(j.method, JoinMethod::Encode);
    assert!(j.reason.contains("is 360x640, not 640x360"), "{}", j.reason);
    assert_eq!((j.size, j.fps), ((640, 360), (30, 1)));
    let gray = output(
        &["ffmpeg", "-v", "error", "-ss", "2.5", "-i"],
        &mixed,
        &["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"],
    );
    assert_eq!(gray.len(), 640 * 360);
    let column = |x: usize| (0..360).map(|y| gray[y * 640 + x] as f64).sum::<f64>() / 360.0;
    assert!(column(10) < 20.0 && column(630) < 20.0);
    assert!((230..410).map(column).sum::<f64>() / 180.0 > 40.0);
    let pcm: Vec<f32> = output(
        &["ffmpeg", "-v", "error", "-i"],
        &mixed,
        &[
            "-map", "0:a:0", "-ac", "2", "-ar", "48000", "-f", "f32le", "-",
        ],
    )
    .chunks_exact(4)
    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
    .collect();
    assert!(
        (pcm.len() as f64 / 96_000.0 - 3.0).abs() < 0.05,
        "{}",
        pcm.len()
    );
    let loud = |a: f64, b: f64| {
        pcm[(a * 96_000.0) as usize..(b * 96_000.0) as usize]
            .iter()
            .fold(0.0f32, |m, v| m.max(v.abs()))
    };
    assert!(loud(0.5, 1.5) > 0.05);
    assert!(loud(2.2, 2.9) < 0.001);
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

fn h264_clip(name: &str, pix: &str, profile: &str) -> Option<PathBuf> {
    let path = dir().join(name);
    ffmpeg(
        &path,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=352x240:r=30:d=1",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            pix,
            "-profile:v",
            profile,
        ],
    )
    .then_some(path)
}

#[test]
fn a_10_bit_4_2_2_h264_joins_by_copy_and_is_refused_by_name_when_it_must_be_re_encoded() {
    if !has_encoder("libx264") {
        eprintln!("no ffmpeg with libx264 on PATH: skipped");
        return;
    }
    let (Some(deep), Some(plain)) = (
        h264_clip("join-h264-422-10.mp4", "yuv422p10le", "high422"),
        h264_clip("join-h264-420-8.mp4", "yuv420p", "high"),
    ) else {
        eprintln!("ffmpeg could not make the clips: skipped");
        return;
    };
    let prefs = Prefs::default();
    let copied = dir().join("join-h264-422-10-copied.mp4");
    let j = join(
        &[deep.clone(), deep.clone()],
        &copied,
        JoinOptions::default(),
        |_| true,
    )
    .unwrap();
    assert_eq!(j.method, JoinMethod::Copy, "{}", j.reason);
    let one = video_samples(&deep);
    let all = video_samples(&copied);
    assert_eq!(all.len(), one.len() * 2);
    assert!(all.chunks(one.len()).all(|c| c == one.as_slice()));
    let p = probe(&copied, &prefs).unwrap();
    assert_eq!(p.video_variant(), "10-bit 4:2:2 H.264");
    assert!(!p.video_native());

    let name = deep.display().to_string();
    for (inputs, opts) in [
        ([plain.clone(), deep.clone()], JoinOptions::default()),
        (
            [deep.clone(), deep.clone()],
            JoinOptions {
                encode: true,
                ..JoinOptions::default()
            },
        ),
    ] {
        let out = dir().join("join-h264-422-10-refused.mp4");
        let _ = std::fs::remove_file(&out);
        let e = join(&inputs, &out, opts, |_| true).unwrap_err().to_string();
        assert_eq!(
            e,
            format!("unsupported: {name}: 10-bit 4:2:2 H.264 video is not supported")
        );
        assert!(
            !e.replace(env!("CARGO_MANIFEST_DIR"), "")
                .to_lowercase()
                .contains("ffmpeg"),
            "{e}"
        );
        assert!(!out.exists());
    }
}
