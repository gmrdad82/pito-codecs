use super::header_cached::CachedBlocksizeDerived;

fn step3_iter0_loop(n: usize, e: &mut [f32], i_off: usize, k_off: isize, a: &[f32]) {
    let mut a_offs = 0;
    let mut i0 = i_off;
    let mut k0 = (i_off as isize + k_off) as usize;
    for _ in 0..n >> 2 {
        for s in 0..4 {
            let d = 2 * s;
            let k00_20 = e[i0 - d] - e[k0 - d];
            let k01_21 = e[i0 - d - 1] - e[k0 - d - 1];
            e[i0 - d] += e[k0 - d];
            e[i0 - d - 1] += e[k0 - d - 1];
            e[k0 - d] = k00_20 * a[a_offs] - k01_21 * a[a_offs + 1];
            e[k0 - d - 1] = k01_21 * a[a_offs] + k00_20 * a[a_offs + 1];
            a_offs += 8;
        }
        i0 -= 8;
        k0 = k0.wrapping_sub(8);
    }
}

fn step3_inner_r_loop(lim: usize, e: &mut [f32], d0: usize, k_off: isize, a: &[f32], k1: usize) {
    let mut a_offs = 0;
    let mut i0 = d0;
    let mut k0 = (d0 as isize + k_off) as usize;
    for _ in 0..lim >> 2 {
        for s in 0..4 {
            let d = 2 * s;
            let k00_20 = e[i0 - d] - e[k0 - d];
            let k01_21 = e[i0 - d - 1] - e[k0 - d - 1];
            e[i0 - d] += e[k0 - d];
            e[i0 - d - 1] += e[k0 - d - 1];
            e[k0 - d] = k00_20 * a[a_offs] - k01_21 * a[a_offs + 1];
            e[k0 - d - 1] = k01_21 * a[a_offs] + k00_20 * a[a_offs + 1];
            a_offs += k1;
        }
        i0 = i0.wrapping_sub(8);
        k0 = k0.wrapping_sub(8);
    }
}

#[allow(clippy::too_many_arguments)]
fn step3_inner_s_loop(
    n: usize,
    e: &mut [f32],
    i_off: usize,
    k_off: isize,
    a: &[f32],
    a_off: usize,
    k0: usize,
) {
    let tw = [
        (a[0], a[1]),
        (a[a_off], a[a_off + 1]),
        (a[a_off * 2], a[a_off * 2 + 1]),
        (a[a_off * 3], a[a_off * 3 + 1]),
    ];
    let mut i0 = i_off;
    let mut k = (i_off as isize + k_off) as usize;
    let mut i = 0;
    loop {
        for (s, &(c0, c1)) in tw.iter().enumerate() {
            let d = 2 * s;
            let k00 = e[i0 - d] - e[k - d];
            let k11 = e[i0 - d - 1] - e[k - d - 1];
            e[i0 - d] += e[k - d];
            e[i0 - d - 1] += e[k - d - 1];
            e[k - d] = k00 * c0 - k11 * c1;
            e[k - d - 1] = k11 * c0 + k00 * c1;
        }
        i += 1;
        if i >= n {
            break;
        }
        i0 -= k0;
        k -= k0;
    }
}

fn iter_54(z: &mut [f32]) {
    let k00 = z[7] - z[3];
    let y0 = z[7] + z[3];
    let y2 = z[5] + z[1];
    let k22 = z[5] - z[1];
    z[7] = y0 + y2;
    z[5] = y0 - y2;
    let k33 = z[4] - z[0];
    z[3] = k00 + k33;
    z[1] = k00 - k33;
    let k11 = z[6] - z[2];
    let y1 = z[6] + z[2];
    let y3 = z[4] + z[0];
    z[6] = y1 + y3;
    z[4] = y1 - y3;
    z[2] = k11 - k22;
    z[0] = k11 + k22;
}

