use super::*;
use std::path::PathBuf;
use std::process::Command;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-ac3");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn signal(rate: u32, channels: u16, seconds: f32) -> Vec<u8> {
    let n = (rate as f32 * seconds) as usize;
    let mut seed = 0x1234_5678u32;
    let mut out = Vec::with_capacity(n * channels as usize * 4);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let f = 180.0 * (c as f32 + 1.0);
            let burst = if (t * 3.0).fract() < 0.1 { 0.3 } else { 0.0 };
            let s = 0.3 * (std::f32::consts::TAU * f * t).sin()
                + 0.1 * (std::f32::consts::TAU * 4100.0 * t).sin()
                + (0.05 + burst) * noise;
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out
}

fn encode(name: &str, codec: &str, rate: u32, layout: &str, channels: u16, kbps: u32) -> PathBuf {
    let raw = dir().join(format!("{name}.f32"));
    std::fs::write(&raw, signal(rate, channels, 2.0)).unwrap();
    let out = dir().join(format!("{name}.{codec}"));
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "f32le", "-ar"])
        .arg(rate.to_string())
        .args(["-ch_layout", layout, "-i"])
        .arg(&raw)
        .args(["-c:a", codec, "-b:a"])
        .arg(format!("{kbps}k"))
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success());
    out
}

fn ffmpeg_decode(path: &PathBuf) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(n) = frame_len(&data[pos..]).filter(|&n| n > 0 && pos + n <= data.len()) {
        out.push(&data[pos..pos + n]);
        pos += n;
    }
    out
}

fn without_dither_flags(path: &PathBuf) -> (PathBuf, usize) {
    let data = std::fs::read(path).unwrap();
    let i = info(&data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    let mut patched = data.clone();
    let mut off = 0;
    let mut cleared = 0;
    let mut buf = Vec::new();
    for f in frames(&data) {
        d.decode_into(f, &mut buf).unwrap();
        for m in d.marks() {
            let bit = off * 8 + m;
            patched[bit / 8] &= !(0x80 >> (bit % 8));
            cleared += 1;
        }
        off += f.len();
    }
    let out = path.with_extension(format!(
        "plain.{}",
        path.extension().unwrap().to_string_lossy()
    ));
    std::fs::write(&out, patched).unwrap();
    (out, cleared)
}

fn ours(path: &PathBuf, dither: bool) -> (Vec<f32>, u16) {
    let data = std::fs::read(path).unwrap();
    let i = info(&data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    if !dither {
        d = d.without_dither();
    }
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for f in frames(&data) {
        d.decode_into(f, &mut buf).unwrap();
        out.extend_from_slice(&buf);
    }
    (out, d.channels())
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn with_dither_flags(path: &PathBuf) -> PathBuf {
    let data = std::fs::read(path).unwrap();
    let i = info(&data).unwrap();
    let mut d = Decoder::new(i.rate, i.channels);
    let nfchans = header::parse(&data).unwrap().nfchans;
    let mut out = Vec::new();
    let mut buf = Vec::new();
    for f in frames(&data) {
        d.decode_into(f, &mut buf).unwrap();
        let holes = d.holes();
        let (flag, rest) = holes.split_first().unwrap();
        let (end, blocks) = rest.split_last().unwrap();
        let total = f.len() * 8;
        assert!(
            end + blocks.len() * nfchans + 16 <= total,
            "no room for dither flags"
        );
        let bit = |i: usize| f[i / 8] >> (7 - i % 8) & 1;
        let mut bits: Vec<u8> = Vec::with_capacity(total);
        let mut next = blocks.iter().peekable();
        for i in 0..total {
            while next.peek() == Some(&&i) {
                bits.extend(std::iter::repeat_n(0, nfchans));
                next.next();
            }
            bits.push(if i == *flag { 1 } else { bit(i) });
        }
        bits.truncate(total);
        let mut frame: Vec<u8> = bits
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| a << 1 | b))
            .collect();
        let n = frame.len();
        let crc = crc16(&frame[2..n - 2]);
        frame[n - 2..].copy_from_slice(&crc.to_be_bytes());
        out.extend(frame);
    }
    let p = path.with_extension(format!(
        "flags.{}",
        path.extension().unwrap().to_string_lossy()
    ));
    std::fs::write(&p, out).unwrap();
    p
}

#[test]
fn ac3_matches_ffmpeg() {
    if !ffmpeg() {
        return;
    }
    for (name, rate, layout, channels, kbps) in [
        ("ac3-51", 48_000, "5.1(side)", 6, 448),
        ("ac3-stereo", 44_100, "stereo", 2, 96),
        ("ac3-mono", 48_000, "mono", 1, 64),
    ] {
        let src = encode(name, "ac3", rate, layout, channels, kbps);
        let (plain, cleared) = without_dither_flags(&src);
        assert!(cleared > 0, "{name}: no dither flags to clear");
        let reference = ffmpeg_decode(&plain);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, channels);
        let d = max_diff(&got, &reference);
        assert!(d < 5e-5, "{name}: max difference {d}");
    }
}

