use pito_codecs::media::{Frame, PixelFormat, TimeBase};
use pito_codecs::vp9::{Encoder, EncoderConfig, Rate};

pub const AREA: &str = "vp9";

pub struct Libvpx {
    pub name: &'static str,
    pub size: (u32, u32),
    pub frames: u64,
    pub amp: u8,
    pub csp: &'static str,
    pub args: &'static [&'static str],
}

const fn fx(
    name: &'static str,
    size: (u32, u32),
    frames: u64,
    csp: &'static str,
    args: &'static [&'static str],
) -> Libvpx {
    Libvpx {
        name,
        size,
        frames,
        amp: 6,
        csp,
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
        "420jpeg",
        &["--target-bitrate=300", "--frame-parallel=0", "--passes=1"],
    ),
    fx(
        "altref_10bit",
        GPU_FLOOR,
        12,
        "420p10",
        &[
            "--target-bitrate=300",
            "--profile=2",
            "--bit-depth=10",
            "--input-bit-depth=10",
            "--auto-alt-ref=1",
            "--lag-in-frames=16",
            "--frame-parallel=0",
            "--passes=2",
        ],
    ),
    fx(
        "tiles_512x240",
        (512, 240),
        4,
        "420jpeg",
        &[
            "--target-bitrate=600",
            "--tile-columns=1",
            "--tile-rows=1",
            "--cpu-used=4",
            "--frame-parallel=0",
            "--passes=1",
        ],
    ),
    fx(
        "profile1_444",
        QCIF,
        6,
        "444",
        &[
            "--target-bitrate=300",
            "--profile=1",
            "--frame-parallel=0",
            "--passes=1",
        ],
    ),
    fx(
        "profile3_422_12bit",
        QCIF,
        4,
        "422p12",
        &[
            "--target-bitrate=300",
            "--profile=3",
            "--bit-depth=12",
            "--input-bit-depth=12",
            "--frame-parallel=0",
            "--passes=1",
        ],
    ),
    fx(
        "lossless",
        (64, 64),
        4,
        "420jpeg",
        &["--lossless=1", "--frame-parallel=0", "--passes=1"],
    ),
    fx(
        "odd_aq",
        (173, 125),
        8,
        "420jpeg",
        &[
            "--target-bitrate=300",
            "--aq-mode=3",
            "--frame-parallel=0",
            "--passes=1",
        ],
    ),
    fx(
        "realtime_resilient",
        QCIF,
        10,
        "420jpeg",
        &[
            "--target-bitrate=300",
            "--rt",
            "--cpu-used=8",
            "--error-resilient=1",
            "--passes=1",
        ],
    ),
];

pub const ALPHA: [(&str, u32, u32); 3] = [
    ("alpha-96x64", 96, 64),
    ("alpha-66x50", 66, 50),
    ("alpha-65x49", 65, 49),
];

pub const ALPHA_FRAMES: u64 = 6;

pub const ALPHA_ARGS: &[&str] = &["--target-bitrate=100", "--kf-max-dist=4", "--passes=1"];

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

pub const EDGES: [(u32, u32); 5] = [(1080, 1920), (1080, 1080), (200, 120), (66, 66), (8, 8)];

pub const EDGE_FRAMES: u32 = 3;

pub fn find(name: &str) -> Result<&'static Libvpx, String> {
    CONFORMANCE
        .iter()
        .find(|f| f.name == name)
        .ok_or_else(|| format!("no libvpx stream named {name}"))
}

pub fn stream(name: &str) -> String {
    format!("{name}.ivf")
}

pub fn decoded(name: &str) -> String {
    format!("{name}.yuv")
}

pub fn alpha_stream(name: &str) -> String {
    format!("{name}.alpha.ivf")
}

pub fn alpha_decoded(name: &str) -> String {
    format!("{name}.alpha")
}

pub fn edge(w: u32, h: u32) -> String {
    format!("edge-{w}x{h}")
}

