use super::Fault;
use super::bits::Bits;

pub(super) fn decode(bits: &mut Bits, bps: u32, out: &mut [i32]) -> Result<(), Fault> {
    let x = bits.read(8)?;
    let wasted = if x & 1 != 0 {
        bits.unary()?.saturating_add(1)
    } else {
        0
    };
    if wasted >= bps {
        return Err(Fault::Bad(
            "a subframe with more wasted bits than its sample size",
        ));
    }
    let bps = bps - wasted;
    let t = x & 0xfe;
    match t {
        _ if t & 0x80 != 0 => return Err(Fault::Bad("a subframe header with its zero bit set")),
        0 => {
            let v = bits.signed(bps)?;
            out.fill(v);
        }
        2 => verbatim(bits, bps, out)?,
        _ if t < 16 => return Err(Fault::Bad("a reserved subframe type")),
        _ if t <= 24 => fixed(bits, bps, ((t >> 1) & 7) as usize, out)?,
        _ if t < 64 => return Err(Fault::Bad("a reserved subframe type")),
        _ => lpc(bits, bps, (((t >> 1) & 31) + 1) as usize, out)?,
    }
    if wasted > 0 {
        for s in out.iter_mut() {
            *s = ((*s as u32) << wasted) as i32;
        }
    }
    Ok(())
}

fn verbatim(bits: &mut Bits, bps: u32, out: &mut [i32]) -> Result<(), Fault> {
    for s in out.iter_mut() {
        *s = bits.signed(bps)?;
    }
    Ok(())
}

fn residual(bits: &mut Bits, order: usize, out: &mut [i32]) -> Result<(), Fault> {
    let (plen, escape) = match bits.read(2)? {
        0 => (4, 15),
        1 => (5, 31),
        _ => return Err(Fault::Bad("a reserved residual coding method")),
    };
    let partition_order = bits.read(4)?;
    let block = out.len();
    let samples = block >> partition_order;
    if samples < order || !block.is_multiple_of(1 << partition_order) {
        return Err(Fault::Bad(
            "a residual partition order its block size cannot hold",
        ));
    }
    let mut at = order;
    for p in 0..1usize << partition_order {
        let end = (p + 1) * samples;
        let part = &mut out[at..end];
        let k = bits.read(plen)?;
        if k < escape {
            for s in part.iter_mut() {
                *s = bits.rice(k)?;
            }
        } else {
            let raw = bits.read(5)?;
            if raw == 0 {
                part.fill(0);
            } else {
                for s in part.iter_mut() {
                    *s = bits.signed(raw)?;
                }
            }
        }
        at = end;
    }
    Ok(())
}

fn fixed(bits: &mut Bits, bps: u32, order: usize, out: &mut [i32]) -> Result<(), Fault> {
    if out.len() <= order {
        return Err(Fault::Bad("a fixed predictor longer than its block"));
    }
    verbatim(bits, bps, &mut out[..order])?;
    residual(bits, order, out)?;
    restore_fixed(order, out);
    Ok(())
}

pub(super) fn restore_fixed(order: usize, d: &mut [i32]) {
    match order {
        1 => {
            for i in 1..d.len() {
                d[i] = d[i].wrapping_add(d[i - 1]);
            }
        }
        2 => {
            for i in 2..d.len() {
                let p = d[i - 1].wrapping_mul(2).wrapping_sub(d[i - 2]);
                d[i] = d[i].wrapping_add(p);
            }
        }
        3 => {
            for i in 3..d.len() {
                let p = d[i - 1]
                    .wrapping_sub(d[i - 2])
                    .wrapping_mul(3)
                    .wrapping_add(d[i - 3]);
                d[i] = d[i].wrapping_add(p);
            }
        }
        4 => {
            for i in 4..d.len() {
                let p = d[i - 1]
                    .wrapping_add(d[i - 3])
                    .wrapping_mul(4)
                    .wrapping_sub(d[i - 2].wrapping_mul(6))
                    .wrapping_sub(d[i - 4]);
                d[i] = d[i].wrapping_add(p);
            }
        }
        _ => {}
    }
}

