use super::lane::{Isa, Vector};
use super::net::*;

pub trait Net<V: Lane, const N: usize> {
    fn run(x: [V; N]) -> [V; N];
}

pub struct Fdct;
pub struct Fadst;
pub struct Idct;
pub struct Iadst;

macro_rules! nets {
    ($($t:ident $n:literal $f:ident),* $(,)?) => {
        $(impl<V: Lane> Net<V, $n> for $t {
            #[inline(always)]
            fn run(x: [V; $n]) -> [V; $n] {
                $f(x)
            }
        })*
    };
}

nets!(
    Fdct 4 fdct4, Fdct 8 fdct8, Fdct 16 fdct16, Fdct 32 fdct32,
    Fadst 4 fadst4, Fadst 8 fadst8, Fadst 16 fadst16,
    Idct 4 idct4, Idct 8 idct8, Idct 16 idct16, Idct 32 idct32,
    Iadst 4 iadst4, Iadst 8 iadst8, Iadst 16 iadst16,
);

#[inline(always)]
fn pass<V: Vector, T: Net<V, N>, const N: usize, const PRE: i32, const POST: i32>(
    buf: &mut [i16],
    groups: usize,
) {
    for g in 0..groups {
        let j = g * V::LANES;
        let mut x = [V::zero(); N];
        for (i, v) in x.iter_mut().enumerate() {
            let l = V::load(&buf[i * N + j..]);
            *v = if PRE > 0 { l.shl::<PRE>() } else { l };
        }
        for (i, v) in T::run(x).into_iter().enumerate() {
            let v = if POST > 0 { v.rshr::<POST>() } else { v };
            v.store(&mut buf[i * N + j..]);
        }
    }
}

#[inline(always)]
fn pass_add<V: Vector, T: Net<V, N>, const N: usize, const SHIFT: i32>(
    buf: &[i16],
    dst: &mut [u8],
    stride: usize,
) {
    for g in 0..N / V::LANES {
        let j = g * V::LANES;
        let mut x = [V::zero(); N];
        for (i, v) in x.iter_mut().enumerate() {
            *v = V::load(&buf[i * N + j..]);
        }
        for (i, v) in T::run(x).into_iter().enumerate() {
            v.rshr::<SHIFT>().add_to(&mut dst[i * stride + j..]);
        }
    }
}

#[inline(always)]
pub fn fwd_n<
    V: Vector,
    I: Isa,
    C: Net<V, N>,
    R: Net<V, N>,
    const N: usize,
    const PRE: i32,
    const MID: i32,
>(
    buf: &mut [i16],
) {
    let g = N / V::LANES;
    pass::<V, C, N, PRE, MID>(buf, g);
    I::transpose(buf, N);
    pass::<V, R, N, 0, 0>(buf, g);
}

#[inline(always)]
pub fn inv_n<V: Vector, I: Isa, R: Net<V, N>, C: Net<V, N>, const N: usize, const SHIFT: i32>(
    buf: &mut [i16],
    rows: usize,
    dst: &mut [u8],
    stride: usize,
) {
    pass::<V, R, N, 0, 0>(buf, rows.div_ceil(V::LANES));
    I::transpose(buf, N);
    pass_add::<V, C, N, SHIFT>(buf, dst, stride);
}

#[inline(always)]
pub fn forward<I: Isa>(buf: &mut [i16], tx_size: u8, tx_type: u8) {
    I::forward(buf, tx_size, tx_type)
}

#[inline(always)]
pub fn inverse_add<I: Isa>(
    buf: &mut [i16],
    tx_size: u8,
    tx_type: u8,
    rows: usize,
    dst: &mut [u8],
    stride: usize,
) {
    I::inverse_add(buf, tx_size, tx_type, rows, dst, stride)
}

