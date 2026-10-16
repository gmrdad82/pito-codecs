use ::png as reference;

fn encode(
    color: reference::ColorType,
    depth: reference::BitDepth,
    (w, h): (u32, u32),
    data: &[u8],
    extra: &dyn Fn(&mut reference::Encoder<&mut Vec<u8>>),
) -> Vec<u8> {
    let mut file = Vec::new();
    {
        let mut enc = reference::Encoder::new(&mut file, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        enc.set_adaptive_filter(reference::AdaptiveFilterType::Adaptive);
        enc.set_compression(reference::Compression::Best);
        extra(&mut enc);
        let mut writer = enc.write_header().expect("header");
        writer.write_image_data(data).expect("image data");
    }
    file
}

fn expected(file: &[u8]) -> Vec<u8> {
    let mut dec = reference::Decoder::new(file);
    dec.set_transformations(
        reference::Transformations::EXPAND
            | reference::Transformations::STRIP_16
            | reference::Transformations::ALPHA,
    );
    let mut reader = dec.read_info().expect("info");
    let mut buf = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).expect("frame");
    let buf = &buf[..frame.buffer_size()];
    match frame.color_type {
        reference::ColorType::Rgba => buf.to_vec(),
        reference::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|g| [g[0], g[0], g[0], g[1]])
            .collect(),
        other => panic!("unexpected reference output {other:?}"),
    }
}

fn pattern(len: usize, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..len)
        .map(|i| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if i % 7 < 4 {
                (i / 5) as u8
            } else {
                (s >> 24) as u8
            }
        })
        .collect()
}

#[test]
fn decodes_like_the_png_crate() {
    use reference::{BitDepth as D, ColorType as C};
    let (w, h) = (97u32, 61u32);
    let n = (w * h) as usize;
    let none = |_: &mut reference::Encoder<&mut Vec<u8>>| {};
    let palette: Vec<u8> = pattern(256 * 3, 9);
    let alphas: Vec<u8> = pattern(200, 11);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "rgba8",
            encode(C::Rgba, D::Eight, (w, h), &pattern(n * 4, 1), &none),
        ),
        (
            "rgb8",
            encode(C::Rgb, D::Eight, (w, h), &pattern(n * 3, 2), &none),
        ),
        (
            "rgb16",
            encode(C::Rgb, D::Sixteen, (w, h), &pattern(n * 6, 3), &none),
        ),
        (
            "greya16",
            encode(
                C::GrayscaleAlpha,
                D::Sixteen,
                (w, h),
                &pattern(n * 4, 4),
                &none,
            ),
        ),
        (
            "grey4",
            encode(
                C::Grayscale,
                D::Four,
                (w, h),
                &pattern((w as usize).div_ceil(2) * h as usize, 5),
                &none,
            ),
        ),
        (
            "grey8 trns",
            encode(C::Grayscale, D::Eight, (w, h), &pattern(n, 6), &|e| {
                e.set_trns(vec![0u8, 3])
            }),
        ),
        (
            "palette8 trns",
            encode(C::Indexed, D::Eight, (w, h), &pattern(n, 7), &|e| {
                e.set_palette(palette.clone());
                e.set_trns(alphas.clone());
            }),
        ),
        (
            "palette2",
            encode(
                C::Indexed,
                D::Two,
                (w, h),
                &pattern((w as usize).div_ceil(4) * h as usize, 8),
                &|e| e.set_palette(palette[..12].to_vec()),
            ),
        ),
    ];
    for (name, file) in cases {
        let ours = super::decode(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((ours.width, ours.height), (w, h), "{name}");
        assert!(ours.data == expected(&file), "{name}: pixels differ");
    }
}

#[test]
fn malformed_files_are_errors() {
    let (w, h) = (40u32, 30u32);
    let file = encode(
        reference::ColorType::Rgb,
        reference::BitDepth::Eight,
        (w, h),
        &pattern((w * h * 3) as usize, 12),
        &|_| {},
    );
    assert!(super::decode(&file).is_ok());
    for cut in [0, 7, 20, 33, 40, file.len() / 2, file.len() - 20] {
        assert!(super::decode(&file[..cut]).is_err(), "cut at {cut}");
    }
    let idat = file.windows(4).position(|w| w == b"IDAT").expect("IDAT") + 4;
    let mut s = 1u32;
    for i in 0..400 {
        let mut bad = file.clone();
        s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let at = idat + (s as usize >> 8) % (file.len() - idat - 16);
        bad[at] ^= 1 << (i % 8);
        let _ = super::decode(&bad);
    }
    assert!(super::decode(b"\x89PNG\r\n\x1a\nnonsense").is_err());
}

#[test]
fn writes_what_the_png_crate_reads() {
    for (w, h, seed) in [(1, 1, 1), (7, 5, 2), (640, 360, 3), (33, 300, 4)] {
        let mut image = crate::media::Rgba::new(w, h);
        let noise = pattern(image.data.len(), seed);
        for (i, (px, n)) in image
            .data
            .chunks_exact_mut(4)
            .zip(noise.chunks_exact(4))
            .enumerate()
        {
            let flat = (i / 97) % 3 == 0;
            px[0] = if flat { 0 } else { n[0] };
            px[1] = if flat { 0 } else { n[1] };
            px[2] = if flat { 200 } else { n[2] };
            px[3] = 255;
        }
        let file = super::encode(&image).expect("encode");
        assert_eq!(expected(&file), image.data, "{w}x{h}");
        assert_eq!(super::decode(&file).expect("decode").data, image.data);
    }
}
