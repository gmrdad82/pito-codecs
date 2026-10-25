use crate::{Def, FFMPEG, ff};

#[path = "../../../src/join/refs.rs"]
mod refs;

fn video(area: &'static str, file: &str, source: &str, args: &[&str]) -> Def {
    let mut all = vec!["-f", "lavfi", "-i", source];
    all.extend_from_slice(args);
    all.push("{out}");
    Def::fixture(area, file.to_string(), FFMPEG, ff(&all))
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, source, extra) in refs::JOIN_CLIPS {
        let mut args = refs::encoder(file).to_vec();
        args.extend_from_slice(extra);
        defs.push(video(refs::JOIN, file, source, &args));
    }
    for (file, extra) in refs::PROBE_SHAPES {
        let mut args = vec!["-c:v", "libx264", "-crf", "35"];
        args.extend_from_slice(extra);
        defs.push(video(refs::PCOD, file, refs::PROBED, &args));
    }
    for file in refs::VORBIS_FILES {
        defs.push(Def::fixture(
            refs::PCOD,
            file.to_string(),
            FFMPEG,
            ff(&[
                "-f",
                "lavfi",
                "-i",
                refs::VORBIS,
                "-c:a",
                "libvorbis",
                "-q:a",
                "3",
                "{out}",
            ]),
        ));
    }
    defs
}
