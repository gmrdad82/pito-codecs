#![allow(dead_code)]

#[allow(clippy::duplicate_mod)]
#[path = "../aac/window.rs"]
pub mod window;

pub const AREA: &str = "mp3";
pub const SECONDS: f64 = 0.5;

pub const PLAIN: [(&str, u32, &[&str]); 4] = [
    ("joint44", 44100, &["-b:a", "192k"]),
    ("mono48vbr", 48000, &["-ac", "1", "-q:a", "4"]),
    ("stereo32", 32000, &["-b:a", "128k", "-joint_stereo", "0"]),
    ("lsf22", 22050, &["-b:a", "48k"]),
];

pub const GAPLESS: [(&str, u32, &[&str]); 2] = [
    ("gapless44", 44100, &["-b:a", "192k"]),
    ("gapless48mono", 48000, &["-ac", "1"]),
];

pub const DELAY_RATE: u32 = 44100;
pub const DELAY_EXTS: [&str; 2] = ["mp4", "mkv"];
pub const DELAY_RANGE: (f64, f64) = (0.2, 0.4);

pub fn signal(rate: u32) -> String {
    format!(
        "aevalsrc=exprs='0.25*sin(2*PI*(300+200*t)*t)+0.2*sin(2*PI*3100*t)*lt(mod(t\\,0.37)\\,0.03)+0.1*(random(0)-0.5)|0.25*sin(2*PI*523*t)+0.15*sin(2*PI*7000*t)*lt(mod(t\\,0.29)\\,0.02)+0.1*(random(1)-0.5)':s={rate}:d={SECONDS}"
    )
}

pub fn stream(name: &str) -> String {
    format!("{name}.mp3")
}

pub fn decoded(file: &str) -> String {
    format!("{file}.f32")
}

pub fn delay(ext: &str) -> String {
    format!("delay.{ext}")
}

pub fn delay_samples() -> (usize, usize) {
    let at = |s: f64| (s * DELAY_RATE as f64).round() as usize;
    (at(DELAY_RANGE.0), at(DELAY_RANGE.1))
}
