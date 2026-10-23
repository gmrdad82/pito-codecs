use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub(super) fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/h264");
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub(super) fn ffmpeg_present() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("ffmpeg not found, skipping");
    }
    ok
}

fn adoption() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/sample/adoption.mp4");
    if p.exists() {
        Some(p)
    } else {
        eprintln!("{} missing, skipping", p.display());
        None
    }
}

fn run(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub(super) fn ffmpeg_yuv(input: &Path) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(input)
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

fn x264(name: &str, size: &str, frames: u32, opts: &[&str]) -> PathBuf {
    let path = dir().join(format!("{name}.h264"));
    let src = format!("testsrc2=size={size}:rate=25,noise=alls=12:allf=t");
    let n = frames.to_string();
    let mut args = vec![
        "-f",
        "lavfi",
        "-i",
        &src,
        "-frames:v",
        &n,
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
    ];
    args.extend_from_slice(opts);
    let p = path.to_str().unwrap();
    args.push(p);
    run(&args);
    path
}

pub(super) fn raw(frames: &[Frame]) -> Vec<u8> {
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

fn decode_annexb(path: &Path, threads: usize) -> Vec<Frame> {
    let data = std::fs::read(path).unwrap();
    let mut dec = Decoder::with_threads(threads);
    let mut frames = Vec::new();
    for (i, au) in access_units(&data).into_iter().enumerate() {
        let packet = Packet {
            data: au.to_vec(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        };
        frames.extend(dec.decode(&packet).unwrap());
    }
    frames.extend(dec.flush().unwrap());
    frames
}

fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    let mut pending_params = None;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let at = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            let t = data[i + 3] & 0x1f;
            let first_mb_zero = data.get(i + 4).is_some_and(|b| b & 0x80 != 0);
            match t {
                6..=9 if pending_params.is_none() => pending_params = Some(at),
                1 | 5 if first_mb_zero => {
                    starts.push(pending_params.take().unwrap_or(at));
                }
                1 | 5 => {}
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

pub(super) fn assert_same(ours: &[u8], theirs: &[u8], frame_size: usize, what: &str) {
    assert_eq!(
        ours.len() / frame_size,
        theirs.len() / frame_size,
        "{what}: frame count"
    );
    if let Some(i) = ours.iter().zip(theirs).position(|(a, b)| a != b) {
        panic!(
            "{what}: first difference in frame {} at byte {}",
            i / frame_size,
            i % frame_size
        );
    }
}

#[test]
fn decode_is_bit_exact_with_ffmpeg() {
    if !ffmpeg_present() {
        return;
    }
    let streams = [
        (
            "baseline",
            "352x288",
            vec!["-profile:v", "baseline", "-g", "15"],
        ),
        (
            "main_cabac_b",
            "352x288",
            vec!["-profile:v", "main", "-bf", "3", "-refs", "3"],
        ),
        (
            "high_bpyramid",
            "640x360",
            vec![
                "-profile:v",
                "high",
                "-bf",
                "3",
                "-x264-params",
                "b-pyramid=normal:weightp=2:direct=temporal:ref=4",
            ],
        ),
        (
            "high_crop_slices",
            "638x358",
            vec![
                "-profile:v",
                "high",
                "-bf",
                "2",
                "-slices",
                "4",
                "-x264-params",
                "cqm=jvt",
            ],
        ),
    ];
    for (name, size, opts) in streams {
        let path = x264(name, size, 24, &opts);
        let theirs = ffmpeg_yuv(&path);
        let (w, h) = size
            .split_once('x')
            .map(|(w, h)| (w.parse::<usize>().unwrap(), h.parse::<usize>().unwrap()))
            .unwrap();
        let frame_size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        for threads in [1, 4] {
            let ours = raw(&decode_annexb(&path, threads));
            assert_same(
                &ours,
                &theirs,
                frame_size,
                &format!("{name} with {threads} threads"),
            );
        }
    }
}

#[test]
fn adoption_first_two_seconds_are_bit_exact() {
    let (true, Some(src)) = (ffmpeg_present(), adoption()) else {
        return;
    };
    let path = dir().join("adoption_2s.h264");
    run(&[
        "-i",
        src.to_str().unwrap(),
        "-t",
        "2",
        "-an",
        "-c:v",
        "copy",
        "-bsf:v",
        "h264_mp4toannexb",
        path.to_str().unwrap(),
    ]);
    let theirs = ffmpeg_yuv(&path);
    let ours = raw(&decode_annexb(&path, default_threads()));
    assert_same(&ours, &theirs, 2560 * 1440 * 3 / 2, "adoption");
    assert!(ours.len() / (2560 * 1440 * 3 / 2) >= 100);
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16)
}

fn source_frames(src: &Path, w: u32, h: u32, frames: u32) -> Vec<Frame> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(src)
        .args([
            "-frames:v",
            &frames.to_string(),
            "-vf",
            &format!("scale={w}:{h}"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ws, hs) = (w as usize, h as usize);
    let (cw, ch) = (ws / 2, hs / 2);
    out.stdout
        .chunks_exact(ws * hs + 2 * cw * ch)
        .enumerate()
        .map(|(i, b)| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 30));
            f.planes[0].data.copy_from_slice(&b[..ws * hs]);
            f.planes[1]
                .data
                .copy_from_slice(&b[ws * hs..ws * hs + cw * ch]);
            f.planes[2].data.copy_from_slice(&b[ws * hs + cw * ch..]);
            f
        })
        .collect()
}

pub(super) fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    let mse = se / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

pub(super) fn to_annexb(avcc: &[u8], packets: &[Packet]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 6;
    for _ in 0..2 {
        let count = if pos == 6 {
            avcc[5] & 0x1f
        } else {
            avcc[pos - 1]
        };
        for _ in 0..count {
            let len = u16::from_be_bytes([avcc[pos], avcc[pos + 1]]) as usize;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&avcc[pos + 2..pos + 2 + len]);
            pos += 2 + len;
        }
        pos += 1;
    }
    for p in packets {
        let mut i = 0;
        while i + 4 <= p.data.len() {
            let len = u32::from_be_bytes(p.data[i..i + 4].try_into().unwrap()) as usize;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&p.data[i + 4..i + 4 + len]);
            i += 4 + len;
        }
    }
    out
}

