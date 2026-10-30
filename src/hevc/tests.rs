use super::{Decoder, Hvcc};
use crate::media::{Frame, Packet, PixelFormat};
use crate::syntax::bits::{annexb_nals, unescape_rbsp};
use crate::syntax::hevc::params::{Pps, ScalingList, Sps};
use crate::syntax::hevc::slice::{NalHeader, SliceHeader};
use crate::testref::{Digest, Refs};
use std::sync::Arc;
use std::time::Instant;

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

use refs::X265;

fn stream(r: &Refs, f: &X265) -> Vec<u8> {
    r.bytes(&refs::stream(f.name))
}

fn format_of(f: &X265) -> PixelFormat {
    if f.ten_bit {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    }
}

fn digest(frames: &[Frame]) -> String {
    let mut d = Digest::new();
    for f in frames {
        d.frame(f);
    }
    d.hex()
}

fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut aus = Vec::new();
    let mut au_start = 0;
    let mut seen_vcl = false;
    for (sc, payload) in starts {
        if payload + 2 >= data.len() {
            continue;
        }
        let t = (data[payload] >> 1) & 0x3f;
        let first_slice = t < 32 && data[payload + 2] & 0x80 != 0;
        if seen_vcl && (matches!(t, 32..=35 | 39) || first_slice) {
            aus.push(&data[au_start..sc]);
            au_start = sc;
            seen_vcl = false;
        }
        seen_vcl |= t < 32;
    }
    if au_start < data.len() {
        aus.push(&data[au_start..]);
    }
    aus
}

fn packet(data: Vec<u8>, i: usize) -> Packet {
    Packet {
        data,
        pts: i as i64,
        dts: i as i64,
        keyframe: false,
    }
}

fn decode_units(dec: &mut Decoder, units: &[Vec<u8>]) -> Vec<Frame> {
    let mut out = Vec::new();
    for (i, u) in units.iter().enumerate() {
        out.extend(
            dec.decode(&packet(u.clone(), i))
                .unwrap_or_else(|e| panic!("access unit {i}: {e}")),
        );
    }
    out.extend(dec.flush().unwrap());
    out
}

fn check_shape(name: &str, frames: &[Frame], f: &X265) {
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(
            (frame.format, frame.width, frame.height),
            (format_of(f), f.size.0, f.size.1),
            "{name}: frame {i}"
        );
    }
    let mut pts: Vec<i64> = frames.iter().map(|f| f.pts).collect();
    pts.sort_unstable();
    pts.dedup();
    assert_eq!(pts.len(), frames.len(), "{name}: pts lost in reordering");
}

fn compare(r: &Refs, name: &str, frames: &[Frame], f: &X265) {
    assert_eq!(frames.len() as u64, f.frames, "{name}: frame count");
    check_shape(name, frames, f);
    r.assert_digest(&refs::recon(f.name), &digest(frames));
}

#[test]
fn bit_exact_against_the_x265_reconstruction() {
    let r = Refs::open(refs::AREA);
    for f in &refs::CONFORMANCE {
        let units = units_of(&stream(&r, f));
        for threads in [1, 4] {
            let frames = decode_units(&mut Decoder::with_threads(threads), &units);
            compare(&r, &format!("{} ({threads} threads)", f.name), &frames, f);
        }
    }
}

fn parameter_sets(data: &[u8], kind: u8) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    for nal in annexb_nals(data) {
        if (nal[0] >> 1) & 0x3f == kind && !out.iter().any(|n| n == nal) {
            out.push(nal.to_vec());
        }
    }
    out
}

