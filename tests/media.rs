#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter"
))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use pito_codecs::media::{
    Codec, ExportOptions, Piece, Prefs, Rgba, audio_pcm, decode_video, export, first_frame,
    frames_at, probe,
};

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        let out = dir().join("two-tracks-bt709.mp4");
        if out.exists() {
            return out;
        }
        let part = dir().join("two-tracks-bt709.part.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc2=s=1280x720:r=60:d=6"])
            .args(["-f", "lavfi", "-i", "sine=f=440:r=48000:d=6"])
            .args(["-f", "lavfi", "-i", "sine=f=880:r=44100:d=6"])
            .args(["-map", "0:v", "-map", "1:a", "-map", "2:a"])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-profile:v",
                "high",
            ])
            .args([
                "-bf",
                "2",
                "-g",
                "60",
                "-keyint_min",
                "60",
                "-sc_threshold",
                "0",
            ])
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-ac:a:1", "1"])
            .args(["-vf", "scale=out_color_matrix=bt709:out_range=tv"])
            .args([
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
            ])
            .args([
                "-metadata:s:a:0",
                "title=Game",
                "-metadata:s:a:1",
                "title=Mic",
            ])
            .arg(&part)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        std::fs::rename(&part, &out).unwrap();
        out
    })
}

fn ffprobe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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

fn lit(img: &Rgba) -> bool {
    img.data.chunks_exact(4).any(|p| p[0] > 40 || p[1] > 40)
}

#[test]
fn the_ffmpeg_route_probes_shows_and_hears_a_file() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg on PATH: skipped");
        return;
    }
    let prefs = Prefs::ffmpeg();
    let p = probe(clip(), &prefs).unwrap();
    assert_eq!((p.width, p.height, p.fps), (1280, 720, (60, 1)));
    assert_eq!(p.video_codec, Some(Codec::H264));
    assert!((p.duration - 6.0).abs() < 0.05, "{}", p.duration);
    let titles: Vec<_> = p.audio.iter().map(|a| a.title.clone()).collect();
    assert_eq!(titles, [Some("Game".into()), Some("Mic".into())]);
    assert_eq!((p.audio[1].index, p.audio[1].channels), (1, 1));

    let still = first_frame(clip(), 240, &prefs).unwrap();
    assert_eq!((still.width, still.height), (426, 240));
    assert!(lit(&still));
    let strip = frames_at(clip(), &[0.5, 2.0, 4.0, 99.0], 120, &prefs).unwrap();
    assert_eq!(strip.len(), 4);
    assert!(strip.iter().all(|s| s.height == 120 && lit(s)));

    let pcm = audio_pcm(clip(), 1, 16_000, true, &prefs).unwrap();
    assert_eq!(pcm.channels, 1);
    assert!(
        (pcm.frames() as i64 - 96_000).abs() < 1_600,
        "{}",
        pcm.frames()
    );
    assert!(pcm.samples.iter().any(|s| s.abs() > 0.1));

    let frames: Vec<_> = decode_video(clip(), 3.0, Some(360), &prefs)
        .unwrap()
        .take(5)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(frames.len(), 5);
    assert_eq!((frames[0].width, frames[0].pts), (640, 180));
}

fn min_psnr(out: &Path, src: &Path, ranges: &[(i64, i64)]) -> f64 {
    let mut graph = format!("[1:v]split={}", ranges.len());
    for i in 0..ranges.len() {
        graph.push_str(&format!("[s{i}]"));
    }
    graph.push(';');
    let mut joined = String::new();
    for (i, (a, b)) in ranges.iter().enumerate() {
        graph.push_str(&format!(
            "[s{i}]trim=start_frame={a}:end_frame={b},setpts=PTS-STARTPTS[p{i}];"
        ));
        joined.push_str(&format!("[p{i}]"));
    }
    let log = out.with_extension("psnr");
    graph.push_str(&format!(
        "{joined}concat=n={}:v=1:a=0[ref];[0:v]setpts=PTS-STARTPTS[o];[o][ref]psnr=stats_file={}",
        ranges.len(),
        log.display()
    ));
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(out)
        .arg("-i")
        .arg(src)
        .args(["-filter_complex", &graph, "-f", "null", "-"])
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let text = std::fs::read_to_string(&log).unwrap();
    let total: i64 = ranges.iter().map(|(a, b)| b - a).sum();
    assert_eq!(text.lines().count() as i64, total);
    text.lines()
        .map(|l| {
            let v = l
                .split_whitespace()
                .nth(5)
                .unwrap()
                .split(':')
                .nth(1)
                .unwrap();
            if v == "inf" {
                999.0
            } else {
                v.parse().unwrap()
            }
        })
        .fold(f64::INFINITY, f64::min)
}

