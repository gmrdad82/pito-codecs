use super::{Decoder, Hvcc};
use crate::media::{Frame, Packet, PixelFormat};
use crate::syntax::bits::{annexb_nals, unescape_rbsp};
use crate::syntax::hevc::params::{Pps, ScalingList, Sps};
use crate::syntax::hevc::slice::{NalHeader, SliceHeader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
        .current_dir(env!("CARGO_MANIFEST_DIR"))
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

fn libx265(context: &str) -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *HAS.get_or_init(|| {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .is_ok_and(|o| {
                o.status.success()
                    && String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .any(|w| w == "libx265")
            })
    });
    if !ok {
        eprintln!("SKIPPED ({context}): this ffmpeg has no libx265 encoder");
    }
    ok
}

const SCALING_FILE: &str = "@scaling-list";

const X265_RECON: &str = "@x265-recon";

fn x265_cli(context: &str) -> bool {
    let ok = Command::new("x265")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("SKIPPED ({context}): no x265 command for its own reconstruction");
    }
    ok
}

fn x265_recon(f: &Fixture, hevc: &Path, yuv: &Path) -> bool {
    if hevc.exists() && yuv.exists() {
        return true;
    }
    if !x265_cli(f.name) {
        return false;
    }
    crate::fixture::once_all(&[hevc, yuv], |parts| {
        let raw = scratch(&format!("{}.raw.yuv", f.name));
        let (tmp_hevc, tmp_yuv) = (&parts[0], &parts[1]);
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
        ff(
            &["-f", "lavfi"],
            Path::new(&src),
            &[
                "-frames:v",
                &frames,
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix_fmt(f.ten_bit),
            ],
            &raw,
        );
        let depth = if f.ten_bit { "10" } else { "8" };
        let mut cmd = Command::new("x265");
        cmd.args(["--log-level", "error", "--input"])
            .arg(&raw)
            .args(["--input-res", &format!("{}x{}", f.width, f.height)])
            .args(["--fps", "25", "--frames", &frames])
            .args([
                "--input-depth",
                depth,
                "--output-depth",
                depth,
                "--recon-depth",
                depth,
            ]);
        for kv in f.args[1..].iter().flat_map(|a| a.split(':')) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            cmd.arg(format!("--{k}"));
            if !v.is_empty() {
                cmd.arg(v);
            }
        }
        let out = cmd
            .arg("--recon")
            .arg(tmp_yuv)
            .arg("-o")
            .arg(tmp_hevc)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&raw);
        assert!(
            out.status.success(),
            "{}: x265 failed: {}",
            f.name,
            String::from_utf8_lossy(&out.stderr)
        );
    });
    true
}

fn scaling_list_text() -> String {
    let sizes = [("4X4", 16), ("8X8", 64), ("16X16", 64), ("32X32", 64)];
    let lists = [
        "INTRA{}_LUMA",
        "INTRA{}_CHROMAU",
        "INTRA{}_CHROMAV",
        "INTER{}_LUMA",
        "INTER{}_CHROMAU",
        "INTER{}_CHROMAV",
    ];
    let mut text = String::new();
    for (s, (size, n)) in sizes.iter().enumerate() {
        for (l, list) in lists.iter().enumerate() {
            let name = list.replace("{}", size);
            let seed = if l == 5 { 4 } else { l };
            let coeffs: Vec<String> = (0..*n)
                .map(|i| (8 + s * 3 + seed * 2 + (i * (seed + 1)) % 23).to_string())
                .collect();
            text += &format!("{name} =\n{}\n", coeffs.join(","));
            if s >= 2 {
                text += &format!("{name}_DC =\n{}\n", 10 + s + seed);
            }
        }
    }
    text
}

