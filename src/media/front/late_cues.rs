use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::demux::{mkv_head, video_end};
use super::probe::probe;
use super::route::{Choice, Prefs};
use super::still::{first_frame, frames_at};
use crate::mkv::{self, Reader};
use crate::mp4::trace::Watch;
use crate::testref::{Clip, Picture};

const AREA: &str = "late-cues";
const STILLS: [f64; 10] = [0.0, 0.4, 1.7, 3.0, 4.6, 6.2, 7.5, 9.1, 10.8, 11.9];
const HEIGHT: u32 = 120;

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
            ..Clip::new((240, 160), (15, 1), 180)
        };
        std::fs::read(clip.make(AREA, "source.mkv")).unwrap()
    })
}

fn copy(name: &str, bytes: &[u8]) -> PathBuf {
    let path = crate::testref::scratch(AREA).join(name);
    crate::testref::write_atomic(&path, bytes).unwrap();
    path
}

fn cues_span(bytes: &[u8]) -> (u64, u64) {
    Reader::with_options(bytes.to_vec(), mkv::ReadOptions { index: false })
        .unwrap()
        .tail_cues_span()
        .unwrap()
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

fn landings(path: &Path, eager: bool) -> Vec<(i64, Vec<u8>)> {
    let r = mkv_head(path).unwrap().unwrap();
    if eager {
        r.cues();
    }
    let v = r.video().unwrap();
    let mut s = r.stream();
    let mut data = Vec::new();
    STILLS
        .iter()
        .rev()
        .map(|&at| {
            s.seek(v, at).unwrap();
            loop {
                let b = s.next_into(&mut data).unwrap().unwrap();
                if b.track == v {
                    return (b.pts, data.clone());
                }
            }
        })
        .collect()
}

fn truth(bytes: &[u8]) -> Vec<i64> {
    let r = Reader::new(bytes.to_vec()).unwrap();
    let v = r.video().unwrap();
    let t = &r.tracks[v];
    STILLS
        .iter()
        .rev()
        .map(|&at| t.samples[t.keyframe_at(t.ticks(at)).unwrap()].pts)
        .collect()
}

fn stills(path: &Path) -> Vec<Vec<u8>> {
    frames_at(path, &STILLS, HEIGHT, &cpu())
        .unwrap()
        .into_iter()
        .map(|s| s.data)
        .collect()
}

#[test]
fn the_probe_and_first_frame_leave_the_cues_unread_until_a_seek_needs_them() {
    let bytes = source();
    let span = cues_span(bytes);
    let lazy = copy("lazy.mkv", bytes);
    let eager = copy("eager.mkv", bytes);
    let alone = copy("alone.mkv", bytes);
    let quiet = Watch::new(&alone);
    first_frame(&alone, HEIGHT, &cpu()).unwrap();
    let second = Reader::new(bytes.to_vec()).unwrap().clusters[1].offset;
    let shown = quiet.reads();
    drop(quiet);
    assert!(!shown.is_empty());
    assert!(shown.iter().all(|r| r.0 < second), "{second} {shown:?}");
    assert_eq!(inside(&shown, span), 0);
    let watch = Watch::new(&lazy);
    let probed = probe(&lazy, &cpu()).unwrap();
    let first = first_frame(&lazy, HEIGHT, &cpu()).unwrap();
    let head = mkv_head(&lazy).unwrap().unwrap();
    assert!(!head.cues_read());
    let opened = watch.reads();
    assert!(!opened.is_empty());
    assert_eq!(inside(&opened, span), 0, "{span:?} {opened:?}");
    let farthest = opened.iter().map(|&(at, n)| at + n).max().unwrap();
    assert!(farthest <= span.0);

    frames_at(&lazy, &[STILLS[4]], HEIGHT, &cpu()).unwrap();
    assert!(head.cues_read());
    let once = starting_in(&watch.reads(), span);
    assert!(once > 0);
    let ours = stills(&lazy);
    let landed = landings(&lazy, false);
    assert_eq!(starting_in(&watch.reads(), span), once);
    drop(watch);

    let full = mkv_head(&eager).unwrap().unwrap();
    full.cues();
    assert!(!full.cues().is_empty());
    assert_eq!(head.cues(), full.cues());
    assert_eq!(probed, probe(&eager, &cpu()).unwrap());
    assert_eq!(first, first_frame(&eager, HEIGHT, &cpu()).unwrap());
    assert_eq!(ours, stills(&eager));
    assert_eq!(landed, landings(&eager, true));
    let pts: Vec<i64> = landed.iter().map(|l| l.0).collect();
    assert_eq!(pts, truth(bytes));
    assert!(ours.windows(2).any(|w| w[0] != w[1]));
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
    let whole = Reader::new(source().to_vec()).unwrap();
    let most = whole.tracks[v]
        .samples
        .iter()
        .map(|s| s.pts + s.duration as i64)
        .max();
    assert_eq!(Some(ahead), most);
}

fn without_cues_entry(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let cues = [0x1C, 0x53, 0xBB, 0x6B];
    let at = out[..4096].windows(4).position(|w| w == cues).unwrap();
    out[at..at + 4].copy_from_slice(&[0x12, 0x54, 0xC3, 0x67]);
    out
}

fn with_damaged_cues(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let (from, to) = cues_span(bytes);
    for b in out[from as usize + 12..to as usize].iter_mut().step_by(7) {
        *b ^= 0x5A;
    }
    out
}

#[test]
fn files_without_a_cues_entry_or_with_damaged_cues_seek_as_before() {
    let bytes = source();
    for (name, changed) in [
        ("unlisted", without_cues_entry(bytes)),
        ("damaged", with_damaged_cues(bytes)),
    ] {
        let lazy = copy(&format!("{name}-lazy.mkv"), &changed);
        let eager = copy(&format!("{name}-eager.mkv"), &changed);
        let ours = stills(&lazy);
        let landed = landings(&lazy, false);
        let theirs = landings(&eager, true);
        assert_eq!(landed, theirs, "{name}");
        assert_eq!(ours, stills(&eager), "{name}");
        assert_eq!(
            probe(&lazy, &cpu()).unwrap(),
            probe(&eager, &cpu()).unwrap(),
            "{name}"
        );
    }
    let unlisted = mkv_head(&copy("unlisted.mkv", &without_cues_entry(bytes)))
        .unwrap()
        .unwrap();
    assert!(unlisted.cues().is_empty());
}
