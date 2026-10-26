use super::super::huffman::HuffmanTable;
use super::*;
use crate::testref::{self, Refs};

use crate::jpeg::tests::refs;

use refs::{FORMATS, SIZES, photo, rgb24};

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (f64::from(x) - f64::from(y)).powi(2))
        .sum();
    let mse = sum / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

fn ours(file: &[u8]) -> Vec<u8> {
    decode_planes(file)
        .expect("decode")
        .planes
        .iter()
        .flat_map(|p| p.cropped())
        .collect()
}

fn djpeg_planes(r: &Refs, name: &str, size: (u32, u32), sampling: (usize, usize)) -> Vec<u8> {
    let ppm = r.bytes(&refs::planes(name));
    refs::planes_of(&refs::ppm_pixels(&ppm), size, sampling)
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len(), "plane sizes differ");
    a.iter()
        .zip(b)
        .map(|(&x, &y)| x.abs_diff(y))
        .max()
        .unwrap_or(0)
}

#[test]
fn huffman_codes_of_the_annex_k_tables_decode_back() {
    for (enc, dc) in [
        (HuffmanTable::luma_dc(), true),
        (HuffmanTable::chroma_dc(), true),
        (HuffmanTable::luma_ac(), false),
        (HuffmanTable::chroma_ac(), false),
    ] {
        let mut lengths = [0u8; 17];
        lengths[1..].copy_from_slice(enc.length());
        let t = Table::new(&lengths, enc.values(), dc).expect("table");
        let mut acc = 0u64;
        let mut n = 0u32;
        let mut stream = Vec::new();
        for &v in enc.values() {
            let (len, code) = enc.get_for_value(v);
            acc = (acc << len) | u64::from(code);
            n += u32::from(len);
            while n >= 8 {
                n -= 8;
                let byte = (acc >> n) as u8;
                stream.push(byte);
                if byte == 0xff {
                    stream.push(0);
                }
            }
        }
        if n > 0 {
            let byte = ((acc << (8 - n)) as u8) | ((1u8 << (8 - n)) - 1);
            stream.push(byte);
            if byte == 0xff {
                stream.push(0);
            }
        }
        stream.extend_from_slice(&[0xff, 0xd9]);
        let mut bits = Bits::new(&stream, 0);
        for &v in enc.values() {
            assert_eq!(bits.huff_for_test(&t).expect("symbol"), i32::from(v));
        }
    }
}

#[test]
fn an_oversubscribed_huffman_table_is_refused() {
    let mut lengths = [0u8; 17];
    lengths[1] = 3;
    assert!(Table::new(&lengths, &[0, 1, 2], true).is_err());
    lengths[1] = 2;
    assert!(Table::new(&lengths, &[0, 1], true).is_err());
    let mut lengths = [0u8; 17];
    lengths[2] = 1;
    assert!(Table::new(&lengths, &[16], true).is_err());
    assert!(Table::new(&lengths, &[16], false).is_ok());
    assert!(Table::new(&lengths, &[0, 1], false).is_err());
}

#[test]
fn bits_unstuff_and_stop_at_a_marker() {
    let data = [0xff, 0x00, 0x0f, 0xff, 0xd9];
    let mut bits = Bits::new(&data, 0);
    assert_eq!(bits.get_for_test(8).unwrap(), 0xff);
    assert_eq!(bits.get_for_test(8).unwrap(), 0x0f);
    assert_eq!(bits.marker, Some(0xd9));
    assert_eq!(bits.resume_at(), 3);
    assert!(bits.finish().is_ok());
    assert_eq!(bits.get_for_test(16).unwrap(), 0);
    let e = bits.finish().unwrap_err().0;
    assert!(e.contains("stops at marker 0xFFD9"), "{e}");
}

#[test]
fn bits_past_the_end_of_the_file_are_a_truncation() {
    let data = [0x12, 0x34];
    let mut bits = Bits::new(&data, 0);
    assert_eq!(bits.get_for_test(16).unwrap(), 0x1234);
    assert!(bits.finish().is_ok());
    let _ = bits.get_for_test(8);
    let e = bits.finish().unwrap_err().0;
    assert!(e.contains("ends inside the image data"), "{e}");
}

#[test]
fn extend_follows_the_standard() {
    assert_eq!(bits::extend_for_test(0, 1), -1);
    assert_eq!(bits::extend_for_test(1, 1), 1);
    assert_eq!(bits::extend_for_test(0, 3), -7);
    assert_eq!(bits::extend_for_test(3, 3), -4);
    assert_eq!(bits::extend_for_test(4, 3), 4);
    assert_eq!(bits::extend_for_test(2047, 11), 2047);
    assert_eq!(bits::extend_for_test(0, 11), -2047);
}

fn reference_idct(c: &[i32; 64]) -> [f64; 64] {
    let mut out = [0f64; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0.0;
            for v in 0..8 {
                for u in 0..8 {
                    let cu = if u == 0 { 1.0 / 2f64.sqrt() } else { 1.0 };
                    let cv = if v == 0 { 1.0 / 2f64.sqrt() } else { 1.0 };
                    s += cu
                        * cv
                        * f64::from(c[v * 8 + u])
                        * (((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI) / 16.0).cos()
                        * (((2 * y + 1) as f64 * v as f64 * std::f64::consts::PI) / 16.0).cos();
                }
            }
            out[y * 8 + x] = (s / 4.0 + 128.0).clamp(0.0, 255.0);
        }
    }
    out
}

