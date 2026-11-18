const Y_CF: i32 = 16384;
const CR_CF: i32 = 22970;
const CB_CF: i32 = 29032;
const G_CR: i32 = -11700;
const G_CB: i32 = -5638;
const PREC: i32 = 14;
const RND: i32 = (1 << (PREC - 1)) - 1;

fn mix(near: u8, far: u8) -> u8 {
    ((3 * u16::from(near) + u16::from(far) + 2) >> 2) as u8
}

pub fn horizontal(input: &[u8], out: &mut [u8]) {
    let w = input.len();
    if w == 0 || out.len() < 2 * w {
        return;
    }
    if w == 1 {
        out[..2].fill(input[0]);
        return;
    }
    out[0] = input[0];
    out[1] = mix(input[0], input[1]);
    for (o, i) in out[2..2 * w - 2].chunks_exact_mut(2).zip(input.windows(3)) {
        o[0] = mix(i[1], i[0]);
        o[1] = mix(i[1], i[2]);
    }
    out[2 * w - 2] = mix(input[w - 1], input[w - 2]);
    out[2 * w - 1] = input[w - 1];
}

pub fn vertical(cur: &[u8], far: &[u8], out: &mut [u8]) {
    for ((o, &c), &f) in out.iter_mut().zip(cur).zip(far) {
        *o = mix(c, f);
    }
}

pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl Plane<'_> {
    fn row(&self, y: usize) -> &[u8] {
        let y = y.min(self.height.saturating_sub(1));
        let at = y * self.stride;
        self.data.get(at..at + self.width).unwrap_or(&[])
    }
}

pub struct Upsampler {
    line: Vec<u8>,
    rows: Vec<u8>,
}

impl Upsampler {
    pub fn new(src: &Plane, fx: usize, width: usize) -> super::Result<Upsampler> {
        let wide = if fx == 1 {
            0
        } else {
            (src.width * fx).max(width)
        };
        Ok(Upsampler {
            line: super::zeroed(wide, 0u8)?,
            rows: super::zeroed(src.width, 0u8)?,
        })
    }

    pub fn row<'a>(&'a mut self, src: &'a Plane, fx: usize, fy: usize, y: usize) -> &'a [u8] {
        let sy = y / fy;
        let cur = src.row(sy);
        match (fx, fy) {
            (1, 2) | (2, 2) => {
                let far = if y.is_multiple_of(2) {
                    src.row(sy.saturating_sub(1))
                } else {
                    src.row(sy + 1)
                };
                vertical(cur, far, &mut self.rows);
                if fx == 1 {
                    return &self.rows;
                }
                horizontal(&self.rows, &mut self.line);
            }
            (1, _) => return cur,
            (2, 1) => horizontal(cur, &mut self.line),
            _ => {
                for (o, &v) in self.line.chunks_exact_mut(fx).zip(cur) {
                    o.fill(v);
                }
            }
        }
        &self.line
    }
}

fn clamp(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub fn ycbcr(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) {
    let n = y.len().min(cb.len()).min(cr.len()).min(out.len() / 4);
    #[cfg(target_arch = "x86_64")]
    let done = if wide() {
        unsafe { avx2::ycbcr(&y[..n], &cb[..n], &cr[..n], out) }
    } else {
        0
    };
    #[cfg(not(target_arch = "x86_64"))]
    let done = 0;
    ycbcr_scalar(&y[done..], &cb[done..], &cr[done..], &mut out[done * 4..]);
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
    use super::{CB_CF, CR_CF, G_CB, G_CR, PREC, RND, Y_CF};
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    pub fn ycbcr(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) -> usize {
        let n = y.len();
        let lanes = |p: &[u8], at: usize| unsafe {
            _mm256_cvtepu8_epi32(_mm_loadl_epi64(p.as_ptr().add(at).cast()))
        };
        let c128 = _mm256_set1_epi32(128);
        let rnd = _mm256_set1_epi32(RND);
        let (ycf, crcf, cbcf) = (
            _mm256_set1_epi32(Y_CF),
            _mm256_set1_epi32(CR_CF),
            _mm256_set1_epi32(CB_CF),
        );
        let (gcr, gcb) = (_mm256_set1_epi32(G_CR), _mm256_set1_epi32(G_CB));
        let zero = _mm256_setzero_si256();
        let top = _mm256_set1_epi32(255);
        let clamp = |v: __m256i| _mm256_min_epi32(_mm256_max_epi32(v, zero), top);
        let mut x = 0;
        while x + 8 <= n {
            let luma = _mm256_add_epi32(_mm256_mullo_epi32(lanes(y, x), ycf), rnd);
            let cbv = _mm256_sub_epi32(lanes(cb, x), c128);
            let crv = _mm256_sub_epi32(lanes(cr, x), c128);
            let r = _mm256_srai_epi32::<{ PREC }>(_mm256_add_epi32(
                luma,
                _mm256_mullo_epi32(crv, crcf),
            ));
            let g = _mm256_srai_epi32::<{ PREC }>(_mm256_add_epi32(
                _mm256_add_epi32(luma, _mm256_mullo_epi32(crv, gcr)),
                _mm256_mullo_epi32(cbv, gcb),
            ));
            let b = _mm256_srai_epi32::<{ PREC }>(_mm256_add_epi32(
                luma,
                _mm256_mullo_epi32(cbv, cbcf),
            ));
            let px = _mm256_or_si256(
                _mm256_or_si256(clamp(r), _mm256_slli_epi32::<8>(clamp(g))),
                _mm256_or_si256(
                    _mm256_slli_epi32::<16>(clamp(b)),
                    _mm256_set1_epi32(0xFF00_0000u32 as i32),
                ),
            );
            unsafe { _mm256_storeu_si256(out.as_mut_ptr().add(x * 4).cast(), px) };
            x += 8;
        }
        x
    }
}

fn ycbcr_scalar(y: &[u8], cb: &[u8], cr: &[u8], out: &mut [u8]) {
    for (((px, &y), &cb), &cr) in out.chunks_exact_mut(4).zip(y).zip(cb).zip(cr) {
        let cb = i32::from(cb) - 128;
        let cr = i32::from(cr) - 128;
        let y0 = i32::from(y) * Y_CF + RND;
        px[0] = clamp((y0 + cr * CR_CF) >> PREC);
        px[1] = clamp((y0 + cr * G_CR + cb * G_CB) >> PREC);
        px[2] = clamp((y0 + cb * CB_CF) >> PREC);
        px[3] = 255;
    }
}

pub fn grey(y: &[u8], out: &mut [u8]) {
    for (px, &y) in out.chunks_exact_mut(4).zip(y) {
        px.copy_from_slice(&[y, y, y, 255]);
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn the_wide_conversion_writes_the_scalar_bytes() {
        let mut s = 99u32;
        let mut bytes = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (s >> 24) as u8
                })
                .collect()
        };
        for n in [0usize, 1, 7, 8, 9, 63, 64, 1000, 1921] {
            let (y, cb, cr) = (bytes(n), bytes(n), bytes(n));
            let mut a = vec![0u8; n * 4];
            let mut b = vec![0u8; n * 4];
            ycbcr(&y, &cb, &cr, &mut a);
            ycbcr_scalar(&y, &cb, &cr, &mut b);
            assert!(a == b, "{n} pixels");
        }
    }
}
