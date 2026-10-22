#[inline(always)]
fn fit(v: i32, depth: u8) -> i32 {
    let s = 32 - depth as u32;
    v.wrapping_shl(s).wrapping_shr(s)
}

#[inline(always)]
pub fn emit(v: i32, low: u16, shift: u32, depth: u8) -> i32 {
    if shift != 0 && depth >= 24 {
        fit(v.wrapping_shl(shift) | low as i32, depth)
    } else {
        fit(v, depth)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn unmix(
    u: &[i32],
    v: &[i32],
    out: &mut [i32],
    stride: usize,
    mix_bits: u32,
    mix_res: i32,
    shift_uv: &[u16],
    shift: u32,
    depth: u8,
) {
    let rows = out.chunks_mut(stride).zip(u.iter().zip(v));
    let lows = shift_uv.chunks_exact(2);
    if mix_res != 0 {
        for ((o, (&u, &v)), low) in rows.zip(lows) {
            let l = u
                .wrapping_add(v)
                .wrapping_sub(mix_res.wrapping_mul(v).wrapping_shr(mix_bits));
            let r = l.wrapping_sub(v);
            if let [a, b, ..] = o {
                *a = emit(l, low[0], shift, depth);
                *b = emit(r, low[1], shift, depth);
            }
        }
    } else {
        for ((o, (&u, &v)), low) in rows.zip(lows) {
            if let [a, b, ..] = o {
                *a = emit(u, low[0], shift, depth);
                *b = emit(v, low[1], shift, depth);
            }
        }
    }
}
