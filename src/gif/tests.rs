use super::*;
use crate::media::Rgba;
use std::path::{Path, PathBuf};
use std::process::Command;

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/gif");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn path(name: &str) -> String {
    dir().join(name).to_string_lossy().into_owned()
}

fn ffmpeg_present() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("ffmpeg not found, skipping");
    }
    ok
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn raw_frames(file: &str, w: u32, h: u32) -> Vec<Rgba> {
    let bytes = std::fs::read(path(file)).unwrap();
    let n = (w * h * 4) as usize;
    assert_eq!(bytes.len() % n, 0);
    bytes
        .chunks_exact(n)
        .map(|c| Rgba {
            width: w,
            height: h,
            data: c.to_vec(),
        })
        .collect()
}

fn source(name: &str, w: u32, h: u32, frames: u32) -> Vec<Rgba> {
    let file = format!("{name}-{w}x{h}-{frames}.rgba");
    crate::fixture::once(&dir().join(&file), |part| {
        ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={w}x{h}:rate=10"),
            "-frames:v",
            &frames.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            &part.to_string_lossy(),
        ]);
    });
    raw_frames(&file, w, h)
}

fn ffmpeg_decode(gif: &str, w: u32, h: u32) -> Vec<Rgba> {
    let out = format!("{gif}.rgba");
    ffmpeg(&[
        "-i",
        &path(gif),
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        &path(&out),
    ]);
    raw_frames(&out, w, h)
}

fn ours(gif: &str) -> Animation {
    decode(&std::fs::read(path(gif)).unwrap()).unwrap()
}

fn same(a: &Rgba, b: &Rgba) -> bool {
    a.data
        .chunks_exact(4)
        .zip(b.data.chunks_exact(4))
        .all(|(p, q)| p == q || (p[3] == 0 && q[3] == 0))
}

fn assert_frames(ours: &Animation, theirs: &[Rgba], what: &str) {
    assert_eq!(ours.frames.len(), theirs.len(), "{what}: frame count");
    for (i, ((a, _), b)) in ours.frames.iter().zip(theirs).enumerate() {
        if !same(a, b) {
            let bad = a
                .data
                .chunks_exact(4)
                .zip(b.data.chunks_exact(4))
                .position(|(p, q)| p != q && !(p[3] == 0 && q[3] == 0))
                .unwrap();
            panic!(
                "{what}: frame {i} differs at pixel {bad}: ours {:?} ffmpeg {:?}",
                &a.data[bad * 4..bad * 4 + 4],
                &b.data[bad * 4..bad * 4 + 4]
            );
        }
    }
}

fn psnr(a: &Rgba, b: &Rgba) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (p, q) in a.data.chunks_exact(4).zip(b.data.chunks_exact(4)) {
        if p[3] == 0 || q[3] == 0 {
            continue;
        }
        for c in 0..3 {
            let d = p[c] as f64 - q[c] as f64;
            se += d * d;
        }
        n += 3.0;
    }
    10.0 * (255.0f64 * 255.0 / (se / n).max(1e-9)).log10()
}

#[test]
fn decodes_ffmpeg_gifs_exactly() {
    if !ffmpeg_present() {
        return;
    }
    let (w, h) = (320, 240);
    source("src", w, h, 30);
    let input = path("src-320x240-30.rgba");
    let size = format!("{w}x{h}");
    let cases = [
        ("ff-global.gif", "palettegen[p];[s1][p]paletteuse"),
        (
            "ff-local.gif",
            "palettegen=stats_mode=single[p];[s1][p]paletteuse=new=1:dither=bayer",
        ),
        (
            "ff-rect.gif",
            "palettegen=max_colors=32[p];[s1][p]paletteuse=diff_mode=rectangle:dither=none",
        ),
    ];
    for (name, graph) in cases {
        ffmpeg(&[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &size,
            "-r",
            "10",
            "-i",
            &input,
            "-filter_complex",
            &format!("split[s0][s1];[s0]{graph}"),
            &path(name),
        ]);
        let a = ours(name);
        assert_eq!((a.width, a.height), (w, h));
        assert_frames(&a, &ffmpeg_decode(name, w, h), name);
    }
}

