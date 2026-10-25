use super::Decoder;
use crate::syntax::bits::BitWriter;
use crate::testref::{Digest, Refs, sha256};

#[allow(dead_code)]
#[path = "refs_synth.rs"]
mod refs_synth;

use refs_synth::*;

fn decode_all(stream: &[u8], threads: usize) -> Vec<crate::media::Frame> {
    let mut dec = Decoder::with_threads(threads);
    let packet = crate::media::Packet {
        data: stream.to_vec(),
        pts: 0,
        dts: 0,
        keyframe: true,
    };
    let mut pics = dec.decode(&packet).expect("decode");
    pics.extend(dec.flush().expect("flush"));
    pics
}

fn digest(pics: &[crate::media::Frame]) -> String {
    let mut d = Digest::new();
    for p in pics {
        d.frame(p);
    }
    d.hex()
}

fn exact(name: &str, tiles: Tiles, segments: Vec<Segment>) {
    let (stream, expect) = sequence(&tiles, true, segments);
    for threads in [1, 4] {
        let pics = decode_all(&stream, threads);
        assert_eq!(pics.len(), expect.len(), "{name}");
        for (i, (p, e)) in pics.iter().zip(&expect).enumerate() {
            assert!(p.planes[0].data == e.y, "{name}: picture {i} luma");
            assert!(
                p.planes[1].data == e.u && p.planes[2].data == e.v,
                "{name}: picture {i} chroma"
            );
        }
    }
}

fn deblocked_like_ffmpeg(name: &str) {
    let r = Refs::open(AREA);
    let stream = refs_synth::stream(name).unwrap();
    r.assert_digest(&file(name), &sha256(&stream));
    let pictures = sequence(&None, true, whole(WC * HC)).1.len();
    for threads in [1, 4] {
        let pics = decode_all(&stream, threads);
        assert_eq!(pics.len(), pictures, "{name} ({threads} threads)");
        r.assert_digest(&decoded(name), &digest(&pics));
    }
}

#[test]
fn pcm_long_term_refs_list_modification() {
    exact("pcm_lt", None, whole(WC * HC));
}

#[test]
fn pcm_deblocked_single_tile() {
    deblocked_like_ffmpeg("pcm_dbk");
}

#[test]
fn deblocking_changes_the_pcm_pictures() {
    let stream = refs_synth::stream("pcm_dbk").unwrap();
    let pics = decode_all(&stream, 1);
    let source = content(0);
    assert_eq!(pics[0].planes[0].data.len(), source.y.len());
    assert!(pics[0].planes[0].data != source.y);
}

#[test]
fn tiles_uniform_exact() {
    exact(
        "tiles_uniform",
        Some((vec![2, 2, 2], vec![2, 2], true, true)),
        whole(WC * HC),
    );
}

#[test]
fn tiles_explicit_filter_across() {
    deblocked_like_ffmpeg("tiles_across");
}

#[test]
fn tiles_explicit_no_filter_across() {
    deblocked_like_ffmpeg("tiles_no_across");
}

#[test]
fn tiles_slices_and_dependent_segments() {
    deblocked_like_ffmpeg("tiles_slices");
}

#[test]
fn slices_without_tiles() {
    deblocked_like_ffmpeg("slices");
}

#[test]
fn row_aligned_slices_decode_in_parallel() {
    deblocked_like_ffmpeg("row_slices");
}

#[test]
fn every_deblocked_case_has_a_stream_and_unknown_ones_are_refused() {
    for name in DEBLOCKED {
        let d = deblocked(name).unwrap();
        assert_eq!(d.name, name);
        assert_eq!(d.segments.iter().map(|s| s.len).sum::<usize>(), WC * HC);
        let s = refs_synth::stream(name).unwrap();
        assert!(s.starts_with(&vps()), "{name}");
        assert_eq!(file(name), format!("synth_{name}.hevc"));
        assert_eq!(decoded(name), format!("synth_{name}.yuv"));
    }
    assert!(refs_synth::stream("pcm_lt").is_err());
    assert!(deblocked("nothing").is_none());
}

#[test]
fn conformance_window_past_the_picture_stays_inside_it() {
    let (stream, expect) = sequence(&None, true, whole(WC * HC));
    let plain = sps(true);
    let at = stream
        .windows(plain.len())
        .position(|w| w == plain.as_slice())
        .unwrap();
    let far = (W as u32 - 2) / 2;
    let window = [far, (1 << 31) - far, 0, 0];
    let mut s = stream[..at].to_vec();
    s.extend(sps_with(true, Some(window)));
    s.extend_from_slice(&stream[at + plain.len()..]);
    for threads in [1, 4] {
        let pics = decode_all(&s, threads);
        assert_eq!(pics.len(), expect.len());
        for p in &pics {
            assert!(p.width >= 1 && p.width <= W as u32 && p.height == H as u32);
            assert!(p.planes[0].data.len() >= p.width as usize);
        }
    }
}

