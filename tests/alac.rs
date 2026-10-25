#![cfg(all(feature = "alac", feature = "audio", feature = "mp4", feature = "mkv"))]

#[path = "../src/alac/refs.rs"]
mod refs;
#[path = "../src/testref.rs"]
mod testref;

use std::path::{Path, PathBuf};

use pito_codecs::alac::{Config, Decoder, entry_cookie};
use pito_codecs::media::{AudioBuffer, Prefs, audio_pcm_range, probe};
use refs::pcm::{fnv, source};
use testref::Refs;

struct Fixture {
    name: &'static str,
    channels: usize,
    depth: u32,
    rate: u32,
    frames: usize,
    salt: u64,
    hash: u64,
}

const fn fixture(
    name: &'static str,
    channels: usize,
    depth: u32,
    rate: u32,
    frames: usize,
    salt: u64,
    hash: u64,
) -> Fixture {
    Fixture {
        name,
        channels,
        depth,
        rate,
        frames,
        salt,
        hash,
    }
}

const FIXTURES: &[Fixture] = &[
    fixture(
        "s16-mono.m4a",
        1,
        16,
        44_100,
        5097,
        1,
        0x38fe_2ad8_37cb_f5bf,
    ),
    fixture(
        "s16-stereo.m4a",
        2,
        16,
        44_100,
        5097,
        4,
        0xfaf2_b693_2497_8db4,
    ),
    fixture(
        "s24-stereo.mov",
        2,
        24,
        48_000,
        5097,
        3,
        0xd4e8_8479_0573_c49b,
    ),
    fixture(
        "s24-stereo-96k.mov",
        2,
        24,
        96_000,
        5097,
        4,
        0xfc82_224f_f7b5_6653,
    ),
    fixture(
        "s24-mono-96k.m4a",
        1,
        24,
        96_000,
        5097,
        5,
        0x819b_20c9_6df9_ec7a,
    ),
    fixture(
        "s16-stereo.mkv",
        2,
        16,
        48_000,
        5097,
        6,
        0x24d4_c6d2_e649_9652,
    ),
    fixture(
        "s24-stereo-order8.mkv",
        2,
        24,
        48_000,
        5097,
        7,
        0xc445_2612_2ddd_f212,
    ),
    fixture(
        "s16-stereo-order4.m4a",
        2,
        16,
        44_100,
        5097,
        8,
        0xa7b4_ec3e_9b75_6457,
    ),
    fixture("s16-5.1.m4a", 6, 16, 48_000, 1501, 9, 0xc488_8270_2336_762b),
    fixture(
        "s24-mono-verbatim.mov",
        1,
        24,
        44_100,
        2000,
        10,
        0xe111_114e_9b0b_5ba7,
    ),
];

fn named(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap()
}

fn path(f: &Fixture) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/alac")
        .join(f.name)
}

fn want(f: &Fixture) -> Vec<i32> {
    let w = source(f.channels, f.depth, f.frames, f.salt);
    assert_eq!(fnv(&w), f.hash, "{}: the PCM generator changed", f.name);
    w
}

fn as_f32(samples: &[i32], depth: u32) -> Vec<f32> {
    let scale = 1.0 / (1u64 << (depth - 1)) as f32;
    samples.iter().map(|&s| s as f32 * scale).collect()
}

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-alac");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn packets(path: &Path) -> (Vec<u8>, Vec<Vec<u8>>) {
    if path.extension().is_some_and(|e| e == "mkv") {
        let r = pito_codecs::mkv::Reader::open(path).unwrap();
        let i = r.audio()[0];
        let t = &r.tracks[i];
        let pkts = (0..t.samples.len())
            .map(|k| r.read(i, k).unwrap())
            .collect();
        (t.codec_private.clone(), pkts)
    } else {
        let r = pito_codecs::mp4::Reader::open(path).unwrap();
        let i = r.audio()[0];
        let t = &r.tracks[i];
        let cookie = entry_cookie(t.entry().unwrap()).unwrap().to_vec();
        let pkts = (0..t.samples.len())
            .map(|k| r.read(i, k).unwrap())
            .collect();
        (cookie, pkts)
    }
}

fn decode_all(path: &Path) -> (Config, Vec<i32>) {
    let (cookie, pkts) = packets(path);
    let mut d = Decoder::new(&cookie).unwrap();
    let mut out = Vec::new();
    for p in &pkts {
        out.extend_from_slice(d.decode(p).unwrap());
    }
    (*d.config(), out)
}

const ALAC_ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[2, 0, 1],
    &[2, 0, 1, 3],
    &[2, 0, 1, 3, 4],
    &[2, 0, 1, 4, 5, 3],
    &[2, 0, 1, 4, 5, 6, 3],
    &[2, 6, 7, 0, 1, 4, 5, 3],
];

