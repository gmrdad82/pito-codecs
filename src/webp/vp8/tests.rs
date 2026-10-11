use super::super::oracle::{self, lib};
use super::super::{Options, Rgba};

fn vp8_chunk(file: &[u8]) -> &[u8] {
    let mut at = 12;
    while at + 8 <= file.len() {
        let id = &file[at..at + 4];
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == b"VP8 " {
            return &file[at + 8..at + 8 + len];
        }
        at += 8 + len + (len & 1);
    }
    panic!("no VP8 chunk");
}

fn crop(f: &super::dec::Frame) -> [Vec<u8>; 3] {
    let (w, h) = (f.width, f.height);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = Vec::with_capacity(w * h);
    for r in 0..h {
        y.extend_from_slice(&f.y[r * f.ys..r * f.ys + w]);
    }
    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for r in 0..ch {
        u.extend_from_slice(&f.u[r * f.uvs..r * f.uvs + cw]);
        v.extend_from_slice(&f.v[r * f.uvs..r * f.uvs + cw]);
    }
    [y, u, v]
}

#[test]
fn decodes_libwebp_bit_exact() {
    let Some(lib) = lib() else { return };
    let cases = [
        ("testsrc2", 320, 240, 75.0),
        ("mandelbrot", 333, 187, 90.0),
        ("smptehdbars", 127, 61, 20.0),
        ("testsrc", 64, 64, 100.0),
        ("cellauto", 97, 33, 5.0),
    ];
    for (src, w, h, q) in cases {
        let Some(frames) = oracle::source(src, w, h, 1) else {
            return;
        };
        let file = lib.encode(&frames[0], q);
        let data = vp8_chunk(&file);
        let f = super::decode_yuv(data).expect("decode");
        eprintln!("{src} {} bytes", data.len());
        let ours = crop(&f);
        let theirs = lib.decode_yuv(&file).expect("libwebp decode");
        for (p, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert!(a == b, "{src} q{q}: plane {p} differs");
        }
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        super::decode_into(data, &mut rgba).expect("rgba");
        let theirs = lib.decode_rgba(&file).expect("libwebp rgba");
        assert!(rgba == theirs.data, "{src} q{q}: rgba differs");
    }
    let _ = (Options::default(), Rgba::new(1, 1));
}

#[test]
fn decodes_libvpx_partitions_and_simple_filter_like_ffmpeg() {
    let cases = [
        ("testsrc2", "0", "1", "0", 352, 288, "30"),
        ("mandelbrot", "0", "8", "0", 352, 288, "50"),
        ("testsrc2", "0", "4", "3", 352, 288, "50"),
        ("mandelbrot", "1", "1", "0", 352, 288, "50"),
        ("testsrc2", "1", "8", "6", 640, 360, "40"),
        ("mandelbrot", "3", "2", "2", 98, 62, "60"),
    ];
    for (src, profile, slices, sharpness, w, h, q) in cases {
        let ivf = oracle::dir().join(format!("libvpx-{profile}-{}.ivf", std::process::id()));
        let lavfi = format!("{src}=s={w}x{h}:r=1");
        let made = oracle::ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &lavfi,
            "-frames:v",
            "1",
            "-c:v",
            "libvpx",
            "-profile:v",
            profile,
            "-slices",
            slices,
            "-sharpness",
            sharpness,
            "-qmin",
            q,
            "-qmax",
            "63",
            "-b:v",
            "50k",
            "-f",
            "ivf",
            ivf.to_str().unwrap(),
        ]);
        if !made {
            return;
        }
        let file = std::fs::read(&ivf).unwrap();
        let theirs = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(&ivf)
            .args([
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .unwrap();
        std::fs::remove_file(&ivf).ok();
        assert!(theirs.status.success());
        let frame = &file[32 + 12..];
        let f = super::decode_yuv(frame).expect("decode");
        assert!(
            crop(&f).concat() == theirs.stdout,
            "{src} profile {profile}, {slices} partitions, sharpness {sharpness}: differs from ffmpeg"
        );
    }
}

fn timed<T>(n: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut out = f();
    for _ in 0..n {
        let t = std::time::Instant::now();
        out = f();
        best = best.min(t.elapsed().as_secs_f64());
    }
    (best, out)
}

