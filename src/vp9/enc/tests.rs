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
    crate::fixture::once(&path, |tmp| {
        let src = format!("testsrc2=size={w}x{h}:rate=25,noise=alls=6:allf=t");
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &src, "-frames:v"])
            .arg(frames.to_string())
            .args(["-pix_fmt", "yuv420p", "-f", "rawvideo"])
            .arg(tmp)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    });
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
fn odd_sizes_encode_without_failing() {
    for (w, h) in [(1, 1), (2, 3), (9, 7), (65, 33), (130, 66), (200, 8)] {
        for threads in [1, 4] {
            let mut enc = Encoder::new(EncoderConfig {
                width: w,
                height: h,
                rate: Rate::Crf(30),
                gop: 3,
                threads,
                ..EncoderConfig::default()
            })
            .unwrap();
            for i in 0..5u32 {
                let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 25));
                for p in &mut f.planes {
                    for (k, v) in p.data.iter_mut().enumerate() {
                        *v = ((k as u32 * 7 + i * 13) ^ (k as u32 >> 3)) as u8;
                    }
                }
                let out = enc.encode(&f).unwrap();
                assert!(!out.is_empty() && !out[0].data.is_empty(), "{w}x{h}");
            }
        }
    }
    let mut enc = Encoder::new(EncoderConfig {
        width: 64,
        height: 64,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut f = Frame::new(PixelFormat::Yuv420p8, 64, 64, 0, TimeBase::new(1, 25));
    f.planes[1].data.truncate(10);
    assert!(enc.encode(&f).is_err());
}

#[test]
#[ignore]
fn bench_encoder() {
    if !ffmpeg_present() {
        return;
    }
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
        rate: std::env::var("VP9_Q")
            .ok()
            .and_then(|v| v.parse().ok())
            .map_or(Rate::Crf(crf), Rate::Q),
        gop: std::env::var("VP9_GOP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(240),
        threads,
        aq: std::env::var("VP9_AQ").is_ok_and(|v| v != "0"),
    })
    .unwrap();
    let t = std::time::Instant::now();
    let mut packets = Vec::new();
    let mut recon = Vec::new();
    for f in &frames {
        for p in enc.encode(f).unwrap() {
            packets.push(p.data);
        }
        if std::env::var("VP9_RECON").is_ok() {
            recon.extend(crop(enc.recon().unwrap()));
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let bytes: usize = packets.iter().map(|p| p.len()).sum();
    let out = root.join("tmp/bench/ours.ivf");
    std::fs::write(&out, ivf(w, h, &packets)).unwrap();
    let dec = ffmpeg_decode(&out, false);
    let p = psnr(&data[..dec.len()], &dec);
    if !recon.is_empty() {
        let first = recon.iter().zip(&dec).position(|(a, b)| a != b);
        eprintln!(
            "recon against ffmpeg: first difference at {first:?} of {}, PSNR {:.2} dB",
            dec.len(),
            psnr(&recon, &dec)
        );
    }
    let kbps = bytes as f64 * 8.0 / (frames.len() as f64 / 30.0) / 1000.0;
    eprintln!(
        "vp9enc crf {crf}: {} frames {:.1} fps, {kbps:.0} kbps, PSNR {p:.2} dB",
        frames.len(),
        frames.len() as f64 / secs
    );
}

fn moving(w: u32, h: u32, i: u32) -> Frame {
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 25));
    let (dx, dy) = (3 * i as i64, 2 * i as i64);
    for (p, plane) in f.planes.iter_mut().enumerate() {
        let s = if p == 0 { 1 } else { 2 };
        for y in 0..plane.height {
            for x in 0..plane.width {
                let gx = (x as i64 * s - dx) as f64;
                let gy = (y as i64 * s - dy) as f64;
                let wave = 50.0 * ((gx / 23.0).sin() + (gy / 17.0 + gx / 41.0).cos());
                let base = 128.0 + wave + if p == 0 { 0.0 } else { 20.0 * p as f64 - 30.0 };
                plane.data[y * plane.stride + x] = base.clamp(0.0, 255.0) as u8;
            }
        }
    }
    f
}

fn edge_strips(s: &Frame, d: &Frame) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for p in 0..3 {
        let (a, b) = (&s.planes[p], &d.planes[p]);
        let n = if p == 0 { 64 } else { 32 };
        let (pw, ph) = (a.width, a.height);
        let (x0, y0) = (pw / n * n, ph / n * n);
        let take =
            |f: &crate::media::Plane, xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| {
                let mut v = Vec::new();
                for y in ys {
                    v.extend_from_slice(&f.data[y * f.stride + xs.start..y * f.stride + xs.end]);
                }
                v
            };
        if x0 < pw {
            out.push((
                format!("plane {p} right {}", pw - x0),
                take(a, x0..pw, 0..ph),
                take(b, x0..pw, 0..ph),
            ));
        }
        if y0 < ph {
            out.push((
                format!("plane {p} bottom {}", ph - y0),
                take(a, 0..pw, y0..ph),
                take(b, 0..pw, y0..ph),
            ));
        }
    }
    out
}

