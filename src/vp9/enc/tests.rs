use super::*;
use crate::media::{Frame, PixelFormat, TimeBase};
use crate::testref::{Refs, sha256};

use crate::vp9::tests::refs;

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

#[test]
fn vpxdec_decodes_our_stream_bit_exact() {
    let r = Refs::open(refs::AREA);
    for o in refs::ours() {
        let mut recon = Vec::new();
        let ivf =
            refs::encode_ours_with(&o, |enc| recon.extend(crop(enc.recon().unwrap()))).unwrap();
        let name = o.name;
        r.assert_digest(&refs::stream(name), &sha256(&ivf));
        assert!(
            refs::encode_ours(name).unwrap() == ivf,
            "{name}: encode_ours is not this encode"
        );
        let ours = sha256(&recon);
        r.assert_digest(&refs::decoded(name), &ours);
        let p = psnr(&raw(&refs::ours_source(&o)), &recon);
        eprintln!("{name}: {} bytes, PSNR {p:.2} dB", ivf.len());
        assert!(p > o.min_psnr, "{name}: PSNR {p:.2}");
    }
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
    let (w, h) = (1920u32, 1080u32);
    let fsize = (w * h * 3 / 2) as usize;
    let frames: Vec<Frame> = match std::env::var("VP9_BENCH") {
        Ok(path) => std::fs::read(&path)
            .unwrap()
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
            .collect(),
        Err(_) => (0..60)
            .map(|n| crate::testref::yuv_frame(w, h, n, 3))
            .collect(),
    };
    let data = raw(&frames);
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
        packets.extend(enc.encode(f).unwrap());
        if std::env::var("VP9_RECON").is_ok() {
            recon.extend(crop(enc.recon().unwrap()));
        }
    }
    let secs = t.elapsed().as_secs_f64();
    let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
    let mut dec = crate::vp9::Decoder::new();
    let mut decoded = Vec::new();
    for p in &packets {
        decoded.extend(dec.decode(p).unwrap());
    }
    decoded.extend(dec.flush().unwrap());
    let dec = raw(&decoded);
    let p = psnr(&data[..dec.len()], &dec);
    if !recon.is_empty() {
        let first = recon.iter().zip(&dec).position(|(a, b)| a != b);
        eprintln!(
            "recon against our decoder: first difference at {first:?} of {}, PSNR {:.2} dB",
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
    let mut enc = Encoder::new(refs::edge_config(w, h, threads)).unwrap();
    let src: Vec<Frame> = (0..frames).map(|i| refs::moving(w, h, i)).collect();
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
fn edge_encode_matches_our_reconstruction() {
    for (w, h) in [(1080, 200), (66, 66), (200, 120)] {
        let mut enc = Encoder::new(refs::edge_config(w, h, 1)).unwrap();
        let mut dec = crate::vp9::Decoder::new();
        let (mut recon, mut got) = (Vec::new(), Vec::new());
        for i in 0..3 {
            for p in enc.encode(&refs::moving(w, h, i)).unwrap() {
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
fn vpxdec_decodes_sizes_off_the_superblock_grid_like_ours() {
    let r = Refs::open(refs::AREA);
    for (w, h) in refs::EDGES {
        let name = refs::edge(w, h);
        let (src, packets) = edge_encode(w, h, refs::EDGE_FRAMES, 1);
        let data: Vec<Vec<u8>> = packets.iter().map(|p| p.data.clone()).collect();
        let file = refs::ivf(w, h, &data);
        assert!(
            refs::edge_ivf(w, h).unwrap() == file,
            "{name}: edge_ivf is not this encode"
        );
        r.assert_digest(&refs::stream(&name), &sha256(&file));
        let mut dec = crate::vp9::Decoder::new();
        let mut out = Vec::new();
        for p in &packets {
            out.extend(dec.decode(p).unwrap_or_else(|e| panic!("{name}: {e}")));
        }
        out.extend(dec.flush().unwrap());
        assert_eq!(out.len(), src.len(), "{name}");
        r.assert_digest(&refs::decoded(&name), &sha256(&raw(&out)));
        let p = psnr(&raw(&src), &raw(&out));
        assert!(p > 30.0, "{name}: PSNR {p:.2}");
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
            enc.encode(&refs::moving(w, h, i)).unwrap();
        }
        for ws in &mut enc.workers {
            ws.fail_at = Some(at);
        }
        let e = enc
            .encode(&refs::moving(w, h, fail_frame))
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
            .encode(&refs::moving(w, h, fail_frame + 1))
            .expect_err("a broken encoder encoded")
            .to_string();
        assert!(again.contains("an earlier frame failed"), "{again}");
    }
}

#[test]
fn an_unknown_encoder_setting_is_refused() {
    assert!(refs::encode_ours("nothing").is_err());
}
