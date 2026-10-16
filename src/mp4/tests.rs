use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-mp4");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tools() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| {
        Command::new(t)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

fn generate(name: &str, seconds: u32, video: &[&str]) -> PathBuf {
    let path = dir().join(name);
    if path.exists() {
        return path;
    }
    let tmp = dir().join(format!(
        ".{}-{:?}-{name}",
        std::process::id(),
        std::thread::current().id()
    ));
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size=320x240:rate=30:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "sine=frequency=440:sample_rate=48000:duration={seconds}"
        ))
        .args(video)
        .args([
            "-c:a", "aac", "-b:a", "96k", "-ac", "2", "-pix_fmt", "yuv420p",
        ])
        .arg(&tmp)
        .status()
        .unwrap();
    assert!(st.success());
    if let Err(e) = std::fs::rename(&tmp, &path) {
        assert!(std::path::Path::new(&path).exists(), "{e}");
        let _ = std::fs::remove_file(&tmp);
    }
    path
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn duration(path: &Path, stream: Option<&str>) -> f64 {
    let out = match stream {
        Some(s) => probe(
            path,
            &[
                "-select_streams",
                s,
                "-show_entries",
                "stream=duration",
                "-of",
                "csv=p=0",
            ],
        ),
        None => probe(
            path,
            &["-show_entries", "format=duration", "-of", "csv=p=0"],
        ),
    };
    out.trim().parse().unwrap()
}

fn decodes_clean(path: &Path) -> usize {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v", "-f", "framemd5", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with('#'))
        .count()
}

#[test]
fn reader_matches_ffprobe_packets() {
    if !tools() {
        return;
    }
    let mp4 = generate(
        "bframes.mp4",
        2,
        &["-c:v", "libx264", "-bf", "3", "-g", "30"],
    );
    let mov = generate(
        "hevc.mov",
        2,
        &[
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=none:bframes=4",
            "-tag:v",
            "hvc1",
        ],
    );
    let frag = generate(
        "fragmented.mp4",
        2,
        &[
            "-c:v",
            "libx264",
            "-bf",
            "3",
            "-g",
            "30",
            "-movflags",
            "frag_keyframe+empty_moov",
        ],
    );
    for path in [mp4, mov, frag] {
        let r = Reader::open(&path).unwrap();
        assert_eq!(r.quicktime, path.extension().unwrap() == "mov");
        for (i, t) in r.tracks.iter().enumerate() {
            let listed = probe(
                &path,
                &[
                    "-select_streams",
                    &i.to_string(),
                    "-show_entries",
                    "packet=pts,dts,size,flags",
                    "-of",
                    "csv=p=0",
                ],
            );
            let theirs: Vec<String> = listed
                .lines()
                .map(|l| {
                    let f: Vec<&str> = l.split(',').collect();
                    let key = if f[3].contains('K') { "K" } else { "_" };
                    format!("{},{},{},{key}", f[0], f[1], f[2])
                })
                .collect();
            let ours: Vec<String> = (0..t.samples.len())
                .map(|k| {
                    let p = r.packet(i, k).unwrap();
                    let key = if p.keyframe { "K" } else { "_" };
                    format!("{},{},{},{key}", p.pts, p.dts, p.data.len())
                })
                .collect();
            assert_eq!(ours, theirs, "{} track {i}", path.display());
        }
        let v = r.video().unwrap();
        assert!(matches!(
            r.tracks[v].codec(),
            Some(crate::media::Codec::H264 | crate::media::Codec::Hevc)
        ));
        assert!((r.tracks[v].fps() - 30.0).abs() < 0.01);
        let a = r.audio()[0];
        assert_eq!(
            (r.tracks[a].channels(), r.tracks[a].sample_rate()),
            (2, 48_000)
        );
        assert!((80_000..112_000).contains(&r.tracks[a].bitrate()));
    }
}

#[cfg(feature = "aac")]
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

#[cfg(feature = "aac")]
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

#[cfg(feature = "aac")]
#[test]
fn written_files_read_back_in_ffprobe() {
    if !tools() {
        return;
    }
    let src = generate(
        "bframes.mp4",
        2,
        &["-c:v", "libx264", "-bf", "3", "-g", "30"],
    );
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
        add_tone(&mut w, 2.0);
        if fast {
            w.finish_faststart().unwrap();
        } else {
            w.finish().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let at = |k: &[u8]| bytes.windows(4).position(|w| w == k).unwrap();
        assert_eq!(fast, at(b"moov") < at(b"mdat"), "{name}");
        let brand_probe = probe(
            &path,
            &[
                "-show_entries",
                "format=format_name:format_tags=major_brand",
                "-of",
                "csv=p=0",
            ],
        );
        assert!(
            brand_probe.contains(if fast { "isom" } else { "qt" }),
            "{brand_probe}"
        );
        assert!(
            (duration(&path, Some("v:0")) - 2.0).abs() < 0.04,
            "{name} video"
        );
        assert!(
            (duration(&path, Some("a:0")) - 2.0).abs() < 0.03,
            "{name} audio"
        );
        assert!((duration(&path, None) - 2.0).abs() < 0.04, "{name}");
        assert_eq!(decodes_clean(&path), 60);
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
    assert!(
        probe(
            &path,
            &["-show_entries", "format=duration", "-of", "csv=p=0"]
        )
        .trim()
        .parse::<f64>()
        .unwrap()
            > 1.9
    );
    assert_eq!(decodes_clean(&path), 60);
}

#[test]
fn remux_cuts_play_in_ffmpeg() {
    if !tools() {
        return;
    }
    let src = generate(
        "long.mp4",
        12,
        &[
            "-c:v",
            "libx264",
            "-bf",
            "2",
            "-g",
            "60",
            "-keyint_min",
            "60",
            "-sc_threshold",
            "0",
        ],
    );
    let out = dir().join("cut-out.mp4");
    let span = remux(&src, &out, 3.3, 8.3, Snap::Outward).unwrap();
    assert_eq!(
        span,
        Span {
            start: 3.3,
            end: 8.3
        }
    );
    assert!((duration(&out, None) - 5.0).abs() < 0.04);
    assert_eq!(decodes_clean(&out), 150);
    let out = dir().join("cut-in.mp4");
    let span = remux(&src, &out, 3.3, 8.3, Snap::Inward).unwrap();
    assert_eq!(
        span,
        Span {
            start: 4.0,
            end: 8.0
        }
    );
    assert_eq!(decodes_clean(&out), 120);
    let r = Reader::open(&out).unwrap();
    assert!(r.tracks[r.video().unwrap()].samples[0].keyframe);
    let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/sample/adoption.mp4");
    if sample.exists() {
        let out = dir().join("adoption-cut.mp4");
        let t = std::time::Instant::now();
        let span = remux(&sample, &out, 12.0, 22.0, Snap::Outward).unwrap();
        let took = t.elapsed();
        let size = std::fs::metadata(&out).unwrap().len();
        eprintln!(
            "adoption 10 s cut: {size} bytes in {took:?} ({:.0} MB/s)",
            size as f64 / took.as_secs_f64() / 1e6
        );
        assert_eq!(
            span,
            Span {
                start: 12.0,
                end: 22.0
            }
        );
        assert!((duration(&out, None) - 10.0).abs() < 0.03);
        assert_eq!(decodes_clean(&out), 600);
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
