use super::isa::dispatch;
use std::cell::RefCell;

#[inline(always)]
fn mult_hi(v: i32, c: i32) -> i32 {
    (v * c) >> 8
}

#[inline(always)]
fn clip(v: i32) -> u8 {
    (v >> 6).clamp(0, 255) as u8
}

dispatch! {
    fn convert(y: &[u8], u: &[u8], v: &[u8], d: &mut [u8]) {
        for ((d, &y), (&u, &v)) in d.chunks_exact_mut(4).zip(y).zip(u.iter().zip(v)) {
            let y = mult_hi(y as i32, 19077);
            let (u, v) = (u as i32, v as i32);
            d[0] = clip(y + mult_hi(v, 26149) - 14234);
            d[1] = clip(y - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708);
            d[2] = clip(y + mult_hi(u, 33050) - 17685);
            d[3] = 255;
        }
    }
}

dispatch! {
    fn upsample(t: &[u8], c: &[u8], top: &mut [u8], bot: &mut [u8], len: usize) {
        let last = (len - 1) >> 1;
        let edge = |a: u8, b: u8| ((3 * a as u32 + b as u32 + 2) >> 2) as u8;
        top[0] = edge(t[0], c[0]);
        bot[0] = edge(c[0], t[0]);
        let pairs = top[1..1 + 2 * last]
            .chunks_exact_mut(2)
            .zip(bot[1..1 + 2 * last].chunks_exact_mut(2));
        let near = t[..last].iter().zip(&t[1..=last]).zip(c[..last].iter().zip(&c[1..=last]));
        for ((tp, bp), ((&tl, &tt), (&l, &cc))) in pairs.zip(near) {
            let (tl, tt, l, cc) = (tl as u32, tt as u32, l as u32, cc as u32);
            let avg = tl + tt + l + cc + 8;
            let d12 = (avg + 2 * (tt + l)) >> 3;
            let d03 = (avg + 2 * (tl + cc)) >> 3;
            tp[0] = ((d12 + tl) >> 1) as u8;
            tp[1] = ((d03 + tt) >> 1) as u8;
            bp[0] = ((d03 + l) >> 1) as u8;
            bp[1] = ((d12 + cc) >> 1) as u8;
        }
        if len & 1 == 0 {
            top[len - 1] = edge(t[last], c[last]);
            bot[len - 1] = edge(c[last], t[last]);
        }
    }
}

thread_local! {
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
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
    scratch: &mut [u8],
) {
    let (ut, rest) = scratch.split_at_mut(len);
    let (ub, rest) = rest.split_at_mut(len);
    let (vt, vb) = rest.split_at_mut(len);
    upsample(tu, cu, ut, ub, len);
    upsample(tv, cv, vt, &mut vb[..len], len);
    convert(&ty[..len], ut, vt, &mut td[..len * 4]);
    if let (Some(by), Some(bd)) = (by, bd) {
        convert(&by[..len], ub, &vb[..len], &mut bd[..len * 4]);
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
    let mut scratch = SCRATCH.with_borrow_mut(std::mem::take);
    if scratch.len() < 4 * w {
        scratch.resize(4 * w, 0);
    }
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
                &mut scratch,
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
                &mut scratch,
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
                &mut scratch,
            );
            y += 1;
        }
    }
    SCRATCH.with_borrow_mut(|s| *s = scratch);
}
