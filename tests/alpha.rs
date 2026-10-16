#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]

use std::path::{Path, PathBuf};
use std::process::Command;

use pito_codecs::media::{
    Canvas, Codec, ExportOptions, Prefs, Rgba, decode_alpha, export_rgba, first_frame, probe,
};

const W: u32 = 192;
const H: u32 = 108;
const FRAMES: u64 = 24;

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn libvpx() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libvpx-vp9"))
}

fn source(n: u64) -> Rgba {
    let mut img = Rgba::new(W, H);
    for y in 0..H as usize {
        for x in 0..W as usize {
            let o = (y * W as usize + x) * 4;
            let cx = 40 + n as i64 * 4;
            let d = ((x as i64 - cx).pow(2) + (y as i64 - 54).pow(2)) as f64;
            img.data[o] = (x * 255 / W as usize) as u8;
            img.data[o + 1] = (y * 255 / H as usize) as u8;
            img.data[o + 2] = ((x + y + n as usize * 8) % 256) as u8;
            img.data[o + 3] = (255.0 - d.sqrt() * 4.0).clamp(0.0, 255.0) as u8;
        }
    }
    img
}

fn ours(path: &Path) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut alpha = 0;
    for got in decode_alpha(path, 0.0).unwrap() {
        let (f, a) = got.unwrap();
        for p in &f.planes {
            for y in 0..p.height {
                out.extend_from_slice(&p.data[y * p.stride..y * p.stride + p.width]);
            }
        }
        let a = a.expect("every frame carries alpha");
        alpha += 1;
        for y in 0..a.height {
            out.extend_from_slice(&a.data[y * a.stride..y * a.stride + a.width]);
        }
    }
    (out, alpha)
}

fn libvpx_yuva(path: &Path) -> Vec<u8> {
    let o = Command::new("ffmpeg")
        .args(["-v", "error", "-c:v", "libvpx-vp9", "-i"])
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuva420p", "-"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

#[test]
fn alpha_webm_round_trips_and_matches_libvpx_both_ways() {
    let out = dir().join("alpha-round-trip.webm");
    let opts = ExportOptions {
        video: Codec::Vp9,
        ..ExportOptions::default()
    };
    let canvas = Canvas {
        width: W,
        height: H,
        fps: (24, 1),
        frames: Some(FRAMES),
    };
    let done = export_rgba(
        &out,
        canvas,
        true,
        opts,
        |n, _| Ok(Some(source(n))),
        |_| true,
    )
    .unwrap();
    assert_eq!(done.frames, FRAMES);
    assert!(probe(&out, &Prefs::default()).unwrap().alpha);
    let (yuva, alpha) = ours(&out);
    assert_eq!(alpha as u64, FRAMES);
    let size = (W * H) as usize;
    let frame = size * 5 / 2;
    let mut worst = f64::MAX;
    for n in 0..FRAMES {
        let at = n as usize * frame + size * 3 / 2;
        let src = source(n);
        let mse = yuva[at..at + size]
            .iter()
            .zip(src.data.chunks_exact(4))
            .map(|(&a, px)| (a as f64 - px[3] as f64).powi(2))
            .sum::<f64>()
            / size as f64;
        worst = worst.min(10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10());
    }
    assert!(worst > 35.0, "alpha PSNR {worst:.2} dB");
    let still = first_frame(&out, 0, &Prefs::default()).unwrap();
    assert!(still.data.chunks_exact(4).any(|p| p[3] < 16));
    assert!(still.data.chunks_exact(4).any(|p| p[3] > 240));
    if !libvpx() {
        return;
    }
    assert!(
        libvpx_yuva(&out) == yuva,
        "libvpx reads our alpha WebM differently"
    );
    let theirs = dir().join("alpha-libvpx.webm");
    let raw = dir().join("alpha-source.rgba");
    let bytes: Vec<u8> = (0..FRAMES).flat_map(|n| source(n).data).collect();
    std::fs::write(&raw, bytes).unwrap();
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{W}x{H}"), "-r", "24", "-i"])
        .arg(&raw)
        .args(["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-crf", "30"])
        .args(["-b:v", "0", "-deadline", "good", "-cpu-used", "4"])
        .arg(&theirs)
        .status()
        .unwrap();
    assert!(made.success());
    assert!(probe(&theirs, &Prefs::default()).unwrap().alpha);
    let (got, alpha) = ours(&theirs);
    assert_eq!(alpha as u64, FRAMES);
    assert!(
        got == libvpx_yuva(&theirs),
        "a libvpx alpha WebM decodes differently"
    );
}
