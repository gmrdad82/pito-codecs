#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "aac",
    feature = "mp4",
    feature = "mkv",
    feature = "opus",
    feature = "mp3",
    feature = "flac",
    feature = "alac",
    feature = "ac3"
))]

#[path = "../src/testref.rs"]
mod testref;

#[allow(dead_code)]
#[path = "../src/s16/refs.rs"]
mod refs;

use std::path::{Path, PathBuf};

use pito_codecs::media::{Error, Prefs, S16Span, audio_pcm_range, decode_s16, probe, to_s16};
use testref::{Clip, Refs, Sound};

const LOSSY_SNR_DB: f64 = 30.0;
const OWN_SNR_DB: f64 = 40.0;
const SOURCE_SNR_DB: f64 = 20.0;

type Tone = fn(u64, u16, u32) -> f32;

fn scratch() -> PathBuf {
    testref::scratch(refs::AREA)
}

fn open() -> Refs {
    Refs::open(refs::AREA)
}

fn clip() -> Clip {
    let mut c = Clip::new((128, 96), (10, 1), 60);
    c.kbps = 192;
    c
}

fn two_tracks(ext: &str) -> PathBuf {
    let mut c = clip();
    c.sounds = vec![
        Sound::Fn(refs::first_track),
        Sound::Fn(refs::second_track),
    ];
    c.make(refs::AREA, &format!("two-tracks.{ext}"))
}

fn mono_track(ext: &str) -> PathBuf {
    let mut c = clip();
    c.sounds = vec![Sound::Fn(refs::mono_track)];
    c.rate = 44_100;
    c.channels = 1;
    c.kbps = 96;
    c.make(refs::AREA, &format!("mono-44k.{ext}"))
}

