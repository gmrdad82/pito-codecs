use crate::{Def, FFMPEG, ff};

#[path = "../../../src/join/refs.rs"]
mod refs;

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, source, extra) in refs::JOIN_CLIPS {
        let mut args = vec!["-f", "lavfi", "-i", source];
        args.extend_from_slice(refs::encoder(file));
        args.extend_from_slice(extra);
        args.push("{out}");
        defs.push(Def::fixture(refs::JOIN, file.to_string(), FFMPEG, ff(&args)));
    }
    defs
}
