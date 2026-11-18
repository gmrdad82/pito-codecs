use super::{
    Color, Compression, Decoder, Options, Picture, Transform, encode_picture, metadata, output,
};
use crate::testref::Refs;
use ::png18 as outside;

use super::edges::refs;

fn ours_color(c: outside::ColorType) -> Color {
    match c {
        outside::ColorType::Grayscale => Color::Grey,
        outside::ColorType::Rgb => Color::Rgb,
        outside::ColorType::Indexed => Color::Palette,
        outside::ColorType::GrayscaleAlpha => Color::GreyAlpha,
        outside::ColorType::Rgba => Color::Rgba,
    }
}

fn outside_decode(file: &[u8], t: outside::Transformations) -> (Color, u8, u32, u32, Vec<u8>) {
    let mut d = outside::Decoder::new(std::io::Cursor::new(file));
    d.set_transformations(t);
    let mut r = d.read_info().expect("info");
    let mut buf = vec![0; r.output_buffer_size().expect("size")];
    let f = r.next_frame(&mut buf).expect("frame");
    buf.truncate(f.buffer_size());
    (
        ours_color(f.color_type),
        f.bit_depth as u8,
        f.width,
        f.height,
        buf,
    )
}

fn pairs() -> [(Transform, outside::Transformations); 3] {
    [
        (Transform::Expand, outside::Transformations::EXPAND),
        (
            Transform::Normalize8,
            outside::Transformations::normalize_to_color8(),
        ),
        (Transform::Identity, outside::Transformations::IDENTITY),
    ]
}

#[test]
fn every_claimed_and_keyed_case_reads_like_the_png_crate_in_each_transform() {
    let mut dec = Decoder::new();
    let mut checked = 0;
    for case in refs::cases() {
        let interlaced = case.name.ends_with("i1");
        for (mine, theirs) in pairs() {
            if mine == Transform::Identity && interlaced && case.depth < 8 {
                assert!(dec.decode_picture(&case.file, mine).is_err());
                continue;
            }
            let (color, depth, w, h, bytes) = outside_decode(&case.file, theirs);
            let pic = dec
                .decode_picture(&case.file, mine)
                .unwrap_or_else(|e| panic!("{} {mine:?}: {e}", case.name));
            let tag = format!("{} {mine:?}", case.name);
            assert_eq!(
                (pic.color, pic.depth, pic.width, pic.height),
                (color, depth, w, h),
                "{tag}"
            );
            assert!(pic.data == bytes, "{tag}: samples differ");
            let out = output(&case.file, mine).unwrap();
            assert_eq!(
                (out.bytes, out.row_bytes * h as usize),
                (bytes.len(), bytes.len()),
                "{tag}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 44 * 3 - 9);
}

#[test]
fn sixteen_bit_samples_match_the_references_bit_for_bit() {
    let r = Refs::open(refs::AREA);
    let mut dec = Decoder::new();
    let mut seen = 0;
    for case in refs::cases().iter().filter(|c| c.wide()) {
        let pic = dec.decode_picture(&case.file, Transform::Expand).unwrap();
        assert_eq!(pic.depth, 16);
        let wide = r.bytes(&format!("{}.rgba64", case.name));
        let want: Vec<u16> = wide
            .chunks_exact(2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]))
            .collect();
        let rgba16 = rgba16(&pic);
        assert_eq!(rgba16.len(), want.len(), "{}", case.name);
        let exact = rgba16.iter().zip(&want).filter(|(a, b)| a == b).count();
        let off = rgba16
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            off <= 257 && exact * 10 >= want.len() * 9,
            "{}: max {off}",
            case.name
        );
        seen += 1;
    }
    assert_eq!(seen, 8 + 4);
}

fn rgba16(p: &Picture) -> Vec<u16> {
    let s = p.samples16();
    let n = p.color.samples();
    s.chunks_exact(n)
        .flat_map(|px| match p.color {
            Color::Grey => [px[0], px[0], px[0], 65535],
            Color::GreyAlpha => [px[0], px[0], px[0], px[1]],
            Color::Rgb => [px[0], px[1], px[2], 65535],
            _ => [px[0], px[1], px[2], px[3]],
        })
        .collect()
}

