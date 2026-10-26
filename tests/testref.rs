#[path = "../src/testref.rs"]
mod testref;

use std::path::PathBuf;

use testref::{Diff, Digest, Entry, Kind, Manifest, Pin, Probe, Refs, Sound, Tools};

fn entry(name: &str, file: &str, kind: Kind) -> Entry {
    Entry {
        name: name.into(),
        file: file.into(),
        sha256: testref::sha256(name.as_bytes()),
        kind,
        tool: "Version: ImageMagick 7.1.2-34 Q16-HDRI x86_64".into(),
        command: "magick {ref:a.gif} -set \"label\" \"x[0]\" rgba:{out}\twith\\slash".into(),
    }
}

fn area(name: &str, m: &Manifest, files: &[(&str, &[u8])]) -> PathBuf {
    let dir = testref::scratch("testref-self").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(testref::MANIFEST), m.render()).unwrap();
    for (f, bytes) in files {
        std::fs::write(dir.join(f), bytes).unwrap();
    }
    dir
}

#[test]
fn a_manifest_renders_and_parses_back_unchanged() {
    let m = Manifest {
        entries: vec![
            entry("a.gif", "a.gif", Kind::Fixture),
            entry("a.rgba", "", Kind::BitExact),
            entry("tone.f32", "tone.f32", Kind::Tolerance),
            Entry {
                command: "quote \" bell \u{7} cr \r".into(),
                ..entry("odd", "odd.bin", Kind::Fixture)
            },
        ],
    };
    let text = m.render();
    assert_eq!(Manifest::parse(&text).unwrap(), m);
    assert!(text.starts_with("[[ref]]\nname = \"a.gif\"\n"));
    assert!(text.contains("kind = \"bit-exact\""));
    assert!(text.contains("\\u0007"));
    assert_eq!(Manifest::parse("").unwrap(), Manifest::default());
    let commented = format!("# made by examples/record_refs\n\n{text}");
    assert_eq!(Manifest::parse(&commented).unwrap(), m);
}

#[test]
fn a_broken_manifest_is_refused_with_its_line() {
    let sha = testref::sha256(b"x");
    let good = format!(
        "[[ref]]\nname = \"a\"\nfile = \"a\"\nsha256 = \"{sha}\"\nkind = \"fixture\"\ntool = \"t\"\ncommand = \"c\"\n"
    );
    assert!(Manifest::parse(&good).is_ok());
    let cases = [
        (
            good.replace("kind = \"fixture\"", "kind = \"close\""),
            "kind 'close'",
        ),
        (good.replace("name = ", "nme = "), "unknown key 'nme'"),
        (good.replace("tool = \"t\"\n", ""), "has no tool"),
        (good.replace(&sha, "abc"), "is not 64 hex digits"),
        (good.replace(&sha, &"g".repeat(64)), "is not 64 hex digits"),
        (format!("{good}{good}"), "named twice"),
        (good.replace("[[ref]]\n", ""), "before any [[ref]]"),
        (
            good.replace("file = \"a\"", "file = \"a\"\nfile = \"b\""),
            "given twice",
        ),
        (good.replace("\"c\"", "c"), "is not a \"quoted\" string"),
        (good.replace("\"c\"", "\"c\\q\""), "unknown escape"),
        (good.replace("\"c\"", "\"a\"b\""), "bare quote"),
        (good.replace("\"c\"", "\"\\u12\""), "bad \\u"),
        (good.replace("\"c\"", "\""), "is not a \"quoted\" string"),
        (
            good.replace("command = \"c\"", "command"),
            "is not key = \"value\"",
        ),
    ];
    for (text, want) in cases {
        let err = Manifest::parse(&text).unwrap_err();
        assert!(err.contains(want), "{want}: got {err}");
    }
    let err = Manifest::parse(&good.replace("kind = \"fixture\"", "kind = \"no\"")).unwrap_err();
    assert!(err.starts_with("line 8: "), "{err}");
}