#[test]
fn idct_stays_within_one_of_the_float_transform() {
    let mut s = 1u32;
    for round in 0..300 {
        let mut c = [0i32; 64];
        let reach = if round % 3 == 0 { 64 } else { 10 };
        for (i, v) in c.iter_mut().enumerate().take(reach) {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let span = if i == 0 { 2000 } else { 200 };
            *v = (s >> 8) as i32 % span - span / 2;
        }
        let want = reference_idct(&c);
        let mut out = [0u8; 64];
        idct::idct(&mut c, &mut out, 8);
        for (i, (&got, &w)) in out.iter().zip(&want).enumerate() {
            assert!(
                (f64::from(got) - w).abs() <= 1.0,
                "round {round} sample {i}: {got} vs {w:.2}"
            );
        }
    }
}

#[test]
fn idct_of_a_dc_block_is_flat_and_respects_the_stride() {
    for (dc, want) in [(0, 128u8), (-1024, 0), (1016, 255), (80, 138)] {
        let mut c = [0i32; 64];
        c[0] = dc;
        let mut out = [7u8; 8 * 12];
        idct::idct(&mut c, &mut out, 12);
        for row in out.chunks(12).take(8) {
            assert!(row[..8].iter().all(|&v| v == want), "dc {dc}: {row:?}");
            assert!(
                row[8..].iter().all(|&v| v == 7),
                "dc {dc} wrote past the block"
            );
        }
    }
}

#[test]
fn horizontal_upsampling_is_the_triangle_filter() {
    let mut out = [0u8; 2];
    sample::horizontal(&[9], &mut out);
    assert_eq!(out, [9, 9]);
    let mut out = [0u8; 4];
    sample::horizontal(&[0, 100], &mut out);
    assert_eq!(out, [0, 25, 75, 100]);
    let mut out = [0u8; 6];
    sample::horizontal(&[0, 100, 200], &mut out);
    assert_eq!(out, [0, 25, 75, 125, 175, 200]);
    let mut short = [0u8; 3];
    sample::horizontal(&[1, 2], &mut short);
    assert_eq!(short, [0, 0, 0]);
}

#[test]
fn upsampling_clamps_rows_at_the_edges() {
    let data = [0u8, 0, 100, 100];
    let p = Plane {
        data: &data,
        stride: 2,
        width: 1,
        height: 2,
    };
    assert_eq!(upsample(&p, 1, 2, 1, 4), vec![0, 25, 75, 100]);
    assert_eq!(upsample(&p, 2, 2, 2, 3), vec![0, 0, 25, 25, 75, 75]);
    assert_eq!(upsample(&p, 3, 1, 3, 2), vec![0, 0, 0, 100, 100, 100]);
    assert_eq!(upsample(&p, 1, 1, 1, 2), vec![0, 100]);
    assert!(upsample(&p, 1, 1, 0, 2).is_empty());
}

#[test]
fn colour_conversion_is_full_range_bt601() {
    let mut px = [0u8; 16];
    sample::ycbcr(
        &[0, 255, 128, 76],
        &[128, 128, 128, 85],
        &[128, 128, 128, 255],
        &mut px,
    );
    assert_eq!(
        &px[..12],
        &[0, 0, 0, 255, 255, 255, 255, 255, 128, 128, 128, 255]
    );
    assert!(
        px[12] >= 253 && px[13] <= 2 && px[14] <= 2,
        "{:?}",
        &px[12..]
    );
}

fn exif(order: &[u8; 2], orientation: u16) -> Vec<u8> {
    let big = order == b"MM";
    let u16b = |v: u16| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u32b = |v: u32| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut t = b"Exif\0\0".to_vec();
    t.extend_from_slice(order);
    t.extend_from_slice(&u16b(42));
    t.extend_from_slice(&u32b(8));
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&u16b(0x010f));
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&u32b(4));
    t.extend_from_slice(b"Pito");
    t.extend_from_slice(&u16b(0x0112));
    t.extend_from_slice(&u16b(3));
    t.extend_from_slice(&u32b(1));
    t.extend_from_slice(&u16b(orientation));
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&u32b(0));
    t
}

#[test]
fn exif_orientation_reads_both_byte_orders() {
    assert_eq!(exif::orientation(&exif(b"II", 6)), Some(6));
    assert_eq!(exif::orientation(&exif(b"MM", 8)), Some(8));
    assert_eq!(exif::orientation(&exif(b"MM", 9)), None);
    assert_eq!(exif::orientation(&exif(b"MM", 0)), None);
    assert_eq!(exif::orientation(b"http://ns.adobe.com/xap/1.0/\0"), None);
    let whole = exif(b"II", 3);
    for cut in 0..whole.len() - 6 {
        assert_eq!(exif::orientation(&whole[..cut]), None, "cut at {cut}");
    }
    let mut far = exif(b"II", 3);
    far[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(exif::orientation(&far), None);
}

fn with_segment(file: &[u8], marker: u8, body: &[u8]) -> Vec<u8> {
    let mut out = file[..2].to_vec();
    out.extend_from_slice(&[0xff, marker]);
    out.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(&file[2..]);
    out
}

#[test]
fn info_reads_the_header_and_the_exif_orientation() {
    let file = super::super::encode(&photo(33, 17), 90).unwrap();
    let i = info(&file).unwrap();
    assert_eq!(
        (i.width, i.height, i.progressive, i.orientation),
        (33, 17, false, 1)
    );
    assert_eq!(i.sampling, vec![(2, 2), (1, 1), (1, 1)]);
    let tagged = with_segment(&file, 0xe1, &exif(b"MM", 6));
    assert_eq!(info(&tagged).unwrap().orientation, 6);
    let a = decode(&file).unwrap();
    let b = decode(&tagged).unwrap();
    assert_eq!(a, b, "orientation is the caller's to apply");
    assert_eq!(info(&file[..sof_end(&file)]).unwrap().width, 33);
}

fn sof_end(file: &[u8]) -> usize {
    let mut at = 2;
    loop {
        let m = file[at + 1];
        let len = usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]));
        at += 2 + len;
        if m == 0xc0 || m == 0xc2 {
            return at;
        }
    }
}