fn in_alac_order(samples: &[i32], channels: usize) -> Vec<i32> {
    let order = ALAC_ORDER[channels - 1];
    samples
        .chunks_exact(channels)
        .flat_map(|f| order.iter().map(move |&c| f[c]))
        .collect()
}

#[test]
fn every_fixture_decodes_bit_exact() {
    for f in FIXTURES {
        let want = want(f);
        let (config, got) = decode_all(&path(f));
        assert_eq!(config.bit_depth as u32, f.depth, "{}", f.name);
        assert_eq!(config.channels as usize, f.channels, "{}", f.name);
        assert_eq!(config.sample_rate, f.rate, "{}", f.name);
        assert_eq!(got.len(), want.len(), "{}", f.name);
        assert!(
            got == in_alac_order(&want, f.channels),
            "{} is not bit-exact",
            f.name
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Head {
    pair: bool,
    partial: bool,
    shifted: u32,
    escape: bool,
    mix_res: i8,
    order: u32,
}

fn head(packet: &[u8]) -> Head {
    let mut pos = 0usize;
    let mut read = |n: usize| {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = packet.get(pos / 8).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - pos % 8)) & 1) as u32;
            pos += 1;
        }
        v
    };
    let pair = read(3) == 1;
    read(16);
    let partial = read(1) == 1;
    let shifted = read(2);
    let escape = read(1) == 1;
    if partial {
        read(32);
    }
    let (mut mix_res, mut order) = (0, 0);
    if !escape {
        read(8);
        mix_res = read(8) as u8 as i8;
        read(8);
        order = read(8) & 31;
    }
    Head {
        pair,
        partial,
        shifted,
        escape,
        mix_res,
        order,
    }
}

#[test]
fn the_fixtures_cover_matrixing_shifted_bytes_escapes_and_every_predictor() {
    let heads = |name: &str| -> Vec<Head> {
        packets(&path(named(name)))
            .1
            .iter()
            .map(|p| head(p))
            .collect()
    };
    for f in FIXTURES {
        let h = heads(f.name);
        assert!(h.last().unwrap().partial, "{}: no partial frame", f.name);
    }
    for name in ["s16-stereo.m4a", "s24-stereo.mov", "s24-stereo-96k.mov"] {
        assert!(
            heads(name).iter().any(|h| h.pair && h.mix_res != 0),
            "{name}: no matrixed pair"
        );
    }
    for name in [
        "s24-stereo.mov",
        "s24-mono-96k.m4a",
        "s24-stereo-order8.mkv",
    ] {
        assert!(
            heads(name).iter().any(|h| h.shifted == 1),
            "{name}: not shifted by a byte"
        );
    }
    assert!(heads("s24-mono-verbatim.mov").iter().all(|h| h.escape));
    for (name, order) in [("s16-stereo-order4.m4a", 4), ("s24-stereo-order8.mkv", 8)] {
        let h = heads(name);
        let compressed: Vec<&Head> = h.iter().filter(|h| !h.escape).collect();
        assert!(!compressed.is_empty(), "{name}: every frame verbatim");
        assert!(
            compressed.iter().all(|h| h.order == order),
            "{name}: not order {order}"
        );
    }
    assert!(
        FIXTURES
            .iter()
            .flat_map(|f| heads(f.name))
            .any(|h| !h.escape && !matches!(h.order, 0 | 4 | 8 | 31)),
        "no fixture runs the general predictor"
    );
}

#[test]
fn the_media_front_hears_every_fixture_bit_exact_in_wave_order_and_seeks_to_the_sample() {
    let prefs = Prefs::default();
    for f in FIXTURES {
        let p = path(f);
        let want = as_f32(&want(f), f.depth);
        let ch = f.channels as u16;
        let at = |frame: usize| frame as f64 / f.rate as f64;
        let whole = audio_pcm_range(&p, 0, 0.0, f64::INFINITY, f.rate, ch, &prefs).unwrap();
        assert_eq!(whole.channels, ch, "{}", f.name);
        assert_eq!(whole.rate, f.rate, "{}", f.name);
        assert_eq!(whole.samples.len(), want.len(), "{}", f.name);
        assert!(whole.samples == want, "{} is not bit-exact", f.name);
        let (a, b) = (f.frames / 3, f.frames * 49 / 50);
        let mid = audio_pcm_range(&p, 0, at(a), at(b), f.rate, ch, &prefs).unwrap();
        assert!(
            mid.samples == want[a * f.channels..b * f.channels],
            "{}: a range is not bit-exact",
            f.name
        );
        let from = f.frames * 5 / 6;
        let tail = audio_pcm_range(&p, 0, at(from), f64::INFINITY, f.rate, ch, &prefs).unwrap();
        assert!(
            tail.samples == want[from * f.channels..],
            "{}: the tail is not bit-exact",
            f.name
        );
    }
}

