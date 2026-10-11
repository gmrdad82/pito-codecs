use crate::hevc::pel::Pel;
use crate::hevc::spec_tables::{INTRA_PRED_ANGLE, INV_ANGLE};

#[derive(Clone)]
pub struct Refs {
    pub left: [u16; 129],
    pub top: [u16; 129],
}

impl Default for Refs {
    fn default() -> Self {
        Refs {
            left: [0; 129],
            top: [0; 129],
        }
    }
}

crate::hevc::isa::dispatch! {
pub fn substitute(
    r: &mut Refs,
    avail_left: &[bool; 129],
    avail_top: &[bool; 129],
    n: usize,
    bit_depth: u32,
) {
    let n2 = 2 * n;
    let any = avail_left[..=n2].iter().any(|&a| a) || avail_top[1..=n2].iter().any(|&a| a);
    if !any {
        let v = 1u16 << (bit_depth - 1);
        r.left[..=n2].fill(v);
        r.top[..=n2].fill(v);
        return;
    }
    if !avail_left[n2] {
        let mut found = None;
        for y in (0..n2).rev() {
            if avail_left[y] {
                found = Some(r.left[y]);
                break;
            }
        }
        if found.is_none() {
            for x in 1..=n2 {
                if avail_top[x] {
                    found = Some(r.top[x]);
                    break;
                }
            }
        }
        r.left[n2] = found.unwrap_or(0);
    }
    for y in (0..n2).rev() {
        if !avail_left[y] {
            r.left[y] = r.left[y + 1];
        }
    }
    r.top[0] = r.left[0];
    for x in 1..=n2 {
        if !avail_top[x] {
            r.top[x] = r.top[x - 1];
        }
    }
}
}

crate::hevc::isa::dispatch! {
pub fn filter(
    r: &mut Refs,
    mode: u32,
    n: usize,
    strong_enabled: bool,
    is_luma: bool,
    bit_depth: u32,
) {
    if mode == 1 || n == 4 {
        return;
    }
    let min_dist = (mode as i32 - 26).abs().min((mode as i32 - 10).abs());
    let thres = match n {
        8 => 7,
        16 => 1,
        _ => 0,
    };
    if min_dist <= thres {
        return;
    }
    let n2 = 2 * n;
    let c = r.left[0] as i32;
    if strong_enabled && is_luma && n == 32 {
        let (bl, tr) = (r.left[n2] as i32, r.top[n2] as i32);
        let thr = 1 << (bit_depth - 5);
        if (c + tr - 2 * r.top[n] as i32).abs() < thr && (c + bl - 2 * r.left[n] as i32).abs() < thr
        {
            for i in 0..63 {
                r.left[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * bl + 32) >> 6) as u16;
                r.top[1 + i] = (((63 - i as i32) * c + (i as i32 + 1) * tr + 32) >> 6) as u16;
            }
            return;
        }
    }
    let (l, t) = (r.left, r.top);
    let corner = ((l[1] as u32 + 2 * c as u32 + t[1] as u32 + 2) >> 2) as u16;
    for i in 1..n2 {
        r.left[i] = ((l[i - 1] as u32 + 2 * l[i] as u32 + l[i + 1] as u32 + 2) >> 2) as u16;
        r.top[i] = ((t[i - 1] as u32 + 2 * t[i] as u32 + t[i + 1] as u32 + 2) >> 2) as u16;
    }
    r.left[0] = corner;
    r.top[0] = corner;
}
}

crate::hevc::isa::dispatch! {
pub fn predict<P: Pel>(
    r: &Refs,
    mode: u32,
    n: usize,
    edge_filters: bool,
    bit_depth: u32,
    out: &mut [P],
    os: usize,
) {
    let max = (1i32 << bit_depth) - 1;
    match mode {
        0 => {
            let log2 = n.trailing_zeros();
            let tr = r.top[1 + n] as i32;
            let bl = r.left[1 + n] as i32;
            for y in 0..n {
                let l = r.left[1 + y] as i32;
                for x in 0..n {
                    let t = r.top[1 + x] as i32;
                    let v = ((n - 1 - x) as i32 * l
                        + (x as i32 + 1) * tr
                        + (n - 1 - y) as i32 * t
                        + (y as i32 + 1) * bl
                        + n as i32)
                        >> (log2 + 1);
                    out[y * os + x] = P::from_i32(v);
                }
            }
        }
        1 => {
            let log2 = n.trailing_zeros();
            let sum: u32 = r.top[1..=n].iter().map(|&v| v as u32).sum::<u32>()
                + r.left[1..=n].iter().map(|&v| v as u32).sum::<u32>();
            let dc = ((sum + n as u32) >> (log2 + 1)) as u16;
            for y in 0..n {
                out[y * os..y * os + n].fill(P::from_i32(dc as i32));
            }
            if edge_filters && n < 32 {
                let d = dc as u32;
                out[0] = P::from_i32(((r.left[1] as u32 + 2 * d + r.top[1] as u32 + 2) >> 2) as i32);
                for x in 1..n {
                    out[x] = P::from_i32(((r.top[1 + x] as u32 + 3 * d + 2) >> 2) as i32);
                }
                for y in 1..n {
                    out[y * os] = P::from_i32(((r.left[1 + y] as u32 + 3 * d + 2) >> 2) as i32);
                }
            }
        }
        _ => {
            let angle = INTRA_PRED_ANGLE[mode as usize] as i32;
            let mut refa = [0i32; 3 * 64 + 1];
            let base = n as i32;
            let (main, side) = if mode >= 18 {
                (&r.top, &r.left)
            } else {
                (&r.left, &r.top)
            };
            for x in 0..=n {
                refa[(base + x as i32) as usize] = main[x] as i32;
            }
            if angle < 0 {
                let lim = (n as i32 * angle) >> 5;
                if lim < -1 {
                    let inv = INV_ANGLE[(mode - 11) as usize] as i32;
                    for x in lim..=-1 {
                        let idx = -1 + ((x * inv + 128) >> 8);
                        refa[(base + x) as usize] = side[(idx + 1) as usize] as i32;
                    }
                }
            } else {
                for x in n + 1..=2 * n {
                    refa[(base + x as i32) as usize] = main[x] as i32;
                }
            }
            let horizontal = mode < 18;
            for j in 0..n {
                let pos = (j as i32 + 1) * angle;
                let idx = pos >> 5;
                let fact = pos & 31;
                for i in 0..n {
                    let k = (base + i as i32 + idx + 1) as usize;
                    let v = if fact != 0 {
                        ((32 - fact) * refa[k] + fact * refa[k + 1] + 16) >> 5
                    } else {
                        refa[k]
                    };
                    if horizontal {
                        out[i * os + j] = P::from_i32(v);
                    } else {
                        out[j * os + i] = P::from_i32(v);
                    }
                }
            }
            if edge_filters && n < 32 {
                if mode == 26 {
                    for y in 0..n {
                        let v = r.top[1] as i32 + ((r.left[1 + y] as i32 - r.left[0] as i32) >> 1);
                        out[y * os] = P::from_i32(v.clamp(0, max));
                    }
                } else if mode == 10 {
                    for x in 0..n {
                        let v = r.left[1] as i32 + ((r.top[1 + x] as i32 - r.top[0] as i32) >> 1);
                        out[x] = P::from_i32(v.clamp(0, max));
                    }
                }
            }
        }
    }
}
}
