use super::oracle::{dir, ffmpeg, lib, source};
use super::{decode, decode_animation, info};
use crate::media::Rgba;

fn ffmpeg_webp(
    name: &str,
    lavfi: &str,
    size: (u32, u32),
    frames: usize,
    args: &[&str],
) -> Option<Vec<u8>> {
    let path = dir().join(name);
    let sep = if lavfi.contains('=') { ':' } else { '=' };
    let input = format!("{lavfi}{sep}size={}x{}", size.0, size.1);
    let frames = frames.to_string();
    let mut a = vec!["-f", "lavfi", "-i", &input, "-frames:v", &frames];
    a.extend_from_slice(args);
    let p = path.to_str()?;
    a.push(p);
    ffmpeg(&a).then(|| std::fs::read(&path).ok()).flatten()
}

fn same(name: &str, ours: &Rgba, theirs: &Rgba) {
    assert_eq!(
        (ours.width, ours.height),
        (theirs.width, theirs.height),
        "{name}: size"
    );
    if let Some(i) = ours.data.iter().zip(&theirs.data).position(|(a, b)| a != b) {
        let px = i / 4;
        panic!(
            "{name}: first difference at pixel ({}, {}) channel {}: ours {} libwebp {}",
            px % ours.width as usize,
            px / ours.width as usize,
            i % 4,
            ours.data[i],
            theirs.data[i]
        );
    }
}

type Case = (
    &'static str,
    &'static str,
    (u32, u32),
    &'static [&'static str],
);

const LOSSLESS: &[Case] = &[
    (
        "ll_testsrc2.webp",
        "testsrc2",
        (331, 207),
        &["-c:v", "libwebp", "-lossless", "1"],
    ),
    (
        "ll_fast.webp",
        "mandelbrot",
        (256, 160),
        &[
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "0",
        ],
    ),
    (
        "ll_best.webp",
        "testsrc",
        (320, 240),
        &[
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "6",
            "-quality",
            "100",
        ],
    ),
    (
        "ll_alpha.webp",
        "testsrc2",
        (200, 120),
        &[
            "-vf",
            "format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='X*2'",
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
        ],
    ),
    (
        "ll_palette.webp",
        "smptebars",
        (300, 100),
        &["-c:v", "libwebp", "-lossless", "1"],
    ),
    (
        "ll_rgbtest.webp",
        "rgbtestsrc",
        (97, 61),
        &[
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "6",
        ],
    ),
];

#[test]
fn lossless_decode_matches_libwebp() {
    let Some(lib) = lib() else { return };
    for &(name, src, size, args) in LOSSLESS {
        let Some(file) = ffmpeg_webp(name, src, size, 1, args) else {
            return;
        };
        let i = info(&file).unwrap();
        assert!(i.lossless, "{name}");
        let ours = decode(&file).unwrap();
        same(name, &ours, &lib.decode_rgba(&file).unwrap());
    }
}

#[test]
fn animation_decode_matches_libwebp() {
    let Some(lib) = lib() else { return };
    let Some(file) = ffmpeg_webp(
        "anim_ll.webp",
        "testsrc2",
        (160, 96),
        8,
        &["-c:v", "libwebp_anim", "-lossless", "1", "-loop", "0"],
    ) else {
        return;
    };
    let Some(theirs) = lib.decode_animation(&file) else {
        return;
    };
    let ours = decode_animation(&file).unwrap();
    assert_eq!(ours.frames.len(), theirs.len());
    for (i, (o, (t, ts))) in ours.frames.iter().zip(&theirs).enumerate() {
        same(&format!("frame {i}"), &o.image, t);
        assert_eq!((o.start + o.duration) as i32, *ts, "frame {i} timestamp");
    }
    let _ = source;
}

fn photo(w: u32, h: u32) -> Option<Rgba> {
    source("mandelbrot", w, h, 1)?.pop().map(|mut img| {
        let noise = source("cellauto=rule=110", w, h, 1).and_then(|mut v| v.pop());
        if let Some(n) = noise {
            for (p, q) in img.data.chunks_exact_mut(4).zip(n.data.chunks_exact(4)) {
                p[0] = p[0].wrapping_add(q[0] >> 5);
            }
        }
        img
    })
}