#[test]
fn the_loader_checks_the_sha256_before_use() {
    let body = b"GIF89a tiny";
    let m = Manifest {
        entries: vec![
            Entry {
                sha256: testref::sha256(body),
                ..entry("a.gif", "a.gif", Kind::Fixture)
            },
            entry("a.rgba", "", Kind::BitExact),
        ],
    };
    let dir = area("good", &m, &[("a.gif", body)]);
    let r = Refs::at(dir.clone(), "good");
    assert_eq!(r.bytes("a.gif"), body);
    assert_eq!(r.path("a.gif"), dir.join("a.gif"));
    assert_eq!(r.digest("a.rgba"), testref::sha256(b"a.rgba"));
    r.assert_digest("a.rgba", &testref::sha256(b"a.rgba"));
    let bad = area("damaged", &m, &[("a.gif", b"GIF89a tinY")]);
    let r = Refs::at(bad, "damaged");
    let err = std::panic::catch_unwind(|| r.bytes("a.gif")).unwrap_err();
    let text = err.downcast_ref::<String>().unwrap();
    assert!(
        text.contains("sha256") && text.contains("record_refs -- damaged"),
        "{text}"
    );
}

#[test]
fn a_missing_name_file_or_digest_fails_loudly() {
    let m = Manifest {
        entries: vec![entry("a.rgba", "", Kind::BitExact)],
    };
    let dir = area("missing", &m, &[]);
    let r = Refs::at(dir.clone(), "missing");
    let text = |f: Box<dyn FnOnce() + std::panic::UnwindSafe>| {
        let e = std::panic::catch_unwind(f).unwrap_err();
        e.downcast_ref::<String>().cloned().unwrap_or_default()
    };
    let r1 = Refs::at(dir.clone(), "missing");
    assert!(text(Box::new(move || drop(r1.bytes("nope")))).contains("has no 'nope'"));
    let r2 = Refs::at(dir.clone(), "missing");
    assert!(text(Box::new(move || drop(r2.bytes("a.rgba")))).contains("digest with no file"));
    let r3 = Refs::at(dir.clone(), "missing");
    let wrong = text(Box::new(move || r3.assert_digest("a.rgba", "00")));
    assert!(
        wrong.contains("ours hashes to 00") && wrong.contains("magick {ref:a.gif}"),
        "{wrong}"
    );
    let none = testref::scratch("testref-self").join("no-area");
    let _ = std::fs::remove_dir_all(&none);
    assert!(
        text(Box::new(move || drop(Refs::at(none, "no-area")))).contains("record_refs -- no-area")
    );
    assert_eq!(r.manifest.entries.len(), 1);
}

