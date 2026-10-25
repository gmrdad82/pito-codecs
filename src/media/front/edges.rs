use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::demux::Demux;
use super::export::{ExportOptions, Exported, Piece, export};
use super::mux::BROKEN;
use super::route::{Choice, Prefs};
use super::video::decode_video;
use crate::gpu::{Backend, Color, Direction, available};
use crate::media::{Codec, Error, Route};
use crate::testref::{Clip, Refs};

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

type Look = Option<((u16, u16), Color)>;

fn dir() -> PathBuf {
    crate::testref::scratch(refs::AREA)
}

fn on_va(codec: Codec) -> bool {
    let all = available();
    let has = |d: Direction| {
        all.iter()
            .any(|&(c, dd, b)| c == codec && dd == d && b == Backend::VaApi)
    };
    has(Direction::Decode) && has(Direction::Encode)
}

fn ready(codec: Codec) -> bool {
    if !on_va(codec) {
        eprintln!("no VA-API {codec} decoder and encoder: skipped");
        return false;
    }
    true
}

fn h264_1080p() -> PathBuf {
    let mut clip = Clip::new(refs::BIG, (refs::FPS, 1), refs::FRAMES);
    clip.gop = refs::GOP;
    clip.make(refs::AREA, "edges-h264-1080p.mp4")
}

fn fixture(name: &str) -> PathBuf {
    Refs::open(refs::AREA).path(name)
}

fn pieces(path: &Path) -> Vec<Piece> {
    (0..refs::PIECES)
        .map(|k| {
            let (from, to) = refs::piece_span(k);
            Piece {
                path: path.to_path_buf(),
                from,
                to,
            }
        })
        .collect()
}

fn gpu_options(codec: Codec) -> ExportOptions {
    ExportOptions {
        video: codec,
        prefs: Prefs {
            decode: Choice::Gpu,
            encode: Choice::Gpu,
            ..Prefs::default()
        },
        ..ExportOptions::default()
    }
}

fn cut(src: &Path, codec: Codec, name: &str, broken: bool) -> (Exported, Duration) {
    let out = dir().join(name);
    let work = std::path::absolute(out.with_extension("parts")).unwrap();
    if broken {
        BROKEN.lock().unwrap().push(work.clone());
    }
    let start = Instant::now();
    let done = export(&out, &pieces(src), gpu_options(codec), |_| true);
    let wall = start.elapsed();
    if broken {
        BROKEN.lock().unwrap().retain(|w| *w != work);
    }
    (done.unwrap(), wall)
}

fn sps_look(codec: Codec, nal: &[u8]) -> Look {
    let vui = match codec {
        Codec::H264 => {
            let rbsp = crate::syntax::bits::unescape_rbsp(nal.get(1..)?);
            let v = crate::syntax::h264::params::Sps::parse(&rbsp).ok()?.vui?;
            (
                v.sar,
                v.full_range,
                v.colour_primaries,
                v.transfer_characteristics,
                v.matrix_coefficients,
            )
        }
        Codec::Hevc => {
            let rbsp = crate::syntax::bits::unescape_rbsp(nal.get(2..)?);
            let v = crate::syntax::hevc::params::Sps::parse(&rbsp).ok()?.vui?;
            (
                v.sar,
                v.full_range,
                v.colour_primaries,
                v.transfer_characteristics,
                v.matrix_coefficients,
            )
        }
        _ => return None,
    };
    let (sar, full_range, primaries, transfer, matrix) = vui;
    let sar = if sar.0 == 0 || sar.1 == 0 { (0, 0) } else { sar };
    Some((
        sar,
        Color {
            primaries,
            transfer,
            matrix,
            full_range,
        },
    ))
}

fn is_sps(codec: Codec, nal: &[u8]) -> bool {
    match codec {
        Codec::H264 => nal.first().is_some_and(|h| h & 0x1f == 7),
        _ => nal.first().is_some_and(|h| (h >> 1) & 0x3f == 33),
    }
}

fn nals(data: &[u8], length_size: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while length_size > 0 && at + length_size <= data.len() {
        let len = data[at..at + length_size]
            .iter()
            .fold(0usize, |n, b| (n << 8) | usize::from(*b));
        at += length_size;
        let Some(nal) = data.get(at..at + len) else {
            break;
        };
        out.push(nal);
        at += len;
    }
    out
}

fn frame_looks(path: &Path, codec: Codec) -> Vec<Look> {
    let d = Demux::open(path).unwrap();
    let r = d.mp4().expect("an mp4");
    let track = d.video().unwrap();
    let t = &r.tracks[track];
    let mut buf = Vec::new();
    let mut looks = Vec::with_capacity(t.samples.len());
    for (i, s) in t.samples.iter().enumerate() {
        let entry = &t.entries[s.entry as usize];
        let length_size = match &entry.codec {
            crate::mp4::CodecConfig::Avc(c) => c.length_size,
            crate::mp4::CodecConfig::Hevc(c) => c.length_size,
            other => panic!("{}: a {} sample", path.display(), other.name()),
        };
        let mut look = entry
            .codec
            .extradata()
            .and_then(|x| crate::gpu::look(codec, &x));
        r.read_into(track, i, &mut buf).unwrap();
        for nal in nals(&buf, usize::from(length_size)) {
            if is_sps(codec, nal) {
                look = sps_look(codec, nal);
            }
        }
        looks.push(look);
    }
    looks
}

