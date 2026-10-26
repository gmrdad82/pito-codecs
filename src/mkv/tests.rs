use super::*;
use crate::media::Codec;
use crate::testref::Refs;
use std::path::PathBuf;

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

use refs::Packets;

fn dir() -> PathBuf {
    crate::testref::scratch(refs::AREA)
}

fn ours<S: ByteSource>(r: &Reader<S>) -> Vec<Vec<(i64, u32, bool)>> {
    r.tracks
        .iter()
        .map(|t| {
            t.samples
                .iter()
                .map(|s| (s.pts, s.size, s.keyframe))
                .collect()
        })
        .collect()
}

fn in_ms<S: ByteSource>(r: &Reader<S>) -> Vec<Vec<(i64, u32, bool)>> {
    r.tracks
        .iter()
        .map(|t| {
            let (num, den) = (f64::from(t.time_base.num), f64::from(t.time_base.den));
            t.samples
                .iter()
                .map(|s| {
                    let ms = (s.pts as f64 * num * 1000.0 / den).round() as i64;
                    (ms, s.size, s.keyframe)
                })
                .collect()
        })
        .collect()
}

fn span_seconds<S: ByteSource>(r: &Reader<S>) -> f64 {
    let t = &r.tracks[r.video().unwrap()];
    let mut pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
    pts.sort_unstable();
    pts.dedup();
    let step = pts.windows(2).map(|w| w[1] - w[0]).min().unwrap_or(0);
    let end = pts.last().copied().unwrap_or(0) + step;
    end as f64 * f64::from(t.time_base.num) / f64::from(t.time_base.den)
}

fn timed<S: ByteSource>(r: &Reader<S>) -> Packets {
    let t = &r.tracks[r.video().unwrap()];
    refs::normalized(
        t.samples
            .iter()
            .map(|s| {
                (
                    (t.seconds(s.pts) * 1000.0).round() as i64,
                    s.size,
                    s.keyframe,
                )
            })
            .collect(),
    )
}

fn twin(r: &Refs, file: &str) -> Packets {
    let bytes = r.bytes(file);
    if file.ends_with(".ivf") {
        return refs::normalized(refs::ivf_packets(&bytes));
    }
    if file.ends_with(".flv") {
        return refs::normalized(refs::flv_packets(&bytes));
    }
    let m = crate::mp4::Reader::new(bytes).unwrap();
    let v = m.video().unwrap();
    let t = &m.tracks[v];
    refs::normalized(
        (0..t.samples.len())
            .map(|k| {
                let at = t.seconds(t.presentation(k).unwrap());
                let s = t.samples[k];
                ((at * 1000.0).round() as i64, s.size, s.keyframe)
            })
            .collect(),
    )
}

fn same_packets(ours: &Packets, theirs: &Packets, what: &str) {
    assert_eq!(ours.len(), theirs.len(), "{what}");
    for (k, (a, b)) in ours.iter().zip(theirs).enumerate() {
        assert_eq!((a.1, a.2), (b.1, b.2), "{what}: packet {k}");
        assert!(
            (a.0 - b.0).abs() <= 1,
            "{what}: packet {k} at {} ms, not {} ms",
            a.0,
            b.0
        );
    }
}

fn streamed<S: ByteSource>(r: &Reader<S>) -> Vec<Vec<(i64, u32, bool)>> {
    let mut out = vec![Vec::new(); r.tracks.len()];
    let mut stream = r.stream();
    let mut data = Vec::new();
    while let Some(b) = stream.next_into(&mut data).unwrap() {
        out[b.track].push((b.pts, data.len() as u32, b.keyframe));
    }
    out
}

