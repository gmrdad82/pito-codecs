use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::nal;
use super::{Direction, EncoderConfig, HwDecoder, HwEncoder, Profile, available};
use crate::media::{Bitstream, Rate};
use crate::media::{Codec, Frame, Packet, PixelFormat, TimeBase};

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

fn ffmpeg() -> bool {
    let ok = Command::new("ffmpeg").arg("-version").output().is_ok();
    if !ok {
        eprintln!("skipped: no ffmpeg");
    }
    ok
}

fn run(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn decode_file(codec: Codec, path: &Path, limit: usize) -> (Vec<Frame>, f64) {
    let data = std::fs::read(path).unwrap();
    let units = access_units(codec, &data);
    let mut dec = HwDecoder::new(codec, &[]).unwrap();
    let mut frames = Vec::new();
    let start = Instant::now();
    for (i, u) in units.iter().take(limit).enumerate() {
        let p = Packet {
            data: u.clone(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        };
        frames.extend(dec.decode(&p).unwrap());
    }
    frames.extend(dec.flush().unwrap());
    let secs = start.elapsed().as_secs_f64();
    let fps = frames_per_second(frames.len(), secs);
    (frames, fps)
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

fn reference(path: &Path, pix: &str) -> Vec<u8> {
    let out = path.with_extension(format!("{pix}.yuv"));
    run(&[
        "-i",
        path.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix,
        out.to_str().unwrap(),
    ]);
    std::fs::read(out).unwrap()
}

fn generate_sized(name: &str, codec: Codec, encoder_args: &[&str], size: (u32, u32)) -> PathBuf {
    let ext = if codec == Codec::Hevc { "hevc" } else { "h264" };
    let path = dir().join(format!("{name}.{ext}"));
    if !path.exists() {
        let testsrc = format!("testsrc2=size={}x{}:rate=25:duration=2", size.0, size.1);
        let mandelbrot = format!("mandelbrot=size={}x{}:rate=25", size.0, size.1);
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            &testsrc,
            "-f",
            "lavfi",
            "-i",
            &mandelbrot,
            "-filter_complex",
            "[0][1]blend=all_mode=average,noise=alls=12:allf=t",
            "-frames:v",
            "50",
        ];
        args.extend_from_slice(encoder_args);
        args.push(path.to_str().unwrap());
        run(&args);
    }
    path
}

#[test]
fn decode_is_bit_exact_with_ffmpeg() {
    if !ffmpeg() {
        return;
    }
    let streams: &[(&str, Codec, &[&str], &str)] = &[
        (
            "h264-baseline",
            Codec::H264,
            &[
                "-c:v",
                "libx264",
                "-profile:v",
                "baseline",
                "-pix_fmt",
                "yuv420p",
            ],
            "yuv420p",
        ),
        (
            "h264-main-cabac",
            Codec::H264,
            &[
                "-c:v",
                "libx264",
                "-profile:v",
                "main",
                "-bf",
                "0",
                "-pix_fmt",
                "yuv420p",
            ],
            "yuv420p",
        ),
        (
            "h264-high-bframes",
            Codec::H264,
            &[
                "-c:v",
                "libx264",
                "-profile:v",
                "high",
                "-bf",
                "3",
                "-refs",
                "4",
                "-pix_fmt",
                "yuv420p",
            ],
            "yuv420p",
        ),
        (
            "hevc-main",
            Codec::Hevc,
            &[
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error:bframes=4",
                "-pix_fmt",
                "yuv420p",
            ],
            "yuv420p",
        ),
        (
            "hevc-main10",
            Codec::Hevc,
            &[
                "-c:v",
                "libx265",
                "-x265-params",
                "log-level=error",
                "-pix_fmt",
                "yuv420p10le",
            ],
            "yuv420p10le",
        ),
    ];
    for &(name, codec, args, pix) in streams {
        if !has(codec, Direction::Decode) {
            continue;
        }
        for size in [(352, 288), (426, 240)] {
            let name = format!("{name}-{}x{}", size.0, size.1);
            let path = generate_sized(&name, codec, args, size);
            let expected = reference(&path, pix);
            let (frames, _) = decode_file(codec, &path, usize::MAX);
            assert_eq!(frames.len(), 50, "{name}: frame count");
            assert_eq!((frames[0].width, frames[0].height), size, "{name}: size");
            let got: Vec<u8> = frames.iter().flat_map(raw).collect();
            assert!(
                got == expected,
                "{name}: decoded pictures differ from ffmpeg's"
            );
        }
    }
}

fn source_frames(w: u32, h: u32, n: usize, format: PixelFormat) -> Vec<Frame> {
    source_frames_from(
        &format!("testsrc2=size={w}x{h}:rate=30"),
        "",
        w,
        h,
        n,
        format,
    )
}

fn source_frames_from(
    input: &str,
    tag: &str,
    w: u32,
    h: u32,
    n: usize,
    format: PixelFormat,
) -> Vec<Frame> {
    let pix = if format == PixelFormat::Yuv420p10 {
        "yuv420p10le"
    } else {
        "yuv420p"
    };
    let path = dir().join(format!("source{tag}-{w}x{h}-{n}-{pix}.yuv"));
    if !path.exists() {
        run(&[
            "-f",
            "lavfi",
            "-i",
            input,
            "-frames:v",
            &n.to_string(),
            "-pix_fmt",
            pix,
            "-f",
            "rawvideo",
            path.to_str().unwrap(),
        ]);
    }
    let data = std::fs::read(&path).unwrap();
    let bps = format.bytes_per_sample();
    let size = (w * h) as usize * bps * 3 / 2;
    data.chunks_exact(size)
        .enumerate()
        .map(|(i, chunk)| {
            let mut f = Frame::new(format, w, h, i as i64 * 3000, TimeBase::new(1, 90000));
            let mut at = 0;
            for p in &mut f.planes {
                let len = p.data.len();
                p.data.copy_from_slice(&chunk[at..at + len]);
                at += len;
            }
            f
        })
        .collect()
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

fn psnr(path: &Path, source: &Path, pix: &str) -> f64 {
    let decoded = path.with_extension(format!("{pix}.yuv"));
    run(&[
        "-i",
        path.to_str().unwrap(),
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix,
        decoded.to_str().unwrap(),
    ]);
    let (a, b) = (
        std::fs::read(decoded).unwrap(),
        std::fs::read(source).unwrap(),
    );
    assert_eq!(a.len(), b.len(), "decoded size");
    let (sum, n, peak) = if pix.contains("10") {
        let sample = |d: &[u8], i: usize| f64::from(u16::from_le_bytes([d[2 * i], d[2 * i + 1]]));
        let n = a.len() / 2;
        (
            (0..n)
                .map(|i| (sample(&a, i) - sample(&b, i)).powi(2))
                .sum::<f64>(),
            n,
            1023.0,
        )
    } else {
        (
            a.iter()
                .zip(&b)
                .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
                .sum::<f64>(),
            a.len(),
            255.0,
        )
    };
    let mse = sum / n as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

fn probe_frames(path: &Path) -> usize {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

#[test]
fn encodes_decode_back_in_ffmpeg() {
    if !ffmpeg() {
        return;
    }
    for (w, h) in [(640, 360), (426, 240)] {
        encode_decode_back(w, h);
    }
}

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
        };
        let (packets, extradata, _) = encode_all(codec, &config, &frames);
        assert_eq!(packets.len(), n, "{codec:?} {profile:?}: packet count");
        assert!(packets[0].keyframe);
        let times: Vec<(i64, i64)> = packets.iter().map(|p| (p.pts, p.dts)).collect();
        assert!(packets.windows(2).all(|p| p[0].dts < p[1].dts), "{times:?}");
        assert!(packets.iter().all(|p| p.dts <= p.pts), "{times:?}");
        let ext = if codec == Codec::Hevc { "hevc" } else { "h264" };
        let path = dir().join(format!("encoded-{profile:?}-{b_frames}-{w}x{h}.{ext}"));
        std::fs::write(&path, annexb_stream(codec, &packets, &extradata)).unwrap();
        assert_eq!(
            probe_frames(&path),
            n,
            "{codec:?} {profile:?}: ffprobe frames"
        );
        let pix = if format == PixelFormat::Yuv420p10 {
            "yuv420p10le"
        } else {
            "yuv420p"
        };
        let source = dir().join(format!("source-{w}x{h}-{n}-{pix}.yuv"));
        let db = psnr(&path, &source, pix);
        assert!(db > 40.0, "{codec:?} {profile:?}: PSNR {db:.2} dB");
    }
}

#[test]
fn bitrate_modes_land_near_their_target() {
    if !ffmpeg() {
        return;
    }
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
            };
            let (packets, _, _) = encode_all(codec, &config, &frames);
            let kbps =
                packets.iter().map(|p| p.data.len()).sum::<usize>() as f64 * 8.0 / 2.0 / 1000.0;
            assert!(
                (1000.0..3500.0).contains(&kbps),
                "{codec:?} {rate:?}: {kbps:.0} kbps"
            );
            let ext = if codec == Codec::Hevc { "hevc" } else { "h264" };
            let path = dir().join(format!("rate-{codec:?}.{ext}"));
            let stream: Vec<u8> = packets.iter().flat_map(|p| p.data.clone()).collect();
            std::fs::write(&path, stream).unwrap();
            assert_eq!(probe_frames(&path), n, "{codec:?} {rate:?}: Annex B frames");
        }
    }
}