fn edge_encode(w: u32, h: u32, frames: u32, threads: usize) -> (Vec<Frame>, Vec<Packet>) {
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        rate: Rate::Q(60),
        gop: 30,
        threads,
        ..EncoderConfig::default()
    })
    .unwrap();
    let src: Vec<Frame> = (0..frames).map(|i| moving(w, h, i)).collect();
    let mut packets = Vec::new();
    for (i, f) in src.iter().enumerate() {
        let out = enc
            .encode(f)
            .unwrap_or_else(|e| panic!("{w}x{h} frame {i}: {e}"));
        assert_eq!(out.len(), 1, "{w}x{h} frame {i}");
        assert_eq!(out[0].keyframe, i == 0, "{w}x{h} frame {i}");
        packets.extend(out);
    }
    (src, packets)
}

#[test]
fn sizes_off_the_superblock_grid_encode_and_decode_back() {
    let sizes = [
        (1080, 1920),
        (1080, 1080),
        (200, 120),
        (66, 66),
        (8, 8),
        (1080, 200),
        (7, 7),
        (5, 3),
        (4, 4),
        (2, 9),
        (1, 1),
    ];
    for (w, h) in sizes {
        for threads in [1, 4] {
            let (src, packets) = edge_encode(w, h, 4, threads);
            let mut dec = crate::vp9::Decoder::new();
            let mut out = Vec::new();
            for p in &packets {
                out.extend(dec.decode(p).unwrap_or_else(|e| panic!("{w}x{h}: {e}")));
            }
            out.extend(dec.flush().unwrap());
            assert_eq!(out.len(), src.len(), "{w}x{h}");
            for (i, (d, s)) in out.iter().zip(&src).enumerate() {
                assert_eq!((d.width, d.height), (w, h), "{w}x{h} frame {i}");
                let p = psnr(&raw(std::slice::from_ref(s)), &raw(std::slice::from_ref(d)));
                assert!(p > 30.0, "{w}x{h} frame {i} threads {threads}: PSNR {p:.2}");
                for (edge, a, b) in edge_strips(s, d) {
                    let p = psnr(&a, &b);
                    assert!(
                        p > 30.0,
                        "{w}x{h} frame {i} threads {threads} {edge}: PSNR {p:.2}"
                    );
                }
            }
        }
    }
}

#[test]
fn ffmpeg_decodes_sizes_off_the_superblock_grid() {
    if !ffmpeg_present() {
        return;
    }
    for (w, h) in [(1080, 1920), (1080, 1080), (200, 120), (66, 66), (8, 8)] {
        let (src, packets) = edge_encode(w, h, 3, 0);
        let data: Vec<Vec<u8>> = packets.into_iter().map(|p| p.data).collect();
        let path = dir().join(format!("edge-{w}x{h}.ivf"));
        std::fs::write(&path, ivf(w, h, &data)).unwrap();
        let src = raw(&src);
        for libvpx in [false, true] {
            let got = ffmpeg_decode(&path, libvpx);
            assert_eq!(got.len(), src.len(), "{w}x{h} libvpx {libvpx}");
            let p = psnr(&src, &got);
            assert!(p > 30.0, "{w}x{h} libvpx {libvpx}: PSNR {p:.2}");
        }
    }
}

#[test]
fn edge_encode_matches_our_reconstruction() {
    for (w, h) in [(1080, 200), (66, 66), (200, 120)] {
        let mut enc = Encoder::new(EncoderConfig {
            width: w,
            height: h,
            rate: Rate::Q(60),
            gop: 30,
            threads: 1,
            ..EncoderConfig::default()
        })
        .unwrap();
        let mut dec = crate::vp9::Decoder::new();
        let (mut recon, mut got) = (Vec::new(), Vec::new());
        for i in 0..3 {
            for p in enc.encode(&moving(w, h, i)).unwrap() {
                got.extend(dec.decode(&p).unwrap());
            }
            recon.extend(crop(enc.recon().unwrap()));
        }
        got.extend(dec.flush().unwrap());
        assert_eq!(got.len(), 3, "{w}x{h}");
        assert!(
            raw(&got) == recon,
            "{w}x{h}: our decoder differs from the reconstruction"
        );
    }
}

#[test]
fn a_block_that_fails_in_a_worker_fails_the_frame_and_the_encoder() {
    for (threads, fail_frame, at) in [(1, 1, (8, 8)), (4, 1, (8, 16)), (4, 0, (0, 0))] {
        let (w, h) = (200, 120);
        let mut enc = Encoder::new(EncoderConfig {
            width: w,
            height: h,
            rate: Rate::Q(60),
            gop: 30,
            threads,
            ..EncoderConfig::default()
        })
        .unwrap();
        for i in 0..fail_frame {
            enc.encode(&moving(w, h, i)).unwrap();
        }
        for ws in &mut enc.workers {
            ws.fail_at = Some(at);
        }
        let e = enc
            .encode(&moving(w, h, fail_frame))
            .expect_err("a failed block still produced a frame")
            .to_string();
        assert_eq!(
            e,
            format!(
                "backend: vp9 encoder: frame {fail_frame} failed: block {},{} failed on purpose",
                at.0, at.1
            ),
            "threads {threads}"
        );
        assert!(
            enc.workers.iter().all(|ws| ws.err.is_none()),
            "threads {threads}"
        );
        for ws in &mut enc.workers {
            ws.fail_at = None;
        }
        let again = enc
            .encode(&moving(w, h, fail_frame + 1))
            .expect_err("a broken encoder encoded")
            .to_string();
        assert!(again.contains("an earlier frame failed"), "{again}");
    }
}
