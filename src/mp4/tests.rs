use super::*;
use crate::testref::Refs;

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

#[test]
fn reader_matches_ffprobe_packets() {
    let r = Refs::open(refs::AREA);
    for (file, _) in refs::FOREIGN {
        let path = r.path(file);
        let csv = String::from_utf8(r.bytes(&refs::packets_of(file))).unwrap();
        let listed = refs::parse_packets(&csv);
        let m = Reader::open(&path).unwrap();
        assert_eq!(m.quicktime, file.ends_with(".mov"));
        assert_eq!(m.fragmented, file.starts_with("fragmented"));
        assert_eq!(listed.len(), m.tracks.len(), "{file}");
        for (i, t) in m.tracks.iter().enumerate() {
            let ours: Vec<String> = (0..t.samples.len())
                .map(|k| {
                    let p = m.packet(i, k).unwrap();
                    let key = if p.keyframe { "K" } else { "_" };
                    format!("{},{},{},{key}", p.pts, p.dts, p.data.len())
                })
                .collect();
            assert_eq!(ours, listed[i], "{file} track {i}");
        }
        let v = m.video().unwrap();
        assert!(matches!(
            m.tracks[v].codec(),
            Some(crate::media::Codec::H264 | crate::media::Codec::Hevc)
        ));
        assert_eq!(
            (m.tracks[v].width, m.tracks[v].height),
            refs::SIZE,
            "{file}"
        );
        assert!((m.tracks[v].fps() - f64::from(refs::FPS)).abs() < 0.01);
        let a = m.audio()[0];
        assert_eq!(
            (m.tracks[a].channels(), m.tracks[a].sample_rate()),
            (2, 48_000)
        );
        let kbps = refs::AUDIO_KBPS * 1000;
        assert!(
            (kbps * 5 / 6..kbps * 7 / 6).contains(&m.tracks[a].bitrate()),
            "{file}: {}",
            m.tracks[a].bitrate()
        );
    }
}

#[cfg(all(
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
mod decoded {
    use super::*;
    use crate::media::{Choice, Frame, Prefs, decode_video};
    use crate::testref::{Clip, Digest};
    use std::path::{Path, PathBuf};

    fn dir() -> PathBuf {
        crate::testref::scratch(refs::AREA)
    }

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

    fn track_seconds(path: &Path, video: bool) -> f64 {
        let r = Reader::open(path).unwrap();
        let t = if video {
            r.video().unwrap()
        } else {
            r.audio()[0]
        };
        r.tracks[t].duration_seconds()
    }

    fn movie_seconds(path: &Path) -> f64 {
        Reader::open(path).unwrap().duration_seconds()
    }

    fn rewrite<W: std::io::Write + std::io::Seek>(r: &Reader, mut w: Writer<W>) -> Writer<W> {
        let v = r.video().unwrap();
        let vt = w
            .add_track(TrackConfig::from_track(&r.tracks[v]).unwrap())
            .unwrap();
        for k in 0..r.tracks[v].samples.len() {
            let s = r.tracks[v].samples[k];
            let data = r.read(v, k).unwrap();
            w.push_sample(vt, &data, s.pts, s.dts, s.duration, s.keyframe)
                .unwrap();
        }
        w
    }

    fn add_tone<W: std::io::Write + std::io::Seek>(w: &mut Writer<W>, seconds: f32) {
        use crate::aac::{Encoder, EncoderConfig};
        use crate::media::AudioBuffer;
        let mut enc = Encoder::new(EncoderConfig {
            rate: 48_000,
            channels: 2,
            bitrate: 128_000,
        })
        .unwrap();
        let mut a = AudioBuffer::new(48_000, 2);
        for i in 0..(48_000.0 * seconds) as usize {
            let s = 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
            a.samples.extend([s, s]);
        }
        let mut cfg = TrackConfig::new(
            SampleEntry::aac(enc.extradata(), 2, 48_000, 128_000),
            48_000,
        );
        cfg.media_start = Some(enc.priming() as i64);
        let at = w.add_track(cfg).unwrap();
        let mut packets: Vec<crate::media::Packet> = enc.encode(&a).unwrap();
        packets.extend(enc.flush());
        for p in &packets {
            w.push(at, p).unwrap();
        }
    }