fn routes() -> [(&'static str, Prefs); 2] {
    [("ffmpeg", Prefs::ffmpeg()), ("ours", Prefs::default())]
}

#[test]
fn our_route_probes_shows_and_hears_without_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let p = probe(clip(), &prefs).unwrap();
    assert_eq!((p.width, p.height, p.fps), (1280, 720, (60, 1)));
    assert_eq!(p.video_codec, Some(Codec::H264));
    let titles: Vec<_> = p.audio.iter().map(|a| a.title.clone()).collect();
    assert_eq!(titles, [Some("Game".into()), Some("Mic".into())]);
    let ours = first_frame(clip(), 240, &prefs).unwrap();
    let theirs = first_frame(clip(), 240, &Prefs::ffmpeg()).unwrap();
    assert_eq!((ours.width, ours.height), (theirs.width, theirs.height));
    let diff = ours
        .data
        .iter()
        .zip(&theirs.data)
        .map(|(a, b)| a.abs_diff(*b) as u64)
        .sum::<u64>() as f64
        / ours.data.len() as f64;
    assert!(diff < 6.0, "{diff}");
    let strip = frames_at(clip(), &[0.5, 2.0, 4.0, 99.0], 120, &prefs).unwrap();
    assert!(strip.iter().all(|s| s.height == 120 && lit(s)));
    let pcm = audio_pcm(clip(), 0, 16_000, true, &prefs).unwrap();
    assert!(
        (pcm.frames() as i64 - 96_000).abs() < 1_600,
        "{}",
        pcm.frames()
    );
    let rms = (pcm.samples.iter().map(|s| s * s).sum::<f32>() / pcm.samples.len() as f32).sqrt();
    assert!(rms > 0.05, "{rms}");
    let frames: Vec<_> = decode_video(clip(), 3.0, None, &prefs)
        .unwrap()
        .take(3)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(frames[0].pts, 180);
}

#[test]
fn a_three_piece_export_with_a_repeat_smart_cuts_to_the_frame_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    for (name, prefs) in routes() {
        let out = dir().join(format!("smart-{name}.mp4"));
        let pieces = [(1.1, 3.4), (0.5, 1.5), (1.1, 3.4)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
            from,
            to,
        });
        let mut stages = 0;
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |_| {
                stages += 1;
                true
            },
        )
        .unwrap();
        assert!(done.smart && stages > 2, "{name}");
        assert_eq!(done.frames, 336, "{name}");
        let at_least = if name == "ours" { 180 } else { 120 };
        assert!(done.copied >= at_least, "{name}: {}", done.copied);
        assert!(decodes_clean(&out), "{name}");
        let psnr = min_psnr(&out, clip(), &[(66, 204), (30, 90), (66, 204)]);
        assert!(psnr > 40.0, "{name}: {psnr}");
        let audio: f64 = ffprobe(
            &out,
            &[
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=duration",
                "-of",
                "csv=p=0",
            ],
        )
        .parse()
        .unwrap();
        assert!((audio - 5.6).abs() < 0.05, "{name}: {audio}");
    }
}

#[test]
fn an_export_with_an_overlay_re_encodes_and_paints_every_frame_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    for (name, prefs) in routes() {
        let out = dir().join(format!("overlay-{name}.mp4"));
        let pieces = [(0.0, 1.0), (4.0, 4.5)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
            from,
            to,
        });
        let red = Arc::new({
            let mut l = Rgba::new(1280, 720);
            for px in l.data.chunks_exact_mut(4).take(1280 * 100) {
                px.copy_from_slice(&[255, 0, 0, 255]);
            }
            l
        });
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                video: Codec::Hevc,
                prefs,
                overlay: Some(Box::new(move |_, _, _| Some(red.clone()))),
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert!(!done.smart && done.encoded == 90, "{name}");
        let codec = ffprobe(
            &out,
            &[
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "csv=p=0",
            ],
        );
        if codec == "h264" {
            assert!(
                name == "ours" && done.reasons.iter().any(|r| r.contains("instead")),
                "{name}: {:?}",
                done.reasons
            );
        } else {
            assert_eq!(codec, "hevc", "{name}");
        }
        let top = first_frame(&out, 720, &Prefs::ffmpeg()).unwrap();
        let px = &top.data[(50 * 1280 + 640) * 4..(50 * 1280 + 640) * 4 + 3];
        assert!(px[0] > 220 && px[1] < 40 && px[2] < 40, "{name}: {px:?}");
        assert!(decodes_clean(&out), "{name}");
        assert!(min_psnr(&out, clip(), &[(0, 60), (240, 270)]) > 0.0);
    }
}

#[test]
fn adoption_through_every_available_route() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/sample/adoption.mp4");
    if !sample.exists() || !have_ffmpeg() {
        eprintln!("no tmp/sample/adoption.mp4: skipped");
        return;
    }
    for (name, prefs) in routes() {
        let p = probe(&sample, &prefs).unwrap();
        assert_eq!((p.width, p.height, p.fps), (2560, 1440, (60, 1)));
        assert_eq!(p.audio[0].title.as_deref(), Some("Gameplay"));
        assert!(lit(&first_frame(&sample, 240, &prefs).unwrap()));
        let out = dir().join(format!("adoption-{name}.mp4"));
        let pieces = [(4.0, 4.5), (8.0, 8.4), (4.0, 4.5)].map(|(from, to)| Piece {
            path: sample.clone(),
            from,
            to,
        });
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(done.frames, 30 + 24 + 30, "{name}");
        assert!(decodes_clean(&out), "{name}");
        let psnr = min_psnr(&out, &sample, &[(240, 270), (480, 504), (240, 270)]);
        assert!(psnr > 40.0, "{name}: {psnr}");
    }
}