#[test]
fn lossless_encode_decodes_exactly_in_libwebp_and_ffmpeg() {
    let Some(lib) = lib() else { return };
    let mut cases = Vec::new();
    for (src, w, h) in [
        ("testsrc", 333, 211),
        ("smptebars", 160, 90),
        ("rgbtestsrc", 7, 5),
        ("mandelbrot", 1, 1),
    ] {
        match source(src, w, h, 1).and_then(|mut v| v.pop()) {
            Some(img) => cases.push((format!("{src} {w}x{h}"), img)),
            None => eprintln!("no {src} {w}x{h}"),
        }
    }
    if let Some(p) = photo(256, 192) {
        cases.push(("photo".into(), p));
    }
    if let Some(mut v) = source("testsrc2", 120, 80, 1) {
        let mut img = v.pop().unwrap();
        for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
            p[3] = (i * 7 % 256) as u8;
        }
        cases.push(("alpha".into(), img));
    }
    for (name, img) in &cases {
        for effort in [0, 4, 6] {
            let opts = super::Options {
                effort,
                exact: true,
                ..super::Options::lossless()
            };
            let file = super::encode(img, &opts).unwrap();
            let theirs = lib
                .decode_rgba(&file)
                .unwrap_or_else(|| panic!("{name} effort {effort}: libwebp refused"));
            same(&format!("{name} effort {effort}"), &theirs, img);
            same(
                &format!("{name} effort {effort} ours"),
                &decode(&file).unwrap(),
                img,
            );
        }
    }
}

fn reel(w: u32, h: u32) -> Option<Rgba> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/coast/frames/reel/00001.png");
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(&path)
        .args([
            "-vf",
            &format!("scale={w}:{h}:flags=lanczos"),
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .ok()?;
    (out.status.success() && out.stdout.len() == (w * h * 4) as usize).then_some(Rgba {
        width: w,
        height: h,
        data: out.stdout,
    })
}

fn timed<T>(runs: usize, mut f: impl FnMut() -> T) -> (T, f64) {
    let mut best = f64::MAX;
    let mut out = None;
    for _ in 0..runs {
        let t = std::time::Instant::now();
        out = Some(f());
        best = best.min(t.elapsed().as_secs_f64());
    }
    (out.unwrap(), best)
}

#[test]
#[ignore]
fn webp_bench() {
    let Some(lib) = lib() else { return };
    for (w, h) in [(1280, 720), (1920, 1080)] {
        let Some(img) = reel(w, h).or_else(|| photo(w, h)) else {
            return;
        };
        let mp = (w * h) as f64 / 1e6;
        let (theirs, t) = timed(3, || lib.encode_lossless(&img));
        println!(
            "{w}x{h} lossless libwebp m4: {:>8} bytes {:>7.1} ms {:>6.1} MP/s",
            theirs.len(),
            t * 1e3,
            mp / t
        );
        for effort in [0, 2, 4, 6] {
            let opts = super::Options {
                effort,
                ..super::Options::lossless()
            };
            let (ours, t) = timed(3, || super::encode(&img, &opts).unwrap());
            println!(
                "{w}x{h} lossless ours e{effort}:   {:>8} bytes {:>7.1} ms {:>6.1} MP/s",
                ours.len(),
                t * 1e3,
                mp / t
            );
        }
        let (_, t) = timed(5, || lib.decode_rgba(&theirs).unwrap());
        println!(
            "{w}x{h} lossless decode libwebp: {:>7.2} ms {:>6.1} MP/s",
            t * 1e3,
            mp / t
        );
        let (_, t) = timed(5, || decode(&theirs).unwrap());
        println!(
            "{w}x{h} lossless decode ours:    {:>7.2} ms {:>6.1} MP/s",
            t * 1e3,
            mp / t
        );
    }
}

#[test]
fn alpha_plane_round_trips() {
    let (w, h) = (93u32, 41u32);
    let mut img = Rgba::new(w, h);
    for levels in [2usize, 9, 256] {
        for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
            let (x, y) = (i % w as usize, i / w as usize);
            p[3] = ((x * 3 + y * 5 + (x * y) % 7) % levels * (255 / (levels - 1).max(1))) as u8;
        }
        let a = super::alpha::encode(&img, &super::Options::default()).unwrap();
        let mut out = vec![0u8; img.data.len()];
        super::alpha::decode(&a, w, h, &mut out).unwrap();
        let back: Vec<u8> = out.chunks_exact(4).map(|p| p[3]).collect();
        let want: Vec<u8> = img.data.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(back, want, "{levels} levels");
    }
}

