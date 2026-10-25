use std::path::PathBuf;
use std::time::Instant;

use super::nal;
use super::{Direction, EncoderConfig, HwDecoder, HwEncoder, Profile, available};
use crate::media::{Bitstream, Rate};
use crate::media::{Codec, Frame, Packet, PixelFormat, TimeBase};
use crate::testref::{Digest, Refs};

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/gpu");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn has(codec: Codec, direction: Direction) -> bool {
    let ok = available()
        .iter()
        .any(|&(c, d, _)| c == codec && d == direction);
    if !ok {
        eprintln!("skipped: no GPU {codec:?} {direction:?}");
    }
    ok
}

fn first_bit_after(codec: Codec, n: &[u8]) -> bool {
    let at = if codec == Codec::Hevc { 2 } else { 1 };
    n.get(at).is_some_and(|b| b & 0x80 != 0)
}

fn access_units(codec: Codec, data: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut has_vcl = false;
    for n in nal::annexb(data) {
        let t = nal::nal_type(codec, n);
        let vcl = nal::is_vcl(codec, n);
        let starts = if vcl {
            has_vcl && first_bit_after(codec, n)
        } else {
            has_vcl
                && match codec {
                    Codec::Hevc => (32..=40).contains(&t),
                    _ => matches!(t, 6..=9),
                }
        };
        if starts {
            units.push(std::mem::take(&mut current));
            has_vcl = false;
        }
        current.extend_from_slice(&[0, 0, 0, 1]);
        current.extend_from_slice(n);
        has_vcl |= vcl;
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

fn packet(i: usize, data: Vec<u8>, step: i64) -> Packet {
    Packet {
        data,
        pts: i as i64 * step,
        dts: i as i64 * step,
        keyframe: false,
    }
}

fn decode_bytes(codec: Codec, data: &[u8], limit: usize) -> (Vec<Frame>, f64) {
    let units = access_units(codec, data);
    let mut dec = HwDecoder::new(codec, &[]).unwrap();
    let mut frames = Vec::new();
    let start = Instant::now();
    for (i, u) in units.into_iter().take(limit).enumerate() {
        frames.extend(dec.decode(&packet(i, u, 1)).unwrap());
    }
    frames.extend(dec.flush().unwrap());
    let secs = start.elapsed().as_secs_f64();
    let fps = frames_per_second(frames.len(), secs);
    (frames, fps)
}

#[cfg(all(feature = "h264", feature = "hevc"))]
fn cpu_decode(codec: Codec, data: &[u8]) -> Vec<Frame> {
    let units = access_units(codec, data);
    let mut frames = Vec::new();
    if codec == Codec::Hevc {
        let mut dec = crate::hevc::Decoder::new();
        for (i, u) in units.into_iter().enumerate() {
            frames.extend(dec.decode(&packet(i, u, 1)).unwrap());
        }
        frames.extend(dec.flush().unwrap());
    } else {
        let mut dec = crate::h264::Decoder::new();
        for (i, u) in units.into_iter().enumerate() {
            frames.extend(dec.decode(&packet(i, u, 1)).unwrap());
        }
        frames.extend(dec.flush().unwrap());
    }
    frames
}

fn frames_per_second(n: usize, secs: f64) -> f64 {
    if secs > 0.0 { n as f64 / secs } else { 0.0 }
}

fn raw(frame: &Frame) -> Vec<u8> {
    let bps = frame.format.bytes_per_sample();
    let mut out = Vec::new();
    for p in &frame.planes {
        for y in 0..p.height {
            out.extend_from_slice(&p.data[y * p.stride..y * p.stride + p.width * bps]);
        }
    }
    out
}

fn frame_digest(frames: &[Frame]) -> String {
    let mut d = Digest::new();
    for f in frames {
        d.frame(f);
    }
    d.hex()
}

fn psnr_bytes(got: &[u8], want: &[u8], ten: bool) -> f64 {
    assert_eq!(got.len(), want.len(), "decoded size");
    let (sum, n, peak) = if ten {
        let sample = |d: &[u8], i: usize| f64::from(u16::from_le_bytes([d[2 * i], d[2 * i + 1]]));
        let n = got.len() / 2;
        (
            (0..n)
                .map(|i| (sample(got, i) - sample(want, i)).powi(2))
                .sum::<f64>(),
            n,
            1023.0,
        )
    } else {
        (
            got.iter()
                .zip(want)
                .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
                .sum::<f64>(),
            got.len(),
            255.0,
        )
    };
    let mse = sum / n.max(1) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

fn psnr(got: &[Frame], want: &[Frame]) -> f64 {
    assert_eq!(got.len(), want.len(), "frame count");
    let ten = want
        .first()
        .is_some_and(|f| f.format == PixelFormat::Yuv420p10);
    let a: Vec<u8> = got.iter().flat_map(raw).collect();
    let b: Vec<u8> = want.iter().flat_map(raw).collect();
    psnr_bytes(&a, &b, ten)
}

fn luma_psnr(got: &Frame, want: &Frame) -> f64 {
    let rows = |f: &Frame| {
        let p = &f.planes[0];
        p.data
            .chunks(p.stride)
            .take(p.height)
            .flat_map(|r| r[..p.width].to_vec())
            .collect::<Vec<u8>>()
    };
    psnr_bytes(&rows(got), &rows(want), false)
}

#[test]
fn decode_is_bit_exact_with_the_recorded_decodes() {
    let r = Refs::open(refs::AREA);
    for s in &refs::STREAMS {
        let codec = if s.hevc { Codec::Hevc } else { Codec::H264 };
        if !has(codec, Direction::Decode) {
            continue;
        }
        for &size in s.sizes {
            let file = s.file(size);
            let (frames, _) = decode_bytes(codec, &r.bytes(&file), usize::MAX);
            assert_eq!(frames.len() as u64, refs::FRAMES, "{file}: frame count");
            assert_eq!((frames[0].width, frames[0].height), size, "{file}: size");
            r.assert_digest(&refs::decoded(&file), &frame_digest(&frames));
        }
    }
}

#[test]
#[cfg(all(feature = "h264", feature = "hevc"))]
fn the_gpu_fixtures_decode_on_the_cpu_to_the_same_digests() {
    let r = Refs::open(refs::AREA);
    for s in &refs::STREAMS {
        let codec = if s.hevc { Codec::Hevc } else { Codec::H264 };
        for &size in s.sizes {
            let file = s.file(size);
            let frames = cpu_decode(codec, &r.bytes(&file));
            assert_eq!(frames.len() as u64, refs::FRAMES, "{file}: frame count");
            let ten = frames.iter().all(|f| f.format == PixelFormat::Yuv420p10);
            assert_eq!(ten, s.ten, "{file}: bit depth");
            r.assert_digest(&refs::decoded(&file), &frame_digest(&frames));
        }
    }
}

#[test]
fn a_damaged_fixture_is_refused_before_use() {
    let r = Refs::open(refs::AREA);
    let file = refs::high(refs::FLOOR);
    let mut bytes = r.bytes(&file);
    let at = bytes.len() / 2;
    bytes[at] ^= 0x40;
    assert_ne!(crate::testref::sha256(&bytes), r.digest(&file));
}

fn source_frames(w: u32, h: u32, n: usize, format: PixelFormat) -> Vec<Frame> {
    source_frames_with(w, h, n, format, 0)
}

fn source_frames_with(w: u32, h: u32, n: usize, format: PixelFormat, amp: u8) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let src = crate::testref::yuv_frame(w, h, i as u64, amp);
            let mut f = Frame::new(format, w, h, i as i64 * 3000, TimeBase::new(1, 90000));
            for (d, s) in f.planes.iter_mut().zip(&src.planes) {
                if format == PixelFormat::Yuv420p10 {
                    for (o, v) in d.data.chunks_exact_mut(2).zip(&s.data) {
                        o.copy_from_slice(&(u16::from(*v) << 2).to_le_bytes());
                    }
                } else {
                    d.data.copy_from_slice(&s.data);
                }
            }
            f
        })
        .collect()
}

