use crate::{Def, FFMPEG, ff, sha};

#[allow(dead_code)]
#[path = "../../src/jpeg/refs.rs"]
mod jpeg;
#[allow(dead_code)]
#[path = "../../src/png/refs.rs"]
mod png;
#[allow(dead_code)]
#[path = "../../src/webp/refs.rs"]
mod webp;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

fn jpeg_planes(area: &'static str, jpg: &str, out: String, pix_fmt: &str) -> Def {
    let input = format!("{{ref:{jpg}}}");
    Def::tolerance(
        area,
        out,
        FFMPEG,
        ff(&[
            "-f", "jpeg_pipe", "-i", &input, "-f", "rawvideo", "-pix_fmt", pix_fmt, "{out}",
        ]),
    )
}

fn jpegtran(area: &'static str, from: &str, out: String, args: &[&str]) -> Def {
    let mut a = strings(args);
    a.extend(["-outfile".into(), "{out}".into(), format!("{{ref:{from}}}")]);
    Def::fixture(area, out, "jpegtran", a)
}

fn djpeg(area: &'static str, jpg: &str, out: String) -> Def {
    Def::tolerance(
        area,
        out,
        "djpeg",
        strings(&["-ppm", "-outfile", "{out}", &format!("{{ref:{jpg}}}")]),
    )
}

fn jpeg_defs() -> Vec<Def> {
    let area = jpeg::AREA;
    let mut defs = Vec::new();
    for (pix_fmt, _) in jpeg::FORMATS {
        for (w, h) in jpeg::SIZES {
            let base = jpeg::ff(pix_fmt, w, h);
            let size = format!("{w}x{h}");
            defs.push(
                Def::fixture(
                    area,
                    format!("{base}.jpg"),
                    FFMPEG,
                    ff(&[
                        "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &size, "-i", "{src}",
                        "-frames:v", "1", "-pix_fmt", pix_fmt, "-q:v", "2", "-f", "mjpeg", "{out}",
                    ]),
                )
                .from(move || Ok(jpeg::rgb24(&jpeg::photo(w, h)))),
            );
            defs.push(jpeg_planes(
                area,
                &format!("{base}.jpg"),
                format!("{base}.yuv"),
                pix_fmt,
            ));
            for (v, args) in jpeg::VARIANTS {
                defs.push(jpegtran(
                    area,
                    &format!("{base}.jpg"),
                    format!("{}.jpg", jpeg::variant(&base, v)),
                    args,
                ));
            }
        }
    }
    for (w, h) in jpeg::SIZES {
        let from = format!("{}.jpg", jpeg::ff("yuvj420p", w, h));
        let g = jpeg::grey(w, h);
        defs.push(jpegtran(area, &from, format!("{g}.jpg"), &["-grayscale"]));
        defs.push(jpeg_planes(area, &format!("{g}.jpg"), format!("{g}.gray"), "gray"));
        for (v, args) in jpeg::GREY_VARIANTS {
            defs.push(jpegtran(
                area,
                &format!("{g}.jpg"),
                format!("{}.jpg", jpeg::variant(&g, v)),
                args,
            ));
        }
    }
    for (w, h) in jpeg::SIZES {
        let name = jpeg::ours(jpeg::OUR_Q80, w, h);
        defs.push(Def::ours(
            area,
            format!("{name}.jpg"),
            &format!("src/jpeg/refs.rs encode_ours({}, {w}, {h})", jpeg::OUR_Q80),
            move || jpeg::encode_ours(jpeg::OUR_Q80, w, h),
        ));
        defs.push(jpeg_planes(
            area,
            &format!("{name}.jpg"),
            format!("{name}.yuv"),
            "yuvj420p",
        ));
    }
    for (q, w, h, variants) in jpeg::OURS_TRANSCODED {
        let name = jpeg::ours(q, w, h);
        defs.push(Def::ours(
            area,
            format!("{name}.jpg"),
            &format!("src/jpeg/refs.rs encode_ours({q}, {w}, {h})"),
            move || jpeg::encode_ours(q, w, h),
        ));
        for v in variants {
            let args = jpeg::OUR_VARIANTS
                .iter()
                .find(|(n, _)| n == v)
                .map_or(&[][..], |(_, a)| *a);
            defs.push(jpegtran(
                area,
                &format!("{name}.jpg"),
                format!("{}.jpg", jpeg::variant(&name, v)),
                args,
            ));
        }
    }
    defs.push(Def::ours(
        area,
        jpeg::SCANS.into(),
        "the cjpeg scan script 0; 1; 2;",
        || Ok(b"0;\n1;\n2;\n".to_vec()),
    ));
    for (w, h) in jpeg::CJPEG_SIZES {
        for (v, args, pix_fmt, _) in jpeg::SAMPLINGS {
            let name = jpeg::cj(v, w, h);
            let mut a = strings(args);
            a.extend(strings(&["-quality", "90", "-outfile", "{out}", "{src}"]));
            defs.push(
                Def::fixture(area, format!("{name}.jpg"), "cjpeg", a)
                    .from(move || Ok(jpeg::ppm(&jpeg::photo(w, h)))),
            );
            defs.push(jpeg_planes(
                area,
                &format!("{name}.jpg"),
                format!("{name}.yuv"),
                pix_fmt,
            ));
            defs.push(djpeg(area, &format!("{name}.jpg"), format!("{name}.ppm")));
        }
    }
    defs.push(
        Def::fixture(
            area,
            format!("{}.jpg", jpeg::RGB),
            "cjpeg",
            strings(&["-rgb", "-quality", "95", "-outfile", "{out}", "{src}"]),
        )
        .from(|| Ok(jpeg::ppm(&jpeg::photo(33, 17)))),
    );
    defs.push(djpeg(
        area,
        &format!("{}.jpg", jpeg::RGB),
        format!("{}.ppm", jpeg::RGB),
    ));
    defs
}

