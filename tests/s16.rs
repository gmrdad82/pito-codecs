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

use pito_codecs::media::{
    AudioBuffer, Error, Prefs, S16Span, audio_pcm_range, decode_s16, probe, to_s16,
};
use refs::Tone;
use testref::{Clip, Refs, Sound};

const LOSSY_SNR_DB: f64 = 28.0;
const OWN_SNR_DB: f64 = 40.0;
const SOURCE_SNR_DB: f64 = 40.0;

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
    c.sounds = vec![Sound::Fn(refs::first_track), Sound::Fn(refs::second_track)];
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
    let shorter = a.len().min(b.len());
    let (m, n) = (-(reach as i64)..=reach as i64)
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
            (Match { lag, snr, worst }, n)
        })
        .max_by(|p, q| p.0.snr.total_cmp(&q.0.snr))
        .unwrap();
    assert!(
        n * 2 >= shorter && n > 0,
        "{n} samples overlap at the best lag, of {shorter}"
    );
    m
}

fn close_to(what: &str, got: &[i16], want: &[i16], rate: u32, lag: i64) {
    let m = best(got, want, 1, rate as usize / 100);
    assert!(
        m.snr >= LOSSY_SNR_DB,
        "{what}: {:.1} dB against the source at a lag of {}",
        m.snr,
        m.lag
    );
    assert!(
        m.lag.abs() <= lag,
        "{what}: starts {} frames off the source's file time",
        m.lag
    );
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
        let path = r.path(&refs::delayed(ext));
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
                got.len().abs_diff(expect) <= rate as usize / 100,
                "{what}: {} samples, about {expect} expected",
                got.len()
            );
            let want = source(refs::tone_b, 1, from - refs::DELAYED, rate, true, expect);
            close_to(&what, &got, &want, rate, rate as i64 / 2000);
        }
        let from = refs::DELAYED_PAST;
        let span = S16Span {
            stream: 1,
            from,
            rate: 16_000,
            mono: true,
        };
        let e = ours(&path, &span).unwrap_err();
        one_line(e, &path, &["stream 1", "past the end", &format!("{from}")]);
        let longer = ours(&path, &S16Span { stream: 0, ..span }).unwrap();
        let left = ((refs::DELAYED_LONG - from) * 16_000.0) as usize;
        assert!(
            longer.len().abs_diff(left) <= 160,
            "{ext}: {}",
            longer.len()
        );
    }
}

#[test]
fn a_mixed_obs_recording_decodes_every_track_against_its_source() {
    let r = open();
    for (ext, third) in refs::MIXED_THIRD {
        let path = r.path(&refs::mixed(ext));
        let p = probe(&path, &Prefs::default()).unwrap();
        let streams: Vec<(usize, u16)> = p.audio.iter().map(|t| (t.stream, t.channels)).collect();
        assert_eq!(streams, [(0, 2), (1, 1), (2, 1)], "{ext} with {third}");
        let rate = refs::LOW_RATE;
        for (stream, t) in refs::mixed_tracks(third).iter().enumerate() {
            let what = format!("{ext} stream {stream} ({})", t.codec);
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
                got.len().abs_diff(expect) <= rate as usize / 100,
                "{what}: {} against about {expect}",
                got.len()
            );
            let want = source(t.tone, t.channels, from, rate, true, expect);
            close_to(&what, &got, &want, rate, rate as i64 / 1000);
        }
    }
}

#[test]
fn a_five_one_track_downmixes_to_mono_by_the_downmix_table() {
    let r = open();
    for (ext, codec) in refs::FIVE_ONE_CODECS {
        let path = r.path(&refs::five_one(ext, codec));
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
                got.len().abs_diff(expect) <= rate as usize / 100,
                "{what}: {} against about {expect}",
                got.len()
            );
            let mut six = AudioBuffer::new(rate, 6);
            six.samples = testref::pcm(Sound::Fn(refs::five_one_at), rate, 6, 0, expect);
            let want = testref::s16_of(&pito_codecs::aac::mono(&six).samples);
            close_to(&what, &got, &want, rate, rate as i64 / 1000);
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
        assert!(
            c.len() <= 2 * 48_000,
            "a chunk of {} stereo frames",
            c.len() / 2
        );
        Ok(())
    })
    .unwrap();
    assert!(chunks >= 6, "{chunks} chunks");
    assert!(frames.abs_diff(6 * 48_000) <= 2_400, "{frames}");
}

