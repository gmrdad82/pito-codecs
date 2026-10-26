use pito_codecs::media::Rgba;

pub const AREA: &str = "jpeg";

pub const FORMATS: [(&str, (usize, usize)); 3] = [
    ("yuvj444p", (1, 1)),
    ("yuvj422p", (2, 1)),
    ("yuvj420p", (2, 2)),
];

pub const SIZES: [(u32, u32); 5] = [(7, 5), (33, 17), (17, 33), (64, 48), (49, 37)];

pub const VARIANTS: [(&str, &[&str]); 4] = [
    ("prog", &["-progressive"]),
    ("rst1", &["-restart", "1"]),
    ("rst3b", &["-restart", "3B"]),
    ("prog-rst2b", &["-progressive", "-restart", "2B"]),
];

pub const GREY_VARIANTS: [(&str, &[&str]); 3] = [
    ("prog", &["-progressive"]),
    ("rst1", &["-restart", "1"]),
    ("prog-rst2b", &["-progressive", "-restart", "2B"]),
];

pub const SCANS: &str = "split-scans.txt";

pub const CJPEG_SIZES: [(u32, u32); 3] = [(7, 5), (33, 17), (17, 33)];

pub type Sampling = (
    &'static str,
    &'static [&'static str],
    &'static str,
    (usize, usize),
);

pub const SAMPLINGS: [Sampling; 4] = [
    ("s1x2", &["-sample", "1x2"], "yuvj440p", (1, 2)),
    ("s4x1", &["-sample", "4x1"], "yuvj411p", (4, 1)),
    (
        "s2x2-scans",
        &["-sample", "2x2", "-scans", "{ref:split-scans.txt}"],
        "yuvj420p",
        (2, 2),
    ),
    (
        "s1x1-scans",
        &["-sample", "1x1", "-scans", "{ref:split-scans.txt}"],
        "yuvj444p",
        (1, 1),
    ),
];

pub const RGB: &str = "cj-rgb-33x17";

pub const OUR_Q80: u8 = 80;

pub const OUR_Q95: u8 = 95;

pub const EDGE_SIZES: [(u32, u32); 3] = [(1, 1), (2, 2), (16, 16)];

pub const OUR_VARIANTS: [(&str, &[&str]); 4] = [
    ("base", &[]),
    ("prog", &["-progressive"]),
    ("rst1b", &["-restart", "1B"]),
    ("prog-rst1b", &["-progressive", "-restart", "1B"]),
];

pub const OURS_TRANSCODED: [(u8, u32, u32, &[&str]); 5] = [
    (75, 7, 5, &["base", "prog"]),
    (75, 33, 17, &["base", "prog"]),
    (75, 64, 48, &["base", "prog"]),
    (90, 33, 17, &["base", "prog", "rst1b", "prog-rst1b"]),
    (85, 48, 32, &["base", "prog", "rst1b"]),
];

pub fn photo(width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    let mut s = 0x9e37_79b9u32 ^ width.wrapping_mul(131) ^ height;
    for y in 0..height {
        for x in 0..width {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (s >> 28) as i32 - 8;
            let at = ((y * width + x) * 4) as usize;
            let r = (x * 255 / width.max(1)) as i32 + n;
            let g = (y * 255 / height.max(1)) as i32 - n;
            let b = ((x + 2 * y) * 255 / (width + 2 * height)) as i32;
            img.data[at..at + 4].copy_from_slice(&[
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                b as u8,
                255,
            ]);
        }
    }
    img
}

pub fn rgb24(img: &Rgba) -> Vec<u8> {
    img.data
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}

pub fn ppm(img: &Rgba) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", img.width, img.height).into_bytes();
    out.extend_from_slice(&rgb24(img));
    out
}

pub fn ppm_pixels(file: &[u8]) -> Vec<u8> {
    let mut lines = 0;
    let at = file
        .iter()
        .position(|&b| {
            lines += usize::from(b == b'\n');
            lines == 3
        })
        .unwrap_or(file.len().saturating_sub(1));
    file.get(at + 1..).unwrap_or_default().to_vec()
}

pub fn ff(pix_fmt: &str, w: u32, h: u32) -> String {
    format!("ff-{pix_fmt}-{w}x{h}")
}

pub fn variant(base: &str, v: &str) -> String {
    format!("{base}-{v}")
}

pub fn grey(w: u32, h: u32) -> String {
    format!("grey-{w}x{h}")
}

pub fn cj(v: &str, w: u32, h: u32) -> String {
    format!("cj-{v}-{w}x{h}")
}

pub fn ours(q: u8, w: u32, h: u32) -> String {
    format!("our{q}-{w}x{h}")
}

pub fn encode_ours(q: u8, w: u32, h: u32) -> Result<Vec<u8>, String> {
    pito_codecs::jpeg::encode(&photo(w, h), q).map_err(|e| e.0)
}
