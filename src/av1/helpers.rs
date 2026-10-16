use super::parser::NUM_REF_FRAMES;

const DIV_LUT: [i32; 257] = [
    16384, 16320, 16257, 16194, 16132, 16070, 16009, 15948, 15888, 15828, 15768, 15709, 15650,
    15592, 15534, 15477, 15420, 15364, 15308, 15252, 15197, 15142, 15087, 15033, 14980, 14926,
    14873, 14821, 14769, 14717, 14665, 14614, 14564, 14513, 14463, 14413, 14364, 14315, 14266,
    14218, 14170, 14122, 14075, 14028, 13981, 13935, 13888, 13843, 13797, 13752, 13707, 13662,
    13618, 13574, 13530, 13487, 13443, 13400, 13358, 13315, 13273, 13231, 13190, 13148, 13107,
    13066, 13026, 12985, 12945, 12906, 12866, 12827, 12788, 12749, 12710, 12672, 12633, 12596,
    12558, 12520, 12483, 12446, 12409, 12373, 12336, 12300, 12264, 12228, 12193, 12157, 12122,
    12087, 12053, 12018, 11984, 11950, 11916, 11882, 11848, 11815, 11782, 11749, 11716, 11683,
    11651, 11619, 11586, 11555, 11523, 11491, 11460, 11429, 11398, 11367, 11336, 11305, 11275,
    11245, 11215, 11185, 11155, 11125, 11096, 11067, 11038, 11009, 10980, 10951, 10923, 10894,
    10866, 10838, 10810, 10782, 10755, 10727, 10700, 10673, 10645, 10618, 10592, 10565, 10538,
    10512, 10486, 10460, 10434, 10408, 10382, 10356, 10331, 10305, 10280, 10255, 10230, 10205,
    10180, 10156, 10131, 10107, 10082, 10058, 10034, 10010, 9986, 9963, 9939, 9916, 9892, 9869,
    9846, 9823, 9800, 9777, 9754, 9732, 9709, 9687, 9664, 9642, 9620, 9598, 9576, 9554, 9533, 9511,
    9489, 9468, 9447, 9425, 9404, 9383, 9362, 9341, 9321, 9300, 9279, 9259, 9239, 9218, 9198, 9178,
    9158, 9138, 9118, 9098, 9079, 9059, 9039, 9020, 9001, 8981, 8962, 8943, 8924, 8905, 8886, 8867,
    8849, 8830, 8812, 8793, 8775, 8756, 8738, 8720, 8702, 8684, 8666, 8648, 8630, 8613, 8595, 8577,
    8560, 8542, 8525, 8508, 8490, 8473, 8456, 8439, 8422, 8405, 8389, 8372, 8355, 8339, 8322, 8306,
    8289, 8273, 8257, 8240, 8224, 8208, 8192,
];

const DIV_LUT_BITS: u32 = 8;
const DIV_LUT_PREC_BITS: u32 = 14;

pub fn floor_log2(x: u32) -> u32 {
    x.checked_ilog2().unwrap_or(0)
}

pub fn get_relative_dist(enable_order_hint: bool, order_hint_bits: i32, a: i32, b: i32) -> i32 {
    if !enable_order_hint || order_hint_bits <= 0 {
        return 0;
    }
    let diff = a.wrapping_sub(b);
    let m = 1i32 << (order_hint_bits - 1);
    (diff & (m - 1)) - (diff & m)
}

pub fn find_latest_backward(
    shifted_order_hints: &[i32; NUM_REF_FRAMES],
    used_frame: &[bool; NUM_REF_FRAMES],
    cur_frame_hint: i32,
    latest_order_hint: &mut i32,
) -> i32 {
    let mut found = -1;
    for i in 0..NUM_REF_FRAMES {
        let hint = shifted_order_hints[i];
        if !used_frame[i] && hint >= cur_frame_hint && (found < 0 || hint >= *latest_order_hint) {
            found = i as i32;
            *latest_order_hint = hint;
        }
    }
    found
}

pub fn find_earliest_backward(
    shifted_order_hints: &[i32; NUM_REF_FRAMES],
    used_frame: &[bool; NUM_REF_FRAMES],
    cur_frame_hint: i32,
    earliest_order_hint: &mut i32,
) -> i32 {
    let mut found = -1;
    for i in 0..NUM_REF_FRAMES {
        let hint = shifted_order_hints[i];
        if !used_frame[i] && hint >= cur_frame_hint && (found < 0 || hint < *earliest_order_hint) {
            found = i as i32;
            *earliest_order_hint = hint;
        }
    }
    found
}

pub fn find_latest_forward(
    shifted_order_hints: &[i32; NUM_REF_FRAMES],
    used_frame: &[bool; NUM_REF_FRAMES],
    cur_frame_hint: i32,
    latest_order_hint: &mut i32,
) -> i32 {
    let mut found = -1;
    for i in 0..NUM_REF_FRAMES {
        let hint = shifted_order_hints[i];
        if !used_frame[i] && hint < cur_frame_hint && (found < 0 || hint >= *latest_order_hint) {
            found = i as i32;
            *latest_order_hint = hint;
        }
    }
    found
}

pub fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0;
    while k < 32 && (u64::from(blk_size) << k) < u64::from(target) {
        k += 1;
    }
    k
}

pub fn clip3(x: i64, y: i64, z: i64) -> i64 {
    z.clamp(x, y)
}

pub fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 != 0 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

pub fn round2signed(x: i64, n: u32) -> i64 {
    if n == 0 {
        return x;
    }
    let r = |x: i64| (x + (1i64 << (n - 1))) >> n;
    if x >= 0 { r(x) } else { -r(-x) }
}

pub fn resolve_divisor(d: i64) -> Option<(u32, i64)> {
    let abs = d.unsigned_abs();
    if abs == 0 {
        return None;
    }
    let n = abs.ilog2();
    let e = abs - (1u64 << n);
    let f = if n > DIV_LUT_BITS {
        (e + (1u64 << (n - DIV_LUT_BITS - 1))) >> (n - DIV_LUT_BITS)
    } else {
        e << (DIV_LUT_BITS - n)
    };
    let factor = i64::from(*DIV_LUT.get(f as usize)?);
    Some((n + DIV_LUT_PREC_BITS, if d < 0 { -factor } else { factor }))
}
