use super::{Block, Ops, color};
use std::arch::x86_64::*;

#[derive(Clone, Copy)]
pub(super) struct Avx2(());

impl Avx2 {
    pub(super) fn detect() -> Option<Self> {
        (std::env::var_os("PITO_CODECS_NO_SIMD").is_none()
            && std::arch::is_x86_feature_detected!("avx2"))
        .then_some(Avx2(()))
    }
}

impl Ops for Avx2 {
    #[inline]
    fn ycbcr_row(self, rgba: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
        unsafe { ycbcr_row(rgba, y, cb, cr) }
    }

    #[inline]
    fn fdct(self, block: &mut Block) {
        unsafe { fdct(block) }
    }
}

#[target_feature(enable = "avx2")]
fn ycbcr_row(rgba: &[u8], y: &mut [u8], cb: &mut [u8], cr: &mut [u8]) {
    let n = (rgba.len() / 4).min(y.len()).min(cb.len()).min(cr.len());

    let mask = _mm256_set1_epi32(0xFF);
    let round = _mm256_set1_epi32(0x7FFF);
    let offset = _mm256_set1_epi32((128 << 16) + 0x7FFF);

    let ymulr = _mm256_set1_epi32(19595);
    let ymulg = _mm256_set1_epi32(38470);
    let ymulb = _mm256_set1_epi32(7471);

    let cbmulr = _mm256_set1_epi32(-11059);
    let cbmulg = _mm256_set1_epi32(21709);
    let cbmulb = _mm256_set1_epi32(32768);

    let crmulr = _mm256_set1_epi32(32768);
    let crmulg = _mm256_set1_epi32(27439);
    let crmulb = _mm256_set1_epi32(5329);

    let order = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);

    let mut x = 0;
    while x + 8 <= n {
        let px = &rgba[x * 4..x * 4 + 32];
        let v = unsafe { _mm256_loadu_si256(px.as_ptr().cast()) };
        let r = _mm256_and_si256(v, mask);
        let g = _mm256_and_si256(_mm256_srli_epi32(v, 8), mask);
        let b = _mm256_and_si256(_mm256_srli_epi32(v, 16), mask);

        let yv = _mm256_add_epi32(
            _mm256_add_epi32(_mm256_mullo_epi32(ymulr, r), _mm256_mullo_epi32(ymulg, g)),
            _mm256_mullo_epi32(ymulb, b),
        );
        let yv = _mm256_srli_epi32(_mm256_add_epi32(yv, round), 16);

        let cbv = _mm256_add_epi32(
            _mm256_sub_epi32(_mm256_mullo_epi32(cbmulr, r), _mm256_mullo_epi32(cbmulg, g)),
            _mm256_mullo_epi32(cbmulb, b),
        );
        let cbv = _mm256_srli_epi32(_mm256_add_epi32(cbv, offset), 16);

        let crv = _mm256_sub_epi32(
            _mm256_sub_epi32(_mm256_mullo_epi32(crmulr, r), _mm256_mullo_epi32(crmulg, g)),
            _mm256_mullo_epi32(crmulb, b),
        );
        let crv = _mm256_srli_epi32(_mm256_add_epi32(crv, offset), 16);

        let ycb = _mm256_packus_epi32(yv, cbv);
        let crcr = _mm256_packus_epi32(crv, crv);
        let packed = _mm256_permutevar8x32_epi32(_mm256_packus_epi16(ycb, crcr), order);

        y[x..x + 8].copy_from_slice(&_mm256_extract_epi64::<0>(packed).to_le_bytes());
        cb[x..x + 8].copy_from_slice(&_mm256_extract_epi64::<1>(packed).to_le_bytes());
        cr[x..x + 8].copy_from_slice(&_mm256_extract_epi64::<2>(packed).to_le_bytes());
        x += 8;
    }

    color::ycbcr_row(
        &rgba[x * 4..n * 4],
        &mut y[x..n],
        &mut cb[x..n],
        &mut cr[x..n],
    );
}

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

const F_0_298: i16 = 2446;
const F_0_390: i16 = 3196;
const F_0_541: i16 = 4433;
const F_0_765: i16 = 6270;
const F_0_899: i16 = 7373;
const F_1_175: i16 = 9633;
const F_1_501: i16 = 12299;
const F_1_847: i16 = 15137;
const F_1_961: i16 = 16069;
const F_2_053: i16 = 16819;
const F_2_562: i16 = 20995;
const F_3_072: i16 = 25172;

