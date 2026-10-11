use std::io::{Read, Write};
use std::time::Instant;

use pito_codecs::h264::{Bitstream, Encoder, EncoderConfig, Preset, Rate};
use pito_codecs::media::{Frame, PixelFormat, Plane, TimeBase};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        return Err("usage: h264_encode <in.yuv> <width> <height> <out.h264> [crf=23|qp=N|kbps=N] [preset=speed|balanced|quality] [bframes=N] [threads=N] [frames=N]".into());
    }
    let (w, h): (u32, u32) = (args[1].parse()?, args[2].parse()?);
    let mut cfg = EncoderConfig {
        width: w,
        height: h,
        bitstream: Bitstream::AnnexB,
        ..EncoderConfig::default()
    };
    let mut limit = usize::MAX;
    for a in &args[4..] {
        let (k, v) = a.split_once('=').ok_or("expected key=value")?;
        match k {
            "crf" => cfg.rate = Rate::Crf(v.parse()?),
            "qp" => cfg.rate = Rate::Qp(v.parse()?),
            "kbps" => cfg.rate = Rate::Bitrate(v.parse()?),
            "preset" => {
                cfg.preset = match v {
                    "speed" => Preset::Speed,
                    "quality" => Preset::Quality,
                    _ => Preset::Balanced,
                }
            }
            "bframes" => cfg.b_frames = v.parse()?,
            "threads" => cfg.threads = v.parse()?,
            "frames" => limit = v.parse()?,
            _ => return Err(format!("unknown option {k}").into()),
        }
    }
    let (ws, hs) = (w as usize, h as usize);
    let (cw, ch) = (ws / 2, hs / 2);
    let size = ws * hs + 2 * cw * ch;
    let mut raw = Vec::new();
    std::fs::File::open(&args[0])?.read_to_end(&mut raw)?;
    let tb = TimeBase::new(1, 30);
    let frames: Vec<Frame> = raw
        .chunks_exact(size)
        .take(limit)
        .enumerate()
        .map(|(i, c)| Frame {
            format: PixelFormat::Yuv420p8,
            width: w,
            height: h,
            pts: i as i64,
            time_base: tb,
            planes: [
                Plane {
                    data: c[..ws * hs].to_vec(),
                    stride: ws,
                    width: ws,
                    height: hs,
                },
                Plane {
                    data: c[ws * hs..ws * hs + cw * ch].to_vec(),
                    stride: cw,
                    width: cw,
                    height: ch,
                },
                Plane {
                    data: c[ws * hs + cw * ch..].to_vec(),
                    stride: cw,
                    width: cw,
                    height: ch,
                },
            ],
        })
        .collect();
    drop(raw);
    let mut out = std::fs::File::create(&args[3])?;
    let t = Instant::now();
    let mut enc = Encoder::new(cfg)?;
    let mut bytes = 0usize;
    for f in &frames {
        for p in enc.encode(f)? {
            bytes += p.data.len();
            out.write_all(&p.data)?;
        }
    }
    for p in enc.flush()? {
        bytes += p.data.len();
        out.write_all(&p.data)?;
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "{} frames {:.1} fps {} bytes",
        frames.len(),
        frames.len() as f64 / secs,
        bytes
    );
    Ok(())
}
