use pito_codecs::h264::{Color, Encoder, EncoderConfig, Profile};
use pito_codecs::media::{Frame, Packet, PixelFormat, Rate, TimeBase};

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
        opts: &["-profile:v", "baseline", "-g", "5", "-slices", "2"],
    },
    X264 {
        name: "main_cabac_b",
        size: (176, 144),
        frames: 10,
        amp: 8,
        opts: &["-profile:v", "main", "-bf", "3", "-refs", "3"],
    },
    X264 {
        name: "high_bpyramid",
        size: (256, 160),
        frames: 10,
        amp: 6,
        opts: &[
            "-profile:v",
            "high",
            "-bf",
            "3",
            "-g",
            "10",
            "-x264-params",
            "b-pyramid=normal:weightp=2:direct=temporal:ref=4",
        ],
    },
    X264 {
        name: "high_crop_slices",
        size: (238, 158),
        frames: 8,
        amp: 4,
        opts: &[
            "-profile:v",
            "high",
            "-bf",
            "2",
            "-slices",
            "4",
            "-x264-params",
            "cqm=jvt",
        ],
    },
];

pub const FUZZ: [&str; 2] = ["baseline", "high_bpyramid"];

pub const CRF18: (u32, u32, u64) = (352, 240, 30);

pub const COLOURS: [(Color, [&str; 4]); 4] = [
    (Color::BT709, ["tv", "bt709", "bt709", "bt709"]),
    (
        Color {
            primaries: 6,
            transfer: 6,
            matrix: 6,
            full_range: false,
        },
        ["tv", "smpte170m", "smpte170m", "smpte170m"],
    ),
    (
        Color {
            primaries: 1,
            transfer: 1,
            matrix: 1,
            full_range: true,
        },
        ["pc", "bt709", "bt709", "bt709"],
    ),
    (
        Color {
            primaries: 5,
            transfer: 6,
            matrix: 5,
            full_range: true,
        },
        ["pc", "bt470bg", "smpte170m", "bt470bg"],
    ),
];

pub const COLOUR_KEYS: [&str; 4] = [
    "color_range",
    "color_primaries",
    "color_transfer",
    "color_space",
];

pub fn x264(name: &str) -> Result<&'static X264, String> {
    X264_STREAMS
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| format!("no x264 stream named {name}"))
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

pub fn encode(config: EncoderConfig, frames: &[Frame]) -> Result<(Vec<u8>, Vec<Packet>), String> {
    let mut enc = Encoder::new(config).map_err(|e| e.to_string())?;
    let mut packets = Vec::new();
    for f in frames {
        packets.extend(enc.encode(f).map_err(|e| e.to_string())?);
    }
    packets.extend(enc.flush().map_err(|e| e.to_string())?);
    Ok((enc.avcc(), packets))
}

pub fn crf18_config(threads: usize) -> EncoderConfig {
    EncoderConfig {
        width: CRF18.0,
        height: CRF18.1,
        fps: (30, 1),
        rate: Rate::Crf(18),
        gop: 15,
        b_frames: 2,
        threads,
        ..EncoderConfig::default()
    }
}

pub fn crf18_source() -> Vec<Frame> {
    source(CRF18.0, CRF18.1, CRF18.2, 0)
}

pub fn crf18() -> Result<Vec<u8>, String> {
    let (avcc, packets) = encode(crf18_config(0), &crf18_source())?;
    to_annexb(&avcc, &packets)
}

pub fn colour_frames() -> Vec<Frame> {
    (0..6u8)
        .map(|i| {
            let mut f = Frame::new(
                PixelFormat::Yuv420p8,
                64,
                48,
                i64::from(i),
                TimeBase::new(1, 30),
            );
            f.planes[0].data.fill(16 + i * 30);
            f
        })
        .collect()
}

pub fn colour_config(n: usize) -> EncoderConfig {
    EncoderConfig {
        width: 64,
        height: 48,
        gop: 4,
        color: COLOURS[n].0,
        ..EncoderConfig::default()
    }
}

pub fn colour(n: usize) -> Result<Vec<u8>, String> {
    let (avcc, packets) = encode(colour_config(n), &colour_frames())?;
    to_annexb(&avcc, &packets)
}

