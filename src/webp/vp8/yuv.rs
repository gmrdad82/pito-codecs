#[inline(always)]
fn mult_hi(v: i32, c: i32) -> i32 {
    (v * c) >> 8
}

#[inline(always)]
fn clip(v: i32) -> u8 {
    if v & !16383 == 0 {
        (v >> 6) as u8
    } else if v < 0 {
        0
    } else {
        255
    }
}

#[inline(always)]
fn put(y: u8, u: u32, v: u32, d: &mut [u8]) {
    let y = mult_hi(y as i32, 19077);
    let u = u as i32;
    let v = v as i32;
    d[0] = clip(y + mult_hi(v, 26149) - 14234);
    d[1] = clip(y - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708);
    d[2] = clip(y + mult_hi(u, 33050) - 17685);
    d[3] = 255;
}

#[inline(always)]
fn load(u: u8, v: u8) -> u32 {
    u as u32 | ((v as u32) << 16)
}

#[allow(clippy::too_many_arguments)]
fn pair(
    ty: &[u8],
    by: Option<&[u8]>,
    tu: &[u8],
    tv: &[u8],
    cu: &[u8],
    cv: &[u8],
    td: &mut [u8],
    bd: Option<&mut [u8]>,
    len: usize,
) {
    let last = (len - 1) >> 1;
    let mut tl = load(tu[0], tv[0]);
    let mut l = load(cu[0], cv[0]);
    let uv0 = (3 * tl + l + 0x0002_0002) >> 2;
    put(ty[0], uv0 & 0xff, uv0 >> 16, &mut td[0..4]);
    let mut bd = bd;
    if let (Some(by), Some(bd)) = (by, bd.as_deref_mut()) {
        let uv0 = (3 * l + tl + 0x0002_0002) >> 2;
        put(by[0], uv0 & 0xff, uv0 >> 16, &mut bd[0..4]);
    }
    for x in 1..=last {
        let t = load(tu[x], tv[x]);
        let uv = load(cu[x], cv[x]);
        let avg = tl + t + l + uv + 0x0008_0008;
        let d12 = (avg + 2 * (t + l)) >> 3;
        let d03 = (avg + 2 * (tl + uv)) >> 3;
        let a = (d12 + tl) >> 1;
        let b = (d03 + t) >> 1;
        put(
            ty[2 * x - 1],
            a & 0xff,
            (a >> 16) & 0xff,
            &mut td[(2 * x - 1) * 4..2 * x * 4],
        );
        put(
            ty[2 * x],
            b & 0xff,
            (b >> 16) & 0xff,
            &mut td[2 * x * 4..2 * x * 4 + 4],
        );
        if let (Some(by), Some(bd)) = (by, bd.as_deref_mut()) {
            let a = (d03 + l) >> 1;
            let b = (d12 + uv) >> 1;
            put(
                by[2 * x - 1],
                a & 0xff,
                (a >> 16) & 0xff,
                &mut bd[(2 * x - 1) * 4..2 * x * 4],
            );
            put(
                by[2 * x],
                b & 0xff,
                (b >> 16) & 0xff,
                &mut bd[2 * x * 4..2 * x * 4 + 4],
            );
        }
        tl = t;
        l = uv;
    }
    if len & 1 == 0 {
        let uv0 = (3 * tl + l + 0x0002_0002) >> 2;
        put(
            ty[len - 1],
            uv0 & 0xff,
            uv0 >> 16,
            &mut td[(len - 1) * 4..len * 4],
        );
        if let (Some(by), Some(bd)) = (by, bd) {
            let uv0 = (3 * l + tl + 0x0002_0002) >> 2;
            put(
                by[len - 1],
                uv0 & 0xff,
                uv0 >> 16,
                &mut bd[(len - 1) * 4..len * 4],
            );
        }
    }
}

pub struct Yuv<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub ys: usize,
    pub uvs: usize,
    pub yr: usize,
    pub cr: usize,
    pub width: usize,
    pub height: usize,
}

pub fn to_rgba_rows(p: &Yuv, rows: std::ops::Range<usize>, out: &mut [u8]) {
    let w = p.width;
    let h = p.height;
    let row = |y: usize| &p.y[(y - p.yr) * p.ys..(y - p.yr) * p.ys + w];
    let cu = |k: usize| &p.u[(k - p.cr) * p.uvs..];
    let cv = |k: usize| &p.v[(k - p.cr) * p.uvs..];
    let base = rows.start * w * 4;
    let mut y = rows.start;
    while y < rows.end {
        let o = (y * w * 4) - base;
        if y == 0 {
            pair(
                row(0),
                None,
                cu(0),
                cv(0),
                cu(0),
                cv(0),
                &mut out[o..o + w * 4],
                None,
                w,
            );
            y += 1;
        } else if y + 1 < h && y + 1 < rows.end {
            let k = y.div_ceil(2);
            let (a, b) = out[o..o + 2 * w * 4].split_at_mut(w * 4);
            pair(
                row(y),
                Some(row(y + 1)),
                cu(k - 1),
                cv(k - 1),
                cu(k),
                cv(k),
                a,
                Some(b),
                w,
            );
            y += 2;
        } else {
            let k = y / 2;
            pair(
                row(y),
                None,
                cu(k),
                cv(k),
                cu(k),
                cv(k),
                &mut out[o..o + w * 4],
                None,
                w,
            );
            y += 1;
        }
    }
}