#[test]
fn source_frames_widen_to_ten_bits_by_a_shift() {
    let eight = source_frames(64, 48, 2, PixelFormat::Yuv420p8);
    let ten = source_frames(64, 48, 2, PixelFormat::Yuv420p10);
    assert_eq!(eight.len(), 2);
    for (a, b) in eight.iter().zip(&ten) {
        assert_eq!(b.format, PixelFormat::Yuv420p10);
        assert_eq!((a.width, a.height, a.pts), (b.width, b.height, b.pts));
        for (p, q) in a.planes.iter().zip(&b.planes) {
            assert_eq!(q.data.len(), 2 * p.data.len());
            for (v, w) in p.data.iter().zip(q.data.chunks_exact(2)) {
                assert_eq!(u16::from_le_bytes([w[0], w[1]]), u16::from(*v) << 2);
            }
        }
    }
    assert_ne!(eight[0].planes[0].data, eight[1].planes[0].data);
    assert_eq!(psnr(&eight, &eight), 99.0);
    assert_eq!(psnr(&ten, &ten), 99.0);
    let mut off = eight.clone();
    off[1].planes[0].data[0] ^= 0x10;
    let db = psnr(&off, &eight);
    assert!(db > 40.0 && db < 99.0, "{db}");
}

#[test]
fn psnr_counts_ten_bit_samples_against_their_own_peak() {
    let want = [0u8, 0, 0, 1];
    let got = [1u8, 0, 0, 1];
    let db = psnr_bytes(&got, &want, true);
    let expect = 10.0 * (1023.0f64 * 1023.0 / 0.5).log10();
    assert!((db - expect).abs() < 1e-9, "{db} against {expect}");
    let db = psnr_bytes(&got, &want, false);
    let expect = 10.0 * (255.0f64 * 255.0 / 0.25).log10();
    assert!((db - expect).abs() < 1e-9, "{db} against {expect}");
}

