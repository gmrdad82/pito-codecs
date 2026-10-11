pub const BPS: usize = 32;

pub const V_PRED: u8 = 1;
pub const H_PRED: u8 = 2;
pub const TM_PRED: u8 = 3;
pub const B_PRED: u8 = 4;

pub const B_DC: u8 = 0;
pub const B_TM: u8 = 1;
pub const B_VE: u8 = 2;
pub const B_HE: u8 = 3;
pub const B_LD: u8 = 4;
pub const B_RD: u8 = 5;
pub const B_VR: u8 = 6;
pub const B_VL: u8 = 7;
pub const B_HD: u8 = 8;

#[inline(always)]
fn mul1(a: i32) -> i32 {
    (a.wrapping_mul(20091) >> 16).wrapping_add(a)
}

#[inline(always)]
fn mul2(a: i32) -> i32 {
    a.wrapping_mul(35468) >> 16
}

#[inline(always)]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline(always)]
pub fn idct_add(c: &[i16], dst: &mut [u8], off: usize) {
    let c = &c[..16];
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a = (c[i] as i32).wrapping_add(c[8 + i] as i32);
        let b = (c[i] as i32).wrapping_sub(c[8 + i] as i32);
        let x = mul2(c[4 + i] as i32).wrapping_sub(mul1(c[12 + i] as i32));
        let y = mul1(c[4 + i] as i32).wrapping_add(mul2(c[12 + i] as i32));
        t[i] = a.wrapping_add(y);
        t[4 + i] = b.wrapping_add(x);
        t[8 + i] = b.wrapping_sub(x);
        t[12 + i] = a.wrapping_sub(y);
    }
    for r in 0..4 {
        let s = &t[r * 4..r * 4 + 4];
        let dc = s[0].wrapping_add(4);
        let a = dc.wrapping_add(s[2]);
        let b = dc.wrapping_sub(s[2]);
        let x = mul2(s[1]).wrapping_sub(mul1(s[3]));
        let y = mul1(s[1]).wrapping_add(mul2(s[3]));
        let d = &mut dst[off + r * BPS..off + r * BPS + 4];
        d[0] = clip8(d[0] as i32 + (a.wrapping_add(y) >> 3));
        d[1] = clip8(d[1] as i32 + (b.wrapping_add(x) >> 3));
        d[2] = clip8(d[2] as i32 + (b.wrapping_sub(x) >> 3));
        d[3] = clip8(d[3] as i32 + (a.wrapping_sub(y) >> 3));
    }
}

#[inline(always)]
pub fn dc_add(c: i16, dst: &mut [u8], off: usize) {
    let dc = (c as i32 + 4) >> 3;
    for r in 0..4 {
        for p in &mut dst[off + r * BPS..off + r * BPS + 4] {
            *p = clip8(*p as i32 + dc);
        }
    }
}

pub fn iwht(input: &[i16; 16], out: &mut [i16]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a = input[i] as i32 + input[12 + i] as i32;
        let b = input[4 + i] as i32 + input[8 + i] as i32;
        let c = input[4 + i] as i32 - input[8 + i] as i32;
        let d = input[i] as i32 - input[12 + i] as i32;
        t[i] = a + b;
        t[4 + i] = c + d;
        t[8 + i] = a - b;
        t[12 + i] = d - c;
    }
    for r in 0..4 {
        let s = &t[r * 4..r * 4 + 4];
        let a = s[0] + s[3];
        let b = s[1] + s[2];
        let c = s[1] - s[2];
        let d = s[0] - s[3];
        out[(r * 4) * 16] = ((a + b + 3) >> 3) as i16;
        out[(r * 4 + 1) * 16] = ((c + d + 3) >> 3) as i16;
        out[(r * 4 + 2) * 16] = ((a - b + 3) >> 3) as i16;
        out[(r * 4 + 3) * 16] = ((d - c + 3) >> 3) as i16;
    }
}

fn fill(dst: &mut [u8], off: usize, n: usize, v: u8) {
    for r in 0..n {
        dst[off + r * BPS..off + r * BPS + n].fill(v);
    }
}

pub fn pred_dc(dst: &mut [u8], off: usize, n: usize, top: bool, left: bool) {
    let shift = n.trailing_zeros();
    let mut s = 0u32;
    if top {
        s += dst[off - BPS..off - BPS + n]
            .iter()
            .map(|&v| v as u32)
            .sum::<u32>();
    }
    if left {
        s += (0..n).map(|r| dst[off + r * BPS - 1] as u32).sum::<u32>();
    }
    let v = match (top, left) {
        (true, true) => (s + n as u32) >> (shift + 1),
        (true, false) | (false, true) => (s + (n as u32 >> 1)) >> shift,
        (false, false) => 128,
    };
    fill(dst, off, n, v as u8);
}

pub fn pred_v(dst: &mut [u8], off: usize, n: usize) {
    let mut top = [0u8; 16];
    top[..n].copy_from_slice(&dst[off - BPS..off - BPS + n]);
    for r in 0..n {
        dst[off + r * BPS..off + r * BPS + n].copy_from_slice(&top[..n]);
    }
}

pub fn pred_h(dst: &mut [u8], off: usize, n: usize) {
    for r in 0..n {
        let v = dst[off + r * BPS - 1];
        dst[off + r * BPS..off + r * BPS + n].fill(v);
    }
}

