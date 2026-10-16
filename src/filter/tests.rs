use super::*;
use crate::media::{AudioBuffer, Frame, PixelFormat, Rgba, TimeBase};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/filter");
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn ffmpeg(args: &[&str]) -> String {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-y"])
        .args(args)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "ffmpeg {args:?}: {err}");
    err
}

fn zscale_present() -> bool {
    let ok = has_filter("zscale");
    if !ok {
        eprintln!("this ffmpeg has no zscale (built without zimg), skipping");
    }
    ok
}

fn has_filter(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .any(|w| w == name)
        })
}

fn path(name: &str) -> String {
    dir().join(name).to_string_lossy().into_owned()
}

fn read_f32(name: &str, rate: u32, channels: u16) -> AudioBuffer {
    let bytes = std::fs::read(path(name)).unwrap();
    AudioBuffer {
        samples: bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        rate,
        channels,
    }
}

fn read_yuv(name: &str, format: PixelFormat, width: u32, height: u32) -> Frame {
    let bytes = std::fs::read(path(name)).unwrap();
    let mut f = Frame::new(format, width, height, 0, TimeBase::new(1, 30));
    let mut at = 0;
    for p in f.planes.iter_mut() {
        let n = p.data.len();
        p.data.copy_from_slice(&bytes[at..at + n]);
        at += n;
    }
    f
}

fn write_yuv(name: &str, frame: &Frame) {
    let mut bytes = Vec::new();
    for p in &frame.planes {
        bytes.extend_from_slice(&p.data);
    }
    let part = format!("{}.part-{}", path(name), std::process::id());
    std::fs::write(&part, bytes).unwrap();
    if let Err(e) = std::fs::rename(&part, path(name)) {
        assert!(std::path::Path::new(&path(name)).exists(), "{e}");
        let _ = std::fs::remove_file(&part);
    }
}

fn ffmpeg_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let out = Command::new("ffmpeg").arg("-version").output().unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    })
}

fn make(label: &str, args: &[&str]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ffmpeg_version().hash(&mut h);
    args.hash(&mut h);
    let name = format!("{label}-{:016x}.raw", h.finish());
    if !dir().join(&name).exists() {
        let part = format!(
            "{}.part-{}-{:?}",
            path(&name),
            std::process::id(),
            std::thread::current().id()
        );
        let mut full = vec!["-v", "error"];
        full.extend_from_slice(args);
        full.push(&part);
        ffmpeg(&full);
        if let Err(e) = std::fs::rename(&part, path(&name)) {
            assert!(std::path::Path::new(&path(&name)).exists(), "{e}");
            let _ = std::fs::remove_file(&part);
        }
    }
    name
}

fn psnr(a: &[u8], b: &[u8], wide: bool) -> f64 {
    let (mse, peak) = if wide {
        let v = |d: &[u8]| -> Vec<f64> {
            d.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]) as f64)
                .collect()
        };
        let (x, y) = (v(a), v(b));
        let m = x.iter().zip(&y).map(|(p, q)| (p - q).powi(2)).sum::<f64>() / x.len() as f64;
        (m, 1023.0)
    } else {
        let m = a
            .iter()
            .zip(b)
            .map(|(&p, &q)| (p as f64 - q as f64).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        (m, 255.0)
    };
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

fn frame_psnr(a: &Frame, b: &Frame) -> [f64; 3] {
    let wide = a.format == PixelFormat::Yuv420p10;
    [0, 1, 2].map(|i| psnr(&a.planes[i].data, &b.planes[i].data, wide))
}

fn source_1440p() -> String {
    make(
        "mandelbrot-2560x1440-bt709-tv",
        &[
            "-f",
            "lavfi",
            "-i",
            "mandelbrot=s=2560x1440",
            "-frames:v",
            "1",
            "-vf",
            "scale=out_color_matrix=bt709:out_range=tv:flags=bilinear+accurate_rnd,format=yuv420p",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
    )
}

fn ebur128(input: &str, rate: u32, channels: u16) -> (f64, f64, f64) {
    let log = ffmpeg(&[
        "-f",
        "f32le",
        "-ar",
        &rate.to_string(),
        "-ac",
        &channels.to_string(),
        "-i",
        &path(input),
        "-af",
        "ebur128=peak=true:framelog=quiet",
        "-f",
        "null",
        "-",
    ]);
    let field = |key: &str| -> f64 {
        log.lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("{key} missing in {log}"))
    };
    (field("I:"), field("LRA:"), field("Peak:"))
}

#[test]
fn r128_matches_ffmpeg_ebur128() {
    if !ffmpeg_present() {
        return;
    }
    let cases: [(&str, &str, u32); 4] = [
        (
            "tone",
            "aevalsrc=0.25*sin(2*PI*997*t)|0.25*sin(2*PI*997*t):s=48000:d=12",
            48000,
        ),
        (
            "steps",
            "aevalsrc=if(lt(t\\,8)\\,0.05\\,if(lt(t\\,16)\\,0.4\\,0.1))*sin(2*PI*440*t)|0.2*sin(2*PI*3000*t):s=48000:d=24",
            48000,
        ),
        (
            "pink",
            "anoisesrc=color=pink:amplitude=0.5:seed=7:r=48000:d=15",
            48000,
        ),
        (
            "white",
            "anoisesrc=color=white:amplitude=0.3:seed=3:r=44100:d=10",
            44100,
        ),
    ];
    for (label, src, rate) in cases {
        let name = &make(
            label,
            &["-f", "lavfi", "-i", src, "-ac", "2", "-f", "f32le"],
        );
        let buf = read_f32(name, rate, 2);
        let mut meter = LoudnessMeter::new(rate, 2).unwrap();
        for chunk in buf.samples.chunks(2 * 1999) {
            let part = AudioBuffer {
                samples: chunk.to_vec(),
                rate,
                channels: 2,
            };
            meter.process(&part).unwrap();
        }
        let ours = meter.summary();
        let (i, lra, tp) = ebur128(name, rate, 2);
        println!(
            "{name}: I {:.2}/{i} LRA {:.2}/{lra} TP {:.2}/{tp}",
            ours.integrated, ours.range, ours.true_peak
        );
        assert!(
            (ours.integrated - i).abs() <= 0.1,
            "{name}: I {} vs {i}",
            ours.integrated
        );
        assert!(
            (ours.true_peak - tp).abs() <= 0.2,
            "{name}: TP {} vs {tp}",
            ours.true_peak
        );
        assert!(
            (ours.range - lra).abs() <= 0.5,
            "{name}: LRA {} vs {lra}",
            ours.range
        );
    }
}

#[test]
fn scale_matches_zscale() {
    if !ffmpeg_present() {
        return;
    }
    if !zscale_present() {
        return;
    }
    let src = &source_1440p();
    let frame = read_yuv(src, PixelFormat::Yuv420p8, 2560, 1440);
    let deep_name = format!("{src}.10bit.yuv");
    let cases = [
        (
            Kernel::Bilinear,
            "bilinear",
            1920,
            1080,
            PixelFormat::Yuv420p8,
        ),
        (
            Kernel::Bicubic,
            "bicubic",
            1920,
            1080,
            PixelFormat::Yuv420p8,
        ),
        (
            Kernel::Lanczos,
            "lanczos",
            1920,
            1080,
            PixelFormat::Yuv420p8,
        ),
        (Kernel::Lanczos, "lanczos", 854, 480, PixelFormat::Yuv420p8),
        (
            Kernel::Bicubic,
            "bicubic",
            3840,
            2160,
            PixelFormat::Yuv420p8,
        ),
        (
            Kernel::Lanczos,
            "lanczos",
            1920,
            1080,
            PixelFormat::Yuv420p10,
        ),
    ];
    for (kernel, name, w, h, format) in cases {
        let (input, pix) = if format == PixelFormat::Yuv420p10 {
            let mut deep = Frame::new(format, 2560, 1440, 0, frame.time_base);
            convert_color(
                &frame,
                ColorSpec::BT709,
                ColorSpec::BT709,
                format,
                &mut deep,
            )
            .unwrap();
            write_yuv(&deep_name, &deep);
            (deep, "yuv420p10le")
        } else {
            (frame.clone(), "yuv420p")
        };
        let file = if format == PixelFormat::Yuv420p10 {
            deep_name.as_str()
        } else {
            src
        };
        let out = make(
            &format!("zscale-{name}-{w}x{h}-{pix}"),
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix,
                "-s",
                "2560x1440",
                "-i",
                &path(file),
                "-vf",
                &format!("zscale=w={w}:h={h}:filter={name}:chromal=left:chromalin=left"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                pix,
            ],
        );
        let theirs = read_yuv(&out, format, w, h);
        let ours = scale(&input, w, h, kernel).unwrap();
        let p = frame_psnr(&ours, &theirs);
        println!(
            "{name} {w}x{h} {pix}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(p.iter().all(|&v| v > 60.0), "{name} {w}x{h} {pix}: {p:?}");
    }
}

#[test]
fn colour_matches_ffmpeg() {
    if !ffmpeg_present() {
        return;
    }
    let src = &source_1440p();
    let frame = read_yuv(src, PixelFormat::Yuv420p8, 2560, 1440);
    for (spec, matrix, range) in [
        (ColorSpec::BT709, "bt709", "tv"),
        (ColorSpec::new(Matrix::Bt601, Range::Full), "bt601", "pc"),
        (ColorSpec::BT2020, "bt2020", "tv"),
    ] {
        let out = make(
            &format!("rgba-{matrix}-{range}"),
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                "-s",
                "2560x1440",
                "-i",
                &path(src),
                "-vf",
                &format!(
                    "scale=in_color_matrix={matrix}:in_range={range}:flags=neighbor+accurate_rnd+full_chroma_int"
                ),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
            ],
        );
        let theirs = std::fs::read(path(&out)).unwrap();
        let mut rgba = Rgba::new(0, 0);
        yuv_to_rgba(&frame, spec, &mut rgba).unwrap();
        let p = psnr(&rgba.data, &theirs, false);
        println!("yuv to rgba {matrix} {range}: PSNR {p:.1}");
        assert!(p > 70.0, "yuv to rgba {matrix} {range}: {p}");

        let back = make(
            &format!("yuv-{matrix}-{range}"),
            &[
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "-s",
                "2560x1440",
                "-i",
                &path(&out),
                "-vf",
                &format!(
                    "scale=out_color_matrix={matrix}:out_range={range}:flags=area+accurate_rnd"
                ),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
            ],
        );
        let theirs = read_yuv(&back, PixelFormat::Yuv420p8, 2560, 1440);
        let image = Rgba {
            width: 2560,
            height: 1440,
            data: std::fs::read(path(&out)).unwrap(),
        };
        let mut ours = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
        rgba_to_yuv(
            &image,
            spec.with_siting(Siting::Centre),
            PixelFormat::Yuv420p8,
            &mut ours,
        )
        .unwrap();
        let p = frame_psnr(&ours, &theirs);
        println!(
            "rgba to yuv {matrix} {range}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(
            p.iter().all(|&v| v > 48.0),
            "rgba to yuv {matrix} {range}: {p:?}"
        );
    }
    let out = &make(
        "bt601-tv-to-bt709-pc-10bit",
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            "2560x1440",
            "-i",
            &path(src),
            "-vf",
            "colorspace=ispace=bt470bg:irange=tv:iprimaries=bt470bg:itrc=bt709:space=bt709:range=pc:primaries=bt470bg:trc=bt709:format=yuv420p10",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p10le",
        ],
    );
    let theirs = read_yuv(out, PixelFormat::Yuv420p10, 2560, 1440);
    let mut ours = Frame::new(PixelFormat::Yuv420p10, 0, 0, 0, frame.time_base);
    convert_color(
        &frame,
        ColorSpec::BT601,
        ColorSpec::new(Matrix::Bt709, Range::Full),
        PixelFormat::Yuv420p10,
        &mut ours,
    )
    .unwrap();
    let p = frame_psnr(&ours, &theirs);
    println!(
        "bt601 limited 8-bit to bt709 full 10-bit: PSNR {:.1} {:.1} {:.1}",
        p[0], p[1], p[2]
    );
    assert!(p.iter().all(|&v| v > 75.0), "convert: {p:?}");
}

