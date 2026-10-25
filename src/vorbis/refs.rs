pub const AREA: &str = "vorbis";
pub const MAX_DIFF: f64 = 1e-6;
pub const MAX_RMS: f64 = 1e-7;
pub const EVERY: usize = 384;
pub const FINE: usize = 127;
pub const WINDOW: usize = 256;
pub const SEAM: f64 = 4.0;

#[derive(Debug, Clone, Copy)]
pub struct Case {
    pub name: &'static str,
    pub native: bool,
    pub rate: u32,
    pub channels: u16,
    pub quality: &'static str,
    pub seconds: f64,
    pub clicks: bool,
}

impl Case {
    pub fn frames(&self) -> usize {
        (self.seconds * self.rate as f64).round() as usize
    }

    pub fn ogg(&self) -> String {
        format!("{}.ogg", self.name)
    }

    pub fn probe(&self) -> String {
        format!("{}.probe.txt", self.name)
    }

    pub fn stride(&self) -> usize {
        let s = stride(self.frames(), self.channels);
        if self.clicks { s } else { s.min(FINE) }
    }
}

const fn case(
    name: &'static str,
    rate: u32,
    channels: u16,
    quality: &'static str,
    seconds: f64,
) -> Case {
    Case {
        name,
        native: false,
        rate,
        channels,
        quality,
        seconds,
        clicks: false,
    }
}

pub const CASES: [Case; 11] = [
    case("mono_22k_q0", 22050, 1, "0", 0.5),
    case("stereo_44k_q3", 44100, 2, "3", 0.5123),
    case("stereo_48k_q9", 48000, 2, "9", 0.15),
    case("stereo_32k_qm1", 32000, 2, "-1", 0.5),
    case("surround_22k_q1", 22050, 6, "1", 0.3),
    Case {
        native: true,
        ..case("native_stereo_44k", 44100, 2, "", 0.3)
    },
    case("c3_44k_q0", 44100, 3, "0", 0.2),
    case("quad_44k_q0", 44100, 4, "0", 0.2),
    case("c5_48k_q0", 48000, 5, "0", 0.2),
    case("c61_48k_q0", 48000, 7, "0", 0.2),
    case("c71_48k_q0", 48000, 8, "0", 0.2),
];

pub const LONG: Case = Case {
    clicks: true,
    ..case("long_96k", 96000, 2, "-1", 4.2123)
};

pub const HIGH: Case = Case {
    clicks: true,
    ..case("high_88k", 88200, 2, "-1", 1.5123)
};

pub const CLUSTER_MS: &str = "300";

pub fn named(name: &str) -> Case {
    CASES
        .iter()
        .chain([&LONG, &HIGH])
        .find(|c| c.name == name)
        .copied()
        .unwrap_or_else(|| panic!("no Vorbis case named {name}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Head,
    Tail,
}

#[derive(Debug, Clone, Copy)]
pub struct Made {
    pub file: &'static str,
    pub case: &'static str,
    pub copied: bool,
}

pub const LONG_WEBM: &str = "long_96k.webm";
pub const HIGH_MKV: &str = "high_88k.mkv";
pub const COPIED_MKV: &str = "stereo_44k_q3_copy.mkv";
pub const CUT_SHORT: &str = "cut_short.webm";

pub const MADE: [Made; 7] = [
    Made {
        file: LONG_WEBM,
        case: "long_96k",
        copied: false,
    },
    Made {
        file: HIGH_MKV,
        case: "high_88k",
        copied: false,
    },
    Made {
        file: COPIED_MKV,
        case: "stereo_44k_q3",
        copied: true,
    },
    Made {
        file: "mono_22k_q0.webm",
        case: "mono_22k_q0",
        copied: false,
    },
    Made {
        file: "surround_22k_q1.mkv",
        case: "surround_22k_q1",
        copied: false,
    },
    Made {
        file: "stereo_44k_q3.mp4",
        case: "stereo_44k_q3",
        copied: true,
    },
    Made {
        file: "surround_22k_q1.mp4",
        case: "surround_22k_q1",
        copied: true,
    },
];

impl Made {
    pub fn case(&self) -> Case {
        named(self.case)
    }

    pub fn mp4(&self) -> bool {
        self.file.ends_with(".mp4")
    }
}

pub fn end(file: &str) -> End {
    if file.ends_with(".ogg") || file.ends_with(".mp4") {
        End::Head
    } else {
        End::Tail
    }
}

pub fn stride(frames: usize, channels: u16) -> usize {
    (frames * usize::from(channels)).div_ceil(EVERY) | 1
}

pub fn window_frames(channels: u16) -> usize {
    (WINDOW / usize::from(channels.max(1))).max(1)
}

pub fn every(file: &str) -> String {
    format!("{file}.every.f32")
}

pub fn head(file: &str) -> String {
    format!("{file}.head.f32")
}

pub fn tail(file: &str) -> String {
    format!("{file}.tail.f32")
}

pub fn seam(file: &str) -> String {
    format!("{file}.seam.f32")
}

pub fn seam_span(c: &Case) -> (usize, usize) {
    let at = (SEAM * c.rate as f64).round() as usize;
    let half = window_frames(c.channels) / 2;
    (at - half, at + half)
}

pub fn decimate(samples: &[f32], channels: u16, stride: usize) -> Vec<f32> {
    samples
        .chunks(usize::from(channels.max(1)))
        .step_by(stride.max(1))
        .flatten()
        .copied()
        .collect()
}

#[cfg(feature = "mkv")]
pub fn cut_short(full: &std::path::Path) -> Result<Vec<u8>, String> {
    let text = |e: pito_codecs::media::Error| format!("{}: {e}", full.display());
    let file = std::fs::read(full).map_err(|e| format!("{}: {e}", full.display()))?;
    let clusters: Vec<usize> = file
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == [0x1f, 0x43, 0xb6, 0x75])
        .map(|(i, _)| i)
        .collect();
    if clusters.len() <= 4 {
        return Err(format!("{} clusters, more than 4 wanted", clusters.len()));
    }
    let cluster = clusters[clusters.len() / 2];
    let r = pito_codecs::mkv::Reader::open(full).map_err(text)?;
    let mut s = r.stream();
    let mut data = Vec::new();
    let mut blocks = Vec::new();
    let mut from = 0;
    while s.next_into(&mut data).map_err(text)?.is_some() {
        let i = from
            + file[from..]
                .windows(data.len())
                .position(|w| w == &data[..])
                .ok_or("a block's bytes are not in the file")?;
        if i > cluster {
            break;
        }
        blocks.push(i);
        from = i + data.len();
    }
    let at = *blocks
        .len()
        .checked_sub(3)
        .and_then(|k| blocks.get(k))
        .ok_or("too few blocks before the middle cluster")?;
    let header = |body: usize, id: u8| {
        (1..=8).find_map(|n| {
            let e = body.checked_sub(n + 1)?;
            (file[e] == id && file[e + 1].leading_zeros() as usize + 1 == n).then_some(e)
        })
    };
    let cut = header(at - 4, 0xa3)
        .or_else(|| header(header(at - 4, 0xa1)?, 0xa0))
        .ok_or("no header before the block")?;
    Ok(file[..cut].to_vec())
}

pub fn off_the_millisecond(frames: usize, rate: u32) -> bool {
    !(frames * 1000).is_multiple_of(rate as usize)
}

pub const OFF_THE_MILLISECOND: [&str; 3] = [LONG_WEBM, HIGH_MKV, COPIED_MKV];
