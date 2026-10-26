use super::{Decoder, Picture, is_keyframe, keyframe_info};
use crate::media::{Frame, Packet, PixelFormat};
use crate::testref::{Digest, Refs};
use std::time::Instant;

#[allow(dead_code)]
#[path = "refs.rs"]
pub(super) mod refs;

use refs::{Libvpx, ivf_frames};

fn stream(r: &Refs, name: &str) -> Vec<u8> {
    r.bytes(&refs::stream(name))
}

fn packet(data: &[u8], i: usize) -> Packet {
    Packet {
        data: data.to_vec(),
        pts: i as i64,
        dts: i as i64,
        keyframe: false,
    }
}

fn decode_frames(dec: &mut Decoder, chunks: &[&[u8]]) -> Vec<Frame> {
    let mut out = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        out.extend(
            dec.decode(&packet(c, i))
                .unwrap_or_else(|e| panic!("chunk {i}: {e}")),
        );
    }
    out.extend(dec.flush().unwrap());
    out
}

fn decode_count(dec: &mut Decoder, chunks: &[&[u8]]) -> usize {
    let mut n = 0;
    for (i, c) in chunks.iter().enumerate() {
        n += dec.decode(&packet(c, i)).unwrap().len();
    }
    n + dec.flush().unwrap().len()
}

fn decode_pictures(dec: &mut Decoder, chunks: &[&[u8]]) -> Vec<Picture> {
    let mut out = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        out.extend(
            dec.decode_pictures(c, i as i64)
                .unwrap_or_else(|e| panic!("chunk {i}: {e}")),
        );
    }
    out.extend(dec.flush_pictures());
    out
}

fn is_420(f: &Libvpx) -> bool {
    f.csp.starts_with("420")
}

fn check(r: &Refs, f: &Libvpx, threads: usize, data: &[u8]) {
    let chunks = ivf_frames(data);
    let name = format!("{} with {threads} thread(s)", f.name);
    let mut dec = Decoder::with_threads(threads);
    let mut d = Digest::new();
    let count = if is_420(f) {
        let frames = decode_frames(&mut dec, &chunks);
        let format = if refs::depth(f.csp) > 8 {
            PixelFormat::Yuv420p10
        } else {
            PixelFormat::Yuv420p8
        };
        for fr in &frames {
            assert_eq!(
                (fr.format, fr.width, fr.height),
                (format, f.size.0, f.size.1),
                "{name}"
            );
            d.frame(fr);
        }
        frames.len()
    } else {
        let pictures = decode_pictures(&mut dec, &chunks);
        for p in &pictures {
            let bytes: Vec<u8> = [&p.y, &p.u, &p.v]
                .iter()
                .flat_map(|p| p.to_le_bytes())
                .collect();
            assert_eq!(bytes.len(), refs::frame_bytes(f), "{name}: frame size");
            d.bytes(&bytes);
        }
        pictures.len()
    };
    assert_eq!(count as u64, f.frames, "{name}: frame count");
    r.assert_digest(&refs::decoded(f.name), &d.hex());
}

#[test]
fn bit_exact_against_vpxdec() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let r = Refs::open(refs::AREA);
    for f in &refs::CONFORMANCE {
        let data = stream(&r, f.name);
        for threads in [1, 3, cores] {
            check(&r, f, threads, &data);
        }
    }
}

#[test]
fn keyframes_are_found_and_described() {
    let r = Refs::open(refs::AREA);
    let f = refs::find("altref_10bit").unwrap();
    let data = stream(&r, f.name);
    let chunks = ivf_frames(&data);
    let info = keyframe_info(chunks[0]).unwrap();
    assert_eq!(
        (info.profile, info.width, info.height, info.bit_depth),
        (2, f.size.0, f.size.1, 10)
    );
    assert!(chunks[1..].iter().all(|c| !is_keyframe(c)));
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn damaged_streams_fail_without_panics() {
    let r = Refs::open(refs::AREA);
    for name in ["good", "altref_10bit", "tiles_512x240"] {
        let data = stream(&r, name);
        let mut seed = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        for round in 0..8 {
            let mut chunks: Vec<Vec<u8>> =
                ivf_frames(&data).into_iter().map(<[u8]>::to_vec).collect();
            for _ in 0..(1 + round * 3) {
                let c = (xorshift(&mut seed) as usize) % chunks.len();
                let len = chunks[c].len();
                let pos = (xorshift(&mut seed) as usize) % len;
                chunks[c][pos] ^= 1 << (xorshift(&mut seed) % 8);
                if round % 4 == 3 {
                    chunks[c].truncate(len * 2 / 3);
                }
            }
            for threads in [1, 4] {
                let mut dec = Decoder::with_threads(threads);
                for (i, c) in chunks.iter().enumerate() {
                    let _ = dec.decode_pictures(c, i as i64);
                }
                let _ = dec.flush_pictures();
            }
        }
    }
    let mut seed = 42u64;
    for len in [0usize, 1, 5, 100, 5000] {
        for _ in 0..20 {
            let g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
            let mut dec = Decoder::new();
            let _ = dec.decode_pictures(&g, 0);
            let _ = dec.flush_pictures();
            let _ = keyframe_info(&g);
        }
    }
}

fn cpu_secs() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, r)| r.split_whitespace().collect())
        .unwrap_or_default();
    let tick = |i: usize| {
        fields
            .get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    (tick(11) + tick(12)) / 100.0
}