#[test]
fn eac3_matches_ffmpeg() {
    if !ffmpeg() {
        return;
    }
    for (name, layout, channels, kbps) in [
        ("eac3-51", "5.1(side)", 6, 384),
        ("eac3-stereo", "stereo", 2, 128),
    ] {
        let src = encode(name, "eac3", 48_000, layout, channels, kbps);
        let plain = with_dither_flags(&src);
        let reference = ffmpeg_decode(&plain);
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, channels);
        let d = max_diff(&got, &reference);
        assert!(d < 5e-5, "{name}: max difference {d}");
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn malformed_frames_fail_cleanly() {
    if !ffmpeg() {
        return;
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for (name, codec, layout, channels) in [
        ("bad-ac3", "ac3", "5.1(side)", 6),
        ("bad-eac3", "eac3", "stereo", 2),
    ] {
        let data = std::fs::read(encode(name, codec, 48_000, layout, channels, 256)).unwrap();
        let good: Vec<Vec<u8>> = frames(&data).into_iter().map(<[u8]>::to_vec).collect();
        let mut buf = Vec::new();
        for round in 0..400 {
            let mut d = Decoder::new(48_000, channels);
            for f in good.iter().take(3) {
                let mut p = f.clone();
                match rng.below(4) {
                    0 => p.truncate(rng.below(p.len() + 1)),
                    1 => {
                        for _ in 0..1 + rng.below(8) {
                            let i = rng.below(p.len());
                            p[i] ^= 1 << rng.below(8);
                        }
                    }
                    2 => {
                        for b in p.iter_mut().skip(2 + rng.below(16)) {
                            *b = rng.next() as u8;
                        }
                    }
                    _ => p.extend((0..rng.below(64)).map(|_| rng.next() as u8)),
                }
                if round % 3 == 0 {
                    p.extend_from_slice(f);
                }
                if d.decode_into(&p, &mut buf).is_ok() {
                    assert_eq!(buf.len() % channels as usize, 0);
                }
            }
        }
        assert!(
            Decoder::new(48_000, channels)
                .decode_into(&[], &mut buf)
                .is_err()
        );
    }
}

#[cfg(all(feature = "mkv", feature = "mp4"))]
#[test]
fn containers_decode_through_the_media_front() {
    use crate::media::{Prefs, audio_pcm_range, probe};
    if !ffmpeg() {
        return;
    }
    let aac = encode("side-aac", "aac", 48_000, "stereo", 2, 128);
    let video = dir().join("video.mkv");
    let st = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=10:duration=2",
        ])
        .args(["-c:v", "libx264", "-preset", "ultrafast"])
        .arg(&video)
        .status()
        .unwrap();
    assert!(st.success());
    for (name, codec, rate, layout, channels, kbps) in [
        ("box-ac3", "ac3", 48_000, "5.1(side)", 6u16, 448),
        ("box-eac3", "eac3", 48_000, "5.1(side)", 6, 384),
        ("box-ac3-44", "ac3", 44_100, "stereo", 2, 192),
    ] {
        let src = encode(name, codec, rate, layout, channels, kbps);
        let plain = if codec == "ac3" {
            without_dither_flags(&src).0
        } else {
            with_dither_flags(&src)
        };
        let reference = ffmpeg_decode(&plain);
        for ext in ["mkv", "mp4"] {
            let path = dir().join(format!("{name}.{ext}"));
            let st = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-i"])
                .arg(&plain)
                .arg("-i")
                .arg(&aac)
                .arg("-i")
                .arg(&video)
                .args(["-map", "2:v", "-map", "0:a", "-map", "1:a", "-c", "copy"])
                .arg(&path)
                .status()
                .unwrap();
            assert!(st.success());
            let p = probe(&path, &Prefs::default()).unwrap();
            assert_eq!(p.audio.len(), 2, "{name}.{ext}");
            assert!(
                p.audio.iter().all(|a| a.native()),
                "{name}.{ext}: {:?}",
                p.audio
            );
            assert_eq!(p.audio[0].channels, channels, "{name}.{ext}");
            let got = audio_pcm_range(
                &path,
                0,
                0.0,
                f64::INFINITY,
                rate,
                channels,
                &Prefs::default(),
            )
            .unwrap();
            assert_eq!(got.channels, channels);
            let n = got.samples.len().min(reference.len());
            assert!(
                n + 1536 * channels as usize >= reference.len(),
                "{name}.{ext}: {n} of {}",
                reference.len()
            );
            let d = max_diff(&got.samples[..n], &reference[..n]);
            assert!(d < 5e-5, "{name}.{ext}: max difference {d}");
            let stereo = audio_pcm_range(&path, 0, 0.5, 1.0, 48_000, 2, &Prefs::default()).unwrap();
            assert_eq!(stereo.samples.len(), 48_000);
        }
    }
}