fn scaling_list_file() {
    let path = final_path("scaling_list.txt");
    let text = scaling_list_text();
    if std::fs::read_to_string(&path).ok().as_deref() != Some(&text[..]) {
        let tmp = scratch("scaling_list.txt");
        std::fs::write(&tmp, &text).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
}

fn cache_name(f: &Fixture) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        f.source, f.width, f.height, f.frames, f.filter, f.ten_bit, f.args,
    )
        .hash(&mut h);
    if f.args.iter().any(|a| a.contains(SCALING_FILE)) {
        scaling_list_text().hash(&mut h);
    }
    format!("{}-{:016x}", f.name, h.finish())
}

fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    if !ffmpeg(f.name) || !libx265(f.name) {
        return None;
    }
    let cached = cache_name(f);
    let hevc = final_path(&format!("{cached}.hevc"));
    let yuv = final_path(&format!("{cached}.yuv"));
    if f.args.first() == Some(&X265_RECON) {
        return x265_recon(f, &hevc, &yuv).then_some((hevc, yuv));
    }
    crate::fixture::once(&hevc, |tmp| {
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
            let mut given = args[i + 1].to_string();
            if given.contains(SCALING_FILE) {
                scaling_list_file();
                given = given.replace(SCALING_FILE, "tmp/fixtures/hevc/scaling_list.txt");
            }
            params = format!("{given}:log-level=error");
            args[i + 1] = &params;
        } else {
            args.extend(["-x265-params", "log-level=error"]);
        }
        mid.extend(args);
        mid.extend(["-f", "hevc"]);
        ff(&["-f", "lavfi"], Path::new(&src), &mid, tmp);
    });
    crate::fixture::once(&yuv, |tmp| {
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
            tmp,
        );
    });
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

const SMALL: (u32, u32) = (128, 96);

const FEATURES: &[Fixture] = &[
    fx(
        "wpp_crop_66x70",
        "testsrc2",
        (66, 70),
        10,
        NOISE,
        false,
        &["-x265-params", "ctu=16:bframes=3"],
    ),
    fx(
        "wpp_slices_main10",
        "testsrc2",
        SMALL,
        8,
        NOISE,
        true,
        &["-x265-params", "ctu=16:slices=3"],
    ),
    fx(
        "long_b_runs",
        "testsrc2",
        SMALL,
        34,
        NOISE,
        false,
        &["-x265-params", "bframes=16:b-pyramid=1:b-adapt=0:ref=6"],
    ),
    fx(
        "pyramid_weightb_main10",
        "testsrc2",
        SMALL,
        20,
        "fade=in:0:16",
        true,
        &[
            "-x265-params",
            "bframes=8:b-pyramid=1:ref=5:weightp=1:weightb=1",
        ],
    ),
    fx(
        "weightp_8bit",
        "testsrc2",
        SMALL,
        12,
        "fade=in:0:10",
        false,
        &["-x265-params", "bframes=2:weightp=1:weightb=1"],
    ),
    fx(
        "scaling_tskip_main10",
        "testsrc2",
        SMALL,
        8,
        NOISE,
        true,
        &["-x265-params", "scaling-list=default:tskip=1:rdoq-level=0"],
    ),
    fx(
        "cu_qp_chroma_offsets",
        "testsrc2",
        SMALL,
        8,
        NOISE,
        false,
        &[
            X265_RECON,
            "ctu=16:aq-mode=3:qg-size=8:cbqpoffs=-4:crqpoffs=5",
        ],
    ),
    fx(
        "intra_tools",
        "mandelbrot",
        SMALL,
        8,
        NOISE,
        false,
        &[
            "-x265-params",
            "constrained-intra=1:signhide=0:temporal-mvp=0:strong-intra-smoothing=0:max-merge=1:sao=0:deblock=-6,6:amp=1:rect=1:tu-intra-depth=4:tu-inter-depth=4",
        ],
    ),
    fx(
        "cu_lossless",
        "testsrc2",
        SMALL,
        6,
        NOISE,
        false,
        &["-x265-params", "cu-lossless=1"],
    ),
    fx(
        "one_ctb_no_deblock",
        "testsrc2",
        (64, 64),
        6,
        NOISE,
        false,
        &["-x265-params", "ctu=64:no-deblock=1"],
    ),
    fx(
        "scaling_list_file",
        "testsrc2",
        SMALL,
        6,
        NOISE,
        false,
        &["-x265-params", "scaling-list=@scaling-list:rdoq-level=0"],
    ),
];