const DESCALE_P1: i32 = CONST_BITS - PASS1_BITS;
const DESCALE_P2: i32 = CONST_BITS + PASS1_BITS;

#[target_feature(enable = "avx2")]
fn pair(hi: [i16; 2], lo: [i16; 2]) -> __m256i {
    _mm256_set_epi16(
        hi[0], hi[1], hi[0], hi[1], hi[0], hi[1], hi[0], hi[1], lo[0], lo[1], lo[0], lo[1], lo[0],
        lo[1], lo[0], lo[1],
    )
}

#[derive(Clone, Copy)]
struct Consts {
    f130_f054: __m256i,
    f078_f117: __m256i,
    f060_f089: __m256i,
    f050_f256: __m256i,
    descale_p1: __m256i,
    descale_p2: __m256i,
    descale_p2x: __m256i,
    sign: __m256i,
}

#[target_feature(enable = "avx2")]
fn consts() -> Consts {
    Consts {
        f130_f054: pair([F_0_541, F_0_541 - F_1_847], [F_0_541, F_0_541 + F_0_765]),
        f078_f117: pair([F_1_175, F_1_175 - F_0_390], [F_1_175, F_1_175 - F_1_961]),
        f060_f089: pair([-F_2_562, F_2_053 - F_2_562], [-F_0_899, F_0_298 - F_0_899]),
        f050_f256: pair([-F_0_899, F_1_501 - F_0_899], [-F_2_562, F_3_072 - F_2_562]),
        descale_p1: _mm256_set1_epi32(1 << (DESCALE_P1 - 1)),
        descale_p2: _mm256_set1_epi32(1 << (DESCALE_P2 - 1)),
        descale_p2x: _mm256_set1_epi16(1 << (PASS1_BITS - 1)),
        sign: _mm256_set_epi16(-1, -1, -1, -1, -1, -1, -1, -1, 1, 1, 1, 1, 1, 1, 1, 1),
    }
}

#[target_feature(enable = "avx2")]
fn transpose(
    i1: __m256i,
    i2: __m256i,
    i3: __m256i,
    i4: __m256i,
) -> (__m256i, __m256i, __m256i, __m256i) {
    let t5 = _mm256_unpacklo_epi16(i1, i2);
    let t6 = _mm256_unpackhi_epi16(i1, i2);
    let t7 = _mm256_unpacklo_epi16(i3, i4);
    let t8 = _mm256_unpackhi_epi16(i3, i4);

    let t1 = _mm256_unpacklo_epi32(t5, t7);
    let t2 = _mm256_unpackhi_epi32(t5, t7);
    let t3 = _mm256_unpacklo_epi32(t6, t8);
    let t4 = _mm256_unpackhi_epi32(t6, t8);

    (
        _mm256_permute4x64_epi64::<0x8D>(t1),
        _mm256_permute4x64_epi64::<0x8D>(t2),
        _mm256_permute4x64_epi64::<0xD8>(t3),
        _mm256_permute4x64_epi64::<0xD8>(t4),
    )
}

#[target_feature(enable = "avx2")]
fn descale<const FIRST: bool>(v: __m256i, c: &Consts) -> __m256i {
    if FIRST {
        _mm256_srai_epi32::<DESCALE_P1>(_mm256_add_epi32(v, c.descale_p1))
    } else {
        _mm256_srai_epi32::<DESCALE_P2>(_mm256_add_epi32(v, c.descale_p2))
    }
}

