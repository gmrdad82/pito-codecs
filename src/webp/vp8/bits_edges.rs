use super::bits::{BoolDecoder, BoolEncoder, Code, codes};
use super::tables::{BMODE_TREE, KF_YMODE_TREE, SEGMENT_TREE, UV_MODE_TREE};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
}

#[test]
fn random_bits_round_trip_at_every_probability_and_length() {
    let mut rng = Lcg(1);
    for len in [
        0usize, 1, 7, 8, 9, 55, 56, 57, 63, 64, 65, 200, 4096, 100_000,
    ] {
        let bits: Vec<(bool, u8)> = (0..len)
            .map(|i| {
                let p = match i % 5 {
                    0 => 0,
                    1 => 1,
                    2 => 255,
                    _ => rng.next() as u8,
                };
                let skew = rng.next() % 256;
                (skew >= p as u32, p)
            })
            .collect();
        let mut e = BoolEncoder::with_capacity(16);
        for &(b, p) in &bits {
            e.put(b, p);
        }
        let data = e.finish();
        let mut d = BoolDecoder::new(&data);
        for (i, &(b, p)) in bits.iter().enumerate() {
            assert_eq!(d.get(p), b, "len {len} bit {i} prob {p}");
        }
        assert!(!d.eof(), "len {len}: the encoded bits read past the end");
    }
}

#[test]
fn long_runs_of_unlikely_ones_carry_into_earlier_bytes() {
    let mut e = BoolEncoder::with_capacity(0);
    let pattern: Vec<(bool, u8)> = (0..20_000)
        .map(|i| (i % 97 != 0, if i % 3 == 0 { 2 } else { 250 }))
        .collect();
    for &(b, p) in &pattern {
        e.put(b, p);
    }
    let data = e.finish();
    let mut d = BoolDecoder::new(&data);
    for (i, &(b, p)) in pattern.iter().enumerate() {
        assert_eq!(d.get(p), b, "bit {i}");
    }
}

#[test]
fn literals_and_signed_values_round_trip() {
    let values: [(i32, u32); 8] = [
        (0, 1),
        (1, 1),
        (0, 7),
        (127, 7),
        (-127, 7),
        (-1, 4),
        (65_535, 16),
        (-65_535, 16),
    ];
    let mut e = BoolEncoder::with_capacity(64);
    for &(v, n) in &values {
        e.literal(v.unsigned_abs(), n);
        e.signed(v, n);
        e.optional_signed(v, n);
        e.flag(v < 0);
    }
    let data = e.finish();
    let mut d = BoolDecoder::new(&data);
    for &(v, n) in &values {
        assert_eq!(d.literal(n), v.unsigned_abs(), "literal {v}");
        assert_eq!(d.signed(n), v, "signed {v}");
        assert_eq!(d.optional_signed(n), v, "optional {v}");
        assert_eq!(d.flag(), v < 0, "flag {v}");
    }
    let mut e = BoolEncoder::with_capacity(8);
    e.flag(true);
    e.flag(false);
    let data = e.finish();
    let mut d = BoolDecoder::new(&data);
    assert_eq!((d.get_signed(5), d.get_signed(5)), (-5, 5));
}

fn tree_round_trip<const N: usize>(tree: &[i8], table: &[Code; N]) {
    let mut rng = Lcg(N as u64);
    for _ in 0..8 {
        let probs: Vec<u8> = (0..tree.len() / 2).map(|_| rng.next() as u8 | 1).collect();
        let symbols: Vec<usize> = (0..200).map(|_| rng.next() as usize % N).collect();
        let mut e = BoolEncoder::with_capacity(64);
        for &s in &symbols {
            e.code(&table[s], &probs);
        }
        let data = e.finish();
        let mut d = BoolDecoder::new(&data);
        for &s in &symbols {
            assert_eq!(d.tree(tree, &probs) as usize, s, "tree of {N}");
        }
    }
    let leaves = tree.iter().filter(|&&v| v <= 0).count();
    assert_eq!(leaves, N);
    for (s, c) in table.iter().enumerate() {
        assert!(c.len >= 1 && (c.len as usize) < tree.len(), "symbol {s}");
    }
}

#[test]
fn every_tree_symbol_round_trips_through_its_code() {
    const Y: [Code; 5] = codes(&KF_YMODE_TREE);
    const UV: [Code; 4] = codes(&UV_MODE_TREE);
    const B: [Code; 10] = codes(&BMODE_TREE);
    const SEG: [Code; 4] = codes(&SEGMENT_TREE);
    tree_round_trip(&KF_YMODE_TREE, &Y);
    tree_round_trip(&UV_MODE_TREE, &UV);
    tree_round_trip(&BMODE_TREE, &B);
    tree_round_trip(&SEGMENT_TREE, &SEG);
}

