pub const AREA: &str = "front";
pub const SMALL: (u32, u32) = (352, 240);
pub const HEVC_SMALL: (u32, u32) = (384, 240);
pub const BIG: (u32, u32) = (1920, 1080);
pub const FPS: u32 = 10;
pub const GOP: u32 = 5;
pub const FRAMES: u64 = 65;
pub const PIECES: usize = 6;
pub const PIECE_FROM: f64 = 0.2;
pub const PIECE_TO: f64 = 1.15;
pub const SEARCH: usize = 4;
pub const EDGE_PSNR: f64 = 30.0;
pub const SOURCE: &str = "source.i420";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tags {
    None,
    Bt709Tv,
    Bt709Full,
}

pub struct Fixture {
    pub name: &'static str,
    pub hevc: bool,
    pub sar: Option<(u32, u32)>,
    pub tags: Tags,
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
        sar: Some((4, 3)),
        tags: Tags::Bt709Tv,
    },
    Fixture {
        name: HEVC_UNTAGGED,
        hevc: true,
        sar: None,
        tags: Tags::None,
    },
    Fixture {
        name: HEVC_ANAMORPHIC_FULL,
        hevc: true,
        sar: Some((8, 9)),
        tags: Tags::Bt709Full,
    },
    Fixture {
        name: H264_UNTAGGED,
        hevc: false,
        sar: None,
        tags: Tags::None,
    },
    Fixture {
        name: H264_ANAMORPHIC_FULL,
        hevc: false,
        sar: Some((8, 9)),
        tags: Tags::Bt709Full,
    },
];

impl Fixture {
    pub fn tool(&self) -> &'static str {
        if self.hevc { "x265" } else { "x264" }
    }

    pub fn stream(&self) -> String {
        let ext = if self.hevc { "265" } else { "264" };
        format!("{}.{ext}", self.name.trim_end_matches(".mp4"))
    }

    pub fn size(&self) -> (u32, u32) {
        if self.hevc { HEVC_SMALL } else { SMALL }
    }

    pub fn out(&self) -> String {
        if self.hevc {
            self.stream()
        } else {
            self.name.into()
        }
    }
}

pub fn source_bytes() -> Vec<u8> {
    source_bytes_at(SMALL)
}

pub fn source_bytes_at((w, h): (u32, u32)) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..FRAMES {
        let f = crate::testref::yuv_frame(w, h, n, 0);
        for p in &f.planes {
            for row in p.data.chunks(p.stride.max(1)).take(p.height) {
                out.extend_from_slice(&row[..p.width.min(row.len())]);
            }
        }
    }
    out
}

pub fn args(f: &Fixture) -> Vec<String> {
    let (w, h) = f.size();
    let size = format!("{w}x{h}");
    let gop = GOP.to_string();
    let fps = FPS.to_string();
    let mut a: Vec<String> = if f.hevc {
        [
            "--input",
            "{src}",
            "--input-res",
            &size,
            "--input-csp",
            "i420",
            "--fps",
            &fps,
            "--frames",
            &FRAMES.to_string(),
            "--preset",
            "ultrafast",
            "--crf",
            "51",
            "--keyint",
            &gop,
            "--min-keyint",
            &gop,
            "--no-scenecut",
            "--no-open-gop",
            "--bframes",
            "0",
            "--frame-threads",
            "1",
            "--no-info",
            "--log-level",
            "error",
        ]
        .map(String::from)
        .to_vec()
    } else {
        [
            "--demuxer",
            "raw",
            "--input-csp",
            "i420",
            "--input-res",
            &size,
            "--fps",
            &fps,
            "--frames",
            &FRAMES.to_string(),
            "--preset",
            "ultrafast",
            "--crf",
            "51",
            "--keyint",
            &gop,
            "--min-keyint",
            &gop,
            "--scenecut",
            "0",
            "--bframes",
            "0",
            "--threads",
            "1",
            "--quiet",
        ]
        .map(String::from)
        .to_vec()
    };
    if let Some((x, y)) = f.sar {
        a.extend(["--sar".into(), format!("{x}:{y}")]);
    }
    let range = match (f.tags, f.hevc) {
        (Tags::None, _) => None,
        (Tags::Bt709Tv, true) => Some("limited"),
        (Tags::Bt709Tv, false) => Some("tv"),
        (Tags::Bt709Full, true) => Some("full"),
        (Tags::Bt709Full, false) => Some("pc"),
    };
    if let Some(range) = range {
        for opt in ["--colorprim", "--transfer", "--colormatrix"] {
            a.extend([opt.into(), "bt709".into()]);
        }
        a.extend(["--range".into(), range.into()]);
        if !f.hevc {
            a.extend(["--input-range".into(), range.into()]);
        }
    }
    if f.hevc {
        a.extend(["--output".into(), "{out}".into()]);
    } else {
        a.extend(["--muxer".into(), "mp4".into(), "-o".into(), "{out}".into()]);
        a.push("{src}".into());
    }
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
