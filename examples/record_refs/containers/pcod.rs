use crate::{Def, FFMPEG, ff, sha};

#[allow(dead_code)]
#[path = "../../../src/pcod/refs.rs"]
mod refs;

use refs::AREA;

const FFPROBE: &str = "ffprobe";

fn lavfi(file: &str, source: &str, args: &[&str]) -> Def {
    let mut all = vec!["-f", "lavfi", "-i", source];
    all.extend_from_slice(args);
    all.push("{out}");
    Def::fixture(AREA, file.into(), FFMPEG, ff(&all))
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, codec, _, _) in refs::TONES {
        defs.push(lavfi(file, refs::TONE, codec));
    }
    for (file, extra) in refs::PROBE_SHAPES {
        let mut args = vec!["-c:v", "libx264", "-crf", "35"];
        args.extend_from_slice(extra);
        defs.push(lavfi(file, refs::PROBED, &args));
    }
    let (start, end) = refs::VORBIS_SLICE;
    let trim = format!("atrim=start_sample={start}:end_sample={end}");
    for file in refs::VORBIS_FILES {
        defs.push(lavfi(file, refs::VORBIS, &["-c:a", "libvorbis", "-q:a", "3"]));
        defs.push(Def::tolerance(
            AREA,
            refs::vorbis_slice_name(file),
            FFMPEG,
            ff(&[
                "-i",
                &format!("{{ref:{file}}}"),
                "-af",
                &trim,
                "-f",
                "s16le",
                "-c:a",
                "pcm_s16le",
                "{out}",
            ]),
        ));
    }
    defs.push(lavfi(
        refs::SMOKE_HEVC,
        "testsrc2=size=352x240:rate=30",
        &[
            "-frames:v",
            "10",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-crf",
            "36",
            "-pix_fmt",
            "yuv420p",
            "-tag:v",
            "hvc1",
        ],
    ));
    defs.push(Def::ours(
        AREA,
        refs::GIF_TICKS.into(),
        "src/pcod/refs.rs ticks_gif()",
        refs::ticks_gif,
    ));
    let probe = format!(
        "scale={}:{}:flags=neighbor",
        refs::GIF_PROBE.0,
        refs::GIF_PROBE.1
    );
    for rate in refs::GIF_RATES {
        defs.push(Def::exact(
            AREA,
            refs::ticks_name(rate),
            FFMPEG,
            ff(&[
                "-i",
                &format!("{{ref:{}}}", refs::GIF_TICKS),
                "-vf",
                &format!("fps={rate},{probe}"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "{out}",
            ]),
        ));
    }
    for input in refs::WEBP_INPUTS {
        let webp = refs::webp_name(input);
        defs.push(
            Def::digest(
                AREA,
                webp.clone(),
                FFMPEG,
                ff(&[
                    "-f",
                    "png_pipe",
                    "-i",
                    "{src}",
                    "-c:v",
                    "libwebp",
                    "-lossless",
                    "1",
                    "-compression_level",
                    "6",
                    "{out}",
                ]),
                sha,
            )
            .from(move || refs::webp_png(input)),
        );
        defs.push(Def::exact(
            AREA,
            refs::webp_size_name(input),
            FFPROBE,
            [
                "-v",
                "error",
                "-show_entries",
                "format=size",
                "-of",
                "csv=p=0",
                "-o",
                "{out}",
                &format!("{{ref:{webp}}}"),
            ]
            .map(str::to_string)
            .to_vec(),
        ));
    }
    defs
}
