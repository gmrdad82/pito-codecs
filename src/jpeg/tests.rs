use super::*;
use crate::testref::Refs;

#[allow(dead_code)]
#[path = "refs.rs"]
pub(crate) mod refs;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pattern {
    Gradient,
    Noise,
    Flat,
}

const PATTERNS: [Pattern; 3] = [Pattern::Gradient, Pattern::Noise, Pattern::Flat];
const SIZES: [(u32, u32); 5] = [(1, 1), (7, 5), (16, 16), (33, 17), (1920, 1080)];
const QUALITIES: [u8; 5] = [1, 50, 75, 90, 100];

fn tri(t: u32) -> u8 {
    let t = t % 510;
    if t < 255 { t as u8 } else { (509 - t) as u8 }
}

fn image(pattern: Pattern, width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    let mut s = 0x1234_5678u32 ^ width.wrapping_mul(31) ^ height;
    for y in 0..height {
        for x in 0..width {
            let at = ((y * width + x) * 4) as usize;
            let px = match pattern {
                Pattern::Gradient => [tri(x + 2 * y), tri(2 * x + y + 90), tri(x + y + 200), 255],
                Pattern::Flat => [200, 120, 40, 255],
                Pattern::Noise => {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let [a, b, c, d] = s.to_le_bytes();
                    [a ^ d, b, c, 255]
                }
            };
            img.data[at..at + 4].copy_from_slice(&px);
        }
    }
    img
}

fn luma(img: &Rgba) -> Vec<u8> {
    img.data
        .chunks_exact(4)
        .map(|p| color::rgb_to_ycbcr(p[0], p[1], p[2]).0)
        .collect()
}

fn chroma(img: &Rgba) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (img.width as usize, img.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let at = |x: usize, y: usize| {
        let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        let p = &img.data[i..i + 3];
        let (_, cb, cr) = color::rgb_to_ycbcr(p[0], p[1], p[2]);
        (u32::from(cb), u32::from(cr))
    };
    let (mut cbs, mut crs) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
    for y in 0..ch {
        for x in 0..cw {
            let (mut cb, mut cr) = (0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (b, r) = at(2 * x + dx, 2 * y + dy);
                cb += b;
                cr += r;
            }
            cbs.push(((cb + 2) / 4) as u8);
            crs.push(((cr + 2) / 4) as u8);
        }
    }
    (cbs, crs)
}

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

fn planes(file: &[u8]) -> Option<Vec<u8>> {
    let p = decode::decode_planes(file).ok()?;
    Some(p.planes.iter().flat_map(|p| p.cropped()).collect())
}

fn scalar(img: &Rgba, quality: u8) -> Vec<u8> {
    encode_with(img, quality, Scalar).expect("scalar encode")
}

#[cfg(target_arch = "x86_64")]
fn avx2() -> Option<avx2::Avx2> {
    let found = avx2::Avx2::detect();
    if found.is_none() {
        eprintln!("skipped: this CPU has no AVX2");
    }
    found
}

fn scan_data(file: &[u8]) -> &[u8] {
    &file[walk(file).1..file.len() - 2]
}

fn segments(file: &[u8]) -> Vec<(u8, &[u8])> {
    walk(file).0
}

fn walk(file: &[u8]) -> (Vec<(u8, &[u8])>, usize) {
    assert_eq!(&file[..2], &[0xFF, 0xD8], "starts with SOI");
    let mut at = 2;
    let mut out = Vec::new();
    loop {
        assert_eq!(file[at], 0xFF, "a marker at {at}");
        let marker = file[at + 1];
        let len = usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]));
        out.push((marker, &file[at + 4..at + 2 + len]));
        at += 2 + len;
        if marker == 0xDA {
            return (out, at);
        }
    }
}