#[test]
fn crf_18_encode_plays_in_ffmpeg_and_decodes_back() {
    if !ffmpeg_present() {
        return;
    }
    let (w, h) = (640, 360);
    let src = match adoption() {
        Some(p) => p,
        None => {
            let p = dir().join("encode_src.mp4");
            run(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=640x360:rate=30",
                "-frames:v",
                "30",
                "-pix_fmt",
                "yuv420p",
                p.to_str().unwrap(),
            ]);
            p
        }
    };
    let frames = source_frames(&src, w, h, 30);
    let config = EncoderConfig {
        width: w,
        height: h,
        fps: (30, 1),
        rate: crate::media::Rate::Crf(18),
        gop: 15,
        b_frames: 2,
        ..EncoderConfig::default()
    };
    let mut enc = Encoder::new(config.clone()).unwrap();
    let mut packets = Vec::new();
    for f in &frames {
        packets.extend(enc.encode(f).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), frames.len());
    let mut one = Encoder::new(EncoderConfig {
        threads: 1,
        ..config.clone()
    })
    .unwrap();
    let mut single = Vec::new();
    for f in &frames {
        single.extend(one.encode(f).unwrap());
    }
    single.extend(one.flush().unwrap());
    assert!(
        packets
            .iter()
            .map(|p| &p.data)
            .eq(single.iter().map(|p| &p.data)),
        "frame threads change the bitstream"
    );
    assert!(packets[0].keyframe);
    assert!(
        packets.windows(2).all(|p| p[0].dts < p[1].dts),
        "dts strictly increasing"
    );
    assert!(
        packets.iter().all(|p| p.dts <= p.pts),
        "dts never after pts"
    );
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    pts.sort();
    assert_eq!(pts, (0..frames.len() as i64).collect::<Vec<_>>());

    let path = dir().join("encode_crf18.h264");
    std::fs::write(&path, to_annexb(&enc.avcc(), &packets)).unwrap();
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        probe.status.success() && probe.stderr.is_empty(),
        "ffprobe: {}",
        String::from_utf8_lossy(&probe.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        format!("h264,{w},{h},{}", frames.len())
    );

    let theirs = ffmpeg_yuv(&path);
    let source = raw(&frames);
    let db = psnr(&theirs, &source);
    assert!(db > 40.0, "PSNR {db:.2} dB at CRF 18");

    let mut dec = Decoder::with_extradata(&enc.avcc()).unwrap();
    let mut ours = Vec::new();
    for p in &packets {
        ours.extend(dec.decode(p).unwrap());
    }
    ours.extend(dec.flush().unwrap());
    assert_same(
        &raw(&ours),
        &theirs,
        (w * h * 3 / 2) as usize,
        "our encode, decoded",
    );
}

