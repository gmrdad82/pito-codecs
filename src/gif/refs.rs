use pito_codecs::gif::{Dither, EncodeOptions, PaletteMode, encode};
use pito_codecs::media::Rgba;

pub const AREA: &str = "gif";
pub const SIZE: (u32, u32) = (80, 60);
pub const FRAMES: u64 = 6;
pub const ALPHA_SIZE: (u32, u32) = (97, 61);

pub const FFMPEG_GIFS: [(&str, &str); 3] = [
    ("ff-global", "palettegen[p];[s1][p]paletteuse"),
    (
        "ff-local",
        "palettegen=stats_mode=single[p];[s1][p]paletteuse=new=1:dither=bayer",
    ),
    (
        "ff-rect",
        "palettegen=max_colors=32[p];[s1][p]paletteuse=diff_mode=rectangle:dither=none",
    ),
];

pub const MAGICK_GIFS: [(&str, u32, u32, &str); 3] = [
    (
        "im-interlaced",
        161,
        117,
        "-seed|7|-size|161x117|plasma:|-colors|200|-interlace|GIF",
    ),
    (
        "im-dispose",
        120,
        90,
        "-size|120x90|xc:none|-delay|7|-dispose|Previous|(|-size|120x90|xc:none|-fill|red\
         |-draw|circle 40,40 40,15|)|-dispose|Background|(|-size|120x90|xc:none|-fill|blue\
         |-draw|rectangle 50,20 110,80|)|-dispose|None|(|-size|120x90|gradient:yellow-green|)\
         |-loop|3",
    ),
    ("im-87", 64, 48, "-seed|3|-size|64x48|plasma:"),
];

pub fn source() -> Vec<Rgba> {
    crate::testref::rgba_frames(SIZE.0, SIZE.1, FRAMES, 0)
}

pub fn source_bytes() -> Vec<u8> {
    source().into_iter().flat_map(|f| f.data).collect()
}

pub fn ours() -> [(&'static str, EncodeOptions); 3] {
    let with = |palette, dither, colors| EncodeOptions {
        colors,
        dither,
        palette,
        ..EncodeOptions::default()
    };
    [
        (
            "our-global-fs",
            with(PaletteMode::Global, Dither::FloydSteinberg, 256),
        ),
        (
            "our-global-none",
            with(PaletteMode::Global, Dither::None, 64),
        ),
        (
            "our-local-ordered",
            with(PaletteMode::PerFrame, Dither::Ordered, 128),
        ),
    ]
}

pub fn encode_ours(name: &str) -> Result<Vec<u8>, String> {
    let (_, opts) = ours()
        .into_iter()
        .find(|(n, _)| *n == name)
        .ok_or_else(|| format!("no encoder setting named {name}"))?;
    let src = source();
    encode(&src, &vec![10; src.len()], opts).map_err(|e| e.to_string())
}

pub fn alpha_frames() -> (Vec<Rgba>, Vec<u16>) {
    let (w, h) = ALPHA_SIZE;
    let mut frames = Vec::new();
    for k in 0..8u32 {
        let mut f = Rgba::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let hole = (k == 1 || k == 4) && (x + k * 7) % 23 < 9;
                let v = if k == 4 { 3 } else { k };
                f.data[i..i + 4].copy_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y + v * 40) % 256) as u8,
                    if hole { 0 } else { 255 },
                ]);
            }
        }
        frames.push(f);
    }
    frames[6] = frames[5].clone();
    (frames, (0..8).map(|k| 5 + k).collect())
}

pub fn alpha_options() -> EncodeOptions {
    EncodeOptions {
        dither: Dither::None,
        ..EncodeOptions::default()
    }
}

pub fn encode_alpha() -> Result<Vec<u8>, String> {
    let (frames, delays) = alpha_frames();
    encode(&frames, &delays, alpha_options()).map_err(|e| e.to_string())
}
