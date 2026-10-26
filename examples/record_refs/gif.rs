use crate::testref;
use crate::{Def, tool};

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
        "magick",
        tool(
            "magick",
            &[
                &format!("{{ref:{gif}.gif}}"),
                "-coalesce",
                "-alpha",
                "on",
                "-depth",
                "8",
                "rgba:{out}",
            ],
        ),
        rgba,
    )
}

fn split(ops: &str) -> Vec<String> {
    ops.split('|').map(str::to_string).collect()
}

pub fn defs() -> Vec<Def> {
    let size = format!("{}x{}", refs::SIZE.0, refs::SIZE.1);
    let mut defs = Vec::new();
    for (name, ops) in refs::MAGICK_ANIMS {
        let mut args = tool(
            "magick",
            &[
                "-size",
                &size,
                "-depth",
                "8",
                "rgba:{src}",
                "-alpha",
                "off",
                "-delay",
                "10",
                "-loop",
                "0",
            ],
        );
        args.extend(split(ops));
        args.push("{out}".into());
        defs.push(
            Def::fixture(AREA, format!("{name}.gif"), "magick", args)
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
        let mut args = split(args);
        args.push(
            if name == "im-87" {
                "GIF87:{out}"
            } else {
                "{out}"
            }
            .into(),
        );
        defs.push(Def::fixture(
            AREA,
            format!("{name}.gif"),
            "magick",
            tool(
                "magick",
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            ),
        ));
        defs.push(decoded(name));
    }
    defs
}
