pub const JOIN: &str = "join";
pub const PCOD: &str = "pcod";

pub const SMALL: &str = "testsrc2=s=160x120:r=30:d=0.2";
pub const GPU_FLOOR: &str = "testsrc2=s=352x240:r=30:d=0.2";
pub const PROBED: &str = "testsrc2=s=128x96:r=30:d=0.5";
pub const VORBIS: &str = "aevalsrc=0.3*sin(440*2*PI*t)|0.2*sin(660*2*PI*t):s=44100:d=0.7:c=stereo";

pub const X264: [&str; 6] = ["-c:v", "libx264", "-preset", "ultrafast", "-crf", "30"];
pub const X265: [&str; 8] = [
    "-c:v",
    "libx265",
    "-preset",
    "ultrafast",
    "-crf",
    "30",
    "-x265-params",
    "log-level=error",
];

pub const JOIN_CLIPS: [(&str, &str, &[&str]); 5] = [
    (
        "h264-422-10.mp4",
        SMALL,
        &["-pix_fmt", "yuv422p10le", "-profile:v", "high422"],
    ),
    (
        "h264-444.mp4",
        SMALL,
        &["-pix_fmt", "yuv444p", "-profile:v", "high444"],
    ),
    ("hevc-422.mp4", SMALL, &["-pix_fmt", "yuv422p"]),
    ("hevc.mp4", GPU_FLOOR, &["-pix_fmt", "yuv420p"]),
    ("mpeg4.mp4", SMALL, &["-c:v", "mpeg4", "-q:v", "5"]),
];

pub const PROBE_SHAPES: [(&str, &[&str]); 3] = [
    (
        "cli-high422-10.mp4",
        &["-pix_fmt", "yuv422p10le", "-profile:v", "high422"],
    ),
    (
        "cli-high422-8.mp4",
        &["-pix_fmt", "yuv422p", "-profile:v", "high422"],
    ),
    (
        "cli-interlaced.mp4",
        &[
            "-pix_fmt",
            "yuv420p",
            "-flags",
            "+ildct+ilme",
            "-x264opts",
            "tff=1",
        ],
    ),
];

pub const VORBIS_FILES: [&str; 2] = ["cli-tone.ogg", "cli-tone.webm"];

pub fn encoder(name: &str) -> &'static [&'static str] {
    if name.starts_with("hevc") {
        &X265
    } else if name.starts_with("mpeg4") {
        &[]
    } else {
        &X264
    }
}