#[test]
fn our_encoder_decodes_back_at_every_size() {
    for (w, h) in [
        (1, 1),
        (2, 2),
        (7, 5),
        (16, 16),
        (33, 17),
        (17, 33),
        (64, 48),
        (161, 91),
    ] {
        let img = photo(w, h);
        let file = super::super::encode(&img, 95).unwrap();
        let got = decode(&file).unwrap();
        assert_eq!((got.width, got.height), (w, h));
        assert!(got.data.chunks_exact(4).all(|p| p[3] == 255));
        if w >= 7 {
            let p = psnr(&rgb24(&img), &rgb24(&got));
            assert!(p > 30.0, "{w}x{h}: {p:.2} dB");
        }
    }
    let r = Refs::open(refs::AREA);
    for (w, h) in refs::EDGE_SIZES {
        let name = refs::ours(refs::OUR_Q95, w, h);
        let file = super::super::encode(&photo(w, h), refs::OUR_Q95).unwrap();
        r.assert_digest(&format!("{name}.jpg"), &testref::sha256(&file));
        let d = max_diff(&ours(&file), &djpeg_planes(&r, &name, (w, h), (2, 2)));
        assert!(d <= 1, "q95 {w}x{h}: off by {d}");
    }
}

#[test]
fn ppm_pixels_of_a_short_or_empty_file_are_empty() {
    assert!(refs::ppm_pixels(&[]).is_empty());
    assert!(refs::ppm_pixels(b"P6\n").is_empty());
    assert!(refs::ppm_pixels(b"P6\n1 1\n255\n").is_empty());
    assert_eq!(
        refs::ppm_pixels(b"P6\n1 1\n255\n\x01\x02\x03"),
        vec![1, 2, 3]
    );
}

#[test]
fn a_flat_colour_comes_back_flat() {
    let mut img = Rgba::new(40, 24);
    for px in img.data.chunks_exact_mut(4) {
        px.copy_from_slice(&[200, 120, 40, 255]);
    }
    let got = decode(&super::super::encode(&img, 100).unwrap()).unwrap();
    for px in got.data.chunks_exact(4) {
        for (c, (&g, &w)) in px.iter().zip(&[200u8, 120, 40, 255]).enumerate() {
            assert!(g.abs_diff(w) <= 2, "channel {c}: {g} vs {w}");
        }
    }
}

#[test]
fn cjpeg_files_decode_within_one_of_djpeg() {
    let r = Refs::open(refs::AREA);
    let mut worst = 0;
    for (pix_fmt, _, sampling) in FORMATS {
        for (w, h) in SIZES {
            let name = refs::base(pix_fmt, w, h);
            let file = r.bytes(&format!("{name}.jpg"));
            let i = info(&file).unwrap();
            assert_eq!((i.width, i.height), (w, h));
            let chroma = &decode_planes(&file).unwrap().planes[1];
            assert_eq!((chroma.fx, chroma.fy), sampling, "{pix_fmt}");
            let d = max_diff(&ours(&file), &djpeg_planes(&r, &name, (w, h), sampling));
            assert!(d <= 1, "{pix_fmt} {w}x{h}: off by {d}");
            worst = worst.max(d);
            let rgba = decode(&file).unwrap();
            assert_eq!((rgba.width, rgba.height), (w, h));
        }
    }
    eprintln!("cjpeg against djpeg: max difference {worst}");
}

#[test]
fn our_encoder_decodes_within_one_of_djpeg() {
    let r = Refs::open(refs::AREA);
    let mut worst = 0;
    for (w, h) in SIZES {
        let name = refs::ours(refs::OUR_Q80, w, h);
        let file = super::super::encode(&photo(w, h), refs::OUR_Q80).unwrap();
        r.assert_digest(&format!("{name}.jpg"), &testref::sha256(&file));
        let d = max_diff(&ours(&file), &djpeg_planes(&r, &name, (w, h), (2, 2)));
        assert!(d <= 1, "{w}x{h}: off by {d}");
        worst = worst.max(d);
    }
    eprintln!("our encoder: max difference {worst}");
}

#[test]
fn progressive_and_restart_files_decode_like_their_baseline() {
    let r = Refs::open(refs::AREA);
    let mut worst = 0;
    for (pix_fmt, _, sampling) in FORMATS {
        for (w, h) in SIZES {
            let name = refs::base(pix_fmt, w, h);
            let want = ours(&r.bytes(&format!("{name}.jpg")));
            let theirs = djpeg_planes(&r, &name, (w, h), sampling);
            for (v, args) in refs::VARIANTS {
                let file = r.bytes(&format!("{}.jpg", refs::variant(&name, v)));
                let i = info(&file).unwrap();
                assert_eq!(i.progressive, args.contains(&"-progressive"), "{v}");
                let got = ours(&file);
                assert_eq!(
                    got, want,
                    "{pix_fmt} {w}x{h} {v}: not the baseline's samples"
                );
                let d = max_diff(&got, &theirs);
                assert!(d <= 1, "{pix_fmt} {w}x{h} {v}: off by {d}");
                worst = worst.max(d);
            }
        }
    }
    eprintln!("progressive and restarts: max difference {worst}");
}

fn transcoded(r: &Refs, q: u8, w: u32, h: u32, v: &str) -> Vec<u8> {
    r.bytes(&format!("{}.jpg", refs::variant(&refs::ours(q, w, h), v)))
}

