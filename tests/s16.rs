#![cfg(all(
    feature = "media",
    feature = "aac",
    feature = "mp4",
    feature = "mkv",
    feature = "opus",
    feature = "mp3",
    feature = "flac",
    feature = "alac",
    feature = "ac3"
))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use pito_codecs::media::{Error, Prefs, S16Span, audio_pcm_range, decode_s16, probe, to_s16};

const LOSSY_SNR_DB: f64 = 30.0;
const OWN_SNR_DB: f64 = 40.0;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media/s16");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg_make(name: &str, args: &[&str]) -> Option<PathBuf> {
    static MAKING: Mutex<()> = Mutex::new(());
    static PARTS: AtomicUsize = AtomicUsize::new(0);
    let _one = MAKING.lock().unwrap_or_else(PoisonError::into_inner);
    let key = args.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, a| {
        a.bytes()
            .chain([0])
            .fold(h, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    });
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let name = &format!("{stem}-{key:016x}.{ext}");
    let out = dir().join(name);
    if out.exists() {
        return Some(out);
    }
    let part = dir().join(format!(
        "{}.{}.{name}",
        std::process::id(),
        PARTS.fetch_add(1, Ordering::Relaxed)
    ));
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .arg(&part)
        .status()
        .unwrap()
        .success();
    if !ok {
        let _ = std::fs::remove_file(&part);
        return None;
    }
    if std::fs::rename(&part, &out).is_err() {
        let _ = std::fs::remove_file(&part);
    }
    Some(out)
}

fn two_tracks(ext: &str) -> Option<PathBuf> {
    ffmpeg_make(
        &format!("two-tracks.{ext}"),
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=352x240:r=30:d=6",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*(200+100*t)*t)|0.5*sin(2*PI*(300+50*t)*t):s=48000:c=stereo:d=6",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*(500+30*t)*t):s=44100:c=mono:d=6",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:a",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
        ],
    )
    .or_else(|| {
        eprintln!("this ffmpeg can't write the two-track {ext} with mpeg4 and aac: skipped");
        None
    })
}

