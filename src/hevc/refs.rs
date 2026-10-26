pub const AREA: &str = "hevc";

pub struct X265 {
    pub name: &'static str,
    pub size: (u32, u32),
    pub frames: u64,
    pub amp: u8,
    pub fade: u64,
    pub ten_bit: bool,
    pub preset: &'static str,
    pub params: &'static str,
}

const fn fx(
    name: &'static str,
    size: (u32, u32),
    frames: u64,
    amp: u8,
    ten_bit: bool,
    preset: &'static str,
    params: &'static str,
) -> X265 {
    X265 {
        name,
        size,
        frames,
        amp,
        fade: 0,
        ten_bit,
        preset,
        params,
    }
}

const fn faded(f: X265, fade: u64) -> X265 {
    X265 { fade, amp: 0, ..f }
}

const QCIF: (u32, u32) = (176, 144);
const SMALL: (u32, u32) = (128, 96);
const AMP: u8 = 6;

pub const SCALING_LIST: &str = "scaling_list.txt";

pub const CONFORMANCE: [X265; 9] = [
    fx("intra_main10", QCIF, 3, AMP, true, "", "keyint=1"),
    fx("medium", QCIF, 10, AMP, false, "medium", ""),
    fx("slow_main10", QCIF, 6, 4, true, "slow", ""),
    fx(
        "bframes",
        QCIF,
        12,
        AMP,
        false,
        "",
        "bframes=8:b-pyramid:ref=4",
    ),
    faded(
        fx("weightp_main10", QCIF, 12, AMP, true, "", "weightp:weightb"),
        10,
    ),
    fx(
        "open_gop",
        QCIF,
        20,
        AMP,
        false,
        "",
        "keyint=8:min-keyint=8:open-gop:bframes=3",
    ),
    fx(
        "tools",
        QCIF,
        6,
        AMP,
        false,
        "",
        "slices=4:scaling-list=default:tskip:rect:amp",
    ),
    fx("lossless", (64, 64), 4, 4, false, "", "lossless"),
    fx("odd_350x238", (350, 238), 2, 4, false, "veryfast", ""),
];

pub const FEATURES: [X265; 11] = [
    fx(
        "wpp_crop_66x70",
        (66, 70),
        10,
        AMP,
        false,
        "",
        "ctu=16:bframes=3",
    ),
    fx(
        "wpp_slices_main10",
        SMALL,
        8,
        AMP,
        true,
        "",
        "ctu=16:slices=3",
    ),
    fx(
        "long_b_runs",
        SMALL,
        18,
        AMP,
        false,
        "",
        "bframes=16:b-pyramid:b-adapt=0:ref=6",
    ),
    faded(
        fx(
            "pyramid_weightb_main10",
            SMALL,
            12,
            AMP,
            true,
            "",
            "bframes=8:b-pyramid:ref=5:weightp:weightb",
        ),
        10,
    ),
    faded(
        fx(
            "weightp_8bit",
            SMALL,
            10,
            AMP,
            false,
            "",
            "bframes=2:weightp:weightb",
        ),
        8,
    ),
    fx(
        "scaling_tskip_main10",
        SMALL,
        6,
        AMP,
        true,
        "",
        "scaling-list=default:tskip:rdoq-level=0",
    ),
    fx(
        "cu_qp_chroma_offsets",
        SMALL,
        6,
        AMP,
        false,
        "",
        "ctu=16:aq-mode=3:qg-size=8:cbqpoffs=-4:crqpoffs=5",
    ),
    fx(
        "intra_tools",
        SMALL,
        6,
        4,
        false,
        "",
        "constrained-intra:no-signhide:no-temporal-mvp:no-strong-intra-smoothing:max-merge=1:no-sao:deblock=-6,6:amp:rect:tu-intra-depth=4:tu-inter-depth=4",
    ),
    fx("cu_lossless", SMALL, 4, AMP, false, "", "cu-lossless"),
    fx(
        "one_ctb_no_deblock",
        (64, 64),
        4,
        AMP,
        false,
        "",
        "ctu=64:no-deblock",
    ),
    fx(
        "scaling_list_file",
        SMALL,
        6,
        AMP,
        false,
        "",
        "scaling-list=scaling_list.txt:rdoq-level=0",
    ),
];

pub const OPEN_GOP_CUT: X265 = fx(
    "open_gop_cut",
    SMALL,
    20,
    AMP,
    false,
    "",
    "keyint=8:min-keyint=8:open-gop:bframes=3:repeat-headers",
);

pub const HVCC: [&str; 2] = ["medium", "intra_main10"];

pub fn all() -> impl Iterator<Item = &'static X265> {
    CONFORMANCE
        .iter()
        .chain(FEATURES.iter())
        .chain(std::iter::once(&OPEN_GOP_CUT))
}

pub fn find(name: &str) -> Result<&'static X265, String> {
    all()
        .find(|f| f.name == name)
        .ok_or_else(|| format!("no x265 stream named {name}"))
}

pub fn stream(name: &str) -> String {
    format!("{name}.hevc")
}

pub fn recon(name: &str) -> String {
    format!("{name}.yuv")
}

pub fn source_bytes(f: &X265) -> Vec<u8> {
    let (w, h) = f.size;
    let mut out = Vec::new();
    for n in 0..f.frames {
        let frame = crate::testref::yuv_frame(w, h, n, f.amp);
        for (k, p) in frame.planes.iter().enumerate() {
            let black = if k == 0 { 16 } else { 128 };
            for y in 0..p.height {
                for x in 0..p.width {
                    let mut v = i64::from(p.data[y * p.stride + x]);
                    if n < f.fade {
                        v = black + (v - black) * n as i64 / f.fade as i64;
                    }
                    if f.ten_bit {
                        let low = ((x * 3 + y * 5 + n as usize + k) & 3) as i64;
                        out.extend_from_slice(&((v * 4 + low) as u16).to_le_bytes());
                    } else {
                        out.push(v as u8);
                    }
                }
            }
        }
    }
    out
}

pub fn source(name: &str) -> Result<Vec<u8>, String> {
    Ok(source_bytes(find(name)?))
}

pub fn x265_cli(params: &str) -> Vec<String> {
    let mut out = Vec::new();
    for kv in params.split(':').filter(|kv| !kv.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        out.push(format!("--{k}"));
        if !v.is_empty() {
            out.push(v.to_string());
        }
    }
    out
}

pub fn scaling_list_text() -> String {
    let sizes = [("4X4", 16), ("8X8", 64), ("16X16", 64), ("32X32", 64)];
    let lists = [
        "INTRA{}_LUMA",
        "INTRA{}_CHROMAU",
        "INTRA{}_CHROMAV",
        "INTER{}_LUMA",
        "INTER{}_CHROMAU",
        "INTER{}_CHROMAV",
    ];
    let mut text = String::new();
    for (s, (size, n)) in sizes.iter().enumerate() {
        for (l, list) in lists.iter().enumerate() {
            let name = list.replace("{}", size);
            let seed = if l == 5 { 4 } else { l };
            let coeffs: Vec<String> = (0..*n)
                .map(|i| (8 + s * 3 + seed * 2 + (i * (seed + 1)) % 23).to_string())
                .collect();
            text += &format!("{name} =\n{}\n", coeffs.join(","));
            if s >= 2 {
                text += &format!("{name}_DC =\n{}\n", 10 + s + seed);
            }
        }
    }
    text
}