fn transcode(
    src: Codec,
    path: &Path,
    dst: Codec,
    config: &EncoderConfig,
    limit: usize,
) -> (Vec<Packet>, Vec<u8>, f64) {
    let data = std::fs::read(path).unwrap();
    let units = access_units(src, &data);
    let mut dec = HwDecoder::new(src, &[]).unwrap();
    let mut enc = HwEncoder::new(dst, config).unwrap();
    let mut packets = Vec::new();
    let start = Instant::now();
    let mut n = 0;
    for (i, u) in units.iter().take(limit).enumerate() {
        let p = Packet {
            data: u.clone(),
            pts: i as i64 * 3000,
            dts: i as i64 * 3000,
            keyframe: false,
        };
        for f in dec.decode_gpu(&p).unwrap() {
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
fn transcodes_on_the_gpu_without_copies() {
    if !ffmpeg() || !has(Codec::H264, Direction::Decode) || !has(Codec::Hevc, Direction::Encode) {
        return;
    }
    let src = generate_sized(
        "h264-high-640",
        Codec::H264,
        &[
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-bf",
            "3",
            "-refs",
            "4",
            "-pix_fmt",
            "yuv420p",
        ],
        (640, 360),
    );
    let reference = reference(&src, "yuv420p");
    for (w, h) in [(640, 360), (480, 272)] {
        let config = EncoderConfig {
            width: w,
            height: h,
            fps: (25, 1),
            profile: Profile::Main,
            rate: Rate::Crf(18),
            gop: 25,
            b_frames: 0,
            bitstream: Bitstream::LengthPrefixed,
        };
        let (packets, extradata, _) =
            transcode(Codec::H264, &src, Codec::Hevc, &config, usize::MAX);
        assert_eq!(packets.len(), 50);
        let path = dir().join(format!("transcoded-{w}x{h}.hevc"));
        std::fs::write(&path, annexb_stream(Codec::Hevc, &packets, &extradata)).unwrap();
        assert_eq!(probe_frames(&path), 50);
        if w == 640 {
            let source = dir().join("transcode-reference.yuv");
            std::fs::write(&source, &reference).unwrap();
            let db = psnr(&path, &source, "yuv420p");
            assert!(db > 38.0, "GPU transcode PSNR {db:.2} dB");
        }
    }
}

fn gpu_frames(codec: Codec, path: &Path, limit: usize) -> (Vec<super::GpuFrame>, f64) {
    let data = std::fs::read(path).unwrap();
    let units = access_units(codec, &data);
    let mut dec = HwDecoder::new(codec, &[]).unwrap();
    let mut kept = Vec::new();
    let mut n = 0;
    let start = Instant::now();
    for (i, u) in units.iter().take(limit).enumerate() {
        let p = Packet {
            data: u.clone(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        };
        for f in dec.decode_gpu(&p).unwrap() {
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
#[cfg(target_os = "linux")]
fn gpu_frames_export_as_dmabuf_and_scale_on_the_gpu() {
    if !ffmpeg() || !has(Codec::H264, Direction::Decode) {
        return;
    }
    let src = generate_sized(
        "h264-high-640",
        Codec::H264,
        &[
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-bf",
            "3",
            "-refs",
            "4",
            "-pix_fmt",
            "yuv420p",
        ],
        (640, 360),
    );
    let (frames, _) = gpu_frames(Codec::H264, &src, 1);
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
            .all(|p| p.pitch >= 640 && p.object < buf.objects.len())
    );
    let separate = frame.dmabuf(true).unwrap();
    assert_eq!(separate.layers.len(), 2);

    let scaler = super::GpuScaler::new(320, 180, false).unwrap();
    let small = scaler.scale(frame).unwrap().download().unwrap();
    assert_eq!((small.width, small.height), (320, 180));
    let reference = dir().join("scaled-reference.yuv");
    run(&[
        "-i",
        src.to_str().unwrap(),
        "-frames:v",
        "1",
        "-vf",
        "scale=320:180:flags=bilinear",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        reference.to_str().unwrap(),
    ]);
    let expected = std::fs::read(&reference).unwrap();
    let got = raw(&small);
    let mse = got
        .iter()
        .zip(&expected)
        .map(|(&a, &b)| (f64::from(a) - f64::from(b)).powi(2))
        .sum::<f64>()
        / got.len() as f64;
    let db = 10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10();
    assert!(
        db > 28.0,
        "GPU scale PSNR {db:.2} dB against ffmpeg bilinear"
    );
}

fn plane_psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum::<f64>()
        / a.len().max(1) as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

fn render_node() -> Option<String> {
    (128..136)
        .map(|n| format!("/dev/dri/renderD{n}"))
        .find(|p| Path::new(p).exists())
}

fn ffmpeg_vaapi_args(codec: Codec, rate: Rate, gop: u32, b_frames: u8) -> Vec<String> {
    let encoder = if codec == Codec::Hevc {
        "hevc_vaapi"
    } else {
        "h264_vaapi"
    };
    let mut args: Vec<String> = vec![
        "-c:v".into(),
        encoder.into(),
        "-bf".into(),
        b_frames.to_string(),
        "-g".into(),
        gop.to_string(),
    ];
    let rate: Vec<String> = match rate {
        Rate::Qp(q) | Rate::Crf(q) => {
            vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), q.to_string()]
        }
        Rate::Bitrate(k) => vec![
            "-rc_mode".into(),
            "CBR".into(),
            "-b:v".into(),
            format!("{k}k"),
        ],
        Rate::Vbr { kbps, max_kbps } => vec![
            "-rc_mode".into(),
            "VBR".into(),
            "-b:v".into(),
            format!("{kbps}k"),
            "-maxrate".into(),
            format!("{max_kbps}k"),
        ],
    };
    args.extend(rate);
    args
}

fn decoded_psnr(path: &Path, frames: &[Frame], w: u32, h: u32) -> Vec<f64> {
    let decoded = path.with_extension("yuv");
    run(&[
        "-i",
        path.to_str().unwrap(),
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        decoded.to_str().unwrap(),
    ]);
    let data = std::fs::read(&decoded).unwrap();
    let luma = (w * h) as usize;
    data.chunks_exact(luma * 3 / 2)
        .zip(frames)
        .map(|(d, f)| plane_psnr(&d[..luma], &f.planes[0].data))
        .collect()
}

struct Side {
    bytes: u64,
    db: Vec<f64>,
}

impl Side {
    fn mean(&self, range: std::ops::Range<usize>) -> f64 {
        let s = &self.db[range.start.min(self.db.len())..range.end.min(self.db.len())];
        s.iter().sum::<f64>() / s.len().max(1) as f64
    }

    fn summary(&self) -> String {
        let min = self.db.iter().copied().fold(f64::INFINITY, f64::min);
        let steps = self
            .db
            .iter()
            .step_by((self.db.len() / 12).max(1))
            .map(|d| format!("{d:.1}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "{:>9} B  mean {:.2} min {min:.2} | {steps}",
            self.bytes,
            self.mean(0..self.db.len())
        )
    }
}

fn ours_and_ffmpeg(
    codec: Codec,
    config: &EncoderConfig,
    frames: &[Frame],
    raw: &Path,
    name: &str,
) -> Option<(Side, Side)> {
    let node = render_node()?;
    let (w, h) = (config.width, config.height);
    let ext = if codec == Codec::Hevc { "hevc" } else { "h264" };
    let (packets, extradata, _) = encode_all(codec, config, frames);
    let ours = dir().join(format!("{name}-ours.{ext}"));
    std::fs::write(&ours, annexb_stream(codec, &packets, &extradata)).unwrap();
    let theirs = dir().join(format!("{name}-ffmpeg.{ext}"));
    let size = format!("{w}x{h}");
    let rate = format!("{}/{}", config.fps.0, config.fps.1);
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-vaapi_device",
        &node,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        &size,
        "-framerate",
        &rate,
        "-i",
        raw.to_str().unwrap(),
        "-vf",
        "format=nv12,hwupload",
    ]
    .map(String::from)
    .to_vec();
    args.extend(ffmpeg_vaapi_args(
        codec,
        config.rate,
        config.gop,
        config.b_frames,
    ));
    args.push(theirs.to_str().unwrap().into());
    let ok = Command::new("ffmpeg")
        .args(&args)
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipped: ffmpeg has no working {ext}_vaapi encoder");
        return None;
    }
    let bytes = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    Some((
        Side {
            bytes: bytes(&ours),
            db: decoded_psnr(&ours, frames, w, h),
        },
        Side {
            bytes: bytes(&theirs),
            db: decoded_psnr(&theirs, frames, w, h),
        },
    ))
}

#[test]
fn per_frame_quality_matches_ffmpeg_vaapi() {
    if !ffmpeg() {
        return;
    }
    let (w, h, n) = (640, 360, 120);
    let input = format!(
        "mandelbrot=size={w}x{h}:rate=30:end_scale=0.01,format=yuv420p[a];testsrc2=size={w}x{h}:rate=30[b];[a][b]blend=all_mode=average,noise=alls=3:allf=t"
    );
    let frames = source_frames_from(&input, "-moving", w, h, n, PixelFormat::Yuv420p8);
    let raw = dir().join(format!("source-moving-{w}x{h}-{n}-yuv420p.yuv"));
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
            };
            let name = format!("match-{codec:?}-{rate:?}");
            let Some((ours, theirs)) = ours_and_ffmpeg(codec, &config, &frames, &raw, &name) else {
                return;
            };
            eprintln!(
                "{codec:?} {rate:?}\n  ours   {}\n  ffmpeg {}",
                ours.summary(),
                theirs.summary()
            );
            assert_eq!(ours.db.len(), n, "{name}: frames decoded");
            if let Rate::Bitrate(kbps) = rate {
                let target = f64::from(kbps) * 1000.0 / 8.0 * n as f64 / 30.0;
                let off = ours.bytes as f64 / target - 1.0;
                assert!(
                    off.abs() < 0.1,
                    "{name}: {:.1} % off the target size",
                    off * 100.0
                );
                for (what, range) in [("first 10 frames", 0..10), ("all frames", 0..n)] {
                    let (a, b) = (ours.mean(range.clone()), theirs.mean(range));
                    assert!(
                        a > b - 0.3,
                        "{name}: {what} at {a:.2} dB against ffmpeg's {b:.2}"
                    );
                }
            } else {
                for (i, (a, b)) in ours.db.iter().zip(&theirs.db).enumerate() {
                    assert!(
                        (a - b).abs() < 0.3,
                        "{name}: frame {i} at {a:.2} dB against ffmpeg's {b:.2}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore]
fn quality_against_ffmpeg_vaapi() {
    let sample = std::env::var("PITO_GPU_SAMPLE").unwrap_or_default();
    if sample.is_empty() || !ffmpeg() {
        return;
    }
    let size = std::env::var("PITO_GPU_SIZE").unwrap_or_else(|_| "1920x1080".into());
    let (w, h) = size
        .split_once('x')
        .map(|(a, b)| (a.parse().unwrap(), b.parse().unwrap()))
        .unwrap();
    let n: usize = std::env::var("PITO_GPU_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let kbps = std::env::var("PITO_GPU_KBPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8000u32);
    let input = format!("movie={sample},scale={w}:{h}:flags=lanczos,fps=60");
    let tag: String = format!("-q{sample}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let frames = source_frames_from(&input, &tag, w, h, n, PixelFormat::Yuv420p8);
    let raw = dir().join(format!("source{tag}-{w}x{h}-{n}-yuv420p.yuv"));
    for codec in [Codec::H264, Codec::Hevc] {
        if !has(codec, Direction::Encode) {
            continue;
        }
        for rate in [
            Rate::Qp(23),
            Rate::Bitrate(kbps),
            Rate::Vbr {
                kbps,
                max_kbps: kbps * 3 / 2,
            },
        ] {
            let config = EncoderConfig {
                width: w,
                height: h,
                fps: (60, 1),
                profile: if codec == Codec::Hevc {
                    Profile::Main
                } else {
                    Profile::High
                },
                rate,
                gop: 250,
                b_frames: 0,
                bitstream: Bitstream::LengthPrefixed,
            };
            let name = format!("q-{codec:?}-{rate:?}");
            let Some((ours, theirs)) = ours_and_ffmpeg(codec, &config, &frames, &raw, &name) else {
                return;
            };
            eprintln!(
                "{codec:?} {rate:?}\n  ours   {}\n  ffmpeg {}",
                ours.summary(),
                theirs.summary()
            );
        }
    }
}

#[test]
#[ignore]
fn speed() {
    let sample = std::env::var("PITO_GPU_SAMPLE").unwrap_or_default();
    let mut report = String::new();
    if !sample.is_empty() && has(Codec::H264, Direction::Decode) {
        let path = dir().join("sample.h264");
        if !path.exists() {
            run(&[
                "-i",
                &sample,
                "-an",
                "-c:v",
                "copy",
                "-bsf:v",
                "h264_mp4toannexb",
                "-f",
                "h264",
                path.to_str().unwrap(),
            ]);
        }
        decode_file(Codec::H264, &path, 60);
        let (frames, fps) = decode_file(Codec::H264, &path, 600);
        report.push_str(&format!(
            "decode H.264 {}x{}: {fps:.0} fps ({} frames)\n",
            frames[0].width,
            frames[0].height,
            frames.len()
        ));
        let small = dir().join("sample-1080.h264");
        if !small.exists() {
            run(&[
                "-i",
                &sample,
                "-an",
                "-t",
                "20",
                "-vf",
                "scale=1920:1080",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "18",
                "-bsf:v",
                "h264_mp4toannexb",
                "-f",
                "h264",
                small.to_str().unwrap(),
            ]);
        }
        gpu_frames(Codec::H264, &path, 60);
        let (_, fps) = gpu_frames(Codec::H264, &path, 600);
        report.push_str(&format!(
            "decode H.264 2560x1440 to GPU surfaces, no read-back: {fps:.0} fps\n"
        ));
        let (frames, fps) = decode_file(Codec::H264, &small, 600);
        report.push_str(&format!(
            "decode H.264 1920x1080: {fps:.0} fps ({} frames)\n",
            frames.len()
        ));
        let hevc = dir().join("sample.hevc");
        if !hevc.exists() {
            run(&[
                "-i",
                &sample,
                "-an",
                "-t",
                "20",
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-x265-params",
                "log-level=error",
                "-crf",
                "20",
                "-f",
                "hevc",
                hevc.to_str().unwrap(),
            ]);
        }
        let (frames, fps) = decode_file(Codec::Hevc, &hevc, 600);
        report.push_str(&format!(
            "decode HEVC 2560x1440: {fps:.0} fps ({} frames)\n",
            frames.len()
        ));
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
            };
            let (packets, _, fps) = transcode(Codec::H264, &path, dst, &config, 600);
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
