use super::Block;

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;

const FIX_0_298631336: i32 = 2446;
const FIX_0_390180644: i32 = 3196;
const FIX_0_541196100: i32 = 4433;
const FIX_0_765366865: i32 = 6270;
const FIX_0_899976223: i32 = 7373;
const FIX_1_175875602: i32 = 9633;
const FIX_1_501321110: i32 = 12299;
const FIX_1_847759065: i32 = 15137;
const FIX_1_961570560: i32 = 16069;
const FIX_2_053119869: i32 = 16819;
const FIX_2_562915447: i32 = 20995;
const FIX_3_072711026: i32 = 25172;

const DCT_SIZE: usize = 8;

#[inline(always)]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

#[inline(always)]
fn into_el(v: i32) -> i16 {
    v as i16
}

#[allow(clippy::erasing_op)]
#[allow(clippy::identity_op)]
pub(super) fn fdct(block: &mut Block) {
    let data = &mut block.0;

    let mut data2 = [0i32; 64];

    for y in 0..8 {
        let offset = y * 8;

        let tmp0 = i32::from(data[offset + 0]) + i32::from(data[offset + 7]);
        let tmp7 = i32::from(data[offset + 0]) - i32::from(data[offset + 7]);
        let tmp1 = i32::from(data[offset + 1]) + i32::from(data[offset + 6]);
        let tmp6 = i32::from(data[offset + 1]) - i32::from(data[offset + 6]);
        let tmp2 = i32::from(data[offset + 2]) + i32::from(data[offset + 5]);
        let tmp5 = i32::from(data[offset + 2]) - i32::from(data[offset + 5]);
        let tmp3 = i32::from(data[offset + 3]) + i32::from(data[offset + 4]);
        let tmp4 = i32::from(data[offset + 3]) - i32::from(data[offset + 4]);

        let tmp10 = tmp0 + tmp3;
        let tmp13 = tmp0 - tmp3;
        let tmp11 = tmp1 + tmp2;
        let tmp12 = tmp1 - tmp2;

        data2[offset + 0] = (tmp10 + tmp11) << PASS1_BITS;
        data2[offset + 4] = (tmp10 - tmp11) << PASS1_BITS;

        let z1 = (tmp12 + tmp13) * FIX_0_541196100;
        data2[offset + 2] = descale(z1 + (tmp13 * FIX_0_765366865), CONST_BITS - PASS1_BITS);
        data2[offset + 6] = descale(z1 + (tmp12 * -FIX_1_847759065), CONST_BITS - PASS1_BITS);

        let z1 = tmp4 + tmp7;
        let z2 = tmp5 + tmp6;
        let z3 = tmp4 + tmp6;
        let z4 = tmp5 + tmp7;
        let z5 = (z3 + z4) * FIX_1_175875602;

        let tmp4 = tmp4 * FIX_0_298631336;
        let tmp5 = tmp5 * FIX_2_053119869;
        let tmp6 = tmp6 * FIX_3_072711026;
        let tmp7 = tmp7 * FIX_1_501321110;
        let z1 = z1 * -FIX_0_899976223;
        let z2 = z2 * -FIX_2_562915447;
        let z3 = z3 * -FIX_1_961570560;
        let z4 = z4 * -FIX_0_390180644;

        let z3 = z3 + z5;
        let z4 = z4 + z5;

        data2[offset + 7] = descale(tmp4 + z1 + z3, CONST_BITS - PASS1_BITS);
        data2[offset + 5] = descale(tmp5 + z2 + z4, CONST_BITS - PASS1_BITS);
        data2[offset + 3] = descale(tmp6 + z2 + z3, CONST_BITS - PASS1_BITS);
        data2[offset + 1] = descale(tmp7 + z1 + z4, CONST_BITS - PASS1_BITS);
    }

    for x in 0..8 {
        let tmp0 = data2[DCT_SIZE * 0 + x] + data2[DCT_SIZE * 7 + x];
        let tmp7 = data2[DCT_SIZE * 0 + x] - data2[DCT_SIZE * 7 + x];
        let tmp1 = data2[DCT_SIZE * 1 + x] + data2[DCT_SIZE * 6 + x];
        let tmp6 = data2[DCT_SIZE * 1 + x] - data2[DCT_SIZE * 6 + x];
        let tmp2 = data2[DCT_SIZE * 2 + x] + data2[DCT_SIZE * 5 + x];
        let tmp5 = data2[DCT_SIZE * 2 + x] - data2[DCT_SIZE * 5 + x];
        let tmp3 = data2[DCT_SIZE * 3 + x] + data2[DCT_SIZE * 4 + x];
        let tmp4 = data2[DCT_SIZE * 3 + x] - data2[DCT_SIZE * 4 + x];

        let tmp10 = tmp0 + tmp3;
        let tmp13 = tmp0 - tmp3;
        let tmp11 = tmp1 + tmp2;
        let tmp12 = tmp1 - tmp2;

        data[DCT_SIZE * 0 + x] = into_el(descale(tmp10 + tmp11, PASS1_BITS));
        data[DCT_SIZE * 4 + x] = into_el(descale(tmp10 - tmp11, PASS1_BITS));

        let z1 = (tmp12 + tmp13) * FIX_0_541196100;
        data[DCT_SIZE * 2 + x] = into_el(descale(
            z1 + tmp13 * FIX_0_765366865,
            CONST_BITS + PASS1_BITS,
        ));
        data[DCT_SIZE * 6 + x] = into_el(descale(
            z1 + tmp12 * -FIX_1_847759065,
            CONST_BITS + PASS1_BITS,
        ));

        let z1 = tmp4 + tmp7;
        let z2 = tmp5 + tmp6;
        let z3 = tmp4 + tmp6;
        let z4 = tmp5 + tmp7;
        let z5 = (z3 + z4) * FIX_1_175875602;

        let tmp4 = tmp4 * FIX_0_298631336;
        let tmp5 = tmp5 * FIX_2_053119869;
        let tmp6 = tmp6 * FIX_3_072711026;
        let tmp7 = tmp7 * FIX_1_501321110;
        let z1 = z1 * -FIX_0_899976223;
        let z2 = z2 * -FIX_2_562915447;
        let z3 = z3 * -FIX_1_961570560;
        let z4 = z4 * -FIX_0_390180644;

        let z3 = z3 + z5;
        let z4 = z4 + z5;

        data[DCT_SIZE * 7 + x] = into_el(descale(tmp4 + z1 + z3, CONST_BITS + PASS1_BITS));
        data[DCT_SIZE * 5 + x] = into_el(descale(tmp5 + z2 + z4, CONST_BITS + PASS1_BITS));
        data[DCT_SIZE * 3 + x] = into_el(descale(tmp6 + z2 + z3, CONST_BITS + PASS1_BITS));
        data[DCT_SIZE * 1 + x] = into_el(descale(tmp7 + z1 + z4, CONST_BITS + PASS1_BITS));
    }
}