#[test]
fn quality_scaling_follows_the_ijg_formula() {
    assert_eq!(quant::scaled(&quant::LUMA, 50), quant::LUMA.map(|v| v << 3));
    assert_eq!(
        quant::scaled(&quant::CHROMA, 50),
        quant::CHROMA.map(|v| v << 3)
    );
    assert!(quant::scaled(&quant::LUMA, 100).iter().all(|&v| v == 8));
    assert!(
        quant::scaled(&quant::CHROMA, 1)
            .iter()
            .all(|&v| v == 255 << 3)
    );
    assert_eq!(quant::scaled(&quant::LUMA, 75)[0], 8 << 3);
    assert_eq!(quant::scaled(&quant::LUMA, 25)[0], 32 << 3);
    assert_eq!(quant::scaled(&quant::LUMA, 90)[0], 3 << 3);
    assert_eq!(quant::scaled(&quant::LUMA, 10)[63], 255 << 3);
}

#[test]
fn quantizing_at_quality_100_divides_by_eight_exactly() {
    let q = QuantTable::new(&quant::LUMA, 100);
    for i in -255i16..255 {
        assert_eq!(i, q.quantize(i << 3, 0));
    }
}

#[test]
fn every_divisor_is_at_least_eight_and_the_reciprocal_is_total() {
    for q in 1..=100u8 {
        for base in [&quant::LUMA, &quant::CHROMA] {
            assert!(quant::scaled(base, q).iter().all(|&d| d >= 8), "q{q}");
        }
    }
    let (r, c) = quant::compute_reciprocal(1);
    for v in [0i32, 1, 2, 255, 1024, 32767] {
        assert_eq!(((v + c) * r) >> 15, v);
    }
    assert_eq!(quant::compute_reciprocal(0), quant::compute_reciprocal(1));
    assert_eq!(quant::compute_reciprocal(8), (4096, 4));
}

#[test]
fn quantizing_rounds_half_away_from_zero() {
    let q = QuantTable::new(&quant::LUMA, 50);
    let d = 16 * 8;
    assert_eq!(q.quantize(d / 2 - 1, 0), 0);
    assert_eq!(q.quantize(d / 2, 0), 1);
    assert_eq!(q.quantize(-(d / 2), 0), -1);
    assert_eq!(q.quantize(-(d / 2 - 1), 0), 0);
    assert_eq!(q.quantize(3 * d, 0), 3);
    assert_eq!(q.quantize(-3 * d, 0), -3);
}

const FDCT_IN1: [i16; 64] = [
    -70, -71, -70, -68, -67, -67, -67, -67, -72, -73, -72, -70, -69, -69, -68, -69, -75, -76, -74,
    -73, -73, -72, -71, -70, -77, -78, -77, -75, -76, -75, -73, -71, -78, -77, -77, -76, -79, -77,
    -76, -75, -78, -78, -77, -77, -77, -77, -78, -77, -79, -79, -78, -78, -78, -78, -79, -78, -80,
    -79, -78, -78, -81, -80, -78, -76,
];

const FDCT_OUT1: [i16; 64] = [
    -4786, -66, 2, -18, 12, 12, 5, -7, 223, -37, -8, 21, 8, 5, -4, 6, 60, 6, -10, 5, 0, -2, -1, 5,
    21, 21, -15, 12, -2, -7, 1, 0, -2, -5, 16, -15, 0, 5, -4, -8, 0, -7, -4, 6, 7, -4, 5, 4, 3, 0,
    1, -5, 0, -1, 4, 1, -5, 7, 0, -3, -6, 1, 1, -4,
];

const FDCT_IN2: [i16; 64] = [
    21, 28, 11, 24, -45, -37, -55, -103, 38, -8, 31, 17, -19, 49, 15, -76, 22, -48, -36, -31, -23,
    35, -23, -72, 13, -30, -45, -42, -44, -15, -20, -44, 13, -30, -45, -42, -44, -15, -20, -44, 13,
    -30, -45, -42, -44, -15, -20, -44, 13, -30, -45, -42, -44, -15, -20, -44, 13, -30, -45, -42,
    -44, -15, -20, -44,
];