fn ffmpeg_s16(path: &Path, stream: usize, from: f64, mono: bool, rate: u32) -> Vec<i16> {
    let mut c = Command::new("ffmpeg");
    c.args(["-nostdin", "-v", "error"]);
    if from > 0.0 {
        c.args(["-ss", &format!("{from:.6}")]);
    }
    c.arg("-i").arg(path).args(["-map", &format!("0:{stream}")]);
    if mono {
        c.args(["-ac", "1"]);
    }
    let out = c
        .args(["-ar", &rate.to_string(), "-f", "s16le", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn ffmpeg_file_time(
    path: &Path,
    stream: usize,
    from: f64,
    mono: bool,
    rate: u32,
    ch: usize,
) -> Vec<i16> {
    let whole = ffmpeg_s16(path, stream, 0.0, mono, rate);
    let skip = (from * f64::from(rate)).round() as usize * ch;
    whole.get(skip..).unwrap_or_default().to_vec()
}

fn ours(path: &Path, span: &S16Span) -> Result<Vec<i16>, Error> {
    let mut all = Vec::new();
    let mut calls = 0;
    decode_s16(path, span, &Prefs::default(), |chunk| {
        calls += 1;
        all.extend_from_slice(chunk);
        Ok(())
    })?;
    assert!(calls >= 1);
    Ok(all)
}

struct Match {
    lag: i64,
    snr: f64,
    worst: i32,
}

fn best(a: &[i16], b: &[i16], ch: usize, reach: usize) -> Match {
    (-(reach as i64)..=reach as i64)
        .map(|lag| {
            let skip = lag.unsigned_abs() as usize * ch;
            let (x, y) = if lag < 0 {
                (&a[skip.min(a.len())..], b)
            } else {
                (a, &b[skip.min(b.len())..])
            };
            let n = x.len().min(y.len()).saturating_sub(reach * ch);
            let (mut sig, mut err, mut worst) = (0f64, 0f64, 0i32);
            for (&p, &q) in x[..n].iter().zip(&y[..n]) {
                let d = p as i32 - q as i32;
                sig += (q as f64).powi(2);
                err += (d as f64).powi(2);
                worst = worst.max(d.abs());
            }
            let snr = if err == 0.0 {
                f64::INFINITY
            } else {
                10.0 * (sig / err).log10()
            };
            Match { lag, snr, worst }
        })
        .max_by(|p, q| p.snr.total_cmp(&q.snr))
        .unwrap()
}

#[test]
fn both_tracks_of_a_two_track_file_match_ffmpeg_from_any_start_mono_or_not() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the s16 track comparison is skipped");
        return;
    }
    let prefs = Prefs::default();
    for ext in ["mkv", "mp4"] {
        let Some(path) = two_tracks(ext) else {
            continue;
        };
        let p = probe(&path, &prefs).unwrap();
        let streams: Vec<(usize, usize, u16)> = p
            .audio
            .iter()
            .map(|t| (t.stream, t.index, t.channels))
            .collect();
        assert_eq!(streams, [(1, 0, 2), (2, 1, 1)], "{ext}");
        for (stream, index, source_channels) in streams {
            for from in [0.0, 2.5] {
                for rate in [16_000, 48_000] {
                    for mono in [true, false] {
                        let what = format!(
                            "{ext} stream {stream} from {from} s at {rate} Hz, mono {mono}"
                        );
                        let span = S16Span {
                            stream,
                            from,
                            rate,
                            mono,
                        };
                        let got = ours(&path, &span).unwrap();
                        let ch = if mono { 1 } else { source_channels as usize };
                        assert_eq!(got.len() % ch, 0, "{what}");
                        let want = ffmpeg_file_time(&path, stream, from, mono, rate, ch);
                        let slack = (rate as usize / 20) * ch;
                        assert!(
                            got.len().abs_diff(want.len()) <= slack,
                            "{what}: {} samples against ffmpeg's {}",
                            got.len(),
                            want.len()
                        );
                        let expect = ((6.0 - from) * rate as f64) as usize * ch;
                        assert!(got.len().abs_diff(expect) <= slack, "{what}: {}", got.len());
                        let m = best(&got, &want, ch, rate as usize / 100);
                        assert!(
                            m.snr >= LOSSY_SNR_DB,
                            "{what}: {:.1} dB against ffmpeg at a lag of {} frames",
                            m.snr,
                            m.lag
                        );
                        assert!(
                            m.lag.abs() <= rate as i64 / 1000,
                            "{what}: starts {} frames off ffmpeg (SNR {:.1} dB)",
                            m.lag,
                            m.snr
                        );
                        if from > 0.0 {
                            let seeked = ffmpeg_s16(&path, stream, from, mono, rate);
                            let s = best(&got, &seeked, ch, rate as usize / 100);
                            assert!(
                                s.lag.abs() <= rate as i64 / 1000,
                                "{what}: starts {} frames off ffmpeg -ss",
                                s.lag
                            );
                        }
                        let own = audio_pcm_range(
                            &path,
                            index,
                            from,
                            f64::INFINITY,
                            rate,
                            ch as u16,
                            &prefs,
                        )
                        .unwrap();
                        let own: Vec<i16> = own.samples.iter().map(|&s| to_s16(s)).collect();
                        let m = best(&got, &own, ch, rate as usize / 100);
                        assert!(
                            m.snr >= OWN_SNR_DB,
                            "{what}: {:.1} dB against our in-memory path at a lag of {} frames",
                            m.snr,
                            m.lag
                        );
                        assert!(
                            m.lag.abs() <= 1,
                            "{what}: starts {} frames off our in-memory path",
                            m.lag
                        );
                    }
                }
            }
        }
        let a = ours(
            &path,
            &S16Span {
                stream: 1,
                from: 0.0,
                rate: 16_000,
                mono: true,
            },
        )
        .unwrap();
        let b = ours(
            &path,
            &S16Span {
                stream: 2,
                from: 0.0,
                rate: 16_000,
                mono: true,
            },
        )
        .unwrap();
        let m = best(&a, &b, 1, 160);
        assert!(
            m.snr < 10.0,
            "{ext}: the two tracks decode alike ({:.1} dB)",
            m.snr
        );
    }
}

fn one_line(e: Error, path: &Path, has: &[&str]) {
    let said = e.to_string();
    assert!(!said.contains('\n'), "{said}");
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(said.contains(name), "{said}");
    for h in has {
        assert!(said.contains(h), "{said}: no {h:?}");
    }
}

#[test]
fn a_missing_stream_a_late_start_and_a_refused_codec_each_fail_in_one_line_naming_it() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the s16 refusals are skipped");
        return;
    }
    let Some(path) = two_tracks("mkv") else {
        return;
    };
    let span = S16Span {
        stream: 1,
        from: 0.0,
        rate: 16_000,
        mono: true,
    };
    for stream in [0, 3, 99] {
        let e = ours(&path, &S16Span { stream, ..span }).unwrap_err();
        one_line(
            e,
            &path,
            &[
                &format!("no audio stream {stream}"),
                "its audio streams are 1, 2",
            ],
        );
    }
    for from in [6.5, 600.0] {
        let e = ours(&path, &S16Span { from, ..span }).unwrap_err();
        one_line(e, &path, &["stream 1", "past the end", &format!("{from}")]);
    }
    match ffmpeg_make(
        "refused-wmav2.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=44100:d=2",
            "-c:a",
            "wmav2",
        ],
    ) {
        Some(refused) => {
            let e = ours(&refused, &S16Span { stream: 0, ..span }).unwrap_err();
            assert!(matches!(e, Error::Unsupported(_)), "{e}");
            one_line(e, &refused, &["stream 0", "WMA v2", "0x0161"]);
        }
        None => eprintln!("this ffmpeg can't write wmav2: the refused codec is skipped"),
    }
    match ffmpeg_make(
        "no-audio-mpeg4.mkv",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=352x240:r=30:d=1",
            "-c:v",
            "mpeg4",
        ],
    ) {
        Some(silent) => {
            let e = ours(&silent, &S16Span { stream: 0, ..span }).unwrap_err();
            one_line(e, &silent, &["no audio stream 0", "it has no audio"]);
        }
        None => eprintln!("this ffmpeg can't write mpeg4: the file with no audio is skipped"),
    }
}

