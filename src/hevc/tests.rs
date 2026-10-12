use super::{Decoder, Hvcc};
use crate::media::{Frame, Packet, PixelFormat};
use crate::syntax::bits::annexb_nals;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub(super) struct Fixture {
    name: &'static str,
    source: &'static str,
    width: u32,
    height: u32,
    frames: u32,
    filter: &'static str,
    ten_bit: bool,
    args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=12:allf=t+u";

const fn fx(
    name: &'static str,
    source: &'static str,
    (width, height): (u32, u32),
    frames: u32,
    filter: &'static str,
    ten_bit: bool,
    args: &'static [&'static str],
) -> Fixture {
    Fixture {
        name,
        source,
        width,
        height,
        frames,
        filter,
        ten_bit,
        args,
    }
}

const CIF: (u32, u32) = (352, 288);

const CONFORMANCE: &[Fixture] = &[
    fx(
        "intra_main10",
        "testsrc2",
        CIF,
        3,
        NOISE,
        true,
        &["-x265-params", "keyint=1"],
    ),
    fx(
        "medium",
        "testsrc2",
        CIF,
        20,
        NOISE,
        false,
        &["-preset", "medium"],
    ),
    fx(
        "slow_main10",
        "mandelbrot",
        CIF,
        12,
        NOISE,
        true,
        &["-preset", "slow"],
    ),
    fx(
        "bframes",
        "testsrc2",
        CIF,
        30,
        NOISE,
        false,
        &["-x265-params", "bframes=8:b-pyramid=1:ref=4"],
    ),
    fx(
        "weightp_main10",
        "testsrc2",
        CIF,
        20,
        "fade=in:0:18",
        true,
        &["-x265-params", "weightp=1:weightb=1"],
    ),
    fx(
        "open_gop",
        "testsrc2",
        CIF,
        40,
        NOISE,
        false,
        &[
            "-x265-params",
            "keyint=12:min-keyint=12:open-gop=1:bframes=3",
        ],
    ),
    fx(
        "tools",
        "testsrc2",
        CIF,
        12,
        NOISE,
        false,
        &[
            "-x265-params",
            "slices=4:scaling-list=default:tskip=1:rect=1:amp=1",
        ],
    ),
    fx(
        "lossless",
        "testsrc2",
        (176, 144),
        6,
        NOISE,
        false,
        &["-x265-params", "lossless=1"],
    ),
    fx(
        "odd_1918x1078",
        "testsrc2",
        (1918, 1078),
        3,
        NOISE,
        false,
        &["-preset", "veryfast"],
    ),
];

const BENCH: &[Fixture] = &[
    fx(
        "bench_1080p",
        "testsrc2",
        (1920, 1080),
        120,
        "noise=alls=3:allf=t",
        false,
        &["-preset", "fast", "-crf", "22"],
    ),
    fx(
        "bench_1440p",
        "testsrc2",
        (2560, 1440),
        90,
        "noise=alls=3:allf=t",
        false,
        &["-preset", "fast", "-crf", "22"],
    ),
    fx(
        "bench_4k",
        "testsrc2",
        (3840, 2160),
        60,
        "noise=alls=3:allf=t",
        false,
        &["-preset", "fast", "-crf", "22"],
    ),
    fx(
        "bench_4k_main10",
        "testsrc2",
        (3840, 2160),
        60,
        "noise=alls=3:allf=t",
        true,
        &["-preset", "fast", "-crf", "22"],
    ),
];

pub(super) fn ffmpeg(context: &str) -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIPPED ({context}): ffmpeg not found");
    }
    ok
}

pub(super) fn scratch(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fixtures/hevc");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!(
        ".{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn final_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/fixtures/hevc")
        .join(name)
}

fn ff(args: &[&str], input: &Path, mid: &[&str], output: &Path) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg("-i")
        .arg(input)
        .args(mid)
        .arg(output)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn pix_fmt(ten_bit: bool) -> &'static str {
    if ten_bit { "yuv420p10le" } else { "yuv420p" }
}

fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    if !ffmpeg(f.name) {
        return None;
    }
    let hevc = final_path(&format!("{}.hevc", f.name));
    let yuv = final_path(&format!("{}.yuv", f.name));
    if !hevc.exists() {
        let tmp = scratch(&format!("{}.hevc", f.name));
        let mut src = format!(
            "{}=size={}x{}:rate=25,format={}",
            f.source,
            f.width,
            f.height,
            pix_fmt(f.ten_bit)
        );
        if !f.filter.is_empty() {
            src = format!("{src},{}", f.filter);
        }
        let frames = f.frames.to_string();
        let mut mid = vec![
            "-frames:v",
            &frames,
            "-c:v",
            "libx265",
            "-pix_fmt",
            pix_fmt(f.ten_bit),
        ];
        let params;
        let mut args = f.args.to_vec();
        if let Some(i) = args.iter().position(|a| *a == "-x265-params") {
            params = format!("{}:log-level=error", args[i + 1]);
            args[i + 1] = &params;
        } else {
            args.extend(["-x265-params", "log-level=error"]);
        }
        mid.extend(args);
        mid.extend(["-f", "hevc"]);
        ff(&["-f", "lavfi"], Path::new(&src), &mid, &tmp);
        if let Err(e) = std::fs::rename(&tmp, &hevc) {
            assert!(std::path::Path::new(&hevc).exists(), "{e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
    if !yuv.exists() {
        let tmp = scratch(&format!("{}.yuv", f.name));
        ff(
            &[],
            &hevc,
            &[
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt(f.ten_bit),
            ],
            &tmp,
        );
        if let Err(e) = std::fs::rename(&tmp, &yuv) {
            assert!(std::path::Path::new(&yuv).exists(), "{e}");
            let _ = std::fs::remove_file(&tmp);
        }
    }
    if !decodes_clean(&hevc) {
        eprintln!(
            "SKIPPED ({}): this ffmpeg's libx265 wrote a stream its own decoder rejects",
            f.name
        );
        return None;
    }
    Some((hevc, yuv))
}

fn decodes_clean(hevc: &Path) -> bool {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(hevc)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    out.status.success() && out.stderr.is_empty()
}

pub(super) fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    let mut au_start = 0;
    let mut seen_vcl = false;
    for (sc, payload) in starts {
        if payload + 2 >= data.len() {
            continue;
        }
        let t = (data[payload] >> 1) & 0x3f;
        let first_slice = t < 32 && data[payload + 2] & 0x80 != 0;
        if seen_vcl && (matches!(t, 32..=35 | 39) || first_slice) {
            aus.push(&data[au_start..sc]);
            au_start = sc;
            seen_vcl = false;
        }
        seen_vcl |= t < 32;
    }
    if au_start < data.len() {
        aus.push(&data[au_start..]);
    }
    aus
}

fn packet(data: Vec<u8>, i: usize) -> Packet {
    Packet {
        data,
        pts: i as i64,
        dts: i as i64,
        keyframe: false,
    }
}

fn decode_units(dec: &mut Decoder, units: &[Vec<u8>]) -> Vec<Frame> {
    let mut out = Vec::new();
    for (i, u) in units.iter().enumerate() {
        out.extend(
            dec.decode(&packet(u.clone(), i))
                .unwrap_or_else(|e| panic!("access unit {i}: {e}")),
        );
    }
    out.extend(dec.flush().unwrap());
    out
}

fn compare(name: &str, frames: &[Frame], reference: &[u8], f: &Fixture) {
    let format = if f.ten_bit {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    };
    let size = (f.width * f.height) as usize * 3 / 2 * format.bytes_per_sample();
    assert_eq!(frames.len(), reference.len() / size, "{name}: frame count");
    for (i, (frame, want)) in frames.iter().zip(reference.chunks(size)).enumerate() {
        assert_eq!(
            (frame.format, frame.width, frame.height),
            (format, f.width, f.height),
            "{name}: frame {i}"
        );
        let got: Vec<u8> = frame
            .planes
            .iter()
            .flat_map(|p| p.data.iter().copied())
            .collect();
        if got != want {
            let k = got.iter().zip(want).position(|(a, b)| a != b).unwrap_or(0);
            panic!(
                "{name}: frame {i} differs from ffmpeg at byte {k} of {}",
                got.len()
            );
        }
    }
    let mut pts: Vec<i64> = frames.iter().map(|f| f.pts).collect();
    pts.sort_unstable();
    pts.dedup();
    assert_eq!(pts.len(), frames.len(), "{name}: pts lost in reordering");
}

#[test]
fn bit_exact_against_ffmpeg() {
    for f in CONFORMANCE {
        let Some((hevc, yuv)) = ensure(f) else {
            continue;
        };
        let data = std::fs::read(hevc).unwrap();
        let reference = std::fs::read(yuv).unwrap();
        let units: Vec<Vec<u8>> = access_units(&data)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        for threads in [1, 4] {
            let frames = decode_units(&mut Decoder::with_threads(threads), &units);
            compare(
                &format!("{} ({threads} threads)", f.name),
                &frames,
                &reference,
                f,
            );
        }
    }
}

fn hvcc_box(mp4: &[u8]) -> &[u8] {
    let at = mp4.windows(4).position(|w| w == b"hvcC").unwrap();
    let size = u32::from_be_bytes(mp4[at - 4..at].try_into().unwrap()) as usize;
    &mp4[at + 4..at - 4 + size]
}

#[test]
fn hvcc_matches_ffmpeg_and_drives_length_prefixed_decode() {
    for f in [&CONFORMANCE[1], &CONFORMANCE[0]] {
        let Some((hevc, yuv)) = ensure(f) else { return };
        let mp4 = scratch(&format!("{}.mp4", f.name));
        ff(&[], &hevc, &["-c", "copy", "-tag:v", "hvc1"], &mp4);
        let theirs = Hvcc::parse(hvcc_box(&std::fs::read(&mp4).unwrap())).unwrap();
        std::fs::remove_file(&mp4).unwrap();
        let data = std::fs::read(&hevc).unwrap();
        let ours = Hvcc::parse(&Hvcc::from_annexb(&data).unwrap().to_bytes()).unwrap();
        assert_eq!(
            (ours.profile, ours.chroma_format, ours.bit_depth_luma),
            (theirs.profile, theirs.chroma_format, theirs.bit_depth_luma),
            "{}",
            f.name
        );
        assert_eq!(
            (&ours.vps, &ours.sps, &ours.pps),
            (&theirs.vps, &theirs.sps, &theirs.pps),
            "{}",
            f.name
        );
        assert_eq!((ours.width, ours.height), (f.width, f.height));
        let units: Vec<Vec<u8>> = access_units(&data)
            .into_iter()
            .map(|au| {
                let mut sample = Vec::new();
                for nal in annexb_nals(au)
                    .into_iter()
                    .filter(|n| !matches!((n[0] >> 1) & 0x3f, 32..=34))
                {
                    sample.extend((nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
                sample
            })
            .collect();
        let mut dec = Decoder::from_hvcc(&theirs.to_bytes()).unwrap();
        let frames = decode_units(&mut dec, &units);
        compare(
            &format!("{} via hvcC", f.name),
            &frames,
            &std::fs::read(yuv).unwrap(),
            f,
        );
    }
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn damaged_streams_fail_without_panics() {
    for f in [&CONFORMANCE[1], &CONFORMANCE[3], &CONFORMANCE[0]] {
        let Some((hevc, _)) = ensure(f) else { return };
        let data = std::fs::read(hevc).unwrap();
        let mut seed = 0x1234_5678_9abc_def0u64 ^ f.name.len() as u64;
        for round in 0..8 {
            let mut d = data.clone();
            for _ in 0..(1 + round * 3) {
                let pos = 100 + (xorshift(&mut seed) as usize) % (d.len() - 100);
                d[pos] ^= 1 << (xorshift(&mut seed) % 8);
            }
            if round % 4 == 3 {
                d.truncate(d.len() * 2 / 3);
            }
            for threads in [1, 4] {
                let mut dec = Decoder::with_threads(threads);
                for (i, au) in access_units(&d).into_iter().enumerate() {
                    let _ = dec.decode(&packet(au.to_vec(), i));
                }
                let _ = dec.flush();
            }
        }
    }
    let mut seed = 42u64;
    for len in [0usize, 1, 5, 100, 5000] {
        for _ in 0..20 {
            let mut g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
            if len > 10 {
                g[..4].copy_from_slice(&[0, 0, 0, 1]);
            }
            let mut dec = Decoder::new();
            let _ = dec.decode(&packet(g, 0));
            let _ = dec.flush();
        }
    }
    assert!(Decoder::from_hvcc(&[1, 2, 3]).is_err());
    assert!(Hvcc::parse(&[1; 30]).is_err());
}

#[test]
#[ignore]
fn decode_speed() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    let only = std::env::var("HEVC_BENCH").ok();
    let threads: Vec<usize> = match std::env::var("HEVC_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        Some(t) => vec![t],
        None => vec![1, cores],
    };
    if let Ok(file) = std::env::var("HEVC_FILE") {
        let data = std::fs::read(&file).unwrap();
        if std::env::var_os("HEVC_CHECK").is_some() {
            check_file(&file, &data);
        }
        time_units(&file, &data, &threads);
        return;
    }
    for f in BENCH
        .iter()
        .filter(|f| only.as_deref().is_none_or(|o| o == f.name))
    {
        let Some((hevc, yuv)) = ensure(f) else { return };
        let data = std::fs::read(hevc).unwrap();
        let units: Vec<Vec<u8>> = access_units(&data)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        compare(
            f.name,
            &decode_units(&mut Decoder::new(), &units),
            &std::fs::read(yuv).unwrap(),
            f,
        );
        time_units(f.name, &data, &threads);
    }
}

fn time_units(name: &str, data: &[u8], threads: &[usize]) {
    let units: Vec<Vec<u8>> = access_units(data).into_iter().map(<[u8]>::to_vec).collect();
    for &threads in threads {
        let mut best = f64::MAX;
        let mut frames = 0;
        for _ in 0..3 {
            let mut dec = Decoder::with_threads(threads);
            let t0 = Instant::now();
            frames = 0;
            for (i, u) in units.iter().enumerate() {
                for f in dec.decode(&packet(u.clone(), i)).unwrap() {
                    frames += 1;
                    dec.recycle(f);
                }
            }
            frames += dec.flush().unwrap().len();
            best = best.min(t0.elapsed().as_secs_f64());
        }
        println!(
            "{} ({} KB): {threads} thread(s): {frames} frames in {best:.3}s = {:.1} fps",
            name,
            data.len() / 1024,
            frames as f64 / best
        );
    }
}

fn check_file(file: &str, data: &[u8]) {
    let units: Vec<Vec<u8>> = access_units(data).into_iter().map(<[u8]>::to_vec).collect();
    let frames = decode_units(&mut Decoder::new(), &units);
    let Some(first) = frames.first() else {
        panic!("{file}: no frames");
    };
    let pix = if first.format == PixelFormat::Yuv420p10 {
        "yuv420p10le"
    } else {
        "yuv420p"
    };
    let out = Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", file, "-f", "rawvideo", "-pix_fmt", pix, "-",
        ])
        .output()
        .unwrap();
    let mut at = 0;
    for (i, f) in frames.iter().enumerate() {
        for p in &f.planes {
            let n = p.width * p.height * if pix == "yuv420p" { 1 } else { 2 };
            let ours: Vec<u8> = p
                .data
                .chunks(p.stride)
                .flat_map(|r| &r[..n / p.height])
                .copied()
                .collect();
            assert!(
                out.stdout.get(at..at + n) == Some(&ours[..]),
                "{file}: frame {i} differs from ffmpeg"
            );
            at += n;
        }
    }
    assert_eq!(
        at,
        out.stdout.len(),
        "{file}: frame count differs from ffmpeg"
    );
    println!("{file}: {} frames bit-exact against ffmpeg", frames.len());
}
