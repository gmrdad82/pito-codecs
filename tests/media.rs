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
    AudioBuffer, Canvas, Choice, Codec, ExportOptions, Frame, IN_FLIGHT, Piece, PixelFormat, Prefs,
    Progress, Rgba, Seek, Stills, TimeBase, VideoFrame, audio_mix, audio_mix_stream, audio_pcm,
    audio_pcm_range, audio_stream, decode_video, decode_video_gpu, export, export_frames,
    export_with_audio, first_frame, frame_count, frames_at, frames_at_seek, keyframes, probe,
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
        "{joined}concat=n={}:v=1:a=0,settb=1,setpts=N[ref];[0:v]settb=1,setpts=N[o];[o][ref]psnr=stats_file={name}",
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
        let whole = audio_pcm(clip(), 0, 48_000, false, &prefs).unwrap();
        assert_eq!(tail.frames(), whole.frames() - 264_000, "{name}");

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
        header_durations_match_the_samples(&out, name);
    }
}

fn children(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let (head, size) = if size == 1 {
            (
                16,
                u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize,
            )
        } else {
            (8, size)
        };
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        out.push((kind, &data[at + head..at + size]));
        at += size;
    }
    out
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(data)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, d)| d)
}

fn be(d: &[u8], at: usize, wide: bool) -> u64 {
    if wide {
        u64::from_be_bytes(d[at..at + 8].try_into().unwrap())
    } else {
        u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as u64
    }
}

fn times(d: &[u8]) -> (u64, u64) {
    let v1 = d[0] == 1;
    let at = if v1 { 20 } else { 12 };
    (be(d, at, false), be(d, at + 4, v1))
}

fn header_durations_match_the_samples(path: &Path, name: &str) {
    let file = std::fs::read(path).unwrap();
    let moov = child(&file, b"moov").unwrap();
    let (movie_ts, movie) = times(child(moov, b"mvhd").unwrap());
    let mut longest = 0.0f64;
    for (kind, trak) in children(moov) {
        if &kind != b"trak" {
            continue;
        }
        let tkhd = child(trak, b"tkhd").unwrap();
        let track =
            be(tkhd, if tkhd[0] == 1 { 28 } else { 20 }, tkhd[0] == 1) as f64 / movie_ts as f64;
        let mdia = child(trak, b"mdia").unwrap();
        let (ts, media) = times(child(mdia, b"mdhd").unwrap());
        let video = &child(mdia, b"hdlr").unwrap()[8..12] == b"vide";
        let stbl = child(child(mdia, b"minf").unwrap(), b"stbl").unwrap();
        let stts = child(stbl, b"stts").unwrap();
        let runs: Vec<(u64, u64)> = (0..be(stts, 4, false) as usize)
            .map(|i| (be(stts, 8 + i * 8, false), be(stts, 12 + i * 8, false)))
            .collect();
        let frames: u64 = runs.iter().map(|r| r.0).sum();
        let ticks: u64 = runs.iter().map(|r| r.0 * r.1).sum();
        let step = runs.iter().max_by_key(|r| r.0).unwrap().1;
        let start = child(trak, b"edts")
            .and_then(|e| child(e, b"elst"))
            .map(|l| {
                let v1 = l[0] == 1;
                (be(l, 8, v1), be(l, if v1 { 16 } else { 12 }, v1))
            });
        let media_time = start.map_or(0, |s| s.1);
        let shown = if video {
            frames * step
        } else {
            ticks - media_time
        };
        let kind = if video { "video" } else { "audio" };
        if name != "ffmpeg" {
            assert_eq!(media, shown + media_time, "{name}: mdhd of a {kind} track");
            assert_eq!(ticks, media, "{name}: stts of a {kind} track");
        }
        let shown = shown as f64 / ts as f64;
        if let Some((segment, _)) = start {
            let segment = segment as f64 / movie_ts as f64;
            assert!(
                (segment - shown).abs() < 1e-3,
                "{name}: elst of a {kind} track {segment} against {shown}"
            );
        }
        assert!(
            (track - shown).abs() < 1e-3,
            "{name}: tkhd of a {kind} track {track} against {shown}"
        );
        longest = longest.max(track);
    }
    let movie = movie as f64 / movie_ts as f64;
    assert!(
        (movie - longest).abs() < 1e-3,
        "{name}: mvhd {movie} against {longest}"
    );
}

#[test]
fn an_export_ending_on_a_copied_gop_lasts_to_its_last_frame_in_the_headers() {
    if !have_ffmpeg() {
        return;
    }
    for (name, prefs) in routes() {
        let out = dir().join(format!("headers-{name}.mp4"));
        let pieces = [(1.1, 2.0), (2.5, 4.0)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
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
        assert_eq!(done.frames, 144, "{name}");
        header_durations_match_the_samples(&out, name);
    }
}

fn steady(told: &[f32], name: &str) {
    assert_eq!(told.last(), Some(&1.0), "{name}");
    let mut last = 0.0f32;
    for &d in told {
        assert!(d + 1e-6 >= last, "{name}: progress fell from {last} to {d}");
        assert!(
            d - last <= 0.06,
            "{name}: progress jumped from {last} to {d}"
        );
        last = d;
    }
}

fn picture(n: u64, size: (u32, u32), spent: Option<Frame>) -> Frame {
    let mut f = spent.unwrap_or_else(|| {
        Frame::new(
            PixelFormat::Yuv420p8,
            size.0,
            size.1,
            0,
            TimeBase::new(1, 30),
        )
    });
    let (w, h) = (f.width as usize, f.height as usize);
    let stride = f.planes[0].stride;
    for y in 0..h {
        for (x, px) in f.planes[0].data[y * stride..y * stride + w]
            .iter_mut()
            .enumerate()
        {
            *px = (16 + (x + y + n as usize * 3) % 200) as u8;
        }
    }
    for p in &mut f.planes[1..] {
        p.data.fill(128);
    }
    f
}

#[test]
fn caller_made_frames_export_at_their_size_with_sample_exact_audio() {
    if !have_ffmpeg() {
        return;
    }
    let size = (360, 640);
    let fps = (30_000, 1_001);
    let count = 299u64;
    let total = 478_878usize;
    let out = dir().join("frames-360x640.mp4");
    let mut asked = Vec::new();
    let mut told = Vec::new();
    let mut at = 0usize;
    let canvas = Canvas {
        width: size.0,
        height: size.1,
        fps,
        frames: Some(count),
    };
    let done = export_frames(
        &out,
        canvas,
        ExportOptions::default(),
        |n, spent| Ok((n < count).then(|| VideoFrame::Cpu(picture(n, size, spent)))),
        |upto, frames| {
            asked.push((upto, frames));
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = (at..at + frames).flat_map(|k| [tone(k), tone(k)]).collect();
            at += frames;
            Ok(buf)
        },
        |p: &Progress| {
            told.push(p.done);
            true
        },
    )
    .unwrap();
    assert_eq!(done.frames, count);
    steady(&told, "frames");
    assert_eq!(asked.last().map(|a| a.0), Some(count));
    assert_eq!(asked.iter().map(|a| a.1).sum::<usize>(), total);
    assert!(asked.windows(2).all(|w| w[0].0 < w[1].0));
    let shape = ffprobe(
        &out,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,nb_read_frames",
            "-of",
            "csv=p=0",
        ],
    );
    assert_eq!(shape, "360,640,299");
    assert!(decodes_clean(&out));
    let pcm = ffmpeg_f32(&out);
    assert_eq!(pcm.len(), total * 2);
    let want: Vec<f32> = (0..total).flat_map(|k| [tone(k), tone(k)]).collect();
    let span = 96_000 * 2..100_096 * 2;
    assert!(worst(&pcm[span.clone()], &want[span]) < 0.01);
    header_durations_match_the_samples(&out, "frames");
    let wrong = export_frames(
        &out,
        canvas,
        ExportOptions::default(),
        |n, _| {
            let s = if n == 7 { (352, 240) } else { size };
            Ok((n < 20).then(|| VideoFrame::Cpu(picture(n, s, None))))
        },
        |_, frames| {
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = vec![0.0; frames * 2];
            Ok(buf)
        },
        |_| true,
    )
    .unwrap_err();
    assert!(wrong.to_string().contains("frame 7"), "{wrong}");
}

