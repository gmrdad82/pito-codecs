use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-mp3");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .any(|w| w == name)
        })
}

fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn floats(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn signal(rate: u32) -> String {
    format!(
        "aevalsrc=exprs='0.25*sin(2*PI*(300+200*t)*t)+0.2*sin(2*PI*3100*t)*lt(mod(t\\,0.37)\\,0.03)+0.1*(random(0)-0.5)|0.25*sin(2*PI*523*t)+0.15*sin(2*PI*7000*t)*lt(mod(t\\,0.29)\\,0.02)+0.1*(random(1)-0.5)':s={rate}:d=3"
    )
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn decode_stream(path: &Path) -> Vec<f32> {
    let data = std::fs::read(path).unwrap();
    let mut dec = Decoder::new();
    let mut pcm = vec![0.0; MAX_SAMPLES_PER_FRAME];
    let mut out = Vec::new();
    let mut rest = &data[..];
    while !rest.is_empty() {
        let f = dec.decode(rest, &mut pcm);
        if f.frame_bytes == 0 {
            break;
        }
        out.extend_from_slice(&pcm[..f.samples * f.channels as usize]);
        rest = &rest[f.frame_bytes..];
    }
    out
}

#[test]
fn decode_matches_ffmpeg_mp3float() {
    if !has_encoder("libmp3lame") {
        return;
    }
    let cases: [(&str, u32, &[&str]); 4] = [
        ("joint44", 44100, &["-b:a", "192k"]),
        ("mono48vbr", 48000, &["-ac", "1", "-q:a", "4"]),
        ("stereo32", 32000, &["-b:a", "128k", "-joint_stereo", "0"]),
        ("lsf22", 22050, &["-b:a", "48k"]),
    ];
    for (name, rate, extra) in cases {
        let mp3 = dir().join(format!("{name}.mp3"));
        let mp3s = mp3.to_str().unwrap();
        let src = signal(rate);
        let mut args = vec!["-f", "lavfi", "-i", &src, "-c:a", "libmp3lame"];
        args.extend_from_slice(extra);
        args.extend(["-write_xing", "0", mp3s]);
        ffmpeg(&args);
        let want = floats(&ffmpeg(&[
            "-c:a", "mp3float", "-i", mp3s, "-f", "f32le", "-",
        ]));
        let got = decode_stream(&mp3);
        assert_eq!(got.len(), want.len(), "{name}");
        let d = max_diff(&got, &want);
        assert!(d < 2e-5, "{name}: max difference {d}");
    }
}

#[test]
fn a_file_is_trimmed_by_its_lame_tag_like_ffmpeg() {
    if !has_encoder("libmp3lame") {
        return;
    }
    for (name, rate, extra) in [
        ("gapless44", 44100u32, ["-b:a", "192k"]),
        ("gapless48mono", 48000, ["-ac", "1"]),
    ] {
        let mp3 = dir().join(format!("{name}.mp3"));
        let mp3s = mp3.to_str().unwrap();
        let src = signal(rate);
        let mut args = vec!["-f", "lavfi", "-i", &src, "-c:a", "libmp3lame"];
        args.extend_from_slice(&extra);
        args.extend(["-id3v2_version", "3", mp3s]);
        ffmpeg(&args);
        let want = floats(&ffmpeg(&[
            "-c:a", "mp3float", "-i", mp3s, "-f", "f32le", "-",
        ]));
        let got = decode_file(&std::fs::read(&mp3).unwrap()).unwrap();
        assert_eq!(got.rate, rate, "{name}");
        assert_eq!(got.samples.len(), want.len(), "{name}");
        let d = max_diff(&got.samples, &want);
        assert!(d < 2e-5, "{name}: max difference {d}");
    }
}

#[cfg(any(
    feature = "audio",
    all(
        feature = "h264",
        feature = "hevc",
        feature = "aac",
        feature = "mp4",
        feature = "gpu",
        feature = "filter",
        feature = "mkv",
        feature = "vp9"
    )
))]
#[test]
fn range_starts_on_the_right_sample_after_the_encoder_delay() {
    if !has_encoder("libmp3lame") || !has_encoder("libx264") {
        return;
    }
    let src = signal(44100);
    for ext in ["mp4", "mkv"] {
        let file = dir().join(format!("delay.{ext}"));
        let fs = file.to_str().unwrap();
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=64x64:r=10:d=3",
            "-f",
            "lavfi",
            "-i",
            &src,
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "160k",
            fs,
        ]);
        let want = floats(&ffmpeg(&["-i", fs, "-f", "f32le", "-"]));
        let prefs = crate::media::Prefs::default();
        let (from, to) = (1.25, 2.5);
        let got = crate::media::audio_pcm_range(&file, 0, from, to, 44100, 2, &prefs).unwrap();
        let start = (from * 44100.0f64).round() as usize * 2;
        let end = (to * 44100.0f64).round() as usize * 2;
        assert_eq!(got.samples.len(), end - start, "{ext}");
        let d = max_diff(&got.samples, &want[start..end]);
        assert!(d < 2e-5, "{ext}: max difference {d}");
    }
}
