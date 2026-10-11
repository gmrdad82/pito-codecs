use super::oracle::{dir, ffmpeg, lib, source};
use super::{Rgba, decode, decode_animation, info};

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