#[test]
fn access_units_split_an_annex_b_stream_per_picture() {
    let sps = [0x67, 0x42, 0x00];
    let pps = [0x68, 0xce];
    let first = [0x65, 0x88, 0x01];
    let second = [0x41, 0x9a, 0x02];
    let mut data = Vec::new();
    for n in [&sps[..], &pps, &first, &second] {
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(n);
    }
    let units = access_units(Codec::H264, &data);
    assert_eq!(units.len(), 2);
    assert!(units[0].ends_with(&first));
    assert!(units[0].starts_with(&[0, 0, 0, 1, 0x67]));
    assert_eq!(units[1], [&[0u8, 0, 0, 1][..], &second].concat());
    assert!(access_units(Codec::H264, &[]).is_empty());
}

fn encode_all(
    codec: Codec,
    config: &EncoderConfig,
    frames: &[Frame],
) -> (Vec<Packet>, Vec<u8>, f64) {
    let mut enc = HwEncoder::new(codec, config).unwrap();
    let mut packets = Vec::new();
    let start = Instant::now();
    for f in frames {
        packets.extend(enc.encode(f).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    let fps = frames_per_second(frames.len(), start.elapsed().as_secs_f64());
    let extradata = enc.extradata().unwrap().to_vec();
    (packets, extradata, fps)
}

fn annexb_stream(codec: Codec, packets: &[Packet], extradata: &[u8]) -> Vec<u8> {
    let config = nal::parse_extradata(codec, extradata).unwrap();
    let mut out = Vec::new();
    for p in packets {
        if p.keyframe {
            for s in &config.parameter_sets {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(s);
            }
        }
        for n in nal::length_prefixed(&p.data, 4).unwrap() {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
    }
    out
}

#[test]
#[cfg(all(feature = "h264", feature = "hevc"))]
fn encodes_decode_back_in_our_decoders() {
    for (w, h) in [(640, 360), (426, 240)] {
        encode_decode_back(w, h);
    }
}

#[cfg(all(feature = "h264", feature = "hevc"))]
fn encode_decode_back(w: u32, h: u32) {
    let n = 60;
    let cases = [
        (Codec::H264, Profile::High, 2, PixelFormat::Yuv420p8),
        (Codec::H264, Profile::Main, 0, PixelFormat::Yuv420p8),
        (Codec::Hevc, Profile::Main, 0, PixelFormat::Yuv420p8),
        (Codec::Hevc, Profile::Main10, 0, PixelFormat::Yuv420p10),
    ];
    for (codec, profile, b_frames, format) in cases {
        if !has(codec, Direction::Encode) {
            continue;
        }
        let frames = source_frames(w, h, n, format);
        let config = EncoderConfig {
            width: w,
            height: h,
            fps: (30, 1),
            profile,
            rate: Rate::Crf(18),
            gop: 30,
            b_frames,
            bitstream: Bitstream::LengthPrefixed,
            ..Default::default()
        };
        let (packets, extradata, _) = encode_all(codec, &config, &frames);
        assert_eq!(packets.len(), n, "{codec:?} {profile:?}: packet count");
        assert!(packets[0].keyframe);
        let times: Vec<(i64, i64)> = packets.iter().map(|p| (p.pts, p.dts)).collect();
        assert!(packets.windows(2).all(|p| p[0].dts < p[1].dts), "{times:?}");
        assert!(packets.iter().all(|p| p.dts <= p.pts), "{times:?}");
        let decoded = cpu_decode(codec, &annexb_stream(codec, &packets, &extradata));
        assert_eq!(decoded.len(), n, "{codec:?} {profile:?}: decoded frames");
        assert!(
            decoded.iter().all(|f| f.format == format),
            "{codec:?} {profile:?}: decoded format"
        );
        let db = psnr(&decoded, &frames);
        assert!(db > 40.0, "{codec:?} {profile:?} {w}x{h}: PSNR {db:.2} dB");
    }
}

#[test]
fn a_59_94_encode_returns_from_the_driver() {
    for (codec, profile) in [(Codec::H264, Profile::High), (Codec::Hevc, Profile::Main)] {
        if !has(codec, Direction::Encode) {
            continue;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (w, h) = (960, 720);
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (60000, 1001),
                profile,
                rate: Rate::Crf(23),
                gop: 8,
                b_frames: 0,
                bitstream: Bitstream::LengthPrefixed,
                ..Default::default()
            };
            let frames: Vec<Frame> = (0..8)
                .map(|i| {
                    Frame::new(
                        PixelFormat::Yuv420p8,
                        w,
                        h,
                        i * 1001,
                        TimeBase::new(1, 60000),
                    )
                })
                .collect();
            let (packets, _, _) = encode_all(codec, &config, &frames);
            let _ = tx.send(packets.len());
        });
        let n = rx.recv_timeout(std::time::Duration::from_secs(60));
        assert_eq!(n, Ok(8), "{codec:?} at 960x720 60000/1001 never returned");
    }
}

#[test]
#[cfg(all(feature = "h264", feature = "hevc"))]
fn bitrate_modes_land_near_their_target() {
    let (w, h, n) = (640, 360, 60);
    for codec in [Codec::H264, Codec::Hevc] {
        if !has(codec, Direction::Encode) {
            continue;
        }
        let frames = source_frames(w, h, n, PixelFormat::Yuv420p8);
        for rate in [
            Rate::Bitrate(2000),
            Rate::Vbr {
                kbps: 2000,
                max_kbps: 3000,
            },
        ] {
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (30, 1),
                profile: Profile::Main,
                rate,
                gop: 30,
                b_frames: 0,
                bitstream: Bitstream::AnnexB,
                ..Default::default()
            };
            let (packets, _, _) = encode_all(codec, &config, &frames);
            let kbps =
                packets.iter().map(|p| p.data.len()).sum::<usize>() as f64 * 8.0 / 2.0 / 1000.0;
            assert!(
                (1000.0..3500.0).contains(&kbps),
                "{codec:?} {rate:?}: {kbps:.0} kbps"
            );
            let stream: Vec<u8> = packets.iter().flat_map(|p| p.data.clone()).collect();
            let decoded = cpu_decode(codec, &stream);
            assert_eq!(decoded.len(), n, "{codec:?} {rate:?}: Annex B frames");
        }
    }
}