#[test]
fn digests_hash_what_they_are_given_and_ignore_hidden_colour() {
    assert_eq!(
        testref::sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mut d = Digest::new();
    d.bytes(b"a").bytes(b"bc");
    assert_eq!(d.hex(), testref::sha256(b"abc"));
    let a = [1, 2, 3, 0, 9, 9, 9, 255];
    let b = [7, 7, 7, 0, 9, 9, 9, 255];
    let c = [7, 7, 7, 1, 9, 9, 9, 255];
    assert_eq!(testref::rgba_digest(&a), testref::rgba_digest(&b));
    assert_ne!(testref::rgba_digest(&b), testref::rgba_digest(&c));
    assert_eq!(
        testref::rgba_digest(&a),
        testref::sha256(&[0, 0, 0, 0, 9, 9, 9, 255])
    );
    let big: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    assert_ne!(testref::rgba_digest(&big), testref::sha256(&big));
    let mut f = Digest::new();
    f.f32s(&[1.0, -0.5]);
    let mut g = Digest::new();
    g.bytes(&[0, 0, 128, 63, 0, 0, 0, 191]);
    assert_eq!(f.hex(), g.hex());
    let mut s = Digest::new();
    s.i16s(&[1, -2]);
    assert_eq!(s.hex(), testref::sha256(&[1, 0, 254, 255]));
    assert_eq!(
        testref::le_f32(&[0, 0, 128, 63, 0, 0, 0, 191, 7]),
        vec![1.0, -0.5]
    );
    assert_eq!(testref::le_i16(&[1, 0, 254, 255, 9]), vec![1, -2]);
}

#[test]
fn differences_report_max_rms_and_length() {
    let d = testref::diff(&[0.0f32, 1.0, -1.0, 0.5], &[0.0f32, 0.5, -1.0, 0.0]);
    assert_eq!(d.max, 0.5);
    assert!((d.rms - (0.5f64 / 4.0).sqrt()).abs() < 1e-12);
    assert!(d.within(0.5, 0.36));
    assert!(!d.within(0.49, 1.0));
    assert!(!d.within(1.0, 0.3));
    let short = testref::diff(&[1u8, 2], &[1u8, 2, 3]);
    assert_eq!((short.ours, short.theirs, short.max), (2, 3, 0.0));
    assert!(!short.within(10.0, 10.0));
    testref::assert_within("same", &testref::diff(&[3i16], &[3i16]), 0.0, 0.0);
    let err = std::panic::catch_unwind(|| {
        testref::assert_within(
            "off",
            &Diff {
                max: 2.0,
                rms: 1.0,
                ours: 1,
                theirs: 1,
            },
            1.0,
            1.0,
        )
    })
    .unwrap_err();
    assert!(
        err.downcast_ref::<String>()
            .unwrap()
            .contains("max 2.000000 (limit 1)")
    );
    assert!(testref::psnr(&[10, 20], &[10, 20]) > 100.0);
    assert!((testref::psnr(&[0, 0], &[255, 255])).abs() < 1e-9);
    let tone = testref::pcm(
        Sound::Sine {
            hz: 1000.0,
            amp: 0.5,
        },
        48_000,
        1,
        0,
        480,
    );
    assert!(testref::snr_db(&tone, &tone) > 200.0);
    let quiet: Vec<f32> = tone.iter().map(|v| v * 0.99).collect();
    let snr = testref::snr_db(&quiet, &tone);
    assert!((snr - 40.0).abs() < 0.01, "{snr}");
}

#[test]
fn patterns_and_sounds_are_deterministic_and_move() {
    let a = testref::rgb_at(160, 120, 3, 50, 70);
    assert_eq!(a, testref::rgb_at(160, 120, 3, 50, 70));
    let first: Vec<[u8; 3]> = (0..160)
        .map(|x| testref::rgb_at(160, 120, 0, x, 60))
        .collect();
    let later: Vec<[u8; 3]> = (0..160)
        .map(|x| testref::rgb_at(160, 120, 9, x, 60))
        .collect();
    assert_ne!(first, later);
    let colours: std::collections::BTreeSet<[u8; 3]> = (0..120)
        .flat_map(|y| (0..160).map(move |x| testref::rgb_at(160, 120, 0, x, y)))
        .collect();
    assert!(colours.len() > 1000, "{}", colours.len());
    assert_eq!(
        testref::rgb_at(4, 3, 0, 0, 0),
        testref::rgb_at(4, 3, 0, 0, 0)
    );
    let noisy = testref::rgb_noisy(160, 120, 1, 5, 100, 12);
    let clean = testref::rgb_at(160, 120, 1, 5, 100);
    assert!(noisy.iter().zip(clean).all(|(n, c)| n.abs_diff(c) <= 12));
    assert_eq!(testref::noise(1, 2, 3, 0), 0);
    let spread: Vec<i32> = (0..400).map(|x| testref::noise(x, 0, 0, 3)).collect();
    assert!(spread.iter().all(|v| (-3..=3).contains(v)));
    assert!(spread.contains(&-3) && spread.contains(&3));
    assert_eq!(testref::yuv_of([0, 0, 0]), [16, 128, 128]);
    assert_eq!(testref::yuv_of([255, 255, 255]), [235, 128, 128]);
    let sine = testref::pcm(
        Sound::Sine {
            hz: 12_000.0,
            amp: 0.5,
        },
        48_000,
        2,
        0,
        4,
    );
    assert_eq!(sine.len(), 8);
    assert!((sine[2] - 0.5).abs() < 1e-6, "{sine:?}");
    assert_eq!(testref::pcm(Sound::Silence, 48_000, 2, 0, 3), vec![0.0; 6]);
    let n1 = testref::pcm(Sound::Noise { amp: 0.1, seed: 1 }, 48_000, 1, 0, 64);
    let n2 = testref::pcm(Sound::Noise { amp: 0.1, seed: 2 }, 48_000, 1, 0, 64);
    assert_ne!(n1, n2);
    assert!(n1.iter().all(|v| v.abs() <= 0.1));
    let tail = testref::pcm(Sound::Noise { amp: 0.1, seed: 1 }, 48_000, 1, 60, 4);
    assert_eq!(tail, n1[60..]);
    let custom = testref::pcm(
        Sound::Fn(|i, ch, _| (i * 10 + u64::from(ch)) as f32),
        8000,
        2,
        1,
        2,
    );
    assert_eq!(custom, vec![10.0, 11.0, 20.0, 21.0]);
    assert_eq!(
        testref::s16_of(&[1.0, -1.0, 0.5, 2.0]),
        vec![32767, -32768, 16384, 32767]
    );
}

#[test]
fn writes_are_atomic_and_name_their_failure() {
    let dir = testref::scratch("testref-self");
    let p = dir.join("atomic.bin");
    testref::write_atomic(&p, b"one").unwrap();
    testref::write_atomic(&p, b"two").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"two");
    let err = testref::write_atomic(&dir.join("no/such/dir/x.bin"), b"x").unwrap_err();
    assert!(err.contains("no/such/dir"), "{err}");
    assert_eq!(
        testref::rerecord("gif"),
        "cargo run --release --example record_refs -- gif"
    );
}