#[test]
#[ignore]
fn bench() {
    let Some(lib) = lib() else { return };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/bench");
    for name in ["coast-1920x1080.png", "coast-2560x1440.png"] {
        let Some(img) = oracle::ffmpeg_decode(&root.join(name)) else {
            continue;
        };
        let img = &img[0];
        let mp = (img.width * img.height) as f64 / 1e6;
        let file = lib.encode(img, 75.0);
        let data = vp8_chunk(&file);
        let mut out = vec![0u8; img.data.len()];
        let (ours, _) = timed(10, || super::decode_into(data, &mut out).unwrap());
        let (theirs, _) = timed(10, || lib.decode_rgba(&file).unwrap());
        eprintln!(
            "{name}: decode ours {:.2} ms ({:.0} MP/s), libwebp {:.2} ms ({:.0} MP/s), {} bytes",
            ours * 1e3,
            mp / ours,
            theirs * 1e3,
            mp / theirs,
            file.len()
        );
        for (q, effort) in [(75.0, 4u8), (75.0, 2), (90.0, 4)] {
            let options = Options {
                quality: q,
                effort,
                ..Options::lossy(q)
            };
            let (te, ours) = timed(3, || {
                super::encode(&img.data, img.width, img.height, &options).unwrap()
            });
            let (tl, theirs) = timed(3, || lib.encode(img, q));
            let ob = lib.decode_rgba(&riff(&ours)).unwrap();
            let tb = lib.decode_rgba(&theirs).unwrap();
            eprintln!(
                "{name} q{q} e{effort}: encode ours {:.1} ms ({:.1} MP/s) {} bytes {:.2} dB, libwebp {:.1} ms ({:.1} MP/s) {} bytes {:.2} dB",
                te * 1e3,
                mp / te,
                ours.len(),
                rgb_psnr(&img.data, &ob.data),
                tl * 1e3,
                mp / tl,
                theirs.len(),
                rgb_psnr(&img.data, &tb.data)
            );
        }
    }
}

#[test]
#[ignore]
fn bench_one_thread() {
    let Some(lib) = lib() else { return };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/bench");
    let Some(img) = oracle::ffmpeg_decode(&root.join("coast-1920x1080.png")) else {
        return;
    };
    let img = &img[0];
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    for effort in [0u8, 2, 4, 6] {
        let options = Options {
            quality: 75.0,
            effort,
            ..Options::lossy(75.0)
        };
        let (te, ours) = timed(3, || {
            one.install(|| super::encode(&img.data, img.width, img.height, &options).unwrap())
        });
        let (tl, theirs) = timed(3, || lib.encode(img, 75.0));
        eprintln!(
            "1 thread e{effort}: ours {:.1} ms {} bytes, libwebp {:.1} ms {} bytes",
            te * 1e3,
            ours.len(),
            tl * 1e3,
            theirs.len()
        );
    }
}

