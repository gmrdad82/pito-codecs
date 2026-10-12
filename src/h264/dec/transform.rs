use crate::h264::dec::tables::{norm_adjust4, norm_adjust8};
use crate::syntax::h264::params::ScalingMatrices;

#[derive(Clone)]
pub struct LevelScale {
    pub ls4: [[[i32; 16]; 6]; 6],
    pub ls8: [[[i32; 64]; 6]; 6],
}

impl LevelScale {
    pub fn new(m: &ScalingMatrices) -> Box<Self> {
        let mut s = Box::new(LevelScale {
            ls4: [[[0; 16]; 6]; 6],
            ls8: [[[0; 64]; 6]; 6],
        });
        for l in 0..6 {
            for q in 0..6 {
                for p in 0..16 {
                    s.ls4[l][q][p] = m.m4[l][p] as i32 * norm_adjust4(q, p) as i32;
                }
                for p in 0..64 {
                    s.ls8[l][q][p] = m.m8[l][p] as i32 * norm_adjust8(q, p) as i32;
                }
            }
        }
        s
    }
}

#[inline(always)]
pub fn scale4(c: i32, ls: i32, qp: i32) -> i32 {
    if qp >= 24 {
        c.wrapping_mul(ls) << (qp / 6 - 4)
    } else {
        c.wrapping_mul(ls).wrapping_add(1 << (3 - qp / 6)) >> (4 - qp / 6)
    }
}

#[inline(always)]
pub fn scale8(c: i32, ls: i32, qp: i32) -> i32 {
    if qp >= 36 {
        c.wrapping_mul(ls) << (qp / 6 - 6)
    } else {
        c.wrapping_mul(ls).wrapping_add(1 << (5 - qp / 6)) >> (6 - qp / 6)
    }
}

pub fn luma_dc_dequant(c: &mut [i32; 16], qp: i32, ls00: i32) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let r = &c[i * 4..i * 4 + 4];
        let s01 = r[0] + r[1];
        let d01 = r[0] - r[1];
        let s23 = r[2] + r[3];
        let d23 = r[2] - r[3];
        t[i * 4] = s01 + s23;
        t[i * 4 + 1] = s01 - s23;
        t[i * 4 + 2] = d01 - d23;
        t[i * 4 + 3] = d01 + d23;
    }
    for j in 0..4 {
        let s01 = t[j] + t[4 + j];
        let d01 = t[j] - t[4 + j];
        let s23 = t[8 + j] + t[12 + j];
        let d23 = t[8 + j] - t[12 + j];
        let f = [s01 + s23, s01 - s23, d01 - d23, d01 + d23];
        for i in 0..4 {
            c[i * 4 + j] = if qp >= 36 {
                f[i].wrapping_mul(ls00) << (qp / 6 - 6)
            } else {
                f[i].wrapping_mul(ls00).wrapping_add(1 << (5 - qp / 6)) >> (6 - qp / 6)
            };
        }
    }
}

pub fn chroma_dc_dequant_420(c: &mut [i32; 4], qp: i32, ls00: i32) {
    let s0 = c[0].wrapping_add(c[1]);
    let d0 = c[0].wrapping_sub(c[1]);
    let s1 = c[2].wrapping_add(c[3]);
    let d1 = c[2].wrapping_sub(c[3]);
    let f = [
        s0.wrapping_add(s1),
        d0.wrapping_add(d1),
        s0.wrapping_sub(s1),
        d0.wrapping_sub(d1),
    ];
    for k in 0..4 {
        c[k] = (f[k].wrapping_mul(ls00) << (qp / 6)) >> 5;
    }
}

#[inline(always)]
fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub fn idct4_add(d: &[i32; 16], dst: &mut [u8], stride: usize) {
    let mut t = [[0i32; 4]; 4];
    for (i, row) in t.iter_mut().enumerate() {
        let r = &d[i * 4..i * 4 + 4];
        let e = r[0] + r[2];
        let f = r[0] - r[2];
        let g = (r[1] >> 1) - r[3];
        let h = r[1] + (r[3] >> 1);
        *row = [e + h, f + g, f - g, e - h];
    }
    let mut o = [[0i32; 4]; 4];
    for j in 0..4 {
        let e = t[0][j] + t[2][j];
        let f = t[0][j] - t[2][j];
        let g = (t[1][j] >> 1) - t[3][j];
        let h = t[1][j] + (t[3][j] >> 1);
        o[0][j] = e + h;
        o[1][j] = f + g;
        o[2][j] = f - g;
        o[3][j] = e - h;
    }
    for (i, row) in o.iter().enumerate() {
        let Some(line) = dst
            .get_mut(i * stride..)
            .and_then(|d| d.first_chunk_mut::<4>())
        else {
            return;
        };
        for (p, &v) in line.iter_mut().zip(row) {
            *p = clip_u8(*p as i32 + ((v + 32) >> 6));
        }
    }
}

#[inline(always)]
pub(in crate::h264) fn idct8_1d(d: [i32; 8]) -> [i32; 8] {
    let a0 = d[0] + d[4];
    let a4 = d[0] - d[4];
    let a2 = (d[2] >> 1) - d[6];
    let a6 = d[2] + (d[6] >> 1);
    let b0 = a0 + a6;
    let b2 = a4 + a2;
    let b4 = a4 - a2;
    let b6 = a0 - a6;
    let a1 = -d[3] + d[5] - d[7] - (d[7] >> 1);
    let a3 = d[1] + d[7] - d[3] - (d[3] >> 1);
    let a5 = -d[1] + d[7] + d[5] + (d[5] >> 1);
    let a7 = d[3] + d[5] + d[1] + (d[1] >> 1);
    let b1 = a1 + (a7 >> 2);
    let b7 = a7 - (a1 >> 2);
    let b3 = a3 + (a5 >> 2);
    let b5 = (a3 >> 2) - a5;
    [
        b0 + b7,
        b2 + b5,
        b4 + b3,
        b6 + b1,
        b6 - b1,
        b4 - b3,
        b2 - b5,
        b0 - b7,
    ]
}

pub fn idct8_add(d: &[i32; 64], dst: &mut [u8], stride: usize) {
    let mut t = [[0i32; 8]; 8];
    for (row, src) in t.iter_mut().zip(d.as_chunks::<8>().0) {
        *row = idct8_1d(*src);
    }
    let mut o = [[0i32; 8]; 8];
    for j in 0..8 {
        let col = idct8_1d(std::array::from_fn(|i| t[i][j]));
        for i in 0..8 {
            o[i][j] = col[i];
        }
    }
    for (i, row) in o.iter().enumerate() {
        let Some(line) = dst
            .get_mut(i * stride..)
            .and_then(|d| d.first_chunk_mut::<8>())
        else {
            return;
        };
        for (p, &v) in line.iter_mut().zip(row) {
            *p = clip_u8(*p as i32 + ((v + 32) >> 6));
        }
    }
}
