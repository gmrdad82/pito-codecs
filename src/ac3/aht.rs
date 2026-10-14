use super::aht_tables::{
    VQ_HEBAP1, VQ_HEBAP2, VQ_HEBAP3, VQ_HEBAP4, VQ_HEBAP5, VQ_HEBAP6, VQ_HEBAP7,
};
use super::bits::Bits;
use super::block::{Chan, N};
use super::tables::{BNDSZ, BNDTAB, EXP2, FLOORTAB, MASKTAB};
use crate::media::Result;

pub const BLOCKS: usize = 6;

const HEBAPTAB: [u8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 8, 8, 9, 9, 9, 10, 10, 10, 10, 11, 11, 11, 11, 12, 12, 12, 12,
    13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15, 16, 16, 16, 16, 17, 17, 17, 17, 18, 18, 18, 18,
    18, 18, 18, 18, 19, 19, 19, 19, 19, 19, 19, 19, 19,
];

const MANT_BITS: [u8; 20] = [
    0, 0, 0, 0, 0, 0, 0, 0, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16,
];

const VQ_BITS: [u8; 8] = [0, 2, 3, 4, 5, 7, 8, 9];

const REMAP: [(i16, [i16; 3], [i16; 3]); 12] = [
    (
        0x1249,
        [0xd555u16 as i16, 0x4000, 0xeaabu16 as i16],
        [0xedb7u16 as i16, 0x2000, 0xfb6eu16 as i16],
    ),
    (
        0x0889,
        [0xc925u16 as i16, 0x4000, 0xd249u16 as i16],
        [0xe666u16 as i16, 0x2000, 0xeccdu16 as i16],
    ),
    (
        0x0421,
        [0xc444u16 as i16, 0x4000, 0xc889u16 as i16],
        [0xe319u16 as i16, 0x2000, 0xe632u16 as i16],
    ),
    (
        0x0208,
        [0xc211u16 as i16, 0x4000, 0xc421u16 as i16],
        [0xe186u16 as i16, 0x2000, 0xe30cu16 as i16],
    ),
    (
        0x0102,
        [0xc104u16 as i16, 0x4000, 0xc208u16 as i16],
        [0xe0c2u16 as i16, 0x2000, 0xe183u16 as i16],
    ),
    (
        0x0081,
        [0xc081u16 as i16, 0x4000, 0xc102u16 as i16],
        [0xe060u16 as i16, 0x2000, 0xe0c1u16 as i16],
    ),
    (
        0x0040,
        [0xc040u16 as i16, 0x4000, 0xc081u16 as i16],
        [0xe030u16 as i16, 0x2000, 0xe060u16 as i16],
    ),
    (
        0x0020,
        [0xc020u16 as i16, 0x4000, 0xc040u16 as i16],
        [0xe018u16 as i16, 0x2000, 0xe030u16 as i16],
    ),
    (
        0x0010,
        [0xc010u16 as i16, 0x4000, 0xc020u16 as i16],
        [0xe00cu16 as i16, 0x2000, 0xe018u16 as i16],
    ),
    (0x0008, [0, 0, 0], [0, 0, 0]),
    (0x0002, [0, 0, 0], [0, 0, 0]),
    (0x0000, [0, 0, 0], [0, 0, 0]),
];

fn q15(v: i16) -> f32 {
    v as f32 / 32768.0
}

fn remap(hebap: u8, gk: u8, x: f32) -> f32 {
    let row = &REMAP[(hebap as usize).saturating_sub(8).min(11)];
    let (a, b) = match gk {
        1 => (row.0, 0),
        2 => (row.1[0], if x >= 0.0 { row.1[1] } else { row.1[2] }),
        _ => (row.2[0], if x >= 0.0 { row.2[1] } else { row.2[2] }),
    };
    x + q15(a) * x + q15(b)
}

fn vq(hebap: u8, index: usize) -> [f32; 6] {
    let raw: &[i16; 6] = match hebap {
        1 => &VQ_HEBAP1[index & 3],
        2 => &VQ_HEBAP2[index & 7],
        3 => &VQ_HEBAP3[index & 15],
        4 => &VQ_HEBAP4[index & 31],
        5 => &VQ_HEBAP5[index & 127],
        6 => &VQ_HEBAP6[index & 255],
        _ => &VQ_HEBAP7[index & 511],
    };
    raw.map(q15)
}