#[test]
fn lossless_animation_plays_back_exactly_in_libwebp() {
    let Some(lib) = lib() else { return };
    let Some(frames) = source("testsrc2", 160, 96, 6) else {
        return;
    };
    let mut enc = super::AnimEncoder::new(160, 96, super::Options::lossless(), 0).unwrap();
    for (i, f) in frames.iter().enumerate() {
        enc.add(f, 40 + i as u32).unwrap();
    }
    enc.add(&frames[5], 10).unwrap();
    let file = enc.finish().unwrap();
    let theirs = lib
        .decode_animation(&file)
        .expect("libwebp decodes our animation");
    assert_eq!(theirs.len(), 6);
    let mut end = 0;
    for (i, (img, ts)) in theirs.iter().enumerate() {
        same(&format!("frame {i}"), img, &frames[i]);
        end += 40 + i as i32 + if i == 5 { 10 } else { 0 };
        assert_eq!(*ts, end, "frame {i} end time");
    }
    let ours = decode_animation(&file).unwrap();
    for (i, f) in ours.frames.iter().enumerate() {
        same(&format!("ours frame {i}"), &f.image, &frames[i]);
    }
}

#[test]
fn damaged_files_fail_without_panics() {
    let mut files = Vec::new();
    for &(name, src, size, args) in &LOSSLESS[..3] {
        if let Some(f) = ffmpeg_webp(&format!("damaged_{name}"), src, size, 1, args) {
            files.push(f);
        }
    }
    if let Some(f) = ffmpeg_webp(
        "damaged_anim.webp",
        "testsrc2",
        (160, 96),
        8,
        &["-c:v", "libwebp_anim", "-lossless", "1", "-loop", "0"],
    ) {
        files.push(f);
    }
    if let Some(frames) = source("testsrc2", 384, 352, 3) {
        let mut a = frames[0].clone();
        for (i, px) in a.data.chunks_exact_mut(4).enumerate() {
            px[3] = (i % 251) as u8;
        }
        files.push(super::encode(&a, &super::Options::lossy(60.0)).unwrap());
        let mut enc = super::AnimEncoder::new(384, 352, super::Options::lossy(60.0), 0).unwrap();
        for f in &frames {
            enc.add(f, 40).unwrap();
        }
        files.push(enc.finish().unwrap());
    }
    let mut seed = 0x2545f4914f6cdd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for file in &files {
        for cut in [
            13,
            20,
            30,
            40,
            file.len() / 3,
            file.len() / 2,
            file.len().saturating_sub(1),
        ] {
            let _ = decode(&file[..cut.min(file.len())]);
            let _ = decode_animation(&file[..cut.min(file.len())]);
        }
        for _ in 0..60 {
            let mut f = file.clone();
            for _ in 0..1 + next() % 4 {
                let i = 12 + (next() as usize % (f.len() - 12));
                f[i] ^= 1 << (next() % 8);
            }
            let _ = decode(&f);
            let _ = decode_animation(&f);
        }
    }
}

#[test]
#[ignore]
fn webp_sizes() {
    let Some(lib) = lib() else { return };
    for src in [
        "testsrc2",
        "smptehdbars",
        "mandelbrot",
        "cellauto",
        "life",
        "rgbtestsrc",
        "yuvtestsrc",
        "gradients=seed=7",
    ] {
        let Some(img) = source(src, 1280, 720, 1).and_then(|mut v| v.pop()) else {
            continue;
        };
        let (theirs, t1) = timed(1, || lib.encode_lossless(&img));
        let (ours, t2) = timed(1, || {
            super::encode(&img, &super::Options::lossless()).unwrap()
        });
        println!(
            "{src:>12}: libwebp {:>8} {:>6.1} ms  ours {:>8} {:>6.1} ms  {:+.1}%",
            theirs.len(),
            t1 * 1e3,
            ours.len(),
            t2 * 1e3,
            (ours.len() as f64 / theirs.len() as f64 - 1.0) * 100.0
        );
    }
}