fn step3_inner_s_loop_ld654(n: usize, e: &mut [f32], i_off: usize, a: &[f32], base_n: usize) {
    let a2 = a[base_n >> 3];
    let mut z = i_off;
    let base = i_off - 16 * (n - 1);
    loop {
        let mut k00 = e[z] - e[z - 8];
        let mut k11 = e[z - 1] - e[z - 9];
        e[z] += e[z - 8];
        e[z - 1] += e[z - 9];
        e[z - 8] = k00;
        e[z - 9] = k11;

        k00 = e[z - 2] - e[z - 10];
        k11 = e[z - 3] - e[z - 11];
        e[z - 2] += e[z - 10];
        e[z - 3] += e[z - 11];
        e[z - 10] = (k00 + k11) * a2;
        e[z - 11] = (k11 - k00) * a2;

        k00 = e[z - 12] - e[z - 4];
        k11 = e[z - 5] - e[z - 13];
        e[z - 4] += e[z - 12];
        e[z - 5] += e[z - 13];
        e[z - 12] = k11;
        e[z - 13] = k00;

        k00 = e[z - 14] - e[z - 6];
        k11 = e[z - 7] - e[z - 15];
        e[z - 6] += e[z - 14];
        e[z - 7] += e[z - 15];
        e[z - 14] = (k00 + k11) * a2;
        e[z - 15] = (k00 - k11) * a2;

        iter_54(&mut e[z - 7..]);
        iter_54(&mut e[z - 15..]);
        if z <= base {
            break;
        }
        z -= 16;
    }
}

pub fn inverse_mdct(
    cached: &CachedBlocksizeDerived,
    buffer: &mut [f32],
    buf2: &mut Vec<f32>,
    bs: u8,
) {
    if bs < FAST_FROM {
        direct_imdct(buffer, buf2);
    } else {
        fast_imdct(cached, buffer, buf2, bs);
    }
}

const FAST_FROM: u8 = 8;

fn direct_imdct(buffer: &mut [f32], buf2: &mut Vec<f32>) {
    let n = buffer.len();
    let n2 = n >> 1;
    let n4 = n >> 2;
    let n3_4 = n - n4;
    let step = std::f64::consts::FRAC_PI_4 / n2 as f64;
    buf2.clear();
    buf2.extend((0..n2).map(|i| {
        buffer[..n2]
            .iter()
            .enumerate()
            .map(|(j, &x)| x as f64 * (step * ((2 * i + 1) * (2 * j + 1)) as f64).cos())
            .sum::<f64>() as f32
    }));
    for (i, o) in buffer.iter_mut().enumerate() {
        *o = if i < n4 {
            buf2[i + n4]
        } else if i < n3_4 {
            -buf2[n3_4 - i - 1]
        } else {
            -buf2[i - n3_4]
        };
    }
}