fn tone(k: usize) -> f32 {
    (k as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.5
}

#[test]
fn a_premixed_export_keeps_its_tone_whole_and_sample_exact_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    let pieces = [(1.1, 3.4), (0.5, 1.5), (4.0, 4.52)].map(|(from, to)| Piece {
        path: clip().to_path_buf(),
        from,
        to,
    });
    let total = 229 * 800;
    for (name, prefs) in routes() {
        let out = dir().join(format!("premixed-{name}.mp4"));
        let mut asked = Vec::new();
        let mut at = 0usize;
        let done = export_with_audio(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |piece, frames| {
                asked.push((piece, frames));
                let mut buf = AudioBuffer::new(48_000, 2);
                buf.samples = (at..at + frames).flat_map(|k| [tone(k), tone(k)]).collect();
                at += frames;
                Ok(buf)
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(done.frames, 229, "{name}");
        assert_eq!(asked, [(0, 110_400), (1, 48_000), (2, 24_800)], "{name}");
        assert!(decodes_clean(&out), "{name}");
        let pcm = ffmpeg_f32(&out);
        assert_eq!(pcm.len(), total * 2, "{name}");
        let ours =
            audio_pcm_range(&out, 0, 0.0, f64::INFINITY, 48_000, 2, &Prefs::default()).unwrap();
        assert_eq!(ours.frames(), total, "{name}");
        let want: Vec<f32> = (0..total).flat_map(|k| [tone(k), tone(k)]).collect();
        for cut in [110_400usize, 158_400] {
            let span = (cut - 2_048) * 2..(cut + 2_048) * 2;
            let d = worst(&pcm[span.clone()], &want[span]);
            println!("{name}: worst sample difference around {cut}: {d}");
            assert!(d < 0.01, "{name} at {cut}: {d}");
        }
    }
    let bad = |frames: usize| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = vec![0.0; frames * 2];
        buf
    };
    let out = dir().join("premixed-bad.mp4");
    let short = export_with_audio(
        &out,
        &pieces,
        ExportOptions::default(),
        |piece, frames| Ok(bad(if piece == 1 { frames - 1 } else { frames })),
        |_| true,
    )
    .unwrap_err();
    assert!(short.to_string().contains("piece 1"), "{short}");
    let nan = export_with_audio(
        &out,
        &pieces,
        ExportOptions::default(),
        |piece, frames| {
            let mut buf = bad(frames);
            if piece == 2 {
                buf.samples[7] = f32::NAN;
            }
            Ok(buf)
        },
        |_| true,
    )
    .unwrap_err();
    assert!(nan.to_string().contains("piece 2"), "{nan}");
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

fn ffmpeg_f32(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn ffmpeg_make(out: &Path, args: &[&str]) {
    let part = dir().join(format!(
        "{}.{}",
        std::process::id(),
        out.file_name().unwrap().to_str().unwrap()
    ));
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&part)
        .status()
        .unwrap()
        .success();
    assert!(ok, "{}", out.display());
    if std::fs::rename(&part, out).is_err() {
        let _ = std::fs::remove_file(&part);
    }
}

#[test]
fn pcm_tracks_decode_bit_exact_without_ffmpeg() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let known: Vec<i16> = (0..72_000i32)
        .flat_map(|i| {
            let l = ((i as f64 * 0.0577).sin() * 30_000.0) as i16;
            let r = match i % 4800 {
                0 => i16::MIN,
                1 => i16::MAX,
                _ => (i.wrapping_mul(7919) % 65_536 - 32_768) as i16,
            };
            [l, r]
        })
        .collect();
    let raw = dir().join("known-s16le.raw");
    let bytes: Vec<u8> = known.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(&raw, bytes).unwrap();
    let obs = dir().join("h264-pcm-s16le.mkv");
    ffmpeg_make(
        &obs,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=30:d=1.5",
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
            raw.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "pcm_s16le",
        ],
    );
    let p = probe(&obs, &prefs).unwrap();
    assert_eq!(p.video_codec, Some(Codec::H264));
    assert_eq!(
        (p.audio[0].codec.as_str(), p.audio[0].native()),
        ("pcm", true)
    );
    assert!(p.unsupported().is_empty(), "{:?}", p.unsupported());
    let pcm = audio_pcm(&obs, 0, 48_000, false, &prefs).unwrap();
    let want: Vec<f32> = known.iter().map(|&s| s as f32 / 32768.0).collect();
    assert_eq!(pcm.samples.len(), want.len());
    assert!(pcm.samples == want, "s16le in MKV is not bit-exact");
    let mid = audio_pcm_range(&obs, 0, 0.5, 1.0, 48_000, 2, &prefs).unwrap();
    assert!(
        mid.samples == want[48_000..96_000],
        "a PCM range is not bit-exact"
    );
    let cases = [
        ("mkv", "pcm_s24le"),
        ("mkv", "pcm_s32be"),
        ("mkv", "pcm_f32le"),
        ("mkv", "pcm_f64le"),
        ("mkv", "pcm_u8"),
        ("mov", "pcm_s16le"),
        ("mov", "pcm_s16be"),
        ("mov", "pcm_s24le"),
        ("mov", "pcm_s32le"),
        ("mov", "pcm_f32be"),
        ("mov", "pcm_f64le"),
        ("mp4", "pcm_s16le"),
        ("mp4", "pcm_s24be"),
        ("mp4", "pcm_f32le"),
    ];
    for (ext, codec) in cases {
        let path = dir().join(format!("{codec}.{ext}"));
        ffmpeg_make(
            &path,
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:r=44100:d=1.2",
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=r=44100:d=1.2:a=0.5",
                "-filter_complex",
                "[0][1]amerge=inputs=2",
                "-c:a",
                codec,
            ],
        );
        let theirs = ffmpeg_f32(&path);
        let ours = audio_pcm(&path, 0, 44_100, false, &prefs).unwrap();
        assert_eq!(ours.samples.len(), theirs.len(), "{codec} in {ext}");
        assert!(ours.samples == theirs, "{codec} in {ext} is not bit-exact");
    }
}

