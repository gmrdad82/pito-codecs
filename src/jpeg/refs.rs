use pito_codecs::media::Rgba;

pub const AREA: &str = "jpeg";

pub type Base = (&'static str, &'static [&'static str], (usize, usize));

pub const FORMATS: [Base; 3] = [
    ("444", &["-sample", "1x1"], (1, 1)),
    ("422", &["-sample", "2x1"], (2, 1)),
    ("420", &["-sample", "2x2"], (2, 2)),
];

pub const BASE_QUALITY: &str = "95";

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

pub type Sampling = (&'static str, &'static [&'static str], (usize, usize));

pub const SAMPLINGS: [Sampling; 4] = [
    ("s1x2", &["-sample", "1x2"], (1, 2)),
    ("s4x1", &["-sample", "4x1"], (4, 1)),
    (
        "s2x2-scans",
        &["-sample", "2x2", "-scans", "{ref:split-scans.txt}"],
        (2, 2),
    ),
    (
        "s1x1-scans",
        &["-sample", "1x1", "-scans", "{ref:split-scans.txt}"],
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

pub fn base(sampling: &str, w: u32, h: u32) -> String {
    format!("cj{sampling}-{w}x{h}")
}

pub fn planes(name: &str) -> String {
    format!("{name}.planes.ppm")
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

pub const ADOBE_RGB: [u8; 16] = [
    0xff, 0xee, 0, 14, b'A', b'd', b'o', b'b', b'e', 0, 100, 0, 0, 0, 0, 0,
];

pub fn as_rgb(jpg: &[u8]) -> Result<Vec<u8>, String> {
    if !jpg.starts_with(&[0xff, 0xd8]) {
        return Err("not a JPEG: no SOI".into());
    }
    let mut out = vec![0xff, 0xd8];
    out.extend_from_slice(&ADOBE_RGB);
    let mut at = 2;
    loop {
        let (Some(&0xff), Some(&marker)) = (jpg.get(at), jpg.get(at + 1)) else {
            return Err(format!("no marker at byte {at}"));
        };
        if marker == 0xda {
            out.extend_from_slice(&jpg[at..]);
            return Ok(out);
        }
        let len = jpg
            .get(at + 2..at + 4)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or_else(|| format!("a segment cut short at byte {at}"))?;
        let end = at + 2 + len;
        let segment = jpg
            .get(at..end)
            .ok_or_else(|| format!("segment {marker:#x} runs past the end"))?;
        if !matches!(marker, 0xe0 | 0xee) {
            out.extend_from_slice(segment);
        }
        at = end;
    }
}

pub fn planes_of(pixels: &[u8], (w, h): (u32, u32), (fx, fy): (usize, usize)) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(fx), h.div_ceil(fy));
    let at = |x: usize, y: usize, c: usize| pixels.get((y * w + x) * 3 + c).copied();
    let mut out: Vec<u8> = (0..w * h).filter_map(|i| at(i % w, i / w, 0)).collect();
    for c in 1..3 {
        out.extend((0..cw * ch).filter_map(|i| at(i % cw * fx, i / cw * fy, c)));
    }
    out
}

pub const LIBJPEG_SIZE: (u32, u32) = (256, 160);
pub const LIBJPEG_QUALITIES: [u8; 4] = [50, 75, 85, 90];

pub fn libjpeg(q: u8) -> String {
    let (w, h) = LIBJPEG_SIZE;
    format!("cj-photo-q{q}-{w}x{h}")
}

fn tri(t: u32) -> u8 {
    let t = t % 510;
    if t < 255 { t as u8 } else { (509 - t) as u8 }
}

pub fn gradient(width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let at = ((y * width + x) * 4) as usize;
            img.data[at..at + 4].copy_from_slice(&[
                tri(x + 2 * y),
                tri(2 * x + y + 90),
                tri(x + y + 200),
                255,
            ]);
        }
    }
    img
}

pub fn photo_like(width: u32, height: u32) -> Rgba {
    let mut img = gradient(width, height);
    let mut s = 99u32;
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let grain = ((s >> 24) as i16 - 128) / 10;
        let disc = {
            let dx = x as i32 - width as i32 / 3;
            let dy = y as i32 - height as i32 / 2;
            dx * dx + dy * dy < (height as i32 / 4).pow(2)
        };
        let stripes = (x / 40 + y / 40) % 7 == 0;
        for (c, p) in px.iter_mut().take(3).enumerate() {
            let mut v = i16::from(*p) + grain;
            if disc {
                v = 230 - v / 3;
            }
            if stripes {
                v = (v + 90 * c as i16) % 256;
            }
            *p = v.clamp(0, 255) as u8;
        }
    }
    img
}