#[test]
fn reader_matches_its_muxers_own_twin_and_streams_and_seeks() {
    let refs_dir = Refs::open(refs::AREA);
    let seconds = f64::from(refs::SECONDS);
    for (file, other) in refs::FOREIGN {
        let path = refs_dir.path(file);
        let r = Reader::open(&path).unwrap();
        assert!(r.warnings.is_empty(), "{file}: {:?}", r.warnings);
        let mine = timed(&r);
        assert_eq!(mine.len() as u32, refs::FPS * refs::SECONDS, "{file}");
        same_packets(
            &mine,
            &twin(&refs_dir, other),
            &format!("{file} against {other}"),
        );
        assert_eq!(r.is_webm(), file.ends_with(".webm"));
        let codecs: Vec<_> = r.tracks.iter().map(|t| t.codec()).collect();
        if r.is_webm() {
            assert_eq!(codecs, [Some(Codec::Vp9)]);
        } else {
            assert_eq!(codecs, [Some(Codec::H264)]);
        }
        let v = r.video().unwrap();
        assert_eq!((r.tracks[v].width, r.tracks[v].height), refs::SIZE);
        assert!((r.duration_seconds() - seconds).abs() < 0.1, "{file}");
        assert_eq!(streamed(&r), ours(&r), "{file}");

        let mut data = Vec::new();
        let lazy = Reader::with_options(
            std::fs::File::open(&path).unwrap(),
            ReadOptions { index: false },
        )
        .unwrap();
        let mut keys = Vec::new();
        for at in [0.0, 0.5, 1.0, seconds - 0.1] {
            let want = r.tracks[v].keyframe_at(r.tracks[v].ticks(at)).unwrap();
            let want = r.tracks[v].samples[want];
            keys.push(want.pts);
            for reader in [&r, &lazy] {
                let mut s = reader.stream();
                let landed = s.seek(v, at).unwrap();
                assert!((landed - r.tracks[v].seconds(want.pts)).abs() < 1e-9);
                let b = loop {
                    let b = s.next_into(&mut data).unwrap().unwrap();
                    if b.track == v {
                        break b;
                    }
                };
                assert_eq!((b.pts, b.keyframe), (want.pts, true), "{file} {at}");
            }
        }
        keys.dedup();
        assert_eq!(
            keys.len(),
            4,
            "{file}: the seeks land on distinct keyframes"
        );
    }
}

#[test]
fn the_webm_writer_refuses_h264() {
    let refs_dir = Refs::open(refs::AREA);
    let h264 = Reader::open(refs_dir.path("h264.mkv")).unwrap();
    let mut w = Writer::new(std::io::Cursor::new(Vec::new()), Options::webm());
    assert!(
        w.add_track(TrackConfig::from_track(&h264.tracks[0]))
            .is_err()
    );
}

#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "gpu",
    feature = "filter",
    feature = "vp9",
    feature = "opus"
))]
mod decoded {
    use super::*;
    use crate::media::{Choice, Frame, Prefs, decode_video};
    use crate::testref::{AudioCodec, Clip, Digest, VideoCodec};
    use std::path::Path;

    fn frames(path: &Path) -> Vec<Frame> {
        let cpu = Prefs {
            decode: Choice::Cpu,
            ..Prefs::default()
        };
        decode_video(path, 0.0, None, &cpu)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .collect::<crate::media::Result<_>>()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn digest(frames: &[Frame]) -> String {
        let mut d = Digest::new();
        for f in frames {
            d.frame(f);
        }
        d.hex()
    }

    #[test]
    fn writer_output_keeps_every_packet_and_frame() {
        let refs_dir = Refs::open(refs::AREA);
        for (file, other, webm) in [
            ("h264.mkv", "h264.flv", false),
            ("vp9.webm", "vp9.ivf", true),
        ] {
            let src = refs_dir.path(file);
            let r = Reader::open(&src).unwrap();
            let out = dir().join(format!("ours.{}", if webm { "webm" } else { "mkv" }));
            let mut w = Writer::create_with(
                &out,
                Options {
                    webm,
                    ..Options::default()
                },
            )
            .unwrap();
            for t in &r.tracks {
                w.add_track(TrackConfig::from_track(t)).unwrap();
            }
            let mut order: Vec<(u64, usize, usize)> = r
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(k, t)| {
                    t.samples
                        .iter()
                        .enumerate()
                        .map(move |(i, s)| (s.offset, k, i))
                })
                .collect();
            order.sort_unstable();
            for (_, k, i) in order {
                w.push(k, &r.packet(k, i).unwrap()).unwrap();
            }
            w.finish().unwrap();
            let back = Reader::open(&out).unwrap();
            assert_eq!(in_ms(&back), in_ms(&r), "{file}");
            same_packets(&timed(&back), &twin(&refs_dir, other), file);
            assert_eq!(back.is_webm(), webm);
            assert!(!back.cues.is_empty());
            let span = span_seconds(&r);
            let d = back.duration_seconds();
            assert!(
                (d - span).abs() < 0.002,
                "{file}: {d} s against the source's {span} s"
            );
            let want = frames(&src);
            let got = frames(&out);
            assert_eq!(got.len(), want.len(), "{file}");
            assert_eq!(digest(&got), digest(&want), "{file}");
        }
    }

