#![allow(dead_code)]

#[allow(clippy::duplicate_mod)]
#[path = "../aac/window.rs"]
pub mod window;

pub const AREA: &str = "ac3";
pub const SAMPLES: usize = 2 * 1536 - 256;
pub const PATCH_TEST: &str = "ac3::tests::write_plain_streams";
pub const BOXED: [&str; 3] = ["ac3-51", "eac3-51", "ac3-stereo"];
pub const BOX_EXTS: [&str; 2] = ["mkv", "mp4"];

pub struct Case {
    pub name: &'static str,
    pub codec: &'static str,
    pub rate: u32,
    pub layout: &'static str,
    pub channels: u16,
    pub kbps: u32,
    pub extra: &'static [&'static str],
}

const fn case(
    name: &'static str,
    codec: &'static str,
    rate: u32,
    layout: &'static str,
    channels: u16,
    kbps: u32,
    extra: &'static [&'static str],
) -> Case {
    Case {
        name,
        codec,
        rate,
        layout,
        channels,
        kbps,
        extra,
    }
}

pub const MAIN: [Case; 5] = [
    case("ac3-51", "ac3", 48_000, "5.1(side)", 6, 448, &[]),
    case("ac3-stereo", "ac3", 44_100, "stereo", 2, 96, &[]),
    case("ac3-mono", "ac3", 48_000, "mono", 1, 64, &[]),
    case("eac3-51", "eac3", 48_000, "5.1(side)", 6, 384, &[]),
    case("eac3-stereo", "eac3", 48_000, "stereo", 2, 128, &[]),
];

pub const EDGES: [Case; 16] = [
    case("edge-ac3-mono-32k", "ac3", 32_000, "mono", 1, 48, &[]),
    case(
        "edge-ac3-stereo-32k",
        "ac3",
        32_000,
        "stereo",
        2,
        64,
        &["-stereo_rematrixing", "0"],
    ),
    case("edge-ac3-21", "ac3", 48_000, "2.1", 3, 192, &[]),
    case("edge-ac3-30", "ac3", 44_100, "3.0", 3, 192, &[]),
    case("edge-ac3-30back", "ac3", 48_000, "3.0(back)", 3, 192, &[]),
    case("edge-ac3-40", "ac3", 32_000, "4.0", 4, 256, &[]),
    case("edge-ac3-quad", "ac3", 48_000, "quad(side)", 4, 256, &[]),
    case("edge-ac3-41", "ac3", 44_100, "4.1", 5, 320, &[]),
    case("edge-ac3-50", "ac3", 48_000, "5.0(side)", 5, 320, &[]),
    case(
        "edge-ac3-51-cpl",
        "ac3",
        48_000,
        "5.1(side)",
        6,
        192,
        &["-channel_coupling", "1", "-cpl_start_band", "3"],
    ),
    case("edge-eac3-mono-44k", "eac3", 44_100, "mono", 1, 64, &[]),
    case("edge-eac3-stereo-32k", "eac3", 32_000, "stereo", 2, 96, &[]),
    case("edge-eac3-30", "eac3", 48_000, "3.0", 3, 192, &[]),
    case("edge-eac3-quad", "eac3", 48_000, "quad(side)", 4, 192, &[]),
    case("edge-eac3-41", "eac3", 44_100, "4.1", 5, 256, &[]),
    case("edge-eac3-51-low", "eac3", 48_000, "5.1(side)", 6, 96, &[]),
];

pub fn cases() -> impl Iterator<Item = &'static Case> {
    MAIN.iter().chain(EDGES.iter())
}

pub fn named(name: &str) -> &'static Case {
    cases()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no AC-3 case {name}"))
}

impl Case {
    pub fn stream(&self) -> String {
        format!("{}.{}", self.name, self.codec)
    }

    pub fn plain(&self) -> String {
        format!("{}.plain", self.name)
    }

    pub fn decoded(&self) -> String {
        format!("{}.f32", self.name)
    }

    pub fn target(&self) -> &'static str {
        match self.channels {
            1 => "mono",
            2 => "stereo",
            _ => "5.1(side)",
        }
    }
}

pub fn boxed(name: &str, ext: &str) -> String {
    format!("box-{name}.{ext}")
}

pub fn signal(rate: u32, channels: u16, n: usize) -> Vec<u8> {
    let mut seed = 0x1234_5678u32;
    let mut out = Vec::with_capacity(n * channels as usize * 4);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        for c in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let f = 180.0 * (c as f32 + 1.0);
            let burst = if (t * 3.0).fract() < 0.1 { 0.3 } else { 0.0 };
            let s = 0.3 * (std::f32::consts::TAU * f * t).sin()
                + 0.1 * (std::f32::consts::TAU * 4100.0 * t).sin()
                + (0.05 + burst) * noise;
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    out
}

pub fn work_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/record_refs/ac3")
}