#[test]
fn a_delayed_short_track_starts_at_file_time_and_ends_on_its_own() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the delayed track is skipped");
        return;
    }
    for ext in ["mkv", "mp4"] {
        let Some(path) = ffmpeg_make(
            &format!("delayed-short.{ext}"),
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=352x240:r=30:d=6",
                "-f",
                "lavfi",
                "-i",
                "aevalsrc=0.5*sin(2*PI*(200+100*t)*t):s=48000:c=mono:d=6",
                "-itsoffset",
                "0.3",
                "-f",
                "lavfi",
                "-i",
                "aevalsrc=0.5*sin(2*PI*(500+300*t)*t):s=48000:c=mono:d=3",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "-b:a",
                "192k",
            ],
        ) else {
            eprintln!("this ffmpeg can't write the delayed {ext}: skipped");
            continue;
        };
        for from in [1.0, 2.5] {
            let rate = 48_000;
            let what = format!("{ext} delayed stream 2 from {from} s");
            let span = S16Span {
                stream: 2,
                from,
                rate,
                mono: true,
            };
            let got = ours(&path, &span).unwrap();
            let expect = ((3.3 - from) * rate as f64) as usize;
            assert!(
                got.len().abs_diff(expect) <= rate as usize / 20,
                "{what}: {} samples, about {expect} expected",
                got.len()
            );
            let want = ffmpeg_s16(&path, 2, from, true, rate);
            let m = best(&got, &want, 1, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB against ffmpeg at a lag of {}",
                m.snr,
                m.lag
            );
            assert!(
                m.lag.abs() <= rate as i64 / 2000,
                "{what}: starts {} frames off ffmpeg's file time",
                m.lag
            );
        }
        let span = S16Span {
            stream: 2,
            from: 4.0,
            rate: 16_000,
            mono: true,
        };
        let e = ours(&path, &span).unwrap_err();
        one_line(e, &path, &["stream 2", "past the end", "4"]);
        let longer = ours(&path, &S16Span { stream: 1, ..span }).unwrap();
        assert!(
            longer.len().abs_diff(32_000) <= 800,
            "{ext}: {}",
            longer.len()
        );
    }
}