fn lossy_ready() -> bool {
    let probe = Rgba::new(16, 16);
    !matches!(
        super::encode(&probe, &super::Options::lossy(75.0)),
        Err(crate::media::Error::Unsupported(_))
    )
}

fn with_alpha(mut img: Rgba) -> Rgba {
    let w = img.width as usize;
    for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i % w, i / w);
        p[3] = if (x / 24 + y / 24) % 3 == 0 {
            0
        } else {
            ((x * 255) / w) as u8 | 0x20
        };
    }
    img
}

#[test]
fn lossy_alpha_decodes_the_same_in_libwebp() {
    let (Some(lib), true) = (lib(), lossy_ready()) else {
        return;
    };
    let Some(img) = source("testsrc2", 320, 180, 1)
        .and_then(|mut v| v.pop())
        .map(with_alpha)
    else {
        return;
    };
    let file = super::encode(&img, &super::Options::lossy(85.0)).unwrap();
    let i = info(&file).unwrap();
    assert!(i.alpha && !i.lossless);
    let theirs = lib
        .decode_rgba(&file)
        .expect("libwebp decodes lossy + alpha");
    let ours = decode(&file).unwrap();
    same("lossy alpha", &ours, &theirs);
    let alpha_in: Vec<u8> = img.data.chunks_exact(4).map(|p| p[3]).collect();
    let alpha_out: Vec<u8> = ours.data.chunks_exact(4).map(|p| p[3]).collect();
    assert_eq!(alpha_in, alpha_out, "alpha is lossless");
    let opaque: Vec<usize> = (0..alpha_in.len()).filter(|&i| alpha_in[i] > 0).collect();
    let a: Vec<u8> = opaque
        .iter()
        .flat_map(|&i| img.data[i * 4..i * 4 + 3].to_vec())
        .collect();
    let b: Vec<u8> = opaque
        .iter()
        .flat_map(|&i| ours.data[i * 4..i * 4 + 3].to_vec())
        .collect();
    let db = super::oracle::psnr(&a, &b);
    let reference = lib.decode_rgba(&lib.encode(&img, 85.0)).unwrap();
    let c: Vec<u8> = opaque
        .iter()
        .flat_map(|&i| reference.data[i * 4..i * 4 + 3].to_vec())
        .collect();
    let floor = super::oracle::psnr(&a, &c) - 1.5;
    assert!(
        db > floor,
        "lossy RGB PSNR {db:.1} dB, libwebp's at the same quality less 1.5 dB is {floor:.1}"
    );
}

#[test]
fn lossy_animation_plays_back_in_libwebp() {
    let (Some(lib), true) = (lib(), lossy_ready()) else {
        return;
    };
    let Some(frames) = source("testsrc2", 160, 96, 5) else {
        return;
    };
    let mut enc = super::AnimEncoder::new(160, 96, super::Options::lossy(80.0), 1).unwrap();
    for f in &frames {
        enc.add(f, 50).unwrap();
    }
    let file = enc.finish().unwrap();
    let theirs = lib
        .decode_animation(&file)
        .expect("libwebp decodes our lossy animation");
    let ours = decode_animation(&file).unwrap();
    assert_eq!(ours.info.loop_count, 1);
    assert_eq!(theirs.len(), frames.len());
    for (i, ((t, _), o)) in theirs.iter().zip(&ours.frames).enumerate() {
        same(&format!("lossy frame {i}"), &o.image, t);
        let db = super::oracle::psnr(&o.image.data, &frames[i].data);
        let reference = lib.decode_rgba(&lib.encode(&frames[i], 80.0)).unwrap();
        let floor = super::oracle::psnr(&reference.data, &frames[i].data) - 1.5;
        assert!(
            db > floor,
            "frame {i} PSNR {db:.1} dB, libwebp's still less 1.5 dB is {floor:.1}"
        );
    }
}

