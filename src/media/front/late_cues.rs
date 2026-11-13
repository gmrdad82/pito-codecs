use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use super::demux::{mkv_head, video_end};
use super::probe::probe;
use super::route::{Choice, Prefs};
use super::still::{Seek, first_frame, frames_at, frames_at_seek};
use crate::media::Rgba;
use crate::mkv::{self, Reader};
use crate::mp4::trace::Watch;
use crate::testref::{Clip, Picture, sha256};

const AREA: &str = "late-cues";
const STILLS: [f64; 10] = [0.0, 0.4, 1.7, 3.0, 4.6, 6.2, 7.5, 9.1, 10.2, 10.9];
const HEIGHT: u32 = 120;
const REORDER: usize = 17;
const WIN: u64 = 64 << 10;

type Look = (&'static str, fn(&Path), bool);

struct Today {
    name: &'static str,
    duration: f64,
    probe: &'static str,
    first: &'static str,
    pts: [i64; 10],
    landed: &'static str,
    keyframe: &'static str,
    exact: &'static str,
}

const V4_0_1: [Today; 5] = [
    Today {
        name: "source",
        duration: 11.0,
        probe: "ec5434b87719ae2ca06c0e528f9ad19575fbde2db9b53ca526a72173f676de37",
        first: "7a68be25a26ebe3c829758a62ab21cb02fb3fe861266b309980b4a0055f290bc",
        pts: [9933, 9933, 8400, 7133, 5667, 4133, 2867, 1467, 0, 0],
        landed: "218cf16e16a6d310c0af1c89fb29e62249c56c5c2c35adb207bd086bd54acba7",
        keyframe: "7a84fa093737fe1be6c6092330e009106402fec6061cb3b3bb2dd97156fbd65f",
        exact: "1ec36063d00fe91f2d010c1f2cc22cbd46243db238c2baa0945fdee7dcc302e3",
    },
    Today {
        name: "unlisted",
        duration: 11.0,
        probe: "ec5434b87719ae2ca06c0e528f9ad19575fbde2db9b53ca526a72173f676de37",
        first: "7a68be25a26ebe3c829758a62ab21cb02fb3fe861266b309980b4a0055f290bc",
        pts: [9933, 9933, 8400, 5667, 5667, 2867, 2867, 0, 0, 0],
        landed: "b0d9fe3cda7df3c75425b5b874cbd55dc41c09b302527f0986c3ba8267112837",
        keyframe: "54568ff3bde4f148e9157ec0a6aa6d822c0ab1194fdde23cadb31a9198f959cc",
        exact: "1ec36063d00fe91f2d010c1f2cc22cbd46243db238c2baa0945fdee7dcc302e3",
    },
    Today {
        name: "damaged",
        duration: 11.0,
        probe: "ec5434b87719ae2ca06c0e528f9ad19575fbde2db9b53ca526a72173f676de37",
        first: "0c730ce43abee2c57d92359bc8ad4cce6e8bbffcddad45697b4b3b05b4f096c2",
        pts: [2867; 10],
        landed: "20e361f3c79534fbd0f7d00c54c6b04a6e2776f02b4dcab8774199f5a503850f",
        keyframe: "9d9e930e51bd5b6bc75244f25b10cbe5bfcea6a6d91eb90887a1d3ded659ed18",
        exact: "b31495b8d80e5dad920891c356a2c061734f6a663b1a93286615caccc7d82cfb",
    },
    Today {
        name: "longer",
        duration: 11.0,
        probe: "ec5434b87719ae2ca06c0e528f9ad19575fbde2db9b53ca526a72173f676de37",
        first: "7a68be25a26ebe3c829758a62ab21cb02fb3fe861266b309980b4a0055f290bc",
        pts: [9933, 9933, 8400, 7133, 5667, 4133, 2867, 1467, 0, 0],
        landed: "218cf16e16a6d310c0af1c89fb29e62249c56c5c2c35adb207bd086bd54acba7",
        keyframe: "7a84fa093737fe1be6c6092330e009106402fec6061cb3b3bb2dd97156fbd65f",
        exact: "1ec36063d00fe91f2d010c1f2cc22cbd46243db238c2baa0945fdee7dcc302e3",
    },
    Today {
        name: "nested",
        duration: 11.0,
        probe: "ec5434b87719ae2ca06c0e528f9ad19575fbde2db9b53ca526a72173f676de37",
        first: "7a68be25a26ebe3c829758a62ab21cb02fb3fe861266b309980b4a0055f290bc",
        pts: [9933, 9933, 8400, 7133, 5667, 4133, 2867, 1467, 0, 0],
        landed: "218cf16e16a6d310c0af1c89fb29e62249c56c5c2c35adb207bd086bd54acba7",
        keyframe: "7a84fa093737fe1be6c6092330e009106402fec6061cb3b3bb2dd97156fbd65f",
        exact: "1ec36063d00fe91f2d010c1f2cc22cbd46243db238c2baa0945fdee7dcc302e3",
    },
];

const CUES_ID: [u8; 4] = [0x1C, 0x53, 0xBB, 0x6B];
const SEEK_HEAD_ID: [u8; 4] = [0x11, 0x4D, 0x9B, 0x74];
const TAGS_ID: [u8; 4] = [0x12, 0x54, 0xC3, 0x67];
const SEGMENT_ID: [u8; 4] = [0x18, 0x53, 0x80, 0x67];
const SEEK_ID: [u8; 3] = [0x53, 0xAB, 0x84];
const SEEK_POSITION: [u8; 2] = [0x53, 0xAC];
const INFO_DURATION: [u8; 3] = [0x44, 0x89, 0x88];

fn cpu() -> Prefs {
    Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    }
}

