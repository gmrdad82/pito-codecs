use crate::hevc::pel::Pel;
use crate::hevc::simd;
use crate::hevc::tables::{DST_MATRIX, TRANS_MATRIX};

crate::hevc::isa::dispatch! {
pub fn inverse_transform(
    c: &mut [i16],
    n: usize,
    dst: bool,
    max_x: usize,
    max_y: usize,
    bd_shift: u32,
) {
    if simd::itx::inverse_transform(c, n, dst, max_x, max_y, bd_shift) {
        return;
    }
    match n {
        4 => inverse::<4>(c, dst, max_x, max_y, bd_shift),
        8 => inverse::<8>(c, false, max_x, max_y, bd_shift),
        16 => inverse::<16>(c, false, max_x, max_y, bd_shift),
        _ => inverse::<32>(c, false, max_x, max_y, bd_shift),
    }
}
}

#[inline(always)]
fn inverse<const N: usize>(c: &mut [i16], dst: bool, max_x: usize, max_y: usize, bd_shift: u32) {
    let step = 32 / N;
    let basis = |j: usize| -> &[i8] {
        if dst {
            &DST_MATRIX[j][..N]
        } else {
            &TRANS_MATRIX[j * step][..N]
        }
    };
    let mut col = [[0i32; N]; N];
    for x in 0..=max_x.min(N - 1) {
        let acc = &mut col[x];
        for j in 0..=max_y.min(N - 1) {
            let d = c[j * N + x] as i32;
            if d != 0 {
                for (a, &m) in acc.iter_mut().zip(basis(j)) {
                    *a += m as i32 * d;
                }
            }
        }
        for a in acc.iter_mut() {
            *a = ((*a + 64) >> 7).clamp(-32768, 32767);
        }
    }
    let round = 1 << (bd_shift - 1);
    for y in 0..N {
        let mut out = [round; N];
        for (j, g) in col.iter().enumerate().take(max_x.min(N - 1) + 1) {
            let g = g[y];
            if g != 0 {
                for (o, &m) in out.iter_mut().zip(basis(j)) {
                    *o += m as i32 * g;
                }
            }
        }
        for (r, o) in c[y * N..y * N + N].iter_mut().zip(out) {
            *r = (o >> bd_shift).clamp(-32768, 32767) as i16;
        }
    }
}

pub fn inverse_dc(dc: i32, bd_shift: u32) -> i32 {
    let e = ((64 * dc + 64) >> 7).clamp(-32768, 32767);
    (64 * e + (1 << (bd_shift - 1))) >> bd_shift
}

crate::hevc::isa::dispatch! {
pub fn transform_skip(c: &mut [i16], n: usize, bd_shift: u32) {
    let ts_shift = 5 + n.trailing_zeros();
    let round = 1 << (bd_shift - 1);
    for v in c[..n * n].iter_mut() {
        *v = ((((*v as i32) << ts_shift) + round) >> bd_shift).clamp(-32768, 32767) as i16;
    }
}
}

crate::hevc::isa::dispatch! {
pub fn dequant(
    coeffs: &mut [i16],
    n: usize,
    max_x: usize,
    max_y: usize,
    m: Option<&[u8]>,
    scale: i64,
    round: i64,
    bd_shift: u32,
) {
    if simd::itx::dequant(coeffs, n, max_x, max_y, m, scale, round, bd_shift) {
        return;
    }
    for y in 0..=max_y {
        for x in 0..=max_x {
            let v = coeffs[y * n + x];
            if v != 0 {
                let f = m.map_or(16, |m| m[y * n + x] as i64);
                coeffs[y * n + x] =
                    ((v as i64 * f * scale + round) >> bd_shift).clamp(-32768, 32767) as i16;
            }
        }
    }
}
}

crate::hevc::isa::dispatch! {
pub fn add_residual<P: Pel>(plane: &mut [P], stride: usize, n: usize, coeffs: &[i16], max: i32) {
    if simd::itx::add_residual(plane, stride, n, coeffs, max) {
        return;
    }
    for (y, res) in coeffs.chunks_exact(n).take(n).enumerate() {
        let row = &mut plane[y * stride..y * stride + n];
        for (s, &r) in row.iter_mut().zip(res) {
            *s = P::from_i32(((*s).into() + r as i32).clamp(0, max));
        }
    }
}
}