#[test]
#[ignore]
fn webp_alpha_sizes() {
    let (Some(lib), true) = (lib(), lossy_ready()) else {
        return;
    };
    let alph = |f: &[u8]| -> usize {
        super::riff::Chunks::new(super::riff::body(f).unwrap())
            .filter_map(|c| c.ok())
            .find(|c| &c.id == b"ALPH")
            .map_or(0, |c| c.data.len())
    };
    for (name, img) in [
        (
            "blocks",
            source("testsrc2", 1280, 720, 1)
                .and_then(|mut v| v.pop())
                .map(with_alpha),
        ),
        (
            "soft",
            reel(1280, 720).map(|mut i| {
                let w = i.width as usize;
                for (k, p) in i.data.chunks_exact_mut(4).enumerate() {
                    let (x, y) = ((k % w) as f32, (k / w) as f32);
                    p[3] = (255.0
                        * (1.0 - ((x - 640.0).powi(2) + (y - 360.0).powi(2)).sqrt() / 700.0)
                            .clamp(0.0, 1.0)) as u8;
                }
                i
            }),
        ),
    ] {
        let Some(img) = img else { continue };
        let theirs = lib.encode(&img, 80.0);
        let (ours, t) = timed(1, || {
            super::encode(&img, &super::Options::lossy(80.0)).unwrap()
        });
        println!(
            "{name}: ALPH libwebp {} ours {} ({:.1} ms whole encode)",
            alph(&theirs),
            alph(&ours),
            t * 1e3
        );
    }
}

#[test]
#[ignore]
fn webp_anim_bench() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/coast/frames/reel");
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-framerate",
            "25",
            "-start_number",
            "1",
            "-i",
        ])
        .arg(dir.join("%05d.png"))
        .args([
            "-frames:v",
            "60",
            "-vf",
            "scale=640:360",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    let frames: Vec<Rgba> = out
        .stdout
        .chunks_exact(640 * 360 * 4)
        .map(|c| Rgba {
            width: 640,
            height: 360,
            data: c.to_vec(),
        })
        .collect();
    let raw = super::oracle::dir().join("anim_src.rgba");
    std::fs::write(&raw, &out.stdout).unwrap();
    for (label, opts, args) in [
        (
            "lossless",
            super::Options::lossless(),
            vec!["-lossless", "1"],
        ),
        (
            "lossy q75",
            super::Options::lossy(75.0),
            vec!["-quality", "75"],
        ),
    ] {
        if !opts.lossless && !lossy_ready() {
            continue;
        }
        let t = std::time::Instant::now();
        let mut enc = super::AnimEncoder::new(640, 360, opts, 0).unwrap();
        for f in &frames {
            enc.add(f, 40).unwrap();
        }
        let ours = enc.finish().unwrap();
        let ours_t = t.elapsed().as_secs_f64();
        let path = super::oracle::dir().join("anim_ffmpeg.webp");
        let t = std::time::Instant::now();
        let mut a: Vec<String> = [
            "-f", "rawvideo", "-pix_fmt", "rgba", "-s", "640x360", "-r", "25", "-i",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.push(raw.to_str().unwrap().into());
        a.extend(["-c:v", "libwebp_anim"].iter().map(|s| s.to_string()));
        a.extend(args.iter().map(|s| s.to_string()));
        a.push(path.to_str().unwrap().into());
        let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        ffmpeg(&refs);
        let theirs_t = t.elapsed().as_secs_f64();
        let theirs = std::fs::metadata(&path).map_or(0, |m| m.len());
        println!(
            "60 frames 640x360 {label}: ours {} bytes {:.0} ms {:.0} fps; ffmpeg libwebp_anim {} bytes {:.0} ms {:.0} fps",
            ours.len(),
            ours_t * 1e3,
            60.0 / ours_t,
            theirs,
            theirs_t * 1e3,
            60.0 / theirs_t
        );
    }
}