const FDCT_OUT2: [i16; 64] = [
    -1420, 717, 187, 910, -244, 579, 222, -191, 461, 487, -497, -29, -220, 179, 63, -95, 213, 414,
    -235, -187, -108, 74, -73, -70, -63, 311, 13, -290, 17, -38, -180, -47, -254, 201, 116, -247,
    102, -109, -185, -36, -310, 107, 73, -91, 126, -121, -99, -37, -253, 43, -15, 53, 101, -91, -3,
    -37, -136, 12, -44, 81, 53, -45, 31, -24,
];

#[test]
fn fdct_matches_libjpeg_islow() {
    for (input, want) in [(FDCT_IN1, FDCT_OUT1), (FDCT_IN2, FDCT_OUT2)] {
        let mut b = Block(input);
        dct::fdct(&mut b);
        assert_eq!(b.0, want);
    }
}

#[test]
fn fdct_of_a_flat_block_is_dc_only() {
    for v in [-128i16, -1, 0, 1, 127] {
        let mut b = Block([v; 64]);
        dct::fdct(&mut b);
        assert_eq!(b.0[0], v * 64, "dc of {v}");
        assert!(b.0[1..].iter().all(|&c| c == 0), "ac of {v}");
    }
}

#[cfg(target_arch = "x86_64")]
fn test_blocks() -> Vec<[i16; 64]> {
    let mut blocks = vec![
        FDCT_IN1,
        FDCT_IN2,
        [-128; 64],
        [127; 64],
        [0; 64],
        std::array::from_fn(|i| if (i + i / 8) % 2 == 0 { 127 } else { -128 }),
        std::array::from_fn(|i| if i % 8 < 4 { 127 } else { -128 }),
        std::array::from_fn(|i| if i < 32 { -128 } else { 127 }),
        std::array::from_fn(|i| (i as i16 * 4) - 128),
    ];
    let mut s = 7u32;
    for _ in 0..2000 {
        blocks.push(std::array::from_fn(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ((s >> 16) & 0xFF) as i16 - 128
        }));
    }
    blocks
}

