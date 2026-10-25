use crate::testref::{self, Sound};
use pito_codecs::media::{Frame, PixelFormat, TimeBase};

pub const AREA: &str = "filter";
pub const FRAMES: u64 = 2;
pub const NOISE: [u8; 2] = [0, 12];
pub const SCALE_SIZE: (u32, u32) = (64, 48);
pub const COLOUR_SIZE: (u32, u32) = (48, 32);

pub const SCALES: [(&str, u32, u32, bool); 6] = [
    ("bilinear", 48, 36, false),
    ("bicubic", 48, 36, false),
    ("lanczos", 48, 36, false),
    ("lanczos", 22, 16, false),
    ("bicubic", 96, 72, false),
    ("lanczos", 48, 36, true),
];

pub const COLOURS: [(&str, &str); 3] = [("bt709", "tv"), ("bt601", "pc"), ("bt2020", "tv")];

pub const SITINGS: [&str; 2] = ["left", "center"];

pub fn pix(deep: bool) -> &'static str {
    if deep { "yuv420p10le" } else { "yuv420p" }
}

pub fn scale_ref(kernel: &str, w: u32, h: u32, deep: bool) -> String {
    format!("zscale-{kernel}-{w}x{h}-{}.yuv", pix(deep))
}

pub fn rgba_ref(matrix: &str, range: &str) -> String {
    format!("rgba-{matrix}-{range}.rgba")
}

pub fn yuv_ref(matrix: &str, range: &str) -> String {
    format!("yuv-{matrix}-{range}.yuv")
}

pub fn zscale_rgba_ref(siting: &str) -> String {
    format!("zscale-rgba-{siting}.rgba")
}

pub const CONVERT_10BIT: &str = "bt601-tv-to-bt709-pc-10bit.yuv";
pub const BGRA: &str = "bgra-bt709-tv.bgra";
pub const BGR0_TO_YUV: &str = "bgr0-to-yuv-bt709-tv-area.yuv";
pub const RGBA_TO_YUV_LEFT: &str = "rgba-to-yuv-bt709-tv-left.yuv";
pub const SCALE_CENTRE: &str = "zscale-lanczos-48x36-center.yuv";

pub fn yuv8(size: (u32, u32), n: u64) -> Frame {
    testref::yuv_frame(size.0, size.1, n, NOISE[n as usize % 2])
}

pub fn yuv10(size: (u32, u32), n: u64) -> Frame {
    let src = yuv8(size, n);
    let mut f = Frame::new(PixelFormat::Yuv420p10, size.0, size.1, 0, TimeBase::new(1, 30));
    for (p, q) in src.planes.iter().zip(f.planes.iter_mut()) {
        for y in 0..p.height {
            for x in 0..p.width {
                let v = (u16::from(p.data[y * p.stride + x]) << 2) | ((x ^ y) & 3) as u16;
                let at = y * q.stride + 2 * x;
                q.data[at..at + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    f
}

pub fn yuv(size: (u32, u32), n: u64, deep: bool) -> Frame {
    if deep { yuv10(size, n) } else { yuv8(size, n) }
}

pub fn rgba(size: (u32, u32), n: u64) -> Vec<u8> {
    testref::rgba_frame(size.0, size.1, n, NOISE[n as usize % 2]).data
}

pub fn bgra(size: (u32, u32), n: u64) -> Vec<u8> {
    let mut px = rgba(size, n);
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    px
}

pub fn visible(f: &Frame) -> Vec<u8> {
    let bytes = f.format.bytes_per_sample();
    let mut out = Vec::new();
    for p in &f.planes {
        for row in p.data.chunks(p.stride.max(1)).take(p.height) {
            out.extend_from_slice(&row[..(p.width * bytes).min(row.len())]);
        }
    }
    out
}

pub fn yuv_bytes(size: (u32, u32), deep: bool) -> Vec<u8> {
    (0..FRAMES)
        .flat_map(|n| visible(&yuv(size, n, deep)))
        .collect()
}

pub fn rgba_bytes(size: (u32, u32)) -> Vec<u8> {
    (0..FRAMES).flat_map(|n| rgba(size, n)).collect()
}

pub fn bgra_bytes(size: (u32, u32)) -> Vec<u8> {
    (0..FRAMES).flat_map(|n| bgra(size, n)).collect()
}

fn tone(i: u64, _: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    (0.25 * (std::f64::consts::TAU * 997.0 * t).sin()) as f32
}

fn steps(i: u64, ch: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    if ch == 1 {
        return (0.2 * (std::f64::consts::TAU * 3000.0 * t).sin()) as f32;
    }
    let amp = if t < 8.0 {
        0.05
    } else if t < 16.0 {
        0.4
    } else {
        0.1
    };
    (amp * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
}

fn pink(i: u64, _: u16, _: u32) -> f32 {
    let sum: i32 = (0..12u32).map(|k| testref::noise(k, 7, i >> k, 127)).sum();
    (0.5 * f64::from(sum) / (127.0 * 12.0)) as f32
}

pub const CHANNELS: u16 = 2;

pub fn loudness() -> [(&'static str, u32, f64, Sound); 4] {
    [
        ("tone", 48_000, 12.0, Sound::Fn(tone)),
        ("steps", 48_000, 24.0, Sound::Fn(steps)),
        ("pink", 48_000, 15.0, Sound::Fn(pink)),
        (
            "white",
            44_100,
            10.0,
            Sound::Noise {
                amp: 0.3,
                seed: 3,
            },
        ),
    ]
}

pub fn loudness_ref(label: &str) -> String {
    format!("r128-{label}.txt")
}

pub fn loudness_frames(rate: u32, seconds: f64) -> usize {
    (f64::from(rate) * seconds) as usize
}

pub fn loudness_pcm(sound: Sound, rate: u32, seconds: f64) -> Vec<f32> {
    testref::pcm(sound, rate, CHANNELS, 0, loudness_frames(rate, seconds))
}

pub fn loudness_bytes(sound: Sound, rate: u32, seconds: f64) -> Vec<u8> {
    loudness_pcm(sound, rate, seconds)
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}

pub fn meter_field(text: &str, key: &str) -> Option<f64> {
    let want = format!("lavfi.r128.{key}=");
    text.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix(want.as_str()))
        .and_then(|v| v.trim().parse().ok())
}