fn silog2(v: u64) -> u32 {
    if v == 0 { 0 } else { 65 - v.leading_zeros() }
}

fn lpc(bits: &mut Bits, bps: u32, order: usize, out: &mut [i32]) -> Result<(), Fault> {
    if out.len() <= order {
        return Err(Fault::Bad("an LPC predictor longer than its block"));
    }
    verbatim(bits, bps, &mut out[..order])?;
    let precision = bits.read(4)?;
    if precision == 15 {
        return Err(Fault::Bad("an LPC coefficient precision of 16 bits"));
    }
    let precision = precision + 1;
    let shift = bits.signed(5)?;
    if shift < 0 {
        return Err(Fault::Bad("a negative LPC shift"));
    }
    let shift = shift as u32;
    let mut qlp = [0i32; 32];
    for c in qlp[..order].iter_mut() {
        *c = bits.signed(precision)?;
    }
    residual(bits, order, out)?;
    let abs_sum: u64 = qlp[..order].iter().map(|c| c.unsigned_abs() as u64).sum();
    let max_prediction = (1u64 << (bps - 1)) * abs_sum;
    let after_shift = max_prediction.div_ceil(1u64 << shift);
    let narrow = silog2(max_prediction) <= 32 && silog2((1u64 << (bps - 1)) + after_shift) <= 32;
    let mut rev = [0i32; 32];
    for (r, &c) in rev[32 - order..].iter_mut().zip(qlp[..order].iter().rev()) {
        *r = c;
    }
    if narrow {
        restore_narrow(&rev, order, shift, out);
    } else {
        restore_wide(&rev[32 - order..], shift, out);
    }
    Ok(())
}

fn restore_narrow(rev: &[i32; 32], order: usize, shift: u32, d: &mut [i32]) {
    macro_rules! fixed_order {
        ($n:literal) => {{
            let mut c = [0i32; $n];
            c.copy_from_slice(&rev[32 - $n..]);
            narrow::<$n>(&c, shift, d)
        }};
    }
    match order {
        1 => fixed_order!(1),
        2 => fixed_order!(2),
        3 => fixed_order!(3),
        4 => fixed_order!(4),
        5 => fixed_order!(5),
        6 => fixed_order!(6),
        7 => fixed_order!(7),
        8 => fixed_order!(8),
        9 => fixed_order!(9),
        10 => fixed_order!(10),
        11 => fixed_order!(11),
        12 => fixed_order!(12),
        _ => {
            let c = &rev[32 - order..];
            for i in order..d.len() {
                let sum = c
                    .iter()
                    .zip(&d[i - order..i])
                    .fold(0i32, |a, (&c, &s)| a.wrapping_add(c.wrapping_mul(s)));
                d[i] = d[i].wrapping_add(sum >> shift);
            }
        }
    }
}

#[inline(always)]
fn narrow<const N: usize>(c: &[i32; N], shift: u32, d: &mut [i32]) {
    for i in N..d.len() {
        let Ok(w) = <&[i32; N]>::try_from(&d[i - N..i]) else {
            return;
        };
        let mut sum = 0i32;
        for (&c, &s) in c.iter().zip(w) {
            sum = sum.wrapping_add(c.wrapping_mul(s));
        }
        d[i] = d[i].wrapping_add(sum >> shift);
    }
}