#[test]
fn an_ac3_track_probes_and_decodes_natively() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let path = dir().join("h264-ac3-aac.mkv");
    ffmpeg_make(
        &path,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=30:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=48000:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=f=880:r=48000:d=1",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a:0",
            "ac3",
            "-c:a:1",
            "aac",
        ],
    );
    let p = probe(&path, &prefs).unwrap();
    let names: Vec<_> = p
        .audio
        .iter()
        .map(|a| (a.codec.as_str(), a.native()))
        .collect();
    assert_eq!(names, [("ac3", true), ("aac", true)]);
    assert!(p.video_native());
    assert!(p.unsupported().is_empty());
    let ac3 = audio_pcm(&path, 0, 48_000, false, &prefs).unwrap();
    assert!(
        (ac3.frames() as i64 - 48_000).abs() < 1_600,
        "{}",
        ac3.frames()
    );
    let aac = audio_pcm(&path, 1, 48_000, false, &prefs).unwrap();
    assert!(
        (aac.frames() as i64 - 48_000).abs() < 1_200,
        "{}",
        aac.frames()
    );
}

#[test]
fn aac_and_opus_in_matroska_line_up_with_ffmpeg_to_the_sample() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    for (name, codec) in [("aac.mkv", "aac"), ("opus.webm", "libopus")] {
        let path = dir().join(format!("tones-{name}"));
        ffmpeg_make(
            &path,
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:r=48000:d=9",
                "-f",
                "lavfi",
                "-i",
                "sine=f=1250:r=48000:d=9",
                "-filter_complex",
                "[0][1]amerge=inputs=2",
                "-c:a",
                codec,
                "-b:a",
                "256k",
            ],
        );
        let theirs = ffmpeg_f32(&path);
        let ours = audio_pcm(&path, 0, 48_000, false, &prefs).unwrap().samples;
        let whole = worst(&ours[20_000..800_000], &theirs[20_000..800_000]);
        assert!(whole < 0.01, "{name}: {whole}");
        let range = audio_pcm_range(&path, 0, 2.93, 6.0, 48_000, 2, &prefs).unwrap();
        let part = worst(&range.samples, &theirs[281_280..576_000]);
        assert!(part < 0.01, "{name}: {part}");
    }
}

fn recording() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let out = dir().join("recording-h264-pcm.mkv");
        if !out.exists() {
            ffmpeg_make(
                &out,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=320x240:r=30:d=180",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=f=440:r=48000:d=180",
                    "-f",
                    "lavfi",
                    "-i",
                    "anoisesrc=r=48000:d=180:a=0.3:seed=7",
                    "-filter_complex",
                    "[1][2]amerge=inputs=2[a]",
                    "-map",
                    "0:v",
                    "-map",
                    "[a]",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-g",
                    "60",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "pcm_s16le",
                ],
            );
        }
        out
    })
}

fn without_cues(src: &Path, name: &str) -> PathBuf {
    let out = dir().join(name);
    if out.exists() {
        return out;
    }
    let part = dir().join(format!("{}.{name}", std::process::id()));
    let file = std::fs::File::create(&part).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args(["-map", "0", "-c", "copy", "-f", "matroska", "pipe:1"])
        .stdout(file)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    if std::fs::rename(&part, &out).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    out
}

fn mean_diff(a: &Rgba, b: &Rgba) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum::<u64>() as f64
        / a.data.len() as f64
}

fn streamed(path: &Path, from: f64, rate: u32, channels: u16) -> Vec<f32> {
    let mut all = Vec::new();
    let stream = audio_stream(path, 0, from, rate, channels, &Prefs::default()).unwrap();
    let blocks: Vec<AudioBuffer> = stream.collect::<Result<_, _>>().unwrap();
    let (last, full) = blocks.split_last().unwrap();
    assert!(full.iter().all(|b| b.frames() == rate as usize));
    assert!(last.frames() <= rate as usize && last.frames() > 0);
    for b in &blocks {
        all.extend_from_slice(&b.samples);
    }
    all
}

#[test]
fn an_mkv_still_late_in_a_recording_comes_from_the_cues_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    let path = recording();
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let theirs = frames_at(path, &[170.5], 120, &Prefs::ffmpeg()).unwrap();
    let mut routes = Vec::new();
    for prefs in [cpu.clone(), Prefs::default()] {
        let mut ours = frames_at(path, &[170.5, 10.0, 171.9, 172.0, 999.0], 120, &prefs).unwrap();
        assert_eq!(ours.len(), 5);
        assert!(ours.iter().all(|s| s.height == 120 && lit(s)));
        assert!(mean_diff(&ours[0], &theirs[0]) < 6.0);
        assert!(ours[0].data == ours[2].data);
        assert!(ours[0].data != ours[1].data && ours[0].data != ours[3].data);
        routes.push(ours.swap_remove(0));
    }
    assert!(mean_diff(&routes[0], &routes[1]) < 1.0);
    let first = first_frame(path, 240, &cpu).unwrap();
    let theirs = first_frame(path, 240, &Prefs::ffmpeg()).unwrap();
    assert!(mean_diff(&first, &theirs) < 6.0);
}

