pub type Soa4 = [[i16; 16]; 16];

#[inline(always)]
fn gather4(px: &[u8; 256], k: usize, rb: usize) -> i32 {
    let (y, x) = (k / 4, k % 4);
    let (by, bx) = (rb / 4, rb % 4);
    px[(by * 4 + y) * 16 + bx * 4 + x] as i32
}

fn luma4_fwd_body(
    src: &[u8; 256],
    pred: &[u8; 256],
    mf: &[i32; 16],
    f: i32,
    qbits: u32,
    lim: i32,
) -> Soa4 {
    let lim = lim.min(i16::MAX as i32);
    let mut out = [[0i16; 16]; 16];
    for rb in 0..16 {
        let mut d = [0i32; 16];
        for (k, v) in d.iter_mut().enumerate() {
            *v = gather4(src, k, rb) - gather4(pred, k, rb);
        }
        let mut t = [0i32; 16];
        for i in 0..4 {
            let (a, b, c, e) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
            t[i * 4] = a + e + b + c;
            t[i * 4 + 1] = 2 * (a - e) + (b - c);
            t[i * 4 + 2] = a + e - b - c;
            t[i * 4 + 3] = (a - e) - 2 * (b - c);
        }
        for i in 0..4 {
            let (a, b, c, e) = (t[i], t[4 + i], t[8 + i], t[12 + i]);
            let o = [
                a + e + b + c,
                2 * (a - e) + (b - c),
                a + e - b - c,
                (a - e) - 2 * (b - c),
            ];
            for (k, &v) in o.iter().enumerate() {
                let pos = k * 4 + i;
                let q = ((v.abs() * mf[pos] + f) >> qbits).min(lim);
                out[pos][rb] = (if v < 0 { -q } else { q }) as i16;
            }
        }
    }
    out
}

fn luma4_rec_body(levels: &Soa4, v: &[i32; 16], sh: u32, pred: &[u8; 256], out: &mut [u8; 256]) {
    for rb in 0..16 {
        let mut d = [0i32; 16];
        for (k, x) in d.iter_mut().enumerate() {
            *x = (levels[k][rb] as i32 * v[k]) << sh;
        }
        let mut f = [0i32; 16];
        for i in 0..4 {
            let (d0, d1, d2, d3) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
            let (e, ff, g, h) = (d0 + d2, d0 - d2, (d1 >> 1) - d3, d1 + (d3 >> 1));
            f[i * 4] = e + h;
            f[i * 4 + 1] = ff + g;
            f[i * 4 + 2] = ff - g;
            f[i * 4 + 3] = e - h;
        }
        let (by, bx) = (rb / 4, rb % 4);
        for j in 0..4 {
            let (f0, f1, f2, f3) = (f[j], f[4 + j], f[8 + j], f[12 + j]);
            let (e, ff, g, h) = (f0 + f2, f0 - f2, (f1 >> 1) - f3, f1 + (f3 >> 1));
            for (y, r) in [e + h, ff + g, ff - g, e - h].into_iter().enumerate() {
                let i = (by * 4 + y) * 16 + bx * 4 + j;
                out[i] = (pred[i] as i32 + ((r + 32) >> 6)).clamp(0, 255) as u8;
            }
        }
    }
}

fn fdct8_1d(s: [i32; 8]) -> [i32; 8] {
    let a: [i32; 4] = std::array::from_fn(|k| s[k] + s[7 - k]);
    let b: [i32; 4] = std::array::from_fn(|k| s[k] - s[7 - k]);
    let (e0, e1, e2, e3) = (a[0] + a[3], a[1] + a[2], a[0] - a[3], a[1] - a[2]);
    [
        8 * (e0 + e1),
        12 * b[0] + 10 * b[1] + 6 * b[2] + 3 * b[3],
        8 * e2 + 4 * e3,
        10 * b[0] - 3 * b[1] - 12 * b[2] - 6 * b[3],
        8 * (e0 - e1),
        6 * b[0] - 12 * b[1] + 3 * b[2] + 10 * b[3],
        4 * e2 - 8 * e3,
        3 * b[0] - 6 * b[1] + 10 * b[2] - 12 * b[3],
    ]
}