#[test]
fn a_mixed_obs_recording_decodes_every_track_like_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the mixed recording is skipped");
        return;
    }
    let sources = [
        "aevalsrc=0.5*sin(2*PI*(200+100*t)*t)|0.4*sin(2*PI*(300+50*t)*t):s=48000:c=stereo:d=4",
        "aevalsrc=0.5*sin(2*PI*(500+30*t)*t):s=48000:c=mono:d=4",
        "aevalsrc=0.4*sin(2*PI*(800+60*t)*t)|0.3*sin(2*PI*(250+20*t)*t):s=48000:c=stereo:d=4",
    ];
    for (ext, third) in [("mkv", "pcm_s24le"), ("mp4", "flac")] {
        let mut args = vec!["-f", "lavfi", "-i", "testsrc2=s=352x240:r=30:d=4"];
        for s in &sources {
            args.extend_from_slice(&["-f", "lavfi", "-i", s]);
        }
        args.extend_from_slice(&[
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a", "-c:v", "mpeg4", "-c:a:0",
            "aac", "-c:a:1", "libopus", "-c:a:2", third,
        ]);
        if third == "flac" {
            args.extend_from_slice(&["-bits_per_raw_sample:a:2", "24"]);
        }
        if ext == "mp4" {
            args.extend_from_slice(&["-strict", "-2"]);
        }
        let Some(path) = ffmpeg_make(&format!("mixed.{ext}"), &args) else {
            eprintln!("this ffmpeg can't write aac, libopus and {third} in one {ext}: skipped");
            continue;
        };
        let p = probe(&path, &Prefs::default()).unwrap();
        let streams: Vec<(usize, u16)> = p.audio.iter().map(|t| (t.stream, t.channels)).collect();
        assert_eq!(streams, [(1, 2), (2, 1), (3, 2)], "{ext}");
        for (stream, channels) in streams {
            let what = format!("{ext} stream {stream}");
            let span = S16Span {
                stream,
                from: 1.0,
                rate: 48_000,
                mono: false,
            };
            let got = ours(&path, &span).unwrap();
            let want = ffmpeg_s16(&path, stream, 1.0, false, 48_000);
            let ch = channels as usize;
            assert!(
                got.len().abs_diff(want.len()) <= 2_400 * ch,
                "{what}: {} against {}",
                got.len(),
                want.len()
            );
            let m = best(&got, &want, ch, 480);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB at a lag of {}",
                m.snr,
                m.lag
            );
            assert!(
                m.lag.abs() <= 48,
                "{what}: starts {} frames off ffmpeg",
                m.lag
            );
        }
    }
}

#[test]
fn a_five_one_track_downmixes_to_mono_like_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the 5.1 downmix is skipped");
        return;
    }
    let tones = "0.3*sin(2*PI*220*t)|0.3*sin(2*PI*330*t)|0.3*sin(2*PI*440*t)|0.3*sin(2*PI*55*t)|0.3*sin(2*PI*660*t)|0.3*sin(2*PI*770*t)";
    for (ext, codec) in [("mkv", "flac"), ("mp4", "aac")] {
        let source = format!("aevalsrc={tones}:s=48000:c=5.1:d=2");
        let Some(path) = ffmpeg_make(
            &format!("five-one-{codec}.{ext}"),
            &["-f", "lavfi", "-i", &source, "-c:a", codec, "-b:a", "384k"],
        ) else {
            eprintln!("this ffmpeg can't write 5.1 {codec} in {ext}: skipped");
            continue;
        };
        let stream = probe(&path, &Prefs::default()).unwrap().audio[0].stream;
        for rate in [16_000, 48_000] {
            let what = format!("5.1 {codec} in {ext} to mono at {rate} Hz");
            let span = S16Span {
                stream,
                from: 0.0,
                rate,
                mono: true,
            };
            let got = ours(&path, &span).unwrap();
            let want = ffmpeg_s16(&path, stream, 0.0, true, rate);
            assert!(
                got.len().abs_diff(want.len()) <= rate as usize / 20,
                "{what}: {} against {}",
                got.len(),
                want.len()
            );
            let m = best(&got, &want, 1, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB against ffmpeg -ac 1 at a lag of {}",
                m.snr,
                m.lag
            );
        }
    }
}

