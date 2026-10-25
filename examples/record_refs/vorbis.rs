use crate::testref;
use crate::{Def, FFMPEG, ff};

#[allow(dead_code)]
#[path = "../../src/vorbis/refs.rs"]
mod refs;
#[allow(dead_code)]
#[path = "../../src/s16/refs.rs"]
mod s16;

use refs::{AREA, Case, End};

fn layout(channels: u16) -> &'static str {
    match channels {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "quad",
        5 => "5.0",
        6 => "5.1",
        7 => "6.1",
        _ => "7.1",
    }
}

fn source(c: &Case) -> String {
    let tones: Vec<String> = (0..c.channels)
        .map(|i| {
            let f = 220.0 * (i + 1) as f64 + 37.0;
            let a = 0.1 + 0.08 * i as f64;
            let click = if c.clicks {
                "+0.6*lt(mod(t+0.043\\,0.31)\\,0.0015)*sin(9000*2*PI*t)"
            } else {
                ""
            };
            format!(
                "{a}*sin({f}*2*PI*t)+0.1*sin({}*2*PI*t)*exp(-3*t){click}",
                f * 7.3
            )
        })
        .collect();
    format!(
        "aevalsrc={}:s={}:d={}:c={}",
        tones.join("|"),
        c.rate,
        c.seconds,
        layout(c.channels)
    )
}

fn encode(c: &Case, file: &str, extra: &[&str]) -> Def {
    let src = source(c);
    let mut args = vec!["-f", "lavfi", "-i", &src, "-c:a"];
    if c.native {
        args.extend(["vorbis", "-strict", "-2"]);
    } else {
        args.extend(["libvorbis", "-q:a", c.quality]);
    }
    args.extend(extra);
    args.push("{out}");
    Def::fixture(AREA, file.to_string(), FFMPEG, ff(&args))
}

fn copy(c: &Case, file: &str) -> Def {
    let input = format!("{{ref:{}}}", c.ogg());
    Def::fixture(
        AREA,
        file.to_string(),
        FFMPEG,
        ff(&["-i", &input, "-c", "copy", "-strict", "-2", "{out}"]),
    )
}

fn decoded(name: String, file: &str, filter: &str) -> Def {
    let input = format!("{{ref:{file}}}");
    Def::tolerance(
        AREA,
        name,
        FFMPEG,
        ff(&["-i", &input, "-af", filter, "-f", "f32le", "{out}"]),
    )
}

fn pcm(c: &Case, file: &str, seam: bool) -> Vec<Def> {
    let every = format!(
        "asetnsamples=n=1:p=0,aselect='not(mod(n\\,{}))'",
        c.stride()
    );
    let w = refs::window_frames(c.channels);
    let mut defs = vec![decoded(refs::every(file), file, &every)];
    defs.push(match refs::end(file) {
        End::Head => decoded(refs::head(file), file, &format!("atrim=end_sample={w}")),
        End::Tail => decoded(
            refs::tail(file),
            file,
            &format!("areverse,atrim=end_sample={w},areverse"),
        ),
    });
    if seam {
        let (a, b) = refs::seam_span(c);
        defs.push(decoded(
            refs::seam(file),
            file,
            &format!("atrim=start_sample={a}:end_sample={b}"),
        ));
    }
    defs
}

fn probe(c: &Case) -> Def {
    let input = format!("{{ref:{}}}", c.ogg());
    Def::exact(
        AREA,
        c.probe(),
        "ffprobe",
        [
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_rate,channels,bit_rate:stream_tags",
            "-of",
            "default=nw=1",
            "-o",
            "{out}",
            &input,
        ]
        .map(str::to_string)
        .to_vec(),
    )
}

fn vorbis() -> Vec<Def> {
    let mut defs = Vec::new();
    for c in &refs::CASES {
        defs.push(encode(c, &c.ogg(), &[]));
        defs.push(probe(c));
        defs.extend(pcm(c, &c.ogg(), false));
    }
    for m in &refs::MADE {
        let c = m.case();
        defs.push(if m.copied {
            copy(&c, m.file)
        } else if m.file == refs::LONG_WEBM {
            encode(&c, m.file, &["-cluster_time_limit", refs::CLUSTER_MS])
        } else {
            encode(&c, m.file, &[])
        });
        defs.extend(pcm(&c, m.file, m.file == refs::LONG_WEBM));
    }
    let full = testref::root()
        .join("tmp/record_refs")
        .join(AREA)
        .join(refs::LONG_WEBM);
    defs.push(Def::ours(
        AREA,
        refs::CUT_SHORT.into(),
        &format!("src/vorbis/refs.rs cut_short({})", refs::LONG_WEBM),
        move || refs::cut_short(&full),
    ));
    defs.extend(pcm(&refs::LONG, refs::CUT_SHORT, false));
    defs
}

fn s16_args(k: &s16::Ref) -> Vec<String> {
    let input = format!("{{ref:{}}}", k.input);
    let from = format!("{:.6}", k.from);
    let length = format!("{:.6}", k.seconds);
    let stream = format!("0:{}", k.stream);
    let rate = k.rate.to_string();
    let mut args = Vec::new();
    if k.from > 0.0 {
        args.extend(["-ss", from.as_str()]);
    }
    args.extend(["-i", input.as_str(), "-map", stream.as_str()]);
    if k.mono {
        args.extend(["-ac", "1"]);
    }
    if k.seconds > 0.0 {
        args.extend(["-t", length.as_str()]);
    }
    args.extend(["-ar", rate.as_str(), "-f", "s16le", "{out}"]);
    ff(&args)
}

fn s16() -> Vec<Def> {
    let mut defs = Vec::new();
    for f in s16::fixtures() {
        let mut args: Vec<String> = Vec::new();
        for src in &f.sources {
            args.extend(["-f", "lavfi"].map(String::from));
            if src.offset > 0.0 {
                args.extend(["-itsoffset".to_string(), format!("{}", src.offset)]);
            }
            args.extend(["-i".to_string(), src.lavfi()]);
        }
        for i in 0..f.sources.len() {
            args.extend(["-map".to_string(), format!("{i}:a")]);
        }
        args.extend(f.codecs.iter().map(|s| s.to_string()));
        if f.file.ends_with(".mp4") || f.file.ends_with(".3gp") || f.file.ends_with(".mov") {
            args.extend(["-strict", "-2"].map(String::from));
        }
        args.push("{out}".into());
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        defs.push(Def::fixture(s16::AREA, f.file.clone(), FFMPEG, ff(&args)));
    }
    for k in s16::refs() {
        defs.push(Def::tolerance(s16::AREA, k.name(), FFMPEG, s16_args(&k)));
    }
    defs
}

pub fn defs() -> Vec<Def> {
    let mut defs = vorbis();
    defs.extend(s16());
    defs
}