    #[test]
    fn written_files_read_back_with_every_frame() {
        let refs_dir = Refs::open(refs::AREA);
        let src = refs_dir.path("bframes.mp4");
        let seconds = f64::from(refs::SECONDS);
        let frames_in = (refs::SECONDS * refs::FPS) as usize;
        let want = digest(&frames(&src));
        let r = Reader::open(&src).unwrap();
        for (name, brand, fast) in [
            ("out.mp4", Brand::Mp4, true),
            ("out.mov", Brand::Mov, false),
        ] {
            let path = dir().join(name);
            let mut w = rewrite(
                &r,
                Writer::create_with(&path, Options { brand, reserve: 0 }).unwrap(),
            );
            add_tone(&mut w, seconds as f32);
            if fast {
                w.finish_faststart().unwrap();
            } else {
                w.finish().unwrap();
            }
            let bytes = std::fs::read(&path).unwrap();
            let at = |k: &[u8]| bytes.windows(4).position(|w| w == k).unwrap();
            assert_eq!(fast, at(b"moov") < at(b"mdat"), "{name}");
            let major: &[u8] = if fast { b"isom" } else { b"qt  " };
            assert_eq!(&bytes[8..12], major, "{name}");
            assert_eq!(Reader::open(&path).unwrap().quicktime, !fast, "{name}");
            assert!(
                (track_seconds(&path, true) - seconds).abs() < 0.04,
                "{name} video"
            );
            assert!(
                (track_seconds(&path, false) - seconds).abs() < 0.03,
                "{name} audio"
            );
            assert!((movie_seconds(&path) - seconds).abs() < 0.04, "{name}");
            let back = frames(&path);
            assert_eq!(back.len(), frames_in, "{name}");
            assert_eq!(digest(&back), want, "{name}");
        }
        let path = dir().join("frag.mp4");
        let mut w = FragmentedWriter::create(&path, 0.5).unwrap();
        let v = r.video().unwrap();
        let vt = w
            .add_track(TrackConfig::from_track(&r.tracks[v]).unwrap())
            .unwrap();
        for k in 0..r.tracks[v].samples.len() {
            w.push(vt, &r.packet(v, k).unwrap()).unwrap();
        }
        w.finish().unwrap();
        assert!(Reader::open(&path).unwrap().fragmented);
        assert!(movie_seconds(&path) > seconds - 0.1);
        let back = frames(&path);
        assert_eq!(back.len(), frames_in);
        assert_eq!(digest(&back), want);
    }

    fn long_clip() -> PathBuf {
        Clip {
            gop: 60,
            b_frames: 2,
            ..Clip::new((160, 120), (30, 1), 360)
        }
        .make(refs::AREA, "long.mp4")
    }

    #[test]
    fn remux_cuts_decode_every_frame() {
        let src = long_clip();
        let out = dir().join("cut-out.mp4");
        let span = remux(&src, &out, 3.3, 8.3, Snap::Outward).unwrap();
        assert_eq!(
            span,
            Span {
                start: 3.3,
                end: 8.3
            }
        );
        assert!((movie_seconds(&out) - 5.0).abs() < 0.04);
        assert_eq!(frames(&out).len(), 150);
        let out = dir().join("cut-in.mp4");
        let span = remux(&src, &out, 3.3, 8.3, Snap::Inward).unwrap();
        assert_eq!(
            span,
            Span {
                start: 4.0,
                end: 8.0
            }
        );
        assert_eq!(frames(&out).len(), 120);
        let r = Reader::open(&out).unwrap();
        assert!(r.tracks[r.video().unwrap()].samples[0].keyframe);
        let sample = crate::testref::root().join("tmp/sample/adoption.mp4");
        if sample.exists() {
            let out = dir().join("adoption-cut.mp4");
            let span = remux(&sample, &out, 12.0, 22.0, Snap::Outward).unwrap();
            assert_eq!(
                span,
                Span {
                    start: 12.0,
                    end: 22.0
                }
            );
            assert!((movie_seconds(&out) - 10.0).abs() < 0.03);
            assert_eq!(frames(&out).len(), 600);
        }
    }

