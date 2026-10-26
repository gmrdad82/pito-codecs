pub const AREA: &str = "gpu";
pub const FRAMES: u64 = 8;
pub const NOISE: u8 = 2;
pub const RATE: u32 = 25;

pub const FLOOR: (u32, u32) = (352, 240);
pub const WIDE: (u32, u32) = (426, 240);

pub struct Stream {
    pub name: &'static str,
    pub hevc: bool,
    pub ten: bool,
    pub args: &'static [&'static str],
    pub sizes: &'static [(u32, u32)],
}

pub static STREAMS: [Stream; 5] = [
    Stream {
        name: "h264-baseline",
        hevc: false,
        ten: false,
        args: &["--profile", "baseline"],
        sizes: &[FLOOR, WIDE],
    },
    Stream {
        name: "h264-main-cabac",
        hevc: false,
        ten: false,
        args: &["--profile", "main", "--bframes", "0"],
        sizes: &[FLOOR, WIDE],
    },
    Stream {
        name: "h264-high-bframes",
        hevc: false,
        ten: false,
        args: &["--profile", "high", "--bframes", "3", "--ref", "4"],
        sizes: &[FLOOR, WIDE],
    },
    Stream {
        name: "hevc-main",
        hevc: true,
        ten: false,
        args: &["--bframes", "4"],
        sizes: &[FLOOR, WIDE],
    },
    Stream {
        name: "hevc-main10",
        hevc: true,
        ten: true,
        args: &[],
        sizes: &[FLOOR, WIDE],
    },
];

impl Stream {
    pub fn tool(&self) -> &'static str {
        if self.hevc { "x265" } else { "x264" }
    }

    pub fn extension(&self) -> &'static str {
        if self.hevc { "hevc" } else { "h264" }
    }

    pub fn file(&self, size: (u32, u32)) -> String {
        format!("{}-{}x{}.{}", self.name, size.0, size.1, self.extension())
    }
}

pub fn decoded(file: &str) -> String {
    format!("{file}.yuv")
}

pub fn high(size: (u32, u32)) -> String {
    STREAMS[2].file(size)
}

pub fn source_bytes(size: (u32, u32)) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..FRAMES {
        let f = crate::testref::yuv_frame(size.0, size.1, n, NOISE);
        for p in &f.planes {
            for row in p.data.chunks(p.stride.max(1)).take(p.height) {
                out.extend_from_slice(&row[..p.width.min(row.len())]);
            }
        }
    }
    out
}