fn decodes_clean(path: &Path) -> u64 {
    let prefs = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let video = decode_video(path, 0.0, None, &prefs).unwrap();
    let mut n = 0;
    for f in video {
        f.unwrap_or_else(|e| panic!("{} frame {n}: {e}", path.display()));
        n += 1;
    }
    n
}

fn per_edge(e: &Exported) -> f64 {
    e.sessions.setup.as_secs_f64() * 1000.0 / f64::from(e.sessions.edges.max(1))
}

fn report(label: &str, e: &Exported, wall: Duration) {
    eprintln!(
        "{label}: {} edges, {} sessions opened, {} reused, {} refreshed, setup {:.1} ms per edge, cut {:.0} ms",
        e.sessions.edges,
        e.sessions.opened,
        e.sessions.reused,
        e.sessions.refreshed,
        per_edge(e),
        wall.as_secs_f64() * 1000.0
    );
}

fn source_look(src: &Path, codec: Codec, tag: &str) -> Look {
    let want = frame_looks(src, codec);
    let first = *want.first().unwrap();
    assert!(want.iter().all(|t| *t == first), "{tag} source tags vary");
    assert_eq!(want.len() as u64, refs::FRAMES, "{tag} source frames");
    first
}

fn keeps_the_look(e: &Exported, codec: Codec, first: Look, what: &str) {
    assert_eq!(
        decodes_clean(&e.path),
        e.frames,
        "{what} doesn't decode clean"
    );
    let got = frame_looks(&e.path, codec);
    assert_eq!(got.len() as u64, e.frames, "{what}");
    for (n, t) in got.iter().enumerate() {
        assert_eq!(*t, first, "{what} frame {n}");
    }
}

fn smart_cut_on_va(src: &Path, codec: Codec, tag: &str) {
    let first = source_look(src, codec, tag);

    let (fresh, fresh_wall) = cut(src, codec, &format!("edges-{tag}-fresh.mp4"), true);
    let (kept, kept_wall) = cut(src, codec, &format!("edges-{tag}-kept.mp4"), false);
    report(
        &format!("{tag} with a fresh session per edge"),
        &fresh,
        fresh_wall,
    );
    report(&format!("{tag} with sessions kept"), &kept, kept_wall);

    for (e, label) in [(&kept, "kept"), (&fresh, "fresh")] {
        assert!(e.smart, "{tag} {label}: {:?}", e.reasons);
        assert_eq!(e.video.route, Route::Gpu, "{tag} {label}: {:?}", e.reasons);
        assert_eq!(e.video.codec, codec);
        assert!(e.copied > 0 && e.encoded > 0, "{tag} {label}");
        assert!(
            refs::frames_per_cut().contains(&e.frames),
            "{tag} {label}: {} frames",
            e.frames
        );
        assert!(e.sessions.edges as usize >= refs::PIECES, "{tag} {label}");
        keeps_the_look(e, codec, first, &format!("{tag} {label}"));
    }
    assert_eq!(kept.frames, fresh.frames);

    assert_eq!(kept.sessions.refreshed, 0, "{:?}", kept.reasons);
    let workers = kept.sessions.workers;
    assert!((1..=4).contains(&workers), "{:?}", kept.sessions);
    assert!(
        kept.sessions.opened <= 1 + 3 * workers,
        "{:?}",
        kept.sessions
    );
    assert!(
        kept.sessions.reused >= kept.sessions.edges - workers,
        "{:?}",
        kept.sessions
    );

    assert_eq!(fresh.sessions.reused, 0, "{:?}", fresh.sessions);
    assert!(fresh.sessions.refreshed > 0, "{:?}", fresh.sessions);
    assert!(
        fresh.sessions.opened > kept.sessions.opened,
        "{:?} against {:?}",
        fresh.sessions,
        kept.sessions
    );
    assert!(
        fresh.reasons.iter().any(|r| r.contains("couldn't restart")),
        "{:?}",
        fresh.reasons
    );
    assert!(
        !kept.reasons.iter().any(|r| r.contains("couldn't re")),
        "{:?}",
        kept.reasons
    );
}

fn tags_on_va(src: &Path, codec: Codec, tag: &str) {
    let first = source_look(src, codec, tag);
    let (e, wall) = cut(src, codec, &format!("edges-{tag}.mp4"), false);
    report(tag, &e, wall);
    assert!(e.smart, "{tag}: {:?}", e.reasons);
    assert_eq!(e.video.route, Route::Gpu, "{tag}: {:?}", e.reasons);
    assert!(e.copied > 0 && e.encoded > 0, "{tag}");
    assert_eq!(e.sessions.refreshed, 0, "{tag}: {:?}", e.reasons);
    keeps_the_look(&e, codec, first, tag);
}

