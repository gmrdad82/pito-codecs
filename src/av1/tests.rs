use std::path::PathBuf;
use std::process::Command;

use super::parser::{FrameType, ObuAction, ObuType, Parser};

#[derive(Default, Debug)]
struct Stats {
    frames: usize,
    shown: usize,
    hidden: usize,
    existing: usize,
    keys: usize,
    tiles: u32,
    grain: usize,
    sizes: Vec<(u32, u32)>,
}

fn walk(parser: &mut Parser, unit: &[u8], stats: &mut Stats) -> Result<(), String> {
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