fn transcode(
    src: Codec,
    data: &[u8],
    dst: Codec,
    config: &EncoderConfig,
    limit: usize,
) -> (Vec<Packet>, Vec<u8>, f64) {
    let units = access_units(src, data);
    let mut dec = HwDecoder::new(src, &[]).unwrap();
    let mut enc = HwEncoder::new(dst, config).unwrap();
    let mut packets = Vec::new();
    let start = Instant::now();
    let mut n = 0;
    for (i, u) in units.into_iter().take(limit).enumerate() {
        for f in dec.decode_gpu(&packet(i, u, 3000)).unwrap() {
            packets.extend(enc.encode_gpu(&f).unwrap());
            n += 1;
        }
    }
    for f in dec.flush_gpu().unwrap() {
        packets.extend(enc.encode_gpu(&f).unwrap());
        n += 1;
    }
    packets.extend(enc.flush().unwrap());
    let fps = frames_per_second(n, start.elapsed().as_secs_f64());
    (packets, enc.extradata().unwrap().to_vec(), fps)
}

#[test]
#[cfg(all(feature = "h264", feature = "hevc"))]
fn transcodes_on_the_gpu_without_copies() {
    if !has(Codec::H264, Direction::Decode) || !has(Codec::Hevc, Direction::Encode) {
        return;
    }
    let r = Refs::open(refs::AREA);
    let src = r.bytes(&refs::high(refs::WIDE));
    let reference = cpu_decode(Codec::H264, &src);
    let n = refs::FRAMES as usize;
    assert_eq!(reference.len(), n);
    for (w, h) in [refs::WIDE, refs::FLOOR] {
        let config = EncoderConfig {
            width: w,
            height: h,
            fps: (25, 1),
            profile: Profile::Main,
            rate: Rate::Crf(18),
            gop: 25,
            b_frames: 0,
            bitstream: Bitstream::LengthPrefixed,
            ..Default::default()
        };
        let (packets, extradata, _) =
            transcode(Codec::H264, &src, Codec::Hevc, &config, usize::MAX);
        assert_eq!(packets.len(), n, "{w}x{h}: packets");
        let decoded = cpu_decode(Codec::Hevc, &annexb_stream(Codec::Hevc, &packets, &extradata));
        assert_eq!(decoded.len(), n, "{w}x{h}: decoded frames");
        assert!(
            decoded.iter().all(|f| (f.width, f.height) == (w, h)),
            "{w}x{h}: decoded size"
        );
        if (w, h) == refs::WIDE {
            let db = psnr(&decoded, &reference);
            assert!(db > 38.0, "GPU transcode PSNR {db:.2} dB");
        }
    }
}

