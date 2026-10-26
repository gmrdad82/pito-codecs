use crate::testref;
use crate::{Def, sha, tool};

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

fn worked(area: &str, file: &str) -> Result<Vec<u8>, String> {
    let at = testref::root()
        .join("tmp/record_refs")
        .join(area)
        .join(file);
    std::fs::read(&at).map_err(|e| format!("{}: {e}", at.display()))
}

fn djpeg_planes(area: &'static str, jpg: String) -> Def {
    let out = jpeg::planes(jpg.trim_end_matches(".jpg"));
    let args = tool(
        "djpeg",
        &[
            "-nosmooth",
            "-dct",
            "int",
            "-pnm",
            "-outfile",
            "{out}",
            "{src}",
        ],
    );
    Def::tolerance(area, out, "djpeg", args).from(move || jpeg::as_rgb(&worked(area, &jpg)?))
}

fn jpegtran(area: &'static str, from: &str, out: String, args: &[&str]) -> Def {
    let mut a = strings(args);
    a.extend(["-outfile".into(), "{out}".into(), format!("{{ref:{from}}}")]);
    Def::fixture(
        area,
        out,
        "jpegtran",
        tool(
            "jpegtran",
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
        ),
    )
}

fn djpeg(area: &'static str, jpg: &str, out: String) -> Def {
    Def::tolerance(
        area,
        out,
        "djpeg",
        tool(
            "djpeg",
            &["-pnm", "-outfile", "{out}", &format!("{{ref:{jpg}}}")],
        ),
    )
}

fn cjpeg(
    area: &'static str,
    out: String,
    args: &[&str],
    img: impl Fn() -> Vec<u8> + 'static,
) -> Def {
    let mut a = args.to_vec();
    a.extend(["-outfile", "{out}", "{src}"]);
    Def::fixture(area, out, "cjpeg", tool("cjpeg", &a)).from(move || Ok(img()))
}

fn jpeg_defs() -> Vec<Def> {
    let area = jpeg::AREA;
    let mut defs = Vec::new();
    for (sampling, sample, _) in jpeg::FORMATS {
        for (w, h) in jpeg::SIZES {
            let base = jpeg::base(sampling, w, h);
            let mut args = sample.to_vec();
            args.extend(["-quality", jpeg::BASE_QUALITY]);
            defs.push(cjpeg(area, format!("{base}.jpg"), &args, move || {
                jpeg::ppm(&jpeg::photo(w, h))
            }));
            defs.push(djpeg_planes(area, format!("{base}.jpg")));
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
        let from = format!("{}.jpg", jpeg::base("420", w, h));
        let g = jpeg::grey(w, h);
        defs.push(jpegtran(area, &from, format!("{g}.jpg"), &["-grayscale"]));
        defs.push(djpeg(area, &format!("{g}.jpg"), format!("{g}.pgm")));
        for (v, args) in jpeg::GREY_VARIANTS {
            defs.push(jpegtran(
                area,
                &format!("{g}.jpg"),
                format!("{}.jpg", jpeg::variant(&g, v)),
                args,
            ));
        }
    }
    let q80 = jpeg::SIZES.map(|s| (jpeg::OUR_Q80, s));
    let q95 = jpeg::EDGE_SIZES.map(|s| (jpeg::OUR_Q95, s));
    for (q, (w, h)) in q80.into_iter().chain(q95) {
        let name = jpeg::ours(q, w, h);
        defs.push(Def::ours(
            area,
            format!("{name}.jpg"),
            &format!("src/jpeg/refs.rs encode_ours({q}, {w}, {h})"),
            move || jpeg::encode_ours(q, w, h),
        ));
        defs.push(djpeg_planes(area, format!("{name}.jpg")));
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
        for (v, args, _) in jpeg::SAMPLINGS {
            let name = jpeg::cj(v, w, h);
            let mut a = args.to_vec();
            a.extend(["-quality", "90"]);
            defs.push(cjpeg(area, format!("{name}.jpg"), &a, move || {
                jpeg::ppm(&jpeg::photo(w, h))
            }));
            defs.push(djpeg_planes(area, format!("{name}.jpg")));
            defs.push(djpeg(area, &format!("{name}.jpg"), format!("{name}.ppm")));
        }
    }
    defs.push(cjpeg(
        area,
        format!("{}.jpg", jpeg::RGB),
        &["-rgb", "-quality", "95"],
        || jpeg::ppm(&jpeg::photo(33, 17)),
    ));
    defs.push(djpeg(
        area,
        &format!("{}.jpg", jpeg::RGB),
        format!("{}.ppm", jpeg::RGB),
    ));
    let (w, h) = jpeg::LIBJPEG_SIZE;
    for q in jpeg::LIBJPEG_QUALITIES {
        let qs = q.to_string();
        defs.push(cjpeg(
            area,
            format!("{}.jpg", jpeg::libjpeg(q)),
            &["-quality", &qs, "-sample", "2x2"],
            move || jpeg::ppm(&jpeg::photo_like(w, h)),
        ));
    }
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
        let file = format!("{name}.png");
        let input = format!("png:{{ref:{file}}}");
        let made = name.clone();
        defs.push(Def::ours(
            area,
            file,
            &format!("src/png/refs.rs file_of(\"{name}\")"),
            move || png::file_of(&made),
        ));
        defs.push(if case.wide() && !case.sub_byte_grey() {
            let args = tool(
                "magick",
                &[&input, "-depth", "16", "-endian", "MSB", "rgba:{out}"],
            );
            Def::tolerance(area, format!("{name}.rgba64"), "magick", args)
        } else {
            let args = tool("magick", &[&input, "-depth", "8", "rgba:{out}"]);
            Def::digest(area, format!("{name}.rgba"), "magick", args, sha)
        });
    }
    defs
}