    #[test]
    fn mp4_to_mkv() {
        let mp4 = Clip {
            b_frames: 2,
            ..Clip::new((160, 120), (30, 1), 60)
        }
        .make(refs::AREA, "bframes.mp4");
        let out = dir().join("from-mp4.mkv");
        remux_mp4(&mp4, &out).unwrap();
        let a = crate::mp4::Reader::open(&mp4).unwrap();
        let b = Reader::open(&out).unwrap();
        assert_eq!(a.tracks.len(), b.tracks.len());
        assert!(
            a.tracks[a.video().unwrap()]
                .samples
                .iter()
                .any(|s| s.pts != s.dts),
            "the clip has no reordered frames"
        );
        for (x, y) in a.tracks.iter().zip(&b.tracks) {
            let theirs: Vec<(u32, bool)> = x.samples.iter().map(|s| (s.size, s.keyframe)).collect();
            let mine: Vec<(u32, bool)> = y.samples.iter().map(|s| (s.size, s.keyframe)).collect();
            assert_eq!(mine, theirs);
        }
        let want = frames(&mp4);
        let got = frames(&out);
        assert_eq!(got.len(), 60);
        assert_eq!(digest(&got), digest(&want));
    }

    fn cut_source(ext: &str) -> PathBuf {
        let clip = Clip::new((160, 120), (30, 1), 180);
        let clip = if ext == "webm" {
            Clip {
                video: Some(VideoCodec::Vp9),
                audio: Some(AudioCodec::Opus),
                kbps: 64,
                ..clip
            }
        } else {
            clip
        };
        clip.make(refs::AREA, &format!("cut-src.{ext}"))
    }

    #[test]
    fn remux_cuts_keyframe_to_keyframe() {
        for ext in ["mkv", "webm"] {
            let src = cut_source(ext);
            for (snap, keys, start) in [(Snap::Outward, 3, 2.2), (Snap::Inward, 1, 3.0)] {
                let out = dir().join(format!("cut-{snap:?}.{ext}"));
                let span = remux(&src, &out, 2.2, 4.5, snap).unwrap();
                assert!((span.start - start).abs() < 0.01, "{span:?}");
                let r = Reader::open(&out).unwrap();
                assert_eq!(r.is_webm(), ext == "webm");
                let v = r.video().unwrap();
                assert_eq!(r.tracks[v].keyframes().len(), keys);
                let first = r
                    .tracks
                    .iter()
                    .filter_map(|t| t.samples.iter().map(|s| s.pts).min())
                    .min();
                assert!(first.unwrap().abs() <= 30, "{first:?}");
                assert_eq!(streamed(&r), ours(&r), "{ext} {snap:?}");
                assert_eq!(
                    frames(&out).len(),
                    r.tracks[v].samples.len(),
                    "{ext} {snap:?}"
                );
            }
        }
    }

    struct Counted {
        file: std::fs::File,
        read: std::sync::atomic::AtomicU64,
    }

    impl ByteSource for Counted {
        fn len(&self) -> u64 {
            self.file.len()
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
            self.read
                .fetch_add(buf.len() as u64, std::sync::atomic::Ordering::Relaxed);
            self.file.read_at(offset, buf)
        }
    }

    #[test]
    fn cues_open_and_seek_late_without_a_walk() {
        let path = Clip::new((128, 96), (15, 1), 1800)
            .video_only()
            .make(refs::AREA, "long.mkv");
        let full = Reader::open(&path).unwrap();
        let v = full.video().unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let len = file.metadata().unwrap().len();
        let lazy = Reader::with_options(
            Counted {
                file,
                read: 0.into(),
            },
            ReadOptions { index: false },
        )
        .unwrap();
        assert!(!lazy.cues.is_empty());
        let mut s = lazy.stream();
        let mut data = Vec::new();
        s.seek(v, 110.0).unwrap();
        let b = s.next_into(&mut data).unwrap().unwrap();
        let t = &full.tracks[v];
        let want = t.samples[t.keyframe_at(t.ticks(110.0)).unwrap()];
        assert_eq!((b.pts, b.keyframe), (want.pts, true));
        assert_eq!(
            data,
            full.read(v, t.samples.iter().position(|x| x.pts == want.pts).unwrap())
                .unwrap()
        );
        let read = lazy
            .source()
            .read
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(read < len / 4, "read {read} of {len}");
        let walked: Vec<i64> = t
            .samples
            .iter()
            .filter(|s| s.keyframe)
            .map(|s| s.pts)
            .collect();
        assert!(walked.len() > 2);
        assert_eq!(lazy.cue_keys(), Some(&walked[..]));
        let mut sparse = Reader::open_with(&path, ReadOptions { index: false }).unwrap();
        let mut n = 0;
        sparse.cues.retain(|_| {
            n += 1;
            n % 2 == 1
        });
        assert_eq!(sparse.cue_keys(), None);
    }
}