#[test]
fn reading_past_the_end_yields_bits_without_panicking() {
    for data in [
        &[][..],
        &[0xFF][..],
        &[0x80, 0x01, 0xFE][..],
        &[0x5A; 7][..],
        &[0xA5; 8][..],
    ] {
        let mut d = BoolDecoder::new(data);
        let mut ones = 0;
        for i in 0..10_000 {
            ones += d.get((i % 256) as u8) as u32;
        }
        let _ = (d.literal(32), d.signed(31), d.tree(&BMODE_TREE, &[128; 9]));
        assert!(d.eof(), "{data:?}");
        let mut again = BoolDecoder::new(data);
        let mut same = 0;
        for i in 0..10_000 {
            same += again.get((i % 256) as u8) as u32;
        }
        assert_eq!(ones, same, "{data:?}");
    }
    let mut d = BoolDecoder::new(&[]);
    assert!(d.eof());
    assert!((0..10_000).all(|i| !d.get((i % 256) as u8)));
    assert_eq!(d.literal(32), 0);

    let mut d = BoolDecoder::new(&[0xFF]);
    assert!(!d.eof());
    assert!(d.get(128));
    assert!(!d.eof());
    assert!(d.get(128));
    assert!(d.eof());
    assert!((0..1000).all(|_| d.get(128)));

    let mut d = BoolDecoder::new(&[0x5A; 9]);
    for _ in 0..8 {
        d.get(128);
    }
    assert!(!d.eof());
}

fn riff(vp8: &[u8]) -> Vec<u8> {
    let pad = vp8.len() & 1;
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&((12 + vp8.len() + pad) as u32).to_le_bytes());
    f.extend_from_slice(b"WEBPVP8 ");
    f.extend_from_slice(&(vp8.len() as u32).to_le_bytes());
    f.extend_from_slice(vp8);
    f.resize(f.len() + pad, 0);
    f
}

#[test]
fn a_token_partition_cut_short_is_refused_like_libwebp() {
    let mut rng = Lcg(7);
    for (w, h) in [(64u32, 48u32), (320, 256)] {
        let rgba: Vec<u8> = (0..w * h * 4).map(|_| rng.next() as u8).collect();
        let vp8 = super::encode(&rgba, w, h, &crate::webp::Options::lossy(95.0)).unwrap();
        let first = (u32::from_le_bytes([vp8[0], vp8[1], vp8[2], 0]) >> 5) as usize;
        let tokens = 10 + first;
        assert!(vp8.len() > tokens + 64, "{w}x{h}: {} bytes", vp8.len());
        let mut out = vec![0u8; (w * h * 4) as usize];
        super::decode_into(&vp8, &mut out).unwrap();
        assert!(crate::webp::decode(&riff(&vp8)).is_ok());
        for cut in [tokens, tokens + 1, (tokens + vp8.len()) / 2, vp8.len() - 16] {
            let short = &vp8[..cut];
            assert!(
                super::decode_into(short, &mut out).is_err(),
                "{w}x{h} cut {cut}"
            );
            assert!(
                crate::webp::decode(&riff(short)).is_err(),
                "{w}x{h} riff cut {cut}"
            );
        }
    }
}

#[test]
fn a_first_partition_that_runs_out_is_refused() {
    let mut rng = Lcg(9);
    let rgba: Vec<u8> = (0..64 * 64 * 4).map(|_| rng.next() as u8).collect();
    let vp8 = super::encode(&rgba, 64, 64, &crate::webp::Options::lossy(80.0)).unwrap();
    let first = (u32::from_le_bytes([vp8[0], vp8[1], vp8[2], 0]) >> 5) as usize;
    let mut out = vec![0u8; 64 * 64 * 4];
    for keep in [0usize, 1, 2, first / 2] {
        let mut c = vp8[..10].to_vec();
        let tag = u32::from_le_bytes([c[0], c[1], c[2], 0]) & 0x1F | (keep as u32) << 5;
        c[..3].copy_from_slice(&tag.to_le_bytes()[..3]);
        c.extend_from_slice(&vp8[10..10 + keep]);
        c.extend_from_slice(&vp8[10 + first..]);
        assert!(
            super::decode_into(&c, &mut out).is_err(),
            "first partition of {keep}"
        );
    }
}
