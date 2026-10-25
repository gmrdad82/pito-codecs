use crate::{Def, FFMPEG, ff};

#[allow(dead_code)]
#[path = "../../src/filter/refs.rs"]
mod refs;

#[allow(dead_code)]
#[path = "../../src/media/front/refs.rs"]
mod front;

use refs::AREA;

fn raw(pix_in: &str, (w, h): (u32, u32), vf: &str, pix_out: &str) -> Vec<String> {
    let size = format!("{w}x{h}");
    ff(&[
        "-f", "rawvideo", "-pix_fmt", pix_in, "-s", &size, "-i", "{src}", "-vf", vf, "-f",
        "rawvideo", "-pix_fmt", pix_out, "{out}",
    ])
}

fn from_yuv(file: String, size: (u32, u32), vf: &str, pix_out: &str) -> Def {
    Def::tolerance(AREA, file, FFMPEG, raw("yuv420p", size, vf, pix_out))
        .from(move || Ok(refs::yuv_bytes(size, false)))
}

fn from_packed(file: String, pix_in: &str, vf: &str, bgr: bool) -> Def {
    let size = refs::COLOUR_SIZE;
    Def::tolerance(AREA, file, FFMPEG, raw(pix_in, size, vf, "yuv420p")).from(move || {
        Ok(if bgr {
            refs::bgra_bytes(size)
        } else {
            refs::rgba_bytes(size)
        })
    })
}

fn scales() -> Vec<Def> {
    refs::SCALES
        .iter()
        .map(|&(kernel, w, h, deep)| {
            let pix = refs::pix(deep);
            let vf = format!("zscale=w={w}:h={h}:filter={kernel}:chromal=left:chromalin=left");
            Def::tolerance(
                AREA,
                refs::scale_ref(kernel, w, h, deep),
                FFMPEG,
                raw(pix, refs::SCALE_SIZE, &vf, pix),
            )
            .from(move || Ok(refs::yuv_bytes(refs::SCALE_SIZE, deep)))
        })
        .collect()
}

fn colours() -> Vec<Def> {
    let size = refs::COLOUR_SIZE;
    let mut defs = Vec::new();
    for (matrix, range) in refs::COLOURS {
        defs.push(from_yuv(
            refs::rgba_ref(matrix, range),
            size,
            &format!(
                "scale=in_color_matrix={matrix}:in_range={range}:flags=neighbor+accurate_rnd+full_chroma_int"
            ),
            "rgba",
        ));
        defs.push(from_packed(
            refs::yuv_ref(matrix, range),
            "rgba",
            &format!("scale=out_color_matrix={matrix}:out_range={range}:flags=area+accurate_rnd"),
            false,
        ));
    }
    defs.push(from_yuv(
        refs::CONVERT_10BIT.into(),
        size,
        "colorspace=ispace=bt470bg:irange=tv:iprimaries=bt470bg:itrc=bt709:space=bt709:range=pc:primaries=bt470bg:trc=bt709:format=yuv420p10",
        "yuv420p10le",
    ));
    defs
}

fn orders() -> Vec<Def> {
    let size = refs::COLOUR_SIZE;
    let mut defs = vec![
        from_yuv(
            refs::BGRA.into(),
            size,
            "scale=in_color_matrix=bt709:in_range=tv:flags=neighbor+accurate_rnd+full_chroma_int",
            "bgra",
        ),
        from_packed(
            refs::BGR0_TO_YUV.into(),
            "bgr0",
            "scale=out_color_matrix=bt709:out_range=tv:flags=area+accurate_rnd",
            true,
        ),
    ];
    for siting in refs::SITINGS {
        defs.push(from_yuv(
            refs::zscale_rgba_ref(siting),
            size,
            &format!(
                "zscale=min=709:rin=limited:tin=709:pin=709:t=709:p=709:m=gbr:r=full:chromalin={siting}:filter=bilinear,format=gbrp"
            ),
            "rgba",
        ));
    }
    defs.push(from_packed(
        refs::RGBA_TO_YUV_LEFT.into(),
        "rgba",
        "format=gbrp,zscale=rin=full:tin=709:pin=709:m=709:r=limited:t=709:p=709:chromal=left:filter=bilinear,format=yuv420p",
        false,
    ));
    defs.push(from_yuv(
        refs::SCALE_CENTRE.into(),
        refs::SCALE_SIZE,
        &format!(
            "zscale=w={}:h={}:filter=lanczos:chromal=center:chromalin=center",
            refs::CENTRE_SIZE.0,
            refs::CENTRE_SIZE.1
        ),
        "yuv420p",
    ));
    defs
}

fn loudness() -> Vec<Def> {
    refs::loudness()
        .into_iter()
        .map(|(label, rate, seconds, sound)| {
            let frames = refs::loudness_frames(rate, seconds);
            let af = format!(
                "asetnsamples=n={frames}:p=0,ebur128=peak=true:framelog=quiet:metadata=1,ametadata=mode=print:file={{out}}"
            );
            let args = ff(&[
                "-f",
                "f32le",
                "-ar",
                &rate.to_string(),
                "-ac",
                &refs::CHANNELS.to_string(),
                "-i",
                "{src}",
                "-af",
                &af,
                "-f",
                "null",
                "-",
            ]);
            Def::exact(AREA, refs::loudness_ref(label), FFMPEG, args)
                .from(move || Ok(refs::loudness_bytes(sound, rate, seconds)))
        })
        .collect()
}

fn edges() -> Vec<Def> {
    front::FIXTURES
        .iter()
        .map(|f| Def::fixture(front::AREA, f.name.into(), FFMPEG, ff_owned(front::args(f))))
        .collect()
}

fn ff_owned(args: Vec<String>) -> Vec<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ff(&args)
}

pub fn defs() -> Vec<Def> {
    [scales(), colours(), orders(), loudness(), edges()]
        .into_iter()
        .flatten()
        .collect()
}