#[target_feature(enable = "avx2")]
fn dct<const FIRST: bool>(
    c: &Consts,
    i1: __m256i,
    i2: __m256i,
    i3: __m256i,
    i4: __m256i,
) -> (__m256i, __m256i, __m256i, __m256i) {
    let t5 = _mm256_sub_epi16(i1, i4);
    let t6 = _mm256_add_epi16(i1, i4);
    let t7 = _mm256_add_epi16(i2, i3);
    let t8 = _mm256_sub_epi16(i2, i3);

    let t6 = _mm256_permute2x128_si256::<0x01>(t6, t6);
    let t1 = _mm256_add_epi16(t6, t7);
    let t6 = _mm256_sub_epi16(t6, t7);

    let t7 = _mm256_permute2x128_si256::<0x01>(t1, t1);
    let t1 = _mm256_sign_epi16(t1, c.sign);
    let t7 = _mm256_add_epi16(t7, t1);

    let t1 = if FIRST {
        _mm256_slli_epi16::<PASS1_BITS>(t7)
    } else {
        _mm256_srai_epi16::<PASS1_BITS>(_mm256_add_epi16(t7, c.descale_p2x))
    };

    let t7 = _mm256_permute2x128_si256::<0x01>(t6, t6);
    let t2 = _mm256_unpacklo_epi16(t6, t7);
    let t6 = _mm256_unpackhi_epi16(t6, t7);

    let t2 = descale::<FIRST>(_mm256_madd_epi16(t2, c.f130_f054), c);
    let t6 = descale::<FIRST>(_mm256_madd_epi16(t6, c.f130_f054), c);
    let t3 = _mm256_packs_epi32(t2, t6);

    let t7 = _mm256_add_epi16(t8, t5);
    let t2 = _mm256_permute2x128_si256::<0x01>(t7, t7);
    let t6 = _mm256_unpacklo_epi16(t7, t2);
    let t7 = _mm256_unpackhi_epi16(t7, t2);

    let t6 = _mm256_madd_epi16(t6, c.f078_f117);
    let t7 = _mm256_madd_epi16(t7, c.f078_f117);

    let t4 = _mm256_permute2x128_si256::<0x01>(t5, t5);
    let t2 = _mm256_unpacklo_epi16(t8, t4);
    let t4 = _mm256_unpackhi_epi16(t8, t4);

    let t2 = _mm256_madd_epi16(t2, c.f060_f089);
    let t4 = _mm256_madd_epi16(t4, c.f060_f089);

    let t2 = descale::<FIRST>(_mm256_add_epi32(t2, t6), c);
    let t4 = descale::<FIRST>(_mm256_add_epi32(t4, t7), c);
    let t4 = _mm256_packs_epi32(t2, t4);

    let t2 = _mm256_permute2x128_si256::<0x01>(t8, t8);
    let t8 = _mm256_unpacklo_epi16(t5, t2);
    let t5 = _mm256_unpackhi_epi16(t5, t2);

    let t8 = _mm256_madd_epi16(t8, c.f050_f256);
    let t5 = _mm256_madd_epi16(t5, c.f050_f256);

    let t8 = descale::<FIRST>(_mm256_add_epi32(t8, t6), c);
    let t5 = descale::<FIRST>(_mm256_add_epi32(t5, t7), c);
    let t2 = _mm256_packs_epi32(t8, t5);

    (t1, t2, t3, t4)
}

#[target_feature(enable = "avx2")]
fn fdct(block: &mut Block) {
    let c = consts();
    let data = &mut block.0;

    let [ymm4, ymm5, ymm6, ymm7] =
        [0, 16, 32, 48].map(|at| unsafe { _mm256_loadu_si256(data[at..at + 16].as_ptr().cast()) });

    let ymm0 = _mm256_permute2x128_si256::<0x20>(ymm4, ymm6);
    let ymm1 = _mm256_permute2x128_si256::<0x31>(ymm4, ymm6);
    let ymm2 = _mm256_permute2x128_si256::<0x20>(ymm5, ymm7);
    let ymm3 = _mm256_permute2x128_si256::<0x31>(ymm5, ymm7);

    let (ymm0, ymm1, ymm2, ymm3) = transpose(ymm0, ymm1, ymm2, ymm3);
    let (ymm0, ymm1, ymm2, ymm3) = dct::<true>(&c, ymm0, ymm1, ymm2, ymm3);

    let ymm4 = _mm256_permute2x128_si256::<0x20>(ymm1, ymm3);
    let ymm1 = _mm256_permute2x128_si256::<0x31>(ymm1, ymm3);

    let (ymm0, ymm1, ymm2, ymm4) = transpose(ymm0, ymm1, ymm2, ymm4);
    let (ymm0, ymm1, ymm2, ymm4) = dct::<false>(&c, ymm0, ymm1, ymm2, ymm4);

    let out = [
        _mm256_permute2x128_si256::<0x30>(ymm0, ymm1),
        _mm256_permute2x128_si256::<0x20>(ymm2, ymm1),
        _mm256_permute2x128_si256::<0x31>(ymm0, ymm4),
        _mm256_permute2x128_si256::<0x21>(ymm2, ymm4),
    ];
    for (chunk, v) in data.chunks_exact_mut(16).zip(out) {
        unsafe { _mm256_storeu_si256(chunk.as_mut_ptr().cast(), v) };
    }
}