const OPEN_GOP_CUT: Fixture = fx(
    "open_gop_cut",
    "testsrc2",
    SMALL,
    30,
    NOISE,
    false,
    &[
        "-x265-params",
        "keyint=8:min-keyint=8:open-gop=1:bframes=3:repeat-headers=1",
    ],
);

fn units_of(data: &[u8]) -> Vec<Vec<u8>> {
    access_units(data).into_iter().map(<[u8]>::to_vec).collect()
}

fn feature(name: &str) -> &'static Fixture {
    FEATURES
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no feature fixture named {name}"))
}

struct Facts {
    sps: Sps,
    pps: Pps,
    max_slices: usize,
    explicit_weights: bool,
}

fn facts(hevc: &Path) -> Facts {
    let data = std::fs::read(hevc).unwrap();
    let (mut sps, mut pps): (Option<Arc<Sps>>, Option<Arc<Pps>>) = (None, None);
    let (mut slices, mut max_slices, mut explicit_weights) = (0, 0, false);
    let mut prev: Option<SliceHeader> = None;
    for nal in annexb_nals(&data) {
        let hdr = NalHeader::parse(nal).unwrap();
        let rbsp = unescape_rbsp(&nal[2..]);
        match hdr.nal_type {
            33 => sps = Some(Arc::new(Sps::parse(&rbsp).unwrap())),
            34 => pps = Some(Arc::new(Pps::parse(&rbsp).unwrap())),
            0..=9 | 16..=21 => {
                let (p, s) = (pps.clone().unwrap(), sps.clone().unwrap());
                let (sh, _, _) =
                    SliceHeader::parse(&rbsp, hdr, |_| Ok((p, s)), prev.as_ref()).unwrap();
                if sh.first_slice_segment_in_pic {
                    slices = 0;
                }
                slices += 1;
                max_slices = max_slices.max(slices);
                explicit_weights |= sh
                    .pwt
                    .iter()
                    .flat_map(|t| t.l.iter().flatten())
                    .any(|e| e.luma_flag || e.chroma_flag);
                prev = Some(sh);
            }
            _ => {}
        }
    }
    Facts {
        sps: Arc::unwrap_or_clone(sps.expect("no SPS")),
        pps: Arc::unwrap_or_clone(pps.expect("no PPS")),
        max_slices,
        explicit_weights,
    }
}

