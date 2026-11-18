use std::error::Error;
use std::io::Cursor;
use std::time::Instant;

use pito_codecs::png::{self as ours, Color, Compression, Options, Picture, Transform};

type Res<T> = Result<T, Box<dyn Error>>;

pub const OUTSIDE: &str = "png 0.18.1";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[derive(Clone, Copy)]
pub struct Format {
    pub key: &'static str,
    pub width: u32,
    pub height: u32,
    pub color: Color,
    pub depth: u8,
}

pub const FORMATS: [Format; 3] = [
    Format {
        key: "rgba16-4k",
        width: 3840,
        height: 2160,
        color: Color::Rgba,
        depth: 16,
    },
    Format {
        key: "rgba8-4k",
        width: 3840,
        height: 2160,
        color: Color::Rgba,
        depth: 8,
    },
    Format {
        key: "rgb8-1080",
        width: 1920,
        height: 1080,
        color: Color::Rgb,
        depth: 8,
    },
];

pub const CONTENTS: [&str; 2] = ["flat", "noisy"];

const PALETTE: [[u16; 3]; 6] = [
    [0x1e1e, 0x2224, 0x3030],
    [0xf2f2, 0xe8e9, 0xd0d1],
    [0xc8c9, 0x3a3b, 0x3c3d],
    [0x2a2b, 0x7e7f, 0xb4b5],
    [0xe0e1, 0xb0b1, 0x2022],
    [0x4a4b, 0xa0a1, 0x5c5d],
];

fn flat_pixel(x: u32, y: u32, w: u32, h: u32, rng: &mut Rng) -> [u16; 4] {
    let (px, py) = (x * 3 / w, y * 2 / h);
    let (cx, cy) = (x - px * w / 3, y - py * h / 2);
    let (pw, ph) = (w / 3, h / 2);
    let panel = (px + py * 3) as usize;
    if cx < 6 || cy < 6 || cx + 6 > pw || cy + 6 > ph {
        let c = PALETTE[0];
        return [c[0], c[1], c[2], 0xffff];
    }
    let base = PALETTE[(panel + 1) % PALETTE.len()];
    let (dx, dy) = (
        i64::from(cx) - i64::from(pw / 2),
        i64::from(cy) - i64::from(ph / 2),
    );
    let r = i64::from(ph / 3);
    let d2 = dx * dx + dy * dy;
    let (r_in, r_out) = ((r - 2) * (r - 2), (r + 2) * (r + 2));
    let disc = PALETTE[(panel + 3) % PALETTE.len()];
    if d2 < r_in {
        return [disc[0], disc[1], disc[2], 0xffff];
    }
    if d2 < r_out {
        let t = ((d2 - r_in) * 256 / (r_out - r_in)) as u32;
        let mix = |a: u16, b: u16| ((u32::from(a) * (256 - t) + u32::from(b) * t) >> 8) as u16;
        return [
            mix(disc[0], base[0]),
            mix(disc[1], base[1]),
            mix(disc[2], base[2]),
            0xffff,
        ];
    }
    if panel == 5 {
        let g = (cx * 65535 / pw) as u16;
        let n = (rng.next() & 0x3ff) as u16;
        return [
            g.saturating_add(n),
            g / 2 + n,
            (65535 - g).saturating_add(n),
            0xffff,
        ];
    }
    if panel == 0 && cy > ph - ph / 4 {
        let cell = (cx / 7, cy / 11);
        let bits = ((u64::from(cell.0) * 0x9E37_79B9) ^ (u64::from(cell.1) * 0x85EB_CA6B)) >> 3;
        let on = bits & 3 == 0 && cx % 7 < 5 && cy % 11 < 8;
        let c = if on { PALETTE[1] } else { base };
        return [c[0], c[1], c[2], 0xffff];
    }
    [base[0], base[1], base[2], 0xffff]
}

