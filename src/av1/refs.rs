use pito_codecs::media::Frame;

pub const AREA: &str = "av1";
pub const RATE: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Motion,
    Noisy(u8),
    Screen,
}

pub struct Stream {
    pub name: &'static str,
    pub source: Source,
    pub size: (u32, u32),
    pub frames: u64,
    pub pix_fmt: &'static str,
    pub encoder: &'static str,
    pub args: &'static [&'static str],
}

const SMALL: (u32, u32) = (128, 96);

const INTER: &[&str] = &[
    "-cpu-used",
    "2",
    "-crf",
    "36",
    "-lag-in-frames",
    "8",
    "-auto-alt-ref",
    "1",
    "-aom-params",
    "enable-masked-comp=1:enable-interinter-wedge=1:enable-diff-wtd-comp=1:\
     enable-dist-wtd-comp=1:enable-interintra-comp=1:enable-interintra-wedge=1:\
     enable-smooth-interintra=1:enable-onesided-comp=1:enable-obmc=1:\
     enable-dual-filter=1:enable-global-motion=1:enable-warped-motion=1:\
     enable-intra-edge-filter=1:enable-filter-intra=1:enable-cdef=1",
];

const PALETTE: &[&str] = &[
    "-cpu-used",
    "1",
    "-crf",
    "30",
    "-g",
    "1",
    "-aom-params",
    "tune-content=screen:enable-palette=1:enable-intrabc=1:enable-cfl-intra=1",
];

const SVT_SCREEN: &[&str] = &[
    "-preset",
    "4",
    "-crf",
    "40",
    "-g",
    "8",
    "-svtav1-params",
    "scm=1",
];

const PALETTE_INTER: &[&str] = &[
    "-cpu-used",
    "2",
    "-crf",
    "34",
    "-aom-params",
    "tune-content=screen:enable-palette=1:enable-intra-edge-filter=1:enable-filter-intra=1",
];

const RAV1E: &[&str] = &["-speed", "6", "-qp", "120", "-tiles", "2"];

const LOSSLESS: &[&str] = &["-cpu-used", "6", "-aom-params", "lossless=1"];

const SUPERRES: &[&str] = &[
    "-preset",
    "6",
    "-crf",
    "40",
    "-svtav1-params",
    "superres-mode=1:superres-denom=12:superres-kf-denom=12:enable-restoration=1",
];

const RESIZE: &[&str] = &[
    "-preset",
    "6",
    "-crf",
    "40",
    "-svtav1-params",
    "resize-mode=1:resize-denom=12:resize-kf-denom=8",
];

const GRAIN: &[&str] = &[
    "-cpu-used",
    "8",
    "-tiles",
    "2x1",
    "-denoise-noise-level",
    "20",
    "-auto-alt-ref",
    "1",
    "-lag-in-frames",
    "8",
    "-crf",
    "32",
];

const fn stream(
    name: &'static str,
    source: Source,
    frames: u64,
    pix_fmt: &'static str,
    encoder: &'static str,
    args: &'static [&'static str],
) -> Stream {
    Stream {
        name,
        source,
        size: SMALL,
        frames,
        pix_fmt,
        encoder,
        args,
    }
}

const AOM: &str = "libaom-av1";
const SVT: &str = "libsvtav1";
const P8: &str = "yuv420p";
const P10: &str = "yuv420p10le";

pub static BIT_EXACT: [Stream; 16] = [
    Stream {
        name: "grain_tiles_10bit",
        source: Source::Noisy(8),
        size: (352, 240),
        frames: 8,
        pix_fmt: P10,
        encoder: AOM,
        args: GRAIN,
    },
    stream("compound_8", Source::Motion, 10, P8, AOM, INTER),
    stream("compound_10", Source::Motion, 10, P10, AOM, INTER),
    stream("screen_8", Source::Screen, 3, P8, AOM, PALETTE),
    stream("screen_10", Source::Screen, 2, P10, AOM, PALETTE),
    stream("lossless_8", Source::Motion, 2, P8, AOM, LOSSLESS),
    stream("lossless_10", Source::Motion, 2, P10, AOM, LOSSLESS),
    stream("rav1e_8", Source::Motion, 6, P8, "librav1e", RAV1E),
    stream("rav1e_10", Source::Motion, 6, P10, "librav1e", RAV1E),
    stream("superres_10", Source::Motion, 6, P10, SVT, SUPERRES),
    stream("superres_8", Source::Motion, 4, P8, SVT, SUPERRES),
    stream("resize_8", Source::Motion, 6, P8, SVT, RESIZE),
    stream("svt_8", Source::Motion, 8, P8, SVT, SVT_SCREEN),
    stream("svt_10", Source::Motion, 8, P10, SVT, SVT_SCREEN),
    stream(
        "screen_inter_10",
        Source::Screen,
        4,
        P10,
        AOM,
        PALETTE_INTER,
    ),
    Stream {
        name: "vaapi_noisy",
        source: Source::Noisy(6),
        size: (352, 240),
        frames: 16,
        pix_fmt: P8,
        encoder: AOM,
        args: &["-cpu-used", "8", "-crf", "40", "-tile-columns", "1"],
    },
];

pub static PARSER: [Stream; 2] = [
    Stream {
        name: "small",
        source: Source::Motion,
        size: (64, 64),
        frames: 3,
        pix_fmt: P8,
        encoder: AOM,
        args: &["-cpu-used", "8"],
    },
    Stream {
        name: "corrupt",
        source: Source::Motion,
        size: (352, 240),
        frames: 8,
        pix_fmt: P8,
        encoder: AOM,
        args: &["-cpu-used", "8", "-tile-columns", "1"],
    },
];

pub fn find(name: &str) -> &'static Stream {
    BIT_EXACT
        .iter()
        .chain(PARSER.iter())
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("src/av1/refs.rs has no stream named {name}"))
}

pub fn ivf(name: &str) -> String {
    format!("{name}.ivf")
}

pub fn decoded(name: &str) -> String {
    format!("{name}.yuv")
}

fn packed(f: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    for p in &f.planes {
        for row in p.data.chunks(p.stride.max(1)).take(p.height) {
            out.extend_from_slice(&row[..p.width.min(row.len())]);
        }
    }
    out
}

fn screen(w: u32, h: u32, n: u64) -> Frame {
    use crate::testref::{rgb_at, yuv_of};
    use pito_codecs::media::{PixelFormat, TimeBase};
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, n as i64, TimeBase::new(1, 1));
    let at = |x: u32, y: u32| yuv_of(rgb_at(w, h, n, x, y).map(|c| c & 0xc0));
    let [yp, up, vp] = &mut f.planes;
    for y in 0..h {
        for x in 0..w {
            yp.data[y as usize * yp.stride + x as usize] = at(x, y)[0];
        }
    }
    for cy in 0..up.height {
        for cx in 0..up.width {
            let c = at(2 * cx as u32, 2 * cy as u32);
            up.data[cy * up.stride + cx] = c[1];
            vp.data[cy * vp.stride + cx] = c[2];
        }
    }
    f
}

pub fn source_bytes(s: &Stream) -> Vec<u8> {
    let (w, h) = s.size;
    (0..s.frames)
        .flat_map(|n| match s.source {
            Source::Motion => packed(&crate::testref::yuv_frame(w, h, n, 0)),
            Source::Noisy(amp) => packed(&crate::testref::yuv_frame(w, h, n, amp)),
            Source::Screen => packed(&screen(w, h, n)),
        })
        .collect()
}
