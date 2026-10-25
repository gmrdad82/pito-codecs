use crate::{Def, FFMPEG, ff, sha};

#[allow(dead_code)]
#[path = "../../src/h264/refs.rs"]
mod h264;
#[allow(dead_code)]
#[path = "../../src/hevc/refs.rs"]
mod hevc;
#[allow(dead_code)]
#[path = "../../src/hevc/refs_synth.rs"]
mod hevc_synth;
#[allow(dead_code)]
#[path = "../../src/vp9/refs.rs"]
mod vp9;
#[allow(dead_code)]
#[path = "../../src/vp9/refs_alpha.rs"]
mod vp9_alpha;

const FFPROBE: &str = "ffprobe";
const X265: &str = "x265";
const STRICT: &[&str] = &["-err_detect", "explode", "-xerror"];

fn raw_input(pix_fmt: &str, (w, h): (u32, u32), rate: &str) -> Vec<String> {
    [
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix_fmt,
        "-s",
        &format!("{w}x{h}"),
        "-r",
        rate,
        "-i",
        "{src}",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn args(parts: &[Vec<String>]) -> Vec<String> {
    let mut out = ff(&[]);
    for p in parts {
        out.extend(p.iter().cloned());
    }
    out
}

fn strs(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn decoded(area: &'static str, name: &str, input: &str, pre: &[&str], pix_fmt: &str) -> Def {
    let mut a = strs(pre);
    a.extend(strs(&[
        "-i",
        &format!("{{ref:{input}}}"),
        "-fps_mode",
        "passthrough",
        "-f",
        "rawvideo",
        "-pix_fmt",
        pix_fmt,
        "{out}",
    ]));
    Def::digest(area, name.to_string(), FFMPEG, args(&[a]), sha)
}

fn h264_defs() -> Vec<Def> {
    let area = h264::AREA;
    let mut defs = Vec::new();
    for s in &h264::X264_STREAMS {
        let name = s.name;
        let mut encode = strs(&[
            "-frames:v",
            &s.frames.to_string(),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ]);
        encode.extend(strs(s.opts));
        encode.extend(strs(&["-f", "h264", "{out}"]));
        defs.push(
            Def::fixture(
                area,
                format!("{name}.h264"),
                FFMPEG,
                args(&[raw_input("yuv420p", s.size, "25"), encode]),
            )
            .from(move || h264::x264_source(name)),
        );
        defs.push(decoded(
            area,
            &format!("{name}.yuv"),
            &format!("{name}.h264"),
            &[],
            "yuv420p",
        ));
    }
    defs.push(Def::ours(
        area,
        "crf18.h264".into(),
        "src/h264/refs.rs crf18()",
        h264::crf18,
    ));
    defs.push(decoded(area, "crf18.yuv", "crf18.h264", STRICT, "yuv420p"));
    defs.push(Def::exact(
        area,
        "crf18.probe".into(),
        FFPROBE,
        strs(&[
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,nb_read_frames",
            "-of",
            "csv=p=0",
            "-o",
            "{out}",
            "{ref:crf18.h264}",
        ]),
    ));
    for n in 0..h264::COLOURS.len() {
        defs.push(Def::ours(
            area,
            format!("colour-{n}.h264"),
            &format!("src/h264/refs.rs colour({n})"),
            move || h264::colour(n),
        ));
        defs.push(Def::exact(
            area,
            format!("colour-{n}.probe"),
            FFPROBE,
            strs(&[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=color_range,color_primaries,color_transfer,color_space",
                "-of",
                "default=nw=1",
                "-o",
                "{out}",
                &format!("{{ref:colour-{n}.h264}}"),
            ]),
        ));
    }
    for name in h264::PINNED {
        let file = format!("{name}.h264");
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/h264/refs.rs pinned(\"{name}\")"),
            move || h264::pinned(name),
        ));
        defs.push(decoded(area, &format!("{name}.yuv"), &file, STRICT, "yuv420p"));
    }
    defs
}

fn x265_cli(f: &hevc::X265, outputs: &[&str]) -> Vec<String> {
    let (w, h) = f.size;
    let depth = if f.ten_bit { "10" } else { "8" };
    let mut a = strs(&[
        "--log-level",
        "error",
        "--input",
        "{src}",
        "--input-res",
        &format!("{w}x{h}"),
        "--fps",
        "25",
        "--frames",
        &f.frames.to_string(),
        "--input-depth",
        depth,
        "--output-depth",
        depth,
        "--recon-depth",
        depth,
    ]);
    a.extend(hevc::x265_cli(f.params));
    a.extend(strs(outputs));
    a
}

fn hevc_defs() -> Vec<Def> {
    let area = hevc::AREA;
    let mut defs = vec![Def::ours(
        area,
        hevc::SCALING_LIST.into(),
        "src/hevc/refs.rs scaling_list_text()",
        || Ok(hevc::scaling_list_text().into_bytes()),
    )];
    for f in hevc::all() {
        let name = f.name;
        let pix = hevc::pix_fmt(f.ten_bit);
        let file = format!("{name}.hevc");
        if f.recon {
            let recon = format!("{name}.recon.yuv");
            defs.push(
                Def::fixture(
                    area,
                    file,
                    X265,
                    x265_cli(f, &["-o", "{out}", "--recon", &recon]),
                )
                .from(move || hevc::source(name)),
            );
            let mut copy = raw_input(pix, f.size, "25");
            copy.pop();
            copy.push(recon);
            copy.extend(strs(&["-c", "copy", "-f", "rawvideo", "{out}"]));
            defs.push(Def::digest(
                area,
                format!("{name}.yuv"),
                FFMPEG,
                args(&[copy]),
                sha,
            ));
            continue;
        }
        let mut encode = strs(&[
            "-frames:v",
            &f.frames.to_string(),
            "-c:v",
            "libx265",
            "-pix_fmt",
            pix,
        ]);
        if !f.preset.is_empty() {
            encode.extend(strs(&["-preset", f.preset]));
        }
        let params = if f.params.is_empty() {
            "log-level=error".to_string()
        } else {
            format!("{}:log-level=error", f.params)
        };
        encode.extend(strs(&["-x265-params", &params, "-f", "hevc", "{out}"]));
        defs.push(
            Def::fixture(
                area,
                file.clone(),
                FFMPEG,
                args(&[raw_input(pix, f.size, "25"), encode]),
            )
            .from(move || hevc::source(name)),
        );
        defs.push(decoded(area, &format!("{name}.yuv"), &file, &[], pix));
    }
    for name in hevc_synth::DEBLOCKED {
        let file = hevc_synth::file(name);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/hevc/refs_synth.rs stream(\"{name}\")"),
            move || hevc_synth::stream(name),
        ));
        defs.push(decoded(
            area,
            &hevc_synth::decoded(name),
            &file,
            STRICT,
            "yuv420p",
        ));
    }
    for name in hevc::HVCC {
        defs.push(Def::fixture(
            area,
            format!("{name}.mp4"),
            FFMPEG,
            ff(&[
                "-i",
                &format!("{{ref:{name}.hevc}}"),
                "-frames:v",
                "1",
                "-c",
                "copy",
                "-tag:v",
                "hvc1",
                "{out}",
            ]),
        ));
    }
    defs
}

