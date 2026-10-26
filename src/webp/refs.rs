use pito_codecs::media::Rgba;
use pito_codecs::webp::{AnimEncoder, Options, encode};

pub const AREA: &str = "webp";

#[derive(Debug, Clone, Copy)]
pub enum Pic {
    Pattern(u64),
    Noisy(u64, u8),
    Bars,
    Ramp,
}

pub fn picture(pic: Pic, w: u32, h: u32) -> Rgba {
    match pic {
        Pic::Pattern(n) => crate::testref::rgba_frame(w, h, n, 0),
        Pic::Noisy(n, amp) => crate::testref::rgba_frame(w, h, n, amp),
        Pic::Bars => {
            let mut f = Rgba::new(w, h);
            for (i, px) in f.data.chunks_exact_mut(4).enumerate() {
                let [r, g, b] = crate::testref::rgb_at(w, 3, 0, i as u32 % w, 0);
                px.copy_from_slice(&[r, g, b, 255]);
            }
            f
        }
        Pic::Ramp => {
            let mut f = crate::testref::rgba_frame(w, h, 1, 0);
            for (i, px) in f.data.chunks_exact_mut(4).enumerate() {
                px[3] = (2 * (i as u32 % w)).min(255) as u8;
            }
            f
        }
    }
}

pub fn photo(w: u32, h: u32) -> Rgba {
    picture(Pic::Noisy(7, 3), w, h)
}

pub fn with_alpha(mut img: Rgba) -> Rgba {
    let w = img.width as usize;
    for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i % w, i / w);
        p[3] = if (x / 24 + y / 24) % 3 == 0 {
            0
        } else {
            ((x * 255) / w) as u8 | 0x20
        };
    }
    img
}

pub fn bytes(frames: &[Rgba]) -> Vec<u8> {
    frames.iter().flat_map(|f| f.data.iter().copied()).collect()
}

