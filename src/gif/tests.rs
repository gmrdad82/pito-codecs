use super::*;
use crate::media::Rgba;
use crate::testref::{self, Digest, Refs};

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

fn rgba_digest(a: &Animation) -> String {
    let mut d = Digest::new();
    for (f, _) in &a.frames {
        d.rgba(f);
    }
    d.hex()
}

fn assert_like_magick(r: &Refs, gif: &str, a: &Animation) {
    r.assert_digest(&format!("{gif}.rgba"), &rgba_digest(a));
}

fn assert_pinned(r: &Refs, gif: &str, bytes: &[u8]) {
    r.assert_digest(&format!("{gif}.gif"), &testref::sha256(bytes));
}

fn same(a: &Rgba, b: &Rgba) -> bool {
    a.data
        .chunks_exact(4)
        .zip(b.data.chunks_exact(4))
        .all(|(p, q)| p == q || (p[3] == 0 && q[3] == 0))
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
fn decodes_magick_animations_like_magick() {
    let r = Refs::open(refs::AREA);
    let (w, h) = refs::SIZE;
    for (name, _) in refs::MAGICK_ANIMS {
        let a = decode(&r.bytes(&format!("{name}.gif"))).unwrap();
        assert_eq!((a.width, a.height), (w, h), "{name}");
        assert_eq!(a.frames.len() as u64, refs::FRAMES, "{name}");
        assert_like_magick(&r, name, &a);
    }
}

#[test]
fn magick_decodes_ours_exactly() {
    let r = Refs::open(refs::AREA);
    let src = refs::source();
    for (name, _) in refs::ours() {
        let bytes = refs::encode_ours(name).unwrap();
        assert_pinned(&r, name, &bytes);
        let a = decode(&bytes).unwrap();
        assert_eq!(a.loop_count, Some(0));
        assert_eq!(a.frames.len(), src.len(), "{name}");
        assert_like_magick(&r, name, &a);
        for (i, ((f, _), s)) in a.frames.iter().zip(&src).enumerate() {
            let p = psnr(f, s);
            assert!(p > 26.0, "{name}: frame {i} PSNR {p:.1}");
        }
    }
}

#[test]
fn a_damaged_reference_is_refused_before_use() {
    let r = Refs::open(refs::AREA);
    let mut bytes = r.bytes("im-global.gif");
    bytes[20] ^= 1;
    assert_ne!(testref::sha256(&bytes), r.digest("im-global.gif"));
    let a = decode(&bytes).ok().map(|a| rgba_digest(&a));
    assert_ne!(a.as_deref(), Some(r.digest("im-global.rgba")));
}

#[test]
fn batches_do_not_change_the_output() {
    let src = testref::rgba_frames(160, 120, 30, 0);
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
    let r = Refs::open(refs::AREA);
    let (frames, delays) = refs::alpha_frames();
    let bytes = refs::encode_alpha().unwrap();
    assert_pinned(&r, "alpha", &bytes);
    let a = decode(&bytes).unwrap();
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
    assert_like_magick(&r, "alpha", &a);
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
    let r = Refs::open(refs::AREA);
    for (name, w, h, _) in refs::MAGICK_GIFS {
        let a = decode(&r.bytes(&format!("{name}.gif"))).unwrap();
        assert_eq!((a.width, a.height), (w, h), "{name}");
        assert_like_magick(&r, name, &a);
    }
    let dispose = decode(&r.bytes("im-dispose.gif")).unwrap();
    assert_eq!(dispose.loop_count, Some(2));
    assert!(r.bytes("im-87.gif").starts_with(b"GIF87a"));
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