fn scalar(b: &mut Bits, hebap: u8, gk: u8, out: &mut [f32; 6]) -> Result<()> {
    let m = MANT_BITS[hebap as usize & 31] as u32;
    if m == 0 {
        *out = [0.0; 6];
        return Ok(());
    }
    let full = EXP2[m as usize - 1];
    for v in out.iter_mut() {
        *v = match gk {
            1 => remap(hebap, 1, b.signed(m)? as f32 * full),
            2 => {
                let small = b.signed(m - 1)?;
                if small == -(1 << (m - 2)) {
                    remap(hebap, 2, b.signed(m - 1)? as f32 * EXP2[m as usize - 2])
                } else {
                    small as f32 * full
                }
            }
            _ => {
                let small = b.signed(m - 2)?;
                if small == -(1 << (m - 3)) {
                    remap(hebap, 4, b.signed(m)? as f32 * full)
                } else {
                    small as f32 * full
                }
            }
        };
    }
    Ok(())
}

fn idct6(x: [f32; 6]) -> [f32; 6] {
    static COS: std::sync::LazyLock<[[f32; 6]; 6]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|m| {
            std::array::from_fn(|j| {
                let r = if j == 0 {
                    std::f64::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let t = j as f64 * (2 * m + 1) as f64 * std::f64::consts::PI / 12.0;
                (std::f64::consts::SQRT_2 * r * t.cos()) as f32
            })
        })
    });
    let c = &*COS;
    std::array::from_fn(|m| (0..6).map(|j| c[m][j] * x[j]).sum())
}

pub fn decode(
    b: &mut Bits,
    c: &Chan,
    start: usize,
    end: usize,
    snroffset: i32,
    floorcod: u8,
    cache: &mut [[f32; N]; BLOCKS],
) -> Result<()> {
    let mut hebap = [0u8; N];
    if start < end {
        let floor = FLOORTAB[floorcod as usize];
        let mut i = start;
        let mut j = MASKTAB[start] as usize;
        while i < end {
            let last = (BNDTAB[j] as usize + BNDSZ[j] as usize).min(end);
            let m = ((c.mask[j] as i32 - snroffset - floor).max(0) & 0x1fe0) + floor;
            while i < last {
                let addr = ((c.psd[i] as i32 - m) >> 5).clamp(0, 63) as usize;
                hebap[i] = HEBAPTAB[addr];
                i += 1;
            }
            j += 1;
        }
    }
    let gaqmod = b.read(2)? as u8;
    let endbap = if gaqmod < 2 { 12 } else { 17 };
    let mut gaqbin = [0i8; N];
    let mut active = 0;
    for bin in start..end {
        let h = hebap[bin];
        gaqbin[bin] = if h > 7 && h < endbap {
            active += 1;
            1
        } else if h >= endbap {
            -1
        } else {
            0
        };
    }
    let mut gains = [1u8; N];
    let mut ng = 0;
    match gaqmod {
        1 | 2 => {
            let big = if gaqmod == 1 { 2 } else { 4 };
            for g in gains.iter_mut().take(active) {
                *g = if b.flag()? { big } else { 1 };
            }
        }
        3 => {
            for _ in 0..active.div_ceil(3) {
                let grp = b.read(5)?;
                for code in [grp / 9, (grp % 9) / 3, grp % 3] {
                    if ng < active {
                        gains[ng] = [1, 2, 4][code.min(2) as usize];
                        ng += 1;
                    }
                }
            }
        }
        _ => {}
    }
    let mut next = 0;
    let mut x = [0.0f32; 6];
    for bin in start..end {
        let h = hebap[bin];
        if h == 0 {
            for blk in cache.iter_mut() {
                blk[bin] = 0.0;
            }
            continue;
        }
        if h <= 7 {
            x = vq(h, b.read(VQ_BITS[h as usize] as u32)? as usize);
        } else {
            let gk = if gaqbin[bin] == 1 && gaqmod != 0 {
                let g = gains.get(next).copied().unwrap_or(1);
                next += 1;
                g
            } else {
                1
            };
            scalar(b, h, gk, &mut x)?;
        }
        let y = idct6(x);
        let scale = EXP2[c.exp[bin] as usize];
        for (blk, v) in cache.iter_mut().zip(y) {
            blk[bin] = v * scale;
        }
    }
    Ok(())
}
