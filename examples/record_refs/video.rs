use crate::testref;
use crate::{Def, sha, tool};

#[allow(dead_code)]
#[path = "../../src/h264/refs.rs"]
mod h264;
#[allow(dead_code)]
#[path = "../../src/hevc/refs.rs"]
mod hevc;
#[allow(dead_code)]
#[path = "../../src/hevc/refs_synth.rs"]
mod hevc_synth;
#[allow(dead_code)]
#[path = "../../src/vp9/refs.rs"]
mod vp9;
#[allow(dead_code)]
#[path = "../../src/vp9/refs_alpha.rs"]
mod vp9_alpha;

pub const X264: &str = "x264";
pub const X265: &str = "x265";
const VPXENC: &str = "vpxenc";
const VPXDEC: &str = "vpxdec";

pub fn i420(w: u32, h: u32) -> usize {
    let (w, h) = (w as usize, h as usize);
    w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
}

pub fn y4m(
    w: u32,
    h: u32,
    fps: u32,
    csp: &str,
    frame: usize,
    raw: &[u8],
) -> Result<Vec<u8>, String> {
    if frame == 0 || !raw.len().is_multiple_of(frame) {
        return Err(format!(
            "{} bytes are not whole {w}x{h} C{csp} frames of {frame} bytes",
            raw.len()
        ));
    }
    let mut out = format!("YUV4MPEG2 W{w} H{h} F{fps}:1 Ip A1:1 C{csp}\n").into_bytes();
    for f in raw.chunks_exact(frame) {
        out.extend_from_slice(b"FRAME\n");
        out.extend_from_slice(f);
    }
    Ok(out)
}

pub fn upshift(raw: &[u8]) -> Vec<u8> {
    raw.iter()
        .flat_map(|&v| (u16::from(v) << 2).to_le_bytes())
        .collect()
}

struct Y4m<'a> {
    w: usize,
    h: usize,
    frames: Vec<&'a [u8]>,
}

fn y4m_frames(bytes: &[u8]) -> Result<Y4m<'_>, String> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("no y4m header")?;
    let head = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
    let field = |tag: char| {
        head.split(' ')
            .find_map(|t| t.strip_prefix(tag))
            .ok_or(format!("y4m header '{head}' has no {tag}"))
    };
    let w: usize = field('W')?.parse().map_err(|e| format!("width: {e}"))?;
    let h: usize = field('H')?.parse().map_err(|e| format!("height: {e}"))?;
    let bytes_per_sample = match field('C')? {
        "420" | "420jpeg" | "420mpeg2" | "420paldv" => 1,
        "420p10" | "420p12" => 2,
        other => return Err(format!("y4m colourspace C{other} is not 4:2:0")),
    };
    let size = (w * h + 2 * w.div_ceil(2) * h.div_ceil(2)) * bytes_per_sample;
    let mut frames = Vec::new();
    let mut at = end + 1;
    while at < bytes.len() {
        if !bytes[at..].starts_with(b"FRAME") {
            return Err(format!("no FRAME at byte {at}"));
        }
        let line = bytes[at..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("a FRAME line has no end")?;
        at += line + 1;
        frames.push(
            bytes
                .get(at..at + size)
                .ok_or(format!("a frame ends early at byte {at}"))?,
        );
        at += size;
    }
    Ok(Y4m { w, h, frames })
}

fn y4m_luma(bytes: &[u8]) -> Result<String, String> {
    let y = y4m_frames(bytes)?;
    let mut d = testref::Digest::new();
    for f in &y.frames {
        if f.len() != i420(y.w as u32, y.h as u32) {
            return Err("the alpha pictures are not 8-bit".into());
        }
        d.bytes(&f[..y.w * y.h]);
    }
    Ok(d.hex())
}

pub fn y4m_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(y4m_frames(bytes)?.frames.concat())
}

pub fn written(
    area: &'static str,
    name: String,
    file: String,
    what: &str,
    take: fn(&[u8]) -> Result<Vec<u8>, String>,
) -> Def {
    Def::ours(area, name, what, move || {
        let at = testref::root()
            .join("tmp/record_refs")
            .join(area)
            .join(&file);
        take(&std::fs::read(&at).map_err(|e| format!("{}: {e}", at.display()))?)
    })
}

pub fn raw(bytes: &[u8]) -> Result<Vec<u8>, String> {
    Ok(bytes.to_vec())
}

pub fn x264_args(frames: u64, opts: &[&str], recon: &str) -> Vec<String> {
    let frames = frames.to_string();
    let mut a = vec![
        "--quiet",
        "--no-progress",
        "--demuxer",
        "y4m",
        "--threads",
        "1",
        "--frames",
        &frames,
    ];
    a.extend_from_slice(opts);
    a.extend(["--dump-yuv", recon, "-o", "{out}", "{src}"]);
    tool(X264, &a)
}

