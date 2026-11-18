const SCALE_BITS: i32 = 512 + 65536 + (128 << 17);

fn wa(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

fn ws(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

fn wm(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

fn fsh(x: i32) -> i32 {
    x.wrapping_shl(12)
}

fn clamp(a: i32) -> u8 {
    a.clamp(0, 255) as u8
}

fn dc_only(v: &[i32; 64], out: &mut [u8], stride: usize) {
    let c = clamp(wa(wa(v[0], 4), 1024) >> 3);
    for row in out.chunks_mut(stride).take(8) {
        if let Some(r) = row.get_mut(..8) {
            r.fill(c);
        }
    }
}

pub fn idct(v: &mut [i32; 64], out: &mut [u8], stride: usize) {
    if v[1..].iter().all(|&c| c == 0) {
        return dc_only(v, out, stride);
    }
    #[cfg(target_arch = "x86_64")]
    if wide() && out.len() >= 7 * stride + 8 {
        return unsafe { avx2::idct(v, out, stride) };
    }
    scalar(v, out, stride)
}

#[cfg(target_arch = "x86_64")]
fn wide() -> bool {
    use std::sync::LazyLock;
    static WIDE: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("PITO_CODECS_NO_SIMD").is_none() && is_x86_feature_detected!("avx2")
    });
    *WIDE
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::SCALE_BITS;
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    fn transpose(r: [__m256i; 8]) -> [__m256i; 8] {
        let t0 = _mm256_unpacklo_epi32(r[0], r[1]);
        let t1 = _mm256_unpackhi_epi32(r[0], r[1]);
        let t2 = _mm256_unpacklo_epi32(r[2], r[3]);
        let t3 = _mm256_unpackhi_epi32(r[2], r[3]);
        let t4 = _mm256_unpacklo_epi32(r[4], r[5]);
        let t5 = _mm256_unpackhi_epi32(r[4], r[5]);
        let t6 = _mm256_unpacklo_epi32(r[6], r[7]);
        let t7 = _mm256_unpackhi_epi32(r[6], r[7]);
        let u0 = _mm256_unpacklo_epi64(t0, t2);
        let u1 = _mm256_unpackhi_epi64(t0, t2);
        let u2 = _mm256_unpacklo_epi64(t1, t3);
        let u3 = _mm256_unpackhi_epi64(t1, t3);
        let u4 = _mm256_unpacklo_epi64(t4, t6);
        let u5 = _mm256_unpackhi_epi64(t4, t6);
        let u6 = _mm256_unpacklo_epi64(t5, t7);
        let u7 = _mm256_unpackhi_epi64(t5, t7);
        [
            _mm256_permute2x128_si256(u0, u4, 0x20),
            _mm256_permute2x128_si256(u1, u5, 0x20),
            _mm256_permute2x128_si256(u2, u6, 0x20),
            _mm256_permute2x128_si256(u3, u7, 0x20),
            _mm256_permute2x128_si256(u0, u4, 0x31),
            _mm256_permute2x128_si256(u1, u5, 0x31),
            _mm256_permute2x128_si256(u2, u6, 0x31),
            _mm256_permute2x128_si256(u3, u7, 0x31),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn mul(a: __m256i, k: i32) -> __m256i {
        _mm256_mullo_epi32(a, _mm256_set1_epi32(k))
    }

    #[target_feature(enable = "avx2")]
    fn pass(r: [__m256i; 8], round: i32, shift: i32) -> [__m256i; 8] {
        let add = _mm256_add_epi32;
        let sub = _mm256_sub_epi32;
        let sh = _mm256_set1_epi32(round);
        let p1 = mul(add(r[2], r[6]), 2217);
        let t2 = add(p1, mul(r[6], -7567));
        let t3 = add(p1, mul(r[2], 3135));
        let t0 = _mm256_slli_epi32(add(r[0], r[4]), 12);
        let t1 = _mm256_slli_epi32(sub(r[0], r[4]), 12);
        let x0 = add(add(t0, t3), sh);
        let x3 = add(sub(t0, t3), sh);
        let x1 = add(add(t1, t2), sh);
        let x2 = add(sub(t1, t2), sh);
        let (a, b, c, d) = (r[7], r[5], r[3], r[1]);
        let p3 = add(a, c);
        let p4 = add(b, d);
        let q1 = add(a, d);
        let q2 = add(b, c);
        let p5 = mul(add(p3, p4), 4816);
        let o0 = mul(a, 1223);
        let o1 = mul(b, 8410);
        let o2 = mul(c, 12586);
        let o3 = mul(d, 6149);
        let q1 = add(p5, mul(q1, -3685));
        let q2 = add(p5, mul(q2, -10497));
        let p3 = mul(p3, -8034);
        let p4 = mul(p4, -1597);
        let e0 = add(o0, add(q1, p3));
        let e1 = add(o1, add(q2, p4));
        let e2 = add(o2, add(q2, p3));
        let e3 = add(o3, add(q1, p4));
        let sr = |x| match shift {
            10 => _mm256_srai_epi32(x, 10),
            _ => _mm256_srai_epi32(x, 17),
        };
        [
            sr(add(x0, e3)),
            sr(add(x1, e2)),
            sr(add(x2, e1)),
            sr(add(x3, e0)),
            sr(sub(x3, e0)),
            sr(sub(x2, e1)),
            sr(sub(x1, e2)),
            sr(sub(x0, e3)),
        ]
    }

    #[target_feature(enable = "avx2")]
    pub fn idct(v: &mut [i32; 64], out: &mut [u8], stride: usize) {
        let load = |k: usize| unsafe { _mm256_loadu_si256(v.as_ptr().add(8 * k).cast()) };
        let rows = [
            load(0),
            load(1),
            load(2),
            load(3),
            load(4),
            load(5),
            load(6),
            load(7),
        ];
        let cols = pass(rows, 512, 10);
        let cols = transpose(cols);
        let px = pass(cols, SCALE_BITS, 17);
        let px = transpose(px);
        for (k, row) in px.iter().enumerate() {
            let lo = _mm256_castsi256_si128(*row);
            let hi = _mm256_extracti128_si256(*row, 1);
            let words = _mm_packs_epi32(lo, hi);
            let bytes = _mm_packus_epi16(words, words);
            unsafe { _mm_storel_epi64(out.as_mut_ptr().add(k * stride).cast(), bytes) };
        }
    }
}

fn scalar(v: &mut [i32; 64], out: &mut [u8], stride: usize) {
    if v[1..].iter().all(|&c| c == 0) {
        return dc_only(v, out, stride);
    }
    for c in 0..8 {
        if (1..8).all(|k| v[c + 8 * k] == 0) {
            let d = wa(fsh(v[c]), 512) >> 10;
            for k in 0..8 {
                v[c + 8 * k] = d;
            }
            continue;
        }
        let p2 = v[c + 16];
        let p3 = v[c + 48];
        let p1 = wm(wa(p2, p3), 2217);
        let t2 = wa(p1, wm(p3, -7567));
        let t3 = wa(p1, wm(p2, 3135));
        let p2 = v[c];
        let p3 = v[32 + c];
        let t0 = fsh(wa(p2, p3));
        let t1 = fsh(ws(p2, p3));
        let x0 = wa(wa(t0, t3), 512);
        let x3 = wa(ws(t0, t3), 512);
        let x1 = wa(wa(t1, t2), 512);
        let x2 = wa(ws(t1, t2), 512);
        let (t0, t1, t2, t3) = odd(v[c + 56], v[c + 40], v[c + 24], v[c + 8]);
        v[c] = wa(x0, t3) >> 10;
        v[c + 8] = wa(x1, t2) >> 10;
        v[c + 16] = wa(x2, t1) >> 10;
        v[c + 24] = wa(x3, t0) >> 10;
        v[c + 32] = ws(x3, t0) >> 10;
        v[c + 40] = ws(x2, t1) >> 10;
        v[c + 48] = ws(x1, t2) >> 10;
        v[c + 56] = ws(x0, t3) >> 10;
    }
    for (r, row) in v.chunks_exact(8).zip(out.chunks_mut(stride)) {
        let p2 = r[2];
        let p3 = r[6];
        let p1 = wm(wa(p2, p3), 2217);
        let t2 = wa(p1, wm(p3, -7567));
        let t3 = wa(p1, wm(p2, 3135));
        let t0 = fsh(wa(r[0], r[4]));
        let t1 = fsh(ws(r[0], r[4]));
        let x0 = wa(wa(t0, t3), SCALE_BITS);
        let x3 = wa(ws(t0, t3), SCALE_BITS);
        let x1 = wa(wa(t1, t2), SCALE_BITS);
        let x2 = wa(ws(t1, t2), SCALE_BITS);
        let (t0, t1, t2, t3) = odd(r[7], r[5], r[3], r[1]);
        let px = [
            clamp(wa(x0, t3) >> 17),
            clamp(wa(x1, t2) >> 17),
            clamp(wa(x2, t1) >> 17),
            clamp(wa(x3, t0) >> 17),
            clamp(ws(x3, t0) >> 17),
            clamp(ws(x2, t1) >> 17),
            clamp(ws(x1, t2) >> 17),
            clamp(ws(x0, t3) >> 17),
        ];
        if let Some(dst) = row.get_mut(..8) {
            dst.copy_from_slice(&px);
        }
    }
}

fn odd(t0: i32, t1: i32, t2: i32, t3: i32) -> (i32, i32, i32, i32) {
    let p3 = wa(t0, t2);
    let p4 = wa(t1, t3);
    let p1 = wa(t0, t3);
    let p2 = wa(t1, t2);
    let p5 = wm(wa(p3, p4), 4816);
    let t0 = wm(t0, 1223);
    let t1 = wm(t1, 8410);
    let t2 = wm(t2, 12586);
    let t3 = wm(t3, 6149);
    let p1 = wa(p5, wm(p1, -3685));
    let p2 = wa(p5, wm(p2, -10497));
    let p3 = wm(p3, -8034);
    let p4 = wm(p4, -1597);
    (
        wa(t0, wa(p1, p3)),
        wa(t1, wa(p2, p4)),
        wa(t2, wa(p2, p3)),
        wa(t3, wa(p1, p4)),
    )
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn the_avx2_transform_writes_the_scalar_bytes_for_every_block_shape() {
        if !wide() {
            return;
        }
        let mut s = 0x1234_5678u32;
        let mut next = move || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            s >> 8
        };
        for round in 0..40_000u32 {
            let mut v = [0i32; 64];
            let density = round % 9;
            let span = match round % 5 {
                0 => 3,
                1 => 200,
                2 => 4000,
                3 => 40_000,
                _ => 260_000,
            };
            for (i, c) in v.iter_mut().enumerate() {
                if i == 0 || next() % 9 < density {
                    *c = (next() % (2 * span + 1)) as i32 - span as i32;
                }
            }
            let (mut a, mut b) = (v, v);
            let mut x = [0u8; 8 * 24];
            let mut y = [0u8; 8 * 24];
            scalar(&mut a, &mut x[3..], 24);
            idct(&mut b, &mut y[3..], 24);
            assert!(x == y, "round {round}: {v:?}");
        }
    }
}
