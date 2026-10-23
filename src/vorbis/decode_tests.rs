use super::ogg::{page_crc, span};
use super::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const MAX_DIFF: f32 = 1e-6;
const MAX_RMS: f64 = 1e-7;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-vorbis-decode");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn encoders() -> String {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

struct Case {
    name: &'static str,
    encoder: &'static str,
    rate: u32,
    channels: u16,
    quality: &'static str,
    clicks: bool,
}

const SECONDS: f64 = 0.7;

const HIGH: [Case; 2] = [
    Case {
        name: "stereo_96k_q3",
        encoder: "libvorbis",
        rate: 96000,
        channels: 2,
        quality: "3",
        clicks: true,
    },
    Case {
        name: "stereo_88k_q3",
        encoder: "libvorbis",
        rate: 88200,
        channels: 2,
        quality: "3",
        clicks: true,
    },
];

const CASES: [Case; 12] = [
    Case {
        name: "mono_22k_q0",
        encoder: "libvorbis",
        rate: 22050,
        channels: 1,
        quality: "0",
        clicks: false,
    },
    Case {
        name: "stereo_44k_q3",
        encoder: "libvorbis",
        rate: 44100,
        channels: 2,
        quality: "3",
        clicks: false,
    },
    Case {
        name: "stereo_48k_q9",
        encoder: "libvorbis",
        rate: 48000,
        channels: 2,
        quality: "9",
        clicks: false,
    },
    Case {
        name: "stereo_32k_qm1",
        encoder: "libvorbis",
        rate: 32000,
        channels: 2,
        quality: "-1",
        clicks: false,
    },
    Case {
        name: "surround_48k_q5",
        encoder: "libvorbis",
        rate: 48000,
        channels: 6,
        quality: "5",
        clicks: false,
    },
    Case {
        name: "surround_22k_q1",
        encoder: "libvorbis",
        rate: 22050,
        channels: 6,
        quality: "1",
        clicks: false,
    },
    Case {
        name: "native_stereo_44k",
        encoder: "vorbis",
        rate: 44100,
        channels: 2,
        quality: "5",
        clicks: false,
    },
    Case {
        name: "c3_44k_q4",
        encoder: "libvorbis",
        rate: 44100,
        channels: 3,
        quality: "4",
        clicks: false,
    },
    Case {
        name: "quad_44k_q4",
        encoder: "libvorbis",
        rate: 44100,
        channels: 4,
        quality: "4",
        clicks: false,
    },
    Case {
        name: "c5_48k_q4",
        encoder: "libvorbis",
        rate: 48000,
        channels: 5,
        quality: "4",
        clicks: false,
    },
    Case {
        name: "c61_48k_q4",
        encoder: "libvorbis",
        rate: 48000,
        channels: 7,
        quality: "4",
        clicks: false,
    },
    Case {
        name: "c71_48k_q4",
        encoder: "libvorbis",
        rate: 48000,
        channels: 8,
        quality: "4",
        clicks: false,
    },
];

fn source(c: &Case, seconds: f64) -> String {
    let tones: Vec<String> = (0..c.channels)
        .map(|i| {
            let f = 220.0 * (i + 1) as f64 + 37.0;
            let a = 0.1 + 0.08 * i as f64;
            let click = if c.clicks {
                "+0.6*lt(mod(t+0.043\\,0.31)\\,0.0015)*sin(9000*2*PI*t)"
            } else {
                ""
            };
            format!(
                "{a}*sin({f}*2*PI*t)+0.1*sin({}*2*PI*t)*exp(-3*t){click}",
                f * 7.3
            )
        })
        .collect();
    let layout = match c.channels {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "quad",
        5 => "5.0",
        6 => "5.1",
        7 => "6.1",
        _ => "7.1",
    };
    format!(
        "aevalsrc={}:s={}:d={seconds}:c={layout}",
        tones.join("|"),
        c.rate
    )
}

fn encode(c: &Case, ext: &str) -> Option<PathBuf> {
    encode_for(c, ext, SECONDS)
}

fn encode_for(c: &Case, ext: &str, seconds: f64) -> Option<PathBuf> {
    let name = if seconds == SECONDS {
        c.name.to_string()
    } else {
        format!("{}_{seconds}s", c.name)
    };
    let ogg = if ext == "mp4" {
        Some(encode_for(c, "ogg", seconds)?)
    } else {
        None
    };
    let path = crate::fixture::once(&dir().join(format!("{name}.{ext}")), |part| {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
        match &ogg {
            Some(ogg) => {
                cmd.arg("-i").arg(ogg).args(["-c", "copy", "-strict", "-2"]);
            }
            None => {
                cmd.args(["-f", "lavfi", "-i"])
                    .arg(source(c, seconds))
                    .args(["-c:a", c.encoder, "-strict", "-2"]);
                if c.encoder == "libvorbis" {
                    cmd.args(["-q:a", c.quality]);
                }
            }
        }
        if !cmd.arg(part).status().unwrap().success() {
            let _ = std::fs::remove_file(part);
            assert_eq!(ext, "mp4", "ffmpeg encode {} {ext}", c.name);
        }
    });
    path.exists().then_some(path)
}

fn ffmpeg_pcm(path: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "ffmpeg decode {}", path.display());
    out.stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn cases() -> Vec<&'static Case> {
    cases_of(&CASES)
}

fn cases_of(all: &'static [Case]) -> Vec<&'static Case> {
    if !has_ffmpeg() {
        eprintln!("skipped vorbis decode fixtures: ffmpeg not on PATH");
        return Vec::new();
    }
    let list = encoders();
    all.iter()
        .filter(|c| {
            let ok = list
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(c.encoder));
            if !ok {
                eprintln!("skipped {}: ffmpeg has no {} encoder", c.name, c.encoder);
            }
            ok
        })
        .collect()
}