pub fn x265_args(frames: u64, ten: bool, opts: &[String], recon: &str) -> Vec<String> {
    let frames = frames.to_string();
    let depth = if ten { "10" } else { "8" };
    let mut a = vec![
        "--log-level",
        "error",
        "--y4m",
        "--input",
        "{src}",
        "--frames",
        &frames,
        "--frame-threads",
        "1",
        "--output-depth",
        depth,
        "--recon-depth",
        depth,
        "--recon",
        recon,
        "--output",
        "{out}",
    ];
    a.extend(opts.iter().map(String::as_str));
    tool(X265, &a)
}

fn h264_defs() -> Vec<Def> {
    let area = h264::AREA;
    let mut defs = Vec::new();
    for s in &h264::X264_STREAMS {
        let (name, (w, h)) = (s.name, s.size);
        let stream = h264::stream(name);
        let recon = format!("{name}.recon.yuv");
        defs.push(
            Def::fixture(
                area,
                stream.clone(),
                X264,
                x264_args(s.frames, s.opts, &recon),
            )
            .from(move || y4m(w, h, 25, "420jpeg", i420(w, h), &h264::x264_source(name)?)),
        );
        defs.push(written(
            area,
            h264::recon(name),
            recon,
            &format!("the --dump-yuv reconstruction x264 wrote while making {{ref:{stream}}}"),
            raw,
        ));
    }
    defs
}

fn hevc_decode(stream: &[u8]) -> Result<Vec<u8>, String> {
    use pito_codecs::media::Packet;
    let mut dec = pito_codecs::hevc::Decoder::with_threads(1);
    let packet = Packet {
        data: stream.to_vec(),
        pts: 0,
        dts: 0,
        keyframe: true,
    };
    let mut frames = dec.decode(&packet).map_err(|e| e.to_string())?;
    frames.extend(dec.flush().map_err(|e| e.to_string())?);
    let mut d = Vec::new();
    for f in &frames {
        let bytes = f.format.bytes_per_sample();
        for p in &f.planes {
            for row in p.data.chunks(p.stride.max(1)).take(p.height) {
                d.extend_from_slice(&row[..(p.width * bytes).min(row.len())]);
            }
        }
    }
    Ok(d)
}

fn hevc_defs() -> Vec<Def> {
    let area = hevc::AREA;
    let mut defs = vec![Def::ours(
        area,
        hevc::SCALING_LIST.into(),
        "src/hevc/refs.rs scaling_list_text()",
        || Ok(hevc::scaling_list_text().into_bytes()),
    )];
    for f in hevc::all() {
        let (name, (w, h)) = (f.name, f.size);
        let stream = hevc::stream(name);
        let recon = format!("{name}.recon.y4m");
        let mut opts = Vec::new();
        if !f.preset.is_empty() {
            opts.extend(["--preset".to_string(), f.preset.to_string()]);
        }
        opts.extend(hevc::x265_cli(f.params));
        let (csp, bytes) = if f.ten_bit {
            ("420p10", 2)
        } else {
            ("420jpeg", 1)
        };
        defs.push(
            Def::fixture(
                area,
                stream.clone(),
                X265,
                x265_args(f.frames, f.ten_bit, &opts, &recon),
            )
            .from(move || y4m(w, h, 25, csp, i420(w, h) * bytes, &hevc::source(name)?)),
        );
        defs.push(written(
            area,
            hevc::recon(name),
            recon,
            &format!("the --recon reconstruction x265 wrote while making {{ref:{stream}}}"),
            y4m_payload,
        ));
    }
    for name in hevc_synth::DEBLOCKED {
        let file = hevc_synth::file(name);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/hevc/refs_synth.rs stream(\"{name}\")"),
            move || hevc_synth::stream(name),
        ));
        defs.push(Def::ours(
            area,
            hevc_synth::decoded(name),
            &format!("our one-thread HEVC decode of {{ref:{file}}}, a regression pin"),
            move || hevc_decode(&hevc_synth::stream(name)?),
        ));
    }
    defs
}

fn vpxenc(frames: u64, opts: &[&str]) -> Vec<String> {
    let limit = format!("--limit={frames}");
    let mut a = vec!["--codec=vp9", "--ivf", "--quiet", &limit];
    a.extend_from_slice(opts);
    a.extend(["-o", "{out}", "{src}"]);
    tool(VPXENC, &a)
}

fn vpxdec(area: &'static str, name: String, input: &str) -> Def {
    let input = format!("{{ref:{input}}}");
    Def::digest(
        area,
        name,
        VPXDEC,
        tool(
            VPXDEC,
            &["--codec=vp9", "--rawvideo", "-o", "{out}", &input],
        ),
        sha,
    )
}

fn vpxdec_luma(area: &'static str, name: String, input: &str) -> Def {
    let input = format!("{{ref:{input}}}");
    Def::digest(
        area,
        name,
        VPXDEC,
        tool(VPXDEC, &["--codec=vp9", "-o", "{out}", &input]),
        y4m_luma,
    )
}

