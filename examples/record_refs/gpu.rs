use crate::{Def, FFMPEG, ff, sha};

#[allow(dead_code)]
#[path = "../../src/gpu/refs.rs"]
mod gpu_refs;

#[allow(dead_code)]
#[path = "../../src/av1/refs.rs"]
mod av1_refs;

fn raw_input(size: (u32, u32), rate: u32) -> Vec<String> {
    [
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        &format!("{}x{}", size.0, size.1),
        "-r",
        &rate.to_string(),
        "-i",
        "{src}",
    ]
    .map(String::from)
    .to_vec()
}

fn gpu() -> Vec<Def> {
    use gpu_refs::{AREA, RATE, STREAMS};
    let mut defs = Vec::new();
    for s in &STREAMS {
        for &size in s.sizes {
            let file = s.file(size);
            let mut args = raw_input(size, RATE);
            args.extend(s.args.iter().map(|a| a.to_string()));
            args.extend(["-f".into(), s.muxer().into(), "{out}".into()]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            defs.push(
                Def::fixture(AREA, file.clone(), FFMPEG, ff(&args))
                    .from(move || Ok(gpu_refs::source_bytes(size))),
            );
            defs.push(Def::digest(
                AREA,
                gpu_refs::decoded(&file),
                FFMPEG,
                ff(&[
                    "-i",
                    &format!("{{ref:{file}}}"),
                    "-fps_mode",
                    "passthrough",
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    s.pix_fmt(),
                    "{out}",
                ]),
                sha,
            ));
        }
    }
    defs
}

fn av1() -> Vec<Def> {
    use av1_refs::{AREA, BIT_EXACT, PARSER, RATE, Stream};
    let encoded = |s: &'static Stream| {
        let mut args = raw_input(s.size, RATE);
        let frames = s.frames.to_string();
        args.extend(
            [
                "-frames:v",
                &frames,
                "-pix_fmt",
                s.pix_fmt,
                "-c:v",
                s.encoder,
            ]
            .map(String::from),
        );
        args.extend(s.args.iter().map(|a| a.to_string()));
        args.extend(["-f".into(), "ivf".into(), "{out}".into()]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Def::fixture(AREA, av1_refs::ivf(s.name), FFMPEG, ff(&args))
            .from(move || Ok(av1_refs::source_bytes(s)))
    };
    let mut defs = Vec::new();
    for s in &BIT_EXACT {
        defs.push(encoded(s));
        let mut args = vec![
            "-c:v".to_string(),
            "libdav1d".into(),
            "-i".into(),
            format!("{{ref:{}}}", av1_refs::ivf(s.name)),
            "-noautoscale".into(),
        ];
        args.extend(["-fps_mode", "passthrough", "-f", "rawvideo", "{out}"].map(String::from));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        defs.push(Def::digest(
            AREA,
            av1_refs::decoded(s.name),
            FFMPEG,
            ff(&args),
            sha,
        ));
    }
    for s in &PARSER {
        defs.push(encoded(s));
    }
    defs
}

pub fn defs() -> Vec<Def> {
    let mut defs = gpu();
    defs.extend(av1());
    defs
}