fn webp_bytes(name: &str) -> Result<Vec<u8>, String> {
    worked(webp::AREA, &format!("{name}.webp"))
}

fn magick_webp(
    name: &str,
    size: (u32, u32),
    ops: &[&str],
    frames: impl Fn() -> Vec<u8> + 'static,
) -> Def {
    let size = format!("{}x{}", size.0, size.1);
    let mut a = vec!["-size", &size, "-depth", "8", "rgba:{src}"];
    a.extend_from_slice(ops);
    a.push("{out}");
    Def::fixture(
        webp::AREA,
        format!("{name}.webp"),
        "magick",
        tool("magick", &a),
    )
    .from(move || Ok(frames()))
}

fn still(name: &str, size: (u32, u32), ops: &[&str], img: impl Fn() -> Vec<u8> + 'static) -> Def {
    let mut a = vec!["-define", "webp:exact=true"];
    a.extend_from_slice(ops);
    magick_webp(name, size, &a, img)
}

fn by_libwebp(
    name: String,
    what: &str,
    decode: impl Fn() -> Result<Vec<u8>, String> + 'static,
) -> Def {
    Def::ours(webp::AREA, name, what, decode).library("libwebp")
}

fn rgba_of(name: String, file: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
    let what = format!("libwebp WebPDecodeRGBA of {name}.webp");
    by_libwebp(format!("{name}.rgba"), &what, move || {
        Ok(libwebp::open()?.decode_rgba(&file()?)?.2)
    })
}

fn yuva_of(name: String, ext: &str, file: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
    let alpha = ext == "yuva420p";
    let what =
        format!("libwebp WebPDecodeYUVInto of {name}.webp, then the alpha of WebPDecodeRGBA");
    by_libwebp(format!("{name}.{ext}"), &what, move || {
        let lib = libwebp::open()?;
        let data = file()?;
        let mut out = lib.decode_yuv(&data)?.concat();
        if alpha {
            out.extend(lib.decode_rgba(&data)?.2.chunks_exact(4).map(|p| p[3]));
        }
        Ok(out)
    })
}

fn i420(img: &pito_codecs::media::Rgba) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let px = |x: usize, y: usize| {
        let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
        testref::yuv_of([img.data[i], img.data[i + 1], img.data[i + 2]])
    };
    let mut out: Vec<u8> = (0..w * h).map(|i| px(i % w, i / w)[0]).collect();
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    for c in 1..3 {
        out.extend((0..cw * ch).map(|i| {
            let (x, y) = (i % cw * 2, i / cw * 2);
            let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .map(|&(dx, dy)| u32::from(px(x + dx, y + dy)[c]))
                .sum();
            ((sum + 2) / 4) as u8
        }));
    }
    out
}