fn compare(name: &str, ours: &[f32], theirs: &[f32]) -> (f32, f64) {
    let n = ours.len().min(theirs.len());
    assert!(n > 0, "{name}: nothing to compare");
    let mut max = 0f32;
    let mut sum = 0f64;
    for (a, b) in ours[..n].iter().zip(&theirs[..n]) {
        let d = (a - b).abs();
        max = max.max(d);
        sum += (d as f64) * (d as f64);
    }
    let rms = (sum / n as f64).sqrt();
    eprintln!("{name}: max {max:e} rms {rms:e} over {n} samples");
    assert!(max <= MAX_DIFF, "{name}: max diff {max} over {MAX_DIFF}");
    assert!(rms <= MAX_RMS, "{name}: rms {rms} over {MAX_RMS}");
    (max, rms)
}

fn expected_frames(c: &Case, ogg: &Path) -> usize {
    if c.encoder == "libvorbis" {
        (SECONDS * c.rate as f64).round() as usize
    } else {
        let parsed = OggVorbis::parse(&std::fs::read(ogg).unwrap()).unwrap();
        parsed.samples().unwrap() as usize
    }
}

#[test]
fn ogg_vorbis_decodes_to_ffmpegs_samples_with_exact_lengths() {
    for c in cases() {
        let Some(path) = encode(c, "ogg") else {
            continue;
        };
        let data = std::fs::read(&path).unwrap();
        let ogg = OggVorbis::parse(&data).unwrap();
        assert!(ogg.warnings.is_empty(), "{}: {:?}", c.name, ogg.warnings);
        let ours = ogg.decode().unwrap();
        assert_eq!(ours.rate, c.rate, "{}", c.name);
        assert_eq!(ours.channels, c.channels, "{}", c.name);
        assert_eq!(
            ours.frames(),
            expected_frames(c, &path),
            "{}: frames",
            c.name
        );
        assert_eq!(ogg.samples().unwrap() as usize, ours.frames(), "{}", c.name);
        let theirs = ffmpeg_pcm(&path);
        let ch = c.channels as usize;
        let long = 1usize << ogg.decoder().unwrap().ident.blocksize_1;
        eprintln!(
            "{}: ffmpeg's Ogg demuxer gives {} frames, the granules {}",
            c.name,
            theirs.len() / ch,
            ours.frames()
        );
        assert!(
            (theirs.len() / ch).abs_diff(ours.frames()) <= long / 2,
            "{}: ffmpeg's Ogg demuxer gives {} frames, ours {}",
            c.name,
            theirs.len() / ch,
            ours.frames()
        );
        compare(c.name, &ours.samples, &theirs);
    }
}

#[test]
fn every_channel_lands_in_ffmpegs_order() {
    let mut seen = Vec::new();
    for c in cases().into_iter().filter(|c| c.channels >= 3) {
        let Some(path) = encode(c, "ogg") else {
            continue;
        };
        seen.push(c.channels);
        let ours = decode(&std::fs::read(&path).unwrap()).unwrap();
        let theirs = ffmpeg_pcm(&path);
        let stride = c.channels as usize;
        for ch in 0..stride {
            let energy = |s: &[f32], skip: usize| -> f64 {
                s.iter()
                    .skip(ch)
                    .step_by(stride)
                    .skip(skip)
                    .take(4000)
                    .map(|&v| (v as f64) * (v as f64))
                    .sum()
            };
            let a = energy(&ours.samples, 0);
            let b = energy(&theirs, 0);
            assert!(
                (a - b).abs() <= 1e-3 * b.max(1e-9),
                "{} channel {ch}: {a} vs {b}",
                c.name
            );
        }
    }
    if !seen.is_empty() {
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, [3, 4, 5, 6, 7, 8]);
    }
}

#[test]
fn the_first_packet_primes_and_later_packets_follow_the_blocksizes() {
    let Some(c) = cases().into_iter().find(|c| c.name == "stereo_44k_q3") else {
        return;
    };
    let path = encode(c, "ogg").unwrap();
    let ogg = OggVorbis::parse(&std::fs::read(&path).unwrap()).unwrap();
    let mut dec = ogg.decoder().unwrap();
    assert!(!dec.primed());
    let first = ogg.packet(0);
    let n0 = dec.blocksize(first).unwrap();
    let (_, flags0) = dec.shape(first).unwrap();
    assert!(flags0.is_none(), "this stream starts on a short block");
    assert_eq!(dec.lead_in(first).unwrap(), n0 / 2);
    let short = 1usize << dec.ident.blocksize_0;
    let tail = |p: &[u8]| match dec.shape(p).unwrap() {
        (n, Some((_, false))) => n / 4 - short / 4,
        _ => 0,
    };
    let last_tail = tail(ogg.packet(ogg.packets.len() - 1));
    let mut out = Vec::new();
    assert_eq!(dec.decode_into(first, &mut out).unwrap(), 0);
    assert!(out.is_empty());
    assert!(dec.primed());
    assert_eq!(dec.decode_into(&[], &mut out).unwrap(), 0);
    let mut total = 0;
    let mut sum_formula = 0;
    let mut prev = n0;
    for i in 1..ogg.packets.len() {
        let p = ogg.packet(i);
        let n = dec.blocksize(p).unwrap();
        sum_formula += prev / 4 + n / 4;
        prev = n;
        total += dec.decode_into(p, &mut out).unwrap();
        assert_eq!(out.len(), total * 2);
    }
    assert_eq!(total, sum_formula + last_tail);
    dec.reset();
    assert!(!dec.primed());
    let mut aligned = Vec::new();
    let got = dec.decode_aligned(ogg.packet(0), &mut aligned).unwrap();
    assert_eq!(got, n0 / 2);
    assert!(aligned.iter().all(|&v| v == 0.0));
}