#[cfg(target_arch = "x86_64")]
#[test]
fn avx2_fdct_matches_scalar() {
    let Some(ops) = avx2() else { return };
    for input in test_blocks() {
        let mut a = Block(input);
        let mut b = Block(input);
        dct::fdct(&mut a);
        ops.fdct(&mut b);
        assert_eq!(a.0, b.0, "input {input:?}");
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn avx2_colour_rows_match_scalar() {
    let Some(ops) = avx2() else { return };
    let mut rgba: Vec<u8> = image(Pattern::Noise, 515, 1).data;
    for (i, px) in [
        [0, 0, 0],
        [255, 255, 255],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
    ]
    .iter()
    .enumerate()
    {
        rgba[i * 4..i * 4 + 3].copy_from_slice(px);
    }
    for width in [0, 1, 7, 8, 9, 16, 515] {
        let src = &rgba[..width * 4];
        let mut a = [vec![0; width], vec![0; width], vec![0; width]];
        let mut b = [vec![0; width], vec![0; width], vec![0; width]];
        let [ay, acb, acr] = &mut a;
        color::ycbcr_row(src, ay, acb, acr);
        let [by, bcb, bcr] = &mut b;
        ops.ycbcr_row(src, by, bcb, bcr);
        assert_eq!(a, b, "width {width}");
    }
}

#[test]
fn colour_conversion_is_bt601_full_range() {
    assert_eq!(color::rgb_to_ycbcr(0, 0, 0), (0, 128, 128));
    assert_eq!(color::rgb_to_ycbcr(255, 255, 255), (255, 128, 128));
    assert_eq!(color::rgb_to_ycbcr(128, 128, 128), (128, 128, 128));
    assert_eq!(color::rgb_to_ycbcr(255, 0, 0), (76, 85, 255));
    assert_eq!(color::rgb_to_ycbcr(0, 255, 0), (150, 44, 21));
    assert_eq!(color::rgb_to_ycbcr(0, 0, 255), (29, 255, 107));
}

#[test]
fn chroma_blocks_average_two_by_two_with_libjpeg_bias() {
    let stride = 16;
    let mut data = vec![0u8; stride * 16];
    data[0] = 1;
    data[2] = 1;
    data[4] = 2;
    data[6] = 2;
    let b = color::block_averaged(&data, 0, 0, stride);
    assert_eq!(&b.0[..4], &[-128, -128, -128, -127]);

    let data: Vec<u8> = (0..stride * 16).map(|i| (i % 251) as u8).collect();
    let b = color::block_averaged(&data, 0, 0, stride);
    for (i, &v) in b.0.iter().enumerate() {
        let (r, c) = (i / 8, i % 8);
        let at = r * 2 * stride + c * 2;
        let sum = u16::from(data[at])
            + u16::from(data[at + 1])
            + u16::from(data[at + stride])
            + u16::from(data[at + stride + 1]);
        let bias = if c % 2 == 0 { 1 } else { 2 };
        assert_eq!(v, ((sum + bias) / 4) as i16 - 128, "sample {i}");
    }
}

#[test]
fn coefficient_categories_and_bits() {
    for (v, want) in [
        (0i16, (0u8, 0u16)),
        (1, (1, 1)),
        (-1, (1, 0)),
        (2, (2, 2)),
        (-2, (2, 1)),
        (3, (2, 3)),
        (-3, (2, 0)),
        (255, (8, 255)),
        (-255, (8, 0)),
        (1023, (10, 1023)),
        (-1024, (11, 1023)),
        (2047, (11, 2047)),
        (-2047, (11, 0)),
    ] {
        assert_eq!(writer::get_code(v), want, "value {v}");
    }
}

#[test]
fn huffman_codes_follow_annex_k() {
    let dc = HuffmanTable::luma_dc();
    assert_eq!(dc.get_for_value(0), (2, 0b00));
    assert_eq!(dc.get_for_value(1), (3, 0b010));
    assert_eq!(dc.get_for_value(5), (3, 0b110));
    assert_eq!(dc.get_for_value(11), (9, 0b1_1111_1110));
    let ac = HuffmanTable::luma_ac();
    assert_eq!(ac.get_for_value(0x00), (4, 0b1010));
    assert_eq!(ac.get_for_value(0x01), (2, 0b00));
    assert_eq!(ac.get_for_value(0xF0), (11, 0b111_1111_1001));
    let cac = HuffmanTable::chroma_ac();
    assert_eq!(cac.get_for_value(0x00), (2, 0b00));
    assert_eq!(cac.get_for_value(0xF0), (10, 0b11_1111_1010));
}

#[test]
fn bits_are_byte_stuffed_and_padded_with_ones() {
    let mut w = Writer::with_capacity(16);
    w.write_bits(0xFF, 8);
    w.write_bits(0b101, 3);
    w.finalize_bits();
    assert_eq!(w.into_inner(), vec![0xFF, 0x00, 0b1011_1111]);

    let mut w = Writer::with_capacity(64);
    for _ in 0..20 {
        w.write_bits(0xFFFF, 16);
    }
    w.finalize_bits();
    let out = w.into_inner();
    assert_eq!(out.len(), 80);
    assert!(out.chunks_exact(2).all(|p| p == [0xFF, 0x00]));
}

#[test]
fn bad_input_is_refused_by_name() {
    let img = image(Pattern::Flat, 8, 8);
    for q in [0u8, 101, 255] {
        let e = encode(&img, q).expect_err("bad quality");
        assert!(e.0.contains("quality"), "{e}");
    }
    for (w, h) in [(0, 0), (0, 5), (5, 0)] {
        let e = encode(&Rgba::new(w, h), 75).expect_err("empty image");
        assert!(e.0.contains("nothing to encode"), "{e}");
    }
    let wide = Rgba {
        width: 65_536,
        height: 1,
        data: Vec::new(),
    };
    assert!(
        encode(&wide, 75)
            .expect_err("too wide")
            .0
            .contains("larger")
    );
    let tall = Rgba {
        width: 1,
        height: 70_000,
        data: Vec::new(),
    };
    assert!(
        encode(&tall, 75)
            .expect_err("too tall")
            .0
            .contains("larger")
    );
    let short = Rgba {
        width: 4,
        height: 4,
        data: vec![0; 63],
    };
    let e = encode(&short, 75).expect_err("short data");
    assert!(e.0.contains("needs 64"), "{e}");
    let e: Box<dyn std::error::Error> = Box::new(e);
    assert!(e.to_string().starts_with("JPEG: "));
}

#[test]
fn the_largest_side_is_accepted() {
    let img = image(Pattern::Flat, 65_535, 1);
    let file = encode(&img, 50).expect("65535 wide");
    let sof = segments(&file)
        .into_iter()
        .find(|s| s.0 == 0xC0)
        .expect("SOF0");
    assert_eq!(&sof.1[1..5], &[0, 1, 0xFF, 0xFF]);
}

#[test]
fn the_stream_is_baseline_jfif_four_two_zero() {
    let img = image(Pattern::Gradient, 33, 17);
    let file = encode(&img, 75).expect("encode");
    let segs = segments(&file);
    let markers: Vec<u8> = segs.iter().map(|s| s.0).collect();
    assert_eq!(
        markers,
        [0xE0, 0xDB, 0xDB, 0xC0, 0xC4, 0xC4, 0xC4, 0xC4, 0xDA]
    );
    assert_eq!(&segs[0].1[..5], b"JFIF\0");
    let lq = QuantTable::new(&quant::LUMA, 75);
    let cq = QuantTable::new(&quant::CHROMA, 75);
    for (seg, id, table) in [(&segs[1], 0, &lq), (&segs[2], 1, &cq)] {
        assert_eq!(seg.1[0], id);
        let want: Vec<u8> = writer::ZIGZAG
            .iter()
            .map(|&z| table.get(usize::from(z)))
            .collect();
        assert_eq!(&seg.1[1..], &want[..]);
    }
    assert_eq!(
        segs[3].1,
        &[8, 0, 17, 0, 33, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]
    );
    let dht: Vec<u8> = segs[4..8].iter().map(|s| s.1[0]).collect();
    assert_eq!(dht, [0x00, 0x10, 0x01, 0x11]);
    assert_eq!(segs[8].1, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
    assert_eq!(&file[file.len() - 2..], &[0xFF, 0xD9]);
    let scan = scan_data(&file);
    for (i, w) in scan.windows(2).enumerate() {
        if w[0] == 0xFF {
            assert_eq!(w[1], 0x00, "unstuffed 0xFF at {i}");
        }
    }
    assert_ne!(scan.last(), Some(&0xFF));
}

fn padded(img: &Rgba, width: u32, height: u32) -> Rgba {
    let mut out = Rgba::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let sx = x.min(img.width - 1);
            let sy = y.min(img.height - 1);
            let from = ((sy * img.width + sx) * 4) as usize;
            let to = ((y * width + x) * 4) as usize;
            out.data[to..to + 4].copy_from_slice(&img.data[from..from + 4]);
        }
    }
    out
}

#[test]
fn edges_replicate_to_whole_macroblocks() {
    for pattern in PATTERNS {
        for (w, h) in [(1, 1), (7, 5), (33, 17), (17, 33)] {
            let img = image(pattern, w, h);
            let full = padded(&img, w.div_ceil(16) * 16, h.div_ceil(16) * 16);
            for q in [10, 75, 100] {
                let a = encode(&img, q).expect("cropped");
                let b = encode(&full, q).expect("padded");
                assert_eq!(scan_data(&a), scan_data(&b), "{pattern:?} {w}x{h} q{q}");
            }
        }
    }
}

#[test]
fn alpha_is_ignored() {
    let opaque = image(Pattern::Noise, 33, 17);
    let mut clear = opaque.clone();
    for (i, px) in clear.data.chunks_exact_mut(4).enumerate() {
        px[3] = (i * 37) as u8;
    }
    assert_eq!(encode(&opaque, 80), encode(&clear, 80));
}

#[test]
fn extra_bytes_after_the_pixels_are_ignored() {
    let img = image(Pattern::Gradient, 9, 9);
    let mut longer = img.clone();
    longer.data.extend_from_slice(&[7; 40]);
    assert_eq!(encode(&img, 60), encode(&longer, 60));
}

#[test]
fn every_quality_decodes_and_psnr_rises_with_quality() {
    let img = image(Pattern::Noise, 33, 17);
    let want = luma(&img);
    let mut last = 0.0;
    for q in 1..=100u8 {
        let file = encode(&img, q).expect("encode");
        assert_eq!(&file[..2], &[0xFF, 0xD8]);
        assert_eq!(&file[file.len() - 2..], &[0xFF, 0xD9]);
        if q % 3 != 1 {
            continue;
        }
        let yuv = planes(&file).unwrap_or_else(|| panic!("q{q}: we refused it"));
        assert_eq!(yuv.len(), 33 * 17 + 2 * 17 * 9, "q{q}");
        let p = psnr(&want, &yuv[..want.len()]);
        assert!(p + 0.5 >= last, "q{q}: {p:.2} dB fell below {last:.2}");
        last = p;
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn scalar_and_avx2_streams_are_byte_identical() {
    let Some(ops) = avx2() else { return };
    for pattern in PATTERNS {
        for (w, h) in SIZES.into_iter().chain([(8, 8), (17, 33), (255, 3)]) {
            let img = image(pattern, w, h);
            for q in QUALITIES {
                let a = scalar(&img, q);
                let b = encode_with(&img, q, ops).expect("avx2 encode");
                assert!(a == b, "{pattern:?} {w}x{h} q{q}: streams differ");
            }
        }
    }
}

fn floor(pattern: Pattern, quality: u8) -> f64 {
    match (pattern, quality) {
        (Pattern::Noise, 1) => 9.0,
        (Pattern::Noise, 50) => 13.0,
        (Pattern::Noise, 75) => 16.0,
        (Pattern::Noise, 90) => 24.0,
        (Pattern::Noise, _) => 38.0,
        (_, 1) => 18.0,
        (_, 50) => 30.0,
        (_, 75) => 32.0,
        (_, 90) => 36.0,
        _ => 40.0,
    }
}

fn chroma_floor(quality: u8) -> f64 {
    match quality {
        1 => 22.0,
        50 => 41.0,
        75 => 44.0,
        90 => 47.0,
        _ => 52.0,
    }
}

#[test]
fn every_size_and_quality_decodes_above_a_psnr_floor() {
    for pattern in PATTERNS {
        for (w, h) in SIZES {
            let img = image(pattern, w, h);
            let want = luma(&img);
            let (w, h) = (w as usize, h as usize);
            let expect_len = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
            for q in QUALITIES {
                let file = encode(&img, q).expect("encode");
                let yuv = planes(&file)
                    .unwrap_or_else(|| panic!("{pattern:?} {w}x{h} q{q}: we refused it"));
                assert_eq!(yuv.len(), expect_len, "{pattern:?} {w}x{h} q{q}: size");
                let p = psnr(&want, &yuv[..w * h]);
                let min = floor(pattern, q);
                assert!(p >= min, "{pattern:?} {w}x{h} q{q}: {p:.2} dB < {min}");
                if pattern != Pattern::Noise {
                    let (cb, cr) = chroma(&img);
                    let c = w.div_ceil(2) * h.div_ceil(2);
                    let u = psnr(&cb, &yuv[w * h..w * h + c]);
                    let v = psnr(&cr, &yuv[w * h + c..]);
                    let min = chroma_floor(q);
                    assert!(
                        u >= min && v >= min,
                        "{pattern:?} {w}x{h} q{q}: chroma {u:.2}/{v:.2} dB < {min}"
                    );
                }
            }
        }
    }
}

#[test]
fn flat_colours_decode_back() {
    for rgb in [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [128, 128, 128],
        [0, 0, 0],
        [255, 255, 255],
        [200, 120, 40],
    ] {
        let mut img = Rgba::new(24, 20);
        for px in img.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        let file = encode(&img, 90).expect("encode");
        let out: Vec<u8> = decode(&file)
            .expect("decode")
            .data
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        assert_eq!(out.len(), 24 * 20 * 3);
        for px in out.chunks_exact(3) {
            for c in 0..3 {
                let d = (i16::from(px[c]) - i16::from(rgb[c])).abs();
                assert!(d <= 4, "{rgb:?} decoded as {px:?}");
            }
        }
    }
}

fn photo_floor(quality: u8) -> f64 {
    match quality {
        50 => 31.0,
        75 => 32.5,
        85 => 34.0,
        _ => 36.5,
    }
}

#[test]
fn size_and_psnr_stay_close_to_libjpeg_turbo() {
    let img = refs::photo_like(1920, 1080);
    let want = luma(&img);
    for q in refs::LIBJPEG_QUALITIES {
        let ours = scalar(&img, q);
        let fast = encode(&img, q).expect("encode");
        assert!(ours == fast, "q{q}: dispatch and scalar differ");
        let ours_y = planes(&ours).expect("decode ours");
        let ours_psnr = psnr(&want, &ours_y[..want.len()]);
        eprintln!("1080p q{q}: ours {} B {ours_psnr:.2} dB", ours.len());
        assert!(
            ours_psnr >= photo_floor(q),
            "q{q}: {ours_psnr:.2} dB < {}",
            photo_floor(q)
        );
    }
    let r = Refs::open(refs::AREA);
    let (w, h) = refs::LIBJPEG_SIZE;
    let img = refs::photo_like(w, h);
    let want = luma(&img);
    for q in refs::LIBJPEG_QUALITIES {
        let ours = encode(&img, q).expect("encode");
        let ours_y = planes(&ours).expect("decode ours");
        let ours_psnr = psnr(&want, &ours_y[..want.len()]);
        let theirs = r.bytes(&format!("{}.jpg", refs::libjpeg(q)));
        let theirs_y = planes(&theirs).expect("decode cjpeg");
        let theirs_psnr = psnr(&want, &theirs_y[..want.len()]);
        let ratio = ours.len() as f64 / theirs.len() as f64;
        eprintln!(
            "{w}x{h} q{q}: ours {} B {ours_psnr:.2} dB, libjpeg-turbo {} B {theirs_psnr:.2} dB; size ratio {ratio:.3}",
            ours.len(),
            theirs.len()
        );
        if q == 85 {
            assert!((0.9..=1.1).contains(&ratio), "q85 size ratio {ratio:.3}");
        }
        assert!(
            ours_psnr >= theirs_psnr - 0.5,
            "q{q}: {ours_psnr:.2} dB against libjpeg-turbo's {theirs_psnr:.2}"
        );
    }
}

#[test]
fn the_photo_is_the_gradient_with_grain_a_disc_and_stripes() {
    let (w, h) = refs::LIBJPEG_SIZE;
    assert_eq!(refs::gradient(w, h), image(Pattern::Gradient, w, h));
    let photo = refs::photo_like(w, h);
    assert_eq!((photo.width, photo.height), (w, h));
    assert_ne!(photo, image(Pattern::Gradient, w, h));
    assert!(photo.data.chunks_exact(4).all(|p| p[3] == 255));
}