#[test]
fn mkv_pcm_streams_bit_exact_against_the_whole_decode() {
    if !have_ffmpeg() {
        return;
    }
    let path = recording();
    let prefs = Prefs::default();
    let theirs = ffmpeg_f32(path);
    let whole = audio_pcm(path, 0, 48_000, false, &prefs).unwrap();
    assert!(whole.samples == theirs, "the whole decode is not bit-exact");
    let ours = streamed(path, 0.0, 48_000, 2);
    assert!(ours == theirs, "the stream from 0 is not bit-exact");
    let late = streamed(path, 150.25, 48_000, 2);
    assert!(
        late == theirs[150 * 96_000 + 24_000..],
        "a late stream is off"
    );
    let part = audio_pcm_range(path, 0, 100.0, 130.0, 48_000, 2, &prefs).unwrap();
    assert!(part.samples == theirs[9_600_000..12_480_000]);
    let mono = audio_pcm(path, 0, 16_000, true, &prefs).unwrap();
    assert!(streamed(path, 0.0, 16_000, 1) == mono.samples);
}

#[test]
fn a_cueless_or_cut_mkv_still_streams_and_shows_or_fails_cleanly() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let path = recording();
    let bare = without_cues(path, "recording-no-cues.mkv");
    let want = frames_at(path, &[120.0], 120, &prefs).unwrap();
    let got = frames_at(&bare, &[120.0], 120, &prefs).unwrap();
    assert!(mean_diff(&want[0], &got[0]) < 1.0);
    let theirs = ffmpeg_f32(path);
    assert!(streamed(&bare, 120.0, 48_000, 2) == theirs[120 * 96_000..]);
    let bytes = std::fs::read(path).unwrap();
    let cut = dir().join("recording-cut.mkv");
    std::fs::write(&cut, &bytes[..bytes.len() * 3 / 5]).unwrap();
    let early = frames_at(&cut, &[20.0], 120, &prefs).unwrap();
    assert!(lit(&early[0]));
    let _ = frames_at(&cut, &[170.0], 120, &prefs);
    let _ = first_frame(&cut, 120, &prefs);
    if let Ok(stream) = audio_stream(&cut, 0, 170.0, 48_000, 2, &prefs) {
        for b in stream {
            if b.is_err() {
                break;
            }
        }
    }
    let head: Vec<f32> = audio_stream(&cut, 0, 0.0, 48_000, 2, &prefs)
        .unwrap()
        .take(30)
        .flat_map(|b| b.unwrap().samples)
        .collect();
    assert!(head == theirs[..30 * 96_000]);
}

#[test]
fn a_range_past_the_end_returns_only_what_the_track_has() {
    if !have_ffmpeg() {
        return;
    }
    let path = recording();
    let theirs = ffmpeg_f32(path);
    let prefs = Prefs::default();
    let tail = audio_pcm_range(path, 0, 178.0, 190.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(tail.frames(), 96_000);
    assert!(tail.samples == theirs[178 * 96_000..]);
    let past = audio_pcm_range(path, 0, 200.0, 210.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(past.frames(), 0);
    let tail = audio_pcm_range(path, 0, 178.0, 190.0, 48_000, 2, &Prefs::ffmpeg()).unwrap();
    assert!(tail.frames() <= 96_000 && tail.frames() > 94_000);
    let whole = audio_pcm(clip(), 0, 48_000, false, &prefs).unwrap();
    let aac = audio_pcm_range(clip(), 0, 4.0, 16.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(aac.frames(), whole.frames() - 4 * 48_000);
}

#[test]
fn an_exact_still_inside_the_first_gop_of_a_fade_is_not_the_keyframe() {
    if !have_ffmpeg() {
        return;
    }
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    for name in ["fade-in.mp4", "fade-in.mkv"] {
        let path = dir().join(name);
        if !path.exists() {
            ffmpeg_make(
                &path,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=320x240:r=30:d=10,fade=in:0:90",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-bf",
                    "2",
                    "-g",
                    "120",
                    "-keyint_min",
                    "120",
                    "-sc_threshold",
                    "0",
                    "-pix_fmt",
                    "yuv420p",
                ],
            );
        }
        let theirs = frames_at_seek(&path, &[1.5], 120, Seek::Exact, &Prefs::ffmpeg()).unwrap();
        for prefs in [cpu.clone(), Prefs::default()] {
            let key = frames_at(&path, &[1.5], 120, &prefs).unwrap();
            assert!(!lit(&key[0]), "{name}: the keyframe at 0 is not black");
            let ours =
                frames_at_seek(&path, &[1.5, 0.5, 1.5, 6.0], 120, Seek::Exact, &prefs).unwrap();
            assert!(lit(&ours[0]) && lit(&ours[3]), "{name}");
            assert!(ours[0].data == ours[2].data && ours[0].data != ours[1].data);
            let d = mean_diff(&ours[0], &theirs[0]);
            assert!(d < 6.0, "{name}: {d}");
        }
    }
}

fn counted() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let out = dir().join("counted-426x240-h264-pcm.mkv");
        if !out.exists() {
            ffmpeg_make(
                &out,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=426x240:r=30:d=60,format=yuv420p,geq=lum='22+13*if(lt(X,142),mod(N,16),if(lt(X,284),mod(floor(N/16),16),mod(floor(N/256),16)))':cb=128:cr=128",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=f=440:r=48000:d=60",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "8",
                    "-bf",
                    "2",
                    "-g",
                    "60",
                    "-keyint_min",
                    "60",
                    "-sc_threshold",
                    "0",
                    "-c:a",
                    "pcm_s16le",
                    "-ac",
                    "2",
                ],
            );
        }
        out
    })
}

