use super::bits::Bits;
use super::hdr::Head;
use super::tables::BITALLOC_CODE;

const MODE_MONO: u8 = 3;
const MODE_JOINT_STEREO: u8 = 1;

struct Alloc {
    tab_offset: u8,
    code_tab_width: u8,
    band_count: u8,
}

const fn a(tab_offset: u8, code_tab_width: u8, band_count: u8) -> Alloc {
    Alloc {
        tab_offset,
        code_tab_width,
        band_count,
    }
}

static ALLOC_L1: [Alloc; 1] = [a(76, 4, 32)];
static ALLOC_L2M2: [Alloc; 3] = [a(60, 4, 4), a(44, 3, 7), a(44, 2, 19)];
static ALLOC_L2M1: [Alloc; 4] = [a(0, 4, 3), a(16, 4, 8), a(32, 3, 12), a(40, 2, 7)];
static ALLOC_L2M1_LOWRATE: [Alloc; 2] = [a(44, 4, 2), a(44, 3, 10)];

const fn deq() -> [f32; 54] {
    let d = [
        3.0f32, 7.0, 15.0, 31.0, 63.0, 127.0, 255.0, 511.0, 1023.0, 2047.0, 4095.0, 8191.0,
        16383.0, 32767.0, 65535.0, 3.0, 5.0, 9.0,
    ];
    let mut t = [0.0f32; 54];
    let mut i = 0;
    while i < 18 {
        t[3 * i] = 9.53674316e-07 / d[i];
        t[3 * i + 1] = 7.56931807e-07 / d[i];
        t[3 * i + 2] = 6.00777173e-07 / d[i];
        i += 1;
    }
    t
}

static DEQ_L12: [f32; 54] = deq();

pub struct ScaleInfo {
    scf: [f32; 192],
    total_bands: usize,
    stereo_bands: usize,
    bitalloc: [u8; 64],
    scfcod: [u8; 64],
}

impl Default for ScaleInfo {
    fn default() -> Self {
        ScaleInfo {
            scf: [0.0; 192],
            total_bands: 0,
            stereo_bands: 0,
            bitalloc: [0; 64],
            scfcod: [0; 64],
        }
    }
}

fn alloc_table(h: Head, sci: &mut ScaleInfo) -> &'static [Alloc] {
    let mode = h.stereo_mode();
    let stereo_bands = if mode == MODE_MONO {
        0
    } else if mode == MODE_JOINT_STEREO {
        ((h.stereo_mode_ext() as usize) << 2) + 4
    } else {
        32
    };
    let (alloc, nbands): (&'static [Alloc], usize) = if h.layer_1() {
        (&ALLOC_L1, 32)
    } else if !h.mpeg1() {
        (&ALLOC_L2M2, 30)
    } else {
        let sample_rate_idx = h.sample_rate_index();
        let mut kbps = h.bitrate_kbps() >> (mode != MODE_MONO) as u32;
        if kbps == 0 {
            kbps = 192;
        }
        if kbps < 56 {
            (
                &ALLOC_L2M1_LOWRATE,
                if sample_rate_idx == 2 { 12 } else { 8 },
            )
        } else if kbps >= 96 && sample_rate_idx != 1 {
            (&ALLOC_L2M1, 30)
        } else {
            (&ALLOC_L2M1, 27)
        }
    };
    sci.total_bands = nbands;
    sci.stereo_bands = stereo_bands.min(nbands);
    alloc
}

fn read_scalefactors(bs: &mut Bits, sci: &mut ScaleInfo, bands: usize) {
    let mut o = 0;
    for i in 0..bands {
        let mut s = 0.0f32;
        let ba = sci.bitalloc[i] as usize;
        let mask = if ba != 0 {
            4 + ((19 >> sci.scfcod[i]) & 3)
        } else {
            0
        };
        let mut m = 4;
        while m != 0 {
            if mask & m != 0 {
                let b = bs.get(6) as usize;
                let d = DEQ_L12
                    .get((ba * 3 + b % 3).wrapping_sub(6))
                    .copied()
                    .unwrap_or(0.0);
                s = d * ((1i32 << 21) >> (b / 3)) as f32;
            }
            sci.scf[o] = s;
            o += 1;
            m >>= 1;
        }
    }
}

pub fn read_scale_info(h: Head, bs: &mut Bits, sci: &mut ScaleInfo) {
    let alloc = alloc_table(h, sci);
    let mut next = alloc.iter();
    let mut k = 0;
    let mut ba_bits = 0;
    let mut code: &[u8] = &BITALLOC_CODE;
    for i in 0..sci.total_bands {
        if i == k
            && let Some(a) = next.next()
        {
            k += a.band_count as usize;
            ba_bits = a.code_tab_width as u32;
            code = BITALLOC_CODE.get(a.tab_offset as usize..).unwrap_or(&[]);
        }
        let mut ba = code.get(bs.get(ba_bits) as usize).copied().unwrap_or(0);
        sci.bitalloc[2 * i] = ba;
        if i < sci.stereo_bands {
            ba = code.get(bs.get(ba_bits) as usize).copied().unwrap_or(0);
        }
        sci.bitalloc[2 * i + 1] = if sci.stereo_bands != 0 { ba } else { 0 };
    }
    for i in 0..2 * sci.total_bands {
        sci.scfcod[i] = if sci.bitalloc[i] != 0 {
            if h.layer_1() { 2 } else { bs.get(2) as u8 }
        } else {
            6
        };
    }
    read_scalefactors(bs, sci, sci.total_bands * 2);
    for i in sci.stereo_bands..sci.total_bands {
        sci.bitalloc[2 * i + 1] = 0;
    }
}

pub fn dequantize_granule(
    g: &mut [f32],
    at: usize,
    bs: &mut Bits,
    sci: &ScaleInfo,
    group_size: usize,
) -> usize {
    for j in 0..4 {
        let mut dst = at + group_size * j;
        for i in 0..2 * sci.total_bands {
            let ba = sci.bitalloc[i] as u32;
            if ba != 0 {
                let out = &mut g[dst..dst + group_size];
                if ba < 17 {
                    let half = (1i32 << (ba - 1)) - 1;
                    for o in out.iter_mut() {
                        *o = (bs.get(ba) as i32 - half) as f32;
                    }
                } else {
                    let m = (2u32 << (ba - 17)) + 1;
                    let mut code = bs.get(m + 2 - (m >> 3));
                    for o in out.iter_mut() {
                        *o = ((code % m) as i32 - (m / 2) as i32) as f32;
                        code /= m;
                    }
                }
            }
            if i % 2 == 0 {
                dst += 576;
            } else {
                dst -= 558;
            }
        }
    }
    group_size * 4
}

pub fn apply_scf_384(sci: &ScaleInfo, scf: usize, g: &mut [f32]) {
    let sb = sci.stereo_bands * 18;
    let n = (sci.total_bands - sci.stereo_bands) * 18;
    g.copy_within(sb..sb + n, 576 + sb);
    for i in 0..sci.total_bands {
        let d = 18 * i;
        let s = scf + 6 * i;
        let (l, r) = (sci.scf[s], sci.scf[s + 3]);
        for k in 0..12 {
            g[d + k] *= l;
            g[d + k + 576] *= r;
        }
    }
}