fn source(tone: Tone, channels: u16, from: f64, rate: u32, mono: bool, frames: usize) -> Vec<i16> {
    let start = (from * f64::from(rate)).round() as u64;
    let pcm = testref::pcm(Sound::Fn(tone), rate, channels, start, frames);
    if !mono || channels == 1 {
        return testref::s16_of(&pcm);
    }
    let mixed: Vec<f32> = pcm
        .chunks_exact(usize::from(channels))
        .map(|f| f.iter().sum::<f32>() / f32::from(channels))
        .collect();
    testref::s16_of(&mixed)
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
fn both_tracks_of_a_two_track_file_match_the_source_from_any_start_mono_or_not() {
    let prefs = Prefs::default();
    for ext in ["mkv", "mp4"] {
        let path = two_tracks(ext);
        let single = mono_track(ext);
        let tracks = |p: &Path| -> Vec<(usize, usize, u16)> {
            probe(p, &prefs)
                .unwrap()
                .audio
                .iter()
                .map(|t| (t.stream, t.index, t.channels))
                .collect()
        };
        assert_eq!(tracks(&path), [(1, 0, 2), (2, 1, 2)], "{ext}");
        assert_eq!(tracks(&single), [(1, 0, 1)], "{ext}");
        let cases: [(&Path, usize, usize, u16, Tone); 3] = [
            (&path, 1, 0, 2, refs::first_track),
            (&path, 2, 1, 2, refs::second_track),
            (&single, 1, 0, 1, refs::mono_track),
        ];
        for (file, stream, index, source_channels, tone) in cases {
            for from in [0.0, 2.5] {
                for rate in [16_000, 48_000] {
                    for mono in [true, false] {
                        let what = format!(
                            "{ext} {source_channels} channel stream {stream} from {from} s at {rate} Hz, mono {mono}"
                        );
                        let span = S16Span {
                            stream,
                            from,
                            rate,
                            mono,
                        };
                        let got = ours(file, &span).unwrap();
                        let ch = if mono { 1 } else { source_channels as usize };
                        assert_eq!(got.len() % ch, 0, "{what}");
                        let slack = (rate as usize / 20) * ch;
                        let expect = ((6.0 - from) * rate as f64) as usize * ch;
                        assert!(got.len().abs_diff(expect) <= slack, "{what}: {}", got.len());
                        let want = source(tone, source_channels, from, rate, mono, got.len() / ch);
                        let m = best(&got, &want, ch, rate as usize / 100);
                        assert!(
                            m.snr >= SOURCE_SNR_DB,
                            "{what}: {:.1} dB against the source at a lag of {} frames",
                            m.snr,
                            m.lag
                        );
                        assert!(
                            m.lag.abs() <= rate as i64 / 1000,
                            "{what}: starts {} frames off the source's file time (SNR {:.1} dB)",
                            m.lag,
                            m.snr
                        );
                        let own = audio_pcm_range(
                            file,
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
        let low = |stream| {
            ours(
                &path,
                &S16Span {
                    stream,
                    from: 0.0,
                    rate: 16_000,
                    mono: true,
                },
            )
            .unwrap()
        };
        let m = best(&low(1), &low(2), 1, 160);
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
    let path = two_tracks("mkv");
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
    let refused = open().path(refs::REFUSED);
    let e = ours(&refused, &S16Span { stream: 0, ..span }).unwrap_err();
    assert!(matches!(e, Error::Unsupported(_)), "{e}");
    one_line(e, &refused, &["stream 0", "WMA v2", "0x0161"]);
    let silent = Clip::new((128, 96), (10, 1), 10)
        .video_only()
        .make(refs::AREA, "no-audio.mkv");
    let e = ours(&silent, &S16Span { stream: 0, ..span }).unwrap_err();
    one_line(e, &silent, &["no audio stream 0", "it has no audio"]);
}

#[test]
fn a_delayed_short_track_starts_at_file_time_and_ends_on_its_own() {
    let r = open();
    for ext in ["mkv", "mp4"] {
        let name = refs::delayed(ext);
        let path = r.path(&name);
        let rate = refs::LOW_RATE;
        for from in refs::DELAYED_FROM {
            let what = format!("{ext} delayed stream 1 from {from} s");
            let span = S16Span {
                stream: 1,
                from,
                rate,
                mono: true,
            };
            let got = ours(&path, &span).unwrap();
            let expect = ((refs::DELAYED + refs::DELAYED_SHORT - from) * rate as f64) as usize;
            assert!(
                got.len().abs_diff(expect) <= rate as usize / 20,
                "{what}: {} samples, about {expect} expected",
                got.len()
            );
            let want = r.i16s(&refs::window(name.clone(), 1, from, rate).name());
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
        let from = refs::DELAYED_LONG - 0.2;
        let span = S16Span {
            stream: 1,
            from,
            rate: 16_000,
            mono: true,
        };
        let e = ours(&path, &span).unwrap_err();
        one_line(e, &path, &["stream 1", "past the end", &format!("{from}")]);
        let longer = ours(&path, &S16Span { stream: 0, ..span }).unwrap();
        assert!(
            longer.len().abs_diff(3_200) <= 800,
            "{ext}: {}",
            longer.len()
        );
    }
}

#[test]
fn a_mixed_obs_recording_decodes_every_track_like_ffmpeg() {
    let r = open();
    for (ext, third) in refs::MIXED_THIRD {
        let name = refs::mixed(ext);
        let path = r.path(&name);
        let p = probe(&path, &Prefs::default()).unwrap();
        let streams: Vec<(usize, u16)> = p.audio.iter().map(|t| (t.stream, t.channels)).collect();
        assert_eq!(streams, [(0, 2), (1, 1), (2, 1)], "{ext} with {third}");
        let rate = refs::LOW_RATE;
        for (stream, _) in streams {
            let what = format!("{ext} stream {stream}");
            let from = refs::MIXED_FROM;
            let span = S16Span {
                stream,
                from,
                rate,
                mono: true,
            };
            let got = ours(&path, &span).unwrap();
            let expect = ((refs::MIXED - from) * rate as f64) as usize;
            assert!(
                got.len().abs_diff(expect) <= rate as usize / 20,
                "{what}: {} against about {expect}",
                got.len()
            );
            let want = r.i16s(&refs::window(name.clone(), stream, from, rate).name());
            let m = best(&got, &want, 1, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB at a lag of {}",
                m.snr,
                m.lag
            );
            assert!(
                m.lag.abs() <= rate as i64 / 1000,
                "{what}: starts {} frames off ffmpeg",
                m.lag
            );
        }
    }
}

#[test]
fn a_five_one_track_downmixes_to_mono_like_ffmpeg() {
    let r = open();
    for (ext, codec) in refs::FIVE_ONE_CODECS {
        let name = refs::five_one(ext, codec);
        let path = r.path(&name);
        let stream = probe(&path, &Prefs::default()).unwrap().audio[0].stream;
        for rate in refs::FIVE_ONE_RATES {
            let what = format!("5.1 {codec} in {ext} to mono at {rate} Hz");
            let span = S16Span {
                stream,
                from: 0.0,
                rate,
                mono: true,
            };
            let got = ours(&path, &span).unwrap();
            let expect = (refs::FIVE_ONE_SECONDS * rate as f64) as usize;
            assert!(
                got.len().abs_diff(expect) <= rate as usize / 20,
                "{what}: {} against about {expect}",
                got.len()
            );
            let want = r.i16s(&refs::window(name.clone(), stream, 0.0, rate).name());
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
    let path = two_tracks("mp4");
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

#[test]
fn every_recorder_codec_decodes_to_s16_like_ffmpeg_or_is_listed_as_refused() {
    let r = open();
    let mut refused = Vec::new();
    let mut unexpected = Vec::new();
    for kind in refs::KINDS {
        let what = kind.label();
        let path = r.path(&kind.file());
        let stream = probe(&path, &Prefs::default())
            .ok()
            .and_then(|p| p.audio.first().map(|t| t.stream))
            .unwrap_or(0);
        let (rate, mono) = if kind.lossless {
            (kind.rate(), false)
        } else {
            (refs::LOW_RATE, true)
        };
        let span = S16Span {
            stream,
            from: 0.0,
            rate,
            mono,
        };
        let got = match ours(&path, &span) {
            Ok(got) => got,
            Err(e) => {
                refused.push(what.clone());
                if !kind.may_refuse || !matches!(e, Error::Unsupported(_)) {
                    unexpected.push(format!("{what}: {e}"));
                }
                continue;
            }
        };
        if kind.lossless {
            let want = if kind.law() {
                r.i16s(&kind.reference().name())
            } else {
                testref::s16_of(&testref::pcm(refs::SURVEY_SOUND, rate, 2, 0, kind.frames()))
            };
            assert_eq!(got.len(), want.len(), "{what}");
            let m = best(&got, &want, 2, 0);
            assert!(m.worst <= 1, "{what}: off by {}", m.worst);
        } else {
            let want = r.i16s(&kind.reference().name());
            assert!(
                got.len().abs_diff(want.len()) <= rate as usize / 20,
                "{what}: {} against ffmpeg's {}",
                got.len(),
                want.len()
            );
            let m = best(&got, &want, 1, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB at a lag of {}",
                m.snr,
                m.lag
            );
        }
    }
    let list = if refused.is_empty() {
        "none".to_string()
    } else {
        refused.join(", ")
    };
    testref::write_atomic(
        &scratch().join("survey.txt"),
        format!("refused: {list}\n").as_bytes(),
    )
    .unwrap();
    assert!(
        unexpected.is_empty(),
        "refused but not ported elsewhere: {}",
        unexpected.join("; ")
    );
}

fn damaged(path: &Path, name: &str, cut: impl Fn(&mut Vec<u8>)) -> PathBuf {
    let mut bytes = std::fs::read(path).unwrap();
    cut(&mut bytes);
    let out = scratch().join(format!(
        "{}.{:?}.{name}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&out, bytes).unwrap();
    out
}

#[test]
fn a_corrupt_frame_partway_fails_in_one_line_naming_the_file_and_stream() {
    let mut c = clip().audio_only();
    c.sounds = vec![
        Sound::Fn(refs::mono_track),
        Sound::Fn(refs::first_track),
    ];
    let path = c.make(refs::AREA, "two-aac.mka");
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
    let wav = scratch().join(format!(
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
    let path = open().path(refs::LAW_MOV);
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
