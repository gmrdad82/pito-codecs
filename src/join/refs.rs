pub const JOIN: &str = "join";

pub const SMALL: &str = "testsrc2=s=160x120:r=30:d=0.2";
pub const GPU_FLOOR: &str = "testsrc2=s=352x240:r=30:d=0.2";

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

pub fn encoder(name: &str) -> &'static [&'static str] {
    if name.starts_with("hevc") {
        &X265
    } else if name.starts_with("mpeg4") {
        &[]
    } else {
        &X264
    }
}
