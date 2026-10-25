#![allow(dead_code)]

#[allow(clippy::duplicate_mod)]
#[path = "window.rs"]
pub mod window;

pub const AREA: &str = "aac";
pub const RATE: u32 = 48_000;
pub const SECONDS: f32 = 0.5;

pub const STREAMS: [(u16, &str, u32); 2] = [(2, "stereo", 128), (6, "5.1", 320)];

pub fn stream(channels: u16) -> String {
    format!("ff{channels}.aac")
}

pub fn decoded(channels: u16) -> String {
    format!("ff{channels}.f32")
}

pub fn signal(rate: u32, channels: u16, seconds: f32) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    let mut out = Vec::with_capacity(n * channels as usize);
    let mut seed = 0x1234_5678u32;
    for i in 0..n {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let f = 220.0 * (c as f32 + 1.0);
            let s = 0.3 * (std::f32::consts::TAU * f * t).sin()
                + 0.15 * (std::f32::consts::TAU * 1000.0 * t).sin()
                + 0.05 * (std::f32::consts::TAU * 5000.0 * t).sin()
                + 0.02 * noise;
            out.push(s);
        }
    }
    out
}

pub fn signal_bytes(channels: u16) -> Vec<u8> {
    signal(RATE, channels, SECONDS)
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect()
}