#[test]
fn every_recorder_codec_decodes_to_s16_against_its_source_or_is_listed_as_refused() {
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
            (refs::LOW_RATE, kind.mono())
        };
        let ch = kind.channels();
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
            let source =
                testref::s16_of(&testref::pcm(refs::SURVEY_SOUND, rate, 2, 0, kind.frames()));
            let want: Vec<i16> = match kind.codec {
                "pcm_alaw" => source
                    .iter()
                    .map(|&s| refs::alaw_value(refs::alaw(s)))
                    .collect(),
                "pcm_mulaw" => source
                    .iter()
                    .map(|&s| refs::mulaw_value(refs::mulaw(s)))
                    .collect(),
                _ => source,
            };
            assert_eq!(got.len(), want.len(), "{what}");
            let m = best(&got, &want, 2, 0);
            assert!(m.worst <= 1, "{what}: off by {}", m.worst);
        } else {
            let frames = (kind.seconds() * f64::from(rate)).round() as usize;
            assert!(
                got.len().abs_diff(frames * ch) <= rate as usize / 100 * ch,
                "{what}: {} samples, {} expected",
                got.len(),
                frames * ch
            );
            let want = source(refs::survey_at, ch as u16, 0.0, rate, false, frames);
            let m = best(&got, &want, ch, rate as usize / 100);
            assert!(
                m.snr >= LOSSY_SNR_DB,
                "{what}: {:.1} dB against the source at a lag of {}",
                m.snr,
                m.lag
            );
            assert!(
                m.lag.abs() <= rate as i64 / 1000,
                "{what}: starts {} frames off the source's file time",
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
    c.sounds = vec![Sound::Fn(refs::mono_track), Sound::Fn(refs::first_track)];
    let path = c.make(refs::AREA, "two-aac.mka");
    let r = pito_codecs::mkv::Reader::open(&path).unwrap();
    let frames = &r.tracks[1].samples;
    let hits: Vec<(usize, usize)> = frames
        .iter()
        .skip(frames.len() / 3)
        .step_by(7)
        .take(4)
        .map(|s| (s.offset as usize, s.size as usize))
        .collect();
    let bad = damaged(&path, "corrupt.mka", |b| {
        for &(at, n) in &hits {
            b[at..at + n.min(64)].fill(0xff);
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

#[test]
fn the_recorder_table_holds_every_file_the_tests_read() {
    use std::collections::HashSet;
    let fixtures = refs::fixtures();
    let files: HashSet<&str> = fixtures.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(files.len(), fixtures.len(), "a fixture file name twice");
    let mut wanted = vec![refs::REFUSED.to_string()];
    for ext in ["mkv", "mp4"] {
        wanted.push(refs::delayed(ext));
    }
    for (ext, _) in refs::MIXED_THIRD {
        wanted.push(refs::mixed(ext));
    }
    for (ext, codec) in refs::FIVE_ONE_CODECS {
        wanted.push(refs::five_one(ext, codec));
    }
    for kind in refs::KINDS {
        wanted.push(kind.file());
    }
    for w in &wanted {
        assert!(
            files.contains(w.as_str()),
            "the tests read {w}, the table makes no such file"
        );
    }
    assert_eq!(
        wanted.len(),
        fixtures.len(),
        "the table makes files no test reads"
    );
    for f in &fixtures {
        assert!(!f.tracks.is_empty(), "{}", f.file);
        for t in &f.tracks {
            assert!(
                t.rate > 0 && t.seconds > 0.0 && t.channels > 0,
                "{}",
                f.file
            );
            assert!(t.frames() > 0, "{}", f.file);
        }
    }
    let law = refs::KINDS
        .iter()
        .find(|k| k.file() == refs::LAW_MOV)
        .unwrap();
    assert!(law.law() && law.lossless);
    for kind in refs::KINDS {
        assert_eq!(
            kind.channels(),
            if kind.mono() { 1 } else { 2 },
            "{}",
            kind.label()
        );
        if kind.lossless {
            assert!(!kind.mono(), "{}", kind.label());
        }
        assert_eq!(
            kind.fixture().tracks[0].frames(),
            kind.frames(),
            "{}",
            kind.label()
        );
    }
    for codec in refs::STEREO {
        assert!(
            refs::KINDS.iter().any(|k| k.codec == codec && !k.mono()),
            "no stereo {codec} in the survey"
        );
    }
    assert!(
        refs::KINDS.iter().any(|k| !k.lossless && k.mono()),
        "no mono lossy kind left"
    );
    let (start, end) = (refs::DELAYED, refs::DELAYED + refs::DELAYED_SHORT);
    for from in refs::DELAYED_FROM {
        assert!(
            from > start && from + refs::WINDOW <= end,
            "{from} outside {start}..{end}"
        );
    }
    assert!(refs::DELAYED_PAST > end && refs::DELAYED_PAST < refs::DELAYED_LONG);
    const { assert!(refs::MIXED_FROM + refs::WINDOW <= refs::MIXED) };
    const { assert!(refs::WINDOW <= refs::FIVE_ONE_SECONDS) };
}

#[test]
fn g711_by_formula_matches_the_published_codes_and_round_trips() {
    assert_eq!(refs::alaw_value(0x55), -8);
    assert_eq!(refs::alaw_value(0xd5), 8);
    assert_eq!(refs::alaw_value(0xaa), 32_256);
    assert_eq!(refs::alaw_value(0x2a), -32_256);
    assert_eq!(refs::mulaw_value(0xff), 0);
    assert_eq!(refs::mulaw_value(0x7f), 0);
    assert_eq!(refs::mulaw_value(0x80), 32_124);
    assert_eq!(refs::mulaw_value(0x00), -32_124);
    for code in 0..=255u8 {
        let a = refs::alaw_value(code);
        assert_eq!(refs::alaw_value(refs::alaw(a)), a, "A-law {code:#04x}");
        let u = refs::mulaw_value(code);
        assert_eq!(refs::mulaw_value(refs::mulaw(u)), u, "mu-law {code:#04x}");
    }
    for s in [i16::MIN, -1000, -1, 0, 1, 999, i16::MAX] {
        let a = i32::from(refs::alaw_value(refs::alaw(s)));
        let u = i32::from(refs::mulaw_value(refs::mulaw(s)));
        let tol = (i32::from(s).abs() / 16).max(140);
        assert!((a - i32::from(s)).abs() <= tol, "A-law {s}: {a}");
        assert!((u - i32::from(s)).abs() <= tol, "mu-law {s}: {u}");
    }
}