#[test]
fn pictures_above_the_largest_level_are_refused() {
    let parse = |width, height| {
        let n = sps_sized(false, None, width, height);
        crate::syntax::hevc::params::Sps::parse(&crate::syntax::bits::unescape_rbsp(&n[6..]))
    };
    assert!(parse(W as u32, H as u32).is_ok());
    assert!(parse(8192, 4352).is_ok());
    assert!(parse(8192, 4368).is_err());
    assert!(parse(16384, 16384).is_err());
}

fn packet(data: Vec<u8>) -> crate::media::Packet {
    crate::media::Packet {
        data,
        pts: 0,
        dts: 0,
        keyframe: true,
    }
}

fn first_error(stream: &[u8], threads: usize) -> Option<String> {
    let mut dec = Decoder::with_threads(threads);
    let a = dec.decode(&packet(stream.to_vec())).err();
    let b = dec.flush().err();
    a.or(b).map(|e| e.to_string())
}

fn replaced(stream: &[u8], part: &[u8], with: &[u8]) -> Vec<u8> {
    let at = stream
        .windows(part.len())
        .position(|w| w == part)
        .expect("part not in the stream");
    [&stream[..at], with, &stream[at + part.len()..]].concat()
}

fn params() -> Vec<u8> {
    [vps(), sps(true), pps(0, &None, false), pps(1, &None, true)].concat()
}

fn expect_error(name: &str, stream: &[u8], wording: &str) {
    for threads in [1, 4] {
        let e = first_error(stream, threads)
            .unwrap_or_else(|| panic!("{name} ({threads} threads): decoded without an error"));
        assert!(
            e.contains(wording) && !e.contains("internal error"),
            "{name} ({threads} threads): {e}"
        );
    }
}

#[test]
fn a_pps_naming_a_missing_sps_is_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    expect_error(
        "no SPS",
        &replaced(&stream, &sps(true), &[]),
        "missing parameter set: SPS 0",
    );
}

#[test]
fn a_slice_before_any_parameter_set_is_an_error_and_the_decoder_recovers() {
    let (stream, expect) = sequence(&None, true, whole(WC * HC));
    let head = params();
    assert!(stream.starts_with(&head));
    let slices = stream[head.len()..].to_vec();
    expect_error("slices first", &slices, "missing parameter set: PPS 0");
    for threads in [1, 4] {
        let mut dec = Decoder::with_threads(threads);
        assert!(dec.decode(&packet(slices.clone())).is_err());
        let mut pics = dec
            .decode(&packet(stream.clone()))
            .expect("decode after the error");
        pics.extend(dec.flush().expect("flush after the error"));
        assert_eq!(pics.len(), expect.len(), "{threads} threads");
        for (i, (p, e)) in pics.iter().zip(&expect).enumerate() {
            for (plane, want) in [&e.y, &e.u, &e.v].into_iter().enumerate() {
                assert!(
                    &p.planes[plane].data == want,
                    "{threads} threads: picture {i}, plane {plane}"
                );
            }
        }
    }
}

fn slice_naming(pps_id: u32) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bit(false);
    w.write_ue(pps_id);
    w.write_bits(0, 7);
    w.rbsp_trailing();
    nal(19, &w.finish())
}

#[test]
fn out_of_range_syntax_values_are_errors() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let good = sps(true);
    for (name, width, wording) in [
        ("zero width", 0, "out of range"),
        ("width off the CB grid", W as u32 + 4, "MinCbSizeY"),
    ] {
        let bad = sps_sized(true, None, width, H as u32);
        expect_error(name, &replaced(&stream, &good, &bad), wording);
    }
    expect_error(
        "PPS id 64",
        &replaced(&stream, &pps(0, &None, false), &pps(64, &None, false)),
        "pps_pic_parameter_set_id out of range",
    );
    let head = params();
    expect_error(
        "slice PPS id 64",
        &[head.clone(), slice_naming(64)].concat(),
        "slice_pic_parameter_set_id out of range",
    );
    expect_error(
        "slice naming an absent PPS",
        &[head.clone(), slice_naming(7)].concat(),
        "missing parameter set: PPS 7",
    );
    let mut forbidden = slice_naming(0);
    forbidden[4] |= 0x80;
    expect_error(
        "forbidden_zero_bit",
        &[head.clone(), forbidden].concat(),
        "forbidden_zero_bit",
    );
    let mut tid0 = slice_naming(0);
    tid0[5] = 0;
    expect_error(
        "temporal id 0",
        &[head, tid0].concat(),
        "nuh_temporal_id_plus1",
    );
}

