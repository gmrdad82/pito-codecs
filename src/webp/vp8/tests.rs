use super::super::Options;
use super::super::oracle::{self, lib};
use crate::media::Rgba;
use crate::testref::{self, Digest, Refs};

use super::super::tests::refs;

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


fn yuv_digest(f: &super::dec::Frame) -> String {
    let mut d = Digest::new();
    for p in crop(f) {
        d.bytes(&p);
    }
    d.hex()
}

#[test]
fn decodes_libwebp_bit_exact() {
    let r = Refs::open(refs::AREA);
    let lib = lib();
    for case in &refs::LOSSY {
        let name = case.libwebp();
        let file = r.bytes(&format!("{name}.webp"));
        let data = vp8_chunk(&file);
        let f = super::decode_yuv(data).expect("decode");
        let (w, h) = case.size;
        assert_eq!((f.width, f.height), (w as usize, h as usize), "{name}");
        r.assert_digest(&format!("{name}.yuv420p"), &yuv_digest(&f));
        let Some(lib) = lib else { continue };
        let ours = crop(&f);
        let theirs = lib.decode_yuv(&file).expect("libwebp decode");
        for (p, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert!(a == b, "{name}: plane {p} differs");
        }
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        super::decode_into(data, &mut rgba).expect("rgba");
        let theirs = lib.decode_rgba(&file).expect("libwebp rgba");
        assert!(rgba == theirs.data, "{name}: rgba differs");
    }
}

#[test]
fn decodes_libvpx_partitions_and_simple_filter_like_ffmpeg() {
    let r = Refs::open(refs::AREA);
    for (name, _, profile, slices, sharpness, (w, h), _) in refs::LIBVPX {
        let file = r.bytes(&format!("{name}.ivf"));
        let frame = &file[32 + 12..];
        let f = super::decode_yuv(frame).expect("decode");
        assert_eq!((f.width, f.height), (w as usize, h as usize), "{name}");
        let label = format!(
            "{name}: profile {profile}, {slices} partitions, sharpness {sharpness}: differs from ffmpeg"
        );
        let e = r.entry(&format!("{name}.yuv"));
        assert!(yuv_digest(&f) == e.sha256, "{label}");
    }
}

fn coast(name: &str) -> Option<Rgba> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/bench")
        .join(name);
    let bytes = std::fs::read(path).ok()?;
    #[cfg(feature = "png")]
    {
        crate::png::decode(&bytes).ok()
    }
    #[cfg(not(feature = "png"))]
    {
        let _ = bytes;
        None
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
    for name in ["coast-1920x1080.png", "coast-2560x1440.png"] {
        let Some(img) = coast(name) else {
            continue;
        };
        let img = &img;
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
    let Some(img) = coast("coast-1920x1080.png") else {
        return;
    };
    let img = &img;
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
    let Some(img) = coast("coast-1920x1080.png") else {
        return;
    };
    let file = lib.encode(&img, 75.0);
    let data = vp8_chunk(&file);
    use super::dec::*;
    let (t_parse, _) = timed(10, || {
        let mut p = Parser::new(data).unwrap();
        let mut row = RowData::new(p.hdr.mb_w);
        for my in 0..p.hdr.height.div_ceil(16) {
            p.row(my, &mut row).unwrap();
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
            p.row(my, &mut row).unwrap();
            let y = &mut f.y[my * 16 * ys..(my + 1) * 16 * ys];
            let u = &mut f.u[my * 8 * uvs..(my + 1) * 8 * uvs];
            let v = &mut f.v[my * 8 * uvs..(my + 1) * 8 * uvs];
            r.row(my, &mut row, y, u, v, ys, uvs);
        }
        f
    });
    eprintln!("parse {:.2} ms", t_parse * 1e3);
    let (t_dec, f) = timed(10, || decode(data).unwrap());
    let mut out = vec![0u8; img.data.len()];
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
    let r = Refs::open(refs::AREA);
    let lib = lib();
    for case in &refs::LOSSY {
        let (name, (w, h), q) = (case.ours(), case.size, case.quality);
        let img = case.image();
        let file = crate::webp::encode(&img, &case.options()).expect("encode");
        r.assert_digest(&format!("{name}.webp"), &testref::sha256(&file));
        let ours = vp8_chunk(&file);
        let f = super::decode_yuv(ours).expect("we decode ours");
        r.assert_digest(&format!("{name}.yuv420p"), &yuv_digest(&f));
        let mut back = vec![0u8; img.data.len()];
        super::decode_into(ours, &mut back).expect("we decode ours");
        if let Some(lib) = lib {
            let theirs = lib.decode_rgba(&file).expect("libwebp decodes ours");
            assert!(
                back == theirs.data,
                "{name} {w}x{h} q{q}: libwebp and we disagree"
            );
        }
        let p = rgb_psnr(&img.data, &back);
        let ref_file = r.bytes(&format!("{}.webp", case.libwebp()));
        let mut ref_back = vec![0u8; img.data.len()];
        super::decode_into(vp8_chunk(&ref_file), &mut ref_back).expect("we decode libwebp's");
        let rp = rgb_psnr(&img.data, &ref_back);
        eprintln!(
            "{name} {w}x{h} q{q} e{}: ours {} bytes {p:.2} dB, libwebp {} bytes {rp:.2} dB",
            case.effort,
            file.len(),
            ref_file.len()
        );
        assert!(
            p >= case.min_psnr.min(rp - 1.0),
            "{name} q{q}: PSNR {p:.2} against libwebp's {rp:.2}"
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