#[test]
fn our_encoder_made_progressive_decodes_the_same() {
    let r = Refs::open(refs::AREA);
    for (q, w, h, _) in &refs::OURS_TRANSCODED[..3] {
        let base = transcoded(&r, *q, *w, *h, "base");
        let prog = transcoded(&r, *q, *w, *h, "prog");
        assert!(info(&prog).unwrap().progressive, "{w}x{h}");
        assert!(!info(&base).unwrap().progressive, "{w}x{h}");
        assert_eq!(decode(&prog).unwrap(), decode(&base).unwrap(), "{w}x{h}");
    }
}

fn refused(file: &[u8]) -> String {
    let e = match decode(file) {
        Ok(_) => panic!("decoded what should be refused"),
        Err(e) => e.0,
    };
    assert!(e.starts_with("JPEG: "), "{e}");
    assert!(!e.contains('\n'), "more than one line: {e}");
    e
}

#[test]
fn a_truncated_file_is_refused_at_every_cut() {
    let file = super::super::encode(&photo(33, 17), 90).unwrap();
    for cut in 0..file.len() - 2 {
        refused(&file[..cut]);
    }
    let e = refused(&file[..file.len() / 2]);
    assert!(e.contains("ends"), "{e}");
}

#[test]
fn a_progressive_truncated_file_is_refused_at_every_cut() {
    let r = Refs::open(refs::AREA);
    let file = transcoded(&r, 90, 33, 17, "prog-rst1b");
    for cut in 0..file.len() - 2 {
        let _ = refused(&file[..cut]);
    }
}

#[test]
fn a_file_without_a_scan_is_refused() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let mut headers = file[..sof_end(&file)].to_vec();
    headers.extend_from_slice(&[0xff, 0xd9]);
    let e = refused(&headers);
    assert!(e.contains("no start-of-scan"), "{e}");
}

#[test]
fn a_zero_size_frame_is_refused() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let at = file.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    let mut zero = file.clone();
    zero[at + 5..at + 7].copy_from_slice(&[0, 0]);
    assert!(refused(&zero).contains("a 16x0 frame"));
    let mut zero = file.clone();
    zero[at + 7..at + 9].copy_from_slice(&[0, 0]);
    assert!(refused(&zero).contains("a 0x8 frame"));
    assert!(info(&zero).unwrap_err().0.contains("a 0x8 frame"));
}

#[test]
fn an_unknown_marker_is_refused() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let e = refused(&with_segment(&file, 0x02, &[1, 2, 3]));
    assert!(e.contains("unknown marker 0xFF02 at byte 2"), "{e}");
    let mut stray = file.clone();
    let at = stray.len() - 2;
    stray.splice(at..at, [0xff, 0xf3]);
    assert!(refused(&stray).contains("0xFFF3"));
    let reserved = with_segment(&file, 0x02, &[1, 2, 3]);
    let i = info(&reserved).unwrap();
    assert_eq!(
        (i.width, i.height),
        (16, 8),
        "probe skips a segment it can't name"
    );
    let tagged = with_segment(&file, 0xf0, &[9; 40]);
    assert_eq!(info(&tagged).unwrap().width, 16);
}

#[test]
fn other_kinds_of_jpeg_are_refused_by_name() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let at = file.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    for (sof, name) in [
        (0xc3, "lossless"),
        (0xc5, "hierarchical"),
        (0xc9, "arithmetic"),
        (0xca, "arithmetic"),
    ] {
        let mut f = file.clone();
        f[at + 1] = sof;
        assert!(refused(&f).contains(name), "0x{sof:02X}");
    }
    let mut twelve = file.clone();
    twelve[at + 4] = 12;
    assert!(refused(&twelve).contains("12-bit"));
    assert!(refused(b"\x89PNG\r\n").contains("not a JPEG"));
}

fn components(n: u8, adobe: Option<u8>) -> Vec<u8> {
    let mut f = vec![0xff, 0xd8];
    if let Some(t) = adobe {
        f.extend_from_slice(&[0xff, 0xee, 0, 14]);
        f.extend_from_slice(b"Adobe");
        f.extend_from_slice(&[0, 100, 0, 0, 0, 0, t]);
    }
    f.extend_from_slice(&[0xff, 0xc0, 0, 8 + 3 * n, 8, 0, 8, 0, 8, n]);
    for id in 1..=n {
        f.extend_from_slice(&[id, 0x11, 0]);
    }
    f.extend_from_slice(&[0xff, 0xda, 0, 6 + 2 * n, n]);
    for id in 1..=n {
        f.extend_from_slice(&[id, 0]);
    }
    f.extend_from_slice(&[0, 63, 0, 0, 0, 0xff, 0xd9]);
    f
}

#[test]
fn cmyk_and_ycck_are_refused_by_name() {
    assert!(refused(&components(4, None)).contains("CMYK"));
    assert!(refused(&components(4, Some(0))).contains("CMYK"));
    assert!(refused(&components(4, Some(2))).contains("YCCK"));
    let i = info(&components(4, Some(2))).unwrap();
    assert_eq!((i.width, i.height, i.sampling.len()), (8, 8, 4));
}

#[test]
fn bad_tables_and_scan_headers_are_refused() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let dht = file.windows(2).position(|w| w == [0xff, 0xc4]).unwrap();
    let mut f = file.clone();
    f[dht + 4] = 0x25;
    assert!(refused(&f).contains("Huffman table with class 2"));
    let dqt = file.windows(2).position(|w| w == [0xff, 0xdb]).unwrap();
    let mut f = file.clone();
    f[dqt + 4] = 0x24;
    assert!(refused(&f).contains("quantization table with precision 2"));
    assert_eq!(
        info(&f).unwrap().width,
        16,
        "probe reads the size past a bad table"
    );
    let sos = file.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    let mut f = file.clone();
    f[sos + 5] = 9;
    assert!(refused(&f).contains("lacks"));
    let mut f = file.clone();
    f[sos + 6] = 0x33;
    assert!(refused(&f).contains("missing"));
    let mut f = file.clone();
    f[sos + 4] = 0;
    refused(&f);
}