fn idct8_1d(s: [i32; 8]) -> [i32; 8] {
    let (a0, a4) = (s[0] + s[4], s[0] - s[4]);
    let (a2, a6) = ((s[2] >> 1) - s[6], s[2] + (s[6] >> 1));
    let (b0, b2, b4, b6) = (a0 + a6, a4 + a2, a4 - a2, a0 - a6);
    let a1 = -s[3] + s[5] - s[7] - (s[7] >> 1);
    let a3 = s[1] + s[7] - s[3] - (s[3] >> 1);
    let a5 = -s[1] + s[7] + s[5] + (s[5] >> 1);
    let a7 = s[3] + s[5] + s[1] + (s[1] >> 1);
    let (b1, b7) = (a1 + (a7 >> 2), a7 - (a1 >> 2));
    let (b3, b5) = (a3 + (a5 >> 2), (a3 >> 2) - a5);
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

fn luma8_fwd_body(
    src: &[u8; 256],
    pred: &[u8; 256],
    b8: usize,
    mf: &[i32; 64],
    f: i64,
    qbits: u32,
) -> [i32; 64] {
    let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
    let mut t = [[0i32; 8]; 8];
    for (y, row) in t.iter_mut().enumerate() {
        let s: [i32; 8] = std::array::from_fn(|x| {
            let i = (oy + y) * 16 + ox + x;
            src[i] as i32 - pred[i] as i32
        });
        *row = fdct8_1d(s);
    }
    let mut out = [0i32; 64];
    for x in 0..8 {
        let o = fdct8_1d(std::array::from_fn(|y| t[y][x]));
        for (k, &v) in o.iter().enumerate() {
            let a = ((v.unsigned_abs() as u64 * mf[k * 8 + x] as u64 + f as u64) >> qbits) as i32;
            out[k * 8 + x] = if v < 0 { -a } else { a };
        }
    }
    out
}

fn dequant8(level: i32, v: i32, q6: i32) -> i32 {
    let ls = 16 * v;
    if q6 >= 6 {
        (level * ls) << (q6 - 6)
    } else {
        (level * ls + (1 << (5 - q6))) >> (6 - q6)
    }
}

fn luma8_rec_body(
    levels: &[i32; 64],
    v: &[i32; 64],
    qp: u8,
    pred: &[u8; 256],
    b8: usize,
    out: &mut [u8; 256],
) {
    let q6 = (qp / 6) as i32;
    let mut g = [[0i32; 8]; 8];
    for (i, row) in g.iter_mut().enumerate() {
        *row = idct8_1d(std::array::from_fn(|k| {
            dequant8(levels[i * 8 + k], v[i * 8 + k], q6)
        }));
    }
    let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
    for x in 0..8 {
        let o = idct8_1d(std::array::from_fn(|i| g[i][x]));
        for (y, &r) in o.iter().enumerate() {
            let i = (oy + y) * 16 + ox + x;
            out[i] = (pred[i] as i32 + ((r + 32) >> 6)).clamp(0, 255) as u8;
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::Soa4;
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    fn byte_transpose() -> __m128i {
        _mm_setr_epi8(0, 4, 8, 12, 1, 5, 9, 13, 2, 6, 10, 14, 3, 7, 11, 15)
    }

    #[target_feature(enable = "avx2")]
    fn transpose4(r: [__m128i; 4]) -> [__m128i; 4] {
        let t0 = _mm_unpacklo_epi32(r[0], r[1]);
        let t1 = _mm_unpacklo_epi32(r[2], r[3]);
        let t2 = _mm_unpackhi_epi32(r[0], r[1]);
        let t3 = _mm_unpackhi_epi32(r[2], r[3]);
        [
            _mm_unpacklo_epi64(t0, t1),
            _mm_unpackhi_epi64(t0, t1),
            _mm_unpacklo_epi64(t2, t3),
            _mm_unpackhi_epi64(t2, t3),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn gather(px: &[u8; 256], y: usize) -> [__m128i; 4] {
        let m = byte_transpose();
        let mut r = [_mm_setzero_si128(); 4];
        for (by, v) in r.iter_mut().enumerate() {
            let o = (by * 4 + y) * 16;
            let row = unsafe { _mm_loadu_si128(px[o..o + 16].as_ptr().cast()) };
            *v = _mm_shuffle_epi8(row, m);
        }
        transpose4(r)
    }

    #[target_feature(enable = "avx2")]
    fn scatter(cols: [__m128i; 4], y: usize, out: &mut [u8; 256]) {
        let m = byte_transpose();
        for (by, v) in transpose4(cols).into_iter().enumerate() {
            let o = (by * 4 + y) * 16;
            unsafe { _mm_storeu_si128(out[o..o + 16].as_mut_ptr().cast(), _mm_shuffle_epi8(v, m)) };
        }
    }

    #[target_feature(enable = "avx2")]
    fn fwd4(a: __m256i, b: __m256i, c: __m256i, e: __m256i) -> [__m256i; 4] {
        let s03 = _mm256_add_epi16(a, e);
        let d03 = _mm256_sub_epi16(a, e);
        let s12 = _mm256_add_epi16(b, c);
        let d12 = _mm256_sub_epi16(b, c);
        [
            _mm256_add_epi16(s03, s12),
            _mm256_add_epi16(_mm256_add_epi16(d03, d03), d12),
            _mm256_sub_epi16(s03, s12),
            _mm256_sub_epi16(d03, _mm256_add_epi16(d12, d12)),
        ]
    }

    #[target_feature(enable = "avx2")]
    pub fn luma4_fwd(
        src: &[u8; 256],
        pred: &[u8; 256],
        mf: &[i32; 16],
        f: i32,
        qbits: u32,
        lim: i32,
    ) -> Soa4 {
        let mut d = [_mm256_setzero_si256(); 16];
        for y in 0..4 {
            let s = gather(src, y);
            let p = gather(pred, y);
            for x in 0..4 {
                d[y * 4 + x] =
                    _mm256_sub_epi16(_mm256_cvtepu8_epi16(s[x]), _mm256_cvtepu8_epi16(p[x]));
            }
        }
        let mut t = [_mm256_setzero_si256(); 16];
        for y in 0..4 {
            let r = fwd4(d[y * 4], d[y * 4 + 1], d[y * 4 + 2], d[y * 4 + 3]);
            t[y * 4..y * 4 + 4].copy_from_slice(&r);
        }
        let fv = _mm256_set1_epi32(f);
        let sh = _mm_cvtsi32_si128(qbits as i32);
        let lv = _mm256_set1_epi16(lim.min(i16::MAX as i32) as i16);
        let mut out = [[0i16; 16]; 16];
        for x in 0..4 {
            let r = fwd4(t[x], t[4 + x], t[8 + x], t[12 + x]);
            for (k, v) in r.into_iter().enumerate() {
                let pos = k * 4 + x;
                let m = _mm256_set1_epi16(mf[pos] as i16);
                let a = _mm256_abs_epi16(v);
                let lo = _mm256_mullo_epi16(a, m);
                let hi = _mm256_mulhi_epu16(a, m);
                let p0 = _mm256_srl_epi32(_mm256_add_epi32(_mm256_unpacklo_epi16(lo, hi), fv), sh);
                let p1 = _mm256_srl_epi32(_mm256_add_epi32(_mm256_unpackhi_epi16(lo, hi), fv), sh);
                let q = _mm256_min_epi16(_mm256_packs_epi32(p0, p1), lv);
                let q = _mm256_sign_epi16(q, v);
                unsafe { _mm256_storeu_si256(out[pos].as_mut_ptr().cast(), q) };
            }
        }
        out
    }

    #[target_feature(enable = "avx2")]
    fn inv4(a: __m256i, b: __m256i, c: __m256i, d: __m256i) -> [__m256i; 4] {
        let e = _mm256_add_epi32(a, c);
        let f = _mm256_sub_epi32(a, c);
        let g = _mm256_sub_epi32(_mm256_srai_epi32(b, 1), d);
        let h = _mm256_add_epi32(b, _mm256_srai_epi32(d, 1));
        [
            _mm256_add_epi32(e, h),
            _mm256_add_epi32(f, g),
            _mm256_sub_epi32(f, g),
            _mm256_sub_epi32(e, h),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn idct4_half(c: &[__m256i; 16]) -> [__m256i; 16] {
        let mut f = [_mm256_setzero_si256(); 16];
        for i in 0..4 {
            let o = inv4(c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]);
            f[i * 4..i * 4 + 4].copy_from_slice(&o);
        }
        let r32 = _mm256_set1_epi32(32);
        let mut r = [_mm256_setzero_si256(); 16];
        for j in 0..4 {
            let o = inv4(f[j], f[4 + j], f[8 + j], f[12 + j]);
            for (k, x) in o.into_iter().enumerate() {
                r[k * 4 + j] = _mm256_srai_epi32(_mm256_add_epi32(x, r32), 6);
            }
        }
        r
    }

    #[target_feature(enable = "avx2")]
    pub fn luma4_rec(levels: &Soa4, v: &[i32; 16], sh: u32, pred: &[u8; 256], out: &mut [u8; 256]) {
        let shv = _mm_cvtsi32_si128(sh as i32);
        let mut lo = [_mm256_setzero_si256(); 16];
        let mut hi = [_mm256_setzero_si256(); 16];
        for k in 0..16 {
            let l = unsafe { _mm256_loadu_si256(levels[k].as_ptr().cast()) };
            let m = _mm256_set1_epi32(v[k]);
            let a = _mm256_cvtepi16_epi32(_mm256_castsi256_si128(l));
            let b = _mm256_cvtepi16_epi32(_mm256_extracti128_si256(l, 1));
            lo[k] = _mm256_sll_epi32(_mm256_mullo_epi32(a, m), shv);
            hi[k] = _mm256_sll_epi32(_mm256_mullo_epi32(b, m), shv);
        }
        let lo = idct4_half(&lo);
        let hi = idct4_half(&hi);
        for y in 0..4 {
            let p = gather(pred, y);
            let mut cols = [_mm_setzero_si128(); 4];
            for (x, col) in cols.iter_mut().enumerate() {
                let k = y * 4 + x;
                let r = _mm256_permute4x64_epi64(_mm256_packs_epi32(lo[k], hi[k]), 0b1101_1000);
                let s = _mm256_adds_epi16(r, _mm256_cvtepu8_epi16(p[x]));
                *col = _mm_packus_epi16(_mm256_castsi256_si128(s), _mm256_extracti128_si256(s, 1));
            }
            scatter(cols, y, out);
        }
    }

    #[target_feature(enable = "avx2")]
    fn transpose8(m: [__m256i; 8]) -> [__m256i; 8] {
        let a = [
            _mm256_unpacklo_epi32(m[0], m[1]),
            _mm256_unpackhi_epi32(m[0], m[1]),
            _mm256_unpacklo_epi32(m[2], m[3]),
            _mm256_unpackhi_epi32(m[2], m[3]),
            _mm256_unpacklo_epi32(m[4], m[5]),
            _mm256_unpackhi_epi32(m[4], m[5]),
            _mm256_unpacklo_epi32(m[6], m[7]),
            _mm256_unpackhi_epi32(m[6], m[7]),
        ];
        let b = [
            _mm256_unpacklo_epi64(a[0], a[2]),
            _mm256_unpackhi_epi64(a[0], a[2]),
            _mm256_unpacklo_epi64(a[1], a[3]),
            _mm256_unpackhi_epi64(a[1], a[3]),
            _mm256_unpacklo_epi64(a[4], a[6]),
            _mm256_unpackhi_epi64(a[4], a[6]),
            _mm256_unpacklo_epi64(a[5], a[7]),
            _mm256_unpackhi_epi64(a[5], a[7]),
        ];
        [
            _mm256_permute2x128_si256(b[0], b[4], 0x20),
            _mm256_permute2x128_si256(b[1], b[5], 0x20),
            _mm256_permute2x128_si256(b[2], b[6], 0x20),
            _mm256_permute2x128_si256(b[3], b[7], 0x20),
            _mm256_permute2x128_si256(b[0], b[4], 0x31),
            _mm256_permute2x128_si256(b[1], b[5], 0x31),
            _mm256_permute2x128_si256(b[2], b[6], 0x31),
            _mm256_permute2x128_si256(b[3], b[7], 0x31),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn dot4(v: [__m256i; 4], k: [i32; 4]) -> __m256i {
        let mut s = _mm256_mullo_epi32(v[0], _mm256_set1_epi32(k[0]));
        for i in 1..4 {
            s = _mm256_add_epi32(s, _mm256_mullo_epi32(v[i], _mm256_set1_epi32(k[i])));
        }
        s
    }

    #[target_feature(enable = "avx2")]
    fn fwd8(s: [__m256i; 8]) -> [__m256i; 8] {
        let mut a = [_mm256_setzero_si256(); 4];
        let mut b = [_mm256_setzero_si256(); 4];
        for k in 0..4 {
            a[k] = _mm256_add_epi32(s[k], s[7 - k]);
            b[k] = _mm256_sub_epi32(s[k], s[7 - k]);
        }
        let e0 = _mm256_add_epi32(a[0], a[3]);
        let e1 = _mm256_add_epi32(a[1], a[2]);
        let e2 = _mm256_sub_epi32(a[0], a[3]);
        let e3 = _mm256_sub_epi32(a[1], a[2]);
        [
            _mm256_slli_epi32(_mm256_add_epi32(e0, e1), 3),
            dot4(b, [12, 10, 6, 3]),
            _mm256_add_epi32(_mm256_slli_epi32(e2, 3), _mm256_slli_epi32(e3, 2)),
            dot4(b, [10, -3, -12, -6]),
            _mm256_slli_epi32(_mm256_sub_epi32(e0, e1), 3),
            dot4(b, [6, -12, 3, 10]),
            _mm256_sub_epi32(_mm256_slli_epi32(e2, 2), _mm256_slli_epi32(e3, 3)),
            dot4(b, [3, -6, 10, -12]),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn inv8(s: [__m256i; 8]) -> [__m256i; 8] {
        let a0 = _mm256_add_epi32(s[0], s[4]);
        let a4 = _mm256_sub_epi32(s[0], s[4]);
        let a2 = _mm256_sub_epi32(_mm256_srai_epi32(s[2], 1), s[6]);
        let a6 = _mm256_add_epi32(s[2], _mm256_srai_epi32(s[6], 1));
        let b0 = _mm256_add_epi32(a0, a6);
        let b2 = _mm256_add_epi32(a4, a2);
        let b4 = _mm256_sub_epi32(a4, a2);
        let b6 = _mm256_sub_epi32(a0, a6);
        let a1 = _mm256_sub_epi32(
            _mm256_sub_epi32(_mm256_sub_epi32(s[5], s[3]), s[7]),
            _mm256_srai_epi32(s[7], 1),
        );
        let a3 = _mm256_sub_epi32(
            _mm256_sub_epi32(_mm256_add_epi32(s[1], s[7]), s[3]),
            _mm256_srai_epi32(s[3], 1),
        );
        let a5 = _mm256_add_epi32(
            _mm256_add_epi32(_mm256_sub_epi32(s[7], s[1]), s[5]),
            _mm256_srai_epi32(s[5], 1),
        );
        let a7 = _mm256_add_epi32(
            _mm256_add_epi32(_mm256_add_epi32(s[3], s[5]), s[1]),
            _mm256_srai_epi32(s[1], 1),
        );
        let b1 = _mm256_add_epi32(a1, _mm256_srai_epi32(a7, 2));
        let b7 = _mm256_sub_epi32(a7, _mm256_srai_epi32(a1, 2));
        let b3 = _mm256_add_epi32(a3, _mm256_srai_epi32(a5, 2));
        let b5 = _mm256_sub_epi32(_mm256_srai_epi32(a3, 2), a5);
        [
            _mm256_add_epi32(b0, b7),
            _mm256_add_epi32(b2, b5),
            _mm256_add_epi32(b4, b3),
            _mm256_add_epi32(b6, b1),
            _mm256_sub_epi32(b6, b1),
            _mm256_sub_epi32(b4, b3),
            _mm256_sub_epi32(b2, b5),
            _mm256_sub_epi32(b0, b7),
        ]
    }

    #[target_feature(enable = "avx2")]
    fn row8(px: &[u8; 256], o: usize) -> __m256i {
        _mm256_cvtepu8_epi32(unsafe { _mm_loadl_epi64(px[o..o + 8].as_ptr().cast()) })
    }

    #[target_feature(enable = "avx2")]
    pub fn luma8_fwd(
        src: &[u8; 256],
        pred: &[u8; 256],
        b8: usize,
        mf: &[i32; 64],
        f: i64,
        qbits: u32,
    ) -> [i32; 64] {
        let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
        let mut d = [_mm256_setzero_si256(); 8];
        for (y, v) in d.iter_mut().enumerate() {
            let o = (oy + y) * 16 + ox;
            *v = _mm256_sub_epi32(row8(src, o), row8(pred, o));
        }
        let t = transpose8(fwd8(transpose8(fwd8(d))));
        let fv = _mm256_set1_epi64x(f);
        let sh = _mm_cvtsi32_si128(qbits as i32);
        let mut out = [0i32; 64];
        for (k, &v) in t.iter().enumerate() {
            let a = _mm256_abs_epi32(v);
            let m = unsafe { _mm256_loadu_si256(mf[k * 8..k * 8 + 8].as_ptr().cast()) };
            let even = _mm256_srl_epi64(_mm256_add_epi64(_mm256_mul_epu32(a, m), fv), sh);
            let odd = _mm256_srl_epi64(
                _mm256_add_epi64(
                    _mm256_mul_epu32(_mm256_srli_epi64(a, 32), _mm256_srli_epi64(m, 32)),
                    fv,
                ),
                sh,
            );
            let r = _mm256_blend_epi32(even, _mm256_slli_epi64(odd, 32), 0b1010_1010);
            let r = _mm256_sign_epi32(r, v);
            unsafe { _mm256_storeu_si256(out[k * 8..k * 8 + 8].as_mut_ptr().cast(), r) };
        }
        out
    }

    #[target_feature(enable = "avx2")]
    pub fn luma8_rec(
        levels: &[i32; 64],
        v: &[i32; 64],
        qp: u8,
        pred: &[u8; 256],
        b8: usize,
        out: &mut [u8; 256],
    ) {
        let q6 = (qp / 6) as i32;
        let up = _mm_cvtsi32_si128((q6 - 6).max(0));
        let down = _mm_cvtsi32_si128((6 - q6).max(0));
        let round = _mm256_set1_epi32(if q6 >= 6 { 0 } else { 1 << (5 - q6) });
        let mut c = [_mm256_setzero_si256(); 8];
        for (k, x) in c.iter_mut().enumerate() {
            let l = unsafe { _mm256_loadu_si256(levels[k * 8..k * 8 + 8].as_ptr().cast()) };
            let m = unsafe { _mm256_loadu_si256(v[k * 8..k * 8 + 8].as_ptr().cast()) };
            let p = _mm256_mullo_epi32(l, _mm256_slli_epi32(m, 4));
            *x = if q6 >= 6 {
                _mm256_sll_epi32(p, up)
            } else {
                _mm256_sra_epi32(_mm256_add_epi32(p, round), down)
            };
        }
        let r = inv8(transpose8(inv8(transpose8(c))));
        let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
        let r32 = _mm256_set1_epi32(32);
        for (y, row) in r.into_iter().enumerate() {
            let o = (oy + y) * 16 + ox;
            let s = _mm256_add_epi32(
                _mm256_srai_epi32(_mm256_add_epi32(row, r32), 6),
                row8(pred, o),
            );
            let w = _mm256_permute4x64_epi64(_mm256_packus_epi32(s, s), 0b1000);
            let b = _mm_packus_epi16(_mm256_castsi256_si128(w), _mm256_castsi256_si128(w));
            unsafe { _mm_storel_epi64(out[o..o + 8].as_mut_ptr().cast(), b) };
        }
    }
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

pub fn luma4_fwd(
    src: &[u8; 256],
    pred: &[u8; 256],
    mf: &[i32; 16],
    f: i32,
    qbits: u32,
    lim: i32,
) -> Soa4 {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        return unsafe { avx2::luma4_fwd(src, pred, mf, f, qbits, lim) };
    }
    luma4_fwd_body(src, pred, mf, f, qbits, lim)
}

pub fn luma4_rec(levels: &Soa4, v: &[i32; 16], sh: u32, pred: &[u8; 256], out: &mut [u8; 256]) {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        return unsafe { avx2::luma4_rec(levels, v, sh, pred, out) };
    }
    luma4_rec_body(levels, v, sh, pred, out)
}

pub fn luma8_fwd(
    src: &[u8; 256],
    pred: &[u8; 256],
    b8: usize,
    mf: &[i32; 64],
    f: i64,
    qbits: u32,
) -> [i32; 64] {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        return unsafe { avx2::luma8_fwd(src, pred, b8, mf, f, qbits) };
    }
    luma8_fwd_body(src, pred, b8, mf, f, qbits)
}

pub fn luma8_rec(
    levels: &[i32; 64],
    v: &[i32; 64],
    qp: u8,
    pred: &[u8; 256],
    b8: usize,
    out: &mut [u8; 256],
) {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        return unsafe { avx2::luma8_rec(levels, v, qp, pred, b8, out) };
    }
    luma8_rec_body(levels, v, qp, pred, b8, out)
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use crate::h264::enc::transform::QuantTables;

    #[test]
    fn avx2_kernels_match_portable() {
        if !has_avx2() {
            return;
        }
        let qt = QuantTables::new();
        let mut s = 7u64;
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as u32
        };
        for round in 0..416 {
            let spread = [255, 40, 6][round % 3];
            let mut src = [0u8; 256];
            let mut pred = [0u8; 256];
            for i in 0..256 {
                src[i] = (next() % 256) as u8;
                let d = (next() % (2 * spread + 1)) as i32 - spread as i32;
                pred[i] = (src[i] as i32 + d).clamp(0, 255) as u8;
            }
            let qp = (round % 52) as u8;
            let dz = [43i64, 85][round % 2];
            let q4 = 15 + (qp / 6) as u32;
            let f4 = (((1i64 << q4) * dz) >> 8) as i32;
            let mf4 = &qt.mf4[(qp % 6) as usize];
            let lim = [i32::MAX, 2063][round % 2];
            let a = unsafe { avx2::luma4_fwd(&src, &pred, mf4, f4, q4, lim) };
            assert_eq!(a, luma4_fwd_body(&src, &pred, mf4, f4, q4, lim));
            let (mut x, mut y) = ([0u8; 256], [0u8; 256]);
            let v4 = &qt.v4[(qp % 6) as usize];
            unsafe { avx2::luma4_rec(&a, v4, (qp / 6) as u32, &pred, &mut x) };
            luma4_rec_body(&a, v4, (qp / 6) as u32, &pred, &mut y);
            assert_eq!(x, y);
            let q8 = 22 + (qp / 6) as u32;
            let f8 = ((1i64 << q8) * dz) >> 8;
            let mf8 = &qt.mf8[(qp % 6) as usize];
            let v8 = &qt.v8[(qp % 6) as usize];
            for b8 in 0..4 {
                let a = unsafe { avx2::luma8_fwd(&src, &pred, b8, mf8, f8, q8) };
                assert_eq!(a, luma8_fwd_body(&src, &pred, b8, mf8, f8, q8));
                unsafe { avx2::luma8_rec(&a, v8, qp, &pred, b8, &mut x) };
                luma8_rec_body(&a, v8, qp, &pred, b8, &mut y);
                assert_eq!(x, y);
            }
        }
    }
}
