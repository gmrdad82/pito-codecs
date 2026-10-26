use crate::video::{
    X264, X265, i420, raw, upshift, written, x264_args, x265_args, y4m, y4m_payload,
};
use crate::{Def, sha, tool};

#[allow(dead_code)]
#[path = "../../src/gpu/refs.rs"]
mod gpu_refs;

#[allow(dead_code)]
#[path = "../../src/av1/refs.rs"]
mod av1_refs;

const DAV1D: &str = "dav1d";

fn gpu() -> Vec<Def> {
    use gpu_refs::{AREA, FRAMES, RATE, STREAMS};
    let mut defs = Vec::new();
    for s in &STREAMS {
        for &size in s.sizes {
            let (w, h) = size;
            let file = s.file(size);
            let recon = format!("{file}.recon.{}", if s.hevc { "y4m" } else { "yuv" });
            let args = if s.hevc {
                let opts: Vec<String> = s.args.iter().map(|a| a.to_string()).collect();
                x265_args(FRAMES, s.ten, &opts, &recon)
            } else {
                x264_args(FRAMES, s.args, &recon)
            };
            let what = if s.hevc {
                "the --recon reconstruction x265"
            } else {
                "the --dump-yuv reconstruction x264"
            };
            let tool_name = if s.hevc { X265 } else { X264 };
            let take = if s.hevc { y4m_payload } else { raw };
            defs.push(
                Def::fixture(AREA, file.clone(), tool_name, args).from(move || {
                    y4m(
                        w,
                        h,
                        RATE,
                        "420jpeg",
                        i420(w, h),
                        &gpu_refs::source_bytes(size),
                    )
                }),
            );
            defs.push(written(
                AREA,
                gpu_refs::decoded(&file),
                recon,
                &format!("{what} wrote while making {{ref:{file}}}"),
                take,
            ));
        }
    }
    defs
}

fn av1_args(s: &av1_refs::Stream) -> Vec<String> {
    let (frames, bits) = (s.frames.to_string(), s.bits.to_string());
    let (limit, depth, input) = (
        format!("--limit={frames}"),
        format!("--bit-depth={bits}"),
        format!("--input-bit-depth={bits}"),
    );
    let mut a: Vec<&str> = match s.encoder {
        av1_refs::AOM => vec!["--ivf", "--quiet", &limit, &depth, &input],
        av1_refs::SVT => vec![
            "-i",
            "{src}",
            "-b",
            "{out}",
            "--progress",
            "0",
            "-n",
            &frames,
            "--input-depth",
            &bits,
        ],
        _ => vec![
            "{src}",
            "-o",
            "{out}",
            "-y",
            "--threads",
            "1",
            "--limit",
            &frames,
        ],
    };
    a.extend_from_slice(s.args);
    if s.encoder == av1_refs::AOM {
        a.extend(["-o", "{out}", "{src}"]);
    }
    tool(s.encoder, &a)
}

fn av1() -> Vec<Def> {
    use av1_refs::{AREA, BIT_EXACT, PARSER, RATE, Stream};
    let encoded = |s: &'static Stream| {
        let (w, h) = s.size;
        let (csp, frame) = if s.bits == 10 {
            ("420p10", 2 * i420(w, h))
        } else {
            ("420jpeg", i420(w, h))
        };
        Def::fixture(AREA, av1_refs::ivf(s.name), s.encoder, av1_args(s)).from(move || {
            let raw = av1_refs::source_bytes(s);
            let raw = if s.bits == 10 { upshift(&raw) } else { raw };
            y4m(w, h, RATE, csp, frame, &raw)
        })
    };
    let mut defs = Vec::new();
    for s in &BIT_EXACT {
        defs.push(encoded(s));
        let input = format!("{{ref:{}}}", av1_refs::ivf(s.name));
        defs.push(Def::digest(
            AREA,
            av1_refs::decoded(s.name),
            DAV1D,
            tool(
                DAV1D,
                &["--quiet", "--muxer", "yuv", "-i", &input, "-o", "{out}"],
            ),
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