fn bench_decode(path: &Path, label: &str) {
    let data = std::fs::read(path).unwrap();
    let aus = access_units(&data);
    let packets: Vec<Packet> = aus
        .iter()
        .enumerate()
        .map(|(i, a)| Packet {
            data: a.to_vec(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        })
        .collect();
    let threads: Vec<usize> = match std::env::var("BENCH_THREADS") {
        Ok(v) => v.split(',').map(|t| t.parse().unwrap()).collect(),
        Err(_) => vec![1, default_threads()],
    };
    for threads in threads {
        let mut dec = Decoder::with_threads(threads);
        let t = Instant::now();
        let mut n = 0;
        for p in &packets {
            n += dec.decode(p).unwrap().len();
        }
        n += dec.flush().unwrap().len();
        let s = t.elapsed().as_secs_f64();
        println!(
            "decode {label} threads {threads}: {n} frames in {s:.2} s, {:.1} fps",
            n as f64 / s
        );
    }
}

fn bench_files(src: &Path) -> (PathBuf, PathBuf) {
    let s = src.to_str().unwrap();
    let p1440 = dir().join("bench_1440.h264");
    crate::fixture::once(&p1440, |part| {
        run(&[
            "-i",
            s,
            "-t",
            "10",
            "-an",
            "-c:v",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
            part.to_str().unwrap(),
        ]);
    });
    let p1080 = dir().join("bench_1080.h264");
    crate::fixture::once(&p1080, |part| {
        run(&[
            "-i",
            s,
            "-t",
            "10",
            "-an",
            "-vf",
            "scale=1920:1080",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "20",
            part.to_str().unwrap(),
        ]);
    });
    (p1080, p1440)
}

#[test]
#[ignore]
fn bench_decoder() {
    let (true, Some(src)) = (ffmpeg_present(), adoption()) else {
        return;
    };
    let (p1080, p1440) = bench_files(&src);
    let only = std::env::var("BENCH_SIZE").unwrap_or_default();
    if only != "1440" {
        bench_decode(&p1080, "1080p");
    }
    if only != "1080" {
        bench_decode(&p1440, "1440p");
    }
}

#[test]
#[ignore]
fn bench_encoder() {
    let (true, Some(src)) = (ffmpeg_present(), adoption()) else {
        return;
    };
    let frames = source_frames(&src, 1920, 1080, 120);
    for (preset, b) in [(Preset::Speed, 2), (Preset::Balanced, 2)] {
        let config = EncoderConfig {
            width: 1920,
            height: 1080,
            fps: (60, 1),
            preset,
            rate: crate::media::Rate::Crf(18),
            b_frames: b,
            ..EncoderConfig::default()
        };
        let mut enc = Encoder::new(config.clone()).unwrap();
        let t = Instant::now();
        let mut bytes = 0;
        for f in &frames {
            bytes += enc
                .encode(f)
                .unwrap()
                .iter()
                .map(|p| p.data.len())
                .sum::<usize>();
        }
        bytes += enc
            .flush()
            .unwrap()
            .iter()
            .map(|p| p.data.len())
            .sum::<usize>();
        let s = t.elapsed().as_secs_f64();
        println!(
            "encode 1080p CRF 18 {preset:?}: {} frames in {s:.2} s, {:.1} fps, {:.1} Mbit/s",
            frames.len(),
            frames.len() as f64 / s,
            bytes as f64 * 8.0 * 60.0 / frames.len() as f64 / 1e6
        );
    }
}

#[test]
fn corrupt_streams_never_panic() {
    if !ffmpeg_present() {
        return;
    }
    let streams = [
        x264(
            "fuzz_cavlc",
            "176x144",
            10,
            &["-profile:v", "baseline", "-g", "5", "-slices", "2"],
        ),
        x264(
            "fuzz_cabac",
            "176x144",
            10,
            &[
                "-profile:v",
                "high",
                "-bf",
                "2",
                "-g",
                "5",
                "-x264-params",
                "ref=3:weightp=2:direct=temporal",
            ],
        ),
    ];
    let me = std::thread::current().id();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let t = std::thread::current();
        if t.id() == me || t.name().is_some_and(|n| n.starts_with("h264-")) {
            if let Ok(mut l) = log.lock() {
                l.push(info.to_string());
            }
        } else {
            prev(info);
        }
    }));
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as usize % n.max(1)
    };
    for path in &streams {
        let data = std::fs::read(path).unwrap();
        for round in 0..300 {
            let mut d = data.clone();
            match round % 4 {
                0 => {
                    for _ in 0..1 + next(8) {
                        let i = 4 + next(d.len() - 4);
                        d[i] ^= 1 << next(8);
                    }
                }
                1 => d.truncate(4 + next(d.len() - 4)),
                2 => {
                    let i = 4 + next(d.len() - 4);
                    let n = next(64).min(d.len() - i);
                    d[i..i + n].fill(next(256) as u8);
                }
                _ => {
                    let i = 4 + next(d.len() - 4);
                    let n = next(400).min(d.len() - i);
                    d.drain(i..i + n);
                }
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut dec = Decoder::with_threads(1 + round % 2 * 3);
                for (i, au) in access_units(&d).into_iter().enumerate() {
                    let _ = dec.decode(&Packet {
                        data: au.to_vec(),
                        pts: i as i64,
                        dts: i as i64,
                        keyframe: false,
                    });
                }
                let _ = dec.flush();
            }));
        }
    }
    drop(std::panic::take_hook());
    let seen = seen.lock().unwrap();
    let mut kinds: Vec<&str> = seen.iter().map(|s| s.as_str()).collect();
    kinds.sort();
    kinds.dedup();
    assert!(
        seen.is_empty(),
        "{} panics:\n{}",
        seen.len(),
        kinds.join("\n")
    );
}