#[cfg(all(
    feature = "media",
    feature = "h264",
    feature = "aac",
    feature = "mp4",
    feature = "mkv",
    feature = "vp9",
    feature = "opus"
))]
mod clips {
    use super::testref::{self, AudioCodec, Clip, Picture, Sound, VideoCodec};
    use pito_codecs::media::{Frame, Prefs, audio_pcm, frame_count, probe};
    use std::path::Path;

    fn frames(p: &Path) -> u64 {
        frame_count(p, &Prefs::default()).unwrap()
    }

    fn flat(n: u64, f: &mut Frame) {
        for p in &mut f.planes {
            p.data.fill(if n.is_multiple_of(2) { 60 } else { 180 });
        }
    }

    #[test]
    fn an_mp4_clip_holds_every_frame_and_its_sound() {
        let clip = Clip::new((352, 240), (25, 1), 20);
        let path = clip.make("testref-self", "clip.mp4");
        let p = probe(&path, &Prefs::default()).unwrap();
        assert_eq!((p.width, p.height), (352, 240));
        assert!((p.fps() - 25.0).abs() < 0.01, "{}", p.fps());
        assert_eq!(frames(&path), 20);
        assert_eq!(p.audio.len(), 1);
        assert_eq!((p.audio[0].rate, p.audio[0].channels), (48_000, 2));
        assert!((p.duration - 0.8).abs() < 0.05, "{}", p.duration);
        let pcm = audio_pcm(&path, 0, 48_000, false, &Prefs::default()).unwrap();
        let frames = pcm.samples.len() / 2;
        assert!(frames.abs_diff(clip.samples() as usize) <= 1024, "{frames}");
        let want = testref::pcm(clip.sounds[0], 48_000, 2, 0, frames);
        let snr = testref::snr_db(
            &pcm.samples[4096..frames * 2 - 4096],
            &want[4096..frames * 2 - 4096],
        );
        assert!(snr > 10.0, "AAC round trip SNR {snr:.1} dB");
    }

    #[test]
    fn a_webm_clip_is_vp9_and_opus_and_an_mka_is_sound_alone() {
        let clip = Clip {
            video: Some(VideoCodec::Vp9),
            audio: Some(AudioCodec::Opus),
            qp: 60,
            ..Clip::new((352, 240), (30, 1), 12)
        };
        let webm = clip.make("testref-self", "clip.webm");
        let p = probe(&webm, &Prefs::default()).unwrap();
        assert_eq!(frames(&webm), 12);
        assert_eq!(p.audio.len(), 1);
        let sound = Clip {
            sounds: vec![
                Sound::Silence,
                Sound::Sine {
                    hz: 880.0,
                    amp: 0.2,
                },
            ],
            ..Clip::new((352, 240), (30, 1), 30).audio_only()
        };
        let p = probe(&sound.make("testref-self", "two.mka"), &Prefs::default()).unwrap();
        assert_eq!((p.video_codec, p.audio.len()), (None, 2));
        let silent = Clip {
            picture: Picture::Fn(flat),
            ..Clip::new((352, 240), (30, 1), 4).video_only()
        };
        let mkv = silent.make("testref-self", "silent.mkv");
        let p = probe(&mkv, &Prefs::default()).unwrap();
        assert_eq!((frames(&mkv), p.audio.len()), (4, 0));
        let noisy = Clip {
            picture: Picture::Noisy(12),
            ..Clip::new((352, 240), (30, 1), 3).video_only()
        };
        assert_eq!(frames(&noisy.make("testref-self", "noisy.mov")), 3);
    }

