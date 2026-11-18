pub const NONE: u8 = 0;
pub const SUB: u8 = 1;
pub const UP: u8 = 2;
pub const AVG: u8 = 3;
pub const PAETH: u8 = 4;

#[derive(Clone, Copy)]
pub struct Isa {
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    avx2: bool,
}

impl Isa {
    pub fn detect() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Isa {
                avx2: std::arch::is_x86_feature_detected!("avx2"),
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Isa { avx2: false }
    }

    #[cfg(test)]
    pub fn scalar() -> Self {
        Isa { avx2: false }
    }
}

#[inline(always)]
fn sub_into(cur: &[u8], bpp: usize, out: &mut [u8]) {
    let n = cur.len();
    out[..bpp].copy_from_slice(&cur[..bpp]);
    for ((o, &x), &a) in out[bpp..n].iter_mut().zip(&cur[bpp..]).zip(&cur[..n - bpp]) {
        *o = x.wrapping_sub(a);
    }
}

#[inline(always)]
fn up_into(cur: &[u8], prev: &[u8], out: &mut [u8]) {
    for ((o, &x), &b) in out.iter_mut().zip(cur).zip(prev) {
        *o = x.wrapping_sub(b);
    }
}

#[inline(always)]
fn avg_into(cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    let n = cur.len();
    for ((o, &x), &b) in out[..bpp].iter_mut().zip(cur).zip(prev) {
        *o = x.wrapping_sub(b >> 1);
    }
    for (((o, &x), &a), &b) in out[bpp..n]
        .iter_mut()
        .zip(&cur[bpp..])
        .zip(&cur[..n - bpp])
        .zip(&prev[bpp..])
    {
        *o = x.wrapping_sub((a & b).wrapping_add((a ^ b) >> 1));
    }
}

#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a16, b16, c16) = (i16::from(a), i16::from(b), i16::from(c));
    let pa = (b16 - c16).abs();
    let pb = (a16 - c16).abs();
    let pc = (a16 + b16 - c16 - c16).abs();
    let take_a = pa <= pb && pa <= pc;
    let take_b = !take_a && pb <= pc;
    if take_a {
        a
    } else if take_b {
        b
    } else {
        c
    }
}

#[inline(always)]
fn paeth_into(cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    let n = cur.len();
    for ((o, &x), &b) in out[..bpp].iter_mut().zip(cur).zip(prev) {
        *o = x.wrapping_sub(b);
    }
    for ((((o, &x), &a), &b), &c) in out[bpp..n]
        .iter_mut()
        .zip(&cur[bpp..])
        .zip(&cur[..n - bpp])
        .zip(&prev[bpp..])
        .zip(&prev[..n - bpp])
    {
        *o = x.wrapping_sub(paeth(a, b, c));
    }
}

#[inline(always)]
fn cost(row: &[u8]) -> u64 {
    row.chunks(4096)
        .map(|c| {
            c.iter()
                .map(|&b| u32::from((b as i8).unsigned_abs()))
                .sum::<u32>() as u64
        })
        .sum()
}

#[inline(always)]
fn one_body(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    match kind {
        NONE => out.copy_from_slice(cur),
        SUB => sub_into(cur, bpp, out),
        UP => up_into(cur, prev, out),
        AVG => avg_into(cur, prev, bpp, out),
        _ => paeth_into(cur, prev, bpp, out),
    }
}