#[test]
fn corrupt_scan_data_never_panics() {
    let base = super::super::encode(&photo(48, 32), 85).unwrap();
    let r = Refs::open(refs::AREA);
    let files = [
        base,
        transcoded(&r, 85, 48, 32, "prog"),
        transcoded(&r, 85, 48, 32, "rst1b"),
    ];
    for file in files {
        let start = sof_end(&file);
        let mut s = 99u32;
        for i in 0..400 {
            let mut f = file.clone();
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let at = start + (s as usize >> 4) % (f.len() - start);
            f[at] ^= 1 << (i % 8);
            if let Ok(img) = decode(&f) {
                assert_eq!(img.data.len(), 48 * 32 * 4);
            }
        }
    }
}

#[test]
fn rgb_tagged_files_skip_the_colour_transform() {
    let planes = Planes {
        width: 1,
        height: 1,
        rgb: true,
        planes: (0..3)
            .map(|i| Decoded {
                width: 1,
                height: 1,
                fx: 1,
                fy: 1,
                stride: 8,
                data: vec![10 * (i + 1); 64],
            })
            .collect(),
    };
    assert_eq!(planes.rgba().unwrap().data, vec![10, 20, 30, 255]);
}

fn scan_data_start(file: &[u8]) -> usize {
    let at = file.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    at + 2 + usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]))
}

fn sof_at(file: &[u8]) -> usize {
    file.windows(2)
        .position(|w| w == [0xff, 0xc0] || w == [0xff, 0xc1] || w == [0xff, 0xc2])
        .unwrap()
}

fn grey_files(r: &Refs) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for (w, h) in SIZES {
        let g = refs::grey(w, h);
        out.push((format!("{w}x{h}"), r.bytes(&format!("{g}.jpg"))));
        for (v, _) in refs::GREY_VARIANTS {
            let file = r.bytes(&format!("{}.jpg", refs::variant(&g, v)));
            out.push((format!("{w}x{h} {v}"), file));
        }
    }
    out
}

#[test]
fn grey_files_decode_within_one_of_djpeg() {
    let r = Refs::open(refs::AREA);
    let mut worst = 0;
    for (w, h) in SIZES {
        let g = refs::grey(w, h);
        let want = ours(&r.bytes(&format!("{g}.jpg")));
        let d = max_diff(&want, &refs::ppm_pixels(&r.bytes(&format!("{g}.pgm"))));
        assert!(d <= 1, "grey {w}x{h}: off by {d}");
        worst = worst.max(d);
        for (v, _) in refs::GREY_VARIANTS {
            let file = r.bytes(&format!("{}.jpg", refs::variant(&g, v)));
            assert_eq!(
                ours(&file),
                want,
                "grey {w}x{h} {v}: not the baseline's samples"
            );
        }
    }
    for (label, file) in grey_files(&r) {
        let i = info(&file).unwrap();
        assert_eq!(i.sampling.len(), 1, "{label}");
        let rgba = decode(&file).unwrap();
        assert_eq!(
            (rgba.width, rgba.height),
            (i.width, i.height),
            "grey {label}"
        );
        assert!(
            rgba.data
                .chunks_exact(4)
                .all(|p| p[0] == p[1] && p[1] == p[2] && p[3] == 255),
            "grey {label}: not grey"
        );
    }
    eprintln!("grey: max difference {worst}");
}

#[test]
fn a_grey_frame_ignores_its_sampling_factors() {
    let r = Refs::open(refs::AREA);
    for (label, file) in grey_files(&r) {
        let at = sof_at(&file);
        assert_eq!(file[at + 9], 1, "{label}");
        let mut patched = file.clone();
        patched[at + 11] = 0x22;
        assert_eq!(info(&patched).unwrap().sampling, vec![(1, 1)], "{label}");
        assert_eq!(ours(&patched), ours(&file), "{label}");
        assert_eq!(decode(&patched).unwrap(), decode(&file).unwrap(), "{label}");
    }
}

fn rgb_diff(file: &[u8], djpeg: &[u8], label: &str) -> u8 {
    let got = rgb24(&decode(file).unwrap());
    let want = refs::ppm_pixels(djpeg);
    let d = max_diff(&got, &want);
    let p = psnr(&got, &want);
    assert!(d <= 3 && p > 45.0, "{label}: off by {d}, {p:.1} dB");
    d
}

#[test]
fn other_samplings_and_split_scans_decode_like_libjpeg() {
    let r = Refs::open(refs::AREA);
    let mut worst = (0, 0);
    for (w, h) in refs::CJPEG_SIZES {
        for (v, _, ratio) in refs::SAMPLINGS {
            let name = refs::cj(v, w, h);
            let file = r.bytes(&format!("{name}.jpg"));
            let planes = decode_planes(&file).unwrap();
            assert_eq!((planes.planes[1].fx, planes.planes[1].fy), ratio, "{name}");
            let d = max_diff(&ours(&file), &djpeg_planes(&r, &name, (w, h), ratio));
            assert!(d <= 1, "{name}: planes off by {d}");
            worst.0 = worst.0.max(d);
            let ppm = r.bytes(&format!("{name}.ppm"));
            worst.1 = worst.1.max(rgb_diff(&file, &ppm, &name));
        }
    }
    eprintln!(
        "cjpeg samplings: planes max {}, RGB against djpeg max {}",
        worst.0, worst.1
    );
}

