use super::{Decoder, Picture, is_keyframe, keyframe_info};
use crate::media::{Frame, Packet, PixelFormat};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Fixture {
    name: &'static str,
    source: &'static str,
    size: (u32, u32),
    frames: u32,
    filter: &'static str,
    pix_fmt: &'static str,
    args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=10:allf=t+u";

const fn fx(
    name: &'static str,
    source: &'static str,
    size: (u32, u32),
    frames: u32,
    filter: &'static str,
    pix_fmt: &'static str,
    args: &'static [&'static str],
) -> Fixture {
    Fixture {
        name,
        source,
        size,
        frames,
        filter,
        pix_fmt,
        args,
    }
}

const CIF: (u32, u32) = (352, 288);
const QCIF: (u32, u32) = (176, 144);

const CONFORMANCE: &[Fixture] = &[
    fx(
        "good",
        "testsrc2",
        CIF,
        20,
        NOISE,
        "yuv420p",
        &["-b:v", "800k", "-frame-parallel", "0"],
    ),
    fx(
        "altref_10bit",
        "mandelbrot",
        CIF,
        24,
        NOISE,
        "yuv420p10le",
        &[
            "2pass",
            "-b:v",
            "600k",
            "-profile:v",
            "2",
            "-auto-alt-ref",
            "1",
            "-lag-in-frames",
            "16",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "tiles_720p",
        "testsrc2",
        (1280, 720),
        8,
        NOISE,
        "yuv420p",
        &[
            "-b:v",
            "2M",
            "-tile-columns",
            "2",
            "-tile-rows",
            "1",
            "-speed",
            "4",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "profile1_444",
        "testsrc2",
        QCIF,
        8,
        NOISE,
        "yuv444p",
        &["-b:v", "500k", "-profile:v", "1", "-frame-parallel", "0"],
    ),
    fx(
        "profile3_422_12bit",
        "mandelbrot",
        QCIF,
        6,
        NOISE,
        "yuv422p12le",
        &["-b:v", "500k", "-profile:v", "3", "-frame-parallel", "0"],
    ),
    fx(
        "lossless",
        "testsrc2",
        QCIF,
        5,
        NOISE,
        "yuv420p",
        &["-lossless", "1", "-frame-parallel", "0"],
    ),
    fx(
        "odd_aq",
        "testsrc2",
        (347, 251),
        12,
        NOISE,
        "yuv420p",
        &["-b:v", "600k", "-aq-mode", "3", "-frame-parallel", "0"],
    ),
    fx(
        "realtime_layers",
        "testsrc2",
        CIF,
        20,
        NOISE,
        "yuv420p",
        &[
            "-b:v",
            "800k",
            "-speed",
            "8",
            "-deadline",
            "realtime",
            "-error-resilient",
            "1",
            "-ts-parameters",
            "ts_number_layers=2:ts_target_bitrate=400,800:ts_rate_decimator=2,1:ts_periodicity=2:ts_layer_id=0,1:ts_layering_mode=2",
        ],
    ),
];

const BENCH: &[Fixture] = &[
    fx(
        "bench_1080p_hq",
        "testsrc2",
        (1920, 1080),
        60,
        "noise=alls=3:allf=t",
        "yuv420p",
        &[
            "-crf",
            "12",
            "-b:v",
            "0",
            "-speed",
            "4",
            "-tile-columns",
            "2",
            "-row-mt",
            "1",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "bench_1080p",
        "testsrc2",
        (1920, 1080),
        120,
        "noise=alls=3:allf=t",
        "yuv420p",
        &[
            "-crf",
            "32",
            "-b:v",
            "0",
            "-speed",
            "4",
            "-tile-columns",
            "2",
            "-row-mt",
            "1",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "bench_1440p",
        "testsrc2",
        (2560, 1440),
        90,
        "noise=alls=3:allf=t",
        "yuv420p",
        &[
            "-crf",
            "32",
            "-b:v",
            "0",
            "-speed",
            "4",
            "-tile-columns",
            "3",
            "-row-mt",
            "1",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "bench_1440p_10bit",
        "testsrc2",
        (2560, 1440),
        60,
        "noise=alls=3:allf=t",
        "yuv420p10le",
        &[
            "-crf",
            "32",
            "-b:v",
            "0",
            "-profile:v",
            "2",
            "-speed",
            "4",
            "-tile-columns",
            "3",
            "-row-mt",
            "1",
            "-frame-parallel",
            "0",
        ],
    ),
];

fn ffmpeg(context: &str) -> bool {
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains("libvpx-vp9")
        });
    if !ok {
        eprintln!("SKIPPED ({context}): ffmpeg with libvpx-vp9 not found");
    }
    ok
}

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fixtures/vp9");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn scratch(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    dir().join(format!(
        ".{}.{}.{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn ff(args: &[&str]) {
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

fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    if !ffmpeg(f.name) {
        return None;
    }
    let ivf = dir().join(format!("{}.ivf", f.name));
    let yuv = dir().join(format!("{}.yuv", f.name));
    if !ivf.exists() {
        let src = format!(
            "{}=size={}x{}:rate=25,format=yuv444p,{},format={}",
            f.source, f.size.0, f.size.1, f.filter, f.pix_fmt
        );
        let frames = f.frames.to_string();
        let two_pass = f.args.first() == Some(&"2pass");
        let args: Vec<&str> = f.args.iter().copied().filter(|a| *a != "2pass").collect();
        let tmp = scratch(&format!("{}.ivf", f.name));
        let tmp_s = tmp.to_str().unwrap();
        let log = scratch(&format!("{}.log", f.name));
        let log_s = log.to_str().unwrap();
        let mut base = vec![
            "-f",
            "lavfi",
            "-i",
            &src,
            "-frames:v",
            &frames,
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            f.pix_fmt,
        ];
        base.extend(&args);
        if two_pass {
            let mut first = base.clone();
            first.extend(["-pass", "1", "-passlogfile", log_s, "-f", "null", "-"]);
            ff(&first);
            base.extend(["-pass", "2", "-passlogfile", log_s]);
        }
        base.extend(["-f", "ivf", tmp_s]);
        ff(&base);
        let _ = std::fs::remove_file(format!("{log_s}-0.log"));
        std::fs::rename(&tmp, &ivf).unwrap();
    }
    if !yuv.exists() {
        let tmp = scratch(&format!("{}.yuv", f.name));
        ff(&[
            "-c:v",
            "vp9",
            "-i",
            ivf.to_str().unwrap(),
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            f.pix_fmt,
            tmp.to_str().unwrap(),
        ]);
        std::fs::rename(&tmp, &yuv).unwrap();
    }
    Some((ivf, yuv))
}

fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let mut pos = u16::from_le_bytes([data[6], data[7]]) as usize;
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
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

fn mismatch(name: &str, i: usize, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{name}: frame {i} size");
    if got != want {
        let k = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(0);
        panic!(
            "{name}: frame {i} differs from ffmpeg at byte {k} of {}",
            got.len()
        );
    }
}

fn is_420(f: &Fixture) -> bool {
    matches!(f.pix_fmt, "yuv420p" | "yuv420p10le")
}

fn frame_bytes(f: &Fixture) -> usize {
    let (w, h) = (f.size.0 as usize, f.size.1 as usize);
    let (cw, ch) = match &f.pix_fmt[..6] {
        "yuv420" => (w.div_ceil(2), h.div_ceil(2)),
        "yuv422" => (w.div_ceil(2), h),
        "yuv440" => (w, h.div_ceil(2)),
        _ => (w, h),
    };
    let bps = if f.pix_fmt.ends_with("le") { 2 } else { 1 };
    (w * h + 2 * cw * ch) * bps
}

fn check(f: &Fixture, threads: usize, data: &[u8], reference: &[u8]) {
    let chunks = ivf_frames(data);
    let name = format!("{} with {threads} thread(s)", f.name);
    let size = frame_bytes(f);
    let mut dec = Decoder::with_threads(threads);
    let got: Vec<Vec<u8>> = if is_420(f) {
        let frames = decode_frames(&mut dec, &chunks);
        let format = if f.pix_fmt.ends_with("le") {
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
        }
        frames
            .iter()
            .map(|fr| {
                fr.planes
                    .iter()
                    .flat_map(|p| p.data.iter().copied())
                    .collect()
            })
            .collect()
    } else {
        decode_pictures(&mut dec, &chunks)
            .iter()
            .map(|p| {
                [&p.y, &p.u, &p.v]
                    .iter()
                    .flat_map(|p| p.to_le_bytes())
                    .collect()
            })
            .collect()
    };
    assert_eq!(got.len(), reference.len() / size, "{name}: frame count");
    for (i, (g, want)) in got.iter().zip(reference.chunks(size)).enumerate() {
        mismatch(&name, i, g, want);
    }
}

#[test]
fn bit_exact_against_ffmpeg() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let ready: Vec<Option<(PathBuf, PathBuf)>> = std::thread::scope(|s| {
        let hs: Vec<_> = CONFORMANCE.iter().map(|f| s.spawn(|| ensure(f))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (f, paths) in CONFORMANCE.iter().zip(ready) {
        let Some((ivf, yuv)) = paths else { return };
        let data = std::fs::read(ivf).unwrap();
        let reference = std::fs::read(yuv).unwrap();
        for threads in [1, 3, cores] {
            check(f, threads, &data, &reference);
        }
    }
}

#[test]
fn keyframes_are_found_and_described() {
    let f = &CONFORMANCE[1];
    let Some((ivf, _)) = ensure(f) else { return };
    let data = std::fs::read(ivf).unwrap();
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
    for f in [&CONFORMANCE[0], &CONFORMANCE[1], &CONFORMANCE[2]] {
        let Some((ivf, _)) = ensure(f) else { return };
        let data = std::fs::read(ivf).unwrap();
        let mut seed = 0x1234_5678_9abc_def0u64 ^ f.name.len() as u64;
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

fn ffmpeg_bench(args: &[&str]) -> (f64, f64) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-benchmark"])
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    let field = |k: &str| {
        text.rsplit(k)
            .next()
            .and_then(|t| t.split('s').next())
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(f64::NAN)
    };
    (field("utime=") + field("stime="), field("rtime="))
}

#[test]
#[ignore]
fn decode_speed() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    let only = std::env::var("VP9_BENCH").ok();
    let threads: Vec<usize> = match std::env::var("VP9_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        Some(t) => vec![t],
        None => vec![1, cores],
    };
    for f in BENCH
        .iter()
        .filter(|f| only.as_deref().is_none_or(|o| o == f.name))
    {
        let Some((ivf, yuv)) = ensure(f) else { return };
        let data = std::fs::read(&ivf).unwrap();
        let ours_only = std::env::var_os("VP9_OURS_ONLY").is_some();
        if !ours_only {
            check(f, cores, &data, &std::fs::read(yuv).unwrap());
        }
        let chunks = ivf_frames(&data);
        for &threads in &threads {
            let t = threads.to_string();
            let mut ours = (f64::MAX, f64::MAX);
            let mut theirs = (f64::MAX, f64::MAX);
            let mut frames = 0;
            let rounds = if ours_only { 3 } else { 5 };
            for _ in 0..rounds {
                let (c0, t0) = (cpu_secs(), Instant::now());
                frames = decode_count(&mut Decoder::with_threads(threads), &chunks);
                ours = (
                    ours.0.min(t0.elapsed().as_secs_f64()),
                    ours.1.min(cpu_secs() - c0),
                );
                if !ours_only {
                    let (cpu, wall) = ffmpeg_bench(&[
                        "-threads",
                        &t,
                        "-c:v",
                        "vp9",
                        "-i",
                        ivf.to_str().unwrap(),
                        "-f",
                        "null",
                        "-",
                    ]);
                    theirs = (theirs.0.min(wall), theirs.1.min(cpu));
                }
            }
            let ms = |s: f64| 1000.0 * s / frames as f64;
            print!(
                "{} {threads} thread(s): ours {:.1} fps wall, {:.2} cpu ms/frame",
                f.name,
                frames as f64 / ours.0,
                ms(ours.1)
            );
            if ours_only {
                println!();
                continue;
            }
            println!(
                "; ffmpeg {:.1} fps wall, {:.2} cpu ms/frame; cpu ratio {:.2}",
                frames as f64 / theirs.0,
                ms(theirs.1),
                ours.1 / theirs.1
            );
        }
    }
}

#[cfg(all(feature = "gpu", target_os = "linux"))]
#[test]
fn vaapi_matches_ffmpeg() {
    use crate::gpu::{Backend, Direction, HwDecoder, available};
    use crate::media::Codec;
    if !available().contains(&(Codec::Vp9, Direction::Decode, Backend::VaApi)) {
        eprintln!("SKIPPED: no VA-API VP9 decoder");
        return;
    }
    for f in CONFORMANCE.iter().filter(|f| is_420(f)) {
        let Some((ivf, yuv)) = ensure(f) else { return };
        let data = std::fs::read(ivf).unwrap();
        let reference = std::fs::read(yuv).unwrap();
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
        let size = frame_bytes(f);
        assert_eq!(
            frames.len(),
            reference.len() / size,
            "{}: frame count",
            f.name
        );
        for (i, (fr, want)) in frames.iter().zip(reference.chunks(size)).enumerate() {
            let bps = fr.format.bytes_per_sample();
            let got: Vec<u8> = fr
                .planes
                .iter()
                .flat_map(|p| {
                    (0..p.height).flat_map(move |y| {
                        p.data[y * p.stride..y * p.stride + p.width * bps]
                            .iter()
                            .copied()
                    })
                })
                .collect();
            mismatch(&format!("{} on VA-API", f.name), i, &got, want);
        }
        eprintln!(
            "{} on VA-API: {} frames bit-exact, {:.0} fps with read-back",
            f.name,
            frames.len(),
            frames.len() as f64 / secs
        );
    }
}