type Rgba = fn() -> Result<Vec<u8>, String>;

fn alpha_pair(
    area: &'static str,
    name: &'static str,
    (w, h): (u32, u32),
    fps: u32,
    frames: u64,
    opts: &'static [&'static str],
    rgba: Rgba,
) -> Vec<Def> {
    let picture = move |alpha: bool| -> Result<Vec<u8>, String> {
        let (colour, a) = vp9::split_rgba(&rgba()?, w, h);
        y4m(
            w,
            h,
            fps,
            "420jpeg",
            i420(w, h),
            if alpha { &a } else { &colour },
        )
    };
    vec![
        Def::fixture(area, vp9::stream(name), VPXENC, vpxenc(frames, opts))
            .from(move || picture(false)),
        Def::fixture(area, vp9::alpha_stream(name), VPXENC, vpxenc(frames, opts))
            .from(move || picture(true)),
        vpxdec(area, vp9::decoded(name), &vp9::stream(name)),
        vpxdec_luma(area, vp9::alpha_decoded(name), &vp9::alpha_stream(name)),
    ]
}

fn ours_alpha(alpha: bool) -> Result<Vec<u8>, String> {
    let webm = testref::scratch(vp9_alpha::AREA).join(format!("{}.recorded.webm", vp9_alpha::OURS));
    vp9_alpha::export_ours(&webm)?;
    let (colour, a) = vp9_alpha::webm_streams(&webm)?;
    let (w, h) = vp9_alpha::SIZE;
    Ok(vp9::ivf(w, h, if alpha { &a } else { &colour }))
}

fn vp9_defs() -> Vec<Def> {
    let area = vp9::AREA;
    let mut defs = Vec::new();
    for f in &vp9::CONFORMANCE {
        let (name, (w, h)) = (f.name, f.size);
        let stream = vp9::stream(name);
        let frame = vp9::frame_bytes(f);
        let csp = f.csp;
        defs.push(
            Def::fixture(area, stream.clone(), VPXENC, vpxenc(f.frames, f.args))
                .from(move || y4m(w, h, 25, csp, frame, &vp9::source(name)?)),
        );
        defs.push(vpxdec(area, vp9::decoded(name), &stream));
    }
    for o in vp9::ours() {
        let name = o.name;
        let stream = vp9::stream(name);
        defs.push(Def::ours(
            area,
            stream.clone(),
            &format!("src/vp9/refs.rs encode_ours(\"{name}\")"),
            move || vp9::encode_ours(name),
        ));
        defs.push(vpxdec(area, vp9::decoded(name), &stream));
    }
    for (w, h) in vp9::EDGES {
        let name = vp9::edge(w, h);
        let stream = vp9::stream(&name);
        defs.push(Def::ours(
            area,
            stream.clone(),
            &format!("src/vp9/refs.rs edge_ivf({w}, {h})"),
            move || vp9::edge_ivf(w, h),
        ));
        defs.push(vpxdec(area, vp9::decoded(&name), &stream));
    }
    for (name, w, h) in vp9::ALPHA {
        let rgba: Rgba = match name {
            "alpha-96x64" => || vp9::alpha_source("alpha-96x64"),
            "alpha-66x50" => || vp9::alpha_source("alpha-66x50"),
            _ => || vp9::alpha_source("alpha-65x49"),
        };
        defs.extend(alpha_pair(
            area,
            name,
            (w, h),
            10,
            vp9::ALPHA_FRAMES,
            vp9::ALPHA_ARGS,
            rgba,
        ));
    }
    defs.extend(alpha_pair(
        area,
        vp9_alpha::LIBVPX,
        vp9_alpha::SIZE,
        24,
        vp9_alpha::LIBVPX_FRAMES,
        vp9_alpha::LIBVPX_ARGS,
        || Ok(vp9_alpha::source_bytes(vp9_alpha::LIBVPX_FRAMES)),
    ));
    let ours = vp9_alpha::OURS;
    defs.push(Def::ours(
        area,
        format!("{ours}.webm"),
        "src/vp9/refs_alpha.rs ours_webm()",
        vp9_alpha::ours_webm,
    ));
    defs.push(Def::ours(
        area,
        vp9::stream(ours),
        "the colour frames of src/vp9/refs_alpha.rs ours_webm() in IVF",
        || ours_alpha(false),
    ));
    defs.push(Def::ours(
        area,
        vp9::alpha_stream(ours),
        "the alpha frames of src/vp9/refs_alpha.rs ours_webm() in IVF",
        || ours_alpha(true),
    ));
    defs.push(vpxdec(area, vp9::decoded(ours), &vp9::stream(ours)));
    defs.push(vpxdec_luma(
        area,
        vp9::alpha_decoded(ours),
        &vp9::alpha_stream(ours),
    ));
    defs
}

pub fn defs() -> Vec<Def> {
    [h264_defs(), hevc_defs(), vp9_defs()]
        .into_iter()
        .flatten()
        .collect()
}