#[test]
fn surround_downmixes_from_wave_order() {
    let prefs = Prefs::default();
    let f = named("s16-5.1.m4a");
    let surround = AudioBuffer {
        samples: as_f32(&want(f), f.depth),
        rate: f.rate,
        channels: 6,
    };
    let stereo = pito_codecs::aac::downmix_stereo(&surround);
    let got = audio_pcm_range(&path(f), 0, 0.0, f64::INFINITY, f.rate, 2, &prefs).unwrap();
    assert_eq!(got.channels, 2);
    assert!(got.samples == stereo.samples, "5.1 downmixed out of order");
}

#[test]
fn probe_names_alac_with_the_cookie_rate_and_calls_it_native() {
    let prefs = Prefs::default();
    for f in FIXTURES {
        let p = probe(&path(f), &prefs).unwrap();
        assert_eq!(p.audio.len(), 1, "{}", f.name);
        let a = &p.audio[0];
        assert_eq!(a.codec, "alac", "{}", f.name);
        assert!(a.native(), "{}", f.name);
        assert_eq!(a.channels as usize, f.channels, "{}", f.name);
        assert_eq!(a.rate, f.rate, "{}", f.name);
        assert!(
            p.unsupported().is_empty(),
            "{}: {:?}",
            f.name,
            p.unsupported()
        );
        assert!(p.video_codec.is_none(), "{}", f.name);
    }
}

fn find(bytes: &[u8], what: &[u8]) -> usize {
    bytes.windows(what.len()).position(|w| w == what).unwrap()
}

fn patched(f: &Fixture, tag: &str, edit: impl FnOnce(&mut Vec<u8>)) -> PathBuf {
    let mut bytes = std::fs::read(path(f)).unwrap();
    edit(&mut bytes);
    let out = dir().join(format!("{tag}-{}", f.name));
    std::fs::write(&out, bytes).unwrap();
    out
}

fn cookie_at(bytes: &[u8]) -> usize {
    find(bytes, b"\0\0\0\x24alac")
}

#[test]
fn a_refused_alac_track_is_named_at_probe_and_refused_cleanly() {
    let prefs = Prefs::default();
    let mut cases: Vec<(PathBuf, &str, u32)> = Vec::new();
    for name in ["s16-stereo.m4a", "s24-stereo.mov", "s24-stereo-96k.mov"] {
        let f = named(name);
        let missing = patched(f, "missing", |b| {
            let at = cookie_at(b) + 4;
            b[at..at + 4].copy_from_slice(b"xxxx");
        });
        cases.push((missing, "ALAC without its magic cookie", f.rate));
        let v1 = patched(f, "v1", |b| {
            let at = cookie_at(b) + 16;
            b[at] = 1;
        });
        cases.push((v1, "ALAC version 1", f.rate));
        let eight = patched(f, "8bit", |b| {
            let at = cookie_at(b) + 17;
            b[at] = 8;
        });
        cases.push((eight, "8-bit ALAC", f.rate));
    }
    let f = named("s16-stereo.mkv");
    let mkv = patched(f, "missing", |b| {
        let at = find(b, &[0x63, 0xa2, 0x98]);
        b[at + 1] = 0xa3;
    });
    cases.push((mkv, "ALAC without its magic cookie", f.rate));
    for (p, variant, rate) in cases {
        let probed = probe(&p, &prefs).unwrap();
        let a = &probed.audio[0];
        assert_eq!(a.codec, "alac", "{}", p.display());
        assert_eq!(a.variant(), variant, "{}", p.display());
        assert_eq!(a.refused.as_deref(), Some(variant), "{}", p.display());
        assert!(!a.native(), "{}", p.display());
        assert_eq!(probed.unsupported(), vec![variant.to_string()]);
        let e = audio_pcm_range(&p, 0, 0.0, f64::INFINITY, rate, 2, &prefs).unwrap_err();
        let said = e.to_string();
        assert!(
            said.contains("ALAC"),
            "{}: {said} does not name ALAC",
            p.display()
        );
    }
}

