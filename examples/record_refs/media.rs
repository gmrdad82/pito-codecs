use crate::{Def, FFMPEG, ff, sha};

#[path = "../../src/media/refs.rs"]
mod refs;

use refs::AREA;

fn fixture(file: &str, args: &str) -> Def {
    let mut all = refs::split(args);
    all.push("{out}".into());
    Def::fixture(AREA, file.into(), FFMPEG, ff(&words(&all)))
}

fn words(all: &[String]) -> Vec<&str> {
    all.iter().map(String::as_str).collect()
}

fn decoded(file: &str) -> Def {
    Def::digest(
        AREA,
        refs::decoded(file),
        FFMPEG,
        ff(&[
            "-i",
            &format!("{{ref:{file}}}"),
            "-map",
            "0:a:0",
            "-f",
            "f32le",
            "{out}",
        ]),
        sha,
    )
}

fn window(file: &str, at: &str, rate: u32, filter: &str) -> Def {
    Def::tolerance(
        AREA,
        refs::window(file, at),
        FFMPEG,
        ff(&[
            "-f",
            "f32le",
            "-ar",
            &rate.to_string(),
            "-ac",
            "2",
            "-i",
            &format!("{{ref:{}}}", refs::decoded(file)),
            "-af",
            filter,
            "-f",
            "f32le",
            "{out}",
        ]),
    )
}

fn from_start(at: usize) -> String {
    format!("atrim=start_sample={at}:end_sample={}", at + refs::WINDOW)
}

fn last() -> String {
    format!("areverse,atrim=end_sample={},areverse", refs::WINDOW)
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    let rate = refs::PCM_RATE.to_string();
    for (ext, codec) in refs::PCM_CASES {
        let file = refs::pcm_file(ext, codec);
        let args = ff(&[
            "-f", "f32le", "-ar", &rate, "-ac", "2", "-i", "{src}", "-c:a", codec, "{out}",
        ]);
        defs.push(
            Def::fixture(AREA, file.clone(), FFMPEG, args)
                .from(|| Ok(refs::f32le(&refs::pcm_source()))),
        );
        defs.push(decoded(&file));
    }
    defs.push(fixture(refs::AC3_AAC, refs::AC3_AAC_ARGS));
    let rate = refs::LINEUP_RATE.to_string();
    for (file, codec) in refs::LINEUPS {
        let args = ff(&[
            "-f", "f32le", "-ar", &rate, "-ac", "2", "-i", "{src}", "-c:a", codec, "-b:a", "96k",
            "{out}",
        ]);
        defs.push(
            Def::fixture(AREA, file.into(), FFMPEG, args)
                .from(|| Ok(refs::f32le(&refs::lineup_source()))),
        );
        defs.push(decoded(file));
        for (k, at) in refs::LINEUP_AT.into_iter().enumerate() {
            defs.push(window(
                file,
                &format!("at{k}"),
                refs::LINEUP_RATE,
                &from_start(at),
            ));
        }
    }
    for (file, codec, rate, kbps, _) in refs::OFF_MS {
        let source = format!("sine=f=440:r={rate}:d={}", refs::OFF_MS_SECONDS);
        let args = ff(&[
            "-f", "lavfi", "-i", &source, "-ac", "2", "-c:a", codec, "-b:a", kbps, "{out}",
        ]);
        defs.push(Def::fixture(AREA, file.into(), FFMPEG, args));
        defs.push(decoded(file));
        defs.push(window(file, "head", rate, &from_start(refs::OFF_MS_HEAD)));
        defs.push(window(file, "tail", rate, &last()));
    }
    let source = format!("sine=f=440:r=48000:d={}", refs::OFF_MS_SECONDS);
    defs.push(Def::fixture(
        AREA,
        refs::OFF_MS_AAC.into(),
        FFMPEG,
        ff(&[
            "-f", "lavfi", "-i", &source, "-c:a", "aac", "-b:a", "32k", "{out}",
        ]),
    ));
    defs.push(fixture(refs::VP9_ALTREF, refs::VP9_ALTREF_ARGS));
    defs.push(fixture(refs::PYRAMID, refs::PYRAMID_ARGS));
    defs.push(Def::digest(
        AREA,
        format!("{}.yuv", refs::PYRAMID),
        FFMPEG,
        ff(&[
            "-i",
            &format!("{{ref:{}}}", refs::PYRAMID),
            "-map",
            "0:v:0",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "{out}",
        ]),
        sha,
    ));
    for (file, args) in refs::tagged_709() {
        defs.push(fixture(file, &args));
    }
    for (size, space, _, range, _) in refs::VP9_KEYS {
        let file = refs::vp9_key(size.1, space, range);
        defs.push(fixture(&file, &refs::vp9_key_args(size, space, range)));
    }
    for (file, pix, _) in refs::AV1S {
        defs.push(fixture(file, &refs::av1_args(pix)));
    }
    for (file, encode) in refs::VARIANTS {
        defs.push(fixture(file, &format!("{}|{encode}", refs::VARIANT_SOURCE)));
    }
    defs.push(fixture(refs::VP9_PROFILE2, refs::VP9_PROFILE2_ARGS));
    defs.push(fixture(refs::MP3, refs::MP3_ARGS));
    defs
}