fn pattern(format: PixelFormat, width: u32, height: u32, seed: u32) -> Frame {
    let mut f = Frame::new(format, width, height, 0, TimeBase::new(1, 30));
    let mut s = seed | 1;
    let top = (1u32 << format.bit_depth()) - 1;
    for p in f.planes.iter_mut() {
        if format == PixelFormat::Yuv420p8 {
            for v in p.data.iter_mut() {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                *v = s as u8;
            }
        } else {
            for c in p.data.chunks_exact_mut(2) {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                c.copy_from_slice(&((s % (top + 1)) as u16).to_le_bytes());
            }
        }
    }
    f
}

#[test]
fn simd_kernels_match_plain() {
    use super::packed::{from_packed, to_packed};
    use super::plane::{Simd, simd};
    let fast = simd();
    let (w, h) = (77u32, 13u32);
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        let frame = pattern(format, w, h, 7);
        for siting in [Siting::Left, Siting::Centre] {
            let spec = ColorSpec::BT709.with_siting(siting);
            for order in [Order::Rgba, Order::Bgra, Order::Rgbx, Order::Bgrx] {
                for chroma in [Chroma::Nearest, Chroma::Bilinear] {
                    let mut a = vec![0u8; (w * h * 4) as usize];
                    let mut b = a.clone();
                    to_packed(
                        &frame,
                        spec,
                        chroma,
                        PackedMut::new(w, h, order, &mut a),
                        Simd::Plain,
                    )
                    .unwrap();
                    to_packed(
                        &frame,
                        spec,
                        chroma,
                        PackedMut::new(w, h, order, &mut b),
                        fast,
                    )
                    .unwrap();
                    assert_eq!(a, b, "{format:?} {siting:?} {order:?} {chroma:?} to packed");
                }
                let [luma, _, _] = pattern(PixelFormat::Yuv420p8, w * 2, h * 2, 9).planes;
                let bytes = luma.data;
                let image = Packed::new(w, h, order, &bytes[..(w * h * 4) as usize]);
                let mut a = Frame::new(format, 0, 0, 0, frame.time_base);
                let mut b = a.clone();
                from_packed(image, spec, format, &mut a, Simd::Plain).unwrap();
                from_packed(image, spec, format, &mut b, fast).unwrap();
                assert_eq!(a, b, "{format:?} {siting:?} {order:?} from packed");
            }
        }
        for (dw, dh) in [(40, 29), (160, 50)] {
            for kernel in [Kernel::Bilinear, Kernel::Lanczos] {
                let s = Scaler::new(w, h, dw, dh, format, kernel).unwrap();
                let mut a = Frame::new(format, 0, 0, 0, frame.time_base);
                let mut b = a.clone();
                s.clone()
                    .with_simd(Simd::Plain)
                    .run(&frame, &mut a)
                    .unwrap();
                s.run(&frame, &mut b).unwrap();
                assert_eq!(a, b, "{format:?} scale {kernel:?} to {dw}x{dh}");
            }
        }
    }
}

fn zscale_rgba(src: &str, chroma: &str) -> String {
    make(
        &format!("zscale-rgba-{chroma}"),
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            "2560x1440",
            "-i",
            &path(src),
            "-vf",
            &format!(
                "zscale=min=709:rin=limited:tin=709:pin=709:t=709:p=709:m=gbr:r=full:chromalin={chroma}:filter=bilinear,format=gbrp"
            ),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
        ],
    )
}

