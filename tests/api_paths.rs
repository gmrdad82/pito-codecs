#![allow(deprecated)]

use pito_codecs::media::{self, Bitstream, Rate, Rgba};
use pito_codecs::mp4::AvcConfig;
use pito_codecs::{filter, gpu, h264, webp};

#[test]
fn old_and_new_paths_are_the_same_types() {
    let a: Rgba = filter::Rgba::new(2, 1);
    let b: webp::Rgba = a.clone();
    let c: filter::Rgba = b;
    assert_eq!(a, c);
    assert_eq!(a.stride(), 8);
    let lit = filter::Rgba {
        width: 1,
        height: 1,
        data: vec![1, 2, 3, 4],
    };
    let _: Rgba = lit;

    let r: Rate = h264::Rate::Crf(20);
    let g: gpu::Rate = r;
    assert_eq!(g, Rate::Crf(20));
    assert!(matches!(
        gpu::Rate::Vbr {
            kbps: 1,
            max_kbps: 2
        },
        Rate::Vbr { .. }
    ));
    match h264::Rate::Qp(3) {
        Rate::Qp(3) => {}
        _ => panic!(),
    }

    let s: Bitstream = h264::Bitstream::AnnexB;
    let t: gpu::Bitstream = s;
    assert_eq!(t, Bitstream::AnnexB);
    assert_eq!(gpu::Bitstream::LengthPrefixed, Bitstream::LengthPrefixed);

    let hc: h264::EncoderConfig = h264::EncoderConfig {
        rate: gpu::Rate::Qp(23),
        bitstream: gpu::Bitstream::AnnexB,
        ..Default::default()
    };
    assert_eq!(hc.rate, Rate::Qp(23));
    let gc = gpu::EncoderConfig {
        rate: h264::Rate::Crf(18),
        bitstream: h264::Bitstream::LengthPrefixed,
        ..Default::default()
    };
    assert_eq!(gc.rate, Rate::Crf(18));
}

#[test]
fn bitstream_helpers_are_public() {
    let mut out = Vec::new();
    media::length_prefixed_to_annexb(&mut out, &[0, 0, 0, 2, 9, 8, 0, 0, 0, 1, 7], 4);
    assert_eq!(out, [0, 0, 0, 1, 9, 8, 0, 0, 0, 1, 7]);

    let cfg = AvcConfig::new(vec![vec![0x67, 1, 2, 3]], vec![vec![0x68, 4]]);
    assert_eq!(
        cfg.parameter_sets_annexb(),
        [0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 0, 1, 0x68, 4]
    );

    assert!((1..=16).contains(&media::threads()));
}
