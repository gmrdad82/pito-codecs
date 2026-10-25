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

use std::path::Path;

use pito_codecs::media::{Prefs, decode_alpha, first_frame, probe};
use testref::{Refs, sha256};

fn ours(path: &Path) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut alpha = 0;
    for got in decode_alpha(path, 0.0).unwrap() {
        let (f, a) = got.unwrap();
        for p in &f.planes {
            for y in 0..p.height {
                out.extend_from_slice(&p.data[y * p.stride..y * p.stride + p.width]);
            }
        }
        let a = a.expect("every frame carries alpha");
        alpha += 1;
        for y in 0..a.height {
            out.extend_from_slice(&a.data[y * a.stride..y * a.stride + a.width]);
        }
    }
    (out, alpha)
}

#[test]
fn alpha_webm_round_trips_and_matches_libvpx_both_ways() {
    let r = Refs::open(refs::AREA);
    let (w, h) = refs::SIZE;
    let out = testref::scratch(refs::AREA).join(format!("{}.webm", refs::OURS));
    assert_eq!(refs::export_ours(&out).unwrap(), refs::FRAMES);
    assert!(probe(&out, &Prefs::default()).unwrap().alpha);
    let (yuva, alpha) = ours(&out);
    assert_eq!(alpha as u64, refs::FRAMES);
    let size = (w * h) as usize;
    let frame = size * 5 / 2;
    let mut worst = f64::MAX;
    for n in 0..refs::FRAMES {
        let at = n as usize * frame + size * 3 / 2;
        let src = refs::source(n);
        let mse = yuva[at..at + size]
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
    r.assert_digest(&format!("{}.yuva", refs::OURS), &sha256(&yuva));

    let theirs = r.path(&format!("{}.webm", refs::LIBVPX));
    assert!(probe(&theirs, &Prefs::default()).unwrap().alpha);
    let (got, alpha) = ours(&theirs);
    assert_eq!(alpha as u64, refs::LIBVPX_FRAMES);
    r.assert_digest(&format!("{}.yuva", refs::LIBVPX), &sha256(&got));
}

#[test]
fn the_pinned_alpha_webm_is_what_the_recorder_makes() {
    let r = Refs::open(refs::AREA);
    let webm = refs::ours_webm().unwrap();
    r.assert_digest(&format!("{}.webm", refs::OURS), &sha256(&webm));
    let (w, h) = refs::SIZE;
    let bytes = refs::source_bytes(refs::LIBVPX_FRAMES);
    assert_eq!(bytes.len() as u64, u64::from(w * h * 4) * refs::LIBVPX_FRAMES);
    assert_eq!(&bytes[..(w * h * 4) as usize], refs::source(0).data.as_slice());
}
