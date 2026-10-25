use super::oracle::{lib, psnr};
use super::{decode, decode_animation, info};
use crate::media::Rgba;
use crate::testref::{self, Digest, Refs};

#[allow(dead_code)]
#[path = "refs.rs"]
pub(super) mod refs;

use refs::{photo, picture, with_alpha};

fn same(name: &str, ours: &Rgba, theirs: &Rgba) {
    assert_eq!(
        (ours.width, ours.height),
        (theirs.width, theirs.height),
        "{name}: size"
    );
    if let Some(i) = ours.data.iter().zip(&theirs.data).position(|(a, b)| a != b) {
        let px = i / 4;
        panic!(
            "{name}: first difference at pixel ({}, {}) channel {}: ours {} theirs {}",
            px % ours.width as usize,
            px / ours.width as usize,
            i % 4,
            ours.data[i],
            theirs.data[i]
        );
    }
}

fn webp(r: &Refs, name: &str) -> Vec<u8> {
    r.bytes(&format!("{name}.webp"))
}

#[test]
fn lossless_decode_matches_libwebp() {
    let r = Refs::open(refs::AREA);
    let lib = lib();
    for (name, _, size, _) in refs::LOSSLESS {
        let file = webp(&r, name);
        let i = info(&file).unwrap();
        assert!(i.lossless, "{name}");
        let ours = decode(&file).unwrap();
        assert_eq!((ours.width, ours.height), size, "{name}");
        r.assert_digest(&format!("{name}.rgba"), &testref::sha256(&ours.data));
        if let Some(lib) = lib {
            same(name, &ours, &lib.decode_rgba(&file).unwrap());
        }
    }
}

#[test]
fn animation_decode_matches_libwebp() {
    let r = Refs::open(refs::AREA);
    let file = webp(&r, refs::ANIM);
    let ours = decode_animation(&file).unwrap();
    assert_eq!(ours.frames.len() as u64, refs::ANIM_FRAMES);
    let mut d = Digest::new();
    let last = ours.frames.len() - 1;
    for (i, o) in ours.frames.iter().enumerate() {
        d.rgba(&o.image);
        assert_eq!(o.start, i as u32 * refs::ANIM_MS, "frame {i} start time");
        assert!(o.duration > 0, "frame {i} has no duration");
        if i < last {
            assert_eq!(o.duration, refs::ANIM_MS, "frame {i} duration");
        }
    }
    r.assert_digest(&format!("{}.rgba", refs::ANIM), &d.hex());
    let Some(theirs) = lib().and_then(|l| l.decode_animation(&file)) else {
        return;
    };
    assert_eq!(ours.frames.len(), theirs.len());
    for (i, (o, (t, ts))) in ours.frames.iter().zip(&theirs).enumerate() {
        same(&format!("frame {i}"), &o.image, t);
        assert_eq!((o.start + o.duration) as i32, *ts, "frame {i} timestamp");
    }
}

#[test]
fn lossless_encode_decodes_exactly_in_libwebp_and_ffmpeg() {
    let r = Refs::open(refs::AREA);
    let lib = lib();
    for (name, img) in &refs::lossless_cases() {
        for effort in [0, 4, 6] {
            let file = super::encode(img, &refs::lossless_options(effort)).unwrap();
            same(
                &format!("{name} effort {effort} ours"),
                &decode(&file).unwrap(),
                img,
            );
            if effort == refs::PINNED_EFFORT && refs::PINNED_LOSSLESS.contains(&name.as_str()) {
                r.assert_digest(&format!("our-ll-{name}.webp"), &testref::sha256(&file));
                r.assert_digest(&format!("our-ll-{name}.rgba"), &testref::sha256(&img.data));
            }
            if let Some(lib) = lib {
                let theirs = lib
                    .decode_rgba(&file)
                    .unwrap_or_else(|| panic!("{name} effort {effort}: libwebp refused"));
                same(&format!("{name} effort {effort}"), &theirs, img);
            }
        }
    }
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
        let img = photo(w, h);
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
    let frames = testref::rgba_frames(160, 96, 6, 0);
    let mut enc = super::AnimEncoder::new(160, 96, super::Options::lossless(), 0).unwrap();
    for (i, f) in frames.iter().enumerate() {
        enc.add(f, 40 + i as u32).unwrap();
    }
    enc.add(&frames[5], 10).unwrap();
    let file = enc.finish().unwrap();
    let ours = decode_animation(&file).unwrap();
    assert_eq!(ours.frames.len(), 6);
    for (i, f) in ours.frames.iter().enumerate() {
        same(&format!("ours frame {i}"), &f.image, &frames[i]);
    }
    let Some(lib) = lib() else { return };
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
}