#[test]
fn rgb_files_are_found_by_their_ids_or_adobe_marker() {
    let r = Refs::open(refs::AREA);
    let img = photo(33, 17);
    let file = r.bytes(&format!("{}.jpg", refs::RGB));
    let djpeg = r.bytes(&format!("{}.ppm", refs::RGB));
    let at = sof_at(&file);
    assert_eq!([file[at + 10], file[at + 13], file[at + 16]], *b"RGB");
    assert!(decode_planes(&file).unwrap().rgb);
    let adobe = file
        .windows(5)
        .position(|w| w == b"Adobe")
        .expect("an Adobe marker");
    assert_eq!(file[adobe + 11], 0);
    let mut ids_only = file.clone();
    ids_only[adobe] = b'a';
    let mut adobe_only = file.clone();
    adobe_only[at + 10] = 1;
    adobe_only[at + 13] = 2;
    adobe_only[at + 16] = 3;
    let sos = file.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    let ns = usize::from(file[sos + 4]);
    for k in 0..ns {
        let id = &mut adobe_only[sos + 5 + 2 * k];
        *id = match *id {
            b'R' => 1,
            b'G' => 2,
            b'B' => 3,
            other => other,
        };
    }
    let mut neither = adobe_only.clone();
    neither[adobe] = b'a';
    for (label, f) in [("both", &file), ("ids", &ids_only), ("adobe", &adobe_only)] {
        assert!(decode_planes(f).unwrap().rgb, "{label}");
        rgb_diff(f, &djpeg, label);
        let p = psnr(&rgb24(&img), &rgb24(&decode(f).unwrap()));
        assert!(p > 30.0, "{label}: {p:.1} dB from the source");
    }
    assert!(!decode_planes(&neither).unwrap().rgb);
    let mut transform_one = file.clone();
    transform_one[adobe + 11] = 1;
    assert!(
        !decode_planes(&transform_one).unwrap().rgb,
        "Adobe transform 1 is YCbCr"
    );
    let jfif = with_segment(&ids_only, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    assert!(!decode_planes(&jfif).unwrap().rgb, "a JFIF file is YCbCr");
    let jfif_adobe = with_segment(&file, 0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    assert!(
        !decode_planes(&jfif_adobe).unwrap().rgb,
        "JFIF wins over Adobe"
    );
}

#[test]
fn a_file_missing_only_its_end_marker_decodes() {
    let file = super::super::encode(&photo(33, 17), 90).unwrap();
    let want = decode(&file).unwrap();
    assert_eq!(decode(&file[..file.len() - 2]).unwrap(), want);
    assert_eq!(decode(&file[..file.len() - 1]).unwrap(), want);
    let r = Refs::open(refs::AREA);
    let want = decode(&transcoded(&r, 90, 33, 17, "base")).unwrap();
    let prog = transcoded(&r, 90, 33, 17, "prog");
    assert_eq!(decode(&prog[..prog.len() - 2]).unwrap(), want);
    let last = prog.windows(2).rposition(|w| w == [0xff, 0xda]).unwrap();
    let e = refused(&prog[..last]);
    assert!(e.contains("ends before its end-of-image"), "{e}");
}

#[test]
fn a_scan_cut_short_by_a_marker_is_refused() {
    let file = super::super::encode(&photo(33, 17), 90).unwrap();
    let [(start, end)] = entropy_ranges(&file)[..] else {
        panic!("one scan");
    };
    assert_eq!(end, file.len() - 2);
    for cut in start..end {
        let mut f = file[..cut].to_vec();
        f.extend_from_slice(&[0xff, 0xd9]);
        let e = refused(&f);
        assert!(e.contains("stops at marker 0xFFD9"), "cut at {cut}: {e}");
    }
    let r = Refs::open(refs::AREA);
    for v in ["prog", "rst1b"] {
        let file = transcoded(&r, 90, 33, 17, v);
        for (start, end) in entropy_ranges(&file) {
            for cut in start..end {
                let mut f = file[..cut].to_vec();
                f.extend_from_slice(&[0xff, 0xd9]);
                refused(&f);
            }
        }
    }
}

#[test]
fn too_many_scans_are_refused() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let sos = file.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    let scan = &file[sos..file.len() - 2];
    let mut f = file[..sos].to_vec();
    for _ in 0..MAX_SCANS {
        f.extend_from_slice(scan);
    }
    f.extend_from_slice(&[0xff, 0xd9]);
    assert_eq!(decode(&f).unwrap(), decode(&file).unwrap());
    let at = f.len() - 2;
    f.splice(at..at, scan.iter().copied());
    assert!(refused(&f).contains("more than 256 scans"));
}

#[test]
fn corrupt_headers_never_panic() {
    let base = super::super::encode(&photo(48, 32), 85).unwrap();
    let r = Refs::open(refs::AREA);
    let files = [base, transcoded(&r, 85, 48, 32, "prog")];
    for file in files {
        let end = scan_data_start(&file);
        let mut s = 7u32;
        for i in 0..2000 {
            let mut f = file.clone();
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let at = 2 + (s as usize >> 4) % (end - 2);
            f[at] ^= 1 << (i % 8);
            if let Ok(img) = decode(&f) {
                assert_eq!(img.data.len(), img.width as usize * img.height as usize * 4);
            }
            let _ = info(&f);
        }
    }
}

#[test]
fn upsampling_row_by_row_matches_every_factor() {
    let data: Vec<u8> = (0..4 * 8).map(|v| (v * 7) as u8).collect();
    let p = Plane {
        data: &data,
        stride: 8,
        width: 3,
        height: 4,
    };
    for (fx, fy) in [(1, 1), (2, 1), (1, 2), (2, 2), (4, 1), (1, 4), (3, 3)] {
        let (w, h) = (3 * fx - 1, 4 * fy - 1);
        let out = upsample(&p, fx, fy, w, h);
        assert_eq!(out.len(), w * h, "{fx}x{fy}");
        for y in 0..h {
            let row = &out[y * w..(y + 1) * w];
            if fx == 1 && fy == 1 {
                assert_eq!(row, &data[y * 8..y * 8 + w]);
            }
            if fy != 2 && fx != 2 {
                for (x, &v) in row.iter().enumerate() {
                    assert_eq!(v, data[(y / fy) * 8 + x / fx], "{fx}x{fy} at {x},{y}");
                }
            }
        }
    }
}

fn entropy_ranges(file: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 2;
    while at + 3 < file.len() && file[at + 1] != 0xd9 {
        let m = file[at + 1];
        let len = usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]));
        at += 2 + len;
        if m == 0xda {
            let start = at;
            while !(file[at] == 0xff && file[at + 1] != 0 && !(0xd0..=0xd7).contains(&file[at + 1]))
            {
                at += 1;
            }
            out.push((start, at));
        }
    }
    out
}