fn gpu_frames(codec: Codec, data: &[u8], limit: usize) -> (Vec<super::GpuFrame>, f64) {
    let units = access_units(codec, data);
    let mut dec = HwDecoder::new(codec, &[]).unwrap();
    let mut kept = Vec::new();
    let mut n = 0;
    let start = Instant::now();
    for (i, u) in units.into_iter().take(limit).enumerate() {
        for f in dec.decode_gpu(&packet(i, u, 1)).unwrap() {
            n += 1;
            if kept.is_empty() {
                kept.push(f);
            }
        }
    }
    for f in dec.flush_gpu().unwrap() {
        n += 1;
        if kept.is_empty() {
            kept.push(f);
        }
    }
    (kept, frames_per_second(n, start.elapsed().as_secs_f64()))
}

#[test]
#[cfg(all(target_os = "linux", feature = "filter"))]
fn gpu_frames_export_as_dmabuf_and_scale_on_the_gpu() {
    if !has(Codec::H264, Direction::Decode) {
        return;
    }
    let r = Refs::open(refs::AREA);
    let (w, h) = refs::FLOOR;
    let (frames, _) = gpu_frames(Codec::H264, &r.bytes(&refs::high(refs::FLOOR)), 1);
    let frame = &frames[0];
    assert!(frame.on_gpu());
    let buf = frame.dmabuf(false).unwrap();
    assert_eq!(buf.fourcc, u32::from_le_bytes(*b"NV12"));
    assert!(!buf.objects.is_empty());
    assert_eq!(buf.layers.len(), 1);
    assert_eq!(buf.layers[0].planes.len(), 2);
    assert!(
        buf.layers[0]
            .planes
            .iter()
            .all(|p| p.pitch >= w && p.object < buf.objects.len())
    );
    let separate = frame.dmabuf(true).unwrap();
    assert_eq!(separate.layers.len(), 2);

    let (sw, sh) = (2 * w, 2 * h);
    let scaler = super::GpuScaler::new(sw, sh, false).unwrap();
    let big = scaler.scale(frame).unwrap().download().unwrap();
    assert_eq!((big.width, big.height), (sw, sh));
    let full = frame.download().unwrap();
    assert_eq!((full.width, full.height), (w, h));
    let expected =
        crate::filter::scale(&full, sw, sh, crate::filter::Kernel::Bilinear).unwrap();
    let db = psnr_bytes(&raw(&big), &raw(&expected), false);
    assert!(db > 28.0, "GPU scale PSNR {db:.2} dB against our CPU bilinear");
}

