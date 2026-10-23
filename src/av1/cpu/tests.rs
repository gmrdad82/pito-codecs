use super::{Decoder, Picture};
use crate::av1::parser::Parser;
use crate::av1::tests::{
    Stats, obus, payload_start, sequence_payload, shortened, small_stream, walk,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn ffmpeg_has(what: &[&str]) -> bool {
    let list = |kind: &str| {
        Command::new("ffmpeg")
            .args(["-hide_banner", kind])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let all = list("-encoders") + &list("-decoders");
    what.iter().all(|w| all.contains(w))
}

fn ff(args: &[&str]) -> Result<(), String> {
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).into_owned())
    }
}

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

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fixtures/av1");
    std::fs::create_dir_all(&d).unwrap();
    d
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

struct Stream {
    name: &'static str,
    source: &'static str,
    frames: usize,
    pix_fmt: &'static str,
    encoder: &'static str,
    args: &'static [&'static str],
    tools: &'static [Tool],
}

const MOTION: &str = "testsrc2=size=128x96:rate=30";
const SCREEN: &str = "testsrc=size=128x96:rate=30";

const INTER: &[&str] = &[
    "-cpu-used",
    "2",
    "-crf",
    "36",
    "-lag-in-frames",
    "8",
    "-auto-alt-ref",
    "1",
    "-aom-params",
    "enable-masked-comp=1:enable-interinter-wedge=1:enable-diff-wtd-comp=1:\
     enable-dist-wtd-comp=1:enable-interintra-comp=1:enable-interintra-wedge=1:\
     enable-smooth-interintra=1:enable-onesided-comp=1:enable-obmc=1:\
     enable-dual-filter=1:enable-global-motion=1:enable-warped-motion=1:\
     enable-intra-edge-filter=1:enable-filter-intra=1:enable-cdef=1",
];

const PALETTE: &[&str] = &[
    "-cpu-used",
    "1",
    "-crf",
    "30",
    "-g",
    "1",
    "-aom-params",
    "tune-content=screen:enable-palette=1:enable-intrabc=1:enable-cfl-intra=1",
];

const SVT_SCREEN: &[&str] = &[
    "-preset",
    "4",
    "-crf",
    "40",
    "-g",
    "8",
    "-svtav1-params",
    "scm=1",
];

const PALETTE_INTER: &[&str] = &[
    "-cpu-used",
    "2",
    "-crf",
    "34",
    "-aom-params",
    "tune-content=screen:enable-palette=1:enable-intra-edge-filter=1:enable-filter-intra=1",
];

const RAV1E: &[&str] = &["-speed", "6", "-qp", "120", "-tiles", "2"];

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

