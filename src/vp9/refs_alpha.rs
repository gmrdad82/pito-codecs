use pito_codecs::media::{Canvas, Codec, ExportOptions, Rgba, export_rgba};
use std::path::Path;

pub const AREA: &str = "vp9";
pub const SIZE: (u32, u32) = (192, 108);
pub const FRAMES: u64 = 24;
pub const LIBVPX_FRAMES: u64 = 8;
pub const OURS: &str = "alpha-round-trip";
pub const LIBVPX: &str = "alpha-libvpx";
pub const LIBVPX_ARGS: &[&str] = &[
    "--end-usage=q",
    "--cq-level=30",
    "--good",
    "--cpu-used=4",
    "--passes=1",
];

pub fn source(n: u64) -> Rgba {
    let (w, h) = SIZE;
    let mut img = Rgba::new(w, h);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let o = (y * w as usize + x) * 4;
            let cx = 40 + n as i64 * 4;
            let d = ((x as i64 - cx).pow(2) + (y as i64 - 54).pow(2)) as f64;
            img.data[o] = (x * 255 / w as usize) as u8;
            img.data[o + 1] = (y * 255 / h as usize) as u8;
            img.data[o + 2] = ((x + y + n as usize * 8) % 256) as u8;
            img.data[o + 3] = (255.0 - d.sqrt() * 4.0).clamp(0.0, 255.0) as u8;
        }
    }
    img
}

pub fn source_bytes(frames: u64) -> Vec<u8> {
    (0..frames).flat_map(|n| source(n).data).collect()
}

pub fn export_ours(out: &Path) -> Result<u64, String> {
    let opts = ExportOptions {
        video: Codec::Vp9,
        ..ExportOptions::default()
    };
    let canvas = Canvas {
        width: SIZE.0,
        height: SIZE.1,
        fps: (24, 1),
        frames: Some(FRAMES),
    };
    export_rgba(
        out,
        canvas,
        true,
        opts,
        |n, _| Ok(Some(source(n))),
        |_| true,
    )
    .map(|done| done.frames)
    .map_err(|e| e.to_string())
}

pub fn ours_webm() -> Result<Vec<u8>, String> {
    let out = crate::testref::scratch(AREA).join(format!("{OURS}.made.webm"));
    export_ours(&out)?;
    std::fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))
}

pub type Frames = Vec<Vec<u8>>;

pub fn webm_streams(path: &Path) -> Result<(Frames, Frames), String> {
    let r = pito_codecs::mkv::Reader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let t = r
        .video()
        .ok_or_else(|| format!("{}: no video track", path.display()))?;
    let (mut colour, mut alpha, mut extra) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..r.tracks[t].samples.len() {
        colour.push(r.packet(t, i).map_err(|e| e.to_string())?.data);
        if !r.read_alpha(t, i, &mut extra).map_err(|e| e.to_string())? {
            return Err(format!("{}: frame {i} has no alpha", path.display()));
        }
        alpha.push(extra.clone());
    }
    Ok((colour, alpha))
}
