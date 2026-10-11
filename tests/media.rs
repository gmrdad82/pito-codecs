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
use std::sync::{Arc, OnceLock};

use pito_codecs::media::{
    AudioBuffer, Choice, Codec, ExportOptions, Frame, IN_FLIGHT, Piece, Prefs, Rgba, VideoFrame,
    audio_mix, audio_pcm, audio_pcm_range, decode_video, decode_video_gpu, export, first_frame,
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
        let part = dir().join(format!("two-tracks-bt709.{}.part.mp4", std::process::id()));
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
        if let Err(e) = std::fs::rename(&part, &out) {
            assert!(std::path::Path::new(&out).exists(), "{e}");
            let _ = std::fs::remove_file(&part);
        }
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
    let name = log.file_name().unwrap().to_str().unwrap();
    graph.push_str(&format!(
        "{joined}concat=n={}:v=1:a=0[ref];[0:v]setpts=PTS-STARTPTS[o];[o][ref]psnr=stats_file={name}",
        ranges.len(),
    ));
    let ok = Command::new("ffmpeg")
        .current_dir(log.parent().unwrap())
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

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn ranged_audio_lines_up_with_the_whole_track_and_mixes_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    for (name, prefs) in routes() {
        for (track, channels, limit) in [(0usize, 2u16, 1e-3f32), (1, 1, 5e-3)] {
            let whole =
                audio_pcm_range(clip(), track, 0.0, f64::INFINITY, 48_000, channels, &prefs)
                    .unwrap();
            let ch = channels as usize;
            let mut joined = Vec::new();
            for (a, b, frames) in [(1.0, 1.5, 24_000), (1.5, 2.25, 36_000), (3.5, 4.5, 48_000)] {
                let part = audio_pcm_range(clip(), track, a, b, 48_000, channels, &prefs).unwrap();
                assert_eq!(
                    (part.frames(), part.channels),
                    (frames, channels),
                    "{name} {track}"
                );
                joined.extend(part.samples);
            }
            let slice = [
                &whole.samples[48_000 * ch..108_000 * ch],
                &whole.samples[168_000 * ch..216_000 * ch],
            ]
            .concat();
            let d = worst(&joined, &slice);
            println!("{name} track {track}: worst sample difference {d}");
            assert!(d < limit, "{name} track {track}: {d}");
        }
        let tail = audio_pcm_range(clip(), 0, 5.5, 9.0, 48_000, 2, &prefs).unwrap();
        assert_eq!(tail.frames(), 168_000, "{name}");
        assert!(tail.samples[48_000..].iter().all(|s| *s == 0.0), "{name}");

        let game = audio_pcm_range(clip(), 0, 2.0, 3.0, 44_100, 2, &prefs).unwrap();
        let alone = audio_mix(clip(), &[1.0, 0.0], 2.0, 3.0, 44_100, &prefs).unwrap();
        assert_eq!(alone.samples, game.samples, "{name}");
        let both: AudioBuffer = audio_mix(clip(), &[0.5, 2.0], 2.0, 3.0, 44_100, &prefs).unwrap();
        assert_eq!(
            (both.frames(), both.channels, both.rate),
            (44_100, 2, 44_100)
        );
        let rms = |b: &[f32]| (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
        assert!(rms(&both.samples) > 2.0 * rms(&game.samples), "{name}");
    }
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

fn luma_diff(a: &Frame, b: &Frame) -> f64 {
    let (pa, pb) = (&a.planes[0], &b.planes[0]);
    let mut sum = 0u64;
    for y in 0..pa.height {
        let ra = &pa.data[y * pa.stride..y * pa.stride + pa.width];
        let rb = &pb.data[y * pb.stride..y * pb.stride + pb.width];
        sum += ra
            .iter()
            .zip(rb)
            .map(|(x, y)| x.abs_diff(*y) as u64)
            .sum::<u64>();
    }
    sum as f64 / (pa.width * pa.height) as f64
}

#[test]
fn gpu_playback_keeps_frames_on_the_gpu_and_falls_back_to_the_cpu() {
    if !have_ffmpeg() {
        return;
    }
    let gpu = pito_codecs::gpu::available()
        .iter()
        .any(|&(c, d, _)| c == Codec::H264 && d == pito_codecs::gpu::Direction::Decode);
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    for (prefs, on_gpu) in [(Prefs::default(), gpu), (cpu.clone(), false)] {
        let mut video = decode_video_gpu(clip(), 3.0, Some((640, 360)), &prefs).unwrap();
        assert_eq!(video.on_gpu(), on_gpu, "{:?}", video.reasons);
        assert_eq!((video.color.matrix, video.color.full_range), (1, false));
        let mut held = Vec::new();
        let mut n = 0;
        while let Some(f) = video.next() {
            let f = f.unwrap();
            assert_eq!(
                (f.on_gpu(), f.size(), f.pts()),
                (on_gpu && cfg!(target_os = "linux"), (640, 360), 180 + n)
            );
            #[cfg(target_os = "linux")]
            if let VideoFrame::Gpu(g) = &f {
                let d = g.dmabuf(false).unwrap();
                assert_eq!((d.fourcc, d.crop), (0x3231_564e, (0, 0, 640, 360)));
            }
            n += 1;
            held.push(f);
            if held.len() == IN_FLIGHT {
                for f in held.drain(..) {
                    video.recycle(f);
                }
            }
        }
        assert_eq!(n, 180);
    }
    if !gpu {
        return;
    }
    let ours = decode_video_gpu(clip(), 1.0, None, &Prefs::default())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        ours.on_gpu(),
        cfg!(target_os = "linux"),
        "a native-size frame left the GPU"
    );
    let ours = match ours {
        VideoFrame::Gpu(g) => g.download().unwrap(),
        VideoFrame::Cpu(f) => f,
    };
    let cpu = decode_video(clip(), 1.0, None, &cpu)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        (ours.width, ours.height, ours.pts),
        (cpu.width, cpu.height, cpu.pts)
    );
    let diff = luma_diff(&ours, &cpu);
    assert!(diff < 1.0, "{diff}");
}

