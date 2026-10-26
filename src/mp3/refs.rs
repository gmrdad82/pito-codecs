#![allow(dead_code)]

pub mod window {
    pub const RUN: usize = 32;
    pub const EVERY: usize = 50;

    pub fn body(samples: &[f32], channels: usize) -> Vec<f32> {
        samples
            .chunks(RUN * channels.max(1))
            .step_by(EVERY)
            .flatten()
            .copied()
            .collect()
    }

    pub fn tail(samples: &[f32], channels: usize) -> &[f32] {
        &samples[samples.len().saturating_sub(RUN * channels.max(1))..]
    }

    pub fn window(samples: &[f32], channels: usize) -> Vec<f32> {
        let mut out = body(samples, channels);
        out.extend_from_slice(tail(samples, channels));
        out
    }

    pub fn window_range(samples: &[f32], channels: usize, start: usize, end: usize) -> Vec<f32> {
        let end = (end * channels).min(samples.len());
        body(&samples[(start * channels).min(end)..end], channels)
    }
}

pub const AREA: &str = "mp3";
pub const SECONDS: f64 = 0.5;

pub struct Case {
    pub name: &'static str,
    pub rate: u32,
    pub channels: u16,
    pub lame: &'static [&'static str],
}

const fn case(name: &'static str, rate: u32, channels: u16, lame: &'static [&'static str]) -> Case {
    Case {
        name,
        rate,
        channels,
        lame,
    }
}

pub const PLAIN: [Case; 4] = [
    case("joint44", 44100, 2, &["-b", "192"]),
    case("mono48vbr", 48000, 1, &["-V", "4"]),
    case("stereo32", 32000, 2, &["-b", "128", "-m", "s"]),
    case("lsf22", 22050, 2, &["-b", "48"]),
];

pub const GAPLESS: [Case; 2] = [
    case("gapless44", 44100, 2, &["-b", "192"]),
    case("gapless48mono", 48000, 1, &["-b", "64"]),
];

pub const DELAY: Case = case("delay", 44100, 2, &["-b", "160"]);
pub const DELAY_EXTS: [&str; 2] = ["mp4", "mkv"];
pub const DELAY_RANGE: (f64, f64) = (0.2, 0.4);

pub fn stream(name: &str) -> String {
    format!("{name}.mp3")
}

pub fn decoded(file: &str) -> String {
    format!("{file}.f32")
}

pub fn delay(ext: &str) -> String {
    format!("delay.{ext}")
}

pub fn frames(c: &Case) -> usize {
    (SECONDS * c.rate as f64).round() as usize
}

pub fn delay_samples() -> (usize, usize) {
    let at = |s: f64| (s * DELAY.rate as f64).round() as usize;
    (at(DELAY_RANGE.0), at(DELAY_RANGE.1))
}