#[test]
fn upsampling_two_by_two_row_by_row_gives_the_fancy_filter() {
    let data = [0u8, 40, 80, 120];
    let p = Plane {
        data: &data,
        stride: 2,
        width: 2,
        height: 2,
    };
    let out = upsample(&p, 2, 2, 4, 4);
    assert_eq!(
        out,
        [
            0, 10, 30, 40, 20, 30, 50, 60, 60, 70, 90, 100, 80, 90, 110, 120
        ]
    );
    let odd = upsample(&p, 2, 2, 3, 3);
    assert_eq!(odd, [0, 10, 30, 20, 30, 50, 60, 70, 90]);
}

#[test]
fn a_table_redefined_between_progressive_scans_keeps_the_first() {
    let r = Refs::open(refs::AREA);
    let file = transcoded(&r, 90, 33, 17, "prog");
    let want = decode(&file).unwrap();
    let last = file.windows(2).rposition(|w| w == [0xff, 0xda]).unwrap();
    let mut redefined = file[..last].to_vec();
    for slot in 0..2u8 {
        redefined.extend_from_slice(&[0xff, 0xdb, 0, 67, slot]);
        redefined.extend_from_slice(&[255; 64]);
    }
    redefined.extend_from_slice(&file[last..]);
    assert_eq!(decode(&redefined).unwrap(), want);
}

fn upsample(src: &Plane, fx: usize, fy: usize, width: usize, height: usize) -> Vec<u8> {
    let mut out = vec![0u8; width * height];
    if width == 0 {
        return out;
    }
    let Ok(mut up) = sample::Upsampler::new(src, fx, width) else {
        return out;
    };
    for (y, dst) in out.chunks_exact_mut(width).enumerate() {
        let line = up.row(src, fx, fy, y);
        let n = width.min(line.len());
        dst[..n].copy_from_slice(&line[..n]);
    }
    out
}

#[test]
fn exif_without_a_usable_orientation_gives_none() {
    let le = |v: u16| v.to_le_bytes();
    assert_eq!(exif::orientation(&exif(b"II", 6)), Some(6));
    let mut order = exif(b"II", 6);
    order[6..8].copy_from_slice(b"IM");
    assert_eq!(exif::orientation(&order), None);
    let mut magic = exif(b"MM", 6);
    magic[8..10].copy_from_slice(&43u16.to_be_bytes());
    assert_eq!(exif::orientation(&magic), None);
    let mut long = exif(b"II", 6);
    long[30..32].copy_from_slice(&le(4));
    assert_eq!(exif::orientation(&long), None);
    let mut other = exif(b"II", 6);
    other[28..30].copy_from_slice(&le(0x0113));
    assert_eq!(exif::orientation(&other), None);
}

#[test]
fn crafted_headers_are_refused_by_name() {
    let file = super::super::encode(&photo(16, 8), 90).unwrap();
    let e = refused(&with_segment(&file, 0xdc, &[0, 8]));
    assert!(e.contains("(DNL) is not supported"), "{e}");
    let e = refused(&with_segment(&file, 0xc0, &[8, 0, 8]));
    assert!(e.contains("a short frame header"), "{e}");
    let sof = sof_at(&file);
    let mut huge = file.clone();
    huge[sof + 5..sof + 9].copy_from_slice(&[0xff; 4]);
    let e = refused(&huge);
    assert!(
        e.contains("65535x65535 is larger than 268435456 pixels"),
        "{e}"
    );
    let i = info(&huge).unwrap();
    assert_eq!((i.width, i.height, i.decodable), (65535, 65535, false));
    let e = refused(&components(2, None));
    assert!(e.contains("2 colour components are not supported"), "{e}");
    assert_eq!(info(&components(2, None)).unwrap().sampling.len(), 2);
    let sos = file.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
    assert_eq!(file[sos + 4], 3);
    let mut empty = file.clone();
    empty[sos + 2..sos + 4].copy_from_slice(&[0, 2]);
    let e = refused(&empty);
    assert!(e.contains("an empty scan header"), "{e}");
    assert_eq!(file[sof + 11], 0x22);
    let mut wide = file.clone();
    wide[sof + 11] = 0x44;
    let e = refused(&wide);
    assert!(e.contains("more than 10 blocks per MCU"), "{e}");
    let mut ten = file.clone();
    ten[sof + 11] = 0x42;
    if let Err(e) = decode(&ten) {
        assert!(!e.0.contains("blocks per MCU"), "{}", e.0);
    }
    for ahal in [0xe0u8, 0x0e] {
        let mut p = file.clone();
        p[sof + 1] = 0xc2;
        p[sos + 12] = 0;
        p[sos + 13] = ahal;
        let e = refused(&p);
        let want = format!("successive approximation {}/{}", ahal >> 4, ahal & 15);
        assert!(e.contains(&want), "{e}");
    }
}