#[test]
fn orders_and_siting_match_ffmpeg() {
    if !ffmpeg_present() || !zscale_present() {
        return;
    }
    let src = &source_1440p();
    let frame = read_yuv(src, PixelFormat::Yuv420p8, 2560, 1440);
    let (w, h) = (2560u32, 1440u32);
    let size = (w * h * 4) as usize;
    let mut ours = vec![0u8; size];

    let bgra = make(
        "bgra-bt709-tv",
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            "2560x1440",
            "-i",
            &path(src),
            "-vf",
            "scale=in_color_matrix=bt709:in_range=tv:flags=neighbor+accurate_rnd+full_chroma_int",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
        ],
    );
    let theirs = std::fs::read(path(&bgra)).unwrap();
    yuv_to_packed(
        &frame,
        ColorSpec::BT709,
        Chroma::Nearest,
        PackedMut::new(w, h, Order::Bgra, &mut ours),
    )
    .unwrap();
    let p = psnr(&ours, &theirs, false);
    println!("yuv to bgra: PSNR {p:.1}");
    assert!(p > 70.0, "yuv to bgra: {p}");

    let back = make(
        "bgr0-to-yuv-bt709-tv-area",
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgr0",
            "-s",
            "2560x1440",
            "-i",
            &path(&bgra),
            "-vf",
            "scale=out_color_matrix=bt709:out_range=tv:flags=area+accurate_rnd",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let expect = read_yuv(&back, PixelFormat::Yuv420p8, w, h);
    let mut got = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
    let spec = ColorSpec::BT709.with_siting(Siting::Centre);
    packed_to_yuv(
        Packed::new(w, h, Order::Bgrx, &theirs),
        spec,
        PixelFormat::Yuv420p8,
        &mut got,
    )
    .unwrap();
    let p = frame_psnr(&got, &expect);
    println!(
        "bgrx to yuv, centre: PSNR {:.1} {:.1} {:.1}",
        p[0], p[1], p[2]
    );
    assert!(p.iter().all(|&v| v > 75.0), "bgrx to yuv: {p:?}");

    for (siting, name) in [(Siting::Left, "left"), (Siting::Centre, "center")] {
        let theirs = std::fs::read(path(&zscale_rgba(src, name))).unwrap();
        let spec = ColorSpec::BT709.with_siting(siting);
        yuv_to_packed(
            &frame,
            spec,
            Chroma::Bilinear,
            PackedMut::new(w, h, Order::Rgba, &mut ours),
        )
        .unwrap();
        let p = psnr(&ours, &theirs, false);
        println!("yuv to rgba, bilinear chroma, {name}: PSNR {p:.1}");
        assert!(p > 80.0, "bilinear chroma {name}: {p}");
    }

    let rgba = zscale_rgba(src, "left");
    let left = make(
        "rgba-to-yuv-bt709-tv-left",
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            "2560x1440",
            "-i",
            &path(&rgba),
            "-vf",
            "format=gbrp,zscale=rin=full:tin=709:pin=709:m=709:r=limited:t=709:p=709:chromal=left:filter=bilinear,format=yuv420p",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let expect = read_yuv(&left, PixelFormat::Yuv420p8, w, h);
    let bytes = std::fs::read(path(&rgba)).unwrap();
    packed_to_yuv(
        Packed::new(w, h, Order::Rgba, &bytes),
        ColorSpec::BT709,
        PixelFormat::Yuv420p8,
        &mut got,
    )
    .unwrap();
    let p = frame_psnr(&got, &expect);
    println!(
        "rgba to yuv, left: PSNR {:.1} {:.1} {:.1}",
        p[0], p[1], p[2]
    );
    assert!(p.iter().all(|&v| v > 53.0), "rgba to yuv left: {p:?}");

    let centre = make(
        "zscale-lanczos-1920x1080-center",
        &[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            "2560x1440",
            "-i",
            &path(src),
            "-vf",
            "zscale=w=1920:h=1080:filter=lanczos:chromal=center:chromalin=center",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
        ],
    );
    let expect = read_yuv(&centre, PixelFormat::Yuv420p8, 1920, 1080);
    let scaler = Scaler::new(w, h, 1920, 1080, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .siting(Siting::Centre);
    let mut scaled = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
    scaler.run(&frame, &mut scaled).unwrap();
    let p = frame_psnr(&scaled, &expect);
    println!(
        "scale lanczos, centre siting: PSNR {:.1} {:.1} {:.1}",
        p[0], p[1], p[2]
    );
    assert!(p.iter().all(|&v| v > 60.0), "scale centre: {p:?}");
}

fn tone(freq: f64, rate: u32, seconds: f64, amp: f64) -> Vec<f32> {
    (0..(seconds * rate as f64) as usize)
        .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32)
        .collect()
}

fn mono(samples: Vec<f32>, rate: u32) -> AudioBuffer {
    AudioBuffer {
        samples,
        rate,
        channels: 1,
    }
}

fn rms_db(x: &[f32]) -> f64 {
    let e = x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * e.log10()
}

#[test]
fn resample_round_trip_keeps_the_band_and_drops_aliases() {
    let rate = 48000;
    let mut x = vec![0.0f32; rate as usize];
    for f in [100.0, 1000.0, 5000.0, 12000.0, 16000.0] {
        for (o, v) in x.iter_mut().zip(tone(f, rate, 1.0, 0.15)) {
            *o += v;
        }
    }
    let input = mono(x.clone(), rate);
    let down = resample(&input, 44100).unwrap();
    assert_eq!(down.samples.len(), 44100);
    let back = resample(&down, rate).unwrap();
    assert_eq!(back.samples.len(), x.len());
    let edge = 2000;
    let err: Vec<f32> = x[edge..x.len() - edge]
        .iter()
        .zip(&back.samples[edge..x.len() - edge])
        .map(|(a, b)| a - b)
        .collect();
    let snr = rms_db(&x[edge..x.len() - edge]) - rms_db(&err);
    assert!(snr > 60.0, "round trip SNR {snr:.1} dB");

    let alias = resample(&mono(tone(23000.0, rate, 1.0, 0.5), rate), 44100).unwrap();
    let level = rms_db(&alias.samples[edge..alias.samples.len() - edge])
        - rms_db(&tone(23000.0, rate, 1.0, 0.5));
    assert!(level < -70.0, "23 kHz into 44.1 kHz leaks {level:.1} dB");

    let speech = resample(&mono(tone(10000.0, rate, 1.0, 0.5), rate), 16000).unwrap();
    assert_eq!(speech.samples.len(), 16000);
    let level = rms_db(&speech.samples[500..15500]) - rms_db(&tone(10000.0, rate, 1.0, 0.5));
    assert!(level < -70.0, "10 kHz into 16 kHz leaks {level:.1} dB");

    let odd = resample(&mono(tone(1000.0, rate, 1.0, 0.5), rate), 47999).unwrap();
    let want = tone(1000.0, 47999, 1.0, 0.5);
    let err: Vec<f32> = odd.samples[edge..40000]
        .iter()
        .zip(&want[edge..40000])
        .map(|(a, b)| a - b)
        .collect();
    let snr = rms_db(&want[edge..40000]) - rms_db(&err);
    assert!(snr > 60.0, "48000 to 47999 SNR {snr:.1} dB");

    for (from, to, ch) in [(44100, 48000, 2u16), (48000, 16000, 1), (48000, 47999, 2)] {
        let input = AudioBuffer {
            samples: (0..from as usize * ch as usize / 3)
                .map(|i| ((i * 7919) % 2003) as f32 / 1001.0 - 1.0)
                .collect(),
            rate: from,
            channels: ch,
        };
        let mut rs = Resampler::new(from, to, ch).unwrap();
        let mut streamed = AudioBuffer::new(to, ch);
        let mut part = AudioBuffer::new(to, ch);
        for c in input.samples.chunks(999 * ch as usize) {
            let piece = AudioBuffer {
                samples: c.to_vec(),
                rate: from,
                channels: ch,
            };
            rs.process(&piece, &mut part).unwrap();
            streamed.samples.extend_from_slice(&part.samples);
        }
        rs.flush(&mut part);
        streamed.samples.extend_from_slice(&part.samples);
        assert_eq!(
            resample(&input, to).unwrap().samples,
            streamed.samples,
            "{from} to {to}"
        );
    }
}

#[test]
fn normalise_hits_target_under_the_ceiling() {
    let rate = 48000;
    let mut x = tone(220.0, rate, 20.0, 0.1);
    let burst = tone(1000.0, rate, 0.01, 0.8);
    for at in (0..x.len() - burst.len()).step_by(2 * rate as usize) {
        for (o, b) in x[at..].iter_mut().zip(&burst) {
            *o += b;
        }
    }
    let stereo: Vec<f32> = x.iter().flat_map(|&v| [v, v]).collect();
    let mut buf = AudioBuffer {
        samples: stereo,
        rate,
        channels: 2,
    };
    let frames = buf.frames();
    let before = normalize(&mut buf, -14.0, -1.0).unwrap();
    assert!(before.true_peak - before.integrated + -14.0 > 0.0);
    assert_eq!(buf.frames(), frames);
    let mut meter = LoudnessMeter::new(rate, 2).unwrap();
    meter.process(&buf).unwrap();
    let after = meter.summary();
    assert!(after.true_peak <= -0.95, "true peak {}", after.true_peak);
    assert!(
        (after.integrated + 14.0).abs() < 0.2,
        "integrated {}",
        after.integrated
    );
}

#[test]
fn fps_change_drops_and_repeats_by_pts() {
    let tiny = |pts: i64, tb: TimeBase| Frame::new(PixelFormat::Yuv420p8, 2, 2, pts, tb);
    for (from, to, count, want) in [
        ((30, 1), (24, 1), 90, 72),
        ((24, 1), (60, 1), 48, 120),
        ((30000, 1001), (30, 1), 300, 300),
    ] {
        let mut conv = FpsConverter::new(to.0, to.1).unwrap();
        let tb = TimeBase::new(from.1 * 1000, from.0 * 1000);
        let mut out = Vec::new();
        for i in 0..count {
            out.extend(conv.push(tiny(i, tb)));
        }
        out.extend(conv.flush());
        let pts: Vec<i64> = out.iter().map(|f| f.pts).collect();
        assert!(
            (pts.len() as i64 - want).abs() <= 1,
            "{from:?} to {to:?}: {} frames",
            pts.len()
        );
        assert!(
            pts.windows(2).all(|w| w[1] == w[0] + 1),
            "{from:?} to {to:?}: gaps"
        );
        assert_eq!(out[0].time_base, conv.time_base());
    }
}

#[test]
fn rotation_matches_a_plain_transpose_at_odd_sizes() {
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        let (w, h) = (150usize, 86usize);
        let mut f = Frame::new(format, w as u32, h as u32, 0, TimeBase::new(1, 30));
        for p in f.planes.iter_mut() {
            for (i, v) in p.data.iter_mut().enumerate() {
                *v = (i * 31 + i / 7) as u8;
            }
        }
        let b = format.bytes_per_sample();
        for rotation in [Rotation::R90, Rotation::R270] {
            let mut r = Frame::new(format, 0, 0, 0, f.time_base);
            rotate(&f, rotation, &mut r).unwrap();
            for i in 0..3 {
                let (pw, ph) = if i == 0 { (w, h) } else { (w / 2, h / 2) };
                let (s, d) = (&f.planes[i], &r.planes[i]);
                for y in 0..pw {
                    for x in 0..ph {
                        let (sy, sx) = match rotation {
                            Rotation::R90 => (ph - 1 - x, y),
                            _ => (x, pw - 1 - y),
                        };
                        let at = |p: &crate::media::Plane, row: usize, col: usize| {
                            p.data[row * p.stride + col * b..row * p.stride + col * b + b].to_vec()
                        };
                        assert_eq!(
                            at(d, y, x),
                            at(s, sy, sx),
                            "{format:?} {rotation:?} plane {i} at {x},{y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn geometry_round_trips() {
    let mut f = Frame::new(PixelFormat::Yuv420p8, 6, 4, 7, TimeBase::new(1, 30));
    for p in f.planes.iter_mut() {
        for (i, v) in p.data.iter_mut().enumerate() {
            *v = (i * 7 + 3) as u8;
        }
    }
    let mut r = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, f.time_base);
    let mut back = r.clone();
    rotate(&f, Rotation::R90, &mut r).unwrap();
    assert_eq!((r.width, r.height), (4, 6));
    assert_eq!(r.planes[0].data[3], f.planes[0].data[0]);
    assert_eq!(r.planes[0].data[0], f.planes[0].data[18]);
    rotate(&r, Rotation::R270, &mut back).unwrap();
    assert_eq!(back, f);
    rotate(&f, Rotation::R180, &mut r).unwrap();
    rotate(&r, Rotation::R180, &mut back).unwrap();
    assert_eq!(back, f);
    let mut flipped = f.clone();
    flip_horizontal(&mut flipped).unwrap();
    flip_vertical(&mut flipped).unwrap();
    assert_eq!(flipped, r);

    let black = Fill::black(Range::Limited, PixelFormat::Yuv420p8);
    pad(&f, 10, 8, 2, 2, black, &mut r).unwrap();
    assert_eq!(r.planes[0].data[0], 16);
    assert_eq!(r.planes[1].data[0], 128);
    crop(&r, 2, 2, 6, 4, &mut back).unwrap();
    assert_eq!(back, f);

    let mut canvas = Frame::new(PixelFormat::Yuv420p8, 8, 8, 0, f.time_base);
    pad(&f, 8, 8, 0, 0, black, &mut canvas).unwrap();
    let mut mark = Rgba::new(4, 4);
    for p in mark.data.chunks_exact_mut(4) {
        p.copy_from_slice(&[255, 255, 255, 255]);
    }
    mark.data[3] = 0;
    let overlay = Overlay::new(&mark, ColorSpec::BT709, PixelFormat::Yuv420p8).unwrap();
    let before = canvas.clone();
    overlay.draw(&mut canvas, 4, 4).unwrap();
    let y = &canvas.planes[0].data;
    assert_eq!(y[4 * 8 + 4], before.planes[0].data[4 * 8 + 4]);
    assert_eq!(y[4 * 8 + 5], 235);
    assert_eq!(y[7 * 8 + 7], 235);
    assert_eq!(&y[..32], &before.planes[0].data[..32]);
    overlay.draw(&mut canvas, -10, -10).unwrap();
    overlay.draw(&mut canvas, 7, 7).unwrap();
}

fn rate(label: &str, n: usize, f: impl FnMut()) -> f64 {
    let mut f = f;
    f();
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    let per = t.elapsed().as_secs_f64() / n as f64;
    println!("{label}: {:.1} fps ({:.2} ms)", 1.0 / per, per * 1000.0);
    1.0 / per
}

#[test]
#[ignore]
fn bench() {
    let mut src = Frame::new(PixelFormat::Yuv420p8, 2560, 1440, 0, TimeBase::new(1, 30));
    if ffmpeg_present() {
        src = read_yuv(&source_1440p(), PixelFormat::Yuv420p8, 2560, 1440);
    }
    let threads = rayon::current_num_threads();
    for kernel in [Kernel::Bilinear, Kernel::Bicubic, Kernel::Lanczos] {
        let s = Scaler::new(2560, 1440, 1920, 1080, PixelFormat::Yuv420p8, kernel).unwrap();
        let mut out = Frame::new(PixelFormat::Yuv420p8, 1920, 1080, 0, src.time_base);
        rate(
            &format!("scale 1440p to 1080p 8-bit {kernel:?}, {threads} threads"),
            200,
            || s.run(&src, &mut out).unwrap(),
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            rate(
                &format!("scale 1440p to 1080p 8-bit {kernel:?}, 1 thread"),
                30,
                || s.run(&src, &mut out).unwrap(),
            );
        });
    }
    let mut deep = Frame::new(PixelFormat::Yuv420p10, 0, 0, 0, src.time_base);
    convert_color(
        &src,
        ColorSpec::BT709,
        ColorSpec::BT709,
        PixelFormat::Yuv420p10,
        &mut deep,
    )
    .unwrap();
    let s = Scaler::new(
        2560,
        1440,
        1920,
        1080,
        PixelFormat::Yuv420p10,
        Kernel::Lanczos,
    )
    .unwrap();
    let mut out = Frame::new(PixelFormat::Yuv420p10, 1920, 1080, 0, src.time_base);
    rate("scale 1440p to 1080p 10-bit Lanczos", 200, || {
        s.run(&deep, &mut out).unwrap()
    });
    let s = Scaler::new(
        1920,
        1080,
        3840,
        2160,
        PixelFormat::Yuv420p8,
        Kernel::Bicubic,
    )
    .unwrap();
    let small = scale(&src, 1920, 1080, Kernel::Bicubic).unwrap();
    let mut big = Frame::new(PixelFormat::Yuv420p8, 3840, 2160, 0, src.time_base);
    rate("scale 1080p to 2160p 8-bit Bicubic", 100, || {
        s.run(&small, &mut big).unwrap()
    });

    let mut bgra = vec![0u8; 2560 * 1440 * 4];
    rate("yuv to bgra 1440p 8-bit", 300, || {
        yuv_to_packed(
            &src,
            ColorSpec::BT709,
            Chroma::Nearest,
            PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
        )
        .unwrap()
    });
    rate("yuv to bgra 1440p 8-bit, bilinear chroma", 300, || {
        yuv_to_packed(
            &src,
            ColorSpec::BT709,
            Chroma::Bilinear,
            PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
        )
        .unwrap()
    });
    let mut out_yuv = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
    rate("bgrx to yuv 1440p 8-bit", 300, || {
        packed_to_yuv(
            Packed::new(2560, 1440, Order::Bgrx, &bgra),
            ColorSpec::BT709,
            PixelFormat::Yuv420p8,
            &mut out_yuv,
        )
        .unwrap()
    });
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| {
        rate("yuv to bgra 1440p 8-bit, 1 thread", 60, || {
            yuv_to_packed(
                &src,
                ColorSpec::BT709,
                Chroma::Nearest,
                PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
            )
            .unwrap()
        });
        rate("bgrx to yuv 1440p 8-bit, 1 thread", 60, || {
            packed_to_yuv(
                Packed::new(2560, 1440, Order::Bgrx, &bgra),
                ColorSpec::BT709,
                PixelFormat::Yuv420p8,
                &mut out_yuv,
            )
            .unwrap()
        });
    });
    let mut rgba = Rgba::new(0, 0);
    rate("yuv to rgba 1440p 8-bit", 300, || {
        yuv_to_rgba(&src, ColorSpec::BT709, &mut rgba).unwrap()
    });
    let mut yuv = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
    rate("rgba to yuv 1440p 8-bit", 300, || {
        rgba_to_yuv(&rgba, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut yuv).unwrap()
    });
    rate("bt601 to bt709 1440p 8-bit", 300, || {
        convert_color(
            &src,
            ColorSpec::BT601,
            ColorSpec::BT709,
            PixelFormat::Yuv420p8,
            &mut yuv,
        )
        .unwrap()
    });
    rate("rotate 90 1440p 8-bit", 300, || {
        rotate(&src, Rotation::R90, &mut yuv).unwrap()
    });
    let mut caption = Rgba::new(1600, 160);
    for (i, p) in caption.data.chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(&[255, 255, 255, if (i / 7) % 3 == 0 { 255 } else { 0 }]);
    }
    let overlay = Overlay::new(&caption, ColorSpec::BT709, PixelFormat::Yuv420p8).unwrap();
    let mut target = src.clone();
    rate("overlay 1600x160 caption on 1440p", 300, || {
        overlay.draw(&mut target, 480, 1200).unwrap()
    });

    let seconds = 60.0;
    let r = 48000;
    let noise: Vec<f32> = {
        let mut s = 0x1234_5678u32;
        (0..(seconds * r as f64) as usize * 2)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    };
    let stereo = AudioBuffer {
        samples: noise.clone(),
        rate: r,
        channels: 2,
    };
    let blocks: Vec<AudioBuffer> = stereo
        .samples
        .chunks(2 * 1024)
        .map(|c| AudioBuffer {
            samples: c.to_vec(),
            rate: r,
            channels: 2,
        })
        .collect();
    let t = Instant::now();
    let mut meter = LoudnessMeter::new(r, 2).unwrap();
    for b in &blocks {
        meter.process(b).unwrap();
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "r128 meter 48 kHz stereo, 1024-frame blocks: {:.0}x real time",
        seconds / s
    );
    let t = Instant::now();
    let mut meter = LoudnessMeter::new(r, 2).unwrap();
    meter.process(&stereo).unwrap();
    let s = t.elapsed().as_secs_f64();
    println!(
        "r128 meter 48 kHz stereo, one block: {:.0}x real time",
        seconds / s
    );

    for (from, to, ch) in [(44100, 48000, 2u16), (48000, 44100, 2), (48000, 16000, 1)] {
        let input = AudioBuffer {
            samples: noise[..(seconds * from as f64) as usize * ch as usize].to_vec(),
            rate: from,
            channels: ch,
        };
        let mut rs = Resampler::new(from, to, ch).unwrap();
        let mut out = AudioBuffer::new(to, ch);
        let t = Instant::now();
        for c in input.samples.chunks(1024 * ch as usize) {
            let part = AudioBuffer {
                samples: c.to_vec(),
                rate: from,
                channels: ch,
            };
            rs.process(&part, &mut out).unwrap();
        }
        let s = t.elapsed().as_secs_f64();
        println!(
            "resample {from} to {to} Hz, {ch} channels: {:.0}x real time",
            seconds / s
        );
    }
    let mut lim = Limiter::new(r, 2, -1.0).unwrap();
    let mut out = AudioBuffer::new(r, 2);
    let t = Instant::now();
    for b in &blocks {
        lim.process(b, &mut out).unwrap();
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "true-peak limiter 48 kHz stereo: {:.0}x real time",
        seconds / s
    );
}

fn wall(args: &[String]) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-nostats", "-v", "error", "-y"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ffmpeg {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        best = best.min(t.elapsed().as_secs_f64());
    }
    best
}