fn frame_numbers(path: &Path) -> Vec<i64> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-fps_mode", "passthrough"])
        .args(["-vf", "scale=3:1:flags=area", "-pix_fmt", "gray"])
        .args(["-f", "rawvideo", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let digit = |v: u8| {
        ((v as f64 * 219.0 / 255.0 - 6.0) / 13.0)
            .round()
            .clamp(0.0, 15.0) as i64
    };
    out.stdout
        .chunks_exact(3)
        .map(|p| digit(p[0]) + 16 * digit(p[1]) + 256 * digit(p[2]))
        .collect()
}

#[test]
fn three_hundred_short_cuts_join_without_drift_on_every_route() {
    if !have_ffmpeg() {
        return;
    }
    let mut seed = 0x2545_f491_u64;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let mut want: Vec<i64> = Vec::new();
    let pieces: Vec<Piece> = (0..300)
        .map(|k| {
            let len = if k % 9 == 4 {
                61 + next(80) as i64
            } else {
                2 + next(19) as i64
            };
            let a = next((1800 - len) as u64) as i64;
            want.extend(a..a + len);
            Piece {
                path: counted().to_path_buf(),
                from: a as f64 / 30.0,
                to: (a + len) as f64 / 30.0,
            }
        })
        .collect();
    let total = want.len() as u64;
    for (name, prefs) in routes() {
        let out = dir().join(format!("drift-{name}.mp4"));
        let mut told = Vec::new();
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |p| {
                told.push(p.done);
                true
            },
        )
        .unwrap();
        assert!(done.smart, "{name}: {:?}", done.reasons);
        steady(&told, name);
        assert_eq!(done.frames, total, "{name}");
        assert!(done.copied > 0 && done.encoded > 0, "{name}: {done:?}");
        let got = frame_numbers(&out);
        assert_eq!(got.len(), want.len(), "{name}");
        let wrong = got.iter().zip(&want).filter(|(g, w)| g != w).count();
        let at = got.iter().zip(&want).position(|(g, w)| g != w).unwrap_or(0);
        let near = at.saturating_sub(3)..(at + 4).min(want.len());
        assert_eq!(
            wrong,
            0,
            "{name}: {wrong} frames are not the source frame they should be, first at {at}: {:?} for {:?}",
            &got[near.clone()],
            &want[near]
        );
        assert_eq!(got.last(), want.last(), "{name}");
        let video: f64 = ffprobe(
            &out,
            &[
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=duration",
                "-of",
                "csv=p=0",
            ],
        )
        .parse()
        .unwrap();
        assert!(
            (video - total as f64 / 30.0).abs() < 1e-3,
            "{name}: {video} s of video for {total} frames"
        );
        let pcm = ffmpeg_f32(&out);
        assert_eq!(pcm.len() as u64, total * 1600 * 2, "{name}");
        let rms = (pcm.iter().map(|&s| s as f64 * s as f64).sum::<f64>() / pcm.len() as f64).sqrt();
        assert!((rms - 0.0625).abs() < 0.004, "{name}: {rms}");
    }
}

#[test]
fn an_mkv_probe_reads_the_head_and_cues_and_matches_the_full_index() {
    if !have_ffmpeg() {
        return;
    }
    let vfr = dir().join("dropped-frames-h264.mkv");
    if !vfr.exists() {
        ffmpeg_make(
            &vfr,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=160x96:r=30:d=8,select='not(eq(mod(n,5),2))'",
                "-fps_mode",
                "vfr",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "30",
            ],
        );
    }
    let prefs = Prefs::default();
    for (name, src) in [
        ("counted", counted()),
        ("recording", recording()),
        ("vfr", vfr.as_path()),
    ] {
        let head = probe(src, &prefs).unwrap();
        let bare = without_cues(
            src,
            &format!("cueless-{}", src.file_name().unwrap().to_str().unwrap()),
        );
        let full = probe(&bare, &prefs).unwrap();
        let count = frame_count(src, &prefs).unwrap();
        assert_eq!(frame_count(&bare, &prefs).unwrap(), count, "{name}");
        assert!(
            full.frames.is_none() || full.frames == Some(count),
            "{name}"
        );
        assert_eq!(
            keyframes(src, &prefs).unwrap(),
            keyframes(&bare, &prefs).unwrap(),
            "{name}"
        );
        assert!(
            head.frames.is_none() || head.frames == Some(count),
            "{name}"
        );
        assert!(
            (head.duration - full.duration).abs() < 1e-6,
            "{name}: {} against {}",
            head.duration,
            full.duration
        );
        assert_eq!(
            (
                head.fps,
                head.width,
                head.height,
                &head.profile,
                head.audio.len()
            ),
            (
                full.fps,
                full.width,
                full.height,
                &full.profile,
                full.audio.len()
            ),
            "{name}"
        );
    }
    assert_eq!(frame_count(counted(), &prefs).unwrap(), 1800);
    let hollow = dir().join("counted-hollow.mkv");
    let mut bytes = std::fs::read(counted()).unwrap();
    let n = bytes.len();
    bytes[n / 4..n * 3 / 4].fill(0);
    std::fs::write(&hollow, &bytes).unwrap();
    let got = probe(&hollow, &prefs).unwrap();
    assert!(got.frames.is_none(), "the probe read the clusters");
    assert_eq!((got.duration, got.fps), (60.0, (30, 1)));
    let bare = without_cues(counted(), "cueless-counted-hollow-src.mkv");
    let hollow = dir().join("cueless-counted-hollow.mkv");
    let mut bytes = std::fs::read(&bare).unwrap();
    let n = bytes.len();
    bytes[n / 4..n * 3 / 4].fill(0);
    std::fs::write(&hollow, &bytes).unwrap();
    let got = probe(&hollow, &prefs).unwrap();
    assert!(got.frames.is_none(), "the Cue-less probe read the clusters");
    assert_eq!((got.duration, got.fps), (60.0, (30, 1)));
}

fn two_tracks() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let out = dir().join("two-pcm-tracks.mkv");
        if !out.exists() {
            ffmpeg_make(
                &out,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=352x240:r=30:d=40",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=f=440:r=48000:d=40",
                    "-f",
                    "lavfi",
                    "-i",
                    "anoisesrc=r=48000:d=37.5:a=0.3:seed=3",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-map",
                    "2:a",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-g",
                    "60",
                    "-c:a",
                    "pcm_s16le",
                    "-ac",
                    "2",
                ],
            );
        }
        out
    })
}

