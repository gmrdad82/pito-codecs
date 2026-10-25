use super::tests::expected;
use super::{decode, info};
use crate::media::Error;
use crate::testref::{self, Refs};

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

use refs::{CLAIMED, Image, Lcg, SIGNATURE, chunk, palette, stored};

const SIZES: [(u32, u32); 5] = [(1, 1), (2, 3), (5, 1), (9, 7), (17, 11)];

#[test]
fn every_colour_type_depth_and_interlace_matches_the_png_crate() {
    let mut rng = Lcg(7);
    for (color, depths) in CLAIMED {
        for &depth in depths {
            for interlaced in [false, true] {
                for (w, h) in SIZES {
                    let img = Image::new(w, h, color, depth, interlaced);
                    let extra = if color == 3 {
                        vec![palette(&mut rng)]
                    } else {
                        vec![]
                    };
                    let file = img.file(&mut rng, &extra);
                    let name =
                        format!("colour {color} depth {depth} {w}x{h} interlaced {interlaced}");
                    let i = info(&file).unwrap();
                    assert_eq!(
                        (i.width, i.height, i.depth, i.interlaced),
                        (w, h, depth, interlaced)
                    );
                    let ours = decode(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
                    assert_eq!((ours.width, ours.height), (w, h), "{name}");
                    assert!(ours.data == expected(&file), "{name}: pixels differ");
                }
            }
        }
    }
}

#[test]
fn transparency_keys_match_the_png_crate() {
    let mut rng = Lcg(21);
    let cases: [(u8, u8, Vec<u8>); 7] = [
        (0, 1, vec![0, 1]),
        (0, 2, vec![0, 2]),
        (0, 4, vec![0, 9]),
        (0, 16, vec![0x12, 0x34]),
        (2, 8, vec![0, 7, 0, 7, 0, 7]),
        (2, 16, vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
        (3, 8, vec![0, 128, 255, 3]),
    ];
    for (color, depth, key) in cases {
        for interlaced in [false, true] {
            let mut extra = vec![];
            if color == 3 {
                extra.push(palette(&mut rng));
            }
            extra.push(chunk(b"tRNS", &key));
            let mut img = Image::new(13, 9, color, depth, interlaced);
            if depth >= 8 && color != 3 {
                img.key = if depth == 16 {
                    key.clone()
                } else {
                    key.iter().skip(1).step_by(2).copied().collect()
                };
            }
            let file = img.file(&mut rng, &extra);
            let name = format!("colour {color} depth {depth} interlaced {interlaced}");
            let ours = decode(&file).unwrap_or_else(|e| panic!("{name}: {e}"));
            let want = expected(&file);
            assert!(ours.data == want, "{name}: pixels differ");
            if color != 3 {
                assert!(
                    want.chunks_exact(4).any(|p| p[3] == 0),
                    "{name}: key never hit"
                );
            }
        }
    }
}

#[test]
fn bad_crcs_refuse_critical_chunks_and_drop_ancillary_ones() {
    let mut rng = Lcg(3);
    let img = Image::new(6, 4, 3, 8, false);
    let extra = vec![
        palette(&mut rng),
        chunk(b"tRNS", &[0; 200]),
        chunk(b"tEXt", b"Comment\0x"),
    ];
    let good = img.file(&mut rng, &extra);
    assert!(
        decode(&good)
            .unwrap()
            .data
            .chunks_exact(4)
            .all(|p| p[3] == 0)
    );
    let crc_at = |kind: &[u8; 4]| {
        let at = good.windows(4).position(|w| w == kind).unwrap() - 4;
        let len = u32::from_be_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        at + 8 + len
    };
    for kind in [b"IHDR", b"PLTE", b"IDAT", b"IEND"] {
        let mut bad = good.clone();
        bad[crc_at(kind)] ^= 0x40;
        match decode(&bad) {
            Err(Error::Invalid(m)) => assert!(m.contains("CRC"), "{m}"),
            other => panic!("{kind:?}: {other:?}"),
        }
    }
    let mut bad = good.clone();
    bad[crc_at(b"IHDR")] ^= 1;
    match info(&bad) {
        Err(Error::Invalid(m)) => assert!(m.contains("CRC on IHDR"), "{m}"),
        other => panic!("info on a bad IHDR CRC: {other:?}"),
    }
    assert!(info(&good[..29]).is_err());
    assert!(info(&good[..33]).is_ok());
    let mut bad = good.clone();
    bad[crc_at(b"tRNS")] ^= 1;
    assert!(
        decode(&bad)
            .unwrap()
            .data
            .chunks_exact(4)
            .all(|p| p[3] == 255)
    );
    let mut bad = good.clone();
    bad[crc_at(b"tEXt")] ^= 1;
    assert_eq!(decode(&bad).unwrap().data, decode(&good).unwrap().data);
    let at = good.windows(4).position(|w| w == b"IDAT").unwrap();
    let len = u32::from_be_bytes(good[at - 4..at].try_into().unwrap()) as usize;
    let mut z = good[at + 4..at + 4 + len].to_vec();
    assert!(z.len() > 4);
    z[3] ^= 0xFF;
    let mut body = good[..at - 4].to_vec();
    body.extend(chunk(b"IDAT", &z));
    body.extend_from_slice(&good[at + 8 + len..]);
    let e = decode(&body).unwrap_err().to_string();
    assert!(e.contains("length check"), "{e}");
    assert!(decode(&good[..good.len() - 2]).is_err());
}

#[test]
fn header_and_layout_faults_are_refused_by_name() {
    let mut rng = Lcg(5);
    let rgb = Image::new(4, 4, 2, 8, false);
    let with = |ihdr: &[u8], rest: &[Vec<u8>]| {
        let mut f = SIGNATURE.to_vec();
        f.extend(chunk(b"IHDR", ihdr));
        for c in rest {
            f.extend_from_slice(c);
        }
        f
    };
    let ihdr = |w: u32, h: u32, color: u8, depth: u8, comp: u8, filt: u8, il: u8| {
        let mut b = w.to_be_bytes().to_vec();
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[depth, color, comp, filt, il]);
        b
    };
    let idat = chunk(b"IDAT", &stored(&rgb.rows(&mut rng)));
    let end = chunk(b"IEND", &[]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "unknown colour type 5",
            with(&ihdr(4, 4, 5, 8, 0, 0, 0), &[]),
        ),
        (
            "bit depth 4 with colour type 2",
            with(&ihdr(4, 4, 2, 4, 0, 0, 0), &[]),
        ),
        (
            "bit depth 16 with colour type 3",
            with(&ihdr(4, 4, 3, 16, 0, 0, 0), &[]),
        ),
        (
            "bit depth 3 with colour type 0",
            with(&ihdr(4, 4, 0, 3, 0, 0, 0), &[]),
        ),
        (
            "unknown interlace method 2",
            with(&ihdr(4, 4, 2, 8, 0, 0, 2), &[]),
        ),
        ("unknown compression", with(&ihdr(4, 4, 2, 8, 1, 0, 0), &[])),
        ("unknown compression", with(&ihdr(4, 4, 2, 8, 0, 1, 0), &[])),
        ("zero width", with(&ihdr(0, 4, 2, 8, 0, 0, 0), &[])),
        (
            "too large",
            with(&ihdr(1 << 15, 1 << 14, 2, 8, 0, 0, 0), &[]),
        ),
        (
            "no image data",
            with(&ihdr(4, 4, 2, 8, 0, 0, 0), std::slice::from_ref(&end)),
        ),
        (
            "before any image data",
            with(&ihdr(4, 4, 2, 8, 0, 0, 0), &[]),
        ),
        (
            "PLTE chunk of the wrong length",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[chunk(b"PLTE", &[0; 4])]),
        ),
        (
            "PLTE chunk of the wrong length",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[chunk(b"PLTE", &[0; 771])]),
        ),
        (
            "without PLTE",
            with(&ihdr(4, 4, 3, 8, 0, 0, 0), &[idat.clone(), end.clone()]),
        ),
        (
            "runs past the end",
            with(
                &ihdr(4, 4, 2, 8, 0, 0, 0),
                &[idat[..idat.len() - 6].to_vec()],
            ),
        ),
        (
            "ends early",
            with(&ihdr(4, 5, 2, 8, 0, 0, 0), &[idat.clone(), end.clone()]),
        ),
        ("first chunk is not", {
            let mut f = SIGNATURE.to_vec();
            f.extend(chunk(b"IHDX", &ihdr(4, 4, 2, 8, 0, 0, 0)));
            f
        }),
        ("not a PNG", b"GIF89a\0\0\0\0\0\0".to_vec()),
    ];
    for (want, file) in cases {
        match decode(&file) {
            Err(Error::Invalid(m)) => assert!(m.contains(want), "{want}: got {m}"),
            other => panic!("{want}: {other:?}"),
        }
    }
    let mut rows = rgb.rows(&mut rng);
    rows[13] = 5;
    let f = with(&rgb.ihdr(), &[chunk(b"IDAT", &stored(&rows)), end]);
    match decode(&f) {
        Err(Error::Invalid(m)) => assert!(m.contains("unknown filter type 5"), "{m}"),
        other => panic!("filter 5: {other:?}"),
    }
}