fn webp_defs() -> Vec<Def> {
    let area = webp::AREA;
    let mut defs = Vec::new();
    for (name, pic, size, ops) in webp::LOSSLESS {
        defs.push(still(name, size, ops, move || {
            webp::picture(pic, size.0, size.1).data
        }));
        defs.push(rgba_of(name.into(), move || webp_bytes(name)));
    }
    let delay = (webp::ANIM_MS / 10).to_string();
    defs.push(magick_webp(
        webp::ANIM,
        webp::ANIM_SIZE,
        &[
            "-delay",
            &delay,
            "-loop",
            "0",
            "-define",
            "webp:lossless=true",
        ],
        || webp::bytes(&webp::anim_frames()),
    ));
    defs.push(Def::digest(
        area,
        format!("{}.rgba", webp::ANIM),
        "magick",
        tool(
            "magick",
            &[
                &format!("{{ref:{}.webp}}", webp::ANIM),
                "-coalesce",
                "-depth",
                "8",
                "rgba:{out}",
            ],
        ),
        |b| Ok(testref::rgba_digest(b)),
    ));
    for name in webp::PINNED_LOSSLESS {
        defs.push(Def::ours(
            area,
            format!("our-ll-{name}.webp"),
            &format!("src/webp/refs.rs encode_lossless(\"{name}\")"),
            move || webp::encode_lossless(name),
        ));
        defs.push(rgba_of(format!("our-ll-{name}"), move || {
            webp::encode_lossless(name)
        }));
    }
    for case in &webp::LOSSY {
        let q = case.quality.to_string();
        let (pic, size, name) = (case.pic, case.size, case.name);
        let lw = case.libwebp();
        defs.push(still(&lw, size, &["-quality", &q], move || {
            webp::picture(pic, size.0, size.1).data
        }));
        let file = lw.clone();
        defs.push(yuva_of(lw, "yuv420p", move || webp_bytes(&file)));
        defs.push(Def::ours(
            area,
            format!("{}.webp", case.ours()),
            &format!("src/webp/refs.rs encode_lossy(\"{name}\")"),
            move || webp::encode_lossy(name),
        ));
        defs.push(yuva_of(case.ours(), "yuv420p", move || {
            webp::encode_lossy(name)
        }));
    }
    let q = webp::ALPHA_QUALITY.to_string();
    let lw = format!("lw-{}", webp::LOSSY_ALPHA);
    defs.push(still(&lw, webp::ALPHA_SIZE, &["-quality", &q], || {
        webp::alpha_image().data
    }));
    let file = lw.clone();
    defs.push(yuva_of(lw, "yuva420p", move || webp_bytes(&file)));
    defs.push(Def::ours(
        area,
        format!("{}.webp", webp::LOSSY_ALPHA),
        "src/webp/refs.rs encode_alpha()",
        webp::encode_alpha,
    ));
    defs.push(yuva_of(
        webp::LOSSY_ALPHA.into(),
        "yuva420p",
        webp::encode_alpha,
    ));
    let q = webp::LOSSY_ANIM_QUALITY.to_string();
    for i in 0..webp::LOSSY_ANIM_FRAMES {
        let still_name = webp::lossy_anim_still(i);
        defs.push(still(
            &still_name,
            webp::LOSSY_ANIM_SIZE,
            &["-quality", &q],
            move || webp::lossy_anim_frames()[i as usize].data.clone(),
        ));
        let file = still_name.clone();
        defs.push(yuva_of(still_name, "yuv420p", move || webp_bytes(&file)));
    }
    for (name, pic, profile, parts, sharpness, (w, h), qmin) in webp::LIBVPX {
        let (ws, hs) = (w.to_string(), h.to_string());
        let args = tool(
            "vpxenc",
            &[
                "--codec=vp8",
                "--i420",
                "-w",
                &ws,
                "-h",
                &hs,
                "--fps=1/1",
                "--limit=1",
                "--passes=1",
                "--threads=1",
                &format!("--profile={profile}"),
                &format!("--token-parts={parts}"),
                &format!("--sharpness={sharpness}"),
                &format!("--min-q={qmin}"),
                "--max-q=63",
                "--target-bitrate=50",
                "--ivf",
                "--disable-warning-prompt",
                "-q",
                "-o",
                "{out}",
                "{src}",
            ],
        );
        defs.push(
            Def::fixture(area, format!("{name}.ivf"), "vpxenc", args)
                .from(move || Ok(i420(&webp::picture(pic, w, h)))),
        );
        let input = format!("{{ref:{name}.ivf}}");
        defs.push(Def::digest(
            area,
            format!("{name}.yuv"),
            "vpxdec",
            tool(
                "vpxdec",
                &["--i420", "--rawvideo", "--limit=1", "-o", "{out}", &input],
            ),
            sha,
        ));
    }
    defs
}

