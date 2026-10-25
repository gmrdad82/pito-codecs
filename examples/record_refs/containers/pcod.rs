use crate::{Def, FFMPEG, ff};

#[allow(dead_code)]
#[path = "../../../src/pcod/refs.rs"]
mod refs;

use refs::AREA;

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, codec, _, _) in refs::TONES {
        let mut args = vec!["-f", "lavfi", "-i", refs::TONE];
        args.extend_from_slice(codec);
        args.push("{out}");
        defs.push(Def::fixture(AREA, file.into(), FFMPEG, ff(&args)));
    }
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
        defs.push(
            Def::exact(
                AREA,
                refs::webp_name(input),
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
            )
            .from(move || refs::webp_png(input)),
        );
    }
    defs
}