#[test]
fn palette_indices_past_the_palette_are_opaque_black() {
    let img = Image::new(4, 1, 3, 8, false);
    let rows = vec![0u8, 0, 1, 2, 3];
    let mut f = SIGNATURE.to_vec();
    f.extend(chunk(b"IHDR", &img.ihdr()));
    f.extend(chunk(b"PLTE", &[10, 20, 30, 40, 50, 60]));
    f.extend(chunk(b"tRNS", &[7]));
    f.extend(chunk(b"IDAT", &stored(&rows)));
    f.extend(chunk(b"IEND", &[]));
    let out = decode(&f).unwrap();
    assert_eq!(
        out.data,
        [10, 20, 30, 7, 40, 50, 60, 255, 0, 0, 0, 255, 0, 0, 0, 255]
    );
}

#[test]
fn corrupted_and_truncated_files_never_panic() {
    let mut rng = Lcg(77);
    for (color, depth, interlaced) in [(3, 2, true), (6, 16, false), (0, 1, true)] {
        let img = Image::new(11, 7, color, depth, interlaced);
        let extra = if color == 3 {
            vec![palette(&mut rng)]
        } else {
            vec![]
        };
        let good = img.file(&mut rng, &extra);
        for cut in 0..good.len() {
            assert!(
                decode(&good[..cut]).is_err() || cut >= good.len() - 12,
                "cut {cut}"
            );
        }
        let rows = img.rows(&mut rng);
        for at in 0..rows.len() {
            for v in [0, 1, 4, 5, 0xFF, rows[at] ^ 0x08] {
                let mut bad = rows.clone();
                bad[at] = v;
                let _ = decode(&img.wrap(&bad, &extra));
                let _ = decode(&img.wrap(&bad[..at], &extra));
            }
        }
        for at in 8..good.len() {
            for v in [0x00, 0xFF, good[at] ^ 0x10] {
                let mut bad = good.clone();
                bad[at] = v;
                let _ = decode(&bad);
            }
        }
    }
}