#[test]
fn a_callback_error_stops_the_stream_and_comes_back_as_is() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the s16 stop is skipped");
        return;
    }
    let Some(path) = two_tracks("mp4") else {
        return;
    };
    let span = S16Span {
        stream: 2,
        from: 0.0,
        rate: 48_000,
        mono: false,
    };
    let mut calls = 0;
    let e = decode_s16(&path, &span, &Prefs::default(), |_| {
        calls += 1;
        Err(Error::Cancelled)
    })
    .unwrap_err();
    assert!(matches!(e, Error::Cancelled), "{e}");
    assert_eq!(calls, 1);
    let mut chunks = 0;
    let frames = decode_s16(&path, &span, &Prefs::default(), |c| {
        chunks += 1;
        assert!(c.len() <= 48_000, "a chunk of {} samples", c.len());
        Ok(())
    })
    .unwrap();
    assert!(chunks >= 6, "{chunks} chunks");
    assert!(frames.abs_diff(6 * 48_000) <= 2_400, "{frames}");
}

struct Kind {
    ext: &'static str,
    codec: &'static str,
    extra: &'static [&'static str],
    lossless: bool,
    may_refuse: bool,
}

const fn k(ext: &'static str, codec: &'static str, lossless: bool) -> Kind {
    Kind {
        ext,
        codec,
        extra: &[],
        lossless,
        may_refuse: false,
    }
}

const fn refusable(ext: &'static str, codec: &'static str) -> Kind {
    Kind {
        may_refuse: true,
        ..k(ext, codec, false)
    }
}

const KINDS: &[Kind] = &[
    k("mkv", "aac", false),
    k("mp4", "aac", false),
    Kind {
        ext: "mp4",
        codec: "libfdk_aac",
        extra: &["-profile:a", "aac_he"],
        lossless: false,
        may_refuse: false,
    },
    Kind {
        ext: "mp4",
        codec: "libfdk_aac",
        extra: &["-profile:a", "aac_he_v2"],
        lossless: false,
        may_refuse: false,
    },
    k("mkv", "libopus", false),
    k("mp4", "libopus", false),
    k("mkv", "libmp3lame", false),
    k("mp4", "libmp3lame", false),
    Kind {
        extra: &["-bits_per_raw_sample", "24"],
        ..k("mkv", "flac", true)
    },
    Kind {
        extra: &["-bits_per_raw_sample", "24"],
        ..k("mp4", "flac", true)
    },
    Kind {
        extra: &["-sample_fmt", "s32"],
        may_refuse: true,
        ..k("mp4", "flac", true)
    },
    k("mkv", "alac", true),
    k("mp4", "alac", true),
    Kind {
        ext: "mkv",
        codec: "libvorbis",
        extra: &[],
        lossless: false,
        may_refuse: true,
    },
    Kind {
        ext: "mkv",
        codec: "libfdk_aac",
        extra: &["-profile:a", "aac_he"],
        lossless: false,
        may_refuse: false,
    },
    k("mov", "aac", false),
    k("3gp", "aac", false),
    Kind {
        ext: "3gp",
        codec: "libopencore_amrnb",
        extra: &["-ac", "1", "-ar", "8000"],
        lossless: false,
        may_refuse: true,
    },
    refusable("m2ts", "aac"),
    refusable("m2ts", "ac3"),
    refusable("m2ts", "pcm_bluray"),
    k("mkv", "ac3", false),
    k("mp4", "ac3", false),
    k("mkv", "eac3", false),
    k("mp4", "eac3", false),
    k("mp4", "pcm_s16le", true),
    k("mp4", "pcm_s16be", true),
    k("mp4", "pcm_s24le", true),
    k("mp4", "pcm_s24be", true),
    k("mp4", "pcm_s32le", true),
    k("mp4", "pcm_s32be", true),
    k("mp4", "pcm_f32le", true),
    k("mp4", "pcm_f32be", true),
    k("mkv", "pcm_s16le", true),
    k("mkv", "pcm_s16be", true),
    k("mkv", "pcm_s24le", true),
    k("mkv", "pcm_s24be", true),
    k("mkv", "pcm_s32le", true),
    k("mkv", "pcm_s32be", true),
    k("mkv", "pcm_f32le", true),
    k("mkv", "pcm_alaw", true),
    k("mkv", "pcm_mulaw", true),
    k("mov", "pcm_s16le", true),
    k("mov", "pcm_s16be", true),
    k("mov", "pcm_s24le", true),
    k("mov", "pcm_s24be", true),
    k("mov", "pcm_s32le", true),
    k("mov", "pcm_s32be", true),
    k("mov", "pcm_f32le", true),
    k("mov", "pcm_f32be", true),
    k("mov", "pcm_alaw", true),
    k("mov", "pcm_mulaw", true),
];

