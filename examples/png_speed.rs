#[macro_use]
#[path = "support/fail.rs"]
mod fail;
#[path = "bench/png.rs"]
mod png;

use std::error::Error;

use png::{CONTENTS, FORMATS, Level, OUTSIDE};

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut runs = 5usize;
    let mut only = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--runs takes a whole number")?
            }
            "--only" => only.push(args.next().ok_or("--only needs a value")?),
            "--threads" => {
                let n = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--threads takes a whole number")?;
                pito_codecs::png::limit_threads(n);
            }
            "--file" => files.push(args.next().ok_or("--file needs a path")?),
            _ => return Err(format!("unknown {a}; usage: png_speed [--runs N] [--threads N] [--only TEXT]... [--file PNG]...").into()),
        }
    }
    say!(
        "pito-codecs against {OUTSIDE}, megapixels per second, median of {runs} interleaved runs"
    )?;
    for path in &files {
        let pic = match png::from_file(std::path::Path::new(path)) {
            Ok(p) => p,
            Err(e) => {
                say!("{path}: skipped, {e}")?;
                continue;
            }
        };
        let name = std::path::Path::new(path)
            .file_name()
            .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
        for level in [Level::Fast, Level::Balanced] {
            let t = png::time_encode(&pic, level, runs)?;
            let (o, x) = (median(&t.ours), median(&t.outside));
            say!(
                "{name} {}x{} {:?} {} bit  encode {:<8} {:>10} B against {:>10} B  size {:+.1}%  {:.2}x",
                pic.width,
                pic.height,
                pic.color,
                pic.depth,
                level.key(),
                t.ours_bytes,
                t.outside_bytes,
                (t.ours_bytes as f64 / t.outside_bytes as f64 - 1.0) * 100.0,
                o / x
            )?;
        }
    }
    if !files.is_empty() {
        return Ok(());
    }
    for f in FORMATS {
        for content in CONTENTS {
            let id = format!("{}.{content}", f.key);
            if !only.is_empty() && !only.iter().any(|o| id.contains(o.as_str())) {
                continue;
            }
            let pic = png::picture(f, content);
            say!("{id}")?;
            let mut files: Vec<(String, Vec<u8>)> = Vec::new();
            for level in [Level::Fast, Level::Balanced] {
                let t = png::time_encode(&pic, level, runs)?;
                let outside = if level == Level::Fast {
                    "fast"
                } else {
                    "default"
                };
                let (o, x) = (median(&t.ours), median(&t.outside));
                say!(
                    "  encode {:<8} ours {:>8.2} MP/s {:>10} B   outside({outside}) {:>8.2} MP/s {:>10} B   {:.2}x  size {:+.1}%   load {:.1} to {:.1}",
                    level.key(),
                    o,
                    t.ours_bytes,
                    x,
                    t.outside_bytes,
                    o / x,
                    (t.ours_bytes as f64 / t.outside_bytes as f64 - 1.0) * 100.0,
                    t.load_start,
                    t.load_end
                )?;
                files.push((
                    format!("ours-{}", level.key()),
                    png::encode_ours(&pic, level)?,
                ));
                files.push((
                    format!("outside-{outside}"),
                    png::encode_outside(&pic, level)?,
                ));
            }
            let mut d = png::Decoders::new(&pic);
            for (name, file) in &files {
                if !d.same_samples(file)? {
                    return Err(format!("{id} {name}: the two decoders disagree").into());
                }
                let t = png::time_decode(&pic, file, runs)?;
                let (o, x) = (median(&t.ours), median(&t.outside));
                say!(
                    "  decode {:<16} ours {:>8.2} MP/s   outside {:>8.2} MP/s   {:.2}x   load {:.1} to {:.1}",
                    name,
                    o,
                    x,
                    o / x,
                    t.load_start,
                    t.load_end
                )?;
            }
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    fail::run("png_speed", run)
}