fn matroska(name: &str, codec: &[&str]) -> PathBuf {
    let out = dir().join(name);
    if out.exists() {
        return out;
    }
    let part = dir().join(format!("{}.{name}", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=3"])
        .args(["-pix_fmt", "yuv420p", "-g", "30"])
        .args(codec)
        .arg(&part)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    if std::fs::rename(&part, &out).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    out
}

#[test]
fn matroska_and_webm_play_through_the_media_front() {
    if !have_ffmpeg() {
        return;
    }
    let files = [
        (
            matroska("h264.mkv", &["-c:v", "libx264", "-bf", "2"]),
            Codec::H264,
        ),
        (
            matroska(
                "vp9.webm",
                &[
                    "-c:v",
                    "libvpx-vp9",
                    "-b:v",
                    "1M",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                ],
            ),
            Codec::Vp9,
        ),
    ];
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    for (path, codec) in files {
        let p = probe(&path, &cpu).unwrap();
        assert_eq!(
            (p.video_codec, p.width, p.height, p.fps),
            (Some(codec), 640, 360, (30, 1))
        );
        let gpu = pito_codecs::gpu::available().iter().any(|&(c, d, b)| {
            c == codec
                && d == pito_codecs::gpu::Direction::Decode
                && b == pito_codecs::gpu::Backend::VaApi
        });
        let mut firsts = Vec::new();
        for (prefs, on_gpu) in [(Prefs::default(), gpu), (cpu.clone(), false)] {
            let video = decode_video_gpu(&path, 1.0, None, &prefs).unwrap();
            let mut n = 0;
            for f in video {
                let f = f.unwrap();
                assert_eq!(
                    (f.on_gpu(), f.size(), f.pts()),
                    (on_gpu, (640, 360), 30 + n),
                    "{codec}"
                );
                if n == 0 {
                    firsts.push(match f {
                        VideoFrame::Gpu(g) => g.download().unwrap(),
                        VideoFrame::Cpu(c) => c,
                    });
                }
                n += 1;
            }
            assert_eq!(n, 60, "{codec}");
        }
        let diff = luma_diff(&firsts[0], &firsts[1]);
        assert!(diff < 1.0, "{codec}: {diff}");
    }
}