#[test]
fn every_recorder_codec_decodes_to_s16_like_ffmpeg_or_is_listed_as_refused() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the codec survey is skipped");
        return;
    }
    let mut refused = Vec::new();
    let mut untested = Vec::new();
    let mut unexpected = Vec::new();
    for kind in KINDS {
        let rate = if kind.codec.contains("law") {
            8_000
        } else {
            48_000
        };
        let profile = kind.extra.last().map_or(String::new(), |p| format!("-{p}"));
        let name = format!("survey-{}{profile}.{}", kind.codec, kind.ext);
        let source = format!(
            "aevalsrc=0.5*sin(2*PI*(200+100*t)*t)|0.4*sin(2*PI*(300+50*t)*t):s={rate}:c=stereo:d=2"
        );
        let mut args = vec!["-f", "lavfi", "-i", &source, "-c:a", kind.codec];
        args.extend_from_slice(kind.extra);
        if kind.ext == "mp4" {
            args.extend_from_slice(&["-strict", "-2"]);
        }
        let Some(path) = ffmpeg_make(&name, &args) else {
            eprintln!(
                "{} in {}: this ffmpeg can't write it, so it went untested",
                kind.codec, kind.ext
            );
            untested.push(format!("{}{profile} in {}", kind.codec, kind.ext));
            continue;
        };
        let stream = probe(&path, &Prefs::default())
            .ok()
            .and_then(|p| p.audio.first().map(|t| t.stream))
            .unwrap_or(0);
        let span = S16Span {
            stream,
            from: 0.0,
            rate,
            mono: false,
        };
        let got = match ours(&path, &span) {
            Ok(got) => got,
            Err(e) => {
                eprintln!("{} in {}: refused: {e}", kind.codec, kind.ext);
                refused.push(format!("{}{profile} in {}", kind.codec, kind.ext));
                if !kind.may_refuse || !matches!(e, Error::Unsupported(_)) {
                    unexpected.push(format!("{}{profile} in {}: {e}", kind.codec, kind.ext));
                }
                continue;
            }
        };
        let want = ffmpeg_s16(&path, stream, 0.0, false, rate);
        let what = format!("{} in {}", kind.codec, kind.ext);
        if kind.lossless {
            assert_eq!(got.len(), want.len(), "{what}");
            let m = best(&got, &want, 2, 0);
            assert!(m.worst <= 1, "{what}: off by {}", m.worst);
        } else {
            assert!(
                got.len().abs_diff(want.len()) <= rate as usize / 10 * 2,
                "{what}"
            );
            let m = best(&got, &want, 2, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB at a lag of {}",
                m.snr,
                m.lag
            );
        }
    }
    let list = |v: &[String]| {
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(", ")
        }
    };
    let said = format!(
        "refused: {}\nuntested: {}\n",
        list(&refused),
        list(&untested)
    );
    eprint!("{said}");
    std::fs::write(dir().join("survey.txt"), said).unwrap();
    assert!(
        unexpected.is_empty(),
        "refused but not ported elsewhere: {}",
        unexpected.join("; ")
    );
}