pub fn pred_tm(dst: &mut [u8], off: usize, n: usize) {
    let mut top = [0i32; 16];
    let tl = dst[off - BPS - 1] as i32;
    for (i, t) in top[..n].iter_mut().enumerate() {
        *t = dst[off - BPS + i] as i32 - tl;
    }
    for r in 0..n {
        let l = dst[off + r * BPS - 1] as i32;
        for (p, t) in dst[off + r * BPS..off + r * BPS + n]
            .iter_mut()
            .zip(&top[..n])
        {
            *p = clip8(l + t);
        }
    }
}

pub fn pred_luma(mode: u8, dst: &mut [u8], off: usize, top: bool, left: bool) {
    match mode {
        V_PRED => pred_v(dst, off, 16),
        H_PRED => pred_h(dst, off, 16),
        TM_PRED => pred_tm(dst, off, 16),
        _ => pred_dc(dst, off, 16, top, left),
    }
}

pub fn pred_chroma(mode: u8, dst: &mut [u8], off: usize, top: bool, left: bool) {
    match mode {
        V_PRED => pred_v(dst, off, 8),
        H_PRED => pred_h(dst, off, 8),
        TM_PRED => pred_tm(dst, off, 8),
        _ => pred_dc(dst, off, 8, top, left),
    }
}

#[inline(always)]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((a as u32 + 2 * b as u32 + c as u32 + 2) >> 2) as u8
}

#[inline(always)]
fn avg2(a: u8, b: u8) -> u8 {
    ((a as u32 + b as u32 + 1) >> 1) as u8
}

pub fn pred4(mode: u8, d: &mut [u8], off: usize) {
    let t = off - BPS;
    let a = [
        d[t],
        d[t + 1],
        d[t + 2],
        d[t + 3],
        d[t + 4],
        d[t + 5],
        d[t + 6],
        d[t + 7],
    ];
    let tl = d[t - 1];
    let l = [
        d[off - 1],
        d[off + BPS - 1],
        d[off + 2 * BPS - 1],
        d[off + 3 * BPS - 1],
    ];
    let mut o = [[0u8; 4]; 4];
    match mode {
        B_DC => {
            let s: u32 = a[..4].iter().chain(&l).map(|&v| v as u32).sum();
            o = [[((s + 4) >> 3) as u8; 4]; 4];
        }
        B_TM => {
            for (r, row) in o.iter_mut().enumerate() {
                for (c, p) in row.iter_mut().enumerate() {
                    *p = clip8(l[r] as i32 + a[c] as i32 - tl as i32);
                }
            }
        }
        B_VE => {
            let v = [
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
            ];
            o = [v; 4];
        }
        B_HE => {
            o[0] = [avg3(tl, l[0], l[1]); 4];
            o[1] = [avg3(l[0], l[1], l[2]); 4];
            o[2] = [avg3(l[1], l[2], l[3]); 4];
            o[3] = [avg3(l[2], l[3], l[3]); 4];
        }
        B_LD => {
            let p = [
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
                avg3(a[3], a[4], a[5]),
                avg3(a[4], a[5], a[6]),
                avg3(a[5], a[6], a[7]),
                avg3(a[6], a[7], a[7]),
            ];
            for (r, row) in o.iter_mut().enumerate() {
                row.copy_from_slice(&p[r..r + 4]);
            }
        }
        B_RD => {
            let e = [l[3], l[2], l[1], l[0], tl, a[0], a[1], a[2], a[3]];
            let mut p = [0u8; 7];
            for (i, v) in p.iter_mut().enumerate() {
                *v = avg3(e[i], e[i + 1], e[i + 2]);
            }
            for (r, row) in o.iter_mut().enumerate() {
                row.copy_from_slice(&p[3 - r..7 - r]);
            }
        }
        B_VR => {
            o[0] = [
                avg2(tl, a[0]),
                avg2(a[0], a[1]),
                avg2(a[1], a[2]),
                avg2(a[2], a[3]),
            ];
            o[1] = [
                avg3(l[0], tl, a[0]),
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
            ];
            o[2] = [avg3(l[1], l[0], tl), o[0][0], o[0][1], o[0][2]];
            o[3] = [avg3(l[2], l[1], l[0]), o[1][0], o[1][1], o[1][2]];
        }
        B_VL => {
            o[0] = [
                avg2(a[0], a[1]),
                avg2(a[1], a[2]),
                avg2(a[2], a[3]),
                avg2(a[3], a[4]),
            ];
            o[1] = [
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
                avg3(a[3], a[4], a[5]),
            ];
            o[2] = [o[0][1], o[0][2], o[0][3], avg3(a[4], a[5], a[6])];
            o[3] = [o[1][1], o[1][2], o[1][3], avg3(a[5], a[6], a[7])];
        }
        B_HD => {
            o[0] = [
                avg2(l[0], tl),
                avg3(l[0], tl, a[0]),
                avg3(tl, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
            ];
            o[1] = [avg2(l[1], l[0]), avg3(l[1], l[0], tl), o[0][0], o[0][1]];
            o[2] = [avg2(l[2], l[1]), avg3(l[2], l[1], l[0]), o[1][0], o[1][1]];
            o[3] = [avg2(l[3], l[2]), avg3(l[3], l[2], l[1]), o[2][0], o[2][1]];
        }
        _ => {
            o[0] = [
                avg2(l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg2(l[1], l[2]),
                avg3(l[1], l[2], l[3]),
            ];
            o[1] = [o[0][2], o[0][3], avg2(l[2], l[3]), avg3(l[2], l[3], l[3])];
            o[2] = [o[1][2], o[1][3], l[3], l[3]];
            o[3] = [l[3]; 4];
        }
    }
    for (r, row) in o.iter().enumerate() {
        d[off + r * BPS..off + r * BPS + 4].copy_from_slice(row);
    }
}
