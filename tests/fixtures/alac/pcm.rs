fn step(s: &mut u64) -> i64 {
    *s = s
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*s >> 40) as i64 - (1 << 23)
}

fn triangle(i: usize, period: i64, amp: i64) -> i64 {
    let p = i as i64 % period;
    let x = if p < period / 2 { p } else { period - p };
    (4 * x - period) * amp / period
}

pub fn source(channels: usize, depth: u32, frames: usize, salt: u64) -> Vec<i32> {
    let max = (1i64 << (depth - 1)) - 1;
    let min = -(1i64 << (depth - 1));
    let quiet = 24 - (depth as i64 - 10);
    let mut s = 0x2545_f491_4f6c_dd1d_u64 ^ salt.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        let section = (i / 1536 + salt as usize) % 6;
        for c in 0..channels {
            let noise = step(&mut s);
            let c64 = c as i64;
            let v = match section {
                0 => triangle(i, 181, max * 5 / 8) * (8 - c64) / 8 + (noise >> quiet),
                1 => 0,
                2 => triangle(i, 97, max / 2),
                3 => triangle(i, 150 + 61 * c64, max / 3) + (noise >> quiet),
                4 => {
                    if (i as i64 / (40 + 7 * c64)) % 2 == 0 {
                        max * 3 / 4
                    } else {
                        min * 3 / 4
                    }
                }
                _ => noise >> 20,
            };
            let v = match i % 1013 {
                0 => max,
                1 => min,
                _ => v.clamp(min, max),
            };
            out.push(v as i32);
        }
    }
    out
}

pub fn fnv(samples: &[i32]) -> u64 {
    samples
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        })
}