    #[test]
    fn a_clip_that_cannot_be_written_is_an_error_and_leaves_nothing() {
        let dir = testref::scratch("testref-self-refused");
        let clip = Clip::new((352, 240), (30, 1), 2);
        let err = clip.write(&dir.join("clip.avi")).unwrap_err();
        assert!(err.contains("not 'avi'"), "{err}");
        let vp9 = Clip {
            video: Some(VideoCodec::Vp9),
            ..clip.clone().video_only()
        };
        assert!(
            vp9.write(&dir.join("vp9.mp4"))
                .unwrap_err()
                .contains("mkv or webm")
        );
        let opus = Clip {
            audio: Some(AudioCodec::Opus),
            ..clip.clone().audio_only()
        };
        assert!(
            opus.write(&dir.join("opus.m4a"))
                .unwrap_err()
                .contains("mkv, mka or webm")
        );
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".part."))
            .count();
        assert_eq!(leftovers, 0);
        assert!(!dir.join("clip.avi").exists() && !dir.join("vp9.mp4").exists());
    }
}

const PINS: &str = "# pins\n\n[x264]\nversion = \"x264 --version\"\nfind = \"x264 \"\nline = \"x264 0.165.3222 b35605a\"\n\n[dav1d]\nversion = \"dav1d   --version\"\nupto = \" - \"\nline = \"1.5.4\"\n\n[libwebp]\nlibrary = \"/usr/lib/libwebp.so.7\"\ncall = \"WebPGetEncoderVersion\"\nreturns = \"int\"\nline = \"libwebp.so.7.2.0 1.6.0\"\n\n[libopus]\nlibrary = \"/usr/lib/libopus.so.0\"\ncall = \"opus_get_version_string\"\nline = \"libopus.so.0.11.1 libopus 1.6.1\"\n";

#[test]
fn tool_pins_parse_commands_and_libraries() {
    let t = Tools::parse(PINS).unwrap();
    assert_eq!(t.pins.len(), 4);
    assert_eq!(
        t.pin("x264").unwrap(),
        &Pin {
            name: "x264".into(),
            probe: Probe::Command {
                run: vec!["x264".into(), "--version".into()],
                find: "x264 ".into(),
                upto: String::new(),
            },
            line: "x264 0.165.3222 b35605a".into(),
        }
    );
    assert_eq!(
        t.pin("dav1d").unwrap().probe,
        Probe::Command {
            run: vec!["dav1d".into(), "--version".into()],
            find: String::new(),
            upto: " - ".into(),
        }
    );
    assert_eq!(
        t.pin("libwebp").unwrap().probe,
        Probe::Library {
            path: "/usr/lib/libwebp.so.7".into(),
            call: "WebPGetEncoderVersion".into(),
            int: true,
        }
    );
    assert!(matches!(
        t.pin("libopus").unwrap().probe,
        Probe::Library { int: false, .. }
    ));
    assert_eq!(Tools::parse("").unwrap(), Tools::default());
}

#[test]
fn a_broken_pin_file_is_refused_with_its_reason() {
    let cases: [(&str, &str); 11] = [
        ("[x]\nline = \"1\"\n", "needs one of version or library"),
        ("[x]\nversion = \"x -v\"\n", "has no line"),
        (
            "[x]\nversion = \"x\"\nlibrary = \"/l.so\"\nline = \"1\"\n",
            "needs one of version or library",
        ),
        ("[x]\nlibrary = \"/l.so\"\nline = \"1\"\n", "a library needs call"),
        (
            "[x]\nlibrary = \"/l.so\"\ncall = \"v\"\nupto = \" \"\nline = \"1\"\n",
            "find and upto go with version",
        ),
        (
            "[x]\nlibrary = \"/l.so\"\ncall = \"v\"\nreturns = \"float\"\nline = \"1\"\n",
            "not \"float\"",
        ),
        (
            "[x]\nversion = \"x\"\ncall = \"v\"\nline = \"1\"\n",
            "call and returns go with library",
        ),
        ("[x]\nversion = \"  \"\nline = \"1\"\n", "version is empty"),
        ("line = \"1\"\n", "line 1: 'line' comes before any [tool]"),
        ("[x]\nline = \"1\"\nline = \"2\"\n", "line 3: 'line' given twice"),
        ("[a b]\nline = \"1\"\n", "line 1: '[a b]' is not a [tool] header"),
    ];
    for (text, want) in cases {
        let err = Tools::parse(text).unwrap_err();
        assert!(err.contains(want), "{text:?}: {err}");
    }
    let twice = "[x]\nversion = \"x\"\nline = \"1\"\n[x]\nversion = \"x\"\nline = \"1\"\n";
    assert!(Tools::parse(twice).unwrap_err().contains("[x] given twice"));
    assert!(
        Tools::parse("[x]\nwhat = \"1\"\n")
            .unwrap_err()
            .contains("unknown key 'what'")
    );
    assert!(
        Tools::parse(PINS)
            .unwrap()
            .pin("lame")
            .unwrap_err()
            .contains("lame is not pinned")
    );
}