#[test]
#[ignore]
fn phases() {
    let Some(lib) = lib() else { return };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/bench");
    let Some(img) = oracle::ffmpeg_decode(&root.join("coast-1920x1080.png")) else {
        return;
    };
    let file = lib.encode(&img[0], 75.0);
    let data = vp8_chunk(&file);
    use super::dec::*;
    let (t_parse, _) = timed(10, || {
        let mut p = Parser::new(data).unwrap();
        let mut row = RowData::new(p.hdr.mb_w);
        for my in 0..p.hdr.height.div_ceil(16) {
            p.row(my, &mut row);
            row.coeffs.fill(0);
        }
    });
    let (t_rows, _) = timed(10, || {
        let mut p = Parser::new(data).unwrap();
        let mut f = Frame::new(p.hdr.width, p.hdr.height);
        let mut r = Recon::new(f.mb_w);
        let mut row = RowData::new(f.mb_w);
        let (ys, uvs) = (f.ys, f.uvs);
        for my in 0..f.mb_h {
            p.row(my, &mut row);
            let y = &mut f.y[my * 16 * ys..(my + 1) * 16 * ys];
            let u = &mut f.u[my * 8 * uvs..(my + 1) * 8 * uvs];
            let v = &mut f.v[my * 8 * uvs..(my + 1) * 8 * uvs];
            r.row(my, &mut row, y, u, v, ys, uvs);
        }
        f
    });
    eprintln!("parse {:.2} ms", t_parse * 1e3);
    let (t_dec, f) = timed(10, || decode(data).unwrap());
    let mut out = vec![0u8; img[0].data.len()];
    let (t_rgb, _) = timed(10, || {
        let p = super::yuv::Yuv {
            y: &f.y,
            u: &f.u,
            v: &f.v,
            ys: f.ys,
            uvs: f.uvs,
            yr: 0,
            cr: 0,
            width: f.width,
            height: f.height,
        };
        super::yuv::to_rgba_rows(&p, 0..f.height, &mut out)
    });
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let (t_one, _) = timed(10, || one.install(|| decode_rgba(data, &mut out).unwrap()));
    let (t_all, _) = timed(10, || decode_rgba(data, &mut out).unwrap());
    let (t_lib_yuv, _) = timed(10, || lib.decode_yuv(&file).unwrap());
    eprintln!(
        "libwebp yuv {:.2} ms, ours yuv {:.2} ms",
        t_lib_yuv * 1e3,
        t_dec * 1e3
    );
    eprintln!(
        "rows {:.2} ms, filter {:.2} ms, rgba {:.2} ms, 1 thread {:.2} ms, all {:.2} ms",
        t_rows * 1e3,
        (t_dec - t_rows) * 1e3,
        t_rgb * 1e3,
        t_one * 1e3,
        t_all * 1e3
    );
}

#[test]
fn simd_filter_matches_scalar() {
    use super::filter::{self, Planes, strength};
    let (mb_w, mb_h) = (3usize, 3usize);
    let (ys, uvs) = (mb_w * 16, mb_w * 8);
    let mut seed = 0x1234_5678u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for round in 0..200 {
        let spread = [4u32, 16, 64, 256][round % 4];
        let base = (rnd() % 256) as i32;
        let mut px = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| (base + (rnd() % spread) as i32 - spread as i32 / 2).clamp(0, 255) as u8)
                .collect()
        };
        let y = px(ys * mb_h * 16);
        let u = px(uvs * mb_h * 8);
        let v = px(uvs * mb_h * 8);
        let level = (rnd() % 64) as i32;
        let sharp = (rnd() % 8) as u8;
        let st = strength(level, sharp, rnd() % 2 == 0);
        let simple = round % 5 == 0;
        let (mut a, mut b) = ((y.clone(), u.clone(), v.clone()), (y, u, v));
        for my in 0..mb_h {
            for mx in 0..mb_w {
                let mut pa = Planes {
                    y: &mut a.0,
                    u: &mut a.1,
                    v: &mut a.2,
                    ys,
                    uvs,
                    yr: 0,
                    cr: 0,
                };
                filter::scalar::filter_mb(&mut pa, mx, my, st, simple);
                let mut pb = Planes {
                    y: &mut b.0,
                    u: &mut b.1,
                    v: &mut b.2,
                    ys,
                    uvs,
                    yr: 0,
                    cr: 0,
                };
                filter::filter_mb_fast(&mut pb, mx, my, st, simple);
            }
        }
        assert!(
            a == b,
            "round {round}: level {level} sharpness {sharp} simple {simple}"
        );
    }
}

fn riff(vp8: &[u8]) -> Vec<u8> {
    let pad = vp8.len() & 1;
    let mut f = Vec::with_capacity(vp8.len() + 20 + pad);
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&((4 + 8 + vp8.len() + pad) as u32).to_le_bytes());
    f.extend_from_slice(b"WEBPVP8 ");
    f.extend_from_slice(&(vp8.len() as u32).to_le_bytes());
    f.extend_from_slice(vp8);
    if pad == 1 {
        f.push(0);
    }
    f
}

fn rgb_psnr(a: &[u8], b: &[u8]) -> f64 {
    let strip = |v: &[u8]| {
        v.chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect::<Vec<u8>>()
    };
    oracle::psnr(&strip(a), &strip(b))
}