macro_rules! kernels {
    ($isa:ty, $v4:ty, $v8:ty, $v16:ty, #[$attr:meta]) => {
        fn forward(buf: &mut [i16], tx_size: u8, tx_type: u8) {
            use $crate::vp9::enc::tx::xform::{Fadst, Fdct, fwd_n};
            macro_rules! k {
                ($name:ident, $v:ty, $c:ty, $r:ty, $n:literal, $pre:literal, $mid:literal) => {
                    #[$attr]
                    unsafe fn $name(buf: &mut [i16]) {
                        fwd_n::<$v, $isa, $c, $r, $n, $pre, $mid>(buf)
                    }
                };
            }
            k!(f4dd, $v4, Fdct, Fdct, 4, 2, 0);
            k!(f4ad, $v4, Fadst, Fdct, 4, 2, 0);
            k!(f4da, $v4, Fdct, Fadst, 4, 2, 0);
            k!(f4aa, $v4, Fadst, Fadst, 4, 2, 0);
            k!(f8dd, $v8, Fdct, Fdct, 8, 2, 1);
            k!(f8ad, $v8, Fadst, Fdct, 8, 2, 1);
            k!(f8da, $v8, Fdct, Fadst, 8, 2, 1);
            k!(f8aa, $v8, Fadst, Fadst, 8, 2, 1);
            k!(f16dd, $v16, Fdct, Fdct, 16, 2, 2);
            k!(f16ad, $v16, Fadst, Fdct, 16, 2, 2);
            k!(f16da, $v16, Fdct, Fadst, 16, 2, 2);
            k!(f16aa, $v16, Fadst, Fadst, 16, 2, 2);
            k!(f32dd, $v16, Fdct, Fdct, 32, 2, 4);
            unsafe {
                match (tx_size, tx_type & 3) {
                    (0, 0) => f4dd(buf),
                    (0, 1) => f4ad(buf),
                    (0, 2) => f4da(buf),
                    (0, _) => f4aa(buf),
                    (1, 0) => f8dd(buf),
                    (1, 1) => f8ad(buf),
                    (1, 2) => f8da(buf),
                    (1, _) => f8aa(buf),
                    (2, 0) => f16dd(buf),
                    (2, 1) => f16ad(buf),
                    (2, 2) => f16da(buf),
                    (2, _) => f16aa(buf),
                    _ => f32dd(buf),
                }
            }
        }

        fn inverse_add(
            buf: &mut [i16],
            tx_size: u8,
            tx_type: u8,
            rows: usize,
            dst: &mut [u8],
            stride: usize,
        ) {
            use $crate::vp9::enc::tx::xform::{Iadst, Idct, inv_n};
            macro_rules! k {
                ($name:ident, $v:ty, $r:ty, $c:ty, $n:literal, $shift:literal) => {
                    #[$attr]
                    unsafe fn $name(buf: &mut [i16], rows: usize, dst: &mut [u8], stride: usize) {
                        inv_n::<$v, $isa, $r, $c, $n, $shift>(buf, rows, dst, stride)
                    }
                };
            }
            k!(i4dd, $v4, Idct, Idct, 4, 4);
            k!(i4ad, $v4, Idct, Iadst, 4, 4);
            k!(i4da, $v4, Iadst, Idct, 4, 4);
            k!(i4aa, $v4, Iadst, Iadst, 4, 4);
            k!(i8dd, $v8, Idct, Idct, 8, 5);
            k!(i8ad, $v8, Idct, Iadst, 8, 5);
            k!(i8da, $v8, Iadst, Idct, 8, 5);
            k!(i8aa, $v8, Iadst, Iadst, 8, 5);
            k!(i16dd, $v16, Idct, Idct, 16, 6);
            k!(i16ad, $v16, Idct, Iadst, 16, 6);
            k!(i16da, $v16, Iadst, Idct, 16, 6);
            k!(i16aa, $v16, Iadst, Iadst, 16, 6);
            k!(i32dd, $v16, Idct, Idct, 32, 6);
            unsafe {
                match (tx_size, tx_type & 3) {
                    (0, 0) => i4dd(buf, rows, dst, stride),
                    (0, 1) => i4ad(buf, rows, dst, stride),
                    (0, 2) => i4da(buf, rows, dst, stride),
                    (0, _) => i4aa(buf, rows, dst, stride),
                    (1, 0) => i8dd(buf, rows, dst, stride),
                    (1, 1) => i8ad(buf, rows, dst, stride),
                    (1, 2) => i8da(buf, rows, dst, stride),
                    (1, _) => i8aa(buf, rows, dst, stride),
                    (2, 0) => i16dd(buf, rows, dst, stride),
                    (2, 1) => i16ad(buf, rows, dst, stride),
                    (2, 2) => i16da(buf, rows, dst, stride),
                    (2, _) => i16aa(buf, rows, dst, stride),
                    _ => i32dd(buf, rows, dst, stride),
                }
            }
        }
    };
}
pub(crate) use kernels;

pub fn dc_add(dc: i32, tx_size: u8, dst: &mut [u8], stride: usize) {
    let n = 4usize << tx_size;
    let shift = (tx_size as u32 + 4).min(6);
    let a = (dc * 11585 + (1 << 13)) >> 14;
    let b = (a * 11585 + (1 << 13)) >> 14;
    let v = (b + (1 << (shift - 1))) >> shift;
    for i in 0..n {
        for p in dst[i * stride..i * stride + n].iter_mut() {
            *p = (*p as i32 + v).clamp(0, 255) as u8;
        }
    }
}

fn fwht1(x: [i32; 4]) -> [i32; 4] {
    let a1 = x[0] + x[1];
    let d1 = x[3] - x[2];
    let e = (a1 - d1) >> 1;
    let b0 = e - x[1];
    let c0 = e - x[2];
    [a1 - c0, c0, d1 + b0, b0]
}

pub fn forward_wht(buf: &mut [i16]) {
    let mut t = [[0i32; 4]; 4];
    for j in 0..4 {
        let y = fwht1([
            buf[j] as i32,
            buf[4 + j] as i32,
            buf[8 + j] as i32,
            buf[12 + j] as i32,
        ]);
        for i in 0..4 {
            t[i][j] = y[i];
        }
    }
    for (u, row) in t.iter().enumerate() {
        let c = fwht1(*row);
        for v in 0..4 {
            buf[v * 4 + u] = c[v] as i16;
        }
    }
}

pub fn inverse_wht_add(buf: &[i16], dst: &mut [u8], stride: usize) {
    let mut t = [[0i32; 4]; 4];
    for (u, row) in t.iter_mut().enumerate() {
        *row = crate::vp9::transform::iwht4(std::array::from_fn(|v| buf[v * 4 + u] as i32), 2);
    }
    for j in 0..4 {
        let r = crate::vp9::transform::iwht4([t[0][j], t[1][j], t[2][j], t[3][j]], 0);
        for i in 0..4 {
            let p = &mut dst[i * stride + j];
            *p = (*p as i32 + r[i]).clamp(0, 255) as u8;
        }
    }
}
