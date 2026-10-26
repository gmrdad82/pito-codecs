use pito_codecs::media::Frame;

pub const AREA: &str = "h264";

pub struct X264 {
    pub name: &'static str,
    pub size: (u32, u32),
    pub frames: u64,
    pub amp: u8,
    pub opts: &'static [&'static str],
}

pub const X264_STREAMS: [X264; 4] = [
    X264 {
        name: "baseline",
        size: (176, 144),
        frames: 10,
        amp: 4,
        opts: &["--profile", "baseline", "--keyint", "5", "--slices", "2"],
    },
    X264 {
        name: "main_cabac_b",
        size: (176, 144),
        frames: 10,
        amp: 8,
        opts: &["--profile", "main", "--bframes", "3", "--ref", "3"],
    },
    X264 {
        name: "high_bpyramid",
        size: (256, 160),
        frames: 10,
        amp: 6,
        opts: &[
            "--profile",
            "high",
            "--bframes",
            "3",
            "--keyint",
            "10",
            "--b-pyramid",
            "normal",
            "--weightp",
            "2",
            "--direct",
            "temporal",
            "--ref",
            "4",
        ],
    },
    X264 {
        name: "high_crop_slices",
        size: (238, 158),
        frames: 8,
        amp: 4,
        opts: &[
            "--profile",
            "high",
            "--bframes",
            "2",
            "--slices",
            "4",
            "--cqm",
            "jvt",
        ],
    },
];

pub const FUZZ: [&str; 2] = ["baseline", "high_bpyramid"];

pub fn x264(name: &str) -> Result<&'static X264, String> {
    X264_STREAMS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("no x264 stream named {name}"))
}

pub fn stream(name: &str) -> String {
    format!("{name}.h264")
}

pub fn recon(name: &str) -> String {
    format!("{name}.yuv")
}

pub fn source(w: u32, h: u32, frames: u64, amp: u8) -> Vec<Frame> {
    (0..frames)
        .map(|n| crate::testref::yuv_frame(w, h, n, amp))
        .collect()
}

pub fn raw(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        for p in &f.planes {
            for r in 0..p.height {
                out.extend_from_slice(&p.data[r * p.stride..r * p.stride + p.width]);
            }
        }
    }
    out
}

pub fn x264_source(name: &str) -> Result<Vec<u8>, String> {
    let s = x264(name)?;
    Ok(raw(&source(s.size.0, s.size.1, s.frames, s.amp)))
}
