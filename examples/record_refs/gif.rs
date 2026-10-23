use crate::testref;
use crate::{Def, FFMPEG, ff};

#[path = "../../src/gif/refs.rs"]
mod refs;

use refs::AREA;

fn rgba(bytes: &[u8]) -> Result<String, String> {
    Ok(testref::rgba_digest(bytes))
}

fn decoded(gif: &str) -> Def {
    Def::digest(
        AREA,
        format!("{gif}.rgba"),
        FFMPEG,
        ff(&[
            "-i",
            &format!("{{ref:{gif}.gif}}"),
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "{out}",
        ]),
        rgba,
    )
}

pub fn defs() -> Vec<Def> {
    let size = format!("{}x{}", refs::SIZE.0, refs::SIZE.1);
    let mut defs = Vec::new();
    for (name, graph) in refs::FFMPEG_GIFS {
        let args = ff(&[
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &size,
            "-r",
            "10",
            "-i",
            "{src}",
            "-filter_complex",
            &format!("split[s0][s1];[s0]{graph}"),
            "{out}",
        ]);
        defs.push(
            Def::fixture(AREA, format!("{name}.gif"), FFMPEG, args)
                .from(|| Ok(refs::source_bytes())),
        );
        defs.push(decoded(name));
    }
    for (name, _) in refs::ours() {
        defs.push(Def::ours(
            AREA,
            format!("{name}.gif"),
            &format!("src/gif/refs.rs encode_ours(\"{name}\")"),
            move || refs::encode_ours(name),
        ));
        defs.push(decoded(name));
    }
    defs.push(Def::ours(
        AREA,
        "alpha.gif".into(),
        "src/gif/refs.rs encode_alpha()",
        refs::encode_alpha,
    ));
    defs.push(decoded("alpha"));
    for (name, _, _, args) in refs::MAGICK_GIFS {
        let mut args: Vec<String> = args.split('|').map(str::to_string).collect();
        args.push(
            if name == "im-87" {
                "GIF87:{out}"
            } else {
                "{out}"
            }
            .into(),
        );
        defs.push(Def::fixture(AREA, format!("{name}.gif"), "magick", args));
        defs.push(decoded(name));
    }
    defs
}