#[cfg(feature = "audio")]
mod front {
    use super::*;
    use crate::media::{Prefs, audio_pcm, audio_pcm_range, probe};

    fn whole(path: &Path, rate: u32, ch: u16) -> crate::media::AudioBuffer {
        audio_pcm_range(path, 0, 0.0, f64::INFINITY, rate, ch, &Prefs::default()).unwrap()
    }

    #[test]
    fn a_bare_ogg_reads_through_probe_and_audio_pcm_range() {
        for c in cases() {
            let path = encode(c, "ogg").unwrap();
            let oga = crate::fixture::once(&path.with_extension("oga"), |part| {
                std::fs::copy(&path, part).unwrap();
            });
            for p in [&path, &oga] {
                let pr = probe(p, &Prefs::default()).unwrap();
                let a = &pr.audio[0];
                assert_eq!(a.codec, "vorbis", "{}", c.name);
                assert!(a.native(), "{}", c.name);
                assert_eq!((a.channels, a.rate), (c.channels, c.rate), "{}", c.name);
                assert!(
                    (pr.duration - expected_frames(c, p) as f64 / c.rate as f64).abs() < 1e-6,
                    "{}: {}",
                    c.name,
                    pr.duration
                );
                let b = whole(p, c.rate, c.channels);
                assert_eq!(b.frames(), expected_frames(c, p), "{}", c.name);
                let direct = decode(&std::fs::read(p).unwrap()).unwrap();
                assert_eq!(b.samples, direct.samples, "{}", c.name);
                if c.channels <= 2 {
                    let pcm = audio_pcm(p, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                    assert_eq!(pcm.samples, b.samples, "{}", c.name);
                }
            }
            let from = 0.25;
            let to = 0.5;
            let part =
                audio_pcm_range(&path, 0, from, to, c.rate, c.channels, &Prefs::default()).unwrap();
            let at = |t: f64| (t * c.rate as f64).round() as usize * c.channels as usize;
            let direct = decode(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(part.samples, direct.samples[at(from)..at(to)], "{}", c.name);
        }
    }

    #[test]
    fn webm_vorbis_matches_ffmpeg_sample_for_sample_and_in_length() {
        for c in cases().into_iter().filter(|c| c.encoder == "libvorbis") {
            for ext in ["webm", "mkv"] {
                let path = encode(c, ext).unwrap();
                let pr = probe(&path, &Prefs::default()).unwrap();
                assert!(pr.audio[0].native(), "{} {ext}", c.name);
                assert!(pr.unsupported().is_empty(), "{} {ext}", c.name);
                let ours = whole(&path, c.rate, c.channels);
                let theirs = ffmpeg_pcm(&path);
                assert_eq!(
                    ours.samples.len(),
                    theirs.len(),
                    "{} {ext}: frames ours {} ffmpeg {}",
                    c.name,
                    ours.frames(),
                    theirs.len() / c.channels as usize
                );
                assert_eq!(ours.frames(), expected_frames(c, &path), "{} {ext}", c.name);
                compare(&format!("{} {ext}", c.name), &ours.samples, &theirs);
                if c.channels <= 2 {
                    let pcm =
                        audio_pcm(&path, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                    assert_eq!(pcm.samples, ours.samples, "{} {ext}", c.name);
                }
            }
        }
    }

    #[test]
    fn a_webm_window_lines_up_with_the_whole_decode() {
        let Some(c) = cases().into_iter().find(|c| c.name == "stereo_44k_q3") else {
            return;
        };
        let path = encode(c, "webm").unwrap();
        let all = whole(&path, c.rate, 2);
        let (from, to) = (0.3, 0.6);
        let part = audio_pcm_range(&path, 0, from, to, c.rate, 2, &Prefs::default()).unwrap();
        let at = |t: f64| (t * c.rate as f64).round() as usize * 2;
        compare("webm window", &part.samples, &all.samples[at(from)..at(to)]);
        assert_eq!(part.samples.len(), at(to) - at(from));
    }

    #[test]
    fn a_long_webm_decodes_in_pieces_that_join_without_a_seam() {
        let Some(c) = cases().into_iter().find(|c| c.name == "stereo_44k_q3") else {
            return;
        };
        let path = encode_for(c, "webm", 9.0).unwrap();
        let ours = whole(&path, c.rate, 2);
        let theirs = ffmpeg_pcm(&path);
        assert_eq!(ours.samples.len(), theirs.len());
        assert_eq!(ours.frames(), 9 * 44100);
        compare("long webm", &ours.samples, &theirs);
        let (from, to) = (3.9, 4.1);
        let part = audio_pcm_range(&path, 0, from, to, c.rate, 2, &Prefs::default()).unwrap();
        let at = |t: f64| (t * c.rate as f64).round() as usize * 2;
        assert_eq!(part.samples.len(), at(to) - at(from));
        compare("long webm window", &part.samples, &theirs[at(from)..at(to)]);
    }

    #[test]
    fn high_rate_matroska_pieces_and_windows_land_on_ffmpegs_samples() {
        for c in cases_of(&HIGH) {
            for ext in ["webm", "mkv"] {
                let name = format!("{} {ext}", c.name);
                let path = encode_for(c, ext, 9.0).unwrap();
                let ours = whole(&path, c.rate, 2);
                let theirs = ffmpeg_pcm(&path);
                assert_eq!(ours.samples.len(), theirs.len(), "{name}");
                assert_eq!(ours.frames(), 9 * c.rate as usize, "{name}");
                compare(&name, &ours.samples, &theirs);
                let at = |t: f64| (t * c.rate as f64).round() as usize * 2;
                for (from, to) in [(3.9, 4.1), (1.2345, 1.5), (6.013, 6.4), (8.0, 9.0)] {
                    let part =
                        audio_pcm_range(&path, 0, from, to, c.rate, 2, &Prefs::default()).unwrap();
                    assert_eq!(part.samples.len(), at(to) - at(from), "{name} {from}");
                    compare(
                        &format!("{name} window {from}"),
                        &part.samples,
                        &theirs[at(from)..at(to)],
                    );
                }
            }
        }
    }

    #[test]
    fn matroska_vorbis_off_the_millisecond_ends_on_ffmpegs_last_sample() {
        let all: Vec<&Case> = cases()
            .into_iter()
            .filter(|c| c.name == "stereo_44k_q3")
            .chain(cases_of(&HIGH).into_iter().filter(|c| c.rate == 96000))
            .collect();
        for c in all {
            let ogg = encode_for(c, "ogg", 9.0123).unwrap();
            for (ext, copied) in [
                ("webm", false),
                ("mkv", false),
                ("webm", true),
                ("mkv", true),
            ] {
                let name = format!("{} {ext}{}", c.name, if copied { " copied" } else { "" });
                let path = if copied {
                    let to = dir().join(format!("{}_9.0123s_copy.{ext}", c.name));
                    crate::fixture::once(&to, |part| {
                        let ok = Command::new("ffmpeg")
                            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                            .arg(&ogg)
                            .args(["-c", "copy"])
                            .arg(part)
                            .status()
                            .unwrap()
                            .success();
                        assert!(ok, "ffmpeg copy {name}");
                    })
                } else {
                    encode_for(c, ext, 9.0123).unwrap()
                };
                let theirs = ffmpeg_pcm(&path);
                let frames = theirs.len() / 2;
                assert_ne!(frames % (c.rate as usize / 1000), 0, "{name}");
                let ours = whole(&path, c.rate, 2);
                assert_eq!(ours.frames(), frames, "{name}");
                compare(&name, &ours.samples, &theirs);
                let streamed: Vec<f32> =
                    crate::media::audio_stream(&path, 0, 0.0, c.rate, 2, &Prefs::default())
                        .unwrap()
                        .flat_map(|b| b.unwrap().samples)
                        .collect();
                assert_eq!(streamed.len(), theirs.len(), "{name} stream");
                let at = |t: f64| (t * c.rate as f64).round() as usize * 2;
                for (from, to) in [(8.5, f64::INFINITY), (8.9, 9.5), (2.0, f64::INFINITY)] {
                    let part =
                        audio_pcm_range(&path, 0, from, to, c.rate, 2, &Prefs::default()).unwrap();
                    assert_eq!(part.samples.len(), theirs.len() - at(from), "{name} {from}");
                    compare(
                        &format!("{name} to the end from {from}"),
                        &part.samples,
                        &theirs[at(from)..],
                    );
                }
                let tail: Vec<f32> =
                    crate::media::audio_stream(&path, 0, 8.5, c.rate, 2, &Prefs::default())
                        .unwrap()
                        .flat_map(|b| b.unwrap().samples)
                        .collect();
                assert_eq!(tail.len(), theirs.len() - at(8.5), "{name} stream from 8.5");
            }
        }
    }

    #[test]
    fn a_webm_cut_short_ends_on_ffmpegs_last_sample_though_its_header_says_longer() {
        let Some(c) = cases().into_iter().find(|c| c.name == "stereo_44k_q3") else {
            return;
        };
        let full = encode_for(c, "webm", 24.0123).unwrap();
        let path = crate::fixture::once(&dir().join("stereo_44k_q3_cut_short.webm"), |part| {
            let file = std::fs::read(&full).unwrap();
            let clusters: Vec<usize> = file
                .windows(4)
                .enumerate()
                .filter(|(_, w)| *w == [0x1f, 0x43, 0xb6, 0x75])
                .map(|(i, _)| i)
                .collect();
            assert!(clusters.len() > 4, "{} clusters", clusters.len());
            let cluster = clusters[clusters.len() / 2];
            let r = crate::mkv::Reader::open(&full).unwrap();
            let mut s = r.stream();
            let mut data = Vec::new();
            let mut blocks = Vec::new();
            let mut from = 0;
            while s.next_into(&mut data).unwrap().is_some() {
                let i = from
                    + file[from..]
                        .windows(data.len())
                        .position(|w| w == &data[..])
                        .expect("a block's bytes in the file");
                if i > cluster {
                    break;
                }
                blocks.push(i);
                from = i + data.len();
            }
            let at = blocks[blocks.len() - 3];
            let header = |body: usize, id: u8| {
                (1..=8).find_map(|n| {
                    let e = body.checked_sub(n + 1)?;
                    (file[e] == id && file[e + 1].leading_zeros() as usize + 1 == n).then_some(e)
                })
            };
            let cut = header(at - 4, 0xa3)
                .or_else(|| header(header(at - 4, 0xa1)?, 0xa0))
                .expect("the block's header");
            std::fs::write(part, &file[..cut]).unwrap();
        });
        let theirs = ffmpeg_pcm(&path);
        let frames = theirs.len() / 2;
        let rate = c.rate as f64;
        assert!(
            (5.0 * rate..14.0 * rate).contains(&(frames as f64)),
            "{frames}"
        );
        assert_ne!(frames % (c.rate as usize / 1000), 0, "{frames}");
        let r = crate::mkv::Reader::open(&path).unwrap();
        assert!(
            r.duration_seconds() > frames as f64 / rate + 10.0,
            "{}",
            r.duration_seconds()
        );
        let ours = whole(&path, c.rate, 2);
        assert_eq!(ours.frames(), frames);
        compare("webm cut short", &ours.samples, &theirs);
        let at = |t: f64| (t * rate).round() as usize * 2;
        let from = frames as f64 / rate - 0.7;
        let part =
            audio_pcm_range(&path, 0, from, f64::INFINITY, c.rate, 2, &Prefs::default()).unwrap();
        assert_eq!(part.samples.len(), theirs.len() - at(from));
        compare(
            "webm cut short to the end",
            &part.samples,
            &theirs[at(from)..],
        );
    }

    #[test]
    fn a_damaged_block_where_a_window_lands_is_refused_and_a_window_at_the_end_still_fits() {
        let Some(c) = cases_of(&HIGH).into_iter().find(|c| c.rate == 96000) else {
            return;
        };
        let path = encode_for(c, "webm", 9.0).unwrap();
        let theirs = ffmpeg_pcm(&path);
        let at = |t: f64| (t * c.rate as f64).round() as usize * 2;
        let (from, to) = (8.95, 9.0);
        let part = audio_pcm_range(&path, 0, from, to, c.rate, 2, &Prefs::default()).unwrap();
        assert_eq!(part.samples.len(), at(to) - at(from));
        compare("96k webm end", &part.samples, &theirs[at(from)..at(to)]);
        let bad = crate::fixture::once(&dir().join("damaged_96k.webm"), |part| {
            let r = crate::mkv::Reader::open(&path).unwrap();
            let mut s = r.stream();
            let mut data = Vec::new();
            let mut file = std::fs::read(&path).unwrap();
            let mut hit = 0;
            while let Some(b) = s.next_into(&mut data).unwrap() {
                if !(3940..=4000).contains(&b.pts) {
                    continue;
                }
                let i = file
                    .windows(data.len())
                    .position(|w| w == &data[..])
                    .expect("a block's bytes in the file");
                file[i] |= 1;
                hit += 1;
            }
            assert!(hit > 2, "{hit}");
            std::fs::write(part, &file).unwrap();
        });
        let e = audio_pcm_range(&bad, 0, 4.0, 4.2, c.rate, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "invalid data: Vorbis header where an audio packet was expected"
        );
        let fine = audio_pcm_range(&bad, 0, 6.0, 6.2, c.rate, 2, &Prefs::default()).unwrap();
        compare(
            "96k webm past the damage",
            &fine.samples,
            &theirs[at(6.0)..at(6.2)],
        );
    }

    #[test]
    fn mp4_vorbis_reads_through_probe_audio_pcm_and_matches_ffmpeg() {
        for c in cases()
            .into_iter()
            .filter(|c| matches!(c.name, "stereo_44k_q3" | "mono_22k_q0" | "surround_48k_q5"))
        {
            let Some(path) = encode(c, "mp4") else {
                eprintln!(
                    "skipped Vorbis in MP4 {}: ffmpeg would not write one",
                    c.name
                );
                continue;
            };
            let pr = probe(&path, &Prefs::default()).unwrap();
            assert_eq!(pr.audio[0].codec, "vorbis", "{}", c.name);
            assert!(pr.audio[0].native(), "{}", c.name);
            assert!(pr.unsupported().is_empty(), "{}", c.name);
            assert_eq!(
                (pr.audio[0].channels, pr.audio[0].rate),
                (c.channels, c.rate),
                "{}",
                c.name
            );
            let ours = whole(&path, c.rate, c.channels);
            let theirs = ffmpeg_pcm(&path);
            let ogg = path.with_extension("ogg");
            let src = OggVorbis::parse(&std::fs::read(&ogg).unwrap()).unwrap();
            let primed = src.decoder().unwrap().lead_in(src.packet(0)).unwrap();
            let mp4 = crate::mp4::Reader::open(&path).unwrap();
            let t = &mp4.tracks[0];
            assert_eq!(t.edit_offset, -(primed as i64), "{} mp4", c.name);
            let media_end = t.samples.last().map_or(0, |s| s.pts + s.duration as i64) as usize;
            assert_eq!(
                ours.frames(),
                expected_frames(c, &ogg).min(media_end) - primed,
                "{} mp4: the first packet's span is priming",
                c.name
            );
            assert!(
                theirs.len() >= ours.samples.len(),
                "{} mp4: frames ours {} ffmpeg {}",
                c.name,
                ours.frames(),
                theirs.len() / c.channels as usize
            );
            compare(&format!("{} mp4", c.name), &ours.samples, &theirs);
            if c.channels <= 2 {
                let pcm = audio_pcm(&path, 0, c.rate, c.channels == 1, &Prefs::default()).unwrap();
                assert_eq!(pcm.samples, ours.samples, "{} mp4", c.name);
            }
            let mut data = std::fs::read(&path).unwrap();
            let at = data
                .windows(4)
                .position(|w| w == b"mp4a")
                .expect("an mp4a sample entry");
            data[at + 20..at + 22].copy_from_slice(&3u16.to_be_bytes());
            data[at + 28..at + 32].copy_from_slice(&(8000u32 << 16).to_be_bytes());
            let lying = dir().join(format!("{}-entry.mp4", c.name));
            std::fs::write(&lying, &data).unwrap();
            let pr = probe(&lying, &Prefs::default()).unwrap();
            assert_eq!(
                (pr.audio[0].channels, pr.audio[0].rate),
                (c.channels, c.rate),
                "{}: the ident header wins over the sample entry",
                c.name
            );
        }
    }

    #[test]
    fn mp4_vorbis_extradata_and_packets_rebuild_the_source_decode() {
        for c in cases()
            .into_iter()
            .filter(|c| matches!(c.name, "stereo_44k_q3" | "surround_48k_q5"))
        {
            let Some(path) = encode(c, "mp4") else {
                eprintln!(
                    "skipped Vorbis in MP4 {}: ffmpeg would not write one",
                    c.name
                );
                continue;
            };
            let r = crate::mp4::Reader::open(&path).unwrap();
            let t = &r.tracks[0];
            let private = t.extradata().unwrap();
            let [ident, comment, setup] = xiph_headers(&private).unwrap();
            let src =
                OggVorbis::parse(&std::fs::read(path.with_extension("ogg")).unwrap()).unwrap();
            assert_eq!(ident, &src.headers[0][..], "{}", c.name);
            assert_eq!(setup, &src.headers[2][..], "{}", c.name);
            assert_eq!(t.samples.len(), src.packets.len(), "{}", c.name);
            let mut dec = Decoder::from_headers(ident, comment, setup).unwrap();
            let mut ours = Vec::new();
            for k in 0..t.samples.len() {
                dec.decode_into(&r.read(0, k).unwrap(), &mut ours).unwrap();
            }
            let mut theirs_dec = src.decoder().unwrap();
            let mut theirs = Vec::new();
            for k in 0..src.packets.len() {
                theirs_dec.decode_into(src.packet(k), &mut theirs).unwrap();
            }
            assert!(!ours.is_empty(), "{}", c.name);
            assert_eq!(ours, theirs, "{}", c.name);
        }
    }

    #[test]
    fn a_broken_vorbis_file_fails_in_one_line() {
        let Some(c) = cases().into_iter().find(|c| c.name == "stereo_44k_q3") else {
            return;
        };
        let path = encode(c, "ogg").unwrap();
        let ogg = OggVorbis::parse(&std::fs::read(&path).unwrap()).unwrap();
        let mut packets: Vec<Vec<u8>> = ogg.headers.to_vec();
        packets.extend((0..ogg.packets.len()).map(|i| ogg.packet(i).to_vec()));
        packets[5][0] |= 1;
        let bad = dir().join("header_bit_audio.ogg");
        std::fs::write(&bad, ogg_file(&packets, 44100 * 7 / 10)).unwrap();
        let e = audio_pcm_range(&bad, 0, 0.0, f64::INFINITY, 44100, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "invalid data: Vorbis header where an audio packet was expected"
        );
        let mut misfit = packets[..3].to_vec();
        misfit.extend([vec![0x0E], vec![0x00]]);
        let bad = dir().join("misfit_window.ogg");
        std::fs::write(&bad, ogg_file(&misfit, 1000)).unwrap();
        let e = audio_pcm_range(&bad, 0, 0.0, f64::INFINITY, 44100, 2, &Prefs::default())
            .unwrap_err()
            .to_string();
        assert_eq!(e, "invalid data: malformed Vorbis audio packet");
    }
}

fn page(serial: u32, seq: u32, flags: u8, granule: i64, lacing: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(lacing);
    out.extend_from_slice(body);
    let crc = page_crc(&out);
    out[22..26].copy_from_slice(&crc.to_le_bytes());
    out
}

fn ogg_file(packets: &[Vec<u8>], last_granule: i64) -> Vec<u8> {
    let count = packets.len();
    let mut out = Vec::new();
    for (seq, p) in packets.iter().enumerate() {
        let mut lacing = vec![255u8; p.len() / 255];
        lacing.push((p.len() % 255) as u8);
        let flags = match seq {
            0 => 0x02,
            s if s + 1 == count => 0x04,
            _ => 0,
        };
        let granule: i64 = if seq < 3 {
            0
        } else if seq + 1 == count {
            last_granule
        } else {
            -1
        };
        out.extend(page(0x5eed, seq as u32, flags, granule, &lacing, p));
    }
    out
}

fn fake_headers() -> Vec<Vec<u8>> {
    [&b"\x01vorbis"[..], b"\x03vorbis", b"\x05vorbis"]
        .iter()
        .map(|h| {
            let mut v = h.to_vec();
            v.resize(30, 0);
            v
        })
        .collect()
}

fn split_packet() -> Vec<u8> {
    (0..600u32).map(|i| (i % 251) as u8).collect()
}

fn spanning_pages() -> Vec<Vec<u8>> {
    let h = fake_headers();
    let a = split_packet();
    let b = [7u8; 20];
    vec![
        page(1, 0, 0x02, 0, &[30], &h[0]),
        page(1, 1, 0, 0, &[30, 30], &[&h[1][..], &h[2]].concat()),
        page(1, 2, 0, -1, &[255], &a[..255]),
        page(1, 3, 0x01, -1, &[255], &a[255..510]),
        page(1, 4, 0x01, 100, &[90, 20], &[&a[510..], &b[..]].concat()),
        page(1, 5, 0x04, 200, &[10], &[9u8; 10]),
    ]
}

fn packet_lengths(ogg: &OggVorbis) -> Vec<usize> {
    (0..ogg.packets.len())
        .map(|i| ogg.packet(i).len())
        .collect()
}

#[test]
fn a_packet_split_over_pages_joins_back() {
    let ogg = OggVorbis::parse(&spanning_pages().concat()).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
    assert_eq!(ogg.packet(0), &split_packet()[..]);
    assert_eq!(ogg.granules, [None, Some(100), Some(200)]);
}

#[test]
fn a_lost_page_drops_the_packet_it_split_instead_of_splicing_it() {
    for damage in ["missing", "bad crc"] {
        let mut pages = spanning_pages();
        if damage == "missing" {
            pages.remove(3);
        } else {
            let last = pages[3].len() - 1;
            pages[3][last] ^= 0xff;
        }
        let ogg = OggVorbis::parse(&pages.concat()).unwrap();
        assert_eq!(packet_lengths(&ogg), [20, 10], "{damage}");
        assert_eq!(ogg.packet(0), &[7u8; 20][..], "{damage}");
        assert!(
            ogg.warnings.iter().any(|w| w.starts_with("missing page")),
            "{damage}: {:?}",
            ogg.warnings
        );
    }
}

#[test]
fn a_page_cut_inside_its_lacing_table_ends_the_stream_with_a_warning() {
    let whole = spanning_pages().concat();
    let mut tail = page(1, 6, 0, 300, &[10; 5], &[3u8; 50]);
    tail.truncate(29);
    let ogg = OggVorbis::parse(&[&whole[..], &tail].concat()).unwrap();
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
    assert_eq!(
        ogg.warnings,
        [format!("truncated page at byte {}", whole.len())]
    );
    assert_eq!(ogg.granules, [None, Some(100), Some(200)]);
}

#[test]
fn a_packet_left_open_when_the_next_page_starts_fresh_is_dropped() {
    let h = fake_headers();
    let a = split_packet();
    let pages = [
        page(1, 0, 0x02, 0, &[30], &h[0]),
        page(1, 1, 0, 0, &[30, 30], &[&h[1][..], &h[2]].concat()),
        page(1, 2, 0, -1, &[255], &a[..255]),
        page(1, 3, 0, 150, &[20], &[7u8; 20]),
        page(1, 4, 0x04, 200, &[10], &[9u8; 10]),
    ];
    let at: usize = pages[..3].iter().map(Vec::len).sum();
    let ogg = OggVorbis::parse(&pages.concat()).unwrap();
    assert_eq!(packet_lengths(&ogg), [20, 10]);
    assert_eq!(ogg.packet(0), &[7u8; 20][..]);
    assert_eq!(ogg.packet(1), &[9u8; 10][..]);
    assert_eq!(ogg.granules, [Some(150), Some(200)]);
    assert_eq!(
        ogg.warnings,
        [format!("incomplete packet before page at byte {at}")]
    );
}

#[test]
fn a_chained_ogg_file_is_refused_by_name_and_a_multiplexed_one_is_read() {
    let mut pages = spanning_pages();
    pages.push(page(2, 0, 0x02, 0, &[30], &fake_headers()[0]));
    let e = OggVorbis::parse(&pages.concat()).err().unwrap();
    assert_eq!(
        one_line(e),
        "unsupported: chained Ogg Vorbis files are not supported"
    );
    let mut theora = b"\x80theora".to_vec();
    theora.resize(30, 0);
    let mut pages = spanning_pages();
    pages.insert(1, page(9, 0, 0x02, 0, &[30], &theora));
    pages.insert(3, page(9, 1, 0, 0, &[5], &[1; 5]));
    let ogg = OggVorbis::parse(&pages.concat()).unwrap();
    assert!(ogg.warnings.is_empty(), "{:?}", ogg.warnings);
    assert_eq!(packet_lengths(&ogg), [600, 20, 10]);
}

#[test]
fn a_reservation_past_memory_is_refused_in_one_line() {
    let mut v = Vec::new();
    assert_eq!(
        one_line(ogg::reserve(&mut v, u64::MAX, 2).unwrap_err()),
        "invalid data: Ogg Vorbis stream too long to decode in memory"
    );
    assert!(ogg::reserve(&mut v, 1 << 60, 8).is_err());
    assert!(ogg::reserve(&mut v, 1 << 62, 255).is_err());
    ogg::reserve(&mut v, 100, 2).unwrap();
    assert!(v.capacity() >= 200);
}

#[test]
fn a_flood_of_one_byte_packets_stops_at_the_last_granule() {
    let Some((ogg, packets)) = real_packets() else {
        return;
    };
    let long = 1usize << ogg.decoder().unwrap().ident.blocksize_1;
    let mut flood: Vec<Vec<u8>> = packets[..3].to_vec();
    flood.extend(std::iter::repeat_n(vec![0u8], 20_000));
    let ogg = OggVorbis::parse(&ogg_file(&flood, 1000)).unwrap();
    assert_eq!(ogg.packets.len(), 20_000);
    let out = ogg.decode().unwrap();
    assert_eq!(out.frames(), 1000);
    assert!(out.samples.iter().all(|&v| v == 0.0));
    assert!(
        out.samples.capacity() <= (1000 + long) * 2,
        "{}",
        out.samples.capacity()
    );
}

#[test]
fn a_misfit_window_through_decode_fails_in_one_line() {
    let Some((ogg, packets)) = real_packets() else {
        return;
    };
    let dec = ogg.decoder().unwrap();
    assert_eq!(dec.setup.modes.len(), 2);
    assert!(!dec.setup.modes[0].mode_blockflag);
    assert!(dec.setup.modes[1].mode_blockflag);
    let mut misfit = packets[..3].to_vec();
    misfit.extend([vec![0x0E], vec![0x00]]);
    let e = decode(&ogg_file(&misfit, 1000)).err().unwrap();
    assert_eq!(one_line(e), "invalid data: malformed Vorbis audio packet");
}

fn real_packets() -> Option<(OggVorbis, Vec<Vec<u8>>)> {
    let c = cases().into_iter().find(|c| c.name == "stereo_44k_q3")?;
    let path = encode(c, "ogg")?;
    let ogg = OggVorbis::parse(&std::fs::read(&path).unwrap()).unwrap();
    let mut packets: Vec<Vec<u8>> = ogg.headers.to_vec();
    packets.extend((0..ogg.packets.len()).map(|i| ogg.packet(i).to_vec()));
    Some((ogg, packets))
}

fn one_line(e: crate::media::Error) -> String {
    let s = e.to_string();
    assert!(!s.contains('\n'), "{s}");
    s
}

#[test]
fn a_packet_before_the_headers_is_refused_in_one_line() {
    let Some((ogg, packets)) = real_packets() else {
        return;
    };
    let audio = ogg.packet(1);
    let e = Decoder::from_headers(audio, &ogg.headers[1], &ogg.headers[2])
        .err()
        .unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis audio packet where a header was expected"
    );
    let mut moved = packets.clone();
    let a = moved.remove(4);
    moved.insert(1, a);
    let file = ogg_file(&moved, 30870);
    let e = decode(&file).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis audio packet where a header was expected"
    );
    let audio_first = ogg_file(&packets[3..], 30870);
    let e = decode(&audio_first).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: no Vorbis stream in the Ogg file"
    );
}