#[test]
fn a_1080p_h264_cut_on_va_api_opens_its_sessions_once_and_keeps_the_sources_tags() {
    if !ready(Codec::H264) {
        return;
    }
    smart_cut_on_va(&h264_1080p(), Codec::H264, "h264");
}

#[test]
fn an_hevc_cut_on_va_api_opens_its_sessions_once_and_keeps_the_sources_tags() {
    if !ready(Codec::Hevc) {
        return;
    }
    smart_cut_on_va(&fixture(refs::HEVC_TAGGED), Codec::Hevc, "hevc");
}

#[test]
fn an_h264_cut_of_an_untagged_source_with_no_aspect_leaves_its_edges_untagged() {
    if !ready(Codec::H264) {
        return;
    }
    tags_on_va(
        &fixture(refs::H264_UNTAGGED),
        Codec::H264,
        "h264-untagged",
    );
}

#[test]
fn an_h264_cut_of_an_anamorphic_full_range_source_keeps_its_aspect_and_range() {
    if !ready(Codec::H264) {
        return;
    }
    tags_on_va(
        &fixture(refs::H264_ANAMORPHIC_FULL),
        Codec::H264,
        "h264-anamorphic-full",
    );
}

#[test]
fn an_hevc_cut_of_an_untagged_source_with_no_aspect_leaves_its_edges_untagged() {
    if !ready(Codec::Hevc) {
        return;
    }
    tags_on_va(
        &fixture(refs::HEVC_UNTAGGED),
        Codec::Hevc,
        "hevc-untagged",
    );
}

#[test]
fn an_hevc_cut_of_an_anamorphic_full_range_source_keeps_its_aspect_and_range() {
    if !ready(Codec::Hevc) {
        return;
    }
    tags_on_va(
        &fixture(refs::HEVC_ANAMORPHIC_FULL),
        Codec::Hevc,
        "hevc-anamorphic-full",
    );
}

#[test]
fn the_edge_fixtures_carry_the_looks_they_were_recorded_with() {
    for f in &refs::FIXTURES {
        let codec = if f.hevc { Codec::Hevc } else { Codec::H264 };
        let look = source_look(&fixture(f.name), codec, f.name);
        let full = f.color.contains(&"pc");
        let anamorphic = f.sar == "8/9" || f.sar == "4/3";
        match look {
            Some((sar, color)) => {
                assert_eq!(anamorphic, sar != (0, 0) && sar != (1, 1), "{}: {sar:?}", f.name);
                assert_eq!(color.full_range, full, "{}", f.name);
                assert_eq!(color.described(), !f.color.is_empty(), "{}", f.name);
            }
            None => assert!(!anamorphic && f.color.is_empty(), "{}: no VUI", f.name),
        }
        assert!(decodes_clean(&fixture(f.name)) == refs::FRAMES, "{}", f.name);
    }
}

#[test]
fn the_h264_clip_is_tagged_bt709_and_decodes_whole() {
    let src = h264_1080p();
    let look = source_look(&src, Codec::H264, "h264-1080p");
    if let Some((_, color)) = look {
        assert!(!color.full_range);
    }
    assert_eq!(decodes_clean(&src), refs::FRAMES);
}

#[test]
fn pieces_fit_inside_the_source_and_do_not_overlap() {
    let spans: Vec<(f64, f64)> = (0..refs::PIECES).map(refs::piece_span).collect();
    let end = refs::FRAMES as f64 / f64::from(refs::FPS);
    for w in spans.windows(2) {
        assert!(w[0].1 < w[1].0, "{w:?}");
    }
    assert!(spans.iter().all(|(a, b)| 0.0 <= *a && a < b && *b < end));
    let gop = f64::from(refs::GOP) / f64::from(refs::FPS);
    assert!(spans.iter().all(|(a, b)| b - a > gop * 1.5), "{spans:?}");
    assert_eq!(*refs::frames_per_cut().start(), 9 * refs::PIECES as u64);
    assert_eq!(*refs::frames_per_cut().end(), 10 * refs::PIECES as u64);
}

#[test]
fn length_prefixed_nals_split_and_stop_at_a_short_tail() {
    let data = [0, 0, 0, 2, 0x67, 1, 0, 0, 0, 1, 0x68, 0, 0, 0, 9, 1];
    let got = nals(&data, 4);
    assert_eq!(got, vec![&[0x67, 1][..], &[0x68][..]]);
    assert!(is_sps(Codec::H264, got[0]));
    assert!(!is_sps(Codec::H264, got[1]));
    assert!(is_sps(Codec::Hevc, &[33 << 1, 1]));
    assert!(nals(&data, 0).is_empty());
    assert_eq!(sps_look(Codec::H264, &[0x67]), None);
    assert_eq!(sps_look(Codec::Av1, &[0x67, 0, 0]), None);
}

#[test]
fn an_export_with_no_pieces_is_refused() {
    let r = export(
        &dir().join("edges-empty.mp4"),
        &[],
        ExportOptions::default(),
        |_| true,
    );
    assert!(matches!(r, Err(Error::Invalid(_))));
}