fn mean(db: &[f64], range: std::ops::Range<usize>) -> f64 {
    let s = &db[range.start.min(db.len())..range.end.min(db.len())];
    s.iter().sum::<f64>() / s.len().max(1) as f64
}

#[test]
fn mean_takes_a_clamped_range() {
    let db = [30.0, 32.0, 34.0];
    assert_eq!(mean(&db, 0..3), 32.0);
    assert_eq!(mean(&db, 1..10), 33.0);
    assert_eq!(mean(&db, 5..9), 0.0);
}

#[test]
#[cfg(all(feature = "h264", feature = "hevc"))]
fn per_frame_quality_holds_from_the_first_frame() {
    let (w, h, n) = (640, 360, 120);
    let frames = source_frames_with(w, h, n, PixelFormat::Yuv420p8, 3);
    for codec in [Codec::H264, Codec::Hevc] {
        if !has(codec, Direction::Encode) {
            continue;
        }
        for rate in [Rate::Qp(26), Rate::Bitrate(1500)] {
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (30, 1),
                profile: if codec == Codec::Hevc {
                    Profile::Main
                } else {
                    Profile::High
                },
                rate,
                gop: 60,
                b_frames: 0,
                bitstream: Bitstream::LengthPrefixed,
                ..Default::default()
            };
            let name = format!("{codec:?}-{rate:?}");
            let (packets, extradata, _) = encode_all(codec, &config, &frames);
            let stream = annexb_stream(codec, &packets, &extradata);
            let decoded = cpu_decode(codec, &stream);
            assert_eq!(decoded.len(), n, "{name}: frames decoded");
            let db: Vec<f64> = decoded
                .iter()
                .zip(&frames)
                .map(|(a, b)| luma_psnr(a, b))
                .collect();
            let all = mean(&db, 0..n);
            let low = db.iter().copied().fold(f64::INFINITY, f64::min);
            eprintln!(
                "{name}: {} B, mean {all:.2} dB, min {low:.2} dB",
                stream.len()
            );
            if let Rate::Bitrate(kbps) = rate {
                let target = f64::from(kbps) * 1000.0 / 8.0 * n as f64 / 30.0;
                let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
                let off = bytes as f64 / target - 1.0;
                assert!(
                    off.abs() < 0.1,
                    "{name}: {:.1} % off the target size",
                    off * 100.0
                );
                let first = mean(&db, 0..10);
                assert!(
                    first > all - 2.0,
                    "{name}: first 10 frames at {first:.2} dB against {all:.2} for all"
                );
            } else {
                assert!(low > 30.0, "{name}: a frame at {low:.2} dB");
                for (i, d) in db.iter().enumerate() {
                    assert!(
                        (d - all).abs() < 2.0,
                        "{name}: frame {i} at {d:.2} dB against a mean of {all:.2}"
                    );
                }
            }
        }
    }
}