#[test]
fn ffmpeg_decodes_ours_exactly() {
    if !ffmpeg_present() {
        return;
    }
    let (w, h) = (320, 240);
    let src = source("src", w, h, 30);
    let delays = vec![10u16; src.len()];
    let modes = [
        (
            "our-global-fs.gif",
            PaletteMode::Global,
            Dither::FloydSteinberg,
            256,
        ),
        ("our-global-none.gif", PaletteMode::Global, Dither::None, 64),
        (
            "our-local-ordered.gif",
            PaletteMode::PerFrame,
            Dither::Ordered,
            128,
        ),
    ];
    for (name, palette, dither, colors) in modes {
        let opts = EncodeOptions {
            colors,
            dither,
            palette,
            ..EncodeOptions::default()
        };
        let bytes = encode(&src, &delays, opts).unwrap();
        std::fs::write(path(name), &bytes).unwrap();
        let a = ours(name);
        assert_eq!(a.loop_count, Some(0));
        assert_eq!(a.frames.len(), src.len(), "{name}");
        assert_frames(&a, &ffmpeg_decode(name, w, h), name);
        for (i, ((f, _), s)) in a.frames.iter().zip(&src).enumerate() {
            let p = psnr(f, s);
            assert!(p > 26.0, "{name}: frame {i} PSNR {p:.1}");
        }
    }
}

#[test]
fn batches_do_not_change_the_output() {
    if !ffmpeg_present() {
        return;
    }
    let src = source("src", 160, 120, 30);
    let delays = vec![4u16; src.len()];
    let small = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    for dither in [Dither::FloydSteinberg, Dither::Ordered] {
        let opts = EncodeOptions {
            dither,
            ..EncodeOptions::default()
        };
        let whole = encode(&src, &delays, opts).unwrap();
        let chunked = small.install(|| encode(&src, &delays, opts).unwrap());
        assert!(whole == chunked, "{dither:?}: encode in batches");
        let refs: Vec<&Rgba> = src.iter().collect();
        let palette = Palette::from_frames(&refs, 255);
        let streamed = small.install(|| {
            let mut e = Encoder::with_palette(160, 120, opts, palette).unwrap();
            for (f, &d) in src.iter().zip(&delays) {
                e.push(f.clone(), d).unwrap();
            }
            e.finish()
        });
        assert!(whole == streamed, "{dither:?}: streamed");
    }
}

#[test]
fn transparency_and_still_frames_survive() {
    if !ffmpeg_present() {
        return;
    }
    let (w, h) = (97u32, 61u32);
    let mut frames = Vec::new();
    for k in 0..8u32 {
        let mut f = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let hole = (k == 1 || k == 4) && (x + k * 7) % 23 < 9;
                let v = if k == 4 { 3 } else { k };
                f.data[i..i + 4].copy_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y + v * 40) % 256) as u8,
                    if hole { 0 } else { 255 },
                ]);
            }
        }
        frames.push(f);
    }
    frames[6] = frames[5].clone();
    let delays: Vec<u16> = (0..8).map(|k| 5 + k).collect();
    let opts = EncodeOptions {
        dither: Dither::None,
        ..EncodeOptions::default()
    };
    let bytes = encode(&frames, &delays, opts).unwrap();
    std::fs::write(path("alpha.gif"), &bytes).unwrap();
    let a = ours("alpha.gif");
    assert_eq!(a.frames.len(), 7);
    assert_eq!(a.frames[5].1, delays[5] + delays[6]);
    let kept: Vec<usize> = vec![0, 1, 2, 3, 4, 5, 7];
    for (j, &k) in kept.iter().enumerate() {
        let got = &a.frames[j].0;
        for (p, q) in got.data.chunks_exact(4).zip(frames[k].data.chunks_exact(4)) {
            assert_eq!(p[3], q[3], "frame {k} alpha");
        }
        let p = psnr(got, &frames[k]);
        assert!(p > 22.0, "frame {k}: PSNR {p:.1}");
    }
    assert_frames(&a, &ffmpeg_decode("alpha.gif", w, h), "alpha.gif");
}