fn damaged(path: &Path, name: &str, cut: impl Fn(&mut Vec<u8>)) -> PathBuf {
    let mut bytes = std::fs::read(path).unwrap();
    cut(&mut bytes);
    let out = dir().join(format!(
        "{}.{:?}.{name}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&out, bytes).unwrap();
    out
}

#[test]
fn a_corrupt_frame_partway_fails_in_one_line_naming_the_file_and_stream() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the corrupt-frame test is skipped");
        return;
    }
    let Some(path) = ffmpeg_make(
        "two-aac.mka",
        &[
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*(200+100*t)*t)|0.5*sin(2*PI*(300+50*t)*t):s=48000:c=stereo:d=6",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*(500+30*t)*t):s=48000:c=mono:d=6",
            "-map",
            "1:a",
            "-map",
            "0:a",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
        ],
    ) else {
        return;
    };
    let bad = damaged(&path, "corrupt.mka", |b| {
        for k in 4..16 {
            let at = b.len() * k / 20;
            b[at..at + 64].fill(0xff);
        }
    });
    let span = S16Span {
        stream: 1,
        from: 0.0,
        rate: 48_000,
        mono: false,
    };
    let mut before = 0usize;
    let e = decode_s16(&bad, &span, &Prefs::default(), |chunk| {
        before += chunk.len();
        Ok(())
    })
    .unwrap_err();
    assert!(before > 0, "the failure comes after the stream started");
    assert!(matches!(e, Error::Invalid(_)), "{e}");
    one_line(e, &bad, &["stream 1: "]);
    assert!(ours(&path, &span).is_ok());
    let _ = std::fs::remove_file(bad);
}

#[test]
fn a_track_with_no_samples_says_so_rather_than_past_the_end() {
    let wav = dir().join(format!(
        "{}.{:?}.empty.wav",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&36u32.to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, 1, 0]);
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&96_000u32.to_le_bytes());
    b.extend_from_slice(&[2, 0, 16, 0]);
    b.extend_from_slice(b"data");
    b.extend_from_slice(&0u32.to_le_bytes());
    std::fs::write(&wav, b).unwrap();
    let mkv = wav.with_extension("mkv");
    let mut w = pito_codecs::mkv::Writer::create(&mkv).unwrap();
    w.add_track(pito_codecs::mkv::TrackConfig::aac(
        vec![0x11, 0x90],
        48_000,
        2,
    ))
    .unwrap();
    w.finish().unwrap();
    for path in [&wav, &mkv] {
        let span = S16Span {
            stream: probe(path, &Prefs::default()).unwrap().audio[0].stream,
            from: 0.0,
            rate: 16_000,
            mono: true,
        };
        let e = ours(path, &span).unwrap_err();
        let said = e.to_string();
        assert!(!said.contains("past the end"), "{said}");
        one_line(e, path, &["holds no audio samples"]);
        let _ = std::fs::remove_file(path);
    }
}

#[test]
fn a_mov_a_law_entry_with_no_channels_or_a_bad_rate_is_refused_in_one_line() {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: the bad A-law entry test is skipped");
        return;
    }
    let Some(path) = ffmpeg_make(
        "law.mov",
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:r=8000:d=1",
            "-c:a",
            "pcm_alaw",
        ],
    ) else {
        return;
    };
    for (name, at, bytes, said) in [
        ("no-channels.mov", 20, &[0u8, 0][..], "PCM with 0 channels"),
        ("ten-hz.mov", 28, &[0, 10, 0, 0], "PCM at 10 Hz"),
    ] {
        let bad = damaged(&path, name, |b| {
            let i = b.windows(4).position(|w| w == b"alaw").unwrap();
            b[i + at..i + at + bytes.len()].copy_from_slice(bytes);
        });
        let span = S16Span {
            stream: 0,
            from: 0.0,
            rate: 8_000,
            mono: false,
        };
        let e = ours(&bad, &span).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e}");
        one_line(e, &bad, &["stream 0: ", said]);
        let _ = std::fs::remove_file(bad);
    }
}
