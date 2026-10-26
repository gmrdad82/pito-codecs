#![allow(dead_code)]

#[path = "../../tests/fixtures/alac/pcm.rs"]
pub mod pcm;

pub const AREA: &str = "alac";

pub struct Case {
    pub name: &'static str,
    pub ext: &'static str,
    pub depth: u32,
    pub channels: usize,
    pub rate: u32,
    pub args: &'static [&'static str],
}

pub const CASES: [Case; 4] = [
    Case {
        name: "s16-mono-m4a-level1",
        ext: "m4a",
        depth: 16,
        channels: 1,
        rate: 48_000,
        args: &["-compression_level", "1"],
    },
    Case {
        name: "s24-stereo-m4a-order1-30",
        ext: "m4a",
        depth: 24,
        channels: 2,
        rate: 96_000,
        args: &["-min_prediction_order", "1", "-max_prediction_order", "30"],
    },
    Case {
        name: "s16-stereo-mov-order12",
        ext: "mov",
        depth: 16,
        channels: 2,
        rate: 32_000,
        args: &["-min_prediction_order", "12", "-max_prediction_order", "12"],
    },
    Case {
        name: "s24-stereo-mkv",
        ext: "mkv",
        depth: 24,
        channels: 2,
        rate: 44_100,
        args: &[],
    },
];

impl Case {
    pub fn file(&self) -> String {
        format!("{}.{}", self.name, self.ext)
    }

    pub fn raw_format(&self) -> (&'static str, &'static str) {
        if self.depth == 16 {
            ("s16le", "s16p")
        } else {
            ("s24le", "s32p")
        }
    }
}

pub fn frames(k: usize) -> usize {
    4096 + 517 * k + 7
}

pub fn source(k: usize) -> Vec<i32> {
    let c = &CASES[k];
    pcm::source(c.channels, c.depth, frames(k), 100 + k as u64)
}

pub fn raw(samples: &[i32], depth: u32) -> Vec<u8> {
    let width = if depth == 16 { 2 } else { 3 };
    samples
        .iter()
        .flat_map(|&s| s.to_le_bytes().into_iter().take(width))
        .collect()
}

pub fn raw_source(k: usize) -> Vec<u8> {
    raw(&source(k), CASES[k].depth)
}