#[test]
fn damaged_files_fail_without_panics() {
    let r = Refs::open(refs::AREA);
    let mut files: Vec<Vec<u8>> = refs::LOSSLESS[..3]
        .iter()
        .map(|(name, ..)| webp(&r, name))
        .collect();
    files.push(webp(&r, refs::ANIM));
    let frames = testref::rgba_frames(384, 352, 3, 0);
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
    for (src, pic) in [
        ("pattern", refs::Pic::Pattern(0)),
        ("bars", refs::Pic::Bars),
        ("ramp", refs::Pic::Ramp),
        ("noisy 3", refs::Pic::Noisy(1, 3)),
        ("noisy 20", refs::Pic::Noisy(2, 20)),
    ] {
        let img = picture(pic, 1280, 720);
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

fn yuva(file: &[u8], alpha: Option<&Rgba>) -> String {
    let body = super::riff::body(file).unwrap();
    let vp8 = super::riff::Chunks::new(body)
        .filter_map(|c| c.ok())
        .find(|c| &c.id == b"VP8 ")
        .expect("a VP8 chunk")
        .data;
    let f = super::vp8::decode_yuv(vp8).unwrap();
    let (w, h) = (f.width, f.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut d = Digest::new();
    for r in 0..h {
        d.bytes(&f.y[r * f.ys..r * f.ys + w]);
    }
    for plane in [&f.u, &f.v] {
        for r in 0..ch {
            d.bytes(&plane[r * f.uvs..r * f.uvs + cw]);
        }
    }
    if let Some(img) = alpha {
        let a: Vec<u8> = img.data.chunks_exact(4).map(|p| p[3]).collect();
        d.bytes(&a);
    }
    d.hex()
}

fn opaque_rgb(img: &Rgba, keep: &[usize]) -> Vec<u8> {
    keep.iter()
        .flat_map(|&i| img.data[i * 4..i * 4 + 3].to_vec())
        .collect()
}

#[test]
fn lossy_alpha_decodes_the_same_in_libwebp() {
    if !lossy_ready() {
        return;
    }
    let r = Refs::open(refs::AREA);
    let img = refs::alpha_image();
    let file = super::encode(&img, &super::Options::lossy(refs::ALPHA_QUALITY)).unwrap();
    r.assert_digest(
        &format!("{}.webp", refs::LOSSY_ALPHA),
        &testref::sha256(&file),
    );
    let i = info(&file).unwrap();
    assert!(i.alpha && !i.lossless);
    let ours = decode(&file).unwrap();
    r.assert_digest(
        &format!("{}.yuva420p", refs::LOSSY_ALPHA),
        &yuva(&file, Some(&ours)),
    );
    let alpha_in: Vec<u8> = img.data.chunks_exact(4).map(|p| p[3]).collect();
    let alpha_out: Vec<u8> = ours.data.chunks_exact(4).map(|p| p[3]).collect();
    assert_eq!(alpha_in, alpha_out, "alpha is lossless");
    let opaque: Vec<usize> = (0..alpha_in.len()).filter(|&i| alpha_in[i] > 0).collect();
    let a = opaque_rgb(&img, &opaque);
    let db = psnr(&a, &opaque_rgb(&ours, &opaque));
    let lw = format!("lw-{}", refs::LOSSY_ALPHA);
    let lw_file = webp(&r, &lw);
    let reference = decode(&lw_file).unwrap();
    r.assert_digest(&format!("{lw}.yuva420p"), &yuva(&lw_file, Some(&reference)));
    let floor = psnr(&a, &opaque_rgb(&reference, &opaque)) - 1.5;
    assert!(
        db > floor,
        "lossy RGB PSNR {db:.1} dB, libwebp's at the same quality less 1.5 dB is {floor:.1}"
    );
    if let Some(lib) = lib() {
        let theirs = lib
            .decode_rgba(&file)
            .expect("libwebp decodes lossy + alpha");
        same("lossy alpha", &ours, &theirs);
    }
}

#[test]
fn lossy_animation_plays_back_in_libwebp() {
    if !lossy_ready() {
        return;
    }
    let r = Refs::open(refs::AREA);
    let frames = refs::lossy_anim_frames();
    let file = refs::encode_lossy_anim().unwrap();
    let ours = decode_animation(&file).unwrap();
    assert_eq!(ours.info.loop_count, 1);
    assert_eq!(ours.frames.len(), frames.len());
    for (i, o) in ours.frames.iter().enumerate() {
        let db = psnr(&o.image.data, &frames[i].data);
        let still = refs::lossy_anim_still(i as u64);
        let still_file = webp(&r, &still);
        r.assert_digest(&format!("{still}.yuv420p"), &yuva(&still_file, None));
        let reference = decode(&still_file).unwrap();
        let floor = psnr(&reference.data, &frames[i].data) - 1.5;
        assert!(
            db > floor,
            "frame {i} PSNR {db:.1} dB, libwebp's still less 1.5 dB is {floor:.1}"
        );
    }
    let Some(theirs) = lib().map(|l| {
        l.decode_animation(&file)
            .expect("libwebp decodes our lossy animation")
    }) else {
        return;
    };
    assert_eq!(theirs.len(), frames.len());
    for (i, ((t, _), o)) in theirs.iter().zip(&ours.frames).enumerate() {
        same(&format!("lossy frame {i}"), &o.image, t);
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
    let mut soft = photo(1280, 720);
    let w = soft.width as usize;
    for (k, p) in soft.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((k % w) as f32, (k / w) as f32);
        p[3] = (255.0
            * (1.0 - ((x - 640.0).powi(2) + (y - 360.0).powi(2)).sqrt() / 700.0).clamp(0.0, 1.0))
            as u8;
    }
    for (name, img) in [
        (
            "blocks",
            with_alpha(picture(refs::Pic::Pattern(0), 1280, 720)),
        ),
        ("soft", soft),
    ] {
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
fn lossy_case_names_are_unique_and_never_a_lossless_pictures_name() {
    for (i, case) in refs::LOSSY.iter().enumerate() {
        assert!(
            refs::LOSSY[..i].iter().all(|c| c.name != case.name),
            "{} twice",
            case.name
        );
        assert_ne!(case.name, "bars", "the bars picture is lossless only");
    }
}