fn fast_imdct(cached: &CachedBlocksizeDerived, buffer: &mut [f32], buf2: &mut Vec<f32>, bs: u8) {
    let n = buffer.len();
    let n2 = n >> 1;
    let n4 = n >> 2;
    let n8 = n >> 3;
    buf2.clear();
    buf2.resize(n2, 0.0);

    let a: &[f32] = &cached.twiddle_factors.a;
    let b: &[f32] = &cached.twiddle_factors.b;
    let c: &[f32] = &cached.twiddle_factors.c;

    {
        let mut a_offs = 0;
        let mut d = n2 - 2;
        let mut e = 0;
        while e != n2 {
            buf2[d + 1] = buffer[e] * a[a_offs] - buffer[e + 2] * a[a_offs + 1];
            buf2[d] = buffer[e] * a[a_offs + 1] + buffer[e + 2] * a[a_offs];
            d -= 2;
            a_offs += 2;
            e += 4;
        }
        e = n2 - 3;
        loop {
            buf2[d + 1] = -buffer[e + 2] * a[a_offs] + buffer[e] * a[a_offs + 1];
            buf2[d] = -buffer[e + 2] * a[a_offs + 1] - buffer[e] * a[a_offs];
            match d.checked_sub(2) {
                Some(v) => d = v,
                None => break,
            }
            a_offs += 2;
            e = e.wrapping_sub(4);
        }
    }

    {
        let u = &mut *buffer;
        let v = &mut buf2[..];
        {
            let mut a_offs = n2 - 8;
            let mut d0 = n4;
            let mut d1 = 0;
            let mut e0 = n4;
            let mut e1 = 0;
            loop {
                let mut v41_21 = v[e0 + 1] - v[e1 + 1];
                let mut v40_20 = v[e0] - v[e1];
                u[d0 + 1] = v[e0 + 1] + v[e1 + 1];
                u[d0] = v[e0] + v[e1];
                u[d1 + 1] = v41_21 * a[a_offs + 4] - v40_20 * a[a_offs + 5];
                u[d1] = v40_20 * a[a_offs + 4] + v41_21 * a[a_offs + 5];

                v41_21 = v[e0 + 3] - v[e1 + 3];
                v40_20 = v[e0 + 2] - v[e1 + 2];
                u[d0 + 3] = v[e0 + 3] + v[e1 + 3];
                u[d0 + 2] = v[e0 + 2] + v[e1 + 2];
                u[d1 + 3] = v41_21 * a[a_offs] - v40_20 * a[a_offs + 1];
                u[d1 + 2] = v40_20 * a[a_offs] + v41_21 * a[a_offs + 1];

                match a_offs.checked_sub(8) {
                    Some(x) => a_offs = x,
                    None => break,
                }
                d0 += 4;
                d1 += 4;
                e0 += 4;
                e1 += 4;
            }
        }

        let ld = bs as usize;
        let ni = n as isize;
        step3_iter0_loop(n >> 4, u, n2 - 1, -(ni >> 3), a);
        step3_iter0_loop(n >> 4, u, n2 - 1 - n4, -(ni >> 3), a);

        for k in 0..4 {
            step3_inner_r_loop(n >> 5, u, n2 - 1 - n8 * k, -(ni >> 4), a, 16);
        }

        for l in 2..(ld - 3) >> 1 {
            let k0 = n >> (l + 2);
            let k0_2 = k0 as isize >> 1;
            let lim = 1 << (l + 1);
            for i in 0..lim {
                step3_inner_r_loop(n >> (l + 4), u, n2 - 1 - k0 * i, -k0_2, a, 1 << (l + 3));
            }
        }
        for l in (ld - 3) >> 1..ld - 6 {
            let k0 = n >> (l + 2);
            let k1 = 1 << (l + 3);
            let k0_2 = k0 as isize >> 1;
            let rlim = n >> (l + 6);
            let lim = 1 << (l + 1);
            let mut i_off = n2 - 1;
            let mut a_off = 0;
            for _ in 0..rlim {
                step3_inner_s_loop(lim, u, i_off, -k0_2, &a[a_off..], k1, k0);
                a_off += k1 * 4;
                i_off -= 8;
            }
        }

        step3_inner_s_loop_ld654(n >> 5, u, n2 - 1, a, n);

        {
            let bitrev = &cached.bitrev;
            let mut d0 = n4 - 4;
            let mut d1 = n2 - 4;
            let mut br = 0;
            loop {
                let mut k4 = bitrev[br] as usize;
                v[d1 + 3] = u[k4];
                v[d1 + 2] = u[k4 + 1];
                v[d0 + 3] = u[k4 + 2];
                v[d0 + 2] = u[k4 + 3];

                k4 = bitrev[br + 1] as usize;
                v[d1 + 1] = u[k4];
                v[d1] = u[k4 + 1];
                v[d0 + 1] = u[k4 + 2];
                v[d0] = u[k4 + 3];

                match d0.checked_sub(4) {
                    Some(x) => d0 = x,
                    None => break,
                }
                d1 -= 4;
                br += 2;
            }
        }

        {
            let mut c_offs = 0;
            let mut d = 0;
            let mut e = n2 - 4;
            while d < e {
                let mut a02 = v[d] - v[e + 2];
                let mut a11 = v[d + 1] + v[e + 3];
                let mut b0 = c[c_offs + 1] * a02 + c[c_offs] * a11;
                let mut b1 = c[c_offs + 1] * a11 - c[c_offs] * a02;
                let mut b2 = v[d] + v[e + 2];
                let mut b3 = v[d + 1] - v[e + 3];
                v[d] = b2 + b0;
                v[d + 1] = b3 + b1;
                v[e + 2] = b2 - b0;
                v[e + 3] = b1 - b3;

                a02 = v[d + 2] - v[e];
                a11 = v[d + 3] + v[e + 1];
                b0 = c[c_offs + 3] * a02 + c[c_offs + 2] * a11;
                b1 = c[c_offs + 3] * a11 - c[c_offs + 2] * a02;
                b2 = v[d + 2] + v[e];
                b3 = v[d + 3] - v[e + 1];
                v[d + 2] = b2 + b0;
                v[d + 3] = b3 + b1;
                v[e] = b2 - b0;
                v[e + 1] = b1 - b3;

                c_offs += 4;
                d += 4;
                e -= 4;
            }
        }
    }

    {
        let mut d0 = 0;
        let mut d1 = n2 - 4;
        let mut d2 = n2;
        let mut d3 = n - 4;
        let mut bo = n2 - 8;
        let mut e = n2 - 8;
        loop {
            for s in 0..4 {
                let j = 6 - 2 * s;
                let p_hi = buf2[e + j] * b[bo + j + 1] - buf2[e + j + 1] * b[bo + j];
                let p_lo = -buf2[e + j] * b[bo + j] - buf2[e + j + 1] * b[bo + j + 1];
                buffer[d0 + s] = p_hi;
                buffer[d1 + 3 - s] = -p_hi;
                buffer[d2 + s] = p_lo;
                buffer[d3 + 3 - s] = p_lo;
            }
            match e.checked_sub(8) {
                Some(x) => e = x,
                None => break,
            }
            bo -= 8;
            d0 += 4;
            d2 += 4;
            d1 -= 4;
            d3 -= 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slow(spectrum: &[f32]) -> Vec<f32> {
        let n2 = spectrum.len();
        let n = n2 * 2;
        let n4 = n >> 2;
        let n3_4 = n - n4;
        let x: Vec<f64> = spectrum.iter().map(|&v| v as f64).collect();
        let dct: Vec<f64> = (0..n2)
            .map(|i| {
                (0..n2)
                    .map(|j| {
                        let arg = std::f64::consts::FRAC_PI_4 * ((2 * i + 1) * (2 * j + 1)) as f64
                            / n2 as f64;
                        x[j] * arg.cos()
                    })
                    .sum()
            })
            .collect();
        let mut out = vec![0.0; n];
        for (i, o) in out.iter_mut().enumerate() {
            *o = if i < n4 {
                dct[i + n4]
            } else if i < n3_4 {
                -dct[n3_4 - i - 1]
            } else {
                -dct[i - n3_4]
            } as f32;
        }
        out
    }

    fn spectrum(n2: usize, seed: u64) -> Vec<f32> {
        let mut s = seed;
        (0..n2)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((s >> 40) as f32 / (1u64 << 24) as f32) - 0.5
            })
            .collect()
    }

    #[test]
    fn the_fast_imdct_matches_the_direct_sum_from_where_it_is_used() {
        let mut fast_ok = Vec::new();
        for bs in 6..=11u8 {
            let n = 1usize << bs;
            let cached = CachedBlocksizeDerived::from_blocksize(bs);
            let spec = spectrum(n / 2, bs as u64);
            let want = slow(&spec);
            let mut buf = spec.clone();
            buf.resize(n, 0.0);
            fast_imdct(&cached, &mut buf, &mut Vec::new(), bs);
            let peak = want.iter().fold(0f32, |m, v| m.max(v.abs()));
            let worst = buf
                .iter()
                .zip(&want)
                .fold(0f32, |m, (g, w)| m.max((g - w).abs()));
            eprintln!("fast imdct bs {bs}: worst {worst:e}, peak {peak}");
            if worst <= 2e-5 * peak.max(1.0) {
                fast_ok.push(bs);
            }
        }
        assert_eq!(fast_ok, (FAST_FROM..=11).collect::<Vec<_>>());
    }

    #[test]
    fn the_imdct_matches_the_direct_sum_at_every_vorbis_blocksize() {
        let mut scratch = Vec::new();
        for bs in 6..=11u8 {
            let n = 1usize << bs;
            let cached = CachedBlocksizeDerived::from_blocksize(bs);
            let spec = spectrum(n / 2, bs as u64);
            let want = slow(&spec);
            let mut buf = spec.clone();
            buf.resize(n, 0.0);
            inverse_mdct(&cached, &mut buf, &mut scratch, bs);
            let peak = want.iter().fold(0f32, |m, v| m.max(v.abs()));
            for (i, (g, w)) in buf.iter().zip(&want).enumerate() {
                assert!(
                    (g - w).abs() <= 2e-5 * peak.max(1.0),
                    "bs {bs} sample {i}: {g} vs {w}"
                );
            }
        }
    }

    #[test]
    fn the_largest_blocksizes_run_and_stay_finite() {
        let mut scratch = Vec::new();
        for bs in 12..=13u8 {
            let n = 1usize << bs;
            let cached = CachedBlocksizeDerived::from_blocksize(bs);
            let mut buf = spectrum(n / 2, 7);
            buf.resize(n, 0.0);
            inverse_mdct(&cached, &mut buf, &mut scratch, bs);
            assert!(buf.iter().all(|v| v.is_finite()), "bs {bs}");
            assert!(buf.iter().any(|v| *v != 0.0), "bs {bs}");
        }
    }

    #[test]
    fn a_single_bin_gives_a_cosine() {
        let bs = 8u8;
        let n = 1usize << bs;
        let cached = CachedBlocksizeDerived::from_blocksize(bs);
        let mut spec = vec![0.0f32; n / 2];
        spec[3] = 1.0;
        let want = slow(&spec);
        let mut buf = spec.clone();
        buf.resize(n, 0.0);
        inverse_mdct(&cached, &mut buf, &mut Vec::new(), bs);
        for (g, w) in buf.iter().zip(&want) {
            assert!((g - w).abs() < 1e-4, "{g} vs {w}");
        }
    }
}