#[test]
#[ignore]
fn decode_speed() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    let threads: Vec<usize> = match std::env::var("VP9_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        Some(t) => vec![t],
        None => vec![1, cores],
    };
    let r = Refs::open(refs::AREA);
    let streams: Vec<(String, Vec<u8>)> = match std::env::var("VP9_FILE") {
        Ok(file) => vec![(file.clone(), std::fs::read(&file).unwrap())],
        Err(_) => refs::CONFORMANCE
            .iter()
            .map(|f| (f.name.to_string(), stream(&r, f.name)))
            .collect(),
    };
    for (name, data) in &streams {
        let chunks = ivf_frames(data);
        for &threads in &threads {
            let mut ours = (f64::MAX, f64::MAX);
            let mut frames = 0;
            for _ in 0..3 {
                let (c0, t0) = (cpu_secs(), Instant::now());
                frames = decode_count(&mut Decoder::with_threads(threads), &chunks);
                ours = (
                    ours.0.min(t0.elapsed().as_secs_f64()),
                    ours.1.min(cpu_secs() - c0),
                );
            }
            println!(
                "{name} {threads} thread(s): {:.1} fps wall, {:.2} cpu ms/frame",
                frames as f64 / ours.0,
                1000.0 * ours.1 / frames as f64
            );
        }
    }
}

#[cfg(all(feature = "gpu", target_os = "linux"))]
fn on_the_gpu_floor(f: &Libvpx) -> bool {
    is_420(f) && f.size.0 >= refs::GPU_FLOOR.0 && f.size.1 >= refs::GPU_FLOOR.1
}

#[cfg(all(feature = "gpu", target_os = "linux"))]
#[test]
fn vaapi_matches_vpxdec() {
    use crate::gpu::{Backend, Direction, HwDecoder, available};
    use crate::media::Codec;
    if !available().contains(&(Codec::Vp9, Direction::Decode, Backend::VaApi)) {
        eprintln!("SKIPPED: no VA-API VP9 decoder");
        return;
    }
    let r = Refs::open(refs::AREA);
    for f in refs::CONFORMANCE.iter().filter(|f| on_the_gpu_floor(f)) {
        let data = stream(&r, f.name);
        let chunks = ivf_frames(&data);
        let mut dec = HwDecoder::new(Codec::Vp9, &[]).unwrap();
        assert_eq!(dec.backend(), Backend::VaApi);
        let start = Instant::now();
        let mut frames = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            frames.extend(
                dec.decode(&packet(c, i))
                    .unwrap_or_else(|e| panic!("{}: chunk {i}: {e}", f.name)),
            );
        }
        frames.extend(dec.flush().unwrap());
        let secs = start.elapsed().as_secs_f64();
        assert_eq!(frames.len() as u64, f.frames, "{}: frame count", f.name);
        let mut d = Digest::new();
        for fr in &frames {
            d.frame(fr);
        }
        r.assert_digest(&refs::decoded(f.name), &d.hex());
        eprintln!(
            "{} on VA-API: {} frames bit-exact, {:.0} fps with read-back",
            f.name,
            frames.len(),
            frames.len() as f64 / secs
        );
    }
}

#[cfg(all(feature = "gpu", target_os = "linux"))]
#[test]
fn vaapi_decodes_while_the_caller_holds_frames_on_the_gpu() {
    use crate::gpu::{Backend, Direction, HwDecoder, available};
    use crate::media::Codec;
    if !available().contains(&(Codec::Vp9, Direction::Decode, Backend::VaApi)) {
        eprintln!("SKIPPED: no VA-API VP9 decoder");
        return;
    }
    let r = Refs::open(refs::AREA);
    let f = refs::find("good").unwrap();
    assert!(on_the_gpu_floor(f) && f.frames > 12);
    let data = stream(&r, f.name);
    let mut dec = HwDecoder::new(Codec::Vp9, &[]).unwrap();
    let mut held = std::collections::VecDeque::new();
    let mut n = 0;
    for (i, c) in ivf_frames(&data).iter().enumerate() {
        for g in dec
            .decode_gpu(&packet(c, i))
            .unwrap_or_else(|e| panic!("{}: chunk {i}: {e}", f.name))
        {
            n += 1;
            held.push_back(g);
            if held.len() > 12 {
                held.pop_front();
            }
        }
    }
    n += dec.flush_gpu().unwrap().len();
    assert!(n > 12, "{}: {n} frames", f.name);
}

