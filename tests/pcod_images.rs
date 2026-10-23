#![cfg(feature = "cli")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pito_codecs::media::Rgba;
use pito_codecs::{gif, png, webp};
use serde_json::Value;

fn dir(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/pcod-images")
        .join(name);
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pcod"))
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn ok(out: &Output) {
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

fn refused(out: &Output, code: i32) -> String {
    assert_eq!(out.status.code(), Some(code), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    if code == 1 {
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(err.starts_with("pcod: "), "{err}");
    } else {
        assert!(err.starts_with("error: "), "{err}");
    }
    err
}

fn pattern(width: u32, height: u32, alpha: bool) -> Rgba {
    let mut img = Rgba::new(width, height);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        px.copy_from_slice(&[
            (x * 255 / width) as u8,
            (y * 255 / height) as u8,
            ((x + y) * 2) as u8,
            if alpha {
                ((x * 3 + y) % 256) as u8
            } else {
                255
            },
        ]);
    }
    img
}

fn rgb_png(img: &Rgba) -> Vec<u8> {
    let rgb: Vec<u8> = img
        .data
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    let mut out = Vec::new();
    let mut enc = ::png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(::png::ColorType::Rgb);
    enc.set_depth(::png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    out
}

fn put(folder: &Path, name: &str, data: &[u8]) -> PathBuf {
    let path = folder.join(name);
    fs::write(&path, data).unwrap();
    path
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn probe(path: &Path) -> Value {
    let out = run(&["probe", "--json", s(path)]);
    ok(&out);
    serde_json::from_slice(&out.stdout).unwrap()
}

fn gif_file(delays: &[u16]) -> Vec<u8> {
    let frames: Vec<Rgba> = (0..delays.len())
        .map(|n| {
            let mut img = pattern(64, 48, false);
            for px in img.data.chunks_exact_mut(4).skip(n * 7).take(30) {
                px[0] ^= 0x80;
            }
            img
        })
        .collect();
    let opts = gif::EncodeOptions {
        diff: false,
        ..gif::EncodeOptions::default()
    };
    gif::encode(&frames, delays, opts).unwrap()
}

fn jpeg_header(width: u16, height: u16) -> Vec<u8> {
    let mut d = vec![0xff, 0xd8, 0xff, 0xc0, 0, 17, 8];
    d.extend_from_slice(&height.to_be_bytes());
    d.extend_from_slice(&width.to_be_bytes());
    d.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    d.extend_from_slice(&[0xff, 0xd9]);
    d
}

fn psnr(a: &Rgba, b: &Rgba) -> f64 {
    let mse = a
        .data
        .chunks_exact(4)
        .zip(b.data.chunks_exact(4))
        .flat_map(|(p, q)| (0..3).map(move |c| (f64::from(p[c]) - f64::from(q[c])).powi(2)))
        .sum::<f64>()
        / (a.data.len() / 4 * 3) as f64;
    10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
}

fn chunks(data: &[u8]) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    let mut at = 12;
    while let Some(h) = data.get(at..at + 8) {
        out.push([h[0], h[1], h[2], h[3]]);
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        at += 8 + len + (len & 1);
    }
    out
}

#[test]
fn probe_reads_each_still_format() {
    let out = dir("probe");
    let rgba = put(
        &out,
        "a.png",
        &png::encode_rgba(&pattern(30, 20, true)).unwrap(),
    );
    let rgb = put(&out, "b.png", &rgb_png(&pattern(31, 21, false)));
    let jpg = put(&out, "c.jpg", &jpeg_header(640, 360));
    let still = put(
        &out,
        "d.webp",
        &webp::encode(&pattern(33, 22, false), &webp::Options::lossy(80.0)).unwrap(),
    );
    for (path, w, h, codec, pix, alpha) in [
        (&rgba, 30, 20, "png", "rgba", true),
        (&rgb, 31, 21, "png", "rgb24", false),
        (&jpg, 640, 360, "mjpeg", "yuvj420p", false),
        (&still, 33, 22, "webp", "yuv420p", false),
    ] {
        let v = probe(path);
        assert_eq!(v["width"], w, "{v}");
        assert_eq!(v["height"], h, "{v}");
        assert_eq!(v["codec"], codec, "{v}");
        assert_eq!(v["pix_fmt"], pix, "{v}");
        assert_eq!(v["alpha"], alpha, "{v}");
        assert_eq!(v["frames"], 1, "{v}");
        assert!(v["audio"].is_null(), "{v}");
        assert!(v["fps"].is_null(), "{v}");
        assert!(v["seconds"].is_null(), "{v}");
        assert!(v["audio_index"].is_null(), "{v}");
        let shown = run(&["probe", s(path)]);
        ok(&shown);
        assert!(
            text(&shown.stdout).contains(&format!("video      {codec} {w}x{h}")),
            "{}",
            text(&shown.stdout)
        );
    }
}

#[test]
fn probe_reads_a_gifs_rate_frames_and_seconds() {
    let out = dir("probe-gif");
    let ntsc: Vec<u16> = [3, 3, 4].iter().copied().cycle().take(17).collect();
    let cases: [(&str, Vec<u16>, f64, u64, f64); 6] = [
        ("uniform-4", vec![4; 10], 25.0, 10, 0.4),
        ("uniform-3-ffprobe-33", vec![3; 6], 100.0 / 3.0, 6, 0.18),
        ("uniform-3-long", vec![3; 20], 100.0 / 3.0, 20, 0.6),
        ("fifteen-ffprobe-ntsc", [7, 7, 6].repeat(5), 15.0, 15, 1.0),
        ("thirty", [3, 3, 4].repeat(10), 30.0, 30, 1.0),
        ("ragged-ffprobe-ntsc", ntsc, 30.357, 17, 0.56),
    ];
    for (name, delays, fps, frames, seconds) in cases {
        let path = put(&out, &format!("{name}.gif"), &gif_file(&delays));
        let v = probe(&path);
        assert_eq!(v["codec"], "gif", "{name}");
        assert_eq!(v["width"], 64, "{name}");
        assert_eq!(v["height"], 48, "{name}");
        assert_eq!(v["frames"], frames, "{name}");
        assert!(
            (v["fps"].as_f64().unwrap() - fps).abs() < 1e-6,
            "{name}: {v}"
        );
        assert!(
            (v["seconds"].as_f64().unwrap() - seconds).abs() < 1e-6,
            "{name}: {v}"
        );
        assert!(v["audio"].is_null());
    }
    let shown = run(&["probe", s(&out.join("uniform-4.gif"))]);
    ok(&shown);
    let shown = text(&shown.stdout);
    assert!(shown.contains("container  GIF"), "{shown}");
    assert!(shown.contains("64x48 25.00 fps, 10 frames"), "{shown}");
}

#[test]
fn probe_reads_an_animated_webp() {
    let out = dir("probe-anim");
    let mut anim = webp::AnimEncoder::new(48, 32, webp::Options::lossy(70.0), 0).unwrap();
    for n in 0..5u8 {
        let mut img = pattern(48, 32, false);
        img.data[0] = n * 40;
        anim.add(&img, 100).unwrap();
    }
    let path = put(&out, "anim.webp", &anim.finish().unwrap());
    let v = probe(&path);
    assert_eq!(v["codec"], "webp");
    assert_eq!(v["width"], 48);
    assert_eq!(v["frames"], 5);
    assert_eq!(v["fps"].as_f64(), Some(10.0));
    assert_eq!(v["seconds"].as_f64(), Some(0.5));
}

#[test]
fn probe_refuses_broken_images_in_one_line() {
    let out = dir("probe-bad");
    let data = png::encode(&pattern(30, 20, false)).unwrap();
    let cut = put(&out, "cut.png", &data[..20]);
    let err = refused(&run(&["probe", "--json", s(&cut)]), 1);
    assert!(err.contains("cut.png"), "{err}");
    refused(&run(&["probe", s(&cut)]), 1);
    let gif = gif_file(&[4, 4]);
    let half = put(&out, "half.gif", &gif[..gif.len() / 2]);
    refused(&run(&["probe", "--json", s(&half)]), 1);
    let jpg = put(&out, "zero.jpg", &jpeg_header(640, 0));
    refused(&run(&["probe", "--json", s(&jpg)]), 1);
}

#[test]
fn a_png_becomes_a_still_webp_at_each_quality() {
    let out = dir("lossy");
    let src = pattern(320, 180, false);
    let input = put(&out, "in.png", &rgb_png(&src));
    let mut sizes = Vec::new();
    for q in [80, 82, 86] {
        let output = out.join(format!("q{q}.webp"));
        ok(&run(&[s(&input), s(&output), "--quality", &q.to_string()]));
        let data = fs::read(&output).unwrap();
        let ids = chunks(&data);
        assert!(!ids.contains(b"ANIM"), "{ids:?}");
        assert!(ids.contains(b"VP8 "), "{ids:?}");
        let back = webp::decode(&data).unwrap();
        assert_eq!((back.width, back.height), (320, 180));
        let p = psnr(&src, &back);
        assert!(p > 29.0, "quality {q}: {p:.2} dB");
        sizes.push(data.len());
    }
    assert!(sizes[0] <= sizes[2], "{sizes:?}");
    let default = out.join("default.webp");
    ok(&run(&[s(&input), s(&default)]));
    assert!(!chunks(&fs::read(&default).unwrap()).contains(b"ANIM"));
}

#[test]
fn an_rgba_png_keeps_its_alpha_lossless_and_lossy() {
    let out = dir("alpha");
    let src = pattern(200, 120, true);
    let input = put(&out, "in.png", &png::encode_rgba(&src).unwrap());
    let lossless = out.join("lossless.webp");
    ok(&run(&[s(&input), s(&lossless), "--lossless"]));
    let data = fs::read(&lossless).unwrap();
    let ids = chunks(&data);
    assert_eq!(ids, vec![*b"VP8L"], "{ids:?}");
    assert!(
        src.data
            .chunks_exact(4)
            .any(|p| p[3] == 0 && p[..3] != [0, 0, 0])
    );
    let back = webp::decode(&data).unwrap();
    assert_eq!((back.width, back.height), (200, 120));
    for (b, p) in back.data.chunks_exact(4).zip(src.data.chunks_exact(4)) {
        if p[3] == 0 {
            assert_eq!(b[3], 0);
        } else {
            assert_eq!(b, p);
        }
    }
    let lossy = out.join("lossy.webp");
    ok(&run(&[s(&input), s(&lossy), "--quality", "86"]));
    let data = fs::read(&lossy).unwrap();
    let ids = chunks(&data);
    assert!(ids.contains(b"ALPH") && !ids.contains(b"ANIM"), "{ids:?}");
    let back = webp::decode(&data).unwrap();
    let worst = src
        .data
        .chunks_exact(4)
        .zip(back.data.chunks_exact(4))
        .map(|(a, b)| a[3].abs_diff(b[3]))
        .max()
        .unwrap();
    assert!(worst <= 2, "alpha off by {worst}");
}

#[test]
fn a_webp_still_becomes_a_still_webp() {
    let out = dir("webp-in");
    let src = pattern(160, 90, false);
    let input = put(
        &out,
        "in.webp",
        &webp::encode(&src, &webp::Options::lossless()).unwrap(),
    );
    let output = out.join("out.webp");
    ok(&run(&[s(&input), s(&output), "--quality", "82"]));
    let back = webp::decode(&fs::read(&output).unwrap()).unwrap();
    assert_eq!((back.width, back.height), (160, 90));
    assert!(psnr(&src, &back) > 32.0);
    let again = out.join("again.webp");
    ok(&run(&[s(&input), s(&again), "--lossless"]));
    assert_eq!(
        webp::decode(&fs::read(&again).unwrap()).unwrap().data,
        src.data
    );
}

#[test]
fn size_takes_a_width_or_an_exact_shape() {
    let out = dir("size");
    let input = put(&out, "in.png", &rgb_png(&pattern(1000, 750, false)));
    let wide = out.join("wide.webp");
    ok(&run(&[s(&input), s(&wide)]));
    let v = webp::info(&fs::read(&wide).unwrap()).unwrap();
    assert_eq!((v.width, v.height), (1000, 750));
    for (size, w, h) in [
        ("640", 640, 480),
        ("320x240", 320, 240),
        ("1200", 1200, 900),
    ] {
        let output = out.join(format!("{size}.webp"));
        ok(&run(&[s(&input), s(&output), "--size", size]));
        let v = webp::info(&fs::read(&output).unwrap()).unwrap();
        assert_eq!((v.width, v.height), (w, h), "{size}");
    }
    let odd = put(&out, "odd.png", &rgb_png(&pattern(301, 101, false)));
    let output = out.join("odd.webp");
    ok(&run(&[s(&odd), s(&output), "--size", "100"]));
    let v = webp::info(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(v.width, 100);
    assert_eq!(v.height % 2, 0);
    assert_eq!(v.height, 34);
    let json = run(&[s(&input), s(&out.join("j.webp")), "--size", "640", "--json"]);
    ok(&json);
    let m: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(m["params"]["size"]["width"], 640, "{m}");
    assert_eq!(m["params"]["size"]["height"], 480, "{m}");
    assert_eq!(m["params"]["encoder"]["encoder"], "webp", "{m}");
    let tall = out.join("720p.webp");
    ok(&run(&[s(&input), s(&tall), "--size", "720p"]));
    let v = webp::info(&fs::read(&tall).unwrap()).unwrap();
    assert_eq!((v.width, v.height), (960, 720));
    let huge = out.join("huge.webp");
    let err = refused(&run(&[s(&input), s(&huge), "--size", "60000"]), 1);
    assert!(err.contains("16383"), "{err}");
    assert!(!huge.exists());
}

#[test]
fn an_rgba_still_resizes_with_its_alpha() {
    let out = dir("size-alpha");
    let mut src = Rgba::new(200, 100);
    for (i, px) in src.data.chunks_exact_mut(4).enumerate() {
        px.copy_from_slice(if i % 200 < 100 {
            &[0, 0, 0, 0]
        } else {
            &[230, 60, 20, 255]
        });
    }
    let input = put(&out, "in.png", &png::encode_rgba(&src).unwrap());
    let output = out.join("out.webp");
    ok(&run(&[
        s(&input),
        s(&output),
        "--size",
        "100",
        "--lossless",
    ]));
    let back = webp::decode(&fs::read(&output).unwrap()).unwrap();
    assert_eq!((back.width, back.height), (100, 50));
    let row = &back.data[25 * 400..26 * 400];
    assert_eq!(row[3], 0);
    assert_eq!(&row[396..], &[230, 60, 20, 255]);
    for px in row.chunks_exact(4).filter(|p| p[3] > 16) {
        assert!(px[0] >= 200 && px[1] <= 90, "{px:?}");
    }
}

#[test]
fn bad_still_inputs_are_refused() {
    let out = dir("bad");
    let data = png::encode(&pattern(30, 20, false)).unwrap();
    let good = put(&out, "good.png", &data);
    let cut = put(&out, "cut.png", &data[..data.len() - 30]);
    refused(&run(&[s(&cut), s(&out.join("cut.webp"))]), 1);
    assert!(!out.join("cut.webp").exists());
    let mut zero = data[..33].to_vec();
    zero[16..20].fill(0);
    let zero = put(&out, "zero.png", &zero);
    let err = refused(&run(&[s(&zero), s(&out.join("zero.webp"))]), 1);
    assert!(err.contains("zero width"), "{err}");
    let jpg = put(&out, "in.jpg", &jpeg_header(64, 48));
    let err = refused(&run(&[s(&jpg), s(&out.join("jpg.webp"))]), 1);
    assert!(err.contains("JPEG input is not supported"), "{err}");
    let mut anim = webp::AnimEncoder::new(16, 16, webp::Options::lossy(70.0), 0).unwrap();
    for n in 0..2u8 {
        let mut img = pattern(16, 16, false);
        img.data[0] = n * 99;
        anim.add(&img, 50).unwrap();
    }
    let animated = put(&out, "anim.webp", &anim.finish().unwrap());
    let err = refused(&run(&[s(&animated), s(&out.join("from-anim.webp"))]), 1);
    assert!(err.contains("animated"), "{err}");
    let target = out.join("t.webp");
    let err = refused(&run(&[s(&good), s(&target), "--quality", "101"]), 2);
    assert!(err.contains("101"), "{err}");
    refused(&run(&[s(&good), s(&target), "--quality", "-1"]), 2);
    refused(&run(&[s(&good), s(&target), "--size", "0"]), 2);
    refused(
        &run(&[s(&good), s(&target), "--quality", "80", "--lossless"]),
        2,
    );
    refused(&run(&[s(&good), s(&target), "--fps", "30"]), 2);
    refused(&run(&[s(&good), s(&target), "--crf", "20"]), 2);
    refused(&run(&[s(&good), s(&target), "--size", "64x64"]), 1);
    refused(
        &run(&[s(&good), s(&out.join("t.mp4")), "--quality", "80"]),
        2,
    );
    refused(&run(&[s(&good), s(&out.join("t.wav")), "--lossless"]), 2);
    ok(&run(&[s(&good), s(&target)]));
    let err = refused(&run(&[s(&good), s(&target)]), 1);
    assert!(err.contains("--force"), "{err}");
    ok(&run(&[s(&good), s(&target), "--force", "--lossless"]));
    assert_eq!(chunks(&fs::read(&target).unwrap()), vec![*b"VP8L"]);
    let err = refused(&run(&[s(&target), s(&target), "--force"]), 1);
    assert!(err.contains("same file"), "{err}");
}

fn ui_like(width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        let panel = (x / 80 + y / 60) % 3;
        let mut rgb: [u8; 3] = [[246, 246, 248], [30, 34, 44], [64, 120, 220]][panel as usize];
        let line = y % 20;
        let glyph = (x * 7 + y * 3 + (x / 6) * 13) % 11 < 4;
        if (4..14).contains(&line) && x % 80 > 8 && x % 80 < 70 && glyph {
            rgb = if panel == 0 {
                [20, 20, 24]
            } else {
                [236, 236, 240]
            };
        }
        let shade = ((x + y) / 8 % 3) as u8;
        let alpha = if y < 12 || x < 12 { 0 } else { 255 };
        px.copy_from_slice(&[rgb[0].saturating_add(shade), rgb[1], rgb[2], alpha]);
    }
    img
}

#[test]
fn lossless_size_is_near_ffmpegs_compression_level_six() {
    let has_libwebp = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| text(&o.stdout).contains("libwebp "));
    if !has_libwebp {
        eprintln!("no ffmpeg with libwebp on PATH: skipped");
        return;
    }
    let out = dir("vs-ffmpeg");
    let mut inputs = vec![(
        "ui",
        put(
            &out,
            "ui.png",
            &png::encode_rgba(&ui_like(640, 360)).unwrap(),
        ),
    )];
    for (name, source) in [("testsrc2", "testsrc2"), ("mandelbrot", "mandelbrot")] {
        let path = out.join(format!("{name}.png"));
        let made = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("{source}=size=640x360:rate=1"))
            .args(["-frames:v", "1", "-pix_fmt", "rgb24"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", text(&made.stderr));
        inputs.push((name, path));
    }
    for (name, input) in inputs {
        let (a, b) = lossless_sizes(&out, name, &input);
        eprintln!("{name}: ours {a} bytes, ffmpeg {b} bytes");
        assert!(a <= b * 1.10, "{name}: ours {a} bytes, ffmpeg {b} bytes");
    }
}

fn lossless_sizes(out: &Path, name: &str, input: &Path) -> (f64, f64) {
    let ours = out.join(format!("{name}-ours.webp"));
    ok(&run(&[s(input), s(&ours), "--lossless"]));
    let theirs = out.join(format!("{name}-ffmpeg.webp"));
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i", s(input)])
        .args([
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "6",
        ])
        .arg(&theirs)
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", text(&made.stderr));
    let a = fs::metadata(&ours).unwrap().len() as f64;
    let b = fs::metadata(&theirs).unwrap().len() as f64;
    (a, b)
}