#[test]
fn lzw_round_trips_noise_and_runs() {
    let mut seed = 0x1234_5678u32;
    let mut data = Vec::new();
    for i in 0..300_000 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        data.push(if i % 5000 < 2500 { 7 } else { seed as u8 });
    }
    for bits in [2u8, 5, 8] {
        let mask = ((1u16 << bits) - 1) as u8;
        let src: Vec<u8> = data.iter().map(|v| v & mask).collect();
        let mut out = Vec::new();
        lzw::LzwEncoder::default().encode(bits, src.chunks(997), &mut out);
        let mut joined = Vec::new();
        let mut at = 1;
        while out[at] != 0 {
            let n = out[at] as usize;
            joined.extend_from_slice(&out[at + 1..at + 1 + n]);
            at += 1 + n;
        }
        let mut back = vec![0; src.len()];
        let n = lzw::LzwDecoder::default().decode(bits, &joined, &mut back);
        assert_eq!(n, src.len());
        assert!(back == src, "{bits} bits");
    }
}

#[test]
fn decodes_interlaced_disposal_and_gif87a_exactly() {
    if !ffmpeg_present()
        || !Command::new("magick")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return;
    }
    let magick = |args: &str| {
        let out = Command::new("magick")
            .current_dir(dir())
            .args(args.split('|'))
            .output()
            .unwrap();
        assert!(out.status.success(), "magick {args}");
    };
    magick("-seed|7|-size|161x117|plasma:|-colors|200|-interlace|GIF|im-interlaced.gif");
    magick(
        "-size|120x90|xc:none|-delay|7|-dispose|Previous|(|-size|120x90|xc:none|-fill|red\
         |-draw|circle 40,40 40,15|)|-dispose|Background|(|-size|120x90|xc:none|-fill|blue\
         |-draw|rectangle 50,20 110,80|)|-dispose|None|(|-size|120x90|gradient:yellow-green|)\
         |-loop|3|im-dispose.gif",
    );
    magick("-seed|3|-size|64x48|plasma:|GIF87:im-87.gif");
    for (name, w, h) in [
        ("im-interlaced.gif", 161, 117),
        ("im-dispose.gif", 120, 90),
        ("im-87.gif", 64, 48),
    ] {
        let a = ours(name);
        assert_frames(&a, &ffmpeg_decode(name, w, h), name);
    }
    assert_eq!(ours("im-dispose.gif").loop_count, Some(2));
}

fn timed(f: impl FnOnce()) -> f64 {
    let t = std::time::Instant::now();
    f();
    t.elapsed().as_secs_f64()
}