mod libwebp {
    use std::ffi::{c_int, c_void};

    type GetInfo = unsafe extern "C" fn(*const u8, usize, *mut c_int, *mut c_int) -> c_int;
    type DecodeRgba = unsafe extern "C" fn(*const u8, usize, *mut c_int, *mut c_int) -> *mut u8;
    type DecodeYuvInto = unsafe extern "C" fn(
        *const u8,
        usize,
        *mut u8,
        usize,
        c_int,
        *mut u8,
        usize,
        c_int,
        *mut u8,
        usize,
        c_int,
    ) -> *mut u8;
    type Free = unsafe extern "C" fn(*mut c_void);

    pub struct Lib(libloading::Library);

    pub fn open() -> Result<Lib, String> {
        let tools = crate::testref::Tools::load()?;
        let crate::testref::Probe::Library { path, .. } = &tools.pin("libwebp")?.probe else {
            return Err("libwebp is pinned as a command, not a library".into());
        };
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| format!("{path}: {e}"))?;
        Ok(Lib(lib))
    }

    impl Lib {
        fn sym<T>(&self, name: &str) -> Result<libloading::Symbol<'_, T>, String> {
            unsafe { self.0.get(name.as_bytes()) }
                .map_err(|e| format!("libwebp has no {name}: {e}"))
        }

        pub fn size(&self, data: &[u8]) -> Result<(usize, usize), String> {
            let f = self.sym::<GetInfo>("WebPGetInfo")?;
            let (mut w, mut h) = (0, 0);
            if unsafe { f(data.as_ptr(), data.len(), &mut w, &mut h) } == 0 {
                return Err("WebPGetInfo refused the file".into());
            }
            Ok((w as usize, h as usize))
        }

        pub fn decode_rgba(&self, data: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
            let f = self.sym::<DecodeRgba>("WebPDecodeRGBA")?;
            let free = self.sym::<Free>("WebPFree")?;
            let (mut w, mut h) = (0, 0);
            let p = unsafe { f(data.as_ptr(), data.len(), &mut w, &mut h) };
            if p.is_null() {
                return Err("WebPDecodeRGBA refused the file".into());
            }
            let n = w as usize * h as usize * 4;
            let out = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
            unsafe { free(p.cast()) };
            Ok((w as usize, h as usize, out))
        }

        pub fn decode_yuv(&self, data: &[u8]) -> Result<[Vec<u8>; 3], String> {
            let (w, h) = self.size(data)?;
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let mut y = vec![0u8; w * h];
            let mut u = vec![0u8; cw * ch];
            let mut v = vec![0u8; cw * ch];
            let f = self.sym::<DecodeYuvInto>("WebPDecodeYUVInto")?;
            let p = unsafe {
                f(
                    data.as_ptr(),
                    data.len(),
                    y.as_mut_ptr(),
                    y.len(),
                    w as c_int,
                    u.as_mut_ptr(),
                    u.len(),
                    cw as c_int,
                    v.as_mut_ptr(),
                    v.len(),
                    cw as c_int,
                )
            };
            if p.is_null() {
                return Err("WebPDecodeYUVInto refused the file".into());
            }
            Ok([y, u, v])
        }
    }
}