fn gpu_stream(codec: Codec, w: u32, h: u32, n: usize) -> Option<Vec<u8>> {
    if !has(codec, Direction::Encode) {
        return None;
    }
    let cycle = source_frames(w, h, 60, PixelFormat::Yuv420p8);
    let profile = if codec == Codec::Hevc {
        Profile::Main
    } else {
        Profile::High
    };
    let config = EncoderConfig {
        width: w,
        height: h,
        fps: (60, 1),
        profile,
        rate: Rate::Crf(20),
        gop: 120,
        b_frames: 0,
        bitstream: Bitstream::LengthPrefixed,
        ..Default::default()
    };
    let mut enc = HwEncoder::new(codec, &config).unwrap();
    let mut packets = Vec::new();
    for i in 0..n {
        let mut f = cycle[i % cycle.len()].clone();
        f.pts = i as i64 * 1500;
        packets.extend(enc.encode(&f).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    Some(annexb_stream(codec, &packets, enc.extradata().unwrap()))
}

#[test]
#[ignore]
fn speed() {
    let mut report = String::new();
    if has(Codec::H264, Direction::Decode)
        && let Some(big) = gpu_stream(Codec::H264, 2560, 1440, 600)
    {
        decode_bytes(Codec::H264, &big, 60);
        let (frames, fps) = decode_bytes(Codec::H264, &big, 600);
        report.push_str(&format!(
            "decode H.264 2560x1440: {fps:.0} fps ({} frames)\n",
            frames.len()
        ));
        gpu_frames(Codec::H264, &big, 60);
        let (_, fps) = gpu_frames(Codec::H264, &big, 600);
        report.push_str(&format!(
            "decode H.264 2560x1440 to GPU surfaces, no read-back: {fps:.0} fps\n"
        ));
        if let Some(small) = gpu_stream(Codec::H264, 1920, 1080, 600) {
            let (frames, fps) = decode_bytes(Codec::H264, &small, 600);
            report.push_str(&format!(
                "decode H.264 1920x1080: {fps:.0} fps ({} frames)\n",
                frames.len()
            ));
        }
        if has(Codec::Hevc, Direction::Decode)
            && let Some(hevc) = gpu_stream(Codec::Hevc, 2560, 1440, 600)
        {
            let (frames, fps) = decode_bytes(Codec::Hevc, &hevc, 600);
            report.push_str(&format!(
                "decode HEVC 2560x1440: {fps:.0} fps ({} frames)\n",
                frames.len()
            ));
        }
        for (dst, w, h) in [
            (Codec::H264, 2560, 1440),
            (Codec::Hevc, 2560, 1440),
            (Codec::H264, 1920, 1080),
        ] {
            if !has(dst, Direction::Encode) {
                continue;
            }
            let profile = if dst == Codec::Hevc {
                Profile::Main
            } else {
                Profile::High
            };
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (60, 1),
                profile,
                rate: Rate::Crf(20),
                gop: 120,
                b_frames: 0,
                bitstream: Bitstream::LengthPrefixed,
                ..Default::default()
            };
            let (packets, _, fps) = transcode(Codec::H264, &big, dst, &config, 600);
            report.push_str(&format!(
                "transcode H.264 2560x1440 -> {dst:?} {w}x{h} on the GPU: {fps:.0} fps ({} frames)\n",
                packets.len()
            ));
        }
    }
    for (w, h) in [(1920, 1080), (2560, 1440)] {
        let frames = source_frames(w, h, 120, PixelFormat::Yuv420p8);
        for (codec, profile, b) in [
            (Codec::H264, Profile::High, 0),
            (Codec::H264, Profile::High, 2),
            (Codec::Hevc, Profile::Main, 0),
        ] {
            if !has(codec, Direction::Encode) {
                continue;
            }
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (30, 1),
                profile,
                rate: Rate::Crf(18),
                gop: 60,
                b_frames: b,
                bitstream: Bitstream::LengthPrefixed,
                ..Default::default()
            };
            let (_, _, fps) = encode_all(codec, &config, &frames);
            report.push_str(&format!(
                "encode {codec:?} {profile:?} b={b} {w}x{h} CRF 18: {fps:.0} fps\n"
            ));
        }
    }
    eprintln!("{report}");
    std::fs::write(dir().join("speed.txt"), report).unwrap();
}

#[test]
#[cfg(all(
    target_os = "linux",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]