fn ffmpeg_rate(input: &[&str], filter: (&str, &str), threads: usize, units: f64) -> f64 {
    let run = |f: &str| -> f64 {
        let mut a: Vec<String> = vec![
            "-threads".into(),
            threads.to_string(),
            "-filter_threads".into(),
            threads.to_string(),
        ];
        a.extend(input.iter().map(|s| s.to_string()));
        a.extend([filter.0.to_string(), f.replace("{t}", &threads.to_string())]);
        a.extend(["-f".to_string(), "null".into(), "-".into()]);
        wall(&a)
    };
    let pass = if filter.0 == "-af" { "anull" } else { "null" };
    let t = (run(filter.1) - run(pass)).max(1e-6);
    units / t
}

fn ours_rate(units: f64, n: usize, mut f: impl FnMut()) -> f64 {
    f();
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        for _ in 0..n {
            f();
        }
        best = best.min(t.elapsed().as_secs_f64() / n as f64);
    }
    units / best
}

#[test]
#[ignore]
fn compare() {
    if !ffmpeg_present() {
        return;
    }
    let threads = rayon::current_num_threads();
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let src_name = source_1440p();
    let src_path = path(&src_name);
    let src = read_yuv(&src_name, PixelFormat::Yuv420p8, 2560, 1440);
    let frames = 480usize;
    let frames_s = frames.to_string();
    let yuv_in = [
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        "2560x1440",
        "-stream_loop",
        "-1",
        "-i",
        &src_path,
        "-frames:v",
        &frames_s,
    ];
    let mut bgra = vec![0u8; 2560 * 1440 * 4];
    yuv_to_packed(
        &src,
        ColorSpec::BT709,
        Chroma::Nearest,
        PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
    )
    .unwrap();
    let bgra_path = path("compare-bgra.raw");
    std::fs::write(&bgra_path, &bgra).unwrap();
    let bgr0_in = [
        "-f",
        "rawvideo",
        "-pix_fmt",
        "bgr0",
        "-s",
        "2560x1440",
        "-stream_loop",
        "-1",
        "-i",
        &bgra_path,
        "-frames:v",
        &frames_s,
    ];
    let mut rows: Vec<(String, [f64; 4], &str)> = Vec::new();
    let mut video = |label: &str, input: &[&str], vf: &str, f: &mut (dyn FnMut() + Send)| {
        let all = ours_rate(1.0, 40, &mut *f);
        let single = one.install(|| ours_rate(1.0, 10, &mut *f));
        let ff_all = ffmpeg_rate(input, ("-vf", vf), threads, frames as f64);
        let ff_one = ffmpeg_rate(input, ("-vf", vf), 1, frames as f64);
        println!("{label}: ours {all:.0}/{single:.0} fps, ffmpeg {ff_all:.0}/{ff_one:.0} fps");
        rows.push((label.to_string(), [all, single, ff_all, ff_one], "fps"));
    };
    for (kernel, flag) in [
        (Kernel::Bilinear, "bilinear"),
        (Kernel::Bicubic, "bicubic"),
        (Kernel::Lanczos, "lanczos"),
    ] {
        let s = Scaler::new(2560, 1440, 1920, 1080, PixelFormat::Yuv420p8, kernel).unwrap();
        let mut out = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
        video(
            &format!("Scale 1440p to 1080p, {flag}"),
            &yuv_in,
            &format!("scale=1920:1080:flags={flag}:threads={{t}}"),
            &mut || s.run(&src, &mut out).unwrap(),
        );
    }
    let mut packed = vec![0u8; 2560 * 1440 * 4];
    for (order, name) in [(Order::Rgba, "rgba"), (Order::Bgra, "bgra")] {
        video(
            &format!("YUV 4:2:0 to {} 1440p", name.to_uppercase()),
            &yuv_in,
            &format!("scale=threads={{t}},format={name}"),
            &mut || {
                yuv_to_packed(
                    &src,
                    ColorSpec::BT709,
                    Chroma::Nearest,
                    PackedMut::new(2560, 1440, order, &mut packed),
                )
                .unwrap()
            },
        );
    }
    let mut yuv = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
    video(
        "BGRX to YUV 4:2:0 1440p",
        &bgr0_in,
        "scale=threads={t},format=yuv420p",
        &mut || {
            packed_to_yuv(
                Packed::new(2560, 1440, Order::Bgrx, &bgra),
                ColorSpec::BT709,
                PixelFormat::Yuv420p8,
                &mut yuv,
            )
            .unwrap()
        },
    );
    video(
        "BT.601 to BT.709 1440p",
        &yuv_in,
        "colorspace=ispace=bt470bg:irange=tv:iprimaries=bt470bg:itrc=bt709:space=bt709:range=tv:primaries=bt470bg:trc=bt709:format=yuv420p",
        &mut || {
            convert_color(
                &src,
                ColorSpec::BT601,
                ColorSpec::BT709,
                PixelFormat::Yuv420p8,
                &mut yuv,
            )
            .unwrap()
        },
    );
    video("Rotate 90 1440p", &yuv_in, "transpose=1", &mut || {
        rotate(&src, Rotation::R90, &mut yuv).unwrap()
    });

    let seconds = 60.0;
    let noise = |rate: u32, channels: u16| -> (AudioBuffer, String) {
        let name = make(
            &format!("compare-noise-{rate}-{channels}"),
            &[
                "-f",
                "lavfi",
                "-i",
                &format!("anoisesrc=color=pink:amplitude=0.5:seed=5:r={rate}:d={seconds}"),
                "-ac",
                &channels.to_string(),
                "-f",
                "f32le",
            ],
        );
        (read_f32(&name, rate, channels), path(&name))
    };
    let mut audio = |label: &str, input: (&AudioBuffer, &str), af: &str, f: &mut dyn FnMut()| {
        let (buf, file) = input;
        let rate = buf.rate.to_string();
        let ch = buf.channels.to_string();
        let args = [
            "-stream_loop",
            "9",
            "-f",
            "f32le",
            "-ar",
            &rate,
            "-ac",
            &ch,
            "-i",
            file,
        ];
        let ours = ours_rate(seconds, 3, &mut *f);
        let ff = ffmpeg_rate(&args, ("-af", af), 1, seconds * 10.0);
        println!("{label}: ours {ours:.0}x, ffmpeg {ff:.0}x real time");
        rows.push((
            label.to_string(),
            [ours, f64::NAN, ff, f64::NAN],
            "x real time",
        ));
    };
    let (stereo48, f48) = noise(48000, 2);
    audio(
        "EBU R128 meter, 48 kHz stereo",
        (&stereo48, &f48),
        "ebur128=peak=true:framelog=quiet",
        &mut || {
            let mut m = LoudnessMeter::new(48000, 2).unwrap();
            for c in stereo48.samples.chunks(2 * 1024) {
                m.process(&AudioBuffer {
                    samples: c.to_vec(),
                    rate: 48000,
                    channels: 2,
                })
                .unwrap();
            }
        },
    );
    let (stereo44, f44) = noise(44100, 2);
    audio(
        "Resample 44.1 to 48 kHz stereo",
        (&stereo44, &f44),
        "aresample=48000",
        &mut || {
            resample(&stereo44, 48000).unwrap();
        },
    );
    let (mono48, fm) = noise(48000, 1);
    audio(
        "Resample 48 to 16 kHz mono",
        (&mono48, &fm),
        "aresample=16000",
        &mut || {
            resample(&mono48, 16000).unwrap();
        },
    );
    audio(
        "Normalise to -16 LUFS, 48 kHz stereo (ffmpeg: loudnorm)",
        (&stereo48, &f48),
        "loudnorm=I=-16:TP=-1.5",
        &mut || {
            let mut b = stereo48.clone();
            normalize(&mut b, -16.0, -1.5).unwrap();
        },
    );

    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mut md = format!(
        "| Operation | pito-codecs, {threads} threads | pito-codecs, 1 thread | ffmpeg, {threads} threads | ffmpeg, 1 thread | Unit |\n|---|---|---|---|---|---|\n"
    );
    let cell = |v: f64| {
        if v.is_nan() {
            "–".to_string()
        } else {
            format!("{v:.0}")
        }
    };
    for (label, v, unit) in &rows {
        md += &format!(
            "| {label} | {} | {} | {} | {} | {unit} |\n",
            cell(v[0]),
            cell(v[1]),
            cell(v[2]),
            cell(v[3])
        );
    }
    md += &format!(
        "\n{} ; load average {}\n",
        ffmpeg_version(),
        load.split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("{md}");
    std::fs::write(dir().join("compare.md"), md).unwrap();
}

#[test]
fn hostile_input_returns_errors_instead_of_panicking() {
    let tb = TimeBase::new(1, 30);
    let mut frames = Vec::new();
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        for (w, h) in [(0, 0), (1, 1), (3, 5), (17, 1), (1, 33)] {
            frames.push(Frame::new(format, w, h, 0, tb));
        }
        let mut short = Frame::new(format, 64, 64, 0, tb);
        short.planes[1].data.truncate(10);
        frames.push(short);
        let mut lying = Frame::new(format, 64, 64, 0, tb);
        lying.width = u32::MAX;
        lying.height = u32::MAX;
        frames.push(lying);
        let mut wide = Frame::new(format, 8, 8, 0, tb);
        wide.planes[0].stride = usize::MAX / 2;
        frames.push(wide);
    }
    let mut out = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, tb);
    let fill = Fill::black(Range::Limited, PixelFormat::Yuv420p8);
    for f in &frames {
        for r in [Rotation::R90, Rotation::R180, Rotation::R270] {
            let _ = rotate(f, r, &mut out);
        }
        let _ = convert_color(
            f,
            ColorSpec::BT601,
            ColorSpec::BT709,
            PixelFormat::Yuv420p10,
            &mut out,
        );
        let mut rgba = Rgba::new(0, 0);
        let _ = yuv_to_rgba(f, ColorSpec::BT709, &mut rgba);
        for k in [Kernel::Bilinear, Kernel::Lanczos] {
            let _ = scale(f, 7, 3, k);
            let _ = scale(f, u32::MAX, u32::MAX, k);
        }
        let _ = crop(f, 1, 1, 2, 2, &mut out);
        let _ = pad(f, u32::MAX, u32::MAX, 0, 0, fill, &mut out);
        let _ = pad(f, 70, 70, 3, 3, fill, &mut out);
        let mut g = f.clone();
        let _ = flip_horizontal(&mut g);
        let _ = flip_vertical(&mut g);
        let mut packed = vec![0u8; 16];
        let _ = yuv_to_packed(
            f,
            ColorSpec::BT709,
            Chroma::Bilinear,
            PackedMut::new(2, 2, Order::Bgra, &mut packed),
        );
        let _ = packed_to_yuv(
            Packed::new(u32::MAX, u32::MAX, Order::Bgrx, &packed),
            ColorSpec::BT709,
            f.format,
            &mut out,
        );
        let _ = rgba_to_yuv(
            &Rgba {
                width: u32::MAX,
                height: 2,
                data: vec![0; 8],
            },
            ColorSpec::BT709,
            f.format,
            &mut out,
        );
        let _ = Overlay::new(
            &Rgba {
                width: u32::MAX,
                height: u32::MAX,
                data: vec![0; 8],
            },
            ColorSpec::BT709,
            f.format,
        );
        if let Ok(o) = Overlay::new(&Rgba::new(5, 3), ColorSpec::BT709, f.format) {
            let mut g = f.clone();
            let _ = o.draw(&mut g, -3, i32::MAX);
            let _ = o.draw(&mut g, i32::MIN, 1);
        }
    }
    let ragged = AudioBuffer {
        samples: vec![0.5; 7],
        rate: 48000,
        channels: 2,
    };
    let empty = AudioBuffer::new(44100, 1);
    let odd = AudioBuffer {
        samples: vec![f32::NAN, f32::INFINITY, -1.0],
        rate: 1,
        channels: 3,
    };
    for a in [&ragged, &empty, &odd] {
        for rate in [0, 1, 8000, 48000, 384000, u32::MAX] {
            let _ = resample(a, rate);
            let _ = Resampler::new(a.rate, rate, a.channels);
        }
        for ch in [0, 1, 2, 9] {
            let _ = downmix(a, ch);
        }
        if let Ok(mut m) = LoudnessMeter::new(a.rate.max(1), a.channels.max(1)) {
            let _ = m.process(a);
            let _ = m.summary();
        }
        let mut b = a.clone();
        let _ = normalize(&mut b, -16.0, -1.0);
        gain(&mut b, f32::NAN);
        if let Ok(mut l) = Limiter::new(a.rate, a.channels, -1.0) {
            let mut o = AudioBuffer::new(a.rate, a.channels);
            let _ = l.process(a, &mut o);
            l.flush(&mut o);
        }
    }
    let _ = FpsConverter::new(0, 0);
    if let Ok(mut c) = FpsConverter::new(30, 1) {
        for pts in [i64::MAX, i64::MIN, 0, -1] {
            let _ = c.push(Frame::new(
                PixelFormat::Yuv420p8,
                2,
                2,
                pts,
                TimeBase::new(1, 0),
            ));
        }
        let _ = c.flush();
    }
}