pub fn defs() -> Vec<Def> {
    let mut defs = jpeg_defs();
    defs.extend(png_defs());
    defs.extend(webp_defs());
    defs
}

fn png_defs() -> Vec<Def> {
    let area = png::AREA;
    let mut defs = Vec::new();
    for case in png::cases() {
        let name = case.name.clone();
        let png = format!("{name}.png");
        let input = format!("{{ref:{png}}}");
        let made = name.clone();
        defs.push(Def::ours(
            area,
            png.clone(),
            &format!("src/png/refs.rs file_of(\"{name}\")"),
            move || png::file_of(&made),
        ));
        let decode = |pix_fmt: &str| {
            ff(&[
                "-f", "png_pipe", "-i", &input, "-f", "rawvideo", "-pix_fmt", pix_fmt, "{out}",
            ])
        };
        defs.push(if case.wide() && !case.sub_byte_grey() {
            Def::tolerance(area, format!("{name}.rgba64"), FFMPEG, decode("rgba64be"))
        } else {
            Def::digest(area, format!("{name}.rgba"), FFMPEG, decode("rgba"), sha)
        });
    }
    defs
}

fn rgba_frames(bytes: &[u8]) -> Result<String, String> {
    Ok(crate::testref::rgba_digest(bytes))
}

fn libwebp(name: String, size: (u32, u32), rate: &str, args: &[&str]) -> Def {
    let size = format!("{}x{}", size.0, size.1);
    let mut a = vec![
        "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", rate, "-i", "{src}", "-pix_fmt",
        "bgra",
    ];
    a.extend_from_slice(args);
    a.push("{out}");
    Def::fixture(webp::AREA, name, FFMPEG, ff(&a))
}

fn still(name: &str, size: (u32, u32), args: &[&str], img: impl Fn() -> Vec<u8> + 'static) -> Def {
    let mut a = vec!["-frames:v", "1", "-c:v", "libwebp"];
    a.extend_from_slice(args);
    libwebp(format!("{name}.webp"), size, "1", &a).from(move || Ok(img()))
}

fn webp_decoded(name: &str, pix_fmt: &str, f: fn(&[u8]) -> Result<String, String>) -> Def {
    let input = format!("{{ref:{name}.webp}}");
    Def::digest(
        webp::AREA,
        format!("{name}.{pix_fmt}"),
        FFMPEG,
        ff(&[
            "-i", &input, "-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", pix_fmt,
            "{out}",
        ]),
        f,
    )
}

