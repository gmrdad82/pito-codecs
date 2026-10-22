use super::tests::{dir, ffmpeg, max_diff, ours, signal, with_dither_flags, without_dither_flags};
use super::{info, layout};
use std::path::PathBuf;
use std::process::Command;

struct Case {
    name: &'static str,
    codec: &'static str,
    rate: u32,
    layout: &'static str,
    channels: u16,
    kbps: u32,
    extra: &'static [&'static str],
}

const fn case(
    name: &'static str,
    codec: &'static str,
    rate: u32,
    layout: &'static str,
    channels: u16,
    kbps: u32,
    extra: &'static [&'static str],
) -> Case {
    Case {
        name,
        codec,
        rate,
        layout,
        channels,
        kbps,
        extra,
    }
}

const CASES: [Case; 16] = [
    case("edge-ac3-mono-32k", "ac3", 32_000, "mono", 1, 48, &[]),
    case(
        "edge-ac3-stereo-32k",
        "ac3",
        32_000,
        "stereo",
        2,
        64,
        &["-stereo_rematrixing", "0"],
    ),
    case("edge-ac3-21", "ac3", 48_000, "2.1", 3, 192, &[]),
    case("edge-ac3-30", "ac3", 44_100, "3.0", 3, 192, &[]),
    case("edge-ac3-30back", "ac3", 48_000, "3.0(back)", 3, 192, &[]),
    case("edge-ac3-40", "ac3", 32_000, "4.0", 4, 256, &[]),
    case("edge-ac3-quad", "ac3", 48_000, "quad(side)", 4, 256, &[]),
    case("edge-ac3-41", "ac3", 44_100, "4.1", 5, 320, &[]),
    case("edge-ac3-50", "ac3", 48_000, "5.0(side)", 5, 320, &[]),
    case(
        "edge-ac3-51-cpl",
        "ac3",
        48_000,
        "5.1(side)",
        6,
        192,
        &["-channel_coupling", "1", "-cpl_start_band", "3"],
    ),
    case("edge-eac3-mono-44k", "eac3", 44_100, "mono", 1, 64, &[]),
    case("edge-eac3-stereo-32k", "eac3", 32_000, "stereo", 2, 96, &[]),
    case("edge-eac3-30", "eac3", 48_000, "3.0", 3, 192, &[]),
    case("edge-eac3-quad", "eac3", 48_000, "quad(side)", 4, 192, &[]),
    case("edge-eac3-41", "eac3", 44_100, "4.1", 5, 256, &[]),
    case("edge-eac3-51-low", "eac3", 48_000, "5.1(side)", 6, 96, &[]),
];

fn encode(c: &Case) -> PathBuf {
    let raw = dir().join(format!("{}.f32", c.name));
    std::fs::write(&raw, signal(c.rate, c.channels, 1.0)).unwrap();
    let out = dir().join(format!("{}.{}", c.name, c.codec));
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "f32le", "-ar"])
        .arg(c.rate.to_string())
        .args(["-ch_layout", c.layout, "-i"])
        .arg(&raw)
        .args(["-c:a", c.codec, "-b:a"])
        .arg(format!("{}k", c.kbps))
        .args(c.extra)
        .arg(&out)
        .status()
        .unwrap();
    assert!(st.success(), "{}: ffmpeg could not encode", c.name);
    out
}

fn ffmpeg_decode_as(path: &PathBuf, channels: u16) -> Vec<f32> {
    let target = match channels {
        1 => "mono",
        2 => "stereo",
        _ => "5.1(side)",
    };
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-ch_layout", target, "-f", "f32le", "-"])
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

#[test]
fn every_layout_and_rate_matches_ffmpeg() {
    if !ffmpeg() {
        return;
    }
    let mut failed = Vec::new();
    for c in &CASES {
        let src = encode(c);
        let data = std::fs::read(&src).unwrap();
        let i = info(&data).unwrap();
        assert_eq!(
            (i.rate, i.channels, i.eac3),
            (c.rate, c.channels, c.codec == "eac3"),
            "{}",
            c.name
        );
        let plain = if c.codec == "ac3" {
            without_dither_flags(&src).0
        } else {
            with_dither_flags(&src)
        };
        let (got, ch) = ours(&plain, false);
        assert_eq!(ch, layout(c.channels), "{}", c.name);
        let n = ch as usize;
        let silent = (0..n)
            .filter(|&k| got.iter().skip(k).step_by(n).all(|&v| v == 0.0))
            .count();
        let spread = matches!(c.layout, "3.0(back)" | "4.0" | "4.1") as usize;
        assert!(
            silent >= n - c.channels as usize - spread,
            "{}: {silent} silent slots",
            c.name
        );
        let reference = ffmpeg_decode_as(&plain, ch);
        if got.len() != reference.len() {
            failed.push(format!(
                "{}: {} samples, ffmpeg {}",
                c.name,
                got.len(),
                reference.len()
            ));
            continue;
        }
        let d = max_diff(&got, &reference);
        if d >= 5e-5 {
            failed.push(format!("{}: max difference {d}", c.name));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