#[test]
fn a_corrupted_audio_packet_is_refused_in_one_line() {
    let Some((_, packets)) = real_packets() else {
        return;
    };
    let mut bad = packets.clone();
    bad[6][0] |= 1;
    let e = decode(&ogg_file(&bad, 30870)).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Vorbis header where an audio packet was expected"
    );
    let good = decode(&ogg_file(&packets, 30870)).unwrap();
    assert_eq!(good.frames(), 30870);
}

#[test]
fn random_damage_to_audio_packets_never_panics() {
    let Some((ogg, _)) = real_packets() else {
        return;
    };
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..40 {
        let mut dec = ogg.decoder().unwrap();
        let mut out = Vec::new();
        for i in 0..ogg.packets.len() {
            let mut p = ogg.packet(i).to_vec();
            if p.is_empty() {
                continue;
            }
            for _ in 0..3 {
                let at = (next() as usize) % p.len();
                p[at] ^= 1 << (next() % 8);
            }
            if next() % 4 == 0 {
                let keep = (next() as usize) % p.len();
                p.truncate(keep);
            }
            let _ = dec.decode_into(&p, &mut out);
        }
        assert!(out.len().is_multiple_of(2));
    }
}

#[test]
fn ogg_damage_is_survived_or_refused_by_name() {
    let Some((_, packets)) = real_packets() else {
        return;
    };
    let file = ogg_file(&packets, 30870);
    let e = decode(b"not an ogg file at all, only words here")
        .err()
        .unwrap();
    assert_eq!(one_line(e), "invalid data: not an Ogg file");
    let e = decode(&ogg_file(&packets[..2], 0)).err().unwrap();
    assert_eq!(
        one_line(e),
        "invalid data: Ogg Vorbis stream ends before its three headers"
    );
    let headers_only = decode(&ogg_file(&packets[..3], 0)).unwrap();
    assert_eq!(headers_only.frames(), 0);
    let cut = &file[..file.len() * 2 / 3];
    let ogg = OggVorbis::parse(cut).unwrap();
    assert!(!ogg.warnings.is_empty());
    let part = ogg.decode().unwrap();
    assert!(part.frames() < 30870 && part.frames() > 0);
    let mut flipped = file.clone();
    let last = flipped.len() - 10;
    flipped[last] ^= 0xff;
    let ogg = OggVorbis::parse(&flipped).unwrap();
    assert!(
        ogg.warnings.iter().any(|w| w.starts_with("bad CRC")),
        "{:?}",
        ogg.warnings
    );
    assert!(ogg.decode().unwrap().frames() > 0);
    for cut in (0..file.len()).step_by(97) {
        let _ = decode(&file[..cut]);
    }
}

