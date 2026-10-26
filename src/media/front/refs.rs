pub const AREA: &str = "front";
pub const SMALL: (u32, u32) = (352, 240);
pub const BIG: (u32, u32) = (1920, 1080);
pub const FPS: u32 = 10;
pub const GOP: u32 = 5;
pub const FRAMES: u64 = 65;
pub const PIECES: usize = 6;
pub const PIECE_FROM: f64 = 0.2;
pub const PIECE_TO: f64 = 1.15;
pub const SEARCH: usize = 4;
pub const EDGE_PSNR: f64 = 30.0;

pub const BT709_TV: [&str; 8] = [
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-color_range",
    "tv",
];

pub const FULL_709: [&str; 8] = [
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-color_range",
    "pc",
];

pub const X264: [&str; 14] = [
    "-c:v",
    "libx264",
    "-preset",
    "ultrafast",
    "-crf",
    "51",
    "-g",
    "5",
    "-keyint_min",
    "5",
    "-sc_threshold",
    "0",
    "-pix_fmt",
    "yuv420p",
];

pub const X265: [&str; 12] = [
    "-c:v",
    "libx265",
    "-preset",
    "ultrafast",
    "-crf",
    "51",
    "-pix_fmt",
    "yuv420p",
    "-tag:v",
    "hvc1",
    "-x265-params",
    "keyint=5:min-keyint=5:scenecut=0:open-gop=0:bframes=0:log-level=error",
];

pub struct Fixture {
    pub name: &'static str,
    pub hevc: bool,
    pub sar: &'static str,
    pub color: &'static [&'static str],
}

pub const HEVC_TAGGED: &str = "hevc-tagged.mp4";
pub const HEVC_UNTAGGED: &str = "hevc-untagged.mp4";
pub const HEVC_ANAMORPHIC_FULL: &str = "hevc-anamorphic-full.mp4";
pub const H264_UNTAGGED: &str = "h264-untagged.mp4";
pub const H264_ANAMORPHIC_FULL: &str = "h264-anamorphic-full.mp4";

pub const FIXTURES: [Fixture; 5] = [
    Fixture {
        name: HEVC_TAGGED,
        hevc: true,
        sar: "4/3",
        color: &BT709_TV,
    },
    Fixture {
        name: HEVC_UNTAGGED,
        hevc: true,
        sar: "0",
        color: &[],
    },
    Fixture {
        name: HEVC_ANAMORPHIC_FULL,
        hevc: true,
        sar: "8/9",
        color: &FULL_709,
    },
    Fixture {
        name: H264_UNTAGGED,
        hevc: false,
        sar: "0",
        color: &[],
    },
    Fixture {
        name: H264_ANAMORPHIC_FULL,
        hevc: false,
        sar: "8/9",
        color: &FULL_709,
    },
];

pub fn source() -> String {
    format!(
        "smptebars=size={}x{}:rate={FPS}:duration={}",
        SMALL.0,
        SMALL.1,
        FRAMES as f64 / f64::from(FPS)
    )
}

pub fn args(f: &Fixture) -> Vec<String> {
    let mut a: Vec<String> = ["-f", "lavfi", "-i"].map(String::from).to_vec();
    a.push(source());
    a.push("-vf".into());
    a.push(format!("setsar={}", f.sar));
    let codec: &[&str] = if f.hevc { &X265 } else { &X264 };
    a.extend(codec.iter().chain(f.color).map(|s| s.to_string()));
    a.push("{out}".into());
    a
}

pub fn piece_span(k: usize) -> (f64, f64) {
    (k as f64 + PIECE_FROM, k as f64 + PIECE_TO)
}

pub fn frames_per_cut() -> std::ops::RangeInclusive<u64> {
    let per = (PIECE_TO - PIECE_FROM) * f64::from(FPS);
    let n = PIECES as u64;
    (per.floor() as u64 * n)..=(per.ceil() as u64 * n)
}
