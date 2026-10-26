#![allow(dead_code)]

pub const AREA: &str = "flac";

pub const REMUXED: [(&str, &[&str]); 4] = [
    ("s16_stereo_44k", &["mka", "m4a"]),
    ("s24_mono_96k_exhaustive", &["mkv", "mp4"]),
    ("s16_six_channels", &["mka", "mp4"]),
    ("s16_block577_independent", &["m4a"]),
];

pub fn remuxed(name: &str, ext: &str) -> String {
    format!("{name}.{ext}")
}

pub fn tracked(name: &str) -> Result<Vec<u8>, String> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/flac")
        .join(format!("{name}.flac"));
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}