#[test]
fn a_mix_stream_equals_the_whole_mix_with_and_without_cues() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let path = two_tracks();
    let bare = without_cues(path, "cueless-two-pcm-tracks.mkv");
    let gains = [0.5, 1.25];
    for file in [path, bare.as_path()] {
        for from in [0.0, 12.3] {
            let want = audio_mix(file, &gains, from, f64::INFINITY, 48_000, &prefs).unwrap();
            let got: Vec<f32> = audio_mix_stream(file, &gains, from, 48_000, &prefs)
                .unwrap()
                .flat_map(|b| b.unwrap().samples)
                .collect();
            assert_eq!(
                got.len(),
                want.samples.len(),
                "{} from {from}",
                file.display()
            );
            assert!(got == want.samples, "{} from {from}", file.display());
        }
    }
}

fn two_tones(name: &str, codec: &str) -> PathBuf {
    let out = dir().join(name);
    if !out.exists() {
        ffmpeg_make(
            &out,
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:r=10:d=20",
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:r=48000:d=20,volume=0.4",
                "-f",
                "lavfi",
                "-i",
                "sine=f=660:r=48000:d=20,volume=0.4",
                "-map",
                "0",
                "-map",
                "1",
                "-map",
                "2",
                "-ac",
                "2",
                "-c:v",
                "libx264",
                "-g",
                "20",
                "-c:a",
                codec,
            ],
        );
    }
    out
}

fn off(got: &[f32], want: &[f32], limit: f32) -> Option<usize> {
    if got.len() != want.len() {
        return Some(got.len().min(want.len()));
    }
    got.iter()
        .zip(want)
        .position(|(a, b)| (a - b).abs() > limit)
}

#[test]
fn every_audio_range_and_stream_start_cuts_the_whole_decode() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let pcm = two_tones("ranged-pcm.mkv", "pcm_s16le");
    let files = [
        (pcm.clone(), 0.0),
        (without_cues(&pcm, "ranged-pcm-no-cues.mkv"), 0.0),
        (two_tones("ranged-pcm.mp4", "pcm_s16le"), 0.0),
        (two_tones("ranged-aac.mkv", "aac"), 1e-5),
        (two_tones("ranged-aac.mp4", "aac"), 1e-5),
        (two_tones("ranged-opus.mkv", "libopus"), 2e-3),
    ];
    let mut ranges: Vec<(usize, usize)> = vec![
        (9_600, 33_600),
        (29_600, 33_600),
        (40_960, 44_960),
        (41_460, 45_460),
        (307_201, 311_297),
        (0, 48_000),
        (48_000, 144_000),
        (144_000, 336_000),
        (336_000, 720_000),
        (900_000, 960_000),
    ];
    ranges.extend((0..8).map(|k| (9_600 + 4_000 * k, 13_600 + 4_000 * k)));
    ranges.extend((0..60).map(|k| (20_000 + 337 * k, 22_000 + 337 * k)));
    for (file, limit) in &files {
        let name = file.file_name().unwrap().to_string_lossy();
        let whole: Vec<Vec<f32>> = (0..2)
            .map(|t| audio_pcm(file, t, 48_000, false, &prefs).unwrap().samples)
            .collect();
        for (t, all) in whole.iter().enumerate() {
            assert!(all.len() >= 960_000 * 2, "{name} track {t}");
            for &(a, b) in &ranges {
                let (from, to) = (a as f64 / 48_000.0, b as f64 / 48_000.0);
                let got = audio_pcm_range(file, t, from, to, 48_000, 2, &prefs).unwrap();
                let at = off(&got.samples, &all[a * 2..b * 2], *limit);
                assert!(at.is_none(), "{name} track {t} [{a}, {b}) at {at:?}");
            }
        }
        for &(a, b) in ranges.iter().take(10) {
            let from = a as f64 / 48_000.0;
            for (t, all) in whole.iter().enumerate() {
                let got: Vec<f32> = audio_stream(file, t, from, 48_000, 2, &prefs)
                    .unwrap()
                    .flat_map(|b| b.unwrap().samples)
                    .take((b - a) * 2)
                    .collect();
                let at = off(&got, &all[a * 2..b * 2], *limit);
                assert!(at.is_none(), "{name} stream {t} from {a} at {at:?}");
            }
            let mixed: Vec<f32> = audio_mix_stream(file, &[0.5, 2.0], from, 48_000, &prefs)
                .unwrap()
                .flat_map(|b| b.unwrap().samples)
                .take((b - a) * 2)
                .collect();
            let want: Vec<f32> = whole[0][a * 2..b * 2]
                .iter()
                .zip(&whole[1][a * 2..b * 2])
                .map(|(x, y)| 0.0 + x * 0.5 + y * 2.0)
                .collect();
            let at = off(&mixed, &want, limit * 3.0);
            assert!(at.is_none(), "{name} mix from {a} at {at:?}");
        }
    }
}

fn vp9_clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        let out = dir().join("vp9-g30.webm");
        if out.exists() {
            return out;
        }
        let part = dir().join(format!("vp9-g30.{}.part.webm", std::process::id()));
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(["-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=8"])
            .args(["-c:v", "libvpx-vp9", "-deadline", "good", "-cpu-used", "4"])
            .args(["-auto-alt-ref", "1", "-lag-in-frames", "16", "-b:v", "1M"])
            .args(["-g", "30", "-keyint_min", "30", "-pix_fmt", "yuv420p"])
            .arg(&part)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        if let Err(e) = std::fs::rename(&part, &out) {
            assert!(out.exists(), "{e}");
            let _ = std::fs::remove_file(&part);
        }
        out
    })
}

fn frame_md5s(path: &Path) -> Vec<String> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.rsplit(',').next().unwrap().trim().to_string())
        .collect()
}

fn luma(path: &Path) -> Vec<Vec<u8>> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout.chunks(640 * 360).map(<[u8]>::to_vec).collect()
}

fn luma_psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    10.0 * (255.0f64.powi(2) * a.len() as f64 / se.max(1e-9)).log10()
}