pub fn to_annexb(avcc: &[u8], packets: &[Packet]) -> Result<Vec<u8>, String> {
    let short = || "avcC or a packet ends early".to_string();
    let mut out = Vec::new();
    let mut pos = 6;
    for k in 0..2 {
        let count = if k == 0 {
            *avcc.get(5).ok_or_else(short)? & 0x1f
        } else {
            *avcc.get(pos - 1).ok_or_else(short)?
        };
        for _ in 0..count {
            let len = avcc
                .get(pos..pos + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                .ok_or_else(short)?;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(avcc.get(pos + 2..pos + 2 + len).ok_or_else(short)?);
            pos += 2 + len;
        }
        pos += 1;
    }
    for p in packets {
        let mut i = 0;
        while i + 4 <= p.data.len() {
            let b = &p.data[i..i + 4];
            let len = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(p.data.get(i + 4..i + 4 + len).ok_or_else(short)?);
            i += 4 + len;
        }
    }
    Ok(out)
}

pub const CUT: i64 = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scene {
    Flat,
    Gradient,
    Noise,
    Motion,
    Cut,
}

fn hash(x: usize, y: usize, i: i64, k: usize) -> u32 {
    let mut s = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (i as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ k as u64;
    s ^= s >> 29;
    s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    s ^= s >> 32;
    s as u32
}

fn sample(scene: Scene, x: usize, y: usize, pw: usize, ph: usize, i: i64, k: usize) -> u8 {
    let noise = |amp: u32| hash(x, y, i, k) % (2 * amp + 1);
    let v = match scene {
        Scene::Flat => [96, 120, 140][k],
        Scene::Gradient => (x * 200 / pw.max(1) + y * 50 / ph.max(1) + 2 * i as usize) as u32,
        Scene::Noise => 108 + noise(20),
        Scene::Motion => {
            let (mx, my) = (x as i64 + 3 * i, y as i64 + 2 * i);
            let checker = ((mx / 8 + my / 8) % 2) as u32;
            50 + checker * 90 + ((x + y) % 7) as u32 + noise(6)
        }
        Scene::Cut => {
            if i < CUT {
                return sample(Scene::Gradient, x, y, pw, ph, i, k);
            }
            255 - (x * 180 / pw.max(1)) as u32
        }
    };
    v.min(255) as u8
}

pub fn clip(scene: Scene, w: u32, h: u32, n: i64) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i, TimeBase::new(1, 30));
            for (k, p) in f.planes.iter_mut().enumerate() {
                for y in 0..p.height {
                    for x in 0..p.width {
                        p.data[y * p.stride + x] = sample(scene, x, y, p.width, p.height, i, k);
                    }
                }
            }
            f
        })
        .collect()
}

pub fn config(w: u32, h: u32, profile: Profile, rate: Rate) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        profile,
        rate,
        gop: 250,
        threads: 2,
        ..EncoderConfig::default()
    }
}

pub fn odd_config(w: u32, h: u32, profile: Profile) -> EncoderConfig {
    EncoderConfig {
        gop: 3,
        ..config(w, h, profile, Rate::Crf(23))
    }
}

pub fn scene_cut_config() -> EncoderConfig {
    EncoderConfig {
        gop: 60,
        ..config(160, 128, Profile::High, Rate::Bitrate(600))
    }
}

pub const PINNED: [&str; 3] = ["odd_2x2_Baseline", "odd_100x58_High", "scene_cut"];

pub fn pinned_case(name: &str) -> Option<(EncoderConfig, Vec<Frame>)> {
    Some(match name {
        "odd_2x2_Baseline" => (
            odd_config(2, 2, Profile::Baseline),
            clip(Scene::Motion, 2, 2, 5),
        ),
        "odd_100x58_High" => (
            odd_config(100, 58, Profile::High),
            clip(Scene::Motion, 100, 58, 5),
        ),
        "scene_cut" => (scene_cut_config(), clip(Scene::Cut, 160, 128, CUT + 6)),
        _ => return None,
    })
}

pub fn pinned(name: &str) -> Result<Vec<u8>, String> {
    let (config, frames) = pinned_case(name).ok_or(format!("no pinned H.264 encode '{name}'"))?;
    let (avcc, packets) = encode(config, &frames)?;
    to_annexb(&avcc, &packets)
}