#[test]
fn tiny_and_odd_sizes_encode_without_crashing() {
    for (w, h) in [(2, 2), (18, 34), (34, 18), (66, 2), (2, 66)] {
        for threads in [1, 4] {
            let mut enc = Encoder::new(EncoderConfig {
                width: w,
                height: h,
                gop: 4,
                b_frames: 2,
                threads,
                ..EncoderConfig::default()
            })
            .unwrap();
            let mut n = 0;
            for i in 0..7i64 {
                let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i, TimeBase::new(1, 30));
                for (k, p) in f.planes.iter_mut().enumerate() {
                    for (j, b) in p.data.iter_mut().enumerate() {
                        *b = ((j as i64 * 7 + i * 13 + k as i64 * 50) % 251) as u8;
                    }
                }
                n += enc.encode(&f).unwrap().len();
            }
            n += enc.flush().unwrap().len();
            assert_eq!(n, 7, "{w}x{h} with {threads} threads");
        }
    }
    for (w, h) in [(3, 2), (2, 3), (0, 16), (16, 0), (16386, 16)] {
        assert!(
            Encoder::new(EncoderConfig {
                width: w,
                height: h,
                ..EncoderConfig::default()
            })
            .is_err()
        );
    }
}

fn crop_sps(sps: &[u8], crop: (u32, u32, u32, u32)) -> Vec<u8> {
    use crate::syntax::bits::{BitReader, BitWriter, escape_rbsp, unescape_rbsp};
    let rbsp = unescape_rbsp(&sps[1..]);
    let end = rbsp
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| i * 8 + 7 - rbsp[i].trailing_zeros() as usize)
        .unwrap();
    let mut r = BitReader::new(&rbsp);
    let mut w = BitWriter::new();
    assert_eq!(rbsp[0], 66);
    w.write_bits(r.read_bits(24).unwrap(), 24);
    for _ in 0..5 {
        w.write_ue(r.read_ue().unwrap());
    }
    w.write_bit(r.read_flag().unwrap());
    w.write_ue(r.read_ue().unwrap());
    w.write_ue(r.read_ue().unwrap());
    assert!(r.read_flag().unwrap());
    w.write_bit(true);
    w.write_bit(r.read_flag().unwrap());
    if r.read_flag().unwrap() {
        for _ in 0..4 {
            r.read_ue().unwrap();
        }
    }
    w.write_bit(true);
    for v in [crop.0, crop.1, crop.2, crop.3] {
        w.write_ue(v);
    }
    while r.position() < end {
        w.write_bit(r.read_bit().unwrap());
    }
    w.rbsp_trailing();
    let mut out = vec![sps[0]];
    out.extend(escape_rbsp(&w.finish()));
    out
}

