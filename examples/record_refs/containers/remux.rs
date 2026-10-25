use crate::{Def, FFMPEG, ff};

#[allow(dead_code)]
#[path = "../../../src/mkv/refs.rs"]
mod mkv;
#[allow(dead_code)]
#[path = "../../../src/mp4/refs.rs"]
mod mp4;

const FFPROBE: &str = "ffprobe";

pub const SMOKE_HEVC: &str = "smoke-hevc.mp4";

fn listed(area: &'static str, file: &str, csv: String, entries: &str) -> Def {
    let args = [
        "-v",
        "error",
        "-show_entries",
        entries,
        "-of",
        "csv=p=0",
        "-o",
        "{out}",
        &format!("{{ref:{file}}}"),
    ];
    Def::exact(area, csv, FFPROBE, args.map(str::to_string).to_vec())
}

fn fixture(area: &'static str, file: &str, args: Vec<String>) -> Def {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Def::fixture(area, file.to_string(), FFMPEG, ff(&args))
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (file, video) in mp4::FOREIGN {
        let mut args = mp4::sources();
        args.extend(mp4::encoding(video));
        args.push("{out}".into());
        defs.push(fixture(mp4::AREA, file, args));
        defs.push(listed(mp4::AREA, file, mp4::packets_of(file), mp4::PACKETS));
    }
    for (file, codecs, muxer) in mkv::FOREIGN {
        let mut args = mkv::sources();
        args.extend(mkv::split(codecs));
        args.extend(mkv::split(muxer));
        args.push("{out}".into());
        defs.push(fixture(mkv::AREA, file, args));
        defs.push(listed(mkv::AREA, file, mkv::packets_of(file), mkv::PACKETS));
    }
    defs.push(fixture(
        "pcod",
        SMOKE_HEVC,
        [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=352x240:rate=30",
            "-frames:v",
            "10",
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-crf",
            "36",
            "-pix_fmt",
            "yuv420p",
            "-tag:v",
            "hvc1",
            "{out}",
        ]
        .map(str::to_string)
        .to_vec(),
    ));
    defs
}