#[test]
fn truncated_synthetic_nal_units_fail_without_panics() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let nals: Vec<Vec<u8>> = crate::syntax::bits::annexb_nals(&stream)
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();
    for (k, n) in nals.iter().enumerate() {
        for cut in [2, n.len() / 2, n.len() - 1] {
            if cut < 2 || cut >= n.len() {
                continue;
            }
            let mut s = Vec::new();
            for (j, m) in nals.iter().enumerate() {
                s.extend([0, 0, 0, 1]);
                s.extend_from_slice(if j == k { &m[..cut] } else { m });
            }
            for threads in [1, 4] {
                if let Some(e) = first_error(&s, threads) {
                    assert!(
                        !e.contains("internal error"),
                        "NAL {k} cut to {cut} ({threads} threads): {e}"
                    );
                } else {
                    assert!(
                        cut > n.len() / 2 || k > 3,
                        "NAL {k} cut to {cut} ({threads} threads) decoded without an error"
                    );
                }
            }
        }
    }
}

#[test]
fn out_of_range_sps_values_are_errors() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let good = sps(true);
    for (field, value, wording) in [
        ("chroma_format_idc", 4, "chroma_format_idc out of range"),
        ("chroma_format_idc", 2, "only 4:2:0"),
        ("bit_depth_luma_minus8", 4, "bit depth 12/8"),
        ("bit_depth_luma_minus8", 9, "bit depth out of range"),
        (
            "log2_max_pic_order_cnt_lsb_minus4",
            13,
            "log2_max_pic_order_cnt_lsb_minus4 out of range",
        ),
        (
            "log2_min_luma_coding_block_size_minus3",
            4,
            "CTB size out of range",
        ),
        (
            "log2_diff_max_min_luma_coding_block_size",
            4,
            "CTB size out of range",
        ),
        (
            "num_short_term_ref_pic_sets",
            65,
            "num_short_term_ref_pic_sets out of range",
        ),
    ] {
        let bad = sps_tuned(true, None, W as u32, H as u32, (field, value));
        expect_error(
            &format!("{field} {value}"),
            &replaced(&stream, &good, &bad),
            wording,
        );
    }
}

#[test]
fn a_slice_segment_address_past_the_picture_is_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    assert_eq!(WC * HC, 24);
    let mut w = BitWriter::new();
    w.write_bit(false);
    w.write_ue(0);
    w.write_bit(false);
    w.write_bits(31, 5);
    w.write_ue(2);
    w.rbsp_trailing();
    expect_error(
        "slice_segment_address 31 of 24",
        &[stream, nal(1, &w.finish())].concat(),
        "slice_segment_address out of range",
    );
}

#[test]
fn sixteen_active_references_are_an_error() {
    let (stream, _) = sequence(&None, true, whole(WC * HC));
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_ue(0);
    w.write_ue(1);
    w.write_bits(1, 8);
    w.write_bit(false);
    w.write_ue(1);
    w.write_ue(0);
    w.write_ue(0);
    w.write_bit(true);
    w.write_ue(0);
    w.write_bit(true);
    w.write_ue(15);
    w.write_ue(0);
    w.write_se(0);
    w.rbsp_trailing();
    expect_error(
        "num_ref_idx_l0_active_minus1 15",
        &[stream, nal(1, &w.finish())].concat(),
        "num_ref_idx_active out of range",
    );
}

#[test]
fn more_entry_points_than_ctbs_are_an_error() {
    let tiles: Tiles = Some((vec![3, 3], vec![2, 2], false, true));
    let mut w = BitWriter::new();
    w.write_bit(true);
    w.write_bit(false);
    w.write_ue(0);
    w.write_ue(2);
    w.write_se(0);
    w.write_bit(true);
    w.write_ue(WC as u32 * HC as u32 + 1);
    w.write_ue(0);
    for _ in 0..=WC * HC {
        w.write_bit(false);
    }
    w.rbsp_trailing();
    let stream = [
        vps(),
        sps(true),
        pps(0, &tiles, false),
        nal(19, &w.finish()),
    ]
    .concat();
    expect_error(
        "num_entry_point_offsets 25 of 24",
        &stream,
        "num_entry_point_offsets out of range",
    );
}