pub(crate) fn far_cropped_stream() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (w, h) = (64, 64);
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        profile: Profile::Baseline,
        gop: 1,
        b_frames: 0,
        threads: 1,
        bitstream: media::Bitstream::AnnexB,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, 0, TimeBase::new(1, 30));
    for p in f.planes.iter_mut() {
        p.data.fill(128);
    }
    let mut packets = enc.encode(&f).unwrap();
    packets.extend(enc.flush().unwrap());
    let sps = crop_sps(&enc.sps(), (0, 0, 0, 0));
    let crafted = crop_sps(&enc.sps(), (2_147_483_649, 2_147_483_647, 0, 0));
    let pps = enc.pps();
    let build = |sps: &[u8]| {
        let mut out = Vec::new();
        for nal in [sps, &pps[..]] {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        for p in &packets {
            for nal in crate::syntax::bits::annexb_nals(&p.data) {
                if !matches!(nal.first().map(|b| b & 0x1f), Some(7 | 8)) {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(nal);
                }
            }
        }
        out
    };
    (build(&sps), build(&crafted), crafted, pps)
}

#[test]
fn crop_offsets_past_the_picture_are_refused() {
    let (plain, crafted, _, _) = far_cropped_stream();
    for threads in [1, 4] {
        let mut dec = Decoder::with_threads(threads);
        let mut n = dec
            .decode(&Packet {
                data: plain.clone(),
                pts: 0,
                dts: 0,
                keyframe: true,
            })
            .unwrap()
            .len();
        n += dec.flush().unwrap().len();
        assert_eq!(n, 1);
        let mut dec = Decoder::with_threads(threads);
        let got = dec.decode(&Packet {
            data: crafted.clone(),
            pts: 0,
            dts: 0,
            keyframe: true,
        });
        assert!(got.is_err(), "{threads} threads");
        assert!(dec.flush().unwrap_or_default().is_empty());
    }
}

#[test]
fn sps_vui_carries_the_given_colour_tags() {
    use crate::syntax::bits::unescape_rbsp;
    use crate::syntax::h264::params::Sps;
    let vui = |color: Option<crate::media::color::Color>| {
        let mut config = EncoderConfig {
            width: 64,
            height: 48,
            ..EncoderConfig::default()
        };
        if let Some(c) = color {
            config.color = c;
        }
        let sps = Encoder::new(config).unwrap().sps();
        Sps::parse(&unescape_rbsp(&sps[1..])).unwrap().vui.unwrap()
    };
    let tags = |v: &crate::syntax::h264::params::Vui| {
        (
            v.colour_primaries,
            v.transfer_characteristics,
            v.matrix_coefficients,
            v.full_range,
        )
    };
    assert_eq!(tags(&vui(None)), (1, 1, 1, false));
    for c in [
        crate::media::color::Color {
            primaries: 1,
            transfer: 1,
            matrix: 6,
            full_range: false,
        },
        crate::media::color::Color {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            full_range: true,
        },
    ] {
        let v = vui(Some(c));
        assert_eq!(tags(&v), (c.primaries, c.transfer, c.matrix, c.full_range));
        assert_eq!(v.video_format, 5);
        assert!(v.timing.is_some() && v.bitstream_restriction.is_some());
    }
}