#[test]
fn sixteen_bit_quantization_tables_decode_like_eight_bit_ones() {
    let file = super::super::encode(&photo(33, 17), 90).unwrap();
    let mut wide = file[..2].to_vec();
    let mut at = 2;
    let mut widened = 0;
    while file[at + 1] != 0xda {
        let len = usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]));
        let seg = &file[at..at + 2 + len];
        if seg[1] == 0xdb {
            assert_eq!((len - 2) % 65, 0);
            let mut body = Vec::new();
            for t in seg[4..].chunks_exact(65) {
                assert_eq!(t[0] >> 4, 0);
                body.push(0x10 | t[0]);
                for &v in &t[1..] {
                    body.extend_from_slice(&u16::from(v).to_be_bytes());
                }
                widened += 1;
            }
            wide.extend_from_slice(&[0xff, 0xdb]);
            wide.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
            wide.extend_from_slice(&body);
        } else {
            wide.extend_from_slice(seg);
        }
        at += 2 + len;
    }
    wide.extend_from_slice(&file[at..]);
    assert_eq!(widened, 2);
    assert_eq!(wide.len(), file.len() + 128);
    assert_eq!(decode(&wide).unwrap(), decode(&file).unwrap());
}

#[test]
fn a_huffman_table_of_more_than_256_codes_is_refused() {
    let mut lengths = [0u8; 17];
    lengths[16] = 255;
    lengths[15] = 2;
    let symbols = [1u8; 257];
    assert!(Table::new(&lengths, &symbols, false).is_err());
    assert!(Table::new(&lengths, &symbols[..256], false).is_err());
}

#[test]
fn internal_guards_refuse_what_the_decoder_never_builds() {
    assert!(zeroed(usize::MAX, 0u8).unwrap_err().0.contains("no memory"));
    assert!(zeroed(usize::MAX / 64, [0i16; 64]).is_err());
    assert_eq!(zeroed(3, 7u8).unwrap(), vec![7; 3]);
    assert!(
        coefs(&mut [], 0)
            .unwrap_err()
            .0
            .contains("a block outside the frame")
    );
    let mut two = [[0i16; 64]; 2];
    coefs(&mut two, 1).unwrap()[0] = 5;
    assert_eq!((two[0][0], two[1][0]), (0, 5));
    let plane = |width| Decoded {
        width,
        height: 1,
        fx: 1,
        fy: 1,
        stride: 8,
        data: vec![9; 8],
    };
    let empty = Planes {
        width: 0,
        height: 1,
        rgb: false,
        planes: vec![plane(0)],
    };
    let img = empty.rgba().unwrap();
    assert_eq!((img.width, img.height, img.data.len()), (0, 1, 0));
    let two = Planes {
        width: 1,
        height: 1,
        rgb: false,
        planes: vec![plane(1), plane(1)],
    };
    let e = two.rgba().unwrap_err().0;
    assert!(e.contains("2 colour components"), "{e}");
}

#[test]
fn an_rgb_tagged_copy_keeps_the_planes_and_skips_the_colour_transform() {
    let r = Refs::open(refs::AREA);
    for (pix_fmt, _, sampling) in FORMATS {
        let name = refs::base(pix_fmt, 33, 17);
        let file = r.bytes(&format!("{name}.jpg"));
        let tagged = refs::as_rgb(&file).unwrap();
        assert!(!decode_planes(&file).unwrap().rgb, "{name}");
        let planes = decode_planes(&tagged).unwrap();
        assert!(planes.rgb, "{name}: not RGB");
        assert_eq!(
            (planes.planes[1].fx, planes.planes[1].fy),
            sampling,
            "{name}"
        );
        assert_eq!(ours(&tagged), ours(&file), "{name}: planes changed");
        assert!(tagged.windows(4).all(|w| w != b"JFIF"), "{name}: JFIF kept");
        assert_eq!(tagged[2..2 + refs::ADOBE_RGB.len()], refs::ADOBE_RGB);
    }
    assert!(refs::as_rgb(b"GIF89a").is_err());
    assert!(refs::as_rgb(&[0xff, 0xd8, 0xff, 0xe0, 0, 40, 0]).is_err());
    assert!(refs::as_rgb(&[0xff, 0xd8, 1, 2]).is_err());
}

#[test]
fn planes_come_back_from_a_replicated_picture() {
    let (w, h) = (5u32, 3u32);
    let mut pixels = Vec::new();
    for y in 0..h as u8 {
        for x in 0..w as u8 {
            pixels.extend([10 * y + x, 100 + (x / 2) + 10 * y, 200 + (x / 2)]);
        }
    }
    let got = refs::planes_of(&pixels, (w, h), (2, 2));
    let luma: Vec<u8> = (0..3u8)
        .flat_map(|y| (0..5u8).map(move |x| 10 * y + x))
        .collect();
    assert_eq!(&got[..15], &luma[..]);
    assert_eq!(&got[15..21], &[100, 101, 102, 120, 121, 122]);
    assert_eq!(&got[21..], &[200, 201, 202, 200, 201, 202]);
    assert_eq!(refs::planes_of(&pixels, (w, h), (1, 1)).len(), 45);
    assert!(refs::planes_of(&[], (w, h), (2, 2)).is_empty());
}