#[test]
fn a_vp9_cut_copies_its_middles_bit_exact_into_vp09_and_encodes_only_its_heads() {
    if !have_ffmpeg() {
        return;
    }
    let src = vp9_clip();
    let out = dir().join("vp9-cut.mp4");
    let pieces = [(1.1, 3.4), (5.5, 6.2), (0.2, 1.5)].map(|(from, to)| Piece {
        path: src.to_path_buf(),
        from,
        to,
    });
    let silent = |_: usize, frames: usize| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = vec![0.0; frames * 2];
        Ok(buf)
    };
    let opts = || ExportOptions {
        video: Codec::Vp9,
        ..ExportOptions::default()
    };
    let done = export_with_audio(&out, &pieces, opts(), silent, |_| true).unwrap();
    assert!(done.smart, "{:?}", done.reasons);
    assert_eq!(done.video.codec, Codec::Vp9);
    assert!(done.video.reasons.is_empty(), "{:?}", done.video.reasons);
    assert_eq!((done.frames, done.copied, done.encoded), (129, 63, 66));
    assert_eq!(
        ffprobe(
            &out,
            &[
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,codec_tag_string",
                "-of",
                "csv=p=0",
            ],
        ),
        "vp9,vp09"
    );
    let warnings = Command::new("ffmpeg")
        .args(["-v", "warning", "-i"])
        .arg(&out)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(warnings.status.success());
    assert_eq!(String::from_utf8_lossy(&warnings.stderr), "");
    let times = ffprobe(
        &out,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts_time",
            "-of",
            "csv=p=0",
        ],
    );
    let times: Vec<f64> = times.lines().map(|t| t.parse().unwrap()).collect();
    assert_eq!(times.len(), 129);
    for (k, t) in times.iter().enumerate() {
        assert!((t - k as f64 / 30.0).abs() < 1e-3, "frame {k} at {t}");
    }
    let (ours, theirs) = (frame_md5s(&out), frame_md5s(src));
    for (at, from, n) in [(27, 60, 42), (84, 180, 6), (114, 30, 15)] {
        assert_eq!(
            ours[at..at + n],
            theirs[from..from + n],
            "copied from {from}"
        );
    }
    let (ours, theirs) = (luma(&out), luma(src));
    for (at, from, n) in [(0, 33, 27), (69, 165, 15), (90, 6, 24)] {
        for k in 0..n {
            let psnr = luma_psnr(&ours[at + k], &theirs[from + k]);
            assert!(psnr > 35.0, "edge frame {} at {psnr}", at + k);
        }
    }
    assert_eq!(
        probe(&out, &Prefs::default()).unwrap().video_codec,
        Some(Codec::Vp9)
    );
    let again = dir().join("vp9-recut.mp4");
    let piece = [Piece {
        path: out.clone(),
        from: 0.0,
        to: 2.3,
    }];
    let done = export_with_audio(&again, &piece, opts(), silent, |_| true).unwrap();
    assert!(done.smart && done.copied == 69, "{:?}", done.reasons);
    assert!(decodes_clean(&again));
}

fn late_start(ext: &str) -> PathBuf {
    let base = dir().join("late-base.mkv");
    if !base.exists() {
        ffmpeg_make(
            &base,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=352x240:r=24000/1001:d=40",
                "-f",
                "lavfi",
                "-i",
                "aevalsrc='if(lt(mod(t,1),0.1),0.5*sin(2*PI*440*t),0)':s=48000:d=40",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "48",
                "-bf",
                "2",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "128k",
            ],
        );
    }
    let out = dir().join(format!("late-start.{ext}"));
    if !out.exists() {
        let base = base.to_str().unwrap();
        ffmpeg_make(
            &out,
            &[
                "-itsoffset",
                "2999.747",
                "-i",
                base,
                "-itsoffset",
                "2999.997",
                "-i",
                base,
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c",
                "copy",
            ],
        );
    }
    out
}