fn restore_wide(c: &[i32], shift: u32, d: &mut [i32]) {
    let order = c.len();
    for i in order..d.len() {
        let sum = c
            .iter()
            .zip(&d[i - order..i])
            .fold(0i64, |a, (&c, &s)| a.wrapping_add(c as i64 * s as i64));
        d[i] = (d[i] as i64).wrapping_add(sum >> shift) as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lpc_reference(qlp: &[i32], shift: u32, d: &mut [i32]) {
        let order = qlp.len();
        for i in order..d.len() {
            let mut sum = 0i64;
            for (j, &c) in qlp.iter().enumerate() {
                sum += c as i64 * d[i - 1 - j] as i64;
            }
            d[i] = (d[i] as i64 + (sum >> shift)) as i32;
        }
    }

    fn reversed(qlp: &[i32]) -> [i32; 32] {
        let mut rev = [0i32; 32];
        for (r, &c) in rev[32 - qlp.len()..].iter_mut().zip(qlp.iter().rev()) {
            *r = c;
        }
        rev
    }

    #[test]
    fn fixed_restore_matches_claxon_vectors() {
        let mut d = [
            -729, -722, -667, -19, -16, 17, -23, -7, 16, -16, -5, 3, -8, -13, -15, -1,
        ];
        restore_fixed(3, &mut d);
        assert_eq!(
            d,
            [
                -729, -722, -667, -583, -486, -359, -225, -91, 59, 209, 354, 497, 630, 740, 812,
                845
            ]
        );
        let mut d = [21877, 27482, -6513];
        restore_fixed(2, &mut d);
        assert_eq!(d, [21877, 27482, 26574]);
    }

    #[test]
    fn fixed_orders_follow_their_polynomials() {
        let signal: Vec<i32> = (0..64)
            .map(|i: i32| (i * i * 7 - i * 300 + 11) % 5000)
            .collect();
        for order in 0..=4usize {
            let mut res = signal.clone();
            for _ in 0..order {
                for i in (1..res.len()).rev() {
                    res[i] -= res[i - 1];
                }
            }
            let mut d = signal[..order].to_vec();
            d.extend_from_slice(&res[order..]);
            restore_fixed(order, &mut d);
            assert_eq!(d, signal, "order {order}");
        }
    }

    #[test]
    fn lpc_restore_matches_claxon_vectors() {
        let qlp = [1042, -399, -75, -269, 121, 166, -75];
        let mut d = [
            -796, -547, -285, -32, 199, 443, 670, -2, -23, 14, 6, 3, -4, 12, -2, 10,
        ];
        restore_narrow(&reversed(&qlp), qlp.len(), 9, &mut d);
        assert_eq!(
            d,
            [
                -796, -547, -285, -32, 199, 443, 670, 875, 1046, 1208, 1343, 1454, 1541, 1616,
                1663, 1701
            ]
        );
        let qlp = [
            11550, -15417, 13265, -6873, 182, 4373, -5543, 3904, -645, -3775, 8586, -12129, 12671,
            -9743, 4220, 1350, -4612, 4600, -2589, 709,
        ];
        let mut d = [
            213238, 210830, 234493, 209515, 235139, 201836, 208151, 186277, 157720, 148176, 115037,
            104836, 60794, 54523, 412, 17943, -6025, -3713, 8373, 11764, 30094,
        ];
        let rev = reversed(&qlp);
        restore_wide(&rev[32 - qlp.len()..], 12, &mut d);
        assert_eq!(d[20], 33931);
    }

    #[test]
    fn every_order_and_path_equals_the_reference() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for order in 1..=32usize {
            let bound = (4096 / order) as u64;
            let qlp: Vec<i32> = (0..order)
                .map(|_| (next() % (2 * bound + 1)) as i32 - bound as i32)
                .collect();
            let base: Vec<i32> = (0..200).map(|_| (next() % 65536) as i32 - 32768).collect();
            let mut want = base.clone();
            lpc_reference(&qlp, 13, &mut want);
            let rev = reversed(&qlp);
            let mut a = base.clone();
            restore_narrow(&rev, order, 13, &mut a);
            let mut b = base.clone();
            restore_wide(&rev[32 - order..], 13, &mut b);
            assert_eq!(b, want, "wide, order {order}");
            assert_eq!(a, want, "narrow, order {order}");
        }
    }

    #[test]
    fn silog2_follows_libflac() {
        assert_eq!(silog2(0), 0);
        assert_eq!(silog2(1), 2);
        assert_eq!(silog2(2), 3);
        assert_eq!(silog2(3), 3);
        assert_eq!(silog2(4), 4);
        assert_eq!(silog2(8), 5);
        assert_eq!(silog2(1 << 31), 33);
        assert_eq!(silog2((1 << 31) - 1), 32);
    }
}