#[test]
#[ignore]
fn speed_against_ffmpeg() {
    for (w, h) in [(640u32, 360u32), (1280, 720)] {
        let name = format!("coast-{w}x{h}.rgba");
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench")
            .join(&name);
        let Ok(bytes) = std::fs::read(&file) else {
            eprintln!("{} missing, skipping", file.display());
            continue;
        };
        let n = (w * h * 4) as usize;
        let src: Vec<Rgba> = bytes
            .chunks_exact(n)
            .map(|c| Rgba {
                width: w,
                height: h,
                data: c.to_vec(),
            })
            .collect();
        let count = src.len() as f64;
        let delays = vec![3u16; src.len()];
        let size = format!("{w}x{h}");
        let input = file.to_string_lossy().into_owned();
        for (label, opts, graph) in [
            (
                "global fs",
                EncodeOptions::default(),
                "palettegen[p];[s1][p]paletteuse=dither=floyd_steinberg",
            ),
            (
                "global none",
                EncodeOptions {
                    dither: Dither::None,
                    ..EncodeOptions::default()
                },
                "palettegen[p];[s1][p]paletteuse=dither=none",
            ),
            (
                "global ordered",
                EncodeOptions {
                    dither: Dither::Ordered,
                    ..EncodeOptions::default()
                },
                "palettegen[p];[s1][p]paletteuse=dither=bayer",
            ),
            (
                "per-frame ordered",
                EncodeOptions {
                    dither: Dither::Ordered,
                    palette: PaletteMode::PerFrame,
                    ..EncodeOptions::default()
                },
                "palettegen=stats_mode=single[p];[s1][p]paletteuse=new=1:dither=bayer",
            ),
        ] {
            let mut gif = Vec::new();
            let ours_s = timed(|| gif = encode(&src, &delays, opts).unwrap());
            let ours_name = format!("speed-ours-{w}.gif");
            std::fs::write(path(&ours_name), &gif).unwrap();
            let ff_name = format!("speed-ff-{w}.gif");
            let ff_s = timed(|| {
                ffmpeg(&[
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgba",
                    "-s",
                    &size,
                    "-r",
                    "30",
                    "-i",
                    &input,
                    "-filter_complex",
                    &format!("split[s0][s1];[s0]{graph}"),
                    &path(&ff_name),
                ])
            });
            let a = ours(&ours_name);
            let ff_len = std::fs::metadata(path(&ff_name)).unwrap().len();
            let b = ffmpeg_decode(&ff_name, w, h);
            let pa: f64 = a
                .frames
                .iter()
                .zip(&src)
                .map(|(f, s)| psnr(&f.0, s))
                .sum::<f64>()
                / a.frames.len() as f64;
            let pb: f64 = b.iter().zip(&src).map(|(f, s)| psnr(f, s)).sum::<f64>() / b.len() as f64;
            eprintln!(
                "encode {w}x{h} {label}: ours {:.0} fps {:.2} MB {pa:.2} dB, ffmpeg {:.0} fps {:.2} MB {pb:.2} dB",
                count / ours_s,
                gif.len() as f64 / 1e6,
                count / ff_s,
                ff_len as f64 / 1e6
            );
        }
        let ours_name = format!("speed-ours-{w}.gif");
        let data = std::fs::read(path(&ours_name)).unwrap();
        let mut frames = 0;
        let mut first = 0.0;
        let ours_s = timed(|| {
            let t = std::time::Instant::now();
            let mut d = Decoder::new(&data).unwrap();
            while d.next_frame().unwrap().is_some() {
                if frames == 0 {
                    first = t.elapsed().as_secs_f64();
                }
                frames += 1;
            }
        });
        let ff_s = timed(|| ffmpeg(&["-i", &path(&ours_name), "-f", "null", "-"]));
        eprintln!(
            "decode {w}x{h}: ours {:.0} fps (first frame {:.1} ms), ffmpeg {:.0} fps",
            frames as f64 / ours_s,
            first * 1e3,
            frames as f64 / ff_s
        );
    }
}

fn survive(bad: &[u8]) {
    if let Ok(mut d) = Decoder::new(bad) {
        let mut n = 0;
        while let Ok(Some(_)) = d.next_frame() {
            n += 1;
            if n > 1000 {
                break;
            }
        }
    }
    let _ = decode(bad);
}