fn webp_defs() -> Vec<Def> {
    let area = webp::AREA;
    let mut defs = Vec::new();
    for (name, pic, size, args) in webp::LOSSLESS {
        defs.push(still(name, size, args, move || {
            webp::picture(pic, size.0, size.1).data
        }));
        defs.push(webp_decoded(name, "rgba", sha));
    }
    defs.push(
        libwebp(
            format!("{}.webp", webp::ANIM),
            webp::ANIM_SIZE,
            &(1000 / webp::ANIM_MS).to_string(),
            &["-c:v", "libwebp_anim", "-lossless", "1", "-loop", "0"],
        )
        .from(|| Ok(webp::bytes(&webp::anim_frames()))),
    );
    defs.push(webp_decoded(webp::ANIM, "rgba", rgba_frames));
    for name in webp::PINNED_LOSSLESS {
        defs.push(Def::ours(
            area,
            format!("our-ll-{name}.webp"),
            &format!("src/webp/refs.rs encode_lossless(\"{name}\")"),
            move || webp::encode_lossless(name),
        ));
        defs.push(webp_decoded(&format!("our-ll-{name}"), "rgba", sha));
    }
    for case in &webp::LOSSY {
        let q = case.quality.to_string();
        let (pic, size) = (case.pic, case.size);
        defs.push(still(&case.libwebp(), size, &["-quality", &q], move || {
            webp::picture(pic, size.0, size.1).data
        }));
        defs.push(webp_decoded(&case.libwebp(), "yuv420p", sha));
        let name = case.name;
        defs.push(Def::ours(
            area,
            format!("{}.webp", case.ours()),
            &format!("src/webp/refs.rs encode_lossy(\"{name}\")"),
            move || webp::encode_lossy(name),
        ));
        defs.push(webp_decoded(&case.ours(), "yuv420p", sha));
    }
    let q = webp::ALPHA_QUALITY.to_string();
    defs.push(still(
        &format!("lw-{}", webp::LOSSY_ALPHA),
        webp::ALPHA_SIZE,
        &["-quality", &q],
        || webp::alpha_image().data,
    ));
    defs.push(Def::ours(
        area,
        format!("{}.webp", webp::LOSSY_ALPHA),
        "src/webp/refs.rs encode_alpha()",
        webp::encode_alpha,
    ));
    defs.push(webp_decoded(webp::LOSSY_ALPHA, "yuva420p", sha));
    let q = webp::LOSSY_ANIM_QUALITY.to_string();
    for i in 0..webp::LOSSY_ANIM_FRAMES {
        defs.push(still(
            &webp::lossy_anim_still(i),
            webp::LOSSY_ANIM_SIZE,
            &["-quality", &q],
            move || webp::lossy_anim_frames()[i as usize].data.clone(),
        ));
    }
    for (name, pic, profile, slices, sharpness, (w, h), qmin) in webp::LIBVPX {
        let size = format!("{w}x{h}");
        defs.push(
            Def::fixture(
                area,
                format!("{name}.ivf"),
                FFMPEG,
                ff(&[
                    "-f", "rawvideo", "-pix_fmt", "rgba", "-s", &size, "-r", "1", "-i", "{src}",
                    "-frames:v", "1", "-c:v", "libvpx", "-profile:v", profile, "-slices", slices,
                    "-sharpness", sharpness, "-qmin", qmin, "-qmax", "63", "-b:v", "50k", "-f",
                    "ivf", "{out}",
                ]),
            )
            .from(move || Ok(webp::picture(pic, w, h).data)),
        );
        let input = format!("{{ref:{name}.ivf}}");
        defs.push(Def::digest(
            area,
            format!("{name}.yuv"),
            FFMPEG,
            ff(&[
                "-i", &input, "-frames:v", "1", "-pix_fmt", "yuv420p", "-f", "rawvideo", "{out}",
            ]),
            sha,
        ));
    }
    defs
}