    #[test]
    fn b_frames_keep_one_tick_per_sample_and_decode_every_frame() {
        let src = Clip {
            b_frames: 3,
            ..Clip::new((160, 120), (30, 1), 30)
        }
        .video_only()
        .make(refs::AREA, "bframes-tick.mp4");
        let r = Reader::open(&src).unwrap();
        let v = r.video().unwrap();
        let step = i64::from(r.tracks[v].timescale / 30).max(1);
        let mut cfg = TrackConfig::from_track(&r.tracks[v]).unwrap();
        cfg.timescale = 30;
        cfg.edits.clear();
        cfg.media_start = None;
        let out = dir().join("bframes-tick-out.mp4");
        let mut w = Writer::create_with(&out, Options::default()).unwrap();
        let t = w.add_track(cfg).unwrap();
        let n = r.tracks[v].samples.len();
        assert!(
            r.tracks[v].samples.iter().any(|s| s.pts != s.dts),
            "the clip has no reordered frames"
        );
        for k in 0..n {
            let s = r.tracks[v].samples[k];
            let dts = k as i64 - 1;
            w.push_sample(
                t,
                &r.read(v, k).unwrap(),
                (s.pts - s.dts) / step + dts,
                dts,
                1,
                s.keyframe,
            )
            .unwrap();
        }
        w.finish_faststart().unwrap();
        let back = Reader::open(&out).unwrap();
        let bv = back.video().unwrap();
        assert!(back.tracks[bv].samples.iter().all(|s| s.duration == 1));
        let decoded = frames(&out);
        assert_eq!(decoded.len(), n);
        assert!(decoded.iter().all(|f| (f.width, f.height) == (160, 120)));
        assert_eq!(digest(&decoded), digest(&frames(&src)));
    }
}

#[cfg(feature = "aac")]
#[test]
fn ten_minutes_of_silent_aac_in_a_small_file_read_every_sample() {
    use crate::media::Packet;
    let asc = [0x11, 0x90];
    let frame = vec![0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c];
    let mut dec = crate::aac::Decoder::new(&asc).unwrap();
    let n = 28_125usize;
    let packet = |i: i64| Packet {
        data: frame.clone(),
        pts: i * 1024,
        dts: i * 1024,
        keyframe: true,
    };
    let pcm = dec.decode(&packet(0)).unwrap();
    assert!(pcm.samples.len() == 2048 && pcm.samples.iter().all(|&s| s == 0.0));
    let mut w = Writer::new(std::io::Cursor::new(Vec::new()), Options::default()).unwrap();
    let t = w
        .add_track(TrackConfig::new(
            SampleEntry::aac(asc.to_vec(), 2, 48_000, 128_000),
            48_000,
        ))
        .unwrap();
    for i in 0..n as i64 {
        w.push(t, &packet(i)).unwrap();
    }
    let bytes = w.finish().unwrap().into_inner();
    assert!(bytes.len() < n * 8, "{}", bytes.len());
    let r = Reader::new(bytes).unwrap();
    let s = &r.tracks[0].samples;
    assert_eq!(s.len(), n);
    assert!(s.iter().all(|s| s.size as usize == frame.len()));
}

#[test]
fn an_av1c_record_reads_and_writes_back_byte_for_byte() {
    let raw = [
        0x81, 0x01, 0x4c, 0x00, 0x0a, 0x0c, 0x02, 0x00, 0x00, 0x0d, 0x26, 0x27, 0xfb, 0x38, 0x05,
        0xf0, 0x80, 0x80,
    ];
    let c = Av1Config::parse(&raw).unwrap();
    assert_eq!(
        (c.seq_profile, c.bit_depth(), c.profile_name().as_str()),
        (0, 10, "Main")
    );
    assert_eq!(c.to_bytes(), raw);
    assert!(Av1Config::parse(&raw[..3]).is_err());
}

#[test]
fn a_track_title_is_written_as_udta_name_and_read_back() {
    let entry = || SampleEntry::aac(vec![0x11, 0x90], 2, 48_000, 0);
    let mut w = Writer::new(std::io::Cursor::new(Vec::new()), Options::default()).unwrap();
    let mut titled = TrackConfig::new(entry(), 48_000);
    titled.title = Some("Voice ü".into());
    let a = w.add_track(titled).unwrap();
    let mut blank = TrackConfig::new(entry(), 48_000);
    blank.title = Some(String::new());
    let b = w.add_track(blank).unwrap();
    let c = w.add_track(TrackConfig::new(entry(), 48_000)).unwrap();
    for t in [a, b, c] {
        for k in 0..3 {
            w.push_sample(t, &[0x21, 0x10, 0x04], k * 1024, k * 1024, 1024, true)
                .unwrap();
        }
    }
    let bytes = w.finish_faststart().unwrap().into_inner();
    let title = "Voice ü".as_bytes();
    let needle = [
        &(16 + title.len() as u32).to_be_bytes()[..],
        b"udta",
        &(8 + title.len() as u32).to_be_bytes(),
        b"name",
        title,
    ]
    .concat();
    assert_eq!(
        bytes
            .windows(needle.len())
            .filter(|w| *w == &needle[..])
            .count(),
        1
    );
    assert_eq!(bytes.windows(4).filter(|w| *w == b"udta").count(), 1);
    let r = Reader::new(bytes).unwrap();
    let names: Vec<&str> = r.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Voice ü", "SoundHandler", "SoundHandler"]);
}