#[test]
fn damaged_files_never_panic() {
    let mut frames = Vec::new();
    for k in 0..4u32 {
        let mut f = Rgba::new(40, 30);
        for (i, p) in f.data.chunks_exact_mut(4).enumerate() {
            let v = (i as u32 * 7 + k * 50) as u8;
            p.copy_from_slice(&[v, v ^ 0x5A, (i as u32 / 40 * 8) as u8, 255]);
        }
        frames.push(f);
    }
    let good = encode(&frames, &[5; 4], EncodeOptions::default()).unwrap();
    assert_eq!(decode(&good).unwrap().frames.len(), 4);
    for cut in (0..300).chain((300..good.len()).step_by(37)) {
        survive(&good[..cut]);
    }
    let images: Vec<usize> = (0..good.len())
        .filter(|&i| i >= 3 && good[i] == 0x2C && good[i - 3] == 0)
        .collect();
    let mut fields: Vec<usize> = (6..13).collect();
    for &at in &images {
        fields.extend(at + 1..(at + 11).min(good.len()));
    }
    let mut seed = 0x9E37_79B9u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    for round in 0..600 {
        let mut bad = good.clone();
        for _ in 0..1 + round % 4 {
            let at = if round % 2 == 0 {
                fields[next() % fields.len()]
            } else {
                next() % bad.len()
            };
            bad[at] = match next() % 4 {
                0 => 0,
                1 => 0xFF,
                _ => next() as u8,
            };
        }
        if round % 5 == 0 {
            bad.truncate(next() % bad.len());
        }
        survive(&bad);
    }
    let mut tiny = b"GIF89a\xff\xff\xff\xff\x00\x00\x00".to_vec();
    assert!(Decoder::new(&tiny).is_err());
    tiny[6..10].copy_from_slice(&[1, 0, 1, 0]);
    tiny.extend_from_slice(&[
        0x2C, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0x40, 11, 2, 0xff, 0xff, 0, 0x3B,
    ]);
    survive(&tiny);
}

#[test]
fn bad_encoder_input_is_an_error() {
    let opts = EncodeOptions::default();
    assert!(encode(&[], &[], opts).is_err());
    let frames = [Rgba::new(8, 8), Rgba::new(8, 9)];
    assert!(encode(&frames, &[], opts).is_err());
    let short = Rgba {
        width: 8,
        height: 8,
        data: vec![0; 10],
    };
    assert!(encode(std::slice::from_ref(&short), &[], opts).is_err());
    assert!(encode(&[Rgba::new(0, 4)], &[], opts).is_err());
    let mut e = Encoder::new(8, 8, opts).unwrap();
    assert!(e.push(short, 1).is_err());
    assert!(e.push(Rgba::new(4, 4), 1).is_err());
    assert!(Palette::new(Vec::new()).is_err());
}

#[test]
fn small_changes_keep_and_a_fade_redraws_whole() {
    let (w, h) = (96u32, 64u32);
    let frame = |lift: u8, noise: bool, square: bool| {
        let mut f = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let jitter = if noise && (x * 7 + y * 3) % 5 == 0 {
                    8
                } else {
                    0
                };
                let v = ((x / 12 + y / 16) % 8) as u8 * 24 + 20 + lift + jitter;
                let inside = (30..40).contains(&x) && (20..30).contains(&y);
                let p = if square && inside {
                    [240, 16, 16, 255]
                } else {
                    [v, v, 255 - v, 255]
                };
                let i = ((y * w + x) * 4) as usize;
                f.data[i..i + 4].copy_from_slice(&p);
            }
        }
        f
    };
    let mut frames = vec![frame(0, false, false), frame(0, true, true)];
    frames.extend((1..=3).map(|k| frame(k * 4, false, false)));
    let opts = EncodeOptions::default();
    let a = decode(&encode(&frames, &[5; 5], opts).unwrap()).unwrap();
    assert_eq!((a.loop_count, a.frames.len()), (Some(0), 5));
    let (first, next) = (&a.frames[0].0, &a.frames[1].0);
    assert!(same(first, &frames[0]), "the first frame is exact");
    let pixels = next
        .data
        .chunks_exact(4)
        .zip(first.data.chunks_exact(4))
        .zip(frames[1].data.chunks_exact(4));
    for (i, ((p, q), s)) in pixels.enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let off = (0..3).map(|c| p[c].abs_diff(s[c])).max().unwrap_or(0);
        assert!(off <= opts.tolerance, "{off} levels off at {x},{y}");
        if (30..40).contains(&x) && (20..30).contains(&y) {
            assert_eq!(p, s, "the moved square is drawn at {x},{y}");
        } else if x >= 56 || y >= 48 {
            assert_eq!(p, q, "noise under the tolerance is kept at {x},{y}");
        }
    }
    assert!(
        same(&a.frames[3].0, &frames[3]),
        "a fade under the tolerance redraws the whole frame"
    );
}
