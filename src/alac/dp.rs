#[inline(always)]
fn sext(v: i32, chanshift: u32) -> i32 {
    v.wrapping_shl(chanshift).wrapping_shr(chanshift)
}

pub fn first_order(buf: &mut [i32], num: usize, chanbits: u32) {
    let chanshift = 32 - chanbits;
    let len = num.min(buf.len());
    let buf = &mut buf[..len];
    let Some((&mut first, rest)) = buf.split_first_mut() else {
        return;
    };
    let mut prev = first;
    for v in rest {
        prev = sext(v.wrapping_add(prev), chanshift);
        *v = prev;
    }
}

pub fn unpc_block(
    pc1: &[i32],
    out: &mut [i32],
    num: usize,
    coefs: &mut [i16],
    numactive: usize,
    chanbits: u32,
    denshift: u32,
) {
    let num = num.min(pc1.len()).min(out.len());
    if num == 0 {
        return;
    }
    let chanshift = 32 - chanbits;
    out[0] = pc1[0];
    if numactive == 0 {
        out[1..num].copy_from_slice(&pc1[1..num]);
        return;
    }
    if numactive == 31 {
        let mut prev = out[0];
        for j in 1..num {
            prev = sext(pc1[j].wrapping_add(prev), chanshift);
            out[j] = prev;
        }
        return;
    }
    for j in 1..=numactive.min(num - 1) {
        out[j] = sext(pc1[j].wrapping_add(out[j - 1]), chanshift);
    }
    let denhalf = 1i32.wrapping_shl(denshift.wrapping_sub(1));
    match numactive {
        4 => fixed::<4>(pc1, out, num, coefs, chanshift, denshift, denhalf),
        8 => fixed::<8>(pc1, out, num, coefs, chanshift, denshift, denhalf),
        _ => general(
            pc1, out, num, coefs, numactive, chanshift, denshift, denhalf,
        ),
    }
}

#[inline(always)]
pub fn fixed<const N: usize>(
    pc1: &[i32],
    out: &mut [i32],
    num: usize,
    coefs: &mut [i16],
    chanshift: u32,
    denshift: u32,
    denhalf: i32,
) {
    let Some(c) = coefs.get_mut(..N) else {
        return;
    };
    let mut a = [0i16; N];
    a.copy_from_slice(c);
    let lim = N + 1;
    for j in lim..num {
        let top = out[j - lim];
        let mut b = [0i32; N];
        for (k, bk) in b.iter_mut().enumerate() {
            *bk = top.wrapping_sub(out[j - 1 - k]);
        }
        let mut sum = denhalf;
        for (&ak, &bk) in a.iter().zip(&b) {
            sum = sum.wrapping_sub((ak as i32).wrapping_mul(bk));
        }
        let sum1 = sum.wrapping_shr(denshift);
        let del = pc1[j];
        let mut del0 = del;
        let sg = del.signum();
        out[j] = sext(del.wrapping_add(top).wrapping_add(sum1), chanshift);
        if sg > 0 {
            for k in (0..N).rev() {
                let sgn = b[k].signum();
                a[k] = a[k].wrapping_sub(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((N - k) as i32).wrapping_mul(sgn.wrapping_mul(b[k]).wrapping_shr(denshift)),
                );
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..N).rev() {
                let sgn = b[k].signum().wrapping_neg();
                a[k] = a[k].wrapping_sub(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((N - k) as i32).wrapping_mul(sgn.wrapping_mul(b[k]).wrapping_shr(denshift)),
                );
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
    c.copy_from_slice(&a);
}

#[allow(clippy::too_many_arguments)]
pub fn general(
    pc1: &[i32],
    out: &mut [i32],
    num: usize,
    coefs: &mut [i16],
    numactive: usize,
    chanshift: u32,
    denshift: u32,
    denhalf: i32,
) {
    let Some(coefs) = coefs.get_mut(..numactive) else {
        return;
    };
    let lim = numactive + 1;
    for j in lim..num {
        let top = out[j - lim];
        let mut sum1 = 0i32;
        for (k, &c) in coefs.iter().enumerate() {
            sum1 = sum1.wrapping_add((c as i32).wrapping_mul(out[j - 1 - k].wrapping_sub(top)));
        }
        let del = pc1[j];
        let mut del0 = del;
        let sg = del.signum();
        let del = del
            .wrapping_add(top)
            .wrapping_add(sum1.wrapping_add(denhalf).wrapping_shr(denshift));
        out[j] = sext(del, chanshift);
        if sg > 0 {
            for k in (0..numactive).rev() {
                let dd = top.wrapping_sub(out[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] = coefs[k].wrapping_sub(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((numactive - k) as i32)
                        .wrapping_mul(sgn.wrapping_mul(dd).wrapping_shr(denshift)),
                );
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for k in (0..numactive).rev() {
                let dd = top.wrapping_sub(out[j - 1 - k]);
                let sgn = dd.signum();
                coefs[k] = coefs[k].wrapping_add(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((numactive - k) as i32)
                        .wrapping_mul(sgn.wrapping_neg().wrapping_mul(dd).wrapping_shr(denshift)),
                );
                if del0 >= 0 {
                    break;
                }
            }
        }
    }
}
