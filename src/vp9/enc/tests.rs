use super::*;
use crate::media::{Frame, PixelFormat, TimeBase};
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/vp9enc");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ffmpeg_present() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("ffmpeg not found, skipping");
    }
    ok
}

fn source(name: &str, w: u32, h: u32, frames: u32) -> Vec<Frame> {
    let path = dir().join(format!("{name}-{w}x{h}-{frames}.yuv"));
    if !path.exists() {
        let tmp = dir().join(format!(
            ".{}-{name}-{w}x{h}-{frames}.yuv",
            std::process::id()
        ));
        let src = format!("testsrc2=size={w}x{h}:rate=25,noise=alls=6:allf=t");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-frames:v"])
            .arg(frames.to_string())
            .args(["-pix_fmt", "yuv420p", "-f", "rawvideo"])
            .arg(&tmp)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::rename(&tmp, &path).unwrap();
    }
    let data = std::fs::read(&path).unwrap();
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    let fsize = (w * h) as usize + 2 * cw * ch;
    data.chunks(fsize)
        .enumerate()
        .map(|(i, d)| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 25));
            let y = (w * h) as usize;
            f.planes[0].data.copy_from_slice(&d[..y]);
            f.planes[1].data.copy_from_slice(&d[y..y + cw * ch]);
            f.planes[2].data.copy_from_slice(&d[y + cw * ch..]);
            f
        })
        .collect()
}

fn ivf(w: u32, h: u32, packets: &[Vec<u8>]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(b"DKIF");
    o.extend_from_slice(&0u16.to_le_bytes());
    o.extend_from_slice(&32u16.to_le_bytes());
    o.extend_from_slice(b"VP90");
    o.extend_from_slice(&(w as u16).to_le_bytes());
    o.extend_from_slice(&(h as u16).to_le_bytes());
    o.extend_from_slice(&25u32.to_le_bytes());
    o.extend_from_slice(&1u32.to_le_bytes());
    o.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    for (i, p) in packets.iter().enumerate() {
        o.extend_from_slice(&(p.len() as u32).to_le_bytes());
        o.extend_from_slice(&(i as u64).to_le_bytes());
        o.extend_from_slice(p);
    }
    o
}

fn crop(p: &Picture) -> Vec<u8> {
    let mut out = Vec::new();
    for pl in 0..3 {
        let (w, h) = p.visible(pl);
        for y in 0..h {
            let o = p.idx(pl, 0, y);
            out.extend_from_slice(&p.planes[pl][o..o + w]);
        }
    }
    out
}

fn raw(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        for p in &f.planes {
            for r in 0..p.height {
                out.extend_from_slice(&p.data[r * p.stride..r * p.stride + p.width]);
            }
        }
    }
    out
}

fn ffmpeg_decode(path: &Path, libvpx: bool) -> Vec<u8> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error"]);
    if libvpx {
        cmd.args(["-c:v", "libvpx-vp9"]);
    }
    let out = cmd
        .arg("-i")
        .arg(path)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let d = x as f64 - y as f64;
            d * d
        })
        .sum();
    let mse = se / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
}

fn run(name: &str, w: u32, h: u32, frames: u32, cfg: EncoderConfig) -> (Vec<u8>, Vec<u8>, usize) {
    let src = source(name, w, h, frames);
    let mut enc = Encoder::new(cfg).unwrap();
    let mut packets = Vec::new();
    let mut recon = Vec::new();
    for f in &src {
        for p in enc.encode(f).unwrap() {
            packets.push(p.data);
        }
        recon.extend(crop(enc.recon().unwrap()));
    }
    let bytes = packets.iter().map(|p| p.len()).sum();
    let path = dir().join(format!("{name}.ivf"));
    std::fs::write(&path, ivf(w, h, &packets)).unwrap();
    (raw(&src), recon, bytes)
}

fn check(name: &str, w: u32, h: u32, frames: u32, cfg: EncoderConfig, min_psnr: f64) {
    let (src, recon, bytes) = run(name, w, h, frames, cfg);
    let path = dir().join(format!("{name}.ivf"));
    let native = ffmpeg_decode(&path, false);
    assert_eq!(native.len(), recon.len());
    assert!(
        native == recon,
        "ffmpeg's decode differs from our reconstruction"
    );
    let libvpx = ffmpeg_decode(&path, true);
    assert!(
        libvpx == recon,
        "libvpx's decode differs from our reconstruction"
    );
    let p = psnr(&src, &recon);
    eprintln!("{w}x{h}: {bytes} bytes, PSNR {p:.2} dB");
    assert!(p > min_psnr, "PSNR {p:.2}");
}

#[test]
fn ffmpeg_decodes_our_stream_bit_exact() {
    if !ffmpeg_present() {
        return;
    }
    let cfg = |w, h, rate, threads, aq| EncoderConfig {
        width: w,
        height: h,
        rate,
        gop: 6,
        threads,
        aq,
        ..Default::default()
    };
    check(
        "exact",
        200,
        120,
        8,
        cfg(200, 120, Rate::Q(60), 1, false),
        34.0,
    );
    check(
        "tiles",
        712,
        200,
        4,
        cfg(712, 200, Rate::Crf(30), 4, true),
        32.0,
    );
}

#[test]
#[ignore]
fn bench_encoder() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = std::env::var("VP9_BENCH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("tmp/bench/notleys-1080p-60.yuv"));
    let (w, h) = (1920u32, 1080u32);
    let data = std::fs::read(&path).unwrap();
    let fsize = (w * h * 3 / 2) as usize;
    let frames: Vec<Frame> = data
        .chunks_exact(fsize)
        .enumerate()
        .map(|(i, d)| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 30));
            let y = (w * h) as usize;
            f.planes[0].data.copy_from_slice(&d[..y]);
            f.planes[1].data.copy_from_slice(&d[y..y + y / 4]);
            f.planes[2].data.copy_from_slice(&d[y + y / 4..]);
            f
        })
        .collect();
    let crf = std::env::var("VP9_CRF")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let threads = std::env::var("VP9_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        fps: (30, 1),
        rate: Rate::Crf(crf),
        gop: 240,
        threads,
        aq: std::env::var("VP9_AQ").map_or(true, |v| v != "0"),
    })
    .unwrap();
    let t = std::time::Instant::now();
    let mut packets = Vec::new();
    for f in &frames {
        for p in enc.encode(f).unwrap() {
            packets.push(p.data);
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let bytes: usize = packets.iter().map(|p| p.len()).sum();
    let out = root.join("tmp/bench/ours.ivf");
    std::fs::write(&out, ivf(w, h, &packets)).unwrap();
    let dec = ffmpeg_decode(&out, false);
    let p = psnr(&data[..dec.len()], &dec);
    let kbps = bytes as f64 * 8.0 / (frames.len() as f64 / 30.0) / 1000.0;
    eprintln!(
        "vp9enc crf {crf}: {} frames {:.1} fps, {kbps:.0} kbps, PSNR {p:.2} dB",
        frames.len(),
        frames.len() as f64 / secs
    );
}