fn like_ffmpeg(r: &Refs, case: &refs::Case) {
    let name = &case.name;
    r.assert_digest(&format!("{name}.png"), &testref::sha256(&case.file));
    let ours = decode(&case.file).unwrap_or_else(|e| panic!("{name}: {e}"));
    if !case.wide() {
        r.assert_digest(&format!("{name}.rgba"), &testref::sha256(&ours.data));
        return;
    }
    let wide = r.bytes(&format!("{name}.rgba64"));
    assert_eq!(wide.len(), ours.data.len() * 2, "{name}: sizes");
    let slack = if matches!(case.color, 0 | 4) { 1 } else { 0 };
    for (i, (&o, w)) in ours.data.iter().zip(wide.chunks_exact(2)).enumerate() {
        assert!(
            o.abs_diff(w[0]) <= slack,
            "{name}: byte {i} is {o}, ffmpeg's high byte {}",
            w[0]
        );
    }
}

#[test]
fn every_claimed_type_and_depth_decodes_like_ffmpeg() {
    let r = Refs::open(refs::AREA);
    let cases = refs::claimed();
    assert_eq!(
        cases.len(),
        2 * CLAIMED.iter().map(|(_, d)| d.len()).sum::<usize>()
    );
    for case in &cases {
        like_ffmpeg(&r, case);
    }
}

#[test]
fn transparency_keys_decode_like_ffmpeg() {
    let r = Refs::open(refs::AREA);
    for case in refs::keyed() {
        let name = &case.name;
        let ours = decode(&case.file).unwrap();
        assert!(
            ours.data.chunks_exact(4).any(|p| p[3] == 0),
            "{name}: key never hit"
        );
        if case.sub_byte_grey() {
            r.assert_digest(&format!("{name}.png"), &testref::sha256(&case.file));
            let opaque: Vec<u8> = ours
                .data
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect();
            r.assert_digest(&format!("{name}.rgba"), &testref::sha256(&opaque));
            continue;
        }
        like_ffmpeg(&r, &case);
    }
}