mod overlay_reference {
    use super::super::plane::{Sample, check, samples_mut};
    use super::*;

    struct Layer {
        width: usize,
        height: usize,
        alpha: Vec<u16>,
        value: Vec<u32>,
        spans: Vec<(usize, usize)>,
    }

    impl Layer {
        fn new(width: usize, height: usize, alpha: Vec<u16>, value: Vec<u32>) -> Self {
            let spans = (0..height)
                .map(|y| {
                    let row = &alpha[y * width..(y + 1) * width];
                    match row.iter().position(|&a| a > 0) {
                        Some(first) => {
                            let last = row.iter().rposition(|&a| a > 0).unwrap_or(first);
                            (first, last + 1)
                        }
                        None => (0, 0),
                    }
                })
                .collect();
            Layer {
                width,
                height,
                alpha,
                value,
                spans,
            }
        }

        fn draw<S: Sample>(
            &self,
            dst: &mut [S],
            stride: usize,
            dw: usize,
            dh: usize,
            x: i64,
            y: i64,
        ) {
            for r in 0..self.height {
                let ty = y + r as i64;
                if ty < 0 || ty >= dh as i64 {
                    continue;
                }
                let (a, b) = self.spans[r];
                let from = (a as i64).max(-x) as usize;
                let to = (b as i64).min(dw as i64 - x);
                if to <= from as i64 {
                    continue;
                }
                let to = to as usize;
                let row = &mut dst[ty as usize * stride..];
                let base = r * self.width;
                for c in from..to {
                    let al = self.alpha[base + c] as u32;
                    if al == 0 {
                        continue;
                    }
                    let o = &mut row[(x + c as i64) as usize];
                    let v = (o.get() as u32 * (256 - al) + self.value[base + c] + 128) >> 8;
                    *o = S::put(v as i32);
                }
            }
        }
    }

