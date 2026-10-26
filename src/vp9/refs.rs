use pito_codecs::media::Frame;
use pito_codecs::vp9::{Encoder, EncoderConfig, Rate};

pub const AREA: &str = "vp9";

pub struct Libvpx {
    pub name: &'static str,
    pub size: (u32, u32),
    pub frames: u64,
    pub amp: u8,
    pub pix_fmt: &'static str,
    pub two_pass: bool,
    pub args: &'static [&'static str],
}

const fn fx(
    name: &'static str,
    size: (u32, u32),
    frames: u64,
    pix_fmt: &'static str,
    two_pass: bool,
    args: &'static [&'static str],
) -> Libvpx {
    Libvpx {
        name,
        size,
        frames,
        amp: 6,
        pix_fmt,
        two_pass,
        args,
    }
}

pub const GPU_FLOOR: (u32, u32) = (352, 240);
const QCIF: (u32, u32) = (176, 144);

pub const CONFORMANCE: [Libvpx; 8] = [
    fx(
        "good",
        GPU_FLOOR,
        14,
        "yuv420p",
        false,
        &["-b:v", "300k", "-frame-parallel", "0"],
    ),
    fx(
        "altref_10bit",
        GPU_FLOOR,
        12,
        "yuv420p10le",
        true,
        &[
            "-b:v",
            "300k",
            "-profile:v",
            "2",
            "-auto-alt-ref",
            "1",
            "-lag-in-frames",
            "16",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "tiles_512x240",
        (512, 240),
        4,
        "yuv420p",
        false,
        &[
            "-b:v",
            "600k",
            "-tile-columns",
            "1",
            "-tile-rows",
            "1",
            "-speed",
            "4",
            "-frame-parallel",
            "0",
        ],
    ),
    fx(
        "profile1_444",
        QCIF,
        6,
        "yuv444p",
        false,
        &["-b:v", "300k", "-profile:v", "1", "-frame-parallel", "0"],
    ),
    fx(
        "profile3_422_12bit",
        QCIF,
        4,
        "yuv422p12le",
        false,
        &["-b:v", "300k", "-profile:v", "3", "-frame-parallel", "0"],
    ),
    fx(
        "lossless",
        (64, 64),
        4,
        "yuv420p",
        false,
        &["-lossless", "1", "-frame-parallel", "0"],
    ),
    fx(
        "odd_aq",
        (173, 125),
        8,
        "yuv420p",
        false,
        &["-b:v", "300k", "-aq-mode", "3", "-frame-parallel", "0"],
    ),
    fx(
        "realtime_layers",
        QCIF,
        10,
        "yuv420p",
        false,
        &[
            "-b:v",
            "300k",
            "-speed",
            "8",
            "-deadline",
            "realtime",
            "-error-resilient",
            "1",
            "-ts-parameters",
            "ts_number_layers=2:ts_target_bitrate=150,300:ts_rate_decimator=2,1:ts_periodicity=2:ts_layer_id=0,1:ts_layering_mode=2",
        ],
    ),
];

pub const ALPHA: [(&str, u32, u32); 3] = [
    ("alpha-96x64", 96, 64),
    ("alpha-66x50", 66, 50),
    ("alpha-65x49", 65, 49),
];

pub const ALPHA_FRAMES: u64 = 6;

pub struct Ours {
    pub name: &'static str,
    pub size: (u32, u32),
    pub frames: u64,
    pub config: EncoderConfig,
    pub min_psnr: f64,
}

pub fn ours() -> [Ours; 2] {
    let cfg = |(width, height), rate, threads, aq| EncoderConfig {
        width,
        height,
        rate,
        gop: 6,
        threads,
        aq,
        ..Default::default()
    };
    [
        Ours {
            name: "exact",
            size: (200, 120),
            frames: 8,
            config: cfg((200, 120), Rate::Q(60), 1, false),
            min_psnr: 34.0,
        },
        Ours {
            name: "tiles",
            size: (712, 200),
            frames: 4,
            config: cfg((712, 200), Rate::Crf(30), 4, true),
            min_psnr: 32.0,
        },
    ]
}

pub fn find(name: &str) -> Result<&'static Libvpx, String> {
    CONFORMANCE
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| format!("no libvpx stream named {name}"))
}

fn chroma(pix_fmt: &str, (w, h): (u32, u32)) -> (u32, u32, u32, u32) {
    match pix_fmt.get(..6) {
        Some("yuv420") => (w.div_ceil(2), h.div_ceil(2), 2, 2),
        Some("yuv422") => (w.div_ceil(2), h, 2, 1),
        _ => (w, h, 1, 1),
    }
}

fn depth(pix_fmt: &str) -> u32 {
    if pix_fmt.ends_with("12le") {
        12
    } else if pix_fmt.ends_with("10le") {
        10
    } else {
        8
    }
}