fn vp9_defs() -> Vec<Def> {
    let area = vp9::AREA;
    let mut defs = Vec::new();
    for f in &vp9::CONFORMANCE {
        let name = f.name;
        let file = format!("{name}.ivf");
        let mut encode = strs(&[
            "-frames:v",
            &f.frames.to_string(),
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            f.pix_fmt,
        ]);
        encode.extend(strs(f.args));
        let log = format!("{name}.passlog");
        if f.two_pass {
            let mut first = encode.clone();
            first.extend(strs(&[
                "-pass",
                "1",
                "-passlogfile",
                &log,
                "-f",
                "ivf",
                "{out}",
            ]));
            defs.push(
                Def::digest(
                    area,
                    format!("{name}.pass1-log"),
                    FFMPEG,
                    args(&[raw_input(f.pix_fmt, f.size, "25"), first]),
                    sha,
                )
                .from(move || vp9::source(name)),
            );
            encode.extend(strs(&["-pass", "2", "-passlogfile", &log]));
        }
        encode.extend(strs(&["-f", "ivf", "{out}"]));
        defs.push(
            Def::fixture(
                area,
                file.clone(),
                FFMPEG,
                args(&[raw_input(f.pix_fmt, f.size, "25"), encode]),
            )
            .from(move || vp9::source(name)),
        );
        defs.push(decoded(
            area,
            &format!("{name}.yuv"),
            &file,
            &["-c:v", "vp9"],
            f.pix_fmt,
        ));
    }
    for o in vp9::ours() {
        let name = o.name;
        let file = format!("{name}.ivf");
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/vp9/refs.rs encode_ours(\"{name}\")"),
            move || vp9::encode_ours(name),
        ));
        defs.push(decoded(
            area,
            &format!("{name}.yuv"),
            &file,
            &["-c:v", "vp9", "-err_detect", "explode", "-xerror"],
            "yuv420p",
        ));
        defs.push(decoded(
            area,
            &format!("{name}.libvpx.yuv"),
            &file,
            &["-c:v", "libvpx-vp9", "-xerror"],
            "yuv420p",
        ));
    }
    for (name, w, h) in vp9::ALPHA {
        let file = format!("{name}.webm");
        let encode = strs(&[
            "-frames:v",
            &vp9::ALPHA_FRAMES.to_string(),
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuva420p",
            "-b:v",
            "100k",
            "-g",
            "4",
            "-f",
            "webm",
            "{out}",
        ]);
        defs.push(
            Def::fixture(
                area,
                file.clone(),
                FFMPEG,
                args(&[raw_input("rgba", (w, h), "10"), encode]),
            )
            .from(move || vp9::alpha_source(name)),
        );
        defs.push(yuva(area, name, &file));
    }
    let ours = format!("{}.webm", vp9_alpha::OURS);
    defs.push(Def::ours(
        area,
        ours.clone(),
        "src/vp9/refs_alpha.rs ours_webm()",
        vp9_alpha::ours_webm,
    ));
    defs.push(yuva(area, vp9_alpha::OURS, &ours));
    let theirs = format!("{}.webm", vp9_alpha::LIBVPX);
    let encode = strs(&[
        "-frames:v",
        &vp9_alpha::LIBVPX_FRAMES.to_string(),
        "-c:v",
        "libvpx-vp9",
        "-pix_fmt",
        "yuva420p",
        "-crf",
        "30",
        "-b:v",
        "0",
        "-deadline",
        "good",
        "-cpu-used",
        "4",
        "-f",
        "webm",
        "{out}",
    ]);
    defs.push(
        Def::fixture(
            area,
            theirs.clone(),
            FFMPEG,
            args(&[raw_input("rgba", vp9_alpha::SIZE, "24"), encode]),
        )
        .from(|| Ok(vp9_alpha::source_bytes(vp9_alpha::LIBVPX_FRAMES))),
    );
    defs.push(yuva(area, vp9_alpha::LIBVPX, &theirs));
    defs
}

fn yuva(area: &'static str, name: &str, file: &str) -> Def {
    Def::digest(
        area,
        format!("{name}.yuva"),
        FFMPEG,
        ff(&[
            "-c:v",
            "libvpx-vp9",
            "-i",
            &format!("{{ref:{file}}}"),
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuva420p",
            "{out}",
        ]),
        sha,
    )
}

pub fn defs() -> Vec<Def> {
    [h264_defs(), hevc_defs(), vp9_defs()]
        .into_iter()
        .flatten()
        .collect()
}
