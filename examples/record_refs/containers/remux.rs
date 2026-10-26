use super::mux::{self, Csp, Timed};
use crate::{Def, OURS, sha, tool, words};
use pito_codecs::mp4;

#[allow(dead_code)]
#[path = "../../../src/mkv/refs.rs"]
mod mkv;
#[allow(dead_code)]
#[path = "../../../src/mp4/refs.rs"]
mod mp4refs;

fn source(size: (u32, u32), fps: u32, frames: u64) -> impl Fn() -> Result<Vec<u8>, String> {
    move || {
        Ok(mux::y4m(
            size,
            (fps, 1),
            Csp::I420,
            8,
            frames,
            &mux::pattern(size),
        ))
    }
}

fn x264_args(file: &str, extra: &[&str]) -> Vec<String> {
    let muxer = if file.ends_with(".flv") { "flv" } else { "mkv" };
    let mut args = vec![
        "--demuxer",
        "y4m",
        "--muxer",
        muxer,
        "--threads",
        "1",
        "--preset",
        "ultrafast",
        "--no-scenecut",
        "--quiet",
        "--no-progress",
    ];
    args.extend_from_slice(extra);
    args.extend_from_slice(&["-o", "{out}", "{src}"]);
    tool("x264", &args)
}

pub fn x264(area: &'static str, file: &str, extra: &[&str]) -> Def {
    Def::fixture(area, file.into(), "x264", x264_args(file, extra))
}

pub fn x264_mp4(
    area: &'static str,
    file: &str,
    fps: u32,
    extra: &[&str],
    make: impl Fn() -> Result<Vec<u8>, String> + 'static,
) -> Vec<Def> {
    let mkv = format!("{file}.mkv");
    let what = format!("{{ref:{mkv}}}'s frames at their Matroska times in our MP4 writer");
    let from = mkv.clone();
    vec![
        Def::digest(area, mkv.clone(), "x264", x264_args(&mkv, extra), sha).from(make),
        Def::fixture(area, file.into(), OURS, words(&what))
            .from(move || mux::x264_mp4(&mux::work(area, &from), fps)),
    ]
}

fn vpx(file: &str) -> Def {
    let gop = format!("--kf-max-dist={}", mkv::GOP);
    let min = format!("--kf-min-dist={}", mkv::GOP);
    let form = if file.ends_with(".ivf") {
        "--ivf"
    } else {
        "--webm"
    };
    let args = [
        "--codec=vp9",
        "--disable-warning-prompt",
        "--debug",
        "--rt",
        "--cpu-used=8",
        "--threads=1",
        "--lag-in-frames=0",
        "--end-usage=cbr",
        "--target-bitrate=40",
        &min,
        &gop,
        form,
        "--quiet",
        "-o",
        "{out}",
        "{src}",
    ];
    let frames = u64::from(mkv::FPS * mkv::SECONDS);
    Def::fixture(mkv::AREA, file.into(), "vpxenc", tool("vpxenc", &args)).from(source(
        mkv::SIZE,
        mkv::FPS,
        frames,
    ))
}

fn mkv_defs() -> Vec<Def> {
    let frames = u64::from(mkv::FPS * mkv::SECONDS);
    let gop = mkv::GOP.to_string();
    let h264 = |file: &str| {
        x264(
            mkv::AREA,
            file,
            &[
                "--bframes",
                "2",
                "--keyint",
                &gop,
                "--min-keyint",
                &gop,
                "--crf",
                "40",
            ],
        )
        .from(source(mkv::SIZE, mkv::FPS, frames))
    };
    vec![
        h264("h264.mkv"),
        h264("h264.flv"),
        vpx("vp9.webm"),
        vpx("vp9.ivf"),
        Def::fixture(
            mkv::AREA,
            "piped.mkv".into(),
            OURS,
            words("{ref:h264.mkv} with its Segment and Cluster sizes unknown and its SeekHead and Cues voided, as a live writer leaves it"),
        )
        .from(|| mux::unseekable(&mux::read(mkv::AREA, "h264.mkv")?)),
    ]
}

