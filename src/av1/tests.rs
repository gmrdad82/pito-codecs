use std::path::PathBuf;
use std::process::Command;

use super::parser::{FrameType, ObuAction, ObuType, Parser};

#[derive(Default, Debug)]
pub(super) struct Stats {
    frames: usize,
    pub(super) shown: usize,
    hidden: usize,
    existing: usize,
    keys: usize,
    pub(super) tiles: u32,
    pub(super) grain: usize,
    pub(super) sizes: Vec<(u32, u32)>,
    pub(super) restoration: bool,
    pub(super) superres: bool,
    pub(super) intrabc: bool,
    pub(super) screen: bool,
    pub(super) lossless: bool,
    pub(super) compound: bool,
}

pub(super) fn walk(parser: &mut Parser, unit: &[u8], stats: &mut Stats) -> Result<(), String> {
    let mut tiles = Vec::new();
    let mut pos = 0;
    while pos < unit.len() {
        let obu = match parser.read_obu(&unit[pos..])? {
            ObuAction::Process(obu) => obu,
            ObuAction::Drop(n) => {
                pos += n.max(1);
                continue;
            }
        };
        match obu.header.obu_type {
            ObuType::SequenceHeader => {
                parser.parse_sequence_header_obu(&obu)?;
            }
            ObuType::TemporalDelimiter => parser.parse_temporal_delimiter_obu(),
            ObuType::FrameHeader | ObuType::Frame => {
                let fh = parser.parse_frame_header_obu(&obu)?;
                if fh.show_existing_frame {
                    stats.existing += 1;
                    parser.ref_frame_update(&fh)?;
                } else {
                    stats.frames += 1;
                    if fh.show_frame {
                        stats.shown += 1;
                    } else {
                        stats.hidden += 1;
                    }
                    stats.keys += usize::from(fh.frame_type == FrameType::KeyFrame);
                    stats.grain += usize::from(fh.film_grain_params.apply_grain);
                    stats.tiles = stats.tiles.max(parser.num_tiles());
                    stats.restoration |= fh.loop_restoration_params.uses_lr;
                    stats.superres |= fh.use_superres;
                    stats.intrabc |= fh.allow_intrabc;
                    stats.screen |= fh.allow_screen_content_tools != 0;
                    stats.lossless |= fh.coded_lossless;
                    stats.compound |= fh.reference_select;
                    let size = (fh.upscaled_width, fh.frame_height);
                    if !stats.sizes.contains(&size) {
                        stats.sizes.push(size);
                    }
                    if obu.header.obu_type == ObuType::Frame {
                        let data = obu.data.get(fh.header_bytes..).unwrap_or_default();
                        parser.parse_tile_group_obu(data, &mut tiles)?;
                    }
                    parser.ref_frame_update(&fh)?;
                }
            }
            ObuType::TileGroup => {
                parser.parse_tile_group_obu(obu.data, &mut tiles)?;
            }
            _ => {}
        }
        pos += obu.bytes_used.max(1);
    }
    Ok(())
}

fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let mut pos = u16::from_le_bytes([data[6], data[7]]) as usize;
    while pos + 12 <= data.len() {
        let size = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
}

#[test]
#[ignore]
fn stream_features_in_tmp() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/av1");
    let list = std::env::var("PITO_AV1_STREAMS").unwrap_or_default();
    for name in list.split(',').filter(|s| !s.is_empty()) {
        let data = std::fs::read(dir.join(format!("{name}.ivf"))).unwrap();
        let mut parser = Parser::default();
        let mut stats = Stats::default();
        for unit in ivf_frames(&data) {
            walk(&mut parser, unit, &mut stats).unwrap();
        }
        eprintln!("{name}: {stats:?}");
    }
}