#[test]
fn a_track_stored_far_behind_the_video_seeks_by_its_own_times() {
    use crate::media::{Packet, TimeBase};
    let path = dir().join("lagging-audio.mkv");
    let ms = TimeBase::new(1, 1000);
    let pcm = || TrackConfig {
        sample_rate: 48_000,
        channels: 2,
        ..TrackConfig::new(TrackKind::Audio, "A_PCM/INT/LIT", ms)
    };
    let mut w = Writer::create(&path).unwrap();
    let v = w
        .add_track(TrackConfig::h264(
            vec![1, 100, 0, 30, 0xff, 0xe0, 0],
            352,
            240,
            ms,
        ))
        .unwrap();
    let a = w.add_track(pcm()).unwrap();
    let b = w.add_track(pcm()).unwrap();
    let packet = |pts: i64, len: usize, keyframe: bool| Packet {
        data: vec![0; len],
        pts,
        dts: pts,
        keyframe,
    };
    let lag = 600;
    for half in 0..2400i64 {
        let at = half * 500;
        if half % 2 == 0 {
            w.push(v, &packet(at, 2000, half % 20 == 0)).unwrap();
        }
        w.push(a, &packet(at, 4096, true)).unwrap();
        if half >= lag && (half - lag) % 200 == 199 {
            for k in half - lag - 199..=half - lag {
                w.push(b, &packet(k * 500, 4096, true)).unwrap();
            }
        }
    }
    for half in 2400 - lag..2400 {
        w.push(b, &packet(half * 500, 4096, true)).unwrap();
    }
    w.finish().unwrap();
    let r = Reader::open_with(&path, ReadOptions { index: false }).unwrap();
    assert!(!r.cues.is_empty());
    for (track, at) in [(b, 600.0), (b, 1100.0), (a, 600.0)] {
        let mut cur = Cursor::new(&r);
        let landed = cur.seek_near(&r, track, at).unwrap();
        assert!(
            landed <= at && landed > at - 10.0,
            "track {track} at {at}: {landed}"
        );
        let mut data = Vec::new();
        let first = cur.next_of(&r, Some(track), &mut data).unwrap().unwrap();
        assert!((r.tracks[track].seconds(first.pts) - landed).abs() < 1e-6);
    }
}

#[test]
fn a_head_indexes_audio_only_when_an_audio_track_is_asked_for() {
    use crate::media::{Packet, TimeBase};
    let path = dir().join("head-audio.mkv");
    let ms = TimeBase::new(1, 1000);
    let mut w = Writer::create(&path).unwrap();
    let v = w
        .add_track(TrackConfig::h264(
            vec![1, 100, 0, 30, 0xff, 0xe0, 0],
            352,
            240,
            ms,
        ))
        .unwrap();
    let a = w
        .add_track(TrackConfig {
            sample_rate: 48_000,
            channels: 2,
            ..TrackConfig::new(TrackKind::Audio, "A_PCM/INT/LIT", ms)
        })
        .unwrap();
    for k in 0..30i64 {
        let packet = |track_byte: u8, keyframe: bool| Packet {
            data: vec![track_byte; 64],
            pts: k * 40,
            dts: k * 40,
            keyframe,
        };
        w.push(v, &packet(1, k % 10 == 0)).unwrap();
        w.push(a, &packet(2, true)).unwrap();
    }
    w.finish().unwrap();
    let head = Reader::open_with(&path, ReadOptions { index: false }).unwrap();
    assert_eq!(head.full().unwrap().tracks[v].samples.len(), 30);
    assert!(head.full().unwrap().tracks[a].samples.is_empty());
    let every = head.indexed_for(a).unwrap();
    assert_eq!(every.tracks[a].samples.len(), 30);
    assert_eq!(every.tracks[a].samples[29].pts, 29 * 40);
    let mut data = Vec::new();
    every.read_into(a, 29, &mut data).unwrap();
    assert_eq!(data, vec![2; 64]);
    assert_eq!(head.indexed_for(v).unwrap().tracks[a].samples.len(), 0);
    assert_eq!(head.indexed_for(99).unwrap().tracks[v].samples.len(), 30);
    let whole = Reader::open(&path).unwrap();
    assert!(std::ptr::eq(whole.indexed_for(a).unwrap(), &whole));
}
