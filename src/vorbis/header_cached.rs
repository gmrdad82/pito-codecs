use std::f32::consts::PI;

#[derive(Clone, Debug)]
pub struct TwiddleFactors {
    pub a: Vec<f32>,
    pub b: Vec<f32>,
    pub c: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct CachedBlocksizeDerived {
    pub twiddle_factors: TwiddleFactors,
    pub window_slope: Vec<f32>,
    pub bitrev: Vec<u32>,
}

impl CachedBlocksizeDerived {
    pub fn from_blocksize(bs: u8) -> Self {
        CachedBlocksizeDerived {
            window_slope: generate_window((1u16 << bs) >> 1),
            twiddle_factors: compute_twiddle_factors(bs),
            bitrev: compute_bitreverse(bs),
        }
    }
}

fn win_slope(x: u16, n: u16) -> f32 {
    let v = (0.5 * PI * (x as f32 + 0.5) / n as f32).sin();
    (0.5 * PI * v * v).sin()
}

fn generate_window(n: u16) -> Vec<f32> {
    (0..n).map(|i| win_slope(i, n)).collect()
}

fn compute_twiddle_factors(blocksize: u8) -> TwiddleFactors {
    let n = 1usize << blocksize;
    let n2 = n >> 1;
    let n4 = n >> 2;
    let n8 = n >> 3;

    let mut a = Vec::with_capacity(n2);
    let mut b = Vec::with_capacity(n2);
    let mut c = Vec::with_capacity(n4);

    let pi_4_n = 4.0 * PI / (n as f32);
    let pi_05_n = 0.5 * PI / (n as f32);
    let pi_2_n = 2.0 * PI / (n as f32);

    let mut k2 = 0;
    for k in 0..n4 {
        a.push(f32::cos((k as f32) * pi_4_n));
        a.push(-f32::sin((k as f32) * pi_4_n));
        b.push(f32::cos(((k2 + 1) as f32) * pi_05_n) * 0.5);
        b.push(f32::sin(((k2 + 1) as f32) * pi_05_n) * 0.5);
        k2 += 2;
    }
    k2 = 0;
    for _ in 0..n8 {
        c.push(f32::cos(((k2 + 1) as f32) * pi_2_n));
        c.push(-f32::sin(((k2 + 1) as f32) * pi_2_n));
        k2 += 2;
    }
    TwiddleFactors { a, b, c }
}

fn compute_bitreverse(blocksize: u8) -> Vec<u32> {
    let ld = blocksize as u32;
    let n8 = (1usize << blocksize) >> 3;
    (0..n8)
        .map(|i| ((i as u32).reverse_bits() >> (32 - ld + 3)) << 2)
        .collect()
}

#[inline]
fn bark(x: f32) -> f32 {
    13.1 * (0.00074 * x).atan() + 2.24 * (0.0000000185 * x * x).atan() + 0.0001 * x
}

pub fn compute_bark_map_cos_omega(n: u16, floor0_rate: u16, floor0_bark_map_size: u16) -> Vec<f32> {
    let hfl = floor0_rate as f32 / 2.0;
    let hfl_dn = hfl / n as f32;
    let foobar_const_part = floor0_bark_map_size as f32 / bark(hfl);
    let bms_m1 = floor0_bark_map_size as f32 - 1.0;
    let omega_factor = PI / floor0_bark_map_size as f32;
    (0..n)
        .map(|i| {
            let foobar = (bark(i as f32 * hfl_dn) * foobar_const_part).floor();
            (foobar.min(bms_m1) * omega_factor).cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitreverse_matches_stb_vorbis() {
        let cmp_arr: [u32; 32] = [
            0, 64, 32, 96, 16, 80, 48, 112, 8, 72, 40, 104, 24, 88, 56, 120, 4, 68, 36, 100, 20,
            84, 52, 116, 12, 76, 44, 108, 28, 92, 60, 124,
        ];
        assert_eq!(compute_bitreverse(8), cmp_arr);
    }

    #[test]
    fn every_legal_blocksize_derives_tables_of_the_right_lengths() {
        for bs in 6..=13u8 {
            let n = 1usize << bs;
            let d = CachedBlocksizeDerived::from_blocksize(bs);
            assert_eq!(d.window_slope.len(), n / 2, "bs {bs}");
            assert_eq!(d.twiddle_factors.a.len(), n / 2, "bs {bs}");
            assert_eq!(d.twiddle_factors.b.len(), n / 2, "bs {bs}");
            assert_eq!(d.twiddle_factors.c.len(), n / 4, "bs {bs}");
            assert_eq!(d.bitrev.len(), n / 8, "bs {bs}");
            assert!(d.bitrev.iter().all(|&r| (r as usize) < n / 2 && r % 4 == 0));
        }
    }

    #[test]
    fn window_slope_rises_from_zero_to_one_and_is_power_complementary() {
        let w = generate_window(128);
        assert!(w[0] > 0.0 && w[0] < 0.01);
        assert!(w[127] > 0.999 && w[127] <= 1.0);
        assert!(w.windows(2).all(|p| p[0] < p[1]));
        for i in 0..128 {
            let s = w[i] * w[i] + w[127 - i] * w[127 - i];
            assert!((s - 1.0).abs() < 1e-5, "{i}: {s}");
        }
    }

    #[test]
    fn bark_map_stays_inside_cosine_range() {
        let m = compute_bark_map_cos_omega(256, 44100, 256);
        assert_eq!(m.len(), 256);
        assert_eq!(m[0], 1.0);
        assert!(m.iter().all(|v| (-1.0..=1.0).contains(v)));
        assert!(m.windows(2).all(|p| p[1] <= p[0]));
        for (n, rate, bark) in [(64u16, 8000u16, 1u16), (1024, 48000, 512), (128, 1, 64)] {
            let m = compute_bark_map_cos_omega(n, rate, bark);
            assert_eq!(m.len(), n as usize);
            assert!(
                m.iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(v)),
                "{rate} {bark}"
            );
        }
    }
}
