use super::Decoder;
use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg_has(what: &[&str]) -> bool {
    let list = |kind: &str| {
        Command::new("ffmpeg")
            .args(["-hide_banner", kind])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let all = list("-encoders") + &list("-decoders");
    what.iter().all(|w| all.contains(w))
}

fn ff(args: &[&str]) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

fn ivf_packets(data: &[u8]) -> Vec<(Vec<u8>, i64)> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else {
        return out;
    };
    let mut pos = u16::from_le_bytes([h[0], h[1]]) as usize;
    while let Some(head) = data.get(pos..pos + 12) {
        let n = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let mut pts = [0u8; 8];
        pts.copy_from_slice(&head[4..12]);
        let Some(p) = data.get(pos + 12..pos + 12 + n) else {
            break;
        };
        out.push((p.to_vec(), i64::from_le_bytes(pts)));
        pos += 12 + n;
    }
    out
}

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fixtures/av1");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn bit_exact_against_libdav1d() {
    if !ffmpeg_has(&["libaom-av1", "libdav1d"]) {
        eprintln!("SKIPPED: ffmpeg with libaom-av1 and libdav1d not found");
        return;
    }
    let d = dir();
    let ivf = d.join(format!("grain_tiles_10bit.{}.ivf", std::process::id()));
    let yuv = d.join(format!("grain_tiles_10bit.{}.yuv", std::process::id()));
    let (ivf_s, yuv_s) = (ivf.to_str().unwrap(), yuv.to_str().unwrap());
    assert!(ff(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=352x240:rate=30,noise=alls=8:allf=t",
        "-frames:v",
        "12",
        "-pix_fmt",
        "yuv420p10le",
        "-c:v",
        "libaom-av1",
        "-cpu-used",
        "8",
        "-tiles",
        "2x1",
        "-denoise-noise-level",
        "20",
        "-auto-alt-ref",
        "1",
        "-lag-in-frames",
        "8",
        "-crf",
        "32",
        ivf_s,
    ]));
    assert!(ff(&[
        "-c:v",
        "libdav1d",
        "-i",
        ivf_s,
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        yuv_s,
    ]));
    let reference = std::fs::read(&yuv).unwrap();
    let data = std::fs::read(&ivf).unwrap();
    let _ = std::fs::remove_file(&ivf);
    let _ = std::fs::remove_file(&yuv);
    for threads in [1, 4] {
        let mut dec = Decoder::with_threads(threads);
        let mut pics = Vec::new();
        for (p, pts) in ivf_packets(&data) {
            pics.extend(dec.decode_pictures(&p, pts).unwrap());
        }
        pics.extend(dec.flush_pictures().unwrap());
        assert_eq!(pics.len(), 12, "threads {threads}");
        let ours: Vec<u8> = pics
            .iter()
            .flat_map(|p| p.planes.iter().flat_map(|pl| pl.data.iter().copied()))
            .collect();
        assert_eq!(ours.len(), reference.len(), "threads {threads}");
        let frame = reference.len() / 12;
        for (i, (a, b)) in ours.chunks(frame).zip(reference.chunks(frame)).enumerate() {
            assert!(a == b, "threads {threads}: frame {i} differs from libdav1d");
        }
    }
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    let mut dec = Decoder::with_threads(2);
    let mut seed = 0x1234_5678u32;
    for _ in 0..64 {
        let buf: Vec<u8> = (0..256)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let _ = dec.decode_pictures(&buf, 0);
    }
    let _ = dec.flush_pictures();
}