fn assert_feature(name: &str, x: &Facts) {
    let (sps, pps) = (&x.sps, &x.pps);
    let carried: Vec<(&str, bool)> = match name {
        "wpp_crop_66x70" => vec![
            ("entropy_coding_sync", pps.entropy_coding_sync),
            ("conformance window", sps.conf_win != (0, 0, 0, 0)),
        ],
        "wpp_slices_main10" => vec![
            ("entropy_coding_sync", pps.entropy_coding_sync),
            ("several slices", x.max_slices > 1),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "long_b_runs" => vec![("reorder", sps.max_num_reorder >= 2)],
        "pyramid_weightb_main10" => vec![
            ("weighted_pred", pps.weighted_pred),
            ("weighted_bipred", pps.weighted_bipred),
            ("explicit weights", x.explicit_weights),
            ("reorder", sps.max_num_reorder >= 2),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "weightp_8bit" => vec![
            ("weighted_pred", pps.weighted_pred),
            ("explicit weights", x.explicit_weights),
        ],
        "scaling_tskip_main10" => vec![
            ("scaling_list_enabled", sps.scaling_list_enabled),
            ("transform_skip", pps.transform_skip),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "scaling_list_file" => vec![
            ("scaling_list_enabled", sps.scaling_list_enabled),
            (
                "explicit lists",
                sps.scaling_list
                    .as_ref()
                    .or(pps.scaling_list.as_ref())
                    .is_some_and(|l| l.lists != ScalingList::default_lists().lists),
            ),
        ],
        "cu_qp_chroma_offsets" => vec![
            ("cu_qp_delta", pps.cu_qp_delta_enabled),
            ("cb_qp_offset", pps.cb_qp_offset == -4),
            ("cr_qp_offset", pps.cr_qp_offset == 5),
        ],
        "intra_tools" => vec![
            ("constrained_intra_pred", pps.constrained_intra_pred),
            ("no sign hiding", !pps.sign_data_hiding),
            ("no temporal mvp", !sps.temporal_mvp),
            ("no strong smoothing", !sps.strong_intra_smoothing),
            ("no sao", !sps.sao),
            ("amp", sps.amp),
        ],
        "cu_lossless" => vec![("transquant_bypass", pps.transquant_bypass)],
        "one_ctb_no_deblock" => vec![
            ("64x64 CTB", sps.log2_ctb == 6),
            ("deblocking off", pps.deblocking_disabled),
        ],
        _ => panic!("{name}: no feature check"),
    };
    for (what, ok) in carried {
        assert!(ok, "{name}: the stream does not carry {what}");
    }
}

#[test]
fn feature_streams_bit_exact_against_ffmpeg() {
    for f in FEATURES {
        let Some((hevc, yuv)) = ensure(f) else {
            continue;
        };
        assert_feature(f.name, &facts(&hevc));
        let units = units_of(&std::fs::read(hevc).unwrap());
        let reference = std::fs::read(yuv).unwrap();
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

#[cfg(target_arch = "x86_64")]
#[test]
fn scalar_path_bit_exact_against_ffmpeg() {
    for f in CONFORMANCE
        .iter()
        .chain(FEATURES)
        .chain(std::iter::once(&OPEN_GOP_CUT))
    {
        let Some((hevc, yuv)) = ensure(f) else {
            continue;
        };
        let units = units_of(&std::fs::read(hevc).unwrap());
        super::simd::force_off(true);
        let before = super::simd::forced_checks();
        let frames = decode_units(&mut Decoder::with_threads(1), &units);
        let checks = super::simd::forced_checks() - before;
        super::simd::force_off(false);
        assert!(
            checks > 0,
            "{}: the one-thread decode never asked for SIMD on the test's thread, so the scalar path was not forced",
            f.name
        );
        compare(
            &format!("{} (scalar)", f.name),
            &frames,
            &std::fs::read(yuv).unwrap(),
            f,
        );
    }
}

fn first_vcl_type(au: &[u8]) -> Option<u8> {
    annexb_nals(au)
        .into_iter()
        .map(|n| (n[0] >> 1) & 0x3f)
        .find(|&t| t < 32)
}

struct Scratch(Vec<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[test]
fn open_gop_cut_at_a_cra_drops_its_rasl_pictures() {
    let f = &OPEN_GOP_CUT;
    let Some((hevc, _)) = ensure(f) else { return };
    let data = std::fs::read(hevc).unwrap();
    let aus = access_units(&data);
    let cra = (1..aus.len())
        .find(|&i| first_vcl_type(aus[i]) == Some(21))
        .expect("open_gop_cut: no CRA after the first picture");
    let rasl = aus[cra + 1..]
        .iter()
        .map(|au| first_vcl_type(au))
        .take_while(|t| !matches!(t, Some(16..=23)))
        .filter(|t| matches!(t, Some(8 | 9)))
        .count();
    assert!(rasl > 0, "open_gop_cut: the cut CRA leads no RASL pictures");
    let tail = scratch("open_gop_cut_tail.hevc");
    let yuv = scratch("open_gop_cut_tail.yuv");
    let _guard = Scratch(vec![tail.clone(), yuv.clone()]);
    std::fs::write(&tail, aus[cra..].concat()).unwrap();
    ff(
        &[],
        &tail,
        &[
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            pix_fmt(false),
        ],
        &yuv,
    );
    let reference = std::fs::read(&yuv).unwrap();
    let units: Vec<Vec<u8>> = aus[cra..].iter().map(|au| au.to_vec()).collect();
    for threads in [1, 4] {
        let frames = decode_units(&mut Decoder::with_threads(threads), &units);
        assert_eq!(
            frames.len(),
            units.len() - rasl,
            "open_gop_cut ({threads} threads): RASL pictures of the first CRA were output"
        );
        compare(
            &format!("open_gop_cut ({threads} threads)"),
            &frames,
            &reference,
            f,
        );
    }
}

fn decode_errors(units: &[Vec<u8>], threads: usize) -> Vec<String> {
    let mut dec = Decoder::with_threads(threads);
    let mut errors = Vec::new();
    for (i, u) in units.iter().enumerate() {
        if let Err(e) = dec.decode(&packet(u.clone(), i)) {
            errors.push(e.to_string());
        }
    }
    if let Err(e) = dec.flush() {
        errors.push(e.to_string());
    }
    errors
}

#[test]
fn truncated_nal_units_are_errors_not_panics() {
    let f = feature("wpp_slices_main10");
    let Some((hevc, _)) = ensure(f) else { return };
    let data = std::fs::read(hevc).unwrap();
    let mut units = units_of(&data);
    units.truncate(4);
    let head = annexb_nals(&units[0])
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    let mut checked = 0;
    for (k, nal) in head.iter().enumerate() {
        let t = (nal[0] >> 1) & 0x3f;
        for cut in [2, 3, nal.len() / 2, nal.len() - 1] {
            if cut < 2 || cut >= nal.len() {
                continue;
            }
            let mut first = Vec::new();
            for (j, n) in head.iter().enumerate() {
                first.extend([0, 0, 0, 1]);
                first.extend_from_slice(if j == k { &n[..cut] } else { n });
            }
            let mut damaged = units.clone();
            damaged[0] = first;
            for threads in [1, 4] {
                let errors = decode_errors(&damaged, threads);
                let what = format!("NAL {k} (type {t}, {} bytes) cut to {cut}", nal.len());
                assert!(
                    errors.iter().all(|e| !e.contains("internal error")),
                    "{what}: a panic was caught: {errors:?}"
                );
                let must_fail = match t {
                    32..=34 => cut <= nal.len() / 2,
                    0..=31 => cut <= 3 || (cut == nal.len() / 2 && nal.len() > 64),
                    _ => false,
                };
                if must_fail {
                    assert!(!errors.is_empty(), "{what}: decoded without an error");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 12, "only {checked} truncations were checked");
}

#[test]
fn a_dropped_reference_picture_is_replaced_without_a_panic() {
    for name in ["long_b_runs", "pyramid_weightb_main10"] {
        let f = feature(name);
        let Some((hevc, _)) = ensure(f) else { continue };
        let mut units = units_of(&std::fs::read(hevc).unwrap());
        assert!(
            matches!(first_vcl_type(&units[0]), Some(16..=21)),
            "{name}: the stream does not start at an IRAP"
        );
        assert!(
            first_vcl_type(&units[1]) == Some(1),
            "{name}: the second access unit is not a trailing reference picture"
        );
        units.remove(1);
        let format = if f.ten_bit {
            PixelFormat::Yuv420p10
        } else {
            PixelFormat::Yuv420p8
        };
        for threads in [1, 4] {
            let mut dec = Decoder::with_threads(threads);
            let mut frames = Vec::new();
            let mut errors = Vec::new();
            for (i, u) in units.iter().enumerate() {
                match dec.decode(&packet(u.clone(), i)) {
                    Ok(out) => frames.extend(out),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            match dec.flush() {
                Ok(out) => frames.extend(out),
                Err(e) => errors.push(e.to_string()),
            }
            let what = format!("{name} without its first P picture ({threads} threads)");
            assert!(errors.is_empty(), "{what}: {errors:?}");
            assert_eq!(frames.len(), units.len(), "{what}: picture count");
            for (i, frame) in frames.iter().enumerate() {
                assert_eq!(
                    (frame.format, frame.width, frame.height),
                    (format, f.width, f.height),
                    "{what}: picture {i}"
                );
            }
        }
    }
}