#[test]
fn reading_into_a_callers_buffer_fills_exactly_the_picture() {
    let mut dec = Decoder::new();
    for case in refs::cases().iter().filter(|c| c.name.ends_with("i0")) {
        let want = dec.decode_picture(&case.file, Transform::Expand).unwrap();
        let mut buf = vec![0xAAu8; want.data.len() + 17];
        let out = dec
            .decode_slice(&case.file, Transform::Expand, &mut buf)
            .unwrap();
        assert_eq!(out.bytes, want.data.len(), "{}", case.name);
        assert!(buf[..out.bytes] == want.data[..], "{}", case.name);
        assert!(buf[out.bytes..].iter().all(|&b| b == 0xAA), "{}", case.name);
        let mut small = vec![0u8; want.data.len() - 1];
        assert!(
            dec.decode_slice(&case.file, Transform::Expand, &mut small)
                .is_err()
        );
    }
}

fn grid(color: Color, depth: u8, w: u32, h: u32, seed: u32) -> Picture {
    let mut p = Picture::new(w, h, color, depth);
    let mut s = seed;
    for (i, b) in p.data.iter_mut().enumerate() {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *b = if i % 11 < 5 {
            (i / 9) as u8
        } else {
            (s >> 24) as u8
        };
    }
    p
}