fn noisy_pixel(x: u32, y: u32, w: u32, h: u32, depth: u8, rng: &mut Rng) -> [u16; 4] {
    let amp = 1u64 << (depth - 3);
    let n = |rng: &mut Rng| (rng.next() >> 20) % amp;
    let (gx, gy) = (x * 65535 / w, y * 65535 / h);
    let max = (1u64 << depth) - 1;
    let sample = |v: u32, noise: u64| -> u16 {
        let base = if depth == 16 { v } else { v >> 8 };
        (u64::from(base) + noise).min(max) as u16
    };
    [
        sample(gx, n(rng)),
        sample(gy, n(rng)),
        sample((gx + gy) / 2, n(rng)),
        ((1u32 << depth) - 1) as u16,
    ]
}

#[allow(dead_code)]
pub fn from_file(path: &std::path::Path) -> Res<Picture> {
    let file = std::fs::read(path)?;
    let mut d = png18::Decoder::new(Cursor::new(&file[..]));
    d.set_transformations(png18::Transformations::EXPAND);
    let mut r = d.read_info()?;
    let mut buf = vec![0; r.output_buffer_size().ok_or("too large")?];
    let f = r.next_frame(&mut buf)?;
    buf.truncate(f.buffer_size());
    let color = match f.color_type {
        png18::ColorType::Grayscale => Color::Grey,
        png18::ColorType::GrayscaleAlpha => Color::GreyAlpha,
        png18::ColorType::Rgb => Color::Rgb,
        png18::ColorType::Rgba => Color::Rgba,
        png18::ColorType::Indexed => return Err("palette".into()),
    };
    let depth = if f.bit_depth == png18::BitDepth::Sixteen {
        16
    } else {
        8
    };
    Ok(Picture {
        width: f.width,
        height: f.height,
        color,
        depth,
        data: buf,
    })
}

pub fn picture(f: Format, content: &str) -> Picture {
    let mut pic = Picture::new(f.width, f.height, f.color, f.depth);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ u64::from(f.width) ^ u64::from(f.depth));
    let n = f.color.samples_per_pixel();
    let mut at = 0;
    for y in 0..f.height {
        for x in 0..f.width {
            let px = if content == "flat" {
                flat_pixel(x, y, f.width, f.height, &mut rng)
            } else {
                noisy_pixel(x, y, f.width, f.height, f.depth, &mut rng)
            };
            for &v in &px[..n] {
                if f.depth == 16 {
                    pic.data[at..at + 2].copy_from_slice(&v.to_be_bytes());
                    at += 2;
                } else {
                    pic.data[at] = if content == "flat" {
                        (v >> 8) as u8
                    } else {
                        v as u8
                    };
                    at += 1;
                }
            }
        }
    }
    pic
}

trait Samples {
    fn samples_per_pixel(self) -> usize;
}

impl Samples for Color {
    fn samples_per_pixel(self) -> usize {
        match self {
            Color::Grey | Color::Palette => 1,
            Color::GreyAlpha => 2,
            Color::Rgb => 3,
            Color::Rgba => 4,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Fast,
    Balanced,
}

impl Level {
    pub fn key(self) -> &'static str {
        match self {
            Level::Fast => "fast",
            Level::Balanced => "balanced",
        }
    }
}

pub fn encode_ours(p: &Picture, level: Level) -> Res<Vec<u8>> {
    let options = Options {
        compression: match level {
            Level::Fast => Compression::Fast,
            Level::Balanced => Compression::Balanced,
        },
        ..Options::default()
    };
    Ok(ours::encode_picture(p, &options)?)
}

pub fn encode_outside(p: &Picture, level: Level) -> Res<Vec<u8>> {
    let mut out = Vec::with_capacity(p.data.len() / 3);
    let mut e = png18::Encoder::new(&mut out, p.width, p.height);
    e.set_color(match p.color {
        Color::Rgb => png18::ColorType::Rgb,
        Color::Rgba => png18::ColorType::Rgba,
        Color::Grey => png18::ColorType::Grayscale,
        Color::GreyAlpha => png18::ColorType::GrayscaleAlpha,
        Color::Palette => png18::ColorType::Indexed,
    });
    e.set_depth(if p.depth == 16 {
        png18::BitDepth::Sixteen
    } else {
        png18::BitDepth::Eight
    });
    if level == Level::Fast {
        e.set_compression(png18::Compression::Fast);
    }
    let mut w = e.write_header()?;
    w.write_image_data(&p.data)?;
    w.finish()?;
    Ok(out)
}

pub struct Decoders {
    ours: ours::Decoder,
    buf: Vec<u8>,
    outside: Vec<u8>,
}

impl Decoders {
    pub fn new(p: &Picture) -> Self {
        Decoders {
            ours: ours::Decoder::new(),
            buf: vec![0; p.data.len()],
            outside: vec![0; p.data.len() + 4096],
        }
    }

