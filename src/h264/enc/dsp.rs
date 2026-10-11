#[inline]
pub fn sad(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..h {
        let ra = &a[r * sa..r * sa + w];
        let rb = &b[r * sb..r * sb + w];
        s += ra
            .iter()
            .zip(rb)
            .map(|(&x, &y)| x.abs_diff(y) as u32)
            .sum::<u32>();
    }
    s
}

#[inline]
pub fn sad16(a: &[u8], sa: usize, b: &[u8], sb: usize, h: usize) -> u32 {
    let mut s = 0u32;
    for r in 0..h {
        let (Some(ra), Some(rb)) = (
            a.get(r * sa..).and_then(|x| x.first_chunk::<16>()),
            b.get(r * sb..).and_then(|x| x.first_chunk::<16>()),
        ) else {
            break;
        };
        let mut acc = 0u16;
        for i in 0..16 {
            acc += ra[i].abs_diff(rb[i]) as u16;
        }
        s += acc as u32;
    }
    s
}

#[inline(always)]
fn satd_strip<const N: usize, const H: usize>(a: &[u8], sa: usize, b: &[u8], sb: usize) -> u32 {
    let mut d = [[0i16; N]; 4];
    for r in 0..4 {
        let ra = &a[r * sa..r * sa + N];
        let rb = &b[r * sb..r * sb + N];
        for i in 0..N {
            d[r][i] = ra[i] as i16 - rb[i] as i16;
        }
    }
    let mut v = [[0i16; N]; 4];
    for i in 0..N {
        let s01 = d[0][i] + d[1][i];
        let d01 = d[0][i] - d[1][i];
        let s23 = d[2][i] + d[3][i];
        let d23 = d[2][i] - d[3][i];
        v[0][i] = s01 + s23;
        v[1][i] = s01 - s23;
        v[2][i] = d01 + d23;
        v[3][i] = d01 - d23;
    }
    let mut total = 0u32;
    for row in &v {
        let mut hs = [0i16; H];
        let mut hd = [0i16; H];
        for k in 0..H {
            hs[k] = row[2 * k] + row[2 * k + 1];
            hd[k] = row[2 * k] - row[2 * k + 1];
        }
        for k in 0..H / 2 {
            total += hs[2 * k].unsigned_abs().max(hs[2 * k + 1].unsigned_abs()) as u32;
            total += hd[2 * k].unsigned_abs().max(hd[2 * k + 1].unsigned_abs()) as u32;
        }
    }
    total
}

pub fn satd(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u32 {
    let mut total = 0;
    for by in (0..h).step_by(4) {
        let (ao, bo) = (by * sa, by * sb);
        match w {
            16 => total += satd_strip::<16, 8>(&a[ao..], sa, &b[bo..], sb),
            8 => total += satd_strip::<8, 4>(&a[ao..], sa, &b[bo..], sb),
            4 => total += satd_strip::<4, 2>(&a[ao..], sa, &b[bo..], sb),
            _ => {
                for bx in (0..w).step_by(4) {
                    total += satd_strip::<4, 2>(&a[ao + bx..], sa, &b[bo + bx..], sb);
                }
            }
        }
    }
    total
}

pub fn ssd(a: &[u8], sa: usize, b: &[u8], sb: usize, w: usize, h: usize) -> u64 {
    let mut s = 0u64;
    for r in 0..h {
        let ra = &a[r * sa..r * sa + w];
        let rb = &b[r * sb..r * sb + w];
        s += ra
            .iter()
            .zip(rb)
            .map(|(&x, &y)| (x as i32 - y as i32).pow(2) as u32)
            .sum::<u32>() as u64;
    }
    s
}

pub fn block_var(a: &[u8], sa: usize, w: usize, h: usize) -> u32 {
    let mut s = 0u32;
    let mut ss = 0u32;
    for r in 0..h {
        for &v in &a[r * sa..r * sa + w] {
            s += v as u32;
            ss += (v as u32) * (v as u32);
        }
    }
    ss - ((s as u64 * s as u64) / (w * h) as u64) as u32
}
