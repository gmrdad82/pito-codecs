#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]

#[path = "../src/testref.rs"]
mod testref;

#[allow(dead_code)]
#[path = "../src/vp9/refs_alpha.rs"]
mod refs;

#[allow(dead_code)]
#[path = "../src/vp9/refs.rs"]
mod vp9_refs;

use std::path::Path;

use pito_codecs::media::{Packet, Prefs, TimeBase, decode_alpha, first_frame, probe};
use pito_codecs::mkv::{Options, TrackConfig, Writer};
use testref::{Digest, Refs, sha256};

struct Decoded {
    colour: String,
    alpha: String,
    frames: usize,
    alpha_planes: Vec<u8>,
}

fn ours(path: &Path) -> Decoded {
    let (mut colour, mut alpha) = (Digest::new(), Digest::new());
    let mut out = Decoded {
        colour: String::new(),
        alpha: String::new(),
        frames: 0,
        alpha_planes: Vec::new(),
    };
    for got in decode_alpha(path, 0.0).unwrap() {
        let (f, a) = got.unwrap();
        colour.frame(&f);
        let a = a.expect("every frame carries alpha");
        out.frames += 1;
        for y in 0..a.height {
            let row = &a.data[y * a.stride..y * a.stride + a.width];
            alpha.bytes(row);
            out.alpha_planes.extend_from_slice(row);
        }
    }
    out.colour = colour.hex();
    out.alpha = alpha.hex();
    out
}

#[test]
fn our_alpha_webm_round_trips_and_vpxdec_decodes_both_its_streams_like_ours() {
    let r = Refs::open(refs::AREA);
    let (w, h) = refs::SIZE;
    let out = testref::scratch(refs::AREA).join(format!("{}.webm", refs::OURS));
    assert_eq!(refs::export_ours(&out).unwrap(), refs::FRAMES);
    assert!(probe(&out, &Prefs::default()).unwrap().alpha);
    let got = ours(&out);
    assert_eq!(got.frames as u64, refs::FRAMES);
    let size = (w * h) as usize;
    let mut worst = f64::MAX;
    for n in 0..refs::FRAMES {
        let at = n as usize * size;
        let src = refs::source(n);
        let mse = got.alpha_planes[at..at + size]
            .iter()
            .zip(src.data.chunks_exact(4))
            .map(|(&a, px)| (a as f64 - px[3] as f64).powi(2))
            .sum::<f64>()
            / size as f64;
        worst = worst.min(10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10());
    }
    assert!(worst > 35.0, "alpha PSNR {worst:.2} dB");
    let still = first_frame(&out, 0, &Prefs::default()).unwrap();
    assert!(still.data.chunks_exact(4).any(|p| p[3] < 16));
    assert!(still.data.chunks_exact(4).any(|p| p[3] > 240));
    r.assert_digest(
        &format!("{}.webm", refs::OURS),
        &sha256(&std::fs::read(&out).unwrap()),
    );

    let (colour, alpha) = refs::webm_streams(&out).unwrap();
    assert_eq!((colour.len(), alpha.len()), (got.frames, got.frames));
    r.assert_digest(
        &vp9_refs::stream(refs::OURS),
        &sha256(&vp9_refs::ivf(w, h, &colour)),
    );
    r.assert_digest(
        &vp9_refs::alpha_stream(refs::OURS),
        &sha256(&vp9_refs::ivf(w, h, &alpha)),
    );
    r.assert_digest(&vp9_refs::decoded(refs::OURS), &got.colour);
    r.assert_digest(&vp9_refs::alpha_decoded(refs::OURS), &got.alpha);
}

fn mux(colour: &[u8], alpha: &[u8], out: &Path) {
    let (w, h) = refs::SIZE;
    let mut cfg = TrackConfig::vp9(w, h, TimeBase::new(1, 24));
    cfg.alpha = true;
    let mut writer = Writer::create_with(out, Options::webm()).unwrap();
    let track = writer.add_track(cfg).unwrap();
    let colour = vp9_refs::ivf_frames(colour);
    let alpha = vp9_refs::ivf_frames(alpha);
    assert_eq!(
        colour.len(),
        alpha.len(),
        "one alpha frame per colour frame"
    );
    for (i, (c, a)) in colour.iter().zip(&alpha).enumerate() {
        let packet = Packet {
            data: c.to_vec(),
            pts: i as i64,
            dts: i as i64,
            keyframe: pito_codecs::vp9::is_keyframe(c),
        };
        writer.push_alpha(track, &packet, Some(*a)).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn a_libvpx_alpha_pair_in_webm_decodes_bit_exact_with_vpxdec() {
    let r = Refs::open(refs::AREA);
    let out = testref::scratch(refs::AREA).join(format!("{}.webm", refs::LIBVPX));
    mux(
        &r.bytes(&vp9_refs::stream(refs::LIBVPX)),
        &r.bytes(&vp9_refs::alpha_stream(refs::LIBVPX)),
        &out,
    );
    assert!(probe(&out, &Prefs::default()).unwrap().alpha);
    let got = ours(&out);
    assert_eq!(got.frames as u64, refs::LIBVPX_FRAMES);
    r.assert_digest(&vp9_refs::decoded(refs::LIBVPX), &got.colour);
    r.assert_digest(&vp9_refs::alpha_decoded(refs::LIBVPX), &got.alpha);
}

#[test]
fn the_pinned_alpha_webm_is_what_the_recorder_makes() {
    let r = Refs::open(refs::AREA);
    let webm = refs::ours_webm().unwrap();
    r.assert_digest(&format!("{}.webm", refs::OURS), &sha256(&webm));
    let (w, h) = refs::SIZE;
    let bytes = refs::source_bytes(refs::LIBVPX_FRAMES);
    assert_eq!(
        bytes.len() as u64,
        u64::from(w * h * 4) * refs::LIBVPX_FRAMES
    );
    assert_eq!(
        &bytes[..(w * h * 4) as usize],
        refs::source(0).data.as_slice()
    );
}

#[test]
fn a_webm_with_no_alpha_or_no_file_is_refused() {
    let dir = testref::scratch(refs::AREA);
    assert!(refs::webm_streams(&dir.join("missing.webm")).is_err());
    let bare = dir.join("bare.webm");
    let (w, h) = refs::SIZE;
    let mut writer = Writer::create_with(&bare, Options::webm()).unwrap();
    let track = writer
        .add_track(TrackConfig::vp9(w, h, TimeBase::new(1, 24)))
        .unwrap();
    let r = Refs::open(refs::AREA);
    let colour = r.bytes(&vp9_refs::stream(refs::LIBVPX));
    let first = vp9_refs::ivf_frames(&colour)[0];
    writer.push_sample(track, first, 0, Some(1), true).unwrap();
    writer.finish().unwrap();
    let e = refs::webm_streams(&bare).unwrap_err();
    assert!(e.contains("has no alpha"), "{e}");
}