#[test]
fn a_version_line_is_found_trimmed_and_checked_against_its_pin() {
    let help = "Usage: vpxenc <options>\n\nIncluded encoders:\n\n    vp8    - WebM Project VP8 Encoder v1.17.0 \n    vp9    - WebM Project VP9 Encoder v1.17.0 (default)\n";
    assert_eq!(
        testref::version_line(help, "VP9 Encoder", "").as_deref(),
        Some("vp9    - WebM Project VP9 Encoder v1.17.0 (default)")
    );
    assert_eq!(
        testref::version_line("\n\n  1.5.4\n", "", "").as_deref(),
        Some("1.5.4")
    );
    assert_eq!(testref::version_line("nothing here", "AV1", ""), None);
    assert_eq!(testref::version_line("", "", ""), None);
    let a52 = "a52dec-0.8.0 - by its authors\nusage";
    assert_eq!(
        testref::version_line(a52, "a52dec-", " - ").as_deref(),
        Some("a52dec-0.8.0")
    );
    assert_eq!(
        testref::version_line("rav1e 0.8.1", "rav1e", " - ").as_deref(),
        Some("rav1e 0.8.1")
    );
    assert_eq!(testref::int_version(0x010600), "1.6.0");
    assert_eq!(testref::int_version(0x0a0b0c), "10.11.12");
    assert_eq!(
        testref::library_line("libwebp.so.7.2.0", "1.6.0"),
        "libwebp.so.7.2.0 1.6.0"
    );
    let t = Tools::parse(PINS).unwrap();
    let pin = t.pin("x264").unwrap();
    assert_eq!(
        pin.check("x264 0.165.3222 b35605a").unwrap(),
        "x264 0.165.3222 b35605a"
    );
    let err = pin.check("x264 0.164.3108 31e19f9").unwrap_err();
    assert!(
        err.contains("'x264 0.165.3222 b35605a'") && err.contains("'x264 0.164.3108 31e19f9'"),
        "{err}"
    );
}

#[test]
fn the_tracked_pins_cover_every_reference_source() {
    let t = Tools::load().unwrap();
    let commands = [
        "x264",
        "x265",
        "vpxenc",
        "vpxdec",
        "aomenc",
        "SvtAv1EncApp",
        "rav1e",
        "dav1d",
        "aomdec",
        "flac",
        "lame",
        "mpg123",
        "faac",
        "faad",
        "a52dec",
        "cjpeg",
        "djpeg",
        "jpegtran",
        "magick",
    ];
    for name in commands {
        let pin = t.pin(name).unwrap();
        let Probe::Command { run, .. } = &pin.probe else {
            panic!("{name} should be pinned as a command");
        };
        assert_eq!(run[0], name);
        assert!(!pin.line.is_empty(), "{name}");
    }
    for name in ["libopus", "libvorbisenc", "libvorbisfile", "libwebp"] {
        let pin = t.pin(name).unwrap();
        let Probe::Library { path, .. } = &pin.probe else {
            panic!("{name} should be pinned as a library");
        };
        assert!(path.contains(name), "{name}: {path}");
        assert!(pin.line.starts_with(name), "{name}: {}", pin.line);
    }
    assert_eq!(t.pins.len(), commands.len() + 4);
}