    pub fn decode_ours(&mut self, file: &[u8]) -> Res<usize> {
        Ok(self
            .ours
            .decode_slice(file, Transform::Identity, &mut self.buf)?
            .bytes)
    }

    pub fn decode_outside(&mut self, file: &[u8]) -> Res<usize> {
        let mut r = png18::Decoder::new(Cursor::new(file)).read_info()?;
        let info = r.next_frame(&mut self.outside)?;
        Ok(info.buffer_size())
    }

    pub fn same_samples(&mut self, file: &[u8]) -> Res<bool> {
        let n = self.decode_ours(file)?;
        let m = self.decode_outside(file)?;
        Ok(n == m && self.buf[..n] == self.outside[..m])
    }
}

pub struct Timed {
    pub ours: Vec<f64>,
    pub outside: Vec<f64>,
    pub ours_bytes: usize,
    pub outside_bytes: usize,
    pub load_start: f64,
    pub load_end: f64,
}

pub fn load_1m() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

fn seconds(f: impl FnOnce() -> Res<usize>) -> Res<(f64, usize)> {
    let t0 = Instant::now();
    let n = f()?;
    Ok((t0.elapsed().as_secs_f64(), n))
}

pub fn time_encode(p: &Picture, level: Level, runs: usize) -> Res<Timed> {
    let mps = f64::from(p.width) * f64::from(p.height) / 1e6;
    let mut t = Timed {
        ours: Vec::new(),
        outside: Vec::new(),
        ours_bytes: 0,
        outside_bytes: 0,
        load_start: load_1m(),
        load_end: 0.0,
    };
    encode_ours(p, level)?;
    encode_outside(p, level)?;
    for _ in 0..runs {
        let (s, n) = seconds(|| Ok(std::hint::black_box(encode_ours(p, level)?).len()))?;
        t.ours.push(mps / s);
        t.ours_bytes = n;
        let (s, n) = seconds(|| Ok(std::hint::black_box(encode_outside(p, level)?).len()))?;
        t.outside.push(mps / s);
        t.outside_bytes = n;
    }
    t.load_end = load_1m();
    Ok(t)
}

pub fn time_decode(p: &Picture, file: &[u8], runs: usize) -> Res<Timed> {
    let mps = f64::from(p.width) * f64::from(p.height) / 1e6;
    let mut d = Decoders::new(p);
    let mut t = Timed {
        ours: Vec::new(),
        outside: Vec::new(),
        ours_bytes: file.len(),
        outside_bytes: file.len(),
        load_start: load_1m(),
        load_end: 0.0,
    };
    d.decode_ours(file)?;
    d.decode_outside(file)?;
    for _ in 0..runs {
        let (s, _) = seconds(|| d.decode_ours(std::hint::black_box(file)))?;
        t.ours.push(mps / s);
        let (s, _) = seconds(|| d.decode_outside(std::hint::black_box(file)))?;
        t.outside.push(mps / s);
    }
    t.load_end = load_1m();
    Ok(t)
}