pub type Still = (&'static str, Pic, (u32, u32), &'static [&'static str]);

pub const LOSSLESS: [Still; 6] = [
    (
        "ll_pattern",
        Pic::Pattern(0),
        (131, 87),
        &["-lossless", "1"],
    ),
    (
        "ll_fast",
        Pic::Noisy(1, 6),
        (96, 64),
        &["-lossless", "1", "-compression_level", "0"],
    ),
    (
        "ll_best",
        Pic::Pattern(2),
        (120, 90),
        &[
            "-lossless",
            "1",
            "-compression_level",
            "6",
            "-quality",
            "100",
        ],
    ),
    ("ll_alpha", Pic::Ramp, (100, 60), &["-lossless", "1"]),
    ("ll_palette", Pic::Bars, (150, 50), &["-lossless", "1"]),
    (
        "ll_odd",
        Pic::Pattern(3),
        (97, 61),
        &["-lossless", "1", "-compression_level", "6"],
    ),
];

pub const ANIM: &str = "anim_ll";
pub const ANIM_SIZE: (u32, u32) = (96, 64);
pub const ANIM_FRAMES: u64 = 6;
pub const ANIM_MS: u32 = 40;

pub fn anim_frames() -> Vec<Rgba> {
    crate::testref::rgba_frames(ANIM_SIZE.0, ANIM_SIZE.1, ANIM_FRAMES, 0)
}

pub fn lossless_cases() -> Vec<(String, Rgba)> {
    let mut cases: Vec<(String, Rgba)> = [
        ("pattern", Pic::Pattern(0), 333, 211),
        ("bars", Pic::Bars, 160, 90),
        ("rgb", Pic::Pattern(3), 7, 5),
        ("one", Pic::Pattern(0), 1, 1),
    ]
    .into_iter()
    .map(|(n, pic, w, h)| (format!("{n}-{w}x{h}"), picture(pic, w, h)))
    .collect();
    cases.push(("photo".into(), photo(256, 192)));
    let mut alpha = picture(Pic::Pattern(2), 120, 80);
    for (i, p) in alpha.data.chunks_exact_mut(4).enumerate() {
        p[3] = (i * 7 % 256) as u8;
    }
    cases.push(("alpha".into(), alpha));
    cases
}

pub const PINNED_LOSSLESS: [&str; 2] = ["photo", "alpha"];
pub const PINNED_EFFORT: u8 = 4;

pub fn lossless_options(effort: u8) -> Options {
    Options {
        effort,
        exact: true,
        ..Options::lossless()
    }
}

pub fn encode_lossless(name: &str) -> Result<Vec<u8>, String> {
    let (_, img) = lossless_cases()
        .into_iter()
        .find(|(n, _)| n == name)
        .ok_or_else(|| format!("no lossless case named {name}"))?;
    encode(&img, &lossless_options(PINNED_EFFORT)).map_err(|e| e.to_string())
}

pub struct Lossy {
    pub name: &'static str,
    pub pic: Pic,
    pub size: (u32, u32),
    pub quality: f32,
    pub effort: u8,
    pub min_psnr: f64,
}

const fn lossy(
    name: &'static str,
    pic: Pic,
    size: (u32, u32),
    quality: f32,
    effort: u8,
    min_psnr: f64,
) -> Lossy {
    Lossy {
        name,
        pic,
        size,
        quality,
        effort,
        min_psnr,
    }
}

pub const LOSSY: [Lossy; 7] = [
    lossy("pattern", Pic::Pattern(0), (320, 240), 75.0, 4, 26.0),
    lossy("noisy", Pic::Noisy(1, 4), (200, 120), 90.0, 6, 30.0),
    lossy("pattern5", Pic::Pattern(5), (127, 61), 20.0, 0, 30.0),
    lossy("square", Pic::Pattern(4), (64, 64), 100.0, 4, 30.0),
    lossy("tiny", Pic::Pattern(0), (1, 1), 50.0, 4, 30.0),
    lossy("cell", Pic::Noisy(2, 40), (97, 33), 5.0, 2, 30.0),
    lossy("big", Pic::Pattern(3), (352, 240), 50.0, 4, 25.0),
];

impl Lossy {
    pub fn image(&self) -> Rgba {
        picture(self.pic, self.size.0, self.size.1)
    }

    pub fn options(&self) -> Options {
        Options {
            quality: self.quality,
            effort: self.effort,
            ..Options::lossy(self.quality)
        }
    }

    pub fn libwebp(&self) -> String {
        format!("lw-{}", self.name)
    }

    pub fn ours(&self) -> String {
        format!("our-{}", self.name)
    }
}

pub fn encode_lossy(name: &str) -> Result<Vec<u8>, String> {
    let case = LOSSY
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| format!("no lossy case named {name}"))?;
    encode(&case.image(), &case.options()).map_err(|e| e.to_string())
}

pub const ALPHA_SIZE: (u32, u32) = (320, 180);
pub const ALPHA_QUALITY: f32 = 85.0;
pub const LOSSY_ALPHA: &str = "lossy-alpha";

pub fn alpha_image() -> Rgba {
    with_alpha(picture(Pic::Pattern(0), ALPHA_SIZE.0, ALPHA_SIZE.1))
}

pub fn encode_alpha() -> Result<Vec<u8>, String> {
    encode(&alpha_image(), &Options::lossy(ALPHA_QUALITY)).map_err(|e| e.to_string())
}

pub const LOSSY_ANIM_SIZE: (u32, u32) = (160, 96);
pub const LOSSY_ANIM_FRAMES: u64 = 5;
pub const LOSSY_ANIM_QUALITY: f32 = 80.0;

pub fn lossy_anim_frames() -> Vec<Rgba> {
    let (w, h) = LOSSY_ANIM_SIZE;
    crate::testref::rgba_frames(w, h, LOSSY_ANIM_FRAMES, 0)
}

pub fn lossy_anim_still(i: u64) -> String {
    format!("lw-anim-{i}")
}

pub fn encode_lossy_anim() -> Result<Vec<u8>, String> {
    let (w, h) = LOSSY_ANIM_SIZE;
    let mut enc =
        AnimEncoder::new(w, h, Options::lossy(LOSSY_ANIM_QUALITY), 1).map_err(|e| e.to_string())?;
    for f in &lossy_anim_frames() {
        enc.add(f, 50).map_err(|e| e.to_string())?;
    }
    enc.finish().map_err(|e| e.to_string())
}

pub type Vpx = (
    &'static str,
    Pic,
    &'static str,
    &'static str,
    &'static str,
    (u32, u32),
    &'static str,
);

pub const LIBVPX: [Vpx; 6] = [
    ("vpx-a", Pic::Pattern(0), "0", "1", "0", (352, 240), "30"),
    ("vpx-b", Pic::Noisy(1, 4), "0", "8", "0", (352, 240), "50"),
    ("vpx-c", Pic::Pattern(1), "0", "4", "3", (352, 240), "50"),
    ("vpx-d", Pic::Noisy(2, 4), "1", "1", "0", (352, 240), "50"),
    ("vpx-e", Pic::Pattern(2), "1", "8", "6", (320, 240), "40"),
    ("vpx-f", Pic::Noisy(3, 4), "3", "2", "2", (98, 62), "60"),
];