    struct Reference {
        format: PixelFormat,
        luma: Layer,
        u: Layer,
        v: Layer,
    }

    impl Reference {
        fn new(image: &Rgba, spec: ColorSpec, format: PixelFormat) -> Self {
            let w = image.width as usize;
            let h = image.height as usize;
            let l = spec.levels(format.bit_depth());
            let m = spec.matrix.forward();
            let top = ((1u32 << format.bit_depth()) - 1) as f64;
            let mut ya = vec![0u16; w * h];
            let mut yv = vec![0u32; w * h];
            let mut planes = vec![[0.0f64; 3]; w * h];
            for (i, p) in image.data.chunks_exact(4).take(w * h).enumerate() {
                let c = [
                    p[0] as f64 / 255.0,
                    p[1] as f64 / 255.0,
                    p[2] as f64 / 255.0,
                ];
                let dot = |k: usize| m[k][0] * c[0] + m[k][1] * c[1] + m[k][2] * c[2];
                let a = (p[3] as f64 * 256.0 / 255.0).round();
                let y = (l.y_off + l.y_range * dot(0)).clamp(0.0, top);
                ya[i] = a as u16;
                yv[i] = (y * a).round() as u32;
                planes[i] = [
                    a,
                    a * (l.c_off + l.c_range * dot(1)),
                    a * (l.c_off + l.c_range * dot(2)),
                ];
            }
            let cw = w.div_ceil(2);
            let ch = h.div_ceil(2);
            let mut ca = vec![0u16; cw * ch];
            let mut cu = vec![0u32; cw * ch];
            let mut cv = vec![0u32; cw * ch];
            for cy in 0..ch {
                for cx in 0..cw {
                    let mut s = [0.0f64; 3];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (px, py) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                        let q = planes[py * w + px];
                        s.iter_mut().zip(q).for_each(|(a, b)| *a += b);
                    }
                    let i = cy * cw + cx;
                    ca[i] = (s[0] / 4.0).round() as u16;
                    cu[i] = (s[1] / 4.0).round().clamp(0.0, top * 256.0) as u32;
                    cv[i] = (s[2] / 4.0).round().clamp(0.0, top * 256.0) as u32;
                }
            }
            Reference {
                format,
                luma: Layer::new(w, h, ya, yv),
                u: Layer::new(cw, ch, ca.clone(), cu),
                v: Layer::new(cw, ch, ca, cv),
            }
        }