#[test]
fn granules_set_the_lead_and_the_end() {
    assert_eq!(span(&[], &[]), (0, None));
    assert_eq!(span(&[0, 576, 1024], &[None, None, None]), (0, None));
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, None, Some(1600), Some(2000)]),
        (0, Some(2000))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, Some(476), None, Some(2000)]),
        (100, Some(2100))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, Some(1576), None, Some(3000)]),
        (0, Some(2000))
    );
    assert_eq!(
        span(&[0, 576, 1024, 1024], &[None, None, Some(1600), None]),
        (0, None)
    );
    assert_eq!(
        span(&[0, 576, 1024], &[None, None, Some(1500)]),
        (0, Some(1500))
    );
    assert_eq!(span(&[0, 576], &[None, Some(9_999_999)]), (0, Some(576)));
    assert_eq!(span(&[0, 576], &[Some(0), Some(-50)]), (0, Some(0)));
    assert_eq!(
        span(&[0, 576], &[Some(i64::MIN), Some(i64::MAX)]),
        (576, Some(576))
    );
}

#[test]
fn xiph_laced_codec_private_splits_into_three_headers() {
    let ident = vec![1u8; 30];
    let comment = vec![3u8; 300];
    let setup = vec![5u8; 40];
    let mut private = vec![2u8, 30, 255, 45];
    private.extend_from_slice(&ident);
    private.extend_from_slice(&comment);
    private.extend_from_slice(&setup);
    let [i, c, s] = xiph_headers(&private).unwrap();
    assert_eq!((i, c, s), (&ident[..], &comment[..], &setup[..]));
    let refused = "invalid data: Vorbis track without its three headers in CodecPrivate";
    for bad in [
        &[][..],
        &[1, 30][..],
        &[2][..],
        &[2, 255][..],
        &[2, 30, 255][..],
        &private[..private.len() - 41],
    ] {
        assert_eq!(
            xiph_headers(bad).err().unwrap().to_string(),
            refused,
            "{bad:?}"
        );
    }
    let empty_setup = &private[..private.len() - 40];
    assert!(xiph_headers(empty_setup).unwrap()[2].is_empty());
}

#[test]
fn sniff_wants_a_vorbis_ident_on_the_first_page() {
    let Some((_, packets)) = real_packets() else {
        return;
    };
    assert!(ogg::sniff(&ogg_file(&packets, 30870)));
    assert!(!ogg::sniff(&ogg_file(&packets[3..], 30870)));
    assert!(!ogg::sniff(b"OggS"));
    assert!(!ogg::sniff(&[0; 64]));
}