fn source() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let clip = Clip {
            picture: Picture::Noisy(12),
            b_frames: 2,
            gop: 30,
            ..Clip::new((240, 160), (15, 1), 165)
        };
        std::fs::read(clip.make(AREA, "made.mkv")).unwrap()
    })
}

fn at(bytes: &[u8], pattern: &[u8]) -> usize {
    bytes[..4096]
        .windows(pattern.len())
        .position(|w| w == pattern)
        .unwrap()
}

fn segment_start(bytes: &[u8]) -> usize {
    at(bytes, &SEGMENT_ID) + 12
}

fn cues_entry(bytes: &[u8]) -> (usize, usize, u64) {
    let id = at(bytes, &[&SEEK_ID[..], &CUES_ID[..]].concat()) + SEEK_ID.len();
    let p = id + 4;
    assert_eq!(bytes[p..p + 2], SEEK_POSITION);
    let n = (bytes[p + 2] & 0x7F) as usize;
    let value = bytes[p + 3..p + 3 + n]
        .iter()
        .fold(0u64, |v, b| v << 8 | *b as u64);
    (id, p + 3, value)
}

fn cues_span(bytes: &[u8]) -> (u64, u64) {
    let (_, _, pos) = cues_entry(bytes);
    let from = segment_start(bytes) + pos as usize;
    assert_eq!(bytes[from..from + 4], CUES_ID);
    (from as u64, bytes.len() as u64)
}

fn unlisted(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let (id, _, _) = cues_entry(bytes);
    out[id..id + 4].copy_from_slice(&TAGS_ID);
    out
}

fn damaged(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let (from, to) = cues_span(bytes);
    for b in out[from as usize + 12..to as usize].iter_mut().step_by(7) {
        *b ^= 0x5A;
    }
    out
}

fn longer(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let d = at(bytes, &INFO_DURATION) + 3;
    let said = f64::from_be_bytes(out[d..d + 8].try_into().unwrap());
    out[d..d + 8].copy_from_slice(&(said + 250.0).to_be_bytes());
    out
}

fn nested(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let seg = segment_start(bytes);
    let (id, value_at, cues) = cues_entry(bytes);
    let n = (bytes[value_at - 1] & 0x7F) as usize;
    let mut seek = [&SEEK_ID[..], &CUES_ID[..], &SEEK_POSITION[..]].concat();
    seek.push(0x80 | n as u8);
    seek.extend_from_slice(&cues.to_be_bytes()[8 - n..]);
    let mut entry = vec![0x4D, 0xBB, 0x80 | seek.len() as u8];
    entry.extend_from_slice(&seek);
    let mut head = SEEK_HEAD_ID.to_vec();
    head.push(0x80 | entry.len() as u8);
    head.extend_from_slice(&entry);
    let inner = (out.len() - seg) as u64;
    assert!(inner < 1 << (8 * n));
    out[id..id + 4].copy_from_slice(&SEEK_HEAD_ID);
    out[value_at..value_at + n].copy_from_slice(&inner.to_be_bytes()[8 - n..]);
    out.extend_from_slice(&head);
    let size = ((out.len() - seg) as u64 | 1 << 56).to_be_bytes();
    out[seg - 8..seg].copy_from_slice(&size);
    out
}

fn variant(name: &str) -> Vec<u8> {
    let bytes = source();
    match name {
        "source" => bytes.to_vec(),
        "unlisted" => unlisted(bytes),
        "damaged" => damaged(bytes),
        "longer" => longer(bytes),
        "nested" => nested(bytes),
        _ => unreachable!(),
    }
}