pub fn chroma(csp: &str, (w, h): (u32, u32)) -> (u32, u32, u32, u32) {
    match csp.get(..3) {
        Some("420") => (w.div_ceil(2), h.div_ceil(2), 2, 2),
        Some("422") => (w.div_ceil(2), h, 2, 1),
        _ => (w, h, 1, 1),
    }
}

pub fn depth(csp: &str) -> u32 {
    match csp.split_once('p').map(|(_, d)| d) {
        Some("10") => 10,
        Some("12") => 12,
        _ => 8,
    }
}

pub fn frame_bytes(f: &Libvpx) -> usize {
    let (w, h) = (f.size.0 as usize, f.size.1 as usize);
    let (cw, ch, _, _) = chroma(f.csp, f.size);
    let bps = if depth(f.csp) > 8 { 2 } else { 1 };
    (w * h + 2 * cw as usize * ch as usize) * bps
}

pub fn source_bytes(f: &Libvpx) -> Vec<u8> {
    let (w, h) = f.size;
    let (cw, ch, sx, sy) = chroma(f.csp, f.size);
    let bits = depth(f.csp);
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

pub fn split_rgba(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let (mut colour, mut alpha) = (Vec::new(), Vec::new());
    for frame in rgba.chunks_exact(w * h * 4) {
        let at = |x: usize, y: usize| {
            let o = (y.min(h - 1) * w + x.min(w - 1)) * 4;
            (
                crate::testref::yuv_of([frame[o], frame[o + 1], frame[o + 2]]),
                frame[o + 3],
            )
        };
        for y in 0..h {
            for x in 0..w {
                colour.push(at(x, y).0[0]);
                alpha.push(at(x, y).1);
            }
        }
        for k in 1..3 {
            for y in 0..ch {
                for x in 0..cw {
                    let sum: u32 = [(0, 0), (1, 0), (0, 1), (1, 1)]
                        .iter()
                        .map(|&(dx, dy)| u32::from(at(2 * x + dx, 2 * y + dy).0[k]))
                        .sum();
                    colour.push(((sum + 2) / 4) as u8);
                }
            }
        }
        alpha.resize(alpha.len() + 2 * cw * ch, 128);
    }
    (colour, alpha)
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

pub fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let mut pos = u16::from_le_bytes([data[6], data[7]]) as usize;
    while pos + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 12;
        if pos + size > data.len() {
            break;
        }
        out.push(&data[pos..pos + size]);
        pos += size;
    }
    out
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

pub fn moving(w: u32, h: u32, i: u32) -> Frame {
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 25));
    let (dx, dy) = (3 * i as i64, 2 * i as i64);
    for (p, plane) in f.planes.iter_mut().enumerate() {
        let s = if p == 0 { 1 } else { 2 };
        for y in 0..plane.height {
            for x in 0..plane.width {
                let gx = (x as i64 * s - dx) as f64;
                let gy = (y as i64 * s - dy) as f64;
                let wave = 50.0 * ((gx / 23.0).sin() + (gy / 17.0 + gx / 41.0).cos());
                let base = 128.0 + wave + if p == 0 { 0.0 } else { 20.0 * p as f64 - 30.0 };
                plane.data[y * plane.stride + x] = base.clamp(0.0, 255.0) as u8;
            }
        }
    }
    f
}

pub fn edge_config(w: u32, h: u32, threads: usize) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        rate: Rate::Q(60),
        gop: 30,
        threads,
        ..EncoderConfig::default()
    }
}

pub fn edge_ivf(w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut enc = Encoder::new(edge_config(w, h, 1)).map_err(|e| e.to_string())?;
    let mut packets = Vec::new();
    for i in 0..EDGE_FRAMES {
        for p in enc.encode(&moving(w, h, i)).map_err(|e| e.to_string())? {
            packets.push(p.data);
        }
    }
    Ok(ivf(w, h, &packets))
}