pub fn frame_bytes(f: &Libvpx) -> usize {
    let (w, h) = (f.size.0 as usize, f.size.1 as usize);
    let (cw, ch, _, _) = chroma(f.pix_fmt, f.size);
    let bps = if depth(f.pix_fmt) > 8 { 2 } else { 1 };
    (w * h + 2 * cw as usize * ch as usize) * bps
}

pub fn source_bytes(f: &Libvpx) -> Vec<u8> {
    let (w, h) = f.size;
    let (cw, ch, sx, sy) = chroma(f.pix_fmt, f.size);
    let bits = depth(f.pix_fmt);
    let mut out = Vec::with_capacity(frame_bytes(f) * f.frames as usize);
    for n in 0..f.frames {
        let px = |x: u32, y: u32| {
            crate::testref::yuv_of(crate::testref::rgb_noisy(
                w,
                h,
                n,
                x.min(w - 1),
                y.min(h - 1),
                f.amp,
            ))
        };
        let mut put = |v: u32, x: u32, y: u32, k: u32| {
            if bits == 8 {
                out.push(v as u8);
            } else {
                let low = (x * 3 + y * 5 + n as u32 + k) & ((1 << (bits - 8)) - 1);
                out.extend_from_slice(&((v << (bits - 8)) as u16 | low as u16).to_le_bytes());
            }
        };
        for y in 0..h {
            for x in 0..w {
                put(u32::from(px(x, y)[0]), x, y, 0);
            }
        }
        for k in 1..3 {
            for y in 0..ch {
                for x in 0..cw {
                    let mut sum = 0;
                    for dy in 0..sy {
                        for dx in 0..sx {
                            sum += u32::from(px(x * sx + dx, y * sy + dy)[k]);
                        }
                    }
                    let n = sx * sy;
                    put((sum + n / 2) / n, x, y, k as u32);
                }
            }
        }
    }
    out
}

pub fn source(name: &str) -> Result<Vec<u8>, String> {
    Ok(source_bytes(find(name)?))
}

pub fn alpha_rgba(w: u32, h: u32, frames: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize * frames as usize);
    for n in 0..frames {
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = crate::testref::rgb_at(w, h, n, x, y);
                let a = (u64::from(x) * 5 + u64::from(y) * 3 + n * 17) % 256;
                out.extend_from_slice(&[r, g, b, a as u8]);
            }
        }
    }
    out
}

pub fn alpha_source(name: &str) -> Result<Vec<u8>, String> {
    let (_, w, h) = ALPHA
        .iter()
        .find(|a| a.0 == name)
        .ok_or_else(|| format!("no alpha stream named {name}"))?;
    Ok(alpha_rgba(*w, *h, ALPHA_FRAMES))
}

pub fn ours_source(o: &Ours) -> Vec<Frame> {
    (0..o.frames)
        .map(|n| crate::testref::yuv_frame(o.size.0, o.size.1, n, 6))
        .collect()
}

pub fn ivf(w: u32, h: u32, packets: &[Vec<u8>]) -> Vec<u8> {
    let mut o = Vec::new();
    o.extend_from_slice(b"DKIF");
    o.extend_from_slice(&0u16.to_le_bytes());
    o.extend_from_slice(&32u16.to_le_bytes());
    o.extend_from_slice(b"VP90");
    o.extend_from_slice(&(w as u16).to_le_bytes());
    o.extend_from_slice(&(h as u16).to_le_bytes());
    o.extend_from_slice(&25u32.to_le_bytes());
    o.extend_from_slice(&1u32.to_le_bytes());
    o.extend_from_slice(&(packets.len() as u32).to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    for (i, p) in packets.iter().enumerate() {
        o.extend_from_slice(&(p.len() as u32).to_le_bytes());
        o.extend_from_slice(&(i as u64).to_le_bytes());
        o.extend_from_slice(p);
    }
    o
}

pub fn encode_ours_with(o: &Ours, mut each: impl FnMut(&Encoder)) -> Result<Vec<u8>, String> {
    let mut enc = Encoder::new(o.config.clone()).map_err(|e| e.to_string())?;
    let mut packets = Vec::new();
    for f in &ours_source(o) {
        for p in enc.encode(f).map_err(|e| e.to_string())? {
            packets.push(p.data);
        }
        each(&enc);
    }
    Ok(ivf(o.size.0, o.size.1, &packets))
}

pub fn encode_ours(name: &str) -> Result<Vec<u8>, String> {
    let o = ours()
        .into_iter()
        .find(|o| o.name == name)
        .ok_or_else(|| format!("no encoder setting named {name}"))?;
    encode_ours_with(&o, |_| {})
}