#[test]
fn every_colour_type_and_depth_is_written_as_itself_and_read_by_the_png_crate() {
    let mut dec = Decoder::new();
    for color in [Color::Grey, Color::GreyAlpha, Color::Rgb, Color::Rgba] {
        for depth in [8u8, 16] {
            for level in [Compression::Fast, Compression::Balanced, Compression::High] {
                for (w, h) in [(1, 1), (3, 2), (37, 29), (300, 40)] {
                    let pic = grid(color, depth, w, h, u32::from(depth) + w);
                    let opts = Options {
                        compression: level,
                        ..Options::default()
                    };
                    let file = encode_picture(&pic, &opts).unwrap();
                    let tag = format!("{color:?} {depth} {level:?} {w}x{h}");
                    assert_eq!(file[24], depth, "{tag}");
                    let (c, d, ow, oh, bytes) =
                        outside_decode(&file, outside::Transformations::IDENTITY);
                    assert_eq!((c, d, ow, oh), (color, depth, w, h), "{tag}");
                    assert!(
                        bytes == pic.data,
                        "{tag}: the png crate reads other samples"
                    );
                    assert!(
                        dec.decode_picture(&file, Transform::Identity).unwrap() == pic,
                        "{tag}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_balanced_and_high_levels_beat_the_fast_one_on_flat_art() {
    let mut p = Picture::new(256, 64, Color::Rgba, 8);
    let row = grid(Color::Rgba, 8, 256, 1, 9).data;
    for line in p.data.chunks_exact_mut(1024) {
        line.copy_from_slice(&row);
    }
    let size = |level| {
        encode_picture(
            &p,
            &Options {
                compression: level,
                ..Options::default()
            },
        )
        .unwrap()
        .len()
    };
    let (fast, balanced, high) = (
        size(Compression::Fast),
        size(Compression::Balanced),
        size(Compression::High),
    );
    assert!(
        balanced < fast && high <= balanced,
        "{fast} {balanced} {high}"
    );
}

#[test]
fn the_colour_chunks_and_text_round_trip_with_the_png_crate() {
    let pic = grid(Color::Rgb, 16, 5, 4, 3);
    let opts = Options {
        compression: Compression::Fast,
        srgb: Some(0),
        gamma: Some(45455),
        chromaticities: Some([31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000]),
        pixel_dims: Some((2835, 2835, 1)),
        text: vec![
            ("Title".into(), "plain".into()),
            ("Naïve".into(), "x".into()),
        ],
    };
    assert!(encode_picture(&pic, &opts).is_err());
    let opts = Options {
        text: vec![
            ("Title".into(), "plain".into()),
            ("Note".into(), "caf\u{e9}".into()),
            ("Wide".into(), "snow \u{2603}".into()),
        ],
        ..opts
    };
    let file = encode_picture(&pic, &opts).unwrap();
    let theirs = outside::Decoder::new(std::io::Cursor::new(&file[..]))
        .read_info()
        .unwrap();
    let info = theirs.info();
    assert!(info.srgb.is_some());
    assert_eq!(info.gama_chunk.map(|g| g.into_scaled()), Some(45455));
    assert_eq!(
        info.chrm_chunk.map(|c| c.white.0.into_scaled()),
        Some(31270)
    );
    assert_eq!(info.pixel_dims.map(|p| p.xppu), Some(2835));
    assert_eq!(info.uncompressed_latin1_text.len(), 2);
    assert_eq!(info.utf8_text.len(), 1);
    let meta = metadata(&file).unwrap();
    assert_eq!(meta.srgb, Some(0));
    assert_eq!(meta.gamma, Some(45455));
    assert_eq!(meta.chromaticities.map(|c| c[0]), Some(31270));
    assert_eq!(meta.pixel_dims, Some((2835, 2835, 1)));
    let got: Vec<(&str, &str)> = meta
        .text
        .iter()
        .map(|t| (t.keyword.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("Title", "plain"),
            ("Note", "caf\u{e9}"),
            ("Wide", "snow \u{2603}")
        ]
    );
}

#[test]
fn text_and_colour_chunks_the_png_crate_wrote_are_read() {
    let mut file = Vec::new();
    {
        let mut e = outside::Encoder::new(&mut file, 3, 2);
        e.set_color(outside::ColorType::Rgba);
        e.set_depth(outside::BitDepth::Eight);
        e.set_source_srgb(outside::SrgbRenderingIntent::Perceptual);
        e.set_source_gamma(outside::ScaledFloat::from_scaled(45455));
        e.add_text_chunk("Author".into(), "Catalin".into()).unwrap();
        e.add_ztxt_chunk("Comment".into(), "squeezed ".repeat(40))
            .unwrap();
        e.add_itxt_chunk("Title".into(), "d\u{e9}j\u{e0} vu \u{2603}".into())
            .unwrap();
        let mut w = e.write_header().unwrap();
        w.write_image_data(&[7u8; 24]).unwrap();
    }
    let meta = metadata(&file).unwrap();
    assert_eq!(meta.srgb, Some(0));
    assert_eq!(meta.gamma, Some(45455));
    let find = |k: &str| {
        meta.text
            .iter()
            .find(|t| t.keyword == k)
            .unwrap()
            .text
            .clone()
    };
    assert_eq!(find("Author"), "Catalin");
    assert_eq!(find("Comment"), "squeezed ".repeat(40));
    assert_eq!(find("Title"), "d\u{e9}j\u{e0} vu \u{2603}");
}

#[test]
fn hostile_files_never_panic_in_the_new_entry_points() {
    let pic = grid(Color::GreyAlpha, 16, 9, 7, 5);
    let good = encode_picture(&pic, &Options::default()).unwrap();
    let mut dec = Decoder::new();
    let mut s = 1u32;
    for round in 0..3000 {
        let mut bad = good.clone();
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let at = 8 + (s as usize >> 8) % (good.len() - 8);
        bad[at] ^= 1 << (round % 8);
        if round % 3 == 0 {
            bad.truncate(at);
        }
        for t in [
            Transform::Identity,
            Transform::Expand,
            Transform::Normalize8,
        ] {
            let _ = dec.decode_picture(&bad, t);
            let _ = output(&bad, t);
            let mut buf = [0u8; 200];
            let _ = dec.decode_slice(&bad, t, &mut buf);
        }
        let _ = metadata(&bad);
    }
}

fn flat_art(w: u32, h: u32, depth: u8, color: Color) -> Picture {
    let mut p = Picture::new(w, h, color, depth);
    let n = color.samples();
    let step = usize::from(depth / 8);
    let mut s = 99u32;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let panel = (x / 37 + y / 23) % 5;
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            for c in 0..n {
                let v = if panel == 4 {
                    ((x * 7 + y * 3 + c * 11) as u32 + (s >> 29)) as u16
                } else {
                    (panel * 40 + c * 13) as u16
                };
                let at = ((y * w as usize + x) * n + c) * step;
                if step == 2 {
                    p.data[at..at + 2].copy_from_slice(&(v << 8 | (v & 0x7f)).to_be_bytes());
                } else {
                    p.data[at] = v as u8;
                }
            }
        }
    }
    p
}

fn encoded(p: &Picture, level: Compression, plan: super::encode::Plan) -> Vec<u8> {
    let opts = Options {
        compression: level,
        ..Options::default()
    };
    super::encode::encode_with(p, &opts, plan).unwrap()
}

#[test]
fn files_cut_into_segments_read_back_the_same_at_every_level_and_cut() {
    let mut dec = Decoder::new();
    for (color, depth) in [
        (Color::Rgba, 16),
        (Color::Rgb, 8),
        (Color::Grey, 16),
        (Color::GreyAlpha, 8),
    ] {
        let pic = flat_art(97, 61, depth, color);
        for level in [Compression::Fast, Compression::Balanced, Compression::High] {
            for segment in [1usize, 300, 2_000, 9_000, 1 << 30] {
                let plan = super::encode::Plan {
                    segment,
                    threads: 0,
                };
                let file = encoded(&pic, level, plan);
                let tag = format!("{color:?} {depth} {level:?} {segment}");
                let (c, d, w, h, bytes) = outside_decode(&file, outside::Transformations::IDENTITY);
                assert_eq!((c, d, w, h), (color, depth, 97, 61), "{tag}");
                assert!(
                    bytes == pic.data,
                    "{tag}: the png crate reads other samples"
                );
                assert!(
                    dec.decode_picture(&file, Transform::Identity).unwrap() == pic,
                    "{tag}"
                );
            }
        }
    }
}

#[test]
fn the_thread_count_never_changes_a_byte() {
    let pic = flat_art(120, 90, 16, Color::Rgba);
    for level in [Compression::Fast, Compression::Balanced, Compression::High] {
        let one = encoded(
            &pic,
            level,
            super::encode::Plan {
                segment: 4_000,
                threads: 1,
            },
        );
        for threads in [0usize, 2, 7] {
            let many = encoded(
                &pic,
                level,
                super::encode::Plan {
                    segment: 4_000,
                    threads,
                },
            );
            assert!(one == many, "{level:?} {threads}");
        }
    }
}

#[test]
fn the_balanced_level_is_no_larger_than_the_png_crates_default() {
    for (color, depth) in [(Color::Rgba, 16), (Color::Rgba, 8), (Color::Rgb, 8)] {
        let pic = flat_art(400, 240, depth, color);
        let mine = encode_picture(
            &pic,
            &Options {
                compression: Compression::Balanced,
                ..Options::default()
            },
        )
        .unwrap();
        let mut theirs = Vec::new();
        {
            let mut e = outside::Encoder::new(&mut theirs, 400, 240);
            e.set_color(match color {
                Color::Rgba => outside::ColorType::Rgba,
                _ => outside::ColorType::Rgb,
            });
            e.set_depth(if depth == 16 {
                outside::BitDepth::Sixteen
            } else {
                outside::BitDepth::Eight
            });
            let mut w = e.write_header().unwrap();
            w.write_image_data(&pic.data).unwrap();
        }
        assert!(
            mine.len() <= theirs.len(),
            "{color:?} {depth}: {} against {}",
            mine.len(),
            theirs.len()
        );
    }
}

#[test]
fn reading_through_the_pipeline_gives_the_samples_and_the_errors_of_reading_in_line() {
    let pic = flat_art(150, 80, 16, Color::Rgba);
    let big = flat_art(2000, 300, 8, Color::Rgba);
    for (pic, level) in [
        (&pic, Compression::Fast),
        (&pic, Compression::Balanced),
        (&big, Compression::High),
    ] {
        let plan = super::encode::Plan {
            segment: 20_000,
            threads: 0,
        };
        let file = encoded(pic, level, plan);
        let mut line = Decoder::new();
        line.pipeline_min = usize::MAX;
        let mut piped = Decoder::new();
        piped.pipeline_min = 0;
        let a = line.decode_picture(&file, Transform::Identity).unwrap();
        let b = piped.decode_picture(&file, Transform::Identity).unwrap();
        assert!(a == *pic && b == *pic, "{level:?}");
        let mut rgba_a = crate::media::Rgba::new(0, 0);
        let mut rgba_b = crate::media::Rgba::new(0, 0);
        if pic.depth == 8 {
            line.decode_into(&file, &mut rgba_a).unwrap();
            piped.decode_into(&file, &mut rgba_b).unwrap();
            assert!(rgba_a.data == rgba_b.data && rgba_a.data == pic.data);
        }
        let z_len = idat_of(&file).len();
        for cut in [z_len / 10, z_len / 2, z_len - 9] {
            let short = with_idat(&file, |z| z.truncate(cut));
            let ea = line.decode_picture(&short, Transform::Identity);
            let eb = piped.decode_picture(&short, Transform::Identity);
            assert!(ea.is_err() && eb.is_err(), "{level:?} cut at {cut}");
        }
        for at in [z_len / 3, z_len / 2, z_len - 6] {
            let bad = with_idat(&file, |z| z[at] ^= 0x5a);
            let ea = line.decode_picture(&bad, Transform::Identity);
            let eb = piped.decode_picture(&bad, Transform::Identity);
            assert!(ea.is_err() && eb.is_err(), "{level:?} flip at {at}");
        }
    }
}

fn idat_of(file: &[u8]) -> Vec<u8> {
    let mut at = 8;
    let mut z = Vec::new();
    while at + 12 <= file.len() {
        let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        if &file[at + 4..at + 8] == b"IDAT" {
            z.extend_from_slice(&file[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    z
}

fn with_idat(file: &[u8], edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut z = idat_of(file);
    edit(&mut z);
    let mut out = file[..8].to_vec();
    let mut at = 8;
    let mut wrote = false;
    while at + 12 <= file.len() {
        let len = u32::from_be_bytes(file[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = file[at + 4..at + 8].try_into().unwrap();
        if &kind == b"IDAT" {
            if !wrote {
                out.extend(refs::chunk(b"IDAT", &z));
                wrote = true;
            }
        } else {
            out.extend_from_slice(&file[at..at + 12 + len]);
        }
        at += 12 + len;
    }
    out
}

#[test]
fn a_filter_byte_that_is_not_a_filter_stops_both_ways_of_reading() {
    let raw: Vec<u8> = (0..30u32)
        .flat_map(|y| {
            let filter = if y == 17 { 9 } else { 0 };
            std::iter::once(filter).chain((0..40u32).map(move |x| (x + y) as u8))
        })
        .collect();
    let mut z = refs::stored(&raw);
    let mut file = super::SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&40u32.to_be_bytes());
    ihdr.extend_from_slice(&30u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    file.extend(refs::chunk(b"IHDR", &ihdr));
    file.extend(refs::chunk(b"IDAT", &z));
    file.extend(refs::chunk(b"IEND", &[]));
    z.clear();
    for min in [0usize, usize::MAX] {
        let mut dec = Decoder::new();
        dec.pipeline_min = min;
        let e = dec.decode_picture(&file, Transform::Identity).unwrap_err();
        assert!(e.to_string().contains("filter type 9"), "{e}");
    }
}