#[test]
fn encoded_frames_carry_the_colour_tags_back_out() {
    let cases = [
        (Color::BT709, ["tv", "bt709", "bt709", "bt709"]),
        (
            Color {
                primaries: 6,
                transfer: 6,
                matrix: 6,
                full_range: false,
            },
            ["tv", "smpte170m", "smpte170m", "smpte170m"],
        ),
        (
            Color {
                primaries: 1,
                transfer: 1,
                matrix: 1,
                full_range: true,
            },
            ["pc", "bt709", "bt709", "bt709"],
        ),
        (
            Color {
                primaries: 5,
                transfer: 6,
                matrix: 5,
                full_range: true,
            },
            ["pc", "bt470bg", "smpte170m", "bt470bg"],
        ),
    ];
    let probe = Command::new("ffprobe").arg("-version").output().is_ok();
    if !probe {
        eprintln!("ffprobe not found, skipping its half");
    }
    for (n, (color, names)) in cases.into_iter().enumerate() {
        let mut enc = Encoder::new(EncoderConfig {
            width: 64,
            height: 48,
            gop: 4,
            color,
            ..EncoderConfig::default()
        })
        .unwrap();
        let mut packets = Vec::new();
        for i in 0..6u8 {
            let mut f = Frame::new(
                PixelFormat::Yuv420p8,
                64,
                48,
                i64::from(i),
                TimeBase::new(1, 30),
            );
            f.planes[0].data.fill(16 + i * 30);
            packets.extend(enc.encode(&f).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        assert_eq!(packets.len(), 6);
        let stream = to_annexb(&enc.avcc(), &packets);
        let starts: Vec<usize> = stream
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == [0, 0, 1])
            .map(|(i, _)| i + 3)
            .collect();
        let sps = starts
            .iter()
            .copied()
            .find(|&at| stream[at] & 0x1f == 7)
            .unwrap();
        let end = starts
            .iter()
            .copied()
            .find(|&at| at > sps)
            .map_or(stream.len(), |at| at - 3);
        let vui = crate::syntax::h264::params::Sps::parse(&crate::syntax::bits::unescape_rbsp(
            &stream[sps + 1..end],
        ))
        .unwrap()
        .vui
        .unwrap();
        assert_eq!(
            (
                vui.colour_primaries,
                vui.transfer_characteristics,
                vui.matrix_coefficients,
                vui.full_range
            ),
            (
                color.primaries,
                color.transfer,
                color.matrix,
                color.full_range
            )
        );
        let mut dec = Decoder::with_extradata(&enc.avcc()).unwrap();
        let mut shown = 0;
        for p in &packets {
            shown += dec.decode(p).unwrap().len();
        }
        shown += dec.flush().unwrap().len();
        assert_eq!(shown, 6);
        assert_eq!(
            dec.color(),
            Some(ColorInfo {
                full_range: color.full_range,
                primaries: color.primaries,
                transfer: color.transfer,
                matrix: color.matrix,
            })
        );
        if !probe {
            continue;
        }
        let path = dir().join(format!("colour-{n}.h264"));
        std::fs::write(&path, &stream).unwrap();
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_range,color_primaries,color_transfer,color_space",
                "-of",
                "default=nw=1",
            ])
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let tag = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            [
                tag("color_range"),
                tag("color_primaries"),
                tag("color_transfer"),
                tag("color_space")
            ],
            names.map(String::from),
            "{text}"
        );
    }
}