fn encode(s: &Stream) -> Option<(Vec<u8>, Vec<u8>)> {
    if !ffmpeg_has(&[s.encoder, "libdav1d"]) {
        eprintln!(
            "SKIPPED {}: ffmpeg with {} and libdav1d not found",
            s.name, s.encoder
        );
        return None;
    }
    let d = dir();
    let ivf = crate::fixture::unique(&d.join(format!("{}.ivf", s.name)));
    let yuv = crate::fixture::unique(&d.join(format!("{}.yuv", s.name)));
    let (ivf_s, yuv_s) = (ivf.to_str().unwrap(), yuv.to_str().unwrap());
    let frames = s.frames.to_string();
    let mut args = vec!["-f", "lavfi", "-i", s.source, "-frames:v", &frames];
    args.extend(["-pix_fmt", s.pix_fmt, "-c:v", s.encoder]);
    args.extend(s.args);
    args.extend(["-f", "ivf", ivf_s]);
    let encoded = ff(&args);
    let decoded = encoded.clone().and_then(|_| {
        ff(&[
            "-c:v",
            "libdav1d",
            "-i",
            ivf_s,
            "-noautoscale",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            yuv_s,
        ])
    });
    let data = std::fs::read(&ivf).unwrap_or_default();
    let reference = std::fs::read(&yuv).unwrap_or_default();
    let _ = std::fs::remove_file(&ivf);
    let _ = std::fs::remove_file(&yuv);
    if let Err(e) = encoded {
        panic!("{}: {} refused the settings: {e}", s.name, s.encoder);
    }
    if let Err(e) = decoded {
        panic!("{}: libdav1d could not decode the stream: {e}", s.name);
    }
    Some((data, reference))
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

fn bit_exact(s: &Stream) {
    let Some((data, reference)) = encode(s) else {
        return;
    };
    let mut parser = Parser::default();
    let mut stats = Stats::default();
    for (p, _) in ivf_packets(&data) {
        walk(&mut parser, &p, &mut stats).unwrap_or_else(|e| panic!("{}: {e}", s.name));
    }
    for &tool in s.tools {
        assert!(
            has(&stats, tool),
            "{}: the encoder never used {tool:?}: {stats:?}",
            s.name
        );
    }
    for threads in [1, 4] {
        let pics = decode_all(&data, threads)
            .unwrap_or_else(|e| panic!("{} threads {threads}: {e}", s.name));
        assert_eq!(pics.len(), s.frames, "{} threads {threads}", s.name);
        let mut at = 0;
        for (i, p) in pics.iter().enumerate() {
            let ours: Vec<u8> = p
                .planes
                .iter()
                .flat_map(|pl| pl.data.iter().copied())
                .collect();
            let want = reference.get(at..at + ours.len()).unwrap_or_else(|| {
                panic!(
                    "{} threads {threads}: frame {i} runs past libdav1d's output",
                    s.name
                )
            });
            assert!(
                ours == want,
                "{} threads {threads}: frame {i} ({}x{}) differs from libdav1d",
                s.name,
                p.width,
                p.height
            );
            at += ours.len();
        }
        assert_eq!(at, reference.len(), "{} threads {threads}", s.name);
    }
}

#[test]
fn bit_exact_against_libdav1d() {
    bit_exact(&Stream {
        name: "grain_tiles_10bit",
        source: "testsrc2=size=352x240:rate=30,noise=alls=8:allf=t",
        frames: 12,
        pix_fmt: "yuv420p10le",
        encoder: "libaom-av1",
        args: &[
            "-cpu-used",
            "8",
            "-tiles",
            "2x1",
            "-denoise-noise-level",
            "20",
            "-auto-alt-ref",
            "1",
            "-lag-in-frames",
            "8",
            "-crf",
            "32",
        ],
        tools: &[Tool::Grain, Tool::Tiles],
    });
}

#[test]
fn compound_and_masked_inter_8bit_match_libdav1d() {
    bit_exact(&Stream {
        name: "compound_8",
        source: MOTION,
        frames: 10,
        pix_fmt: "yuv420p",
        encoder: "libaom-av1",
        args: INTER,
        tools: &[Tool::Compound],
    });
}

#[test]
fn compound_and_masked_inter_10bit_match_libdav1d() {
    bit_exact(&Stream {
        name: "compound_10",
        source: MOTION,
        frames: 10,
        pix_fmt: "yuv420p10le",
        encoder: "libaom-av1",
        args: INTER,
        tools: &[Tool::Compound],
    });
}

#[test]
fn screen_content_8bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "screen_8",
        source: SCREEN,
        frames: 3,
        pix_fmt: "yuv420p",
        encoder: "libaom-av1",
        args: PALETTE,
        tools: &[Tool::Screen],
    });
}

#[test]
fn screen_content_10bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "screen_10",
        source: SCREEN,
        frames: 2,
        pix_fmt: "yuv420p10le",
        encoder: "libaom-av1",
        args: PALETTE,
        tools: &[Tool::Screen],
    });
}

#[test]
fn lossless_matches_libdav1d() {
    bit_exact(&Stream {
        name: "lossless_8",
        source: MOTION,
        frames: 2,
        pix_fmt: "yuv420p",
        encoder: "libaom-av1",
        args: &["-cpu-used", "6", "-aom-params", "lossless=1"],
        tools: &[Tool::Lossless],
    });
}

