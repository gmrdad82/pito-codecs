use crate::{Def, FFMPEG, ff, sha};

#[path = "../../src/aac/refs.rs"]
mod aac;
#[path = "../../src/ac3/refs.rs"]
mod ac3;
#[path = "../../src/mp3/refs.rs"]
mod mp3;
#[path = "../../src/flac/refs.rs"]
mod flac;
#[path = "../../src/opus/refs.rs"]
mod opus;
#[path = "../../src/alac/refs.rs"]
mod alac;

pub fn defs() -> Vec<Def> {
    [
        aac_defs(),
        ac3_defs(),
        mp3_defs(),
        flac_defs(),
        opus_defs(),
        alac_defs(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn f32_input(rate: u32, layout: &str) -> Vec<String> {
    [
        "-f",
        "f32le",
        "-ar",
        &rate.to_string(),
        "-ch_layout",
        layout,
        "-i",
        "{src}",
    ]
    .map(str::to_string)
    .to_vec()
}

fn args(parts: &[Vec<String>]) -> Vec<String> {
    let all: Vec<&str> = parts.iter().flatten().map(String::as_str).collect();
    ff(&all)
}

fn strs(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn windowed(
    area: &'static str,
    file: String,
    input: &str,
    before: &[&str],
    filter: String,
) -> Def {
    let mut a = strs(before);
    let input = format!("{{ref:{input}}}");
    a.extend(strs(&["-i", &input, "-af", &filter, "-f", "f32le", "{out}"]));
    Def::tolerance(area, file, FFMPEG, args(&[a]))
}

fn aac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (channels, layout, kbps) in aac::STREAMS {
        let encode = strs(&[
            "-c:a",
            "aac",
            "-aac_pns",
            "0",
            "-b:a",
            &format!("{kbps}k"),
            "{out}",
        ]);
        defs.push(
            Def::fixture(
                aac::AREA,
                aac::stream(channels),
                FFMPEG,
                args(&[f32_input(aac::RATE, layout), encode]),
            )
            .from(move || Ok(aac::signal_bytes(channels))),
        );
        defs.push(windowed(
            aac::AREA,
            aac::decoded(channels),
            &aac::stream(channels),
            &[],
            aac::window::window_filter(),
        ));
    }
    defs
}

fn ac3_defs() -> Vec<Def> {
    let area = ac3::AREA;
    let mut defs = Vec::new();
    for c in ac3::cases() {
        let mut encode = strs(&["-c:a", c.codec, "-b:a", &format!("{}k", c.kbps)]);
        encode.extend(strs(c.extra));
        encode.push("{out}".into());
        let (rate, channels) = (c.rate, c.channels);
        defs.push(
            Def::fixture(
                area,
                c.stream(),
                FFMPEG,
                args(&[f32_input(rate, c.layout), encode]),
            )
            .from(move || Ok(ac3::signal(rate, channels, ac3::SAMPLES))),
        );
    }
    for c in ac3::cases() {
        defs.push(Def::digest(
            area,
            c.plain(),
            "cargo",
            strs(&[
                "test",
                "--release",
                "--lib",
                "--",
                "--ignored",
                "--exact",
                ac3::PATCH_TEST,
            ]),
            sha,
        ));
    }
    for c in ac3::cases() {
        defs.push(windowed(
            area,
            c.decoded(),
            &c.plain(),
            &["-f", c.codec],
            format!(
                "{},aformat=channel_layouts={}",
                ac3::window::window_filter(),
                c.target()
            ),
        ));
    }
    for name in ac3::BOXED {
        let c = ac3::named(name);
        for ext in ac3::BOX_EXTS {
            let a = strs(&[
                "-f",
                c.codec,
                "-i",
                &format!("{{ref:{}}}", c.plain()),
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=10:duration=0.2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.2",
                "-map",
                "1:v",
                "-map",
                "0:a",
                "-map",
                "2:a",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a:0",
                "copy",
                "-c:a:1",
                "aac",
                "-b:a:1",
                "64k",
                "{out}",
            ]);
            defs.push(Def::fixture(area, ac3::boxed(name, ext), FFMPEG, args(&[a])));
        }
    }
    defs
}

fn mp3_defs() -> Vec<Def> {
    let area = mp3::AREA;
    let mut defs = Vec::new();
    let lame = |name: &str, rate: u32, extra: &[&str], tail: &[&str]| {
        let source = mp3::signal(rate);
        let mut a = strs(&["-f", "lavfi", "-i", &source, "-c:a", "libmp3lame"]);
        a.extend(strs(extra));
        a.extend(strs(tail));
        a.push("{out}".into());
        Def::fixture(area, mp3::stream(name), FFMPEG, args(&[a]))
    };
    let decode = |file: String| {
        windowed(
            area,
            mp3::decoded(&file),
            &file,
            &["-c:a", "mp3float"],
            mp3::window::window_filter(),
        )
    };
    for (name, rate, extra) in mp3::PLAIN {
        defs.push(lame(name, rate, extra, &["-write_xing", "0"]));
        defs.push(decode(mp3::stream(name)));
    }
    for (name, rate, extra) in mp3::GAPLESS {
        defs.push(lame(name, rate, extra, &["-id3v2_version", "3"]));
        defs.push(decode(mp3::stream(name)));
    }
    let (start, end) = mp3::delay_samples();
    for ext in mp3::DELAY_EXTS {
        let file = mp3::delay(ext);
        let source = mp3::signal(mp3::DELAY_RATE);
        let video = format!("testsrc=s=64x64:r=10:d={}", mp3::SECONDS);
        let a = strs(&[
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &source,
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "160k",
            "{out}",
        ]);
        defs.push(Def::fixture(area, file.clone(), FFMPEG, args(&[a])));
        defs.push(windowed(
            area,
            mp3::decoded(&file),
            &file,
            &[],
            mp3::window::range_filter(start, end),
        ));
    }
    defs
}

fn flac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (name, exts) in flac::REMUXED {
        for ext in exts {
            let a = strs(&[
                "-i",
                "{src}",
                "-map",
                "0:a",
                "-c:a",
                "copy",
                "-f",
                flac::format(ext),
                "{out}",
            ]);
            defs.push(
                Def::fixture(flac::AREA, flac::remuxed(name, ext), FFMPEG, args(&[a]))
                    .from(move || flac::tracked(name)),
            );
        }
    }
    defs
}

fn opus_defs() -> Vec<Def> {
    let area = opus::AREA;
    let mut defs = Vec::new();
    let libopus = |file: String| {
        windowed(
            area,
            opus::decoded(&file),
            &file,
            &["-c:a", "libopus"],
            opus::window::window_filter(),
        )
    };
    for (name, source, extra, _) in opus::LIBOPUS {
        let mut a = strs(&["-f", "lavfi", "-i", source, "-c:a", "libopus"]);
        a.extend(strs(extra));
        a.push("{out}".into());
        defs.push(Def::fixture(area, opus::stream(name), FFMPEG, args(&[a])));
        defs.push(libopus(opus::stream(name)));
    }
    for (channels, bitrate) in opus::OURS {
        let file = opus::ours(channels, bitrate);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/opus/refs.rs encode_ours({channels}, {bitrate})"),
            move || opus::encode_ours(channels, bitrate),
        ));
        defs.push(libopus(file));
    }
    for (channels, bitrate) in opus::SILK {
        let file = opus::silk(channels, bitrate);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/opus/refs.rs encode_silk({channels}, {bitrate})"),
            move || opus::encode_silk(channels, bitrate),
        ));
        defs.push(libopus(file));
    }
    defs
}

fn alac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (k, c) in alac::CASES.iter().enumerate() {
        let (fmt, sample_fmt) = c.raw_format();
        let mut a = strs(&[
            "-f",
            fmt,
            "-ar",
            &c.rate.to_string(),
            "-ac",
            &c.channels.to_string(),
            "-i",
            "{src}",
            "-c:a",
            "alac",
            "-sample_fmt",
            sample_fmt,
        ]);
        a.extend(strs(c.args));
        a.push("{out}".into());
        defs.push(
            Def::fixture(alac::AREA, c.file(), FFMPEG, args(&[a]))
                .from(move || Ok(alac::raw_source(k))),
        );
    }
    defs
}