#[inline(always)]
fn best_body(cur: &[u8], prev: &[u8], bpp: usize, trial: &mut [Vec<u8>; 4], out: &mut [u8]) -> u8 {
    if cur == prev {
        out.fill(0);
        return UP;
    }
    let [t_sub, t_up, t_avg, t_paeth] = trial;
    up_into(cur, prev, t_up);
    let mut best = (cost(t_up), UP);
    sub_into(cur, bpp, t_sub);
    let c = cost(t_sub);
    if c <= best.0 {
        best = (c, SUB);
    }
    avg_into(cur, prev, bpp, t_avg);
    let c = cost(t_avg);
    if c <= best.0 {
        best = (c, AVG);
    }
    paeth_into(cur, prev, bpp, t_paeth);
    let c = cost(t_paeth);
    if c <= best.0 {
        best = (c, PAETH);
    }
    match best.1 {
        NONE => out.copy_from_slice(cur),
        SUB => out.copy_from_slice(t_sub),
        UP => out.copy_from_slice(t_up),
        AVG => out.copy_from_slice(t_avg),
        _ => out.copy_from_slice(t_paeth),
    }
    best.1
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn one_avx2(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    one_body(kind, cur, prev, bpp, out)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn best_avx2(cur: &[u8], prev: &[u8], bpp: usize, trial: &mut [Vec<u8>; 4], out: &mut [u8]) -> u8 {
    best_body(cur, prev, bpp, trial, out)
}

pub fn filter(isa: Isa, kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    if isa.avx2 {
        return unsafe { one_avx2(kind, cur, prev, bpp, out) };
    }
    let _ = isa;
    one_body(kind, cur, prev, bpp, out)
}

pub fn choose(
    isa: Isa,
    cur: &[u8],
    prev: &[u8],
    bpp: usize,
    trial: &mut [Vec<u8>; 4],
    out: &mut [u8],
) -> u8 {
    #[cfg(target_arch = "x86_64")]
    if isa.avx2 {
        return unsafe { best_avx2(cur, prev, bpp, trial, out) };
    }
    let _ = isa;
    best_body(cur, prev, bpp, trial, out)
}

pub fn trials(row: usize) -> [Vec<u8>; 4] {
    [vec![0; row], vec![0; row], vec![0; row], vec![0; row]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow(kind: u8, cur: &[u8], prev: &[u8], bpp: usize) -> Vec<u8> {
        (0..cur.len())
            .map(|i| {
                let a = if i >= bpp { cur[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                let p = match kind {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => {
                        let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
                        let p = a + b - c;
                        let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                        (if pa <= pb && pa <= pc {
                            a
                        } else if pb <= pc {
                            b
                        } else {
                            c
                        }) as u8
                    }
                };
                cur[i].wrapping_sub(p)
            })
            .collect()
    }

    fn rows(seed: u32, n: usize) -> (Vec<u8>, Vec<u8>) {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 24) as u8
        };
        (
            (0..n).map(|_| next()).collect(),
            (0..n).map(|_| next()).collect(),
        )
    }

    #[test]
    fn every_filter_matches_the_definition_at_every_pixel_size_and_isa() {
        for isa in [Isa::scalar(), Isa::detect()] {
            for bpp in [1, 2, 3, 4, 6, 8] {
                for n in [bpp, bpp * 2, bpp * 7, bpp * 37 + bpp, 4096 + bpp * 5] {
                    let (cur, prev) = rows(n as u32 + bpp as u32, n);
                    for kind in 0..5 {
                        let mut out = vec![0; n];
                        filter(isa, kind, &cur, &prev, bpp, &mut out);
                        assert_eq!(out, slow(kind, &cur, &prev, bpp), "{kind} {bpp} {n}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_choice_is_the_cheapest_of_up_sub_average_paeth_and_identical_rows_take_up() {
        for isa in [Isa::scalar(), Isa::detect()] {
            let bpp = 4;
            let n = 400;
            let (cur, prev) = rows(9, n);
            let mut trial = trials(n);
            let mut out = vec![0; n];
            let kind = choose(isa, &cur, &prev, bpp, &mut trial, &mut out);
            let mut want = (u64::MAX, 0u8);
            for k in [UP, SUB, AVG, PAETH] {
                let c = cost(&slow(k, &cur, &prev, bpp));
                if c <= want.0 {
                    want = (c, k);
                }
            }
            let want = Some(want.1);
            assert_eq!(Some(kind), want);
            assert_eq!(out, slow(kind, &cur, &prev, bpp));
            let kind = choose(isa, &cur, &cur, bpp, &mut trial, &mut out);
            assert_eq!(kind, UP);
            assert!(out.iter().all(|&b| b == 0));
        }
    }
}