#[test]
fn restoration_8bit_from_rav1e_matches_libdav1d() {
    bit_exact(&Stream {
        name: "rav1e_8",
        source: MOTION,
        frames: 6,
        pix_fmt: "yuv420p",
        encoder: "librav1e",
        args: RAV1E,
        tools: &[Tool::Restoration, Tool::Tiles, Tool::Compound],
    });
}

#[test]
fn restoration_10bit_from_rav1e_matches_libdav1d() {
    bit_exact(&Stream {
        name: "rav1e_10",
        source: MOTION,
        frames: 6,
        pix_fmt: "yuv420p10le",
        encoder: "librav1e",
        args: RAV1E,
        tools: &[Tool::Restoration, Tool::Tiles, Tool::Compound],
    });
}

#[test]
fn superres_with_restoration_10bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "superres_10",
        source: MOTION,
        frames: 6,
        pix_fmt: "yuv420p10le",
        encoder: "libsvtav1",
        args: &[
            "-preset",
            "6",
            "-crf",
            "40",
            "-svtav1-params",
            "superres-mode=1:superres-denom=12:superres-kf-denom=12:enable-restoration=1",
        ],
        tools: &[Tool::Superres, Tool::Restoration],
    });
}

#[test]
fn scaled_references_8bit_match_libdav1d() {
    bit_exact(&Stream {
        name: "resize_8",
        source: MOTION,
        frames: 6,
        pix_fmt: "yuv420p",
        encoder: "libsvtav1",
        args: &[
            "-preset",
            "6",
            "-crf",
            "40",
            "-svtav1-params",
            "resize-mode=1:resize-denom=12:resize-kf-denom=8",
        ],
        tools: &[Tool::Resize],
    });
}

#[test]
fn svt_intrabc_and_compound_8bit_match_libdav1d() {
    bit_exact(&Stream {
        name: "svt_8",
        source: MOTION,
        frames: 8,
        pix_fmt: "yuv420p",
        encoder: "libsvtav1",
        args: SVT_SCREEN,
        tools: &[Tool::Intrabc, Tool::Compound],
    });
}

#[test]
fn svt_screen_and_compound_10bit_match_libdav1d() {
    bit_exact(&Stream {
        name: "svt_10",
        source: MOTION,
        frames: 8,
        pix_fmt: "yuv420p10le",
        encoder: "libsvtav1",
        args: SVT_SCREEN,
        tools: &[Tool::Screen, Tool::Compound],
    });
}

#[test]
fn superres_8bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "superres_8",
        source: MOTION,
        frames: 4,
        pix_fmt: "yuv420p",
        encoder: "libsvtav1",
        args: &[
            "-preset",
            "6",
            "-crf",
            "40",
            "-svtav1-params",
            "superres-mode=1:superres-denom=12:superres-kf-denom=12:enable-restoration=1",
        ],
        tools: &[Tool::Superres],
    });
}

#[test]
fn lossless_10bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "lossless_10",
        source: MOTION,
        frames: 2,
        pix_fmt: "yuv420p10le",
        encoder: "libaom-av1",
        args: &["-cpu-used", "6", "-aom-params", "lossless=1"],
        tools: &[Tool::Lossless],
    });
}

#[test]
fn palette_in_inter_frames_10bit_matches_libdav1d() {
    bit_exact(&Stream {
        name: "screen_inter_10",
        source: SCREEN,
        frames: 4,
        pix_fmt: "yuv420p10le",
        encoder: "libaom-av1",
        args: PALETTE_INTER,
        tools: &[Tool::Screen],
    });
}

fn first_unit() -> Option<Vec<(Vec<u8>, i64)>> {
    let data = small_stream("cpu-malformed")?;
    let packets = ivf_packets(&data);
    assert_eq!(packets.len(), 3);
    Some(packets)
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
    let Some(packets) = first_unit() else {
        return;
    };
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
    let Some(packets) = first_unit() else {
        return;
    };
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
    let Some(packets) = first_unit() else {
        return;
    };
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
    let Some(packets) = first_unit() else {
        return;
    };
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
    let Some(packets) = first_unit() else {
        return;
    };
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