fn ffmpeg_raw(path: &Path, at: f64, args: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{at:.6}"), "-i"])
        .arg(path)
        .args(args)
        .arg("-")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn onset(samples: &[f32], rate: u32) -> f64 {
    samples.iter().position(|s| s.abs() > 0.05).unwrap() as f64 / rate as f64
}

#[test]
fn a_file_that_starts_late_counts_every_time_from_its_start() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let gpu = pito_codecs::gpu::available()
        .iter()
        .any(|&(c, d, _)| c == Codec::H264 && d == pito_codecs::gpu::Direction::Decode);
    for ext in ["mkv", "mp4"] {
        let path = late_start(ext);
        let p = probe(&path, &prefs).unwrap();
        assert!((p.start - 2999.747).abs() < 0.002, "{ext}: {}", p.start);
        assert!((p.duration - 40.0).abs() < 0.2, "{ext}: {}", p.duration);
        assert!((p.video_start.unwrap() - 2999.747).abs() < 0.002, "{ext}");
        let audio_start = p.audio[0].start.unwrap();
        assert!((audio_start - 2999.99).abs() < 0.03, "{ext}: {audio_start}");

        let times: Vec<f64> = ffprobe(
            &path,
            &[
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts_time",
                "-of",
                "csv=p=0",
            ],
        )
        .lines()
        .map(|l| l.trim().parse::<f64>().unwrap() - p.start)
        .filter(|t| *t >= 30.0)
        .collect();
        let at = times.iter().copied().fold(f64::INFINITY, f64::min);
        let theirs = ffmpeg_raw(
            &path,
            at,
            &[
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        let frame = decode_video(&path, at, None, &cpu)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let fps = p.fps.0 as f64 / p.fps.1 as f64;
        assert_eq!(frame.pts, (at * fps).round() as i64, "{ext}");
        let ours: Vec<u8> = frame.planes.iter().flat_map(|q| q.data.clone()).collect();
        assert!(ours == theirs, "{ext}: the frame at {at:.4} s differs");
        let mut on_gpu = decode_video_gpu(&path, at, None, &prefs).unwrap();
        assert_eq!(on_gpu.on_gpu(), gpu, "{ext}: {:?}", on_gpu.reasons);
        let g = on_gpu.next().unwrap().unwrap();
        assert_eq!(g.pts(), frame.pts, "{ext}");
        if gpu {
            let g = match g {
                VideoFrame::Gpu(g) => g.download().unwrap(),
                VideoFrame::Cpu(f) => f,
            };
            let diff = luma_diff(&g, &frame);
            assert!(diff < 1.0, "{ext}: {diff}");
        }
        let still = frames_at_seek(&path, &[at], 240, Seek::Exact, &cpu).unwrap();
        let theirs_still =
            frames_at_seek(&path, &[at], 240, Seek::Exact, &Prefs::ffmpeg()).unwrap();
        let d = mean_diff(&still[0], &theirs_still[0]);
        assert!(d < 6.0, "{ext}: {d}");

        let ours = audio_pcm_range(&path, 0, 30.0, 35.0, 48_000, 1, &prefs).unwrap();
        let theirs: Vec<f32> = ffmpeg_raw(
            &path,
            30.0,
            &["-t", "5", "-map", "0:a:0", "-ac", "1", "-f", "f32le"],
        )
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
        assert_eq!(ours.frames(), 240_000, "{ext}");
        let (lag, w) = (-24i64..=24)
            .map(|lag| {
                let skip = lag.unsigned_abs() as usize;
                let (a, b) = if lag < 0 {
                    (&ours.samples[skip..], &theirs[..])
                } else {
                    (&ours.samples[..], &theirs[skip..])
                };
                let n = a.len().min(b.len()) - 48;
                (lag, worst(&a[..n], &b[..n]))
            })
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap();
        assert!(w < 0.01, "{ext}: {w} at a lag of {lag} samples");
        let head = audio_pcm_range(&path, 0, 0.0, 1.0, 48_000, 1, &prefs).unwrap();
        let o = onset(&head.samples, 48_000);
        assert!((o - 0.25).abs() < 0.01, "{ext}: {o}");

        let out = dir().join(format!("late-cut-{ext}.mp4"));
        let done = export(
            &out,
            &[Piece {
                path: path.clone(),
                from: 10.0,
                to: 20.0,
            }],
            ExportOptions {
                prefs: prefs.clone(),
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert!(done.smart && done.copied > 0, "{ext}");
        assert!(decodes_clean(&out), "{ext}");
        let first = (10.0 * fps).round() as i64;
        let psnr = min_psnr(&out, &path, &[(first, first + done.frames as i64)]);
        assert!(psnr > 40.0, "{ext}: {psnr}");
        for s in ["v:0", "a:0"] {
            let line = ffprobe(
                &out,
                &[
                    "-select_streams",
                    s,
                    "-show_entries",
                    "stream=start_time,duration",
                    "-of",
                    "csv=p=0",
                ],
            );
            let (start, length) = line.split_once(',').unwrap();
            let (start, length): (f64, f64) = (start.parse().unwrap(), length.parse().unwrap());
            assert!(start.abs() < 0.05, "{ext} {s}: {start}");
            assert!((length - 10.0).abs() < 0.06, "{ext} {s}: {length}");
        }
        let cut: Vec<f32> = ffmpeg_raw(&out, 0.0, &["-map", "0:a:0", "-ac", "1", "-f", "f32le"])
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let o = onset(&cut, 48_000);
        assert!(
            (o - 0.25).abs() < 0.02,
            "{ext}: the cut's audio starts at {o}"
        );
    }
}

fn packed(f: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    for p in &f.planes {
        for y in 0..p.height {
            out.extend_from_slice(&p.data[y * p.stride..y * p.stride + p.width]);
        }
    }
    out
}

fn pyramid_hevc() -> PathBuf {
    let out = dir().join("pyramid-hevc.mp4");
    if !out.exists() {
        ffmpeg_make(
            &out,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=352x240:r=30:d=3",
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-x265-params",
                "log-level=error:keyint=60:min-keyint=60:scenecut=0:bframes=4:b-pyramid=1",
                "-pix_fmt",
                "yuv420p",
                "-tag:v",
                "hvc1",
            ],
        );
    }
    out
}

#[test]
fn a_start_inside_a_gop_skips_unused_pictures_and_stays_bit_exact() {
    if !have_ffmpeg() {
        return;
    }
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let hevc = pyramid_hevc();
    for (path, k) in [
        (clip(), 47),
        (clip(), 22),
        (hevc.as_path(), 53),
        (hevc.as_path(), 29),
    ] {
        let p = probe(path, &cpu).unwrap();
        let fps = p.fps.0 as f64 / p.fps.1 as f64;
        let at = k as f64 / fps;
        let size = p.width as usize * p.height as usize * 3 / 2;
        let theirs = ffmpeg_raw(
            path,
            0.0,
            &[
                "-map",
                "0:v:0",
                "-frames:v",
                &(k + 8).to_string(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        let ours: Vec<Frame> = decode_video(path, at, None, &cpu)
            .unwrap()
            .take(6)
            .map(Result::unwrap)
            .collect();
        let first = usize::try_from(ours[0].pts).unwrap();
        assert!(first.abs_diff(k) <= 1, "{k}: {first}");
        for (n, f) in ours.iter().enumerate() {
            let want = &theirs[(first + n) * size..(first + n + 1) * size];
            assert!(
                packed(f) == want,
                "{}: frame {} differs",
                path.display(),
                first + n
            );
        }
        let mut on_gpu = decode_video_gpu(path, at, None, &Prefs::default()).unwrap();
        if on_gpu.on_gpu() {
            for f in &ours {
                let g = match on_gpu.next().unwrap().unwrap() {
                    VideoFrame::Gpu(g) => g.download().unwrap(),
                    VideoFrame::Cpu(f) => f,
                };
                assert_eq!(g.pts, f.pts, "{}", path.display());
                let diff = luma_diff(&g, f);
                assert!(diff < 1.0, "{}: {diff}", path.display());
            }
        }
    }
}

#[test]
fn a_stills_handle_hands_each_index_once_and_matches_single_calls() {
    if !have_ffmpeg() {
        return;
    }
    let prefs = Prefs::default();
    let hevc = pyramid_hevc();
    let times = [0.5, 2.2, 0.5, 1.37, 0.0];
    for path in [clip(), hevc.as_path()] {
        let stills = Stills::open(path, 120, &prefs).unwrap();
        for seek in [Seek::Keyframe, Seek::Exact] {
            let want = frames_at_seek(path, &times, 120, seek, &prefs).unwrap();
            for round in 0..4 {
                match round {
                    2 => stills.keep(1),
                    3 => stills.trim(),
                    _ => {}
                }
                let mut got: Vec<Vec<Rgba>> = vec![Vec::new(); times.len()];
                stills
                    .frames(&times, seek, |i, r| got[i].push(r.unwrap()))
                    .unwrap();
                for (i, g) in got.iter().enumerate() {
                    assert_eq!(g.len(), 1, "{} {seek:?} {i}", path.display());
                    assert!(g[0] == want[i], "{} {seek:?} {i}", path.display());
                }
            }
        }
    }
}