#[test]
fn the_libvpx_sources_are_what_the_recorder_feeds_libvpx() {
    for f in &refs::CONFORMANCE {
        let bytes = refs::source_bytes(f);
        assert_eq!(
            bytes.len(),
            refs::frame_bytes(f) * f.frames as usize,
            "{}",
            f.name
        );
        assert_eq!(refs::source(f.name).unwrap(), bytes, "{}", f.name);
        let bits = refs::depth(f.csp);
        if bits > 8 {
            assert!(
                bytes
                    .chunks_exact(2)
                    .all(|s| u16::from_le_bytes([s[0], s[1]]) < 1 << bits),
                "{}: a sample above {bits} bits",
                f.name
            );
        }
    }
    let size = |name: &str| refs::frame_bytes(refs::find(name).unwrap());
    assert_eq!(size("lossless"), 64 * 64 * 3 / 2);
    assert_eq!(size("profile1_444"), 176 * 144 * 3);
    assert_eq!(size("profile3_422_12bit"), 176 * 144 * 2 * 2);
    assert_eq!(size("odd_aq"), (173 * 125 + 2 * 87 * 63));
    assert!(refs::source("nothing").is_err());
}

#[test]
fn the_alpha_sources_are_rgba_frames() {
    for (name, w, h) in refs::ALPHA {
        let bytes = refs::alpha_source(name).unwrap();
        assert_eq!(
            bytes.len(),
            (w * h * 4) as usize * refs::ALPHA_FRAMES as usize
        );
        assert_eq!(bytes, refs::alpha_rgba(w, h, refs::ALPHA_FRAMES));
        assert!(bytes.chunks_exact(4).any(|p| p[3] == 0));
        assert!(bytes.chunks_exact(4).any(|p| p[3] == 255));
    }
    assert!(refs::alpha_source("nothing").is_err());
}

#[test]
fn colourspaces_name_their_depth_and_chroma() {
    assert_eq!(
        ["420jpeg", "420p10", "444", "422p12"].map(refs::depth),
        [8, 10, 8, 12]
    );
    assert_eq!(refs::chroma("420jpeg", (5, 3)), (3, 2, 2, 2));
    assert_eq!(refs::chroma("422p12", (5, 3)), (3, 3, 2, 1));
    assert_eq!(refs::chroma("444", (5, 3)), (5, 3, 1, 1));
    assert_eq!(
        (refs::stream("a"), refs::decoded("a")),
        ("a.ivf".into(), "a.yuv".into())
    );
    assert_eq!(
        (refs::alpha_stream("a"), refs::alpha_decoded("a")),
        ("a.alpha.ivf".into(), "a.alpha".into())
    );
    assert_eq!(refs::edge(66, 8), "edge-66x8");
}

#[test]
fn rgba_splits_into_a_colour_and_an_alpha_picture() {
    let (w, h) = (3u32, 3u32);
    let mut rgba = Vec::new();
    for i in 0..9u8 {
        rgba.extend_from_slice(&[200, 10, 10, i * 20]);
    }
    rgba.extend_from_slice(&rgba.clone());
    let (colour, alpha) = refs::split_rgba(&rgba, w, h);
    let frame = 9 + 2 * 4;
    assert_eq!((colour.len(), alpha.len()), (2 * frame, 2 * frame));
    let [y, u, v] = crate::testref::yuv_of([200, 10, 10]);
    assert!(colour[..9].iter().all(|&s| s == y));
    assert!(colour[9..13].iter().all(|&s| s == u));
    assert!(colour[13..17].iter().all(|&s| s == v));
    assert_eq!(&alpha[..9], &[0, 20, 40, 60, 80, 100, 120, 140, 160]);
    assert!(alpha[9..17].iter().all(|&s| s == 128));
    assert_eq!(colour[..frame], colour[frame..]);
}

#[test]
fn ivf_round_trips_its_frames_and_refuses_what_is_not_ivf() {
    let packets = vec![vec![1, 2, 3], vec![], vec![9; 40]];
    let file = refs::ivf(64, 48, &packets);
    let back: Vec<Vec<u8>> = ivf_frames(&file).into_iter().map(<[u8]>::to_vec).collect();
    assert_eq!(back, packets);
    assert!(ivf_frames(b"RIFF").is_empty());
    assert_eq!(ivf_frames(&file[..file.len() - 1]).len(), 2);
}

#[test]
fn the_off_grid_encodes_are_the_moving_picture_at_one_thread() {
    for (w, h) in [(8, 8), (66, 66)] {
        let file = refs::edge_ivf(w, h).unwrap();
        let frames = ivf_frames(&file);
        assert_eq!(frames.len() as u32, refs::EDGE_FRAMES, "{w}x{h}");
        assert!(is_keyframe(frames[0]) && !is_keyframe(frames[1]), "{w}x{h}");
        assert_eq!(refs::edge_config(w, h, 1).threads, 1);
    }
    assert!(refs::edge_ivf(0, 8).is_err());
}
