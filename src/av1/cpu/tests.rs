use super::{Decoder, Picture};
use crate::av1::parser::Parser;
use crate::av1::tests::{
    Stats, obus, payload_start, refs, sequence_payload, shortened, small_stream, walk,
};
use crate::testref::{Digest, Refs};

fn ivf_packets(data: &[u8]) -> Vec<(Vec<u8>, i64)> {
    let mut out = Vec::new();
    let Some(h) = data.get(6..8) else {
        return out;
    };
    let mut pos = u16::from_le_bytes([h[0], h[1]]) as usize;
    while let Some(head) = data.get(pos..pos + 12) {
        let n = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let mut pts = [0u8; 8];
        pts.copy_from_slice(&head[4..12]);
        let Some(p) = data.get(pos + 12..pos + 12 + n) else {
            break;
        };
        out.push((p.to_vec(), i64::from_le_bytes(pts)));
        pos += 12 + n;
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tool {
    Grain,
    Tiles,
    Restoration,
    Superres,
    Resize,
    Intrabc,
    Screen,
    Lossless,
    Compound,
}

fn has(stats: &Stats, tool: Tool) -> bool {
    match tool {
        Tool::Grain => stats.grain > 0,
        Tool::Tiles => stats.tiles > 1,
        Tool::Restoration => stats.restoration,
        Tool::Superres => stats.superres,
        Tool::Resize => stats.sizes.len() > 1,
        Tool::Intrabc => stats.intrabc,
        Tool::Screen => stats.screen,
        Tool::Lossless => stats.lossless,
        Tool::Compound => stats.compound,
    }
}

fn decode_all(data: &[u8], threads: usize) -> crate::media::Result<Vec<Picture>> {
    let mut dec = Decoder::with_threads(threads);
    let mut pics = Vec::new();
    for (p, pts) in ivf_packets(data) {
        pics.extend(dec.decode_pictures(&p, pts)?);
    }
    pics.extend(dec.flush_pictures()?);
    Ok(pics)
}

fn digest(pics: &[Picture]) -> String {
    let mut d = Digest::new();
    for p in pics {
        for pl in &p.planes {
            d.bytes(&pl.data);
        }
    }
    d.hex()
}

fn bit_exact(name: &str, tools: &[Tool]) {
    let s = refs::find(name);
    let r = Refs::open(refs::AREA);
    let data = r.bytes(&refs::ivf(name));
    let mut parser = Parser::default();
    let mut stats = Stats::default();
    for (p, _) in ivf_packets(&data) {
        walk(&mut parser, &p, &mut stats).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for &tool in tools {
        assert!(
            has(&stats, tool),
            "{name}: the encoder never used {tool:?}: {stats:?}"
        );
    }
    let mut first: Option<Vec<Vec<u8>>> = None;
    for threads in [1, 4] {
        let pics = decode_all(&data, threads)
            .unwrap_or_else(|e| panic!("{name} threads {threads}: {e}"));
        assert_eq!(pics.len() as u64, s.frames, "{name} threads {threads}");
        let planes: Vec<Vec<u8>> = pics
            .iter()
            .map(|p| p.planes.iter().flat_map(|pl| pl.data.clone()).collect())
            .collect();
        if let Some(one) = &first {
            for (i, (a, b)) in one.iter().zip(&planes).enumerate() {
                assert!(a == b, "{name}: frame {i} differs between 1 and {threads} threads");
            }
        }
        first = Some(planes);
        r.assert_digest(&refs::decoded(name), &digest(&pics));
    }
}

#[test]
fn bit_exact_against_libdav1d() {
    bit_exact("grain_tiles_10bit", &[Tool::Grain, Tool::Tiles]);
}

#[test]
fn compound_and_masked_inter_8bit_match_libdav1d() {
    bit_exact("compound_8", &[Tool::Compound]);
}

#[test]
fn compound_and_masked_inter_10bit_match_libdav1d() {
    bit_exact("compound_10", &[Tool::Compound]);
}

#[test]
fn screen_content_8bit_matches_libdav1d() {
    bit_exact("screen_8", &[Tool::Screen]);
}

#[test]
fn screen_content_10bit_matches_libdav1d() {
    bit_exact("screen_10", &[Tool::Screen]);
}

#[test]
fn lossless_matches_libdav1d() {
    bit_exact("lossless_8", &[Tool::Lossless]);
}

#[test]
fn restoration_8bit_from_rav1e_matches_libdav1d() {
    bit_exact(
        "rav1e_8",
        &[Tool::Restoration, Tool::Tiles, Tool::Compound],
    );
}

#[test]
fn restoration_10bit_from_rav1e_matches_libdav1d() {
    bit_exact(
        "rav1e_10",
        &[Tool::Restoration, Tool::Tiles, Tool::Compound],
    );
}

#[test]
fn superres_with_restoration_10bit_matches_libdav1d() {
    bit_exact("superres_10", &[Tool::Superres, Tool::Restoration]);
}

#[test]
fn scaled_references_8bit_match_libdav1d() {
    bit_exact("resize_8", &[Tool::Resize]);
}

#[test]
fn svt_intrabc_and_compound_8bit_match_libdav1d() {
    bit_exact("svt_8", &[Tool::Intrabc, Tool::Compound]);
}

#[test]
fn svt_screen_and_compound_10bit_match_libdav1d() {
    bit_exact("svt_10", &[Tool::Screen, Tool::Compound]);
}

#[test]
fn superres_8bit_matches_libdav1d() {
    bit_exact("superres_8", &[Tool::Superres]);
}

#[test]
fn lossless_10bit_matches_libdav1d() {
    bit_exact("lossless_10", &[Tool::Lossless]);
}

#[test]
fn palette_in_inter_frames_10bit_matches_libdav1d() {
    bit_exact("screen_inter_10", &[Tool::Screen]);
}

#[test]
fn the_vaapi_stream_decodes_on_the_cpu_like_libdav1d() {
    bit_exact("vaapi_noisy", &[Tool::Tiles]);
}

#[test]
fn a_damaged_stream_is_refused_before_use_and_decodes_differently() {
    let r = Refs::open(refs::AREA);
    let mut data = r.bytes(&refs::ivf("compound_8"));
    let at = data.len() - 8;
    data[at] ^= 0x5a;
    assert_ne!(crate::testref::sha256(&data), r.digest(&refs::ivf("compound_8")));
    let got = decode_all(&data, 1).ok().map(|p| digest(&p));
    assert_ne!(got.as_deref(), Some(r.digest(&refs::decoded("compound_8"))));
}

#[test]
fn the_digest_covers_every_plane_of_every_picture() {
    let data = small_stream();
    let pics = decode_all(&data, 1).unwrap();
    assert_eq!(pics.len(), 3);
    let whole = digest(&pics);
    assert_eq!(whole, digest(&pics));
    assert_ne!(whole, digest(&pics[..2]));
    let mut d = Digest::new();
    for p in &pics {
        for pl in &p.planes {
            assert!(!pl.data.is_empty());
            d.bytes(&pl.data);
        }
    }
    assert_eq!(d.hex(), whole);
}

fn first_unit() -> Vec<(Vec<u8>, i64)> {
    let packets = ivf_packets(&small_stream());
    assert_eq!(packets.len(), 3);
    packets
}

fn frames_of(dec: &mut Decoder, packets: &[(Vec<u8>, i64)]) -> usize {
    let mut n = 0;
    for (p, pts) in packets {
        n += dec.decode_pictures(p, *pts).unwrap().len();
    }
    n + dec.flush_pictures().unwrap().len()
}

#[test]
fn truncated_obus_are_errors_not_panics() {
    let packets = first_unit();
    let mut cuts = 0;
    for (k, (unit, pts)) in packets.iter().enumerate().take(2) {
        for (_, r) in obus(unit) {
            let len = r.end - r.start;
            let mut at: Vec<usize> = (1..len).step_by((len / 12).max(1)).collect();
            at.extend(
                [1, 2, len / 2, len - 1]
                    .into_iter()
                    .filter(|&c| c > 0 && c < len),
            );
            at.sort_unstable();
            at.dedup();
            for threads in [1, 3] {
                for &c in &at {
                    let mut dec = Decoder::with_threads(threads);
                    for (p, t) in &packets[..k] {
                        dec.decode_pictures(p, *t).unwrap();
                    }
                    let got = dec.decode_pictures(&unit[..r.start + c], *pts);
                    assert!(got.is_err(), "unit {k}: cut {c} into OBU {r:?} decoded");
                    cuts += 1;
                }
            }
        }
    }
    assert!(cuts > 30);
    let mut dec = Decoder::with_threads(2);
    let (unit, pts) = &packets[0];
    assert!(dec.decode_pictures(&unit[..unit.len() - 1], *pts).is_err());
    assert_eq!(frames_of(&mut dec, &packets), 3);
}

fn pixels(dec: &mut Decoder, packets: &[(Vec<u8>, i64)]) -> Vec<Vec<u8>> {
    let mut pics = Vec::new();
    for (p, pts) in packets {
        pics.extend(dec.decode_pictures(p, *pts).unwrap());
    }
    pics.extend(dec.flush_pictures().unwrap());
    pics.iter()
        .map(|p| {
            p.planes
                .iter()
                .flat_map(|pl| pl.data.iter().copied())
                .collect()
        })
        .collect()
}

fn clean(packets: &[(Vec<u8>, i64)]) -> Vec<Vec<u8>> {
    let want = pixels(&mut Decoder::with_threads(1), packets);
    assert_eq!(want.len(), packets.len());
    want
}

fn recovers(dec: &mut Decoder, packets: &[(Vec<u8>, i64)], want: &[Vec<u8>], what: &str) {
    let _ = dec.flush_pictures();
    assert!(
        pixels(dec, packets) == want,
        "{what}: the replay differs from a clean decode"
    );
}

#[test]
fn short_obus_with_valid_framing_are_errors_not_panics() {
    let packets = first_unit();
    let want = clean(&packets);
    let mut short = 0;
    for (k, (unit, pts)) in packets.iter().enumerate() {
        for (t, r) in obus(unit) {
            let len = r.end - payload_start(unit, &r);
            let mut keep: Vec<usize> = (0..len).step_by((len / 8).max(1)).collect();
            keep.extend(
                [1, 2, len / 2, len.saturating_sub(1)]
                    .into_iter()
                    .filter(|&c| c < len),
            );
            keep.sort_unstable();
            keep.dedup();
            for threads in [1, 3] {
                for &n in &keep {
                    let what =
                        format!("unit {k} OBU type {t} {r:?} short at {n}, threads {threads}");
                    let mut dec = Decoder::with_threads(threads);
                    for (p, q) in &packets[..k] {
                        dec.decode_pictures(p, *q).unwrap();
                    }
                    let got = dec.decode_pictures(&shortened(unit, &r, n), *pts);
                    if t == 1 || t == 3 {
                        assert!(got.is_err(), "{what} decoded");
                    }
                    recovers(&mut dec, &packets, &want, &what);
                    short += 1;
                }
            }
        }
    }
    assert!(short > 30);
}

#[test]
fn flipped_tile_data_is_ok_or_an_error_and_a_replay_decodes_clean() {
    let packets = first_unit();
    let want = clean(&packets);
    let mut flips = 0;
    for (k, (unit, pts)) in packets.iter().enumerate() {
        for (t, r) in obus(unit).into_iter().filter(|(t, _)| *t == 6 || *t == 4) {
            let start = payload_start(unit, &r);
            let len = r.end - start;
            if len < 2 {
                continue;
            }
            for i in 0..4 {
                let at = start + len / 2 + (len / 2) * i / 4;
                for mask in [0x01u8, 0x80, 0xff] {
                    let mut bad = unit.clone();
                    bad[at] ^= mask;
                    for threads in [1, 3] {
                        let what = format!(
                            "unit {k} OBU type {t} byte {at} ^ {mask:02x}, threads {threads}"
                        );
                        let mut dec = Decoder::with_threads(threads);
                        for (p, q) in &packets[..k] {
                            dec.decode_pictures(p, *q).unwrap();
                        }
                        let _ = dec.decode_pictures(&bad, *pts);
                        for (p, q) in &packets[k + 1..] {
                            let _ = dec.decode_pictures(p, *q);
                        }
                        recovers(&mut dec, &packets, &want, &what);
                        flips += 1;
                    }
                }
            }
        }
    }
    assert!(flips > 15);
}

#[test]
fn out_of_range_sequence_headers_are_errors() {
    let packets = first_unit();
    let (unit, pts) = &packets[0];
    let (r, payload) = sequence_payload(unit).unwrap();
    let mut patched = Vec::new();
    for profile in 3u8..=7 {
        let mut bad = unit.clone();
        bad[payload] = (bad[payload] & 0x1f) | (profile << 5);
        patched.push(bad);
    }
    let mut reduced = unit.clone();
    reduced[payload] = (reduced[payload] & 0x07) | 0x08;
    patched.push(reduced);
    for bad in &patched {
        let mut dec = Decoder::with_threads(1);
        assert!(
            dec.decode_pictures(bad, *pts).is_err(),
            "{:02x}",
            bad[payload]
        );
        let mut av1c = vec![0x81, 0, 0, 0];
        av1c.extend_from_slice(&bad[r.clone()]);
        assert!(dec.configure(&av1c).is_err(), "{:02x}", bad[payload]);
        assert_eq!(dec.sequence(), None);
    }
    let mut dec = Decoder::with_threads(1);
    let mut av1c = vec![0x81, 0, 0, 0];
    av1c.extend_from_slice(&unit[r.clone()]);
    let info = dec.configure(&av1c).unwrap().unwrap();
    assert_eq!(
        (info.max_width, info.max_height, info.bit_depth),
        (64, 64, 8)
    );
    assert_eq!(dec.sequence(), Some(info));
    assert!(dec.configure(&[0x82, 0, 0, 0]).is_err());
    for cut in 1..r.len() {
        let mut av1c = vec![0x81, 0, 0, 0];
        av1c.extend_from_slice(&unit[r.start..r.start + cut]);
        assert!(dec.configure(&av1c).is_err(), "av1C cut at {cut} parsed");
    }
    assert_eq!(dec.sequence(), Some(info));
    assert!(dec.configure(&[0x81, 0, 0]).is_err());
    assert_eq!(dec.configure(&[0x81, 0, 0, 0]).unwrap(), None);
    assert_eq!(frames_of(&mut dec, &packets), 3);
}

#[test]
fn a_frame_before_its_sequence_header_is_an_error() {
    let packets = first_unit();
    let (unit, pts) = &packets[0];
    let without: Vec<u8> = obus(unit)
        .into_iter()
        .filter(|(t, _)| *t != 1)
        .flat_map(|(_, r)| unit[r].to_vec())
        .collect();
    assert!(without.len() < unit.len());
    for threads in [1, 3] {
        let mut dec = Decoder::with_threads(threads);
        assert!(
            dec.decode_pictures(&without, *pts).is_err(),
            "threads {threads}"
        );
        assert!(
            dec.decode_pictures(&packets[1].0, packets[1].1).is_err(),
            "threads {threads}"
        );
        assert_eq!(frames_of(&mut dec, &packets), 3, "threads {threads}");
    }
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    let mut dec = Decoder::with_threads(2);
    let mut seed = 0x1234_5678u32;
    for _ in 0..64 {
        let buf: Vec<u8> = (0..256)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let _ = dec.decode_pictures(&buf, 0);
    }
    let _ = dec.flush_pictures();
}
