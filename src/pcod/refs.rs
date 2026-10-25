use pito_codecs::gif::{Dither, EncodeOptions, PaletteMode, encode};
use pito_codecs::media::Rgba;

pub const AREA: &str = "pcod";

pub const TONE: &str = "aevalsrc=0.01*sin(2*PI*440*t)|0.01*sin(2*PI*440*t):s=48000:d=0.5";

pub const TONES: [(&str, &[&str], &str, &str); 3] = [
    ("commands-tone.flac", &["-c:a", "flac"], "FLAC", "flac"),
    (
        "commands-tone.mp3",
        &["-c:a", "libmp3lame", "-b:a", "32k"],
        "MP3",
        "mp3",
    ),
    ("commands-alac.m4a", &["-c:a", "alac"], "MP4", "alac"),
];

pub const GIF_SIZE: (u32, u32) = (384, 240);
pub const GIF_DELAYS: [u16; 6] = [7, 1, 13, 4, 25, 3];
pub const GIF_RATES: [u64; 2] = [10, 30];
pub const GIF_TICKS: &str = "gif-ticks.gif";
pub const GIF_PROBE: (u32, u32) = (8, 1);

pub fn ticks_name(rate: u64) -> String {
    format!("gif-fps{rate}.gray")
}

pub fn numbered_at(width: u32, height: u32, n: u32, clear: bool) -> Rgba {
    let block = width / 8;
    let mut img = Rgba::new(width, height);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let bit = (i as u32 % width) / block;
        let on = n >> bit & 1 == 1;
        px.copy_from_slice(&match (on, clear) {
            (true, _) => [255, 255, 255, 255],
            (false, true) => [0, 0, 0, 0],
            (false, false) => [0, 0, 0, 255],
        });
    }
    img
}

pub fn gif_options() -> EncodeOptions {
    EncodeOptions {
        dither: Dither::None,
        palette: PaletteMode::Global,
        diff: false,
        ..EncodeOptions::default()
    }
}

pub fn ticks_gif() -> Result<Vec<u8>, String> {
    let (w, h) = GIF_SIZE;
    let frames: Vec<Rgba> = (0..GIF_DELAYS.len())
        .map(|i| numbered_at(w, h, i as u32 + 1, false))
        .collect();
    encode(&frames, &GIF_DELAYS, gif_options()).map_err(|e| e.to_string())
}

pub fn numbers_of_probes(gray: &[u8]) -> Vec<usize> {
    let per = (GIF_PROBE.0 * GIF_PROBE.1) as usize;
    gray.chunks_exact(per)
        .map(|frame| {
            let n = frame
                .iter()
                .take(8)
                .enumerate()
                .fold(
                    0usize,
                    |n, (bit, &v)| if v > 128 { n | 1 << bit } else { n },
                );
            n.saturating_sub(1)
        })
        .collect()
}

pub const WEBP_SIZE: (u32, u32) = (256, 144);
pub const WEBP_INPUTS: [&str; 3] = ["ui", "pattern", "mandelbrot"];

pub fn webp_name(input: &str) -> String {
    format!("images-{input}.webp")
}

pub fn ui_like(width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        let panel = (x / 80 + y / 60) % 3;
        let mut rgb: [u8; 3] = [[246, 246, 248], [30, 34, 44], [64, 120, 220]][panel as usize];
        let line = y % 20;
        let glyph = (x * 7 + y * 3 + (x / 6) * 13) % 11 < 4;
        if (4..14).contains(&line) && x % 80 > 8 && x % 80 < 70 && glyph {
            rgb = if panel == 0 {
                [20, 20, 24]
            } else {
                [236, 236, 240]
            };
        }
        let shade = ((x + y) / 8 % 3) as u8;
        let alpha = if y < 12 || x < 12 { 0 } else { 255 };
        px.copy_from_slice(&[rgb[0].saturating_add(shade), rgb[1], rgb[2], alpha]);
    }
    img
}

pub fn mandelbrot(width: u32, height: u32) -> Rgba {
    let mut img = Rgba::new(width, height);
    for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        let cr = -2.5 + 3.5 * f64::from(x) / f64::from(width);
        let ci = -1.0 + 2.0 * f64::from(y) / f64::from(height);
        let (mut zr, mut zi, mut n) = (0.0f64, 0.0f64, 0u32);
        while n < 64 && zr * zr + zi * zi <= 4.0 {
            let t = zr * zr - zi * zi + cr;
            zi = 2.0 * zr * zi + ci;
            zr = t;
            n += 1;
        }
        let c = if n == 64 {
            [0, 0, 0]
        } else {
            [(n * 4) as u8, (n * 9 % 256) as u8, (255 - n * 3) as u8]
        };
        px.copy_from_slice(&[c[0], c[1], c[2], 255]);
    }
    img
}

pub fn webp_input(input: &str) -> Result<Rgba, String> {
    let (w, h) = WEBP_SIZE;
    match input {
        "ui" => Ok(ui_like(w, h)),
        "pattern" => Ok(crate::testref::rgba_frame(w, h, 0, 0)),
        "mandelbrot" => Ok(mandelbrot(w, h)),
        _ => Err(format!("no WebP input named {input}")),
    }
}

pub fn webp_png(input: &str) -> Result<Vec<u8>, String> {
    pito_codecs::png::encode_rgba(&webp_input(input)?).map_err(|e| e.to_string())
}