#[test]
fn parser_refuses_corrupt_units_without_panicking() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/av1-parser");
    std::fs::create_dir_all(&dir).unwrap();
    let ivf = dir.join("corrupt.ivf");
    let made = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg("testsrc2=size=352x288:rate=30")
        .args(["-frames:v", "8", "-c:v", "libaom-av1", "-cpu-used", "8"])
        .args(["-tile-columns", "1", "-f", "ivf"])
        .arg(&ivf)
        .output()
        .is_ok_and(|o| o.status.success());
    if !made {
        eprintln!("SKIPPED: no ffmpeg with libaom-av1");
        return;
    }
    let data = std::fs::read(&ivf).unwrap();
    let units = ivf_frames(&data);
    let mut clean = Parser::default();
    let mut stats = Stats::default();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for unit in &units {
        for round in 0..64 {
            let mut bad = unit.to_vec();
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            if round % 2 == 0 {
                bad.truncate((seed as usize) % (unit.len() + 1));
            } else {
                for k in 0..4 {
                    let at = ((seed >> (k * 16)) as usize) % bad.len().max(1);
                    if let Some(b) = bad.get_mut(at) {
                        *b ^= (seed >> (k * 8)) as u8 | 1;
                    }
                }
            }
            let mut parser = clean.clone();
            let _ = walk(&mut parser, &bad, &mut Stats::default());
        }
        walk(&mut clean, unit, &mut stats).unwrap();
    }
    assert_eq!(stats.shown + stats.existing, 8);
    assert_eq!(stats.tiles, 2);
}

pub(super) fn obus(unit: &[u8]) -> Vec<(u8, std::ops::Range<usize>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(&b) = unit.get(pos) {
        let mut at = pos + 1 + usize::from(b & 4 != 0);
        let mut size = 0usize;
        if b & 2 != 0 {
            let mut shift = 0;
            loop {
                let Some(&v) = unit.get(at) else {
                    return out;
                };
                at += 1;
                size |= usize::from(v & 0x7f) << shift;
                shift += 7;
                if v & 0x80 == 0 || shift > 56 {
                    break;
                }
            }
        } else {
            size = unit.len().saturating_sub(at);
        }
        let end = at.saturating_add(size);
        if end > unit.len() {
            return out;
        }
        out.push(((b >> 3) & 15, pos..end));
        pos = end;
    }
    out
}

pub(super) fn small_stream(tag: &str) -> Option<Vec<u8>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/av1-parser");
    std::fs::create_dir_all(&dir).ok()?;
    let ivf = dir.join(format!("{tag}.{}.ivf", std::process::id()));
    let made = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "testsrc2=size=64x64:rate=30", "-frames:v", "3"])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
        ])
        .args(["-f", "ivf"])
        .arg(&ivf)
        .output()
        .is_ok_and(|o| o.status.success());
    let data = std::fs::read(&ivf).ok();
    let _ = std::fs::remove_file(&ivf);
    if !made {
        eprintln!("SKIPPED: no ffmpeg with libaom-av1");
        return None;
    }
    data
}

pub(super) fn payload_start(unit: &[u8], r: &std::ops::Range<usize>) -> usize {
    let b = unit[r.start];
    let mut at = r.start + 1 + usize::from(b & 4 != 0);
    if b & 2 != 0 {
        while unit.get(at).is_some_and(|v| v & 0x80 != 0) {
            at += 1;
        }
        at += 1;
    }
    at
}

pub(super) fn sequence_payload(unit: &[u8]) -> Option<(std::ops::Range<usize>, usize)> {
    let (_, r) = obus(unit).into_iter().find(|(t, _)| *t == 1)?;
    let at = payload_start(unit, &r);
    Some((r, at))
}

pub(super) fn shortened(unit: &[u8], r: &std::ops::Range<usize>, keep: usize) -> Vec<u8> {
    let head = r.start + 1 + usize::from(unit[r.start] & 4 != 0);
    let payload = payload_start(unit, r);
    let mut out = unit[..head].to_vec();
    out[r.start] |= 2;
    let mut n = keep;
    loop {
        let v = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(v);
            break;
        }
        out.push(v | 0x80);
    }
    out.extend_from_slice(&unit[payload..(payload + keep).min(r.end)]);
    out
}