#[test]
fn damaged_packets_fail_cleanly_and_a_good_one_after_them_decodes_exactly() {
    for name in ["s16-stereo.m4a", "s24-stereo.mov", "s16-5.1.m4a"] {
        let f = named(name);
        let want = in_alac_order(&want(f), f.channels);
        let (cookie, pkts) = packets(&path(f));
        let mut d = Decoder::new(&cookie).unwrap();
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut start = 0;
        for p in &pkts {
            let good = d.decode(p).unwrap().to_vec();
            assert!(
                good == want[start..start + good.len()],
                "{name}: not bit-exact"
            );
            for cut in [0, 1, 2, 3, 4, 5, 8, p.len() / 2, p.len().saturating_sub(1)] {
                if cut < p.len()
                    && let Ok(got) = d.decode(&p[..cut])
                {
                    assert!(got == good, "{name}: a cut packet decoded wrong");
                }
            }
            for _ in 0..24 {
                let mut bad = p.clone();
                for _ in 0..1 + next() % 6 {
                    let at = (next() as usize) % bad.len();
                    bad[at] ^= 1 << (next() % 8);
                }
                let _ = d.decode(&bad);
            }
            assert!(
                d.decode(p).unwrap() == good.as_slice(),
                "{name}: a good packet after damaged ones"
            );
            start += good.len();
        }
        assert_eq!(start, want.len(), "{name}");
        assert!(d.decode(&pkts[0][..1]).is_err());
    }
}

#[test]
fn a_truncated_alac_file_gives_an_error_or_a_bit_exact_prefix() {
    let prefs = Prefs::default();
    for name in ["s16-stereo.mkv", "s24-stereo-order8.mkv", "s16-mono.m4a"] {
        let f = named(name);
        let want = as_f32(&want(f), f.depth);
        let bytes = std::fs::read(path(f)).unwrap();
        for cut in [bytes.len() * 3 / 5, bytes.len() * 3 / 4, bytes.len() - 5] {
            let p = dir().join(format!("cut-{cut}-{name}"));
            std::fs::write(&p, &bytes[..cut]).unwrap();
            let ch = f.channels as u16;
            if let Ok(b) = audio_pcm_range(&p, 0, 0.0, f64::INFINITY, f.rate, ch, &prefs) {
                assert!(
                    want.starts_with(&b.samples),
                    "{name} cut at {cut}: not a bit-exact prefix"
                );
            }
        }
    }
}

#[test]
fn a_damaged_alac_file_never_panics() {
    let prefs = Prefs::default();
    let f = named("s16-stereo.m4a");
    let bytes = std::fs::read(path(f)).unwrap();
    let mut s = 7u64;
    for round in 0..8 {
        let mut bad = bytes.clone();
        for _ in 0..64 {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let at = (s >> 33) as usize % bad.len();
            bad[at] = (s >> 20) as u8;
        }
        let damaged = dir().join(format!("damaged-{round}.m4a"));
        std::fs::write(&damaged, &bad).unwrap();
        let _ = probe(&damaged, &prefs);
        let _ = audio_pcm_range(&damaged, 0, 0.0, f64::INFINITY, f.rate, 2, &prefs);
    }
}
#[test]
fn longer_streams_from_ffmpeg_decode_bit_exact() {
    let r = Refs::open(refs::AREA);
    for (k, case) in refs::CASES.iter().enumerate() {
        let want = refs::source(k);
        assert_eq!(want.len(), refs::frames(k) * case.channels, "{}", case.name);
        let path = r.path(&case.file());
        let (config, got) = decode_all(&path);
        assert_eq!(config.bit_depth as u32, case.depth, "{}", case.name);
        assert_eq!(config.channels as usize, case.channels, "{}", case.name);
        assert_eq!(config.sample_rate, case.rate, "{}", case.name);
        let (_, pkts) = packets(&path);
        assert!(pkts.len() >= 2, "{}: {} packets", case.name, pkts.len());
        assert!(got == want, "{} is not bit-exact", case.name);
        let whole = audio_pcm_range(
            &path,
            0,
            0.0,
            f64::INFINITY,
            case.rate,
            case.channels as u16,
            &Prefs::default(),
        )
        .unwrap();
        assert_eq!(whole.samples.len(), want.len(), "{}", case.name);
        assert!(
            whole.samples == as_f32(&want, case.depth),
            "{}: the media front is not bit-exact",
            case.name
        );
    }
}

#[test]
fn the_raw_input_packs_each_sample_in_its_width() {
    assert_eq!(refs::raw(&[1, -2], 16), vec![1, 0, 0xfe, 0xff]);
    assert_eq!(refs::raw(&[-1, 0x123456], 24), vec![0xff, 0xff, 0xff, 0x56, 0x34, 0x12]);
    for (k, case) in refs::CASES.iter().enumerate() {
        let width = if case.depth == 16 { 2 } else { 3 };
        assert_eq!(
            refs::raw_source(k).len(),
            refs::frames(k) * case.channels * width
        );
    }
}
