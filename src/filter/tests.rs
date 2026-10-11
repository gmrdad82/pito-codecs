use super::*;
use crate::media::{AudioBuffer, Frame, PixelFormat, TimeBase};
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
    std::fs::rename(&part, path(name)).unwrap();
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
        std::fs::rename(&part, path(&name)).unwrap();
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
        rgba_to_yuv(&image, spec, PixelFormat::Yuv420p8, &mut ours).unwrap();
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