fn copy(name: &str, bytes: &[u8]) -> PathBuf {
    let path = crate::testref::scratch(AREA).join(name);
    crate::testref::write_atomic(&path, bytes).unwrap();
    path
}

fn inside(reads: &[(u64, u64)], (from, to): (u64, u64)) -> usize {
    reads
        .iter()
        .filter(|&&(at, n)| at < to && at + n > from)
        .count()
}

fn starting_in(reads: &[(u64, u64)], (from, to): (u64, u64)) -> usize {
    reads.iter().filter(|r| r.0 >= from && r.0 < to).count()
}

fn whole(bytes: &[u8]) -> Reader<Vec<u8>> {
    Reader::new(bytes.to_vec()).unwrap()
}

fn second_cluster(bytes: &[u8]) -> u64 {
    whole(bytes).clusters[1].offset
}

fn needed_tail(bytes: &[u8]) -> u64 {
    let r = whole(bytes);
    let v = r.video().unwrap();
    let mut seen = 0;
    for (i, c) in r.clusters.iter().enumerate().rev() {
        seen += r.tracks[v]
            .samples
            .iter()
            .filter(|s| s.cluster as usize == i)
            .count();
        if seen >= REORDER {
            return c.offset;
        }
    }
    r.clusters[0].offset
}

fn stills_digest(v: &[Rgba]) -> String {
    let mut all = Vec::new();
    for s in v {
        all.extend_from_slice(&s.width.to_le_bytes());
        all.extend_from_slice(&s.height.to_le_bytes());
        all.extend_from_slice(&s.data);
    }
    sha256(&all)
}

fn landings(r: &mkv::Reader) -> (Vec<i64>, String) {
    let v = r.video().unwrap();
    let mut s = r.stream();
    let mut data = Vec::new();
    let (mut pts, mut all) = (Vec::new(), Vec::new());
    for &at in STILLS.iter().rev() {
        s.seek(v, at).unwrap();
        loop {
            let b = s.next_into(&mut data).unwrap().unwrap();
            if b.track == v {
                pts.push(b.pts);
                all.extend_from_slice(&data);
                break;
            }
        }
    }
    (pts, sha256(&all))
}

fn probe_digest(path: &Path) -> (f64, String) {
    let p = probe(path, &cpu()).unwrap();
    (p.duration, sha256(format!("{p:?}").as_bytes()))
}

fn first_digest(path: &Path) -> String {
    stills_digest(&[first_frame(path, HEIGHT, &cpu()).unwrap()])
}

#[test]
fn opening_and_the_first_frame_read_only_the_head_and_the_first_cluster() {
    for name in ["source", "longer", "nested", "damaged"] {
        let bytes = variant(name);
        let second = second_cluster(&bytes);
        let cues = cues_span(source());
        let looks: [Look; 3] = [
            (
                "open",
                |p: &Path| {
                    mkv_head(p).unwrap().unwrap();
                },
                true,
            ),
            (
                "first frame",
                |p: &Path| {
                    first_frame(p, HEIGHT, &cpu()).unwrap();
                },
                true,
            ),
            (
                "exact still at the start",
                |p: &Path| {
                    frames_at_seek(p, &[0.0], HEIGHT, Seek::Exact, &cpu()).unwrap();
                },
                false,
            ),
        ];
        for (how, look, head_only) in looks {
            let path = copy(&format!("{name}-{}.mkv", how.replace(' ', "-")), &bytes);
            let watch = Watch::new(&path);
            look(&path);
            let reads = watch.reads();
            drop(watch);
            assert!(!reads.is_empty(), "{name} {how}");
            assert!(
                !head_only || reads.iter().all(|&(at, n)| at + n <= second),
                "{name} {how}: past {second}: {reads:?}"
            );
            assert_eq!(inside(&reads, cues), 0, "{name} {how}");
            assert!(
                !mkv_head(&path).unwrap().unwrap().cues_read(),
                "{name} {how}"
            );
        }
    }
}

