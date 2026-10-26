use super::mux::{self, Csp, Timed};
use crate::{Def, OURS, sha, tool, words};
use pito_codecs::mp4;

#[path = "../../../src/join/refs.rs"]
mod refs;

use refs::{JOIN, Made};

fn csp(chroma: u8) -> Csp {
    match chroma {
        1 => Csp::I420,
        2 => Csp::I422,
        _ => Csp::I444,
    }
}

fn x264(file: &str, size: (u32, u32), chroma: u8, bits: u8) -> Vec<Def> {
    let c = csp(chroma);
    let depth = bits.to_string();
    let profile = match (chroma, bits) {
        (2, _) => "high422",
        (3, _) => "high444",
        (_, 10) => "high10",
        _ => "high",
    };
    super::remux::x264_mp4(
        JOIN,
        file,
        refs::FPS,
        &[
            "--crf",
            "30",
            "--output-csp",
            c.x26x(),
            "--output-depth",
            &depth,
            "--profile",
            profile,
        ],
        move || {
            Ok(mux::y4m(
                size,
                (refs::FPS, 1),
                c,
                bits,
                refs::FRAMES,
                &mux::pattern(size),
            ))
        },
    )
}

fn x265(file: &'static str, size: (u32, u32), chroma: u8, defs: &mut Vec<Def>) {
    let stream = format!("{file}.hevc");
    let c = csp(chroma);
    let res = format!("{}x{}", size.0, size.1);
    let fps = refs::FPS.to_string();
    let n = refs::FRAMES.to_string();
    defs.push(
        Def::digest(
            JOIN,
            stream.clone(),
            "x265",
            tool(
                "x265",
                &[
                    "--input",
                    "{src}",
                    "--input-res",
                    &res,
                    "--fps",
                    &fps,
                    "--input-csp",
                    c.x26x(),
                    "--frames",
                    &n,
                    "--preset",
                    "ultrafast",
                    "--bframes",
                    "0",
                    "--crf",
                    "30",
                    "--frame-threads",
                    "1",
                    "--no-wpp",
                    "--pools",
                    "none",
                    "--log-level",
                    "error",
                    "--no-info",
                    "--no-progress",
                    "--output",
                    "{out}",
                ],
            ),
            sha,
        )
        .from(move || {
            Ok(mux::raw_video(
                size,
                c,
                8,
                refs::FRAMES,
                &mux::pattern(size),
            ))
        }),
    );
    let what = format!("{{ref:{stream}}} in our MP4 writer as hvc1");
    defs.push(
        Def::fixture(JOIN, file.into(), OURS, words(&what)).from(move || {
            let coded = mux::hevc(&mux::read(JOIN, &stream)?)?;
            Timed {
                coded: &coded,
                pocs: &[],
                size,
                fps: refs::FPS,
            }
            .hevc_mp4(mp4::Brand::Mp4)
        }),
    );
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, size, made) in refs::JOIN_CLIPS {
        match made {
            Made::X264 { chroma, bits } => defs.extend(x264(file, size, chroma, bits)),
            Made::X265 { chroma } => x265(file, size, chroma, &mut defs),
            Made::Mpeg4 => defs.push(
                Def::fixture(
                    JOIN,
                    file.into(),
                    OURS,
                    words("an mp4v track (MPEG-4 Part 2, object type 0x20) of placeholder samples in our MP4 writer"),
                )
                .from(move || {
                    mux::mp4v(
                        (size.0 as u16, size.1 as u16),
                        Some(0x20),
                        refs::FPS,
                        refs::FRAMES as usize,
                    )
                }),
            ),
        }
    }
    defs
}