#[test]
fn shortened_rewrites_the_size_to_the_kept_payload() {
    let unit = [0x12, 0x00, 0x0a, 0x03, 1, 2, 3];
    assert_eq!(payload_start(&unit, &(2..7)), 4);
    assert_eq!(
        shortened(&unit, &(2..7), 1),
        vec![0x12, 0x00, 0x0a, 0x01, 1]
    );
    assert_eq!(shortened(&unit, &(2..7), 0), vec![0x12, 0x00, 0x0a, 0x00]);
    let ext = [0x0e, 0x00, 0x82, 0x01, 9, 9];
    assert_eq!(payload_start(&ext, &(0..6)), 4);
    assert_eq!(shortened(&ext, &(0..6), 2), vec![0x0e, 0x00, 0x02, 9, 9]);
    let bare = [0x30, 7, 8, 9];
    assert_eq!(payload_start(&bare, &(0..4)), 1);
    assert_eq!(shortened(&bare, &(0..4), 2), vec![0x32, 0x02, 7, 8]);
    let long: Vec<u8> = [0x32, 0x80, 0x01].into_iter().chain([5u8; 128]).collect();
    assert_eq!(payload_start(&long, &(0..131)), 3);
    assert_eq!(shortened(&long, &(0..131), 128)[..3], [0x32, 0x80, 0x01]);
    assert_eq!(shortened(&long, &(0..131), 127)[..3], [0x32, 0x7f, 5]);
}

#[test]
fn obus_splits_units_and_stops_at_a_short_one() {
    let unit = [0x12, 0x00, 0x0a, 0x02, 0xaa, 0xbb, 0x32, 0x81, 0x00];
    let got = obus(&unit);
    assert_eq!(got, vec![(2, 0..2), (1, 2..6)]);
    assert!(obus(&[]).is_empty());
    assert_eq!(obus(&[0x0a]), vec![]);
    assert_eq!(obus(&[0x30, 1, 2]), vec![(6, 0..3)]);
    assert_eq!(obus(&[0x0e, 0x00, 0x01, 0xcc]), vec![(1, 0..4)]);
}

#[test]
fn parser_refuses_every_truncated_obu() {
    let Some(data) = small_stream("parser-truncated") else {
        return;
    };
    let units = ivf_frames(&data);
    assert_eq!(units.len(), 3);
    let mut clean = Parser::default();
    let mut cuts = 0;
    let mut short = 0;
    for unit in &units {
        for (t, r) in obus(unit) {
            for cut in r.start + 1..r.end {
                let mut parser = clean.clone();
                let got = walk(&mut parser, &unit[..cut], &mut Stats::default());
                assert!(got.is_err(), "cut at {cut} inside OBU {r:?} parsed");
                cuts += 1;
            }
            for keep in 0..r.end - payload_start(unit, &r) {
                let bad = shortened(unit, &r, keep);
                let got = walk(&mut clean.clone(), &bad, &mut Stats::default());
                if t == 1 || t == 3 {
                    assert!(got.is_err(), "OBU type {t} {r:?} short at {keep} parsed");
                }
                short += 1;
            }
        }
        walk(&mut clean, unit, &mut Stats::default()).unwrap();
    }
    assert!(cuts > 100);
    assert!(short > 100);
}

#[test]
fn parser_refuses_an_out_of_range_profile() {
    let Some(data) = small_stream("parser-profile") else {
        return;
    };
    let unit = ivf_frames(&data)[0].to_vec();
    let (r, payload) = sequence_payload(&unit).unwrap();
    let mut good = Parser::default();
    walk(&mut good, &unit[..r.end], &mut Stats::default()).unwrap();
    assert_eq!(good.sequence().unwrap().seq_profile, 0);
    for profile in 3u8..=7 {
        let mut bad = unit.clone();
        bad[payload] = (bad[payload] & 0x1f) | (profile << 5);
        let got = walk(&mut Parser::default(), &bad, &mut Stats::default());
        let e = got.expect_err("profile above 2 parsed");
        assert!(e.contains("seq_profile"), "{e}");
    }
}

#[test]
fn parser_refuses_a_frame_before_its_sequence_header() {
    let Some(data) = small_stream("parser-order") else {
        return;
    };
    let unit = ivf_frames(&data)[0];
    let without: Vec<u8> = obus(unit)
        .into_iter()
        .filter(|(t, _)| *t != 1)
        .flat_map(|(_, r)| unit[r].to_vec())
        .collect();
    assert!(without.len() < unit.len());
    let mut parser = Parser::default();
    let e = walk(&mut parser, &without, &mut Stats::default()).expect_err("frame parsed");
    assert!(e.contains("sequence header"), "{e}");
    let mut stats = Stats::default();
    walk(&mut Parser::default(), unit, &mut stats).unwrap();
    assert_eq!(stats.shown, 1);
}