#[test]
fn the_probe_reads_no_cues_and_only_the_clusters_that_hold_the_last_frames() {
    for name in ["source", "longer", "nested", "damaged"] {
        let bytes = variant(name);
        let path = copy(&format!("{name}-probe.mkv"), &bytes);
        let (from, end) = cues_span(source());
        let second = second_cluster(&bytes);
        let needed = needed_tail(&bytes);
        assert!(needed > second, "{name}");
        let watch = Watch::new(&path);
        let (duration, _) = probe_digest(&path);
        let reads = watch.reads();
        drop(watch);
        let today = V4_0_1.iter().find(|t| t.name == name).unwrap();
        assert_eq!(duration, today.duration, "{name}");
        assert!(!mkv_head(&path).unwrap().unwrap().cues_read(), "{name}");
        assert_eq!(inside(&reads, (from, end)), 0, "{name} {reads:?}");
        let past: Vec<_> = reads.iter().filter(|r| r.0 >= end).collect();
        assert_eq!(past.len(), usize::from(name == "nested"), "{name} {past:?}");
        let tail: Vec<_> = reads
            .iter()
            .filter(|r| r.0 + r.1 > second && r.0 < end)
            .collect();
        let lowest = tail.iter().map(|r| r.0).min().unwrap();
        let bytes_read: u64 = tail.iter().map(|r| r.1).sum();
        assert!(
            lowest + (from - needed) + WIN >= needed,
            "{name} {lowest} {tail:?}"
        );
        assert!(
            bytes_read <= 2 * (from - needed) + WIN,
            "{name} {bytes_read} {tail:?}"
        );
        assert!(tail.iter().all(|r| r.0 + r.1 <= from), "{name}");
    }
}

#[test]
fn seeks_and_stills_after_the_first_frame_match_v4_0_1_and_read_the_cues_once() {
    let cues = cues_span(source());
    for today in &V4_0_1 {
        let name = today.name;
        let bytes = variant(name);
        let path = copy(&format!("{name}-today.mkv"), &bytes);
        let head = mkv_head(&path).unwrap().unwrap();
        let watch = Watch::new(&path);
        let probed = probe_digest(&path);
        let first = first_digest(&path);
        assert_eq!(head.cues_read(), name == "unlisted", "{name}");
        if name != "unlisted" {
            assert_eq!(inside(&watch.reads(), cues), 0, "{name}");
        }
        let (pts, landed) = landings(&head);
        assert!(head.cues_read(), "{name}");
        let once = starting_in(&watch.reads(), cues);
        let again = landings(&head);
        let after = starting_in(&watch.reads(), cues);
        drop(watch);
        if name == "unlisted" {
            assert!(head.cues().is_empty());
        } else {
            assert!(once > 0, "{name}");
            assert_eq!(after, once, "{name}");
        }
        assert!(
            Arc::ptr_eq(&head, &mkv_head(&path).unwrap().unwrap()),
            "{name}"
        );
        let keyframe = stills_digest(&frames_at(&path, &STILLS, HEIGHT, &cpu()).unwrap());
        let exact =
            stills_digest(&frames_at_seek(&path, &STILLS, HEIGHT, Seek::Exact, &cpu()).unwrap());
        assert_eq!(
            (probed.0, probed.1.as_str()),
            (today.duration, today.probe),
            "{name}"
        );
        assert_eq!(pts, today.pts, "{name}");
        assert_eq!(landed, today.landed, "{name}");
        assert_eq!(again, (pts.clone(), landed.clone()), "{name}");
        assert_eq!(keyframe, today.keyframe, "{name}");
        assert_eq!(exact, today.exact, "{name}");
        if name == "damaged" {
            assert_ne!(first, today.first, "{name}");
            assert_eq!(first, V4_0_1[0].first, "{name}");
        } else {
            assert_eq!(first, today.first, "{name}");
        }
    }
}

#[test]
fn the_probe_ends_where_the_last_cue_said_it_ends() {
    let path = copy("ends.mkv", source());
    let r = mkv_head(&path).unwrap().unwrap();
    let v = r.video().unwrap();
    let ahead = r.end_before_cues(v).unwrap();
    assert!(!r.cues_read());
    let t = &r.tracks[v];
    let last = r.cues().iter().rev().find(|c| c.track == t.number).unwrap();
    let mut s = r.stream();
    s.seek(v, t.seconds(last.time as i64)).unwrap();
    assert_eq!(Some(ahead), video_end(&mut s, v).unwrap());
    let most = whole(source()).tracks[v]
        .samples
        .iter()
        .map(|s| s.pts + s.duration as i64)
        .max();
    assert_eq!(Some(ahead), most);
}

#[test]
fn a_cues_entry_found_only_in_a_second_seek_head_is_read_on_the_first_seek() {
    let path = copy("nested-cues.mkv", &nested(source()));
    let r = mkv_head(&path).unwrap().unwrap();
    assert!(!r.cues_read());
    let eager = whole(source());
    assert!(!eager.cues().is_empty());
    assert_eq!(r.cues(), eager.cues());
    let unlisted = mkv_head(&copy("unlisted-cues.mkv", &unlisted(source())))
        .unwrap()
        .unwrap();
    assert!(unlisted.cues().is_empty());
}