#[test]
fn hvcc_carries_the_x265_parameter_sets_and_drives_length_prefixed_decode() {
    let r = Refs::open(refs::AREA);
    for name in refs::HVCC {
        let f = refs::find(name).unwrap();
        let data = stream(&r, f);
        let ours = Hvcc::parse(&Hvcc::from_annexb(&data).unwrap().to_bytes()).unwrap();
        let intra = f.params.split(':').any(|p| p == "keyint=1");
        let (profile, depth) = match (intra, f.ten_bit) {
            (true, true) => (4, 10),
            (true, false) => (4, 8),
            (false, true) => (2, 10),
            (false, false) => (1, 8),
        };
        assert_eq!(
            (
                ours.profile_idc(),
                ours.chroma_format,
                ours.bit_depth_luma,
                ours.bit_depth_chroma
            ),
            (profile, 1, depth, depth),
            "{name}"
        );
        assert_eq!(
            (&ours.vps, &ours.sps, &ours.pps),
            (
                &parameter_sets(&data, 32),
                &parameter_sets(&data, 33),
                &parameter_sets(&data, 34)
            ),
            "{name}"
        );
        assert_eq!(ours.nal_length_size, 4, "{name}");
        assert_eq!((ours.width, ours.height), f.size);
        let units: Vec<Vec<u8>> = access_units(&data)
            .into_iter()
            .map(|au| {
                let mut sample = Vec::new();
                for nal in annexb_nals(au)
                    .into_iter()
                    .filter(|n| !matches!((n[0] >> 1) & 0x3f, 32..=34))
                {
                    sample.extend((nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(nal);
                }
                sample
            })
            .collect();
        let mut dec = Decoder::from_hvcc(&ours.to_bytes()).unwrap();
        let frames = decode_units(&mut dec, &units);
        compare(&r, &format!("{name} via hvcC"), &frames, f);
    }
}

fn xorshift(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn damaged_streams_fail_without_panics() {
    let r = Refs::open(refs::AREA);
    for name in ["medium", "bframes", "intra_main10"] {
        let data = stream(&r, refs::find(name).unwrap());
        let mut seed = 0x1234_5678_9abc_def0u64 ^ name.len() as u64;
        for round in 0..8 {
            let mut d = data.clone();
            for _ in 0..(1 + round * 3) {
                let pos = 100 + (xorshift(&mut seed) as usize) % (d.len() - 100);
                d[pos] ^= 1 << (xorshift(&mut seed) % 8);
            }
            if round % 4 == 3 {
                d.truncate(d.len() * 2 / 3);
            }
            for threads in [1, 4] {
                let mut dec = Decoder::with_threads(threads);
                for (i, au) in access_units(&d).into_iter().enumerate() {
                    let _ = dec.decode(&packet(au.to_vec(), i));
                }
                let _ = dec.flush();
            }
        }
    }
    let mut seed = 42u64;
    for len in [0usize, 1, 5, 100, 5000] {
        for _ in 0..20 {
            let mut g: Vec<u8> = (0..len).map(|_| xorshift(&mut seed) as u8).collect();
            if len > 10 {
                g[..4].copy_from_slice(&[0, 0, 0, 1]);
            }
            let mut dec = Decoder::new();
            let _ = dec.decode(&packet(g, 0));
            let _ = dec.flush();
        }
    }
    assert!(Decoder::from_hvcc(&[1, 2, 3]).is_err());
    assert!(Hvcc::parse(&[1; 30]).is_err());
}

#[test]
#[ignore]
fn decode_speed() {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(16);
    let threads: Vec<usize> = match std::env::var("HEVC_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
    {
        Some(t) => vec![t],
        None => vec![1, cores],
    };
    if let Ok(file) = std::env::var("HEVC_FILE") {
        time_units(&file, &std::fs::read(&file).unwrap(), &threads);
        return;
    }
    let only = std::env::var("HEVC_BENCH").ok();
    let r = Refs::open(refs::AREA);
    for f in refs::all().filter(|f| only.as_deref().is_none_or(|o| o == f.name)) {
        time_units(f.name, &stream(&r, f), &threads);
    }
}

fn time_units(name: &str, data: &[u8], threads: &[usize]) {
    let units: Vec<Vec<u8>> = access_units(data).into_iter().map(<[u8]>::to_vec).collect();
    for &threads in threads {
        let mut best = f64::MAX;
        let mut frames = 0;
        for _ in 0..3 {
            let mut dec = Decoder::with_threads(threads);
            let t0 = Instant::now();
            frames = 0;
            for (i, u) in units.iter().enumerate() {
                for f in dec.decode(&packet(u.clone(), i)).unwrap() {
                    frames += 1;
                    dec.recycle(f);
                }
            }
            frames += dec.flush().unwrap().len();
            best = best.min(t0.elapsed().as_secs_f64());
        }
        println!(
            "{} ({} KB): {threads} thread(s): {frames} frames in {best:.3}s = {:.1} fps",
            name,
            data.len() / 1024,
            frames as f64 / best
        );
    }
}

fn units_of(data: &[u8]) -> Vec<Vec<u8>> {
    access_units(data).into_iter().map(<[u8]>::to_vec).collect()
}

fn feature(name: &str) -> &'static X265 {
    refs::FEATURES
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no feature fixture named {name}"))
}

struct Facts {
    sps: Sps,
    pps: Pps,
    max_slices: usize,
    explicit_weights: bool,
}

fn facts(data: &[u8]) -> Facts {
    let (mut sps, mut pps): (Option<Arc<Sps>>, Option<Arc<Pps>>) = (None, None);
    let (mut slices, mut max_slices, mut explicit_weights) = (0, 0, false);
    let mut prev: Option<SliceHeader> = None;
    for nal in annexb_nals(data) {
        let hdr = NalHeader::parse(nal).unwrap();
        let rbsp = unescape_rbsp(&nal[2..]);
        match hdr.nal_type {
            33 => sps = Some(Arc::new(Sps::parse(&rbsp).unwrap())),
            34 => pps = Some(Arc::new(Pps::parse(&rbsp).unwrap())),
            0..=9 | 16..=21 => {
                let (p, s) = (pps.clone().unwrap(), sps.clone().unwrap());
                let (sh, _, _) =
                    SliceHeader::parse(&rbsp, hdr, |_| Ok((p, s)), prev.as_ref()).unwrap();
                if sh.first_slice_segment_in_pic {
                    slices = 0;
                }
                slices += 1;
                max_slices = max_slices.max(slices);
                explicit_weights |= sh
                    .pwt
                    .iter()
                    .flat_map(|t| t.l.iter().flatten())
                    .any(|e| e.luma_flag || e.chroma_flag);
                prev = Some(sh);
            }
            _ => {}
        }
    }
    Facts {
        sps: Arc::unwrap_or_clone(sps.expect("no SPS")),
        pps: Arc::unwrap_or_clone(pps.expect("no PPS")),
        max_slices,
        explicit_weights,
    }
}

fn assert_feature(name: &str, x: &Facts) {
    let (sps, pps) = (&x.sps, &x.pps);
    let carried: Vec<(&str, bool)> = match name {
        "wpp_crop_66x70" => vec![
            ("entropy_coding_sync", pps.entropy_coding_sync),
            ("conformance window", sps.conf_win != (0, 0, 0, 0)),
        ],
        "wpp_slices_main10" => vec![
            ("entropy_coding_sync", pps.entropy_coding_sync),
            ("several slices", x.max_slices > 1),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "long_b_runs" => vec![("reorder", sps.max_num_reorder >= 2)],
        "pyramid_weightb_main10" => vec![
            ("weighted_pred", pps.weighted_pred),
            ("weighted_bipred", pps.weighted_bipred),
            ("explicit weights", x.explicit_weights),
            ("reorder", sps.max_num_reorder >= 2),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "weightp_8bit" => vec![
            ("weighted_pred", pps.weighted_pred),
            ("explicit weights", x.explicit_weights),
        ],
        "scaling_tskip_main10" => vec![
            ("scaling_list_enabled", sps.scaling_list_enabled),
            ("transform_skip", pps.transform_skip),
            ("10-bit", sps.bit_depth_luma == 10),
        ],
        "scaling_list_file" => vec![
            ("scaling_list_enabled", sps.scaling_list_enabled),
            (
                "explicit lists",
                sps.scaling_list
                    .as_ref()
                    .or(pps.scaling_list.as_ref())
                    .is_some_and(|l| l.lists != ScalingList::default_lists().lists),
            ),
        ],
        "cu_qp_chroma_offsets" => vec![
            ("cu_qp_delta", pps.cu_qp_delta_enabled),
            ("cb_qp_offset", pps.cb_qp_offset == -4),
            ("cr_qp_offset", pps.cr_qp_offset == 5),
        ],
        "intra_tools" => vec![
            ("constrained_intra_pred", pps.constrained_intra_pred),
            ("no sign hiding", !pps.sign_data_hiding),
            ("no temporal mvp", !sps.temporal_mvp),
            ("no strong smoothing", !sps.strong_intra_smoothing),
            ("no sao", !sps.sao),
            ("amp", sps.amp),
        ],
        "cu_lossless" => vec![("transquant_bypass", pps.transquant_bypass)],
        "one_ctb_no_deblock" => vec![
            ("64x64 CTB", sps.log2_ctb == 6),
            ("deblocking off", pps.deblocking_disabled),
        ],
        _ => panic!("{name}: no feature check"),
    };
    for (what, ok) in carried {
        assert!(ok, "{name}: the stream does not carry {what}");
    }
}

#[test]
fn feature_streams_bit_exact_against_the_x265_reconstruction() {
    let r = Refs::open(refs::AREA);
    for f in &refs::FEATURES {
        let data = stream(&r, f);
        assert_feature(f.name, &facts(&data));
        let units = units_of(&data);
        for threads in [1, 4] {
            let frames = decode_units(&mut Decoder::with_threads(threads), &units);
            compare(&r, &format!("{} ({threads} threads)", f.name), &frames, f);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[test]
fn scalar_path_bit_exact_against_the_x265_reconstruction() {
    let r = Refs::open(refs::AREA);
    for f in refs::all() {
        let units = units_of(&stream(&r, f));
        super::simd::force_off(true);
        let before = super::simd::forced_checks();
        let frames = decode_units(&mut Decoder::with_threads(1), &units);
        let checks = super::simd::forced_checks() - before;
        super::simd::force_off(false);
        assert!(
            checks > 0,
            "{}: the one-thread decode never asked for SIMD on the test's thread, so the scalar path was not forced",
            f.name
        );
        compare(&r, &format!("{} (scalar)", f.name), &frames, f);
    }
}

#[test]
fn open_gop_cut_at_a_cra_drops_its_rasl_pictures() {
    let r = Refs::open(refs::AREA);
    let f = &refs::OPEN_GOP_CUT;
    let data = stream(&r, f);
    let aus = access_units(&data);
    let whole = decode_units(&mut Decoder::with_threads(1), &units_of(&data));
    compare(&r, "open_gop_cut", &whole, f);
    let cra = (1..aus.len())
        .find(|&i| first_vcl_type(aus[i]) == Some(21))
        .expect("open_gop_cut: no CRA after the first picture");
    let rasl = aus[cra + 1..]
        .iter()
        .map(|au| first_vcl_type(au))
        .take_while(|t| !matches!(t, Some(16..=23)))
        .filter(|t| matches!(t, Some(8 | 9)))
        .count();
    assert!(rasl > 0, "open_gop_cut: the cut CRA leads no RASL pictures");
    let units: Vec<Vec<u8>> = aus[cra..].iter().map(|au| au.to_vec()).collect();
    for threads in [1, 4] {
        let name = format!("open_gop_cut ({threads} threads)");
        let frames = decode_units(&mut Decoder::with_threads(threads), &units);
        assert_eq!(
            frames.len(),
            units.len() - rasl,
            "{name}: RASL pictures of the first CRA were output"
        );
        check_shape(&name, &frames, f);
        assert_eq!(
            digest(&frames),
            digest(&whole[whole.len() - frames.len()..]),
            "{name}: the pictures from the CRA on differ from the whole stream's"
        );
    }
}

fn first_vcl_type(au: &[u8]) -> Option<u8> {
    annexb_nals(au)
        .into_iter()
        .map(|n| (n[0] >> 1) & 0x3f)
        .find(|&t| t < 32)
}

fn decode_errors(units: &[Vec<u8>], threads: usize) -> Vec<String> {
    let mut dec = Decoder::with_threads(threads);
    let mut errors = Vec::new();
    for (i, u) in units.iter().enumerate() {
        if let Err(e) = dec.decode(&packet(u.clone(), i)) {
            errors.push(e.to_string());
        }
    }
    if let Err(e) = dec.flush() {
        errors.push(e.to_string());
    }
    errors
}

#[test]
fn truncated_nal_units_are_errors_not_panics() {
    let r = Refs::open(refs::AREA);
    let f = feature("wpp_slices_main10");
    let data = stream(&r, f);
    let mut units = units_of(&data);
    units.truncate(4);
    let head = annexb_nals(&units[0])
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    let mut checked = 0;
    for (k, nal) in head.iter().enumerate() {
        let t = (nal[0] >> 1) & 0x3f;
        for cut in [2, 3, nal.len() / 2, nal.len() - 1] {
            if cut < 2 || cut >= nal.len() {
                continue;
            }
            let mut first = Vec::new();
            for (j, n) in head.iter().enumerate() {
                first.extend([0, 0, 0, 1]);
                first.extend_from_slice(if j == k { &n[..cut] } else { n });
            }
            let mut damaged = units.clone();
            damaged[0] = first;
            for threads in [1, 4] {
                let errors = decode_errors(&damaged, threads);
                let what = format!("NAL {k} (type {t}, {} bytes) cut to {cut}", nal.len());
                assert!(
                    errors.iter().all(|e| !e.contains("internal error")),
                    "{what}: a panic was caught: {errors:?}"
                );
                let must_fail = match t {
                    32..=34 => cut <= nal.len() / 2,
                    0..=31 => cut <= 3 || (cut == nal.len() / 2 && nal.len() > 64),
                    _ => false,
                };
                if must_fail {
                    assert!(!errors.is_empty(), "{what}: decoded without an error");
                    checked += 1;
                }
            }
        }
    }
    assert!(checked >= 12, "only {checked} truncations were checked");
}

#[test]
fn a_dropped_reference_picture_is_replaced_without_a_panic() {
    let r = Refs::open(refs::AREA);
    for name in ["long_b_runs", "pyramid_weightb_main10"] {
        let f = feature(name);
        let mut units = units_of(&stream(&r, f));
        assert!(
            matches!(first_vcl_type(&units[0]), Some(16..=21)),
            "{name}: the stream does not start at an IRAP"
        );
        assert!(
            first_vcl_type(&units[1]) == Some(1),
            "{name}: the second access unit is not a trailing reference picture"
        );
        units.remove(1);
        let format = if f.ten_bit {
            PixelFormat::Yuv420p10
        } else {
            PixelFormat::Yuv420p8
        };
        for threads in [1, 4] {
            let mut dec = Decoder::with_threads(threads);
            let mut frames = Vec::new();
            let mut errors = Vec::new();
            for (i, u) in units.iter().enumerate() {
                match dec.decode(&packet(u.clone(), i)) {
                    Ok(out) => frames.extend(out),
                    Err(e) => errors.push(e.to_string()),
                }
            }
            match dec.flush() {
                Ok(out) => frames.extend(out),
                Err(e) => errors.push(e.to_string()),
            }
            let what = format!("{name} without its first P picture ({threads} threads)");
            assert!(errors.is_empty(), "{what}: {errors:?}");
            assert_eq!(frames.len(), units.len(), "{what}: picture count");
            for (i, frame) in frames.iter().enumerate() {
                assert_eq!(
                    (frame.format, frame.width, frame.height),
                    (format, f.size.0, f.size.1),
                    "{what}: picture {i}"
                );
            }
        }
    }
}

#[test]
fn the_x265_sources_are_what_the_recorder_feeds_x265() {
    for f in refs::all() {
        let (w, h) = (f.size.0 as usize, f.size.1 as usize);
        let samples = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        let bytes = refs::source_bytes(f);
        let depth = if f.ten_bit { 2 } else { 1 };
        assert_eq!(
            bytes.len(),
            samples * depth * f.frames as usize,
            "{}",
            f.name
        );
        assert_eq!(refs::source(f.name).unwrap(), bytes, "{}", f.name);
        if f.ten_bit {
            assert!(
                bytes
                    .chunks_exact(2)
                    .all(|s| u16::from_le_bytes([s[0], s[1]]) <= 1023),
                "{}: a sample above 10 bits",
                f.name
            );
        }
    }
    assert!(refs::source("nothing").is_err());
    assert!(refs::find("nothing").is_err());
    assert_eq!(
        (refs::stream("a"), refs::recon("a")),
        ("a.hevc".into(), "a.yuv".into())
    );
}

#[test]
fn a_faded_source_starts_black() {
    let f = refs::find("weightp_8bit").unwrap();
    assert!(f.fade > 0 && !f.ten_bit);
    let (w, h) = (f.size.0 as usize, f.size.1 as usize);
    let bytes = refs::source_bytes(f);
    let luma = w * h;
    let chroma = (w / 2) * (h / 2);
    assert!(bytes[..luma].iter().all(|&v| v == 16));
    assert!(bytes[luma..luma + 2 * chroma].iter().all(|&v| v == 128));
    let ten = refs::find("weightp_main10").unwrap();
    let first = refs::source_bytes(ten);
    assert!(
        first[..2 * ten.size.0 as usize * ten.size.1 as usize]
            .chunks_exact(2)
            .all(|s| u16::from_le_bytes([s[0], s[1]]) >> 2 == 16)
    );
}

#[test]
fn x265_params_become_cli_flags() {
    assert_eq!(
        refs::x265_cli("a=1:b:c=-4"),
        ["--a", "1", "--b", "--c", "-4"]
    );
    assert!(refs::x265_cli("").is_empty());
}

#[test]
fn the_scaling_list_file_is_the_recorded_one() {
    let r = Refs::open(refs::AREA);
    let text = refs::scaling_list_text();
    r.assert_digest(refs::SCALING_LIST, &crate::testref::sha256(text.as_bytes()));
    assert_eq!(text.lines().count(), 72);
    assert!(text.starts_with("INTRA4X4_LUMA =\n"));
    assert!(text.contains("INTER32X32_CHROMAV_DC =\n"));
}

#[test]
fn a_long_cu_qp_delta_abs_suffix_is_an_error_not_an_overflow() {
    let input = include_bytes!("../../tests/fixtures/hevc/cu_qp_delta_abs_overflow.bin");
    let mut dec = Decoder::with_threads(1);
    let err = dec.decode(&packet(input.to_vec(), 0)).unwrap_err();
    assert!(err.to_string().contains("invalid cu_qp_delta_abs"), "{err}");
    let _ = dec.flush();
}
