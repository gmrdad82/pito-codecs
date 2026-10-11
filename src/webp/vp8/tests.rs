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