fn an_overlay_drawn_on_the_gpu_matches_the_cpu_blend() {
    use crate::media::Rgba;
    use std::sync::Arc;
    if !has(Codec::H264, Direction::Decode) || !has(Codec::H264, Direction::Encode) {
        return;
    }
    let r = Refs::open(refs::AREA);
    let data = r.bytes(&refs::high(refs::FLOOR));
    let (w, h) = refs::FLOOR;
    let config = EncoderConfig {
        width: w,
        height: h,
        fps: (25, 1),
        profile: Profile::High,
        rate: Rate::Crf(1),
        gop: 25,
        b_frames: 0,
        bitstream: Bitstream::LengthPrefixed,
        ..Default::default()
    };
    let mut drawn = HwEncoder::new(Codec::H264, &config).unwrap();
    if !drawn.composes() {
        eprintln!("skipped: this GPU encoder doesn't draw overlays");
        return;
    }
    let mut plain = HwEncoder::new(Codec::H264, &config).unwrap();
    let mut clear = HwEncoder::new(Codec::H264, &config).unwrap();
    let stride = w as usize;
    let mut mark = Rgba::new(w, h);
    for y in 160..230usize {
        for x in 200..340usize {
            let o = (y * stride + x) * 4;
            let a = ((x - 200) * 255 / 139) as u8;
            let stripe = if (x / 5 + y / 7) % 2 == 0 { 250 } else { 30 };
            mark.data[o..o + 4].copy_from_slice(&[stripe, 200, 255 - stripe, a]);
        }
    }
    let mark = Arc::new(mark);
    let empty = Arc::new(Rgba::new(w, h));
    let mut dec = HwDecoder::new(Codec::H264, &[]).unwrap();
    let mut blended = Vec::new();
    let (mut a, mut b, mut c) = (Vec::new(), Vec::new(), Vec::new());
    let mut each = |f: super::GpuFrame| {
        a.extend(drawn.encode_gpu_over(&f, &mark).unwrap());
        b.extend(plain.encode_gpu(&f).unwrap());
        c.extend(clear.encode_gpu_over(&f, &empty).unwrap());
        let mut cpu = f.download().unwrap();
        crate::media::blend(&mut cpu, &mark);
        blended.extend(raw(&cpu));
    };
    for (i, u) in access_units(Codec::H264, &data).into_iter().enumerate() {
        dec.decode_gpu(&packet(i, u, 1))
            .unwrap()
            .into_iter()
            .for_each(&mut each);
    }
    dec.flush_gpu().unwrap().into_iter().for_each(&mut each);
    a.extend(drawn.flush().unwrap());
    b.extend(plain.flush().unwrap());
    c.extend(clear.flush().unwrap());
    assert_eq!(a.len() as u64, refs::FRAMES);
    let bytes = |v: &[Packet]| v.iter().map(|p| p.data.clone()).collect::<Vec<_>>();
    assert_eq!(bytes(&b), bytes(&c), "a clear overlay changed the frames");
    let stream = annexb_stream(Codec::H264, &a, drawn.extradata().unwrap());
    let decoded: Vec<u8> = cpu_decode(Codec::H264, &stream)
        .iter()
        .flat_map(raw)
        .collect();
    let db = psnr_bytes(&decoded, &blended, false);
    assert!(db > 45.0, "GPU overlay against the CPU blend: {db:.2} dB");
}

#[test]
fn a_restart_may_change_only_the_rate_and_the_gop() {
    let asked = EncoderConfig::default();
    let next = EncoderConfig {
        rate: Rate::Crf(30),
        gop: 7,
        ..asked.clone()
    };
    assert!(super::restartable(&asked, &asked));
    assert!(super::restartable(&asked, &next));
    for changed in [
        EncoderConfig {
            sar: (4, 3),
            ..asked.clone()
        },
        EncoderConfig {
            color: super::Color::UNKNOWN,
            ..asked.clone()
        },
        EncoderConfig {
            width: 1280,
            ..asked.clone()
        },
        EncoderConfig {
            b_frames: 2,
            ..asked.clone()
        },
    ] {
        assert!(!super::restartable(&asked, &changed), "{changed:?}");
    }
}