fn mp4_defs() -> Vec<Def> {
    let area = mp4refs::AREA;
    let frames = u64::from(mp4refs::FPS * mp4refs::SECONDS);
    let size = mp4refs::SIZE;
    let bframes = ["--bframes", "3", "--keyint", "30", "--crf", "40"];
    let kbps = mp4refs::AUDIO_KBPS.to_string();
    let faac = |file: &str| {
        Def::fixture(
            area,
            file.into(),
            "faac",
            tool(
                "faac",
                &[
                    "-P", "-R", "48000", "-B", "16", "-C", "2", "-b", &kbps, "-o", "{out}", "{src}",
                ],
            ),
        )
        .from(|| {
            let tone = crate::testref::pcm(
                crate::testref::Sound::Sine {
                    hz: 440.0,
                    amp: 0.25,
                },
                48_000,
                2,
                0,
                48_000,
            );
            Ok(crate::testref::s16_of(&tone)
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect())
        })
    };
    let csv = format!("{}.csv", mp4refs::HEVC_STREAM);
    let n = frames.to_string();
    let res = format!("{}x{}", size.0, size.1);
    let fps = mp4refs::FPS.to_string();
    let x265 = Def::digest(
        area,
        mp4refs::HEVC_STREAM.into(),
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
                "i420",
                "--frames",
                &n,
                "--preset",
                "ultrafast",
                "--bframes",
                "4",
                "--keyint",
                "30",
                "--no-scenecut",
                "--crf",
                "40",
                "--frame-threads",
                "1",
                "--no-wpp",
                "--pools",
                "none",
                "--log-level",
                "error",
                "--no-info",
                "--no-progress",
                "--csv",
                &csv,
                "--csv-log-level",
                "1",
                "--output",
                "{out}",
            ],
        ),
        sha,
    )
    .from(move || {
        Ok(mux::raw_video(
            size,
            Csp::I420,
            8,
            frames,
            &mux::pattern(size),
        ))
    });
    let pocs = move || -> Result<Vec<i64>, String> {
        mux::x265_pocs(&mux::read(area, &format!("{}.csv", mp4refs::HEVC_STREAM))?)
    };
    let mut defs = x264_mp4(
        area,
        mp4refs::BFRAMES,
        mp4refs::FPS,
        &bframes,
        source(size, mp4refs::FPS, frames),
    );
    defs.push(x264(area, mp4refs::BFRAMES_TWIN, &bframes).from(source(size, mp4refs::FPS, frames)));
    defs.extend([
        faac(mp4refs::TONE),
        faac(mp4refs::TONE_TWIN),
        x265,
        Def::fixture(
            area,
            mp4refs::HEVC.into(),
            OURS,
            words("{ref:hevc.hevc} in our MOV writer, its times from the x265 csv's POCs"),
        )
        .from(move || {
            let coded = mux::hevc(&mux::read(area, mp4refs::HEVC_STREAM)?)?;
            let pocs = pocs()?;
            Timed {
                coded: &coded,
                pocs: &pocs,
                size,
                fps: mp4refs::FPS,
            }
            .hevc_mp4(mp4::Brand::Mov)
        }),
        Def::exact(
            area,
            mp4refs::HEVC_POCS.into(),
            OURS,
            words("the POC of each frame of {ref:hevc.hevc} in decode order, from its x265 csv"),
        )
        .from(move || {
            let text: Vec<String> = pocs()?.iter().map(i64::to_string).collect();
            Ok(format!("{}\n", text.join(" ")).into_bytes())
        }),
    ]);
    defs
}

pub fn defs() -> Vec<Def> {
    let mut defs = mkv_defs();
    defs.extend(mp4_defs());
    defs
}