#[test]
fn encode_decodes_in_libwebp() {
    let Some(lib) = lib() else { return };
    let cases = [
        ("testsrc2", 320, 240, 75.0, 4u8, 26.0f64),
        ("mandelbrot", 333, 187, 90.0, 6, 30.0),
        ("smptehdbars", 127, 61, 20.0, 0, 30.0),
        ("testsrc", 1, 1, 50.0, 4, 30.0),
        ("cellauto", 97, 33, 100.0, 2, 30.0),
        ("testsrc2", 640, 512, 50.0, 4, 25.0),
    ];
    for (src, w, h, q, effort, min_psnr) in cases {
        let Some(frames) = oracle::source(src, w, h, 1) else {
            return;
        };
        let img = &frames[0];
        let options = Options {
            quality: q,
            effort,
            ..Options::lossy(q)
        };
        let ours = super::encode(&img.data, w, h, &options).expect("encode");
        let file = riff(&ours);
        let theirs = lib.decode_rgba(&file).expect("libwebp decodes ours");
        let mut back = vec![0u8; img.data.len()];
        super::decode_into(&ours, &mut back).expect("we decode ours");
        assert!(
            back == theirs.data,
            "{src} {w}x{h} q{q}: libwebp and we disagree"
        );
        let p = rgb_psnr(&img.data, &back);
        let ref_file = lib.encode(img, q);
        let ref_back = lib.decode_rgba(&ref_file).unwrap();
        let rp = rgb_psnr(&img.data, &ref_back.data);
        eprintln!(
            "{src} {w}x{h} q{q} e{effort}: ours {} bytes {p:.2} dB, libwebp {} bytes {rp:.2} dB",
            file.len(),
            ref_file.len()
        );
        assert!(
            p >= min_psnr.min(rp - 1.0),
            "{src} q{q}: PSNR {p:.2} against libwebp's {rp:.2}"
        );
    }
}

#[test]
fn simd_kernels_match_scalar() {
    use super::dsp::{self, BPS};
    let mut seed = 0x9e37_79b9u32;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for round in 0..2000 {
        let spread = [2u32, 16, 256][round % 3];
        let base = rnd() % 256;
        let mut px = || (base + rnd() % spread).min(255) as u8;
        let a: Vec<u8> = (0..BPS * 16).map(|_| px()).collect();
        let b: Vec<u8> = (0..BPS * 16).map(|_| px()).collect();
        assert_eq!(dsp::sse16(&a, 0, &b, 0), dsp::sse16_scalar(&a, 0, &b, 0));
        assert_eq!(dsp::sse8(&a, 3, &b, 5), dsp::sse8_scalar(&a, 3, &b, 5));
        assert_eq!(dsp::sse4(&a, 7, &b, 1), dsp::sse4_scalar(&a, 7, &b, 1));
        let (mut f1, mut f2) = ([0i16; 16], [0i16; 16]);
        dsp::fdct(&a, 1, &b, 2, &mut f1);
        dsp::fdct_scalar(&a, 1, &b, 2, &mut f2);
        assert_eq!(f1, f2, "fdct round {round}");
        let q = 4 + (rnd() % 157) as i16;
        let mut c = [0i16; 16];
        for (i, v) in c.iter_mut().enumerate() {
            if rnd() % 3 == 0 || i == 0 {
                *v = ((rnd() % 64) as i16 - 32) * q;
            }
        }
        let (mut d1, mut d2) = (a.clone(), a.clone());
        dsp::idct_add(&c, &mut d1, 2 * BPS + 4);
        dsp::idct_add_scalar(&c, &mut d2, 2 * BPS + 4);
        assert!(d1 == d2, "idct round {round}");
        assert_eq!(dsp::nz_zigzag(&c), dsp::nz_zigzag_scalar(&c));
        let m = super::enc::Matrix::new([q as i32, q as i32 + 7], [96, 110]);
        let (mut l1, mut l2) = ([0i16; 16], [0i16; 16]);
        let first = round % 2;
        let n1 = super::enc::quantize(&m, &f1, first, &mut l1);
        let n2 = super::enc::quantize_scalar(&m, &f1, first, &mut l2);
        assert_eq!((n1, l1), (n2, l2), "quantize round {round}");
    }
}