        fn draw(&self, frame: &mut Frame, x: i32, y: i32) {
            check(frame).unwrap();
            let (fw, fh) = (frame.width as usize, frame.height as usize);
            let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
            let (x, y) = (x as i64, y as i64);
            let (cx, cy) = (x.div_euclid(2), y.div_euclid(2));
            let jobs = [
                (&self.luma, fw, fh, x, y),
                (&self.u, cw, ch, cx, cy),
                (&self.v, cw, ch, cx, cy),
            ];
            for (plane, (layer, w, h, px, py)) in frame.planes.iter_mut().zip(jobs) {
                match self.format {
                    PixelFormat::Yuv420p8 => {
                        let stride = plane.stride;
                        layer.draw(samples_mut::<u8>(plane).unwrap(), stride, w, h, px, py);
                    }
                    PixelFormat::Yuv420p10 => {
                        let stride = plane.stride / 2;
                        layer.draw(samples_mut::<u16>(plane).unwrap(), stride, w, h, px, py);
                    }
                }
            }
        }
    }

    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    fn layers(w: u32, h: u32, noise: &mut Noise) -> Vec<Rgba> {
        let mut out = Vec::new();
        let mut make = |f: &mut dyn FnMut(u32, u32, &mut [u8])| {
            let mut img = Rgba::new(w, h);
            for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
                f(i as u32 % w, i as u32 / w, px);
            }
            out.push(img);
        };
        make(&mut |_, _, _| {});
        make(&mut |x, y, px| {
            px.copy_from_slice(&[(x * 37) as u8, (y * 91) as u8, (x ^ y) as u8, 255])
        });
        make(&mut |_, _, px| {
            let r = noise.next();
            px.copy_from_slice(&r.to_le_bytes());
            if r.is_multiple_of(3) {
                px[3] = 0;
            }
        });
        make(&mut |_, _, px| {
            if noise.next().is_multiple_of(23) {
                px.copy_from_slice(&noise.next().to_le_bytes());
                px[3] = px[3].max(1);
            }
        });
        let colours = [[255, 255, 255], [255, 220, 0], [0, 0, 0], [30, 140, 255]];
        make(&mut |x, y, px| {
            let band = y * 3 >= h && y * 4 < h * 3 + 1 && x * 8 >= w && x * 8 < w * 7;
            if band {
                let c = colours[((x / 3 + y) % 4) as usize];
                let edge = (x * 13 + y * 7) % 9;
                let a = if edge < 3 {
                    0
                } else {
                    [40, 128, 255, 255, 255, 200][edge as usize - 3]
                };
                px.copy_from_slice(&[c[0], c[1], c[2], a]);
                if edge == 3 {
                    px[0] = px[0] / 2 + 10;
                }
            }
        });
        make(&mut |x, y, px| {
            let c = colours[((x / 4 + y / 4) % 4) as usize];
            let a = [0, 255, 128, 1][((x / 6 + y / 2) % 4) as usize];
            px.copy_from_slice(&[c[0], c[1], c[2], a]);
        });
        if w > 1 && h > 1 {
            make(&mut |x, y, px| {
                if x == w - 1 && y == h - 1 {
                    px.copy_from_slice(&[200, 30, 90, 1]);
                }
            });
            make(&mut |x, y, px| {
                if (x == 1 && y == 1) || (x == w / 2 && y == h - 2) {
                    px.copy_from_slice(&[10, 250, 90, 2]);
                }
            });
        }
        out
    }

    fn canvas(format: PixelFormat, w: u32, h: u32, noise: &mut Noise) -> Frame {
        let mut f = Frame::new(format, w, h, 0, TimeBase::new(1, 30));
        for plane in &mut f.planes {
            match format {
                PixelFormat::Yuv420p8 => {
                    plane.data.iter_mut().for_each(|s| *s = noise.next() as u8)
                }
                PixelFormat::Yuv420p10 => samples_mut::<u16>(plane)
                    .unwrap()
                    .iter_mut()
                    .for_each(|s| *s = (noise.next() % 1024) as u16),
            }
        }
        f
    }

    #[test]
    fn a_cropped_overlay_blends_what_the_full_size_one_blended() {
        let mut noise = Noise(7);
        let specs = [
            ColorSpec::BT709,
            ColorSpec::new(Matrix::Bt601, Range::Full),
            ColorSpec::new(Matrix::Bt2020, Range::Limited),
        ];
        let sizes = [
            (1, 1),
            (2, 2),
            (1, 6),
            (7, 5),
            (33, 17),
            (64, 36),
            (101, 3),
            (40, 41),
        ];
        for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
            for (k, &(w, h)) in sizes.iter().enumerate() {
                let spec = specs[k % specs.len()];
                for layer in layers(w, h, &mut noise) {
                    let ours = Overlay::new(&layer, spec, format).unwrap();
                    let reference = Reference::new(&layer, spec, format);
                    for (fw, fh) in [(48u32, 30u32), (9, 7)] {
                        let (lw, lh) = (w as i32, h as i32);
                        let (fw_, fh_) = (fw as i32, fh as i32);
                        for (x, y) in [
                            (0, 0),
                            (3, 2),
                            (-3, -2),
                            (-1, 5),
                            (5, -1),
                            (fw_ - 2, fh_ - 1),
                            (fw_ - lw / 2, fh_ - lh / 2 - 1),
                            (-lw - 1, 0),
                            (0, -lh - 2),
                            (fw_ + 3, fh_ + 3),
                        ] {
                            let mut a = canvas(format, fw, fh, &mut noise);
                            let mut b = a.clone();
                            ours.draw(&mut a, x, y).unwrap();
                            reference.draw(&mut b, x, y);
                            assert_eq!(a, b, "{format:?} {w}x{h} on {fw}x{fh} at {x},{y}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn linear_light_resize_keeps_light_and_alpha_edges() {
    let mut board = Rgba::new(64, 64);
    for (i, px) in board.data.chunks_exact_mut(4).enumerate() {
        let v = if (i % 64 + i / 64) % 2 == 0 { 255 } else { 0 };
        px.copy_from_slice(&[v, v, v, 255]);
    }
    let s = Scaler::new(64, 64, 16, 16, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear);
    let mut out = Rgba::new(1, 1);
    s.convert(Image::Rgba(&board), ImageMut::Rgba(&mut out))
        .unwrap();
    assert_eq!((out.width, out.height), (16, 16));
    assert!(
        out.data
            .chunks_exact(4)
            .all(|p| (180..=200).contains(&p[0]) && p[3] == 255)
    );

    let mut edge = Rgba::new(64, 32);
    for (i, px) in edge.data.chunks_exact_mut(4).enumerate() {
        if i % 64 < 32 {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let s = Scaler::new(64, 32, 21, 11, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear);
    s.convert(Image::Rgba(&edge), ImageMut::Rgba(&mut out))
        .unwrap();
    let seen: Vec<_> = out.data.chunks_exact(4).filter(|p| p[3] > 8).collect();
    assert!(seen.iter().any(|p| p[3] < 250));
    assert!(seen.iter().all(|p| p[0] >= 250 && p[1] <= 4 && p[2] <= 4));

    let mut flat = Frame::new(PixelFormat::Yuv420p10, 64, 48, 0, TimeBase::new(1, 25));
    for (p, v) in flat.planes.iter_mut().zip([400u16, 450, 560]) {
        p.data
            .chunks_exact_mut(2)
            .for_each(|c| c.copy_from_slice(&v.to_ne_bytes()));
    }
    let s = Scaler::new(64, 48, 40, 30, PixelFormat::Yuv420p10, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear)
        .output(PixelFormat::Yuv420p8);
    let mut small = Frame::new(PixelFormat::Yuv420p8, 2, 2, 0, TimeBase::new(1, 25));
    s.run(&flat, &mut small).unwrap();
    for (p, want) in small.planes.iter().zip([100u8, 112, 140]) {
        assert!(p.data.iter().all(|&v| v.abs_diff(want) <= 1), "{want}");
    }
}
