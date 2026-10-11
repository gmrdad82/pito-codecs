use crate::media::Result;

pub(super) enum Transform {
    Predict {
        bits: u32,
        data: Vec<u32>,
    },
    Color {
        bits: u32,
        data: Vec<u32>,
    },
    SubtractGreen,
    Index {
        bits: u32,
        palette: Vec<u32>,
        width: usize,
    },
}

impl Transform {
    pub fn kind(&self) -> u8 {
        match self {
            Transform::Predict { .. } => 0,
            Transform::Color { .. } => 1,
            Transform::SubtractGreen => 2,
            Transform::Index { .. } => 3,
        }
    }
}

#[inline(always)]
pub(super) fn add_pixels(a: u32, b: u32) -> u32 {
    let ag = (a & 0xff00ff00).wrapping_add(b & 0xff00ff00) & 0xff00ff00;
    let rb = (a & 0x00ff00ff).wrapping_add(b & 0x00ff00ff) & 0x00ff00ff;
    ag | rb
}

#[inline(always)]
pub(super) fn average2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefefefe) >> 1) + (a & b)
}

#[inline(always)]
fn clip255(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

#[inline(always)]
fn ch(p: u32, s: u32) -> i32 {
    ((p >> s) & 0xff) as i32
}

#[inline(always)]
fn select(t: u32, l: u32, tl: u32) -> u32 {
    let mut d = 0;
    for s in [24, 16, 8, 0] {
        d += (ch(l, s) - ch(tl, s)).abs() - (ch(t, s) - ch(tl, s)).abs();
    }
    if d <= 0 { t } else { l }
}

#[inline(always)]
fn clamp_full(a: u32, b: u32, c: u32) -> u32 {
    let mut out = 0;
    for s in [24, 16, 8, 0] {
        out |= clip255(ch(a, s) + ch(b, s) - ch(c, s)) << s;
    }
    out
}

#[inline(always)]
fn clamp_half(a: u32, b: u32) -> u32 {
    let mut out = 0;
    for s in [24, 16, 8, 0] {
        let x = ch(a, s);
        out |= clip255(x + (x - ch(b, s)) / 2) << s;
    }
    out
}

#[inline(always)]
pub(super) fn predict(mode: u32, l: u32, t: u32, tl: u32, tr: u32) -> u32 {
    match mode {
        1 => l,
        2 => t,
        3 => tr,
        4 => tl,
        5 => average2(average2(l, tr), t),
        6 => average2(l, tl),
        7 => average2(l, t),
        8 => average2(tl, t),
        9 => average2(t, tr),
        10 => average2(average2(l, tl), average2(t, tr)),
        11 => select(t, l, tl),
        12 => clamp_full(l, t, tl),
        13 => clamp_half(average2(l, t), tl),
        _ => 0xff000000,
    }
}

pub(super) fn inverse(
    transforms: &[Transform],
    width: usize,
    height: usize,
    mut data: Vec<u32>,
) -> Result<Vec<u32>> {
    let mut xsize = data.len() / height;
    for t in transforms.iter().rev() {
        match t {
            Transform::Predict { bits, data: modes } => {
                inverse_predict(&mut data, xsize, height, *bits, modes)
            }
            Transform::Color { bits, data: elems } => inverse_color(&mut data, xsize, *bits, elems),
            Transform::SubtractGreen => {
                for p in data.iter_mut() {
                    let g = (*p >> 8) & 0xff;
                    *p = add_pixels(*p, g << 16 | g);
                }
            }
            Transform::Index {
                bits,
                palette,
                width: w,
            } => {
                data = unpack(&data, xsize, *w, height, *bits, palette);
                xsize = *w;
            }
        }
    }
    debug_assert_eq!(xsize, width);
    Ok(data)
}

fn inverse_predict(data: &mut [u32], width: usize, height: usize, bits: u32, modes: &[u32]) {
    data[0] = add_pixels(data[0], 0xff000000);
    for x in 1..width {
        data[x] = add_pixels(data[x], data[x - 1]);
    }
    let bw = 1usize << bits;
    let mw = width.div_ceil(bw);
    for y in 1..height {
        let (prev, cur) = data[(y - 1) * width..(y + 1) * width].split_at_mut(width);
        cur[0] = add_pixels(cur[0], prev[0]);
        let row_modes = &modes[(y >> bits) * mw..];
        let mut x = 1;
        while x < width {
            let end = ((x & !(bw - 1)) + bw).min(width);
            let mode = (row_modes[x >> bits] >> 8) & 0xf;
            predict_run(mode, prev, cur, x, end);
            x = end;
        }
    }
}

#[inline(always)]
fn predict_run(mode: u32, prev: &[u32], cur: &mut [u32], start: usize, end: usize) {
    let width = cur.len();
    let last = if end == width { end - 1 } else { end };
    match mode {
        0 => {
            for p in &mut cur[start..end] {
                *p = add_pixels(*p, 0xff000000);
            }
            return;
        }
        1 => {
            let mut l = cur[start - 1];
            for p in &mut cur[start..end] {
                l = add_pixels(*p, l);
                *p = l;
            }
            return;
        }
        2 => {
            for (p, &t) in cur[start..end].iter_mut().zip(&prev[start..end]) {
                *p = add_pixels(*p, t);
            }
            return;
        }
        4 => {
            for (p, &t) in cur[start..end].iter_mut().zip(&prev[start - 1..end - 1]) {
                *p = add_pixels(*p, t);
            }
            return;
        }
        8 => {
            for (i, p) in cur[start..end].iter_mut().enumerate() {
                *p = add_pixels(*p, average2(prev[start + i - 1], prev[start + i]));
            }
            return;
        }
        _ => {}
    }
    for x in start..last {
        let p = predict(mode, cur[x - 1], prev[x], prev[x - 1], prev[x + 1]);
        cur[x] = add_pixels(cur[x], p);
    }
    if last < end {
        let x = last;
        let p = predict(mode, cur[x - 1], prev[x], prev[x - 1], cur[0]);
        cur[x] = add_pixels(cur[x], p);
    }
}

#[inline(always)]
pub(super) fn color_delta(t: u8, c: u8) -> i32 {
    (t as i8 as i32 * c as i8 as i32) >> 5
}

fn inverse_color(data: &mut [u32], width: usize, bits: u32, elems: &[u32]) {
    let mw = width.div_ceil(1 << bits);
    for (y, row) in data.chunks_exact_mut(width).enumerate() {
        let er = &elems[(y >> bits) * mw..];
        for (bx, seg) in row.chunks_mut(1 << bits).enumerate() {
            let e = er[bx];
            let g2r = e as u8;
            let g2b = (e >> 8) as u8;
            let r2b = (e >> 16) as u8;
            for p in seg {
                let v = *p;
                let g = (v >> 8) as u8;
                let r = ((v >> 16) as i32 + color_delta(g2r, g)) as u8;
                let b = (v as i32 + color_delta(g2b, g) + color_delta(r2b, r)) as u8;
                *p = (v & 0xff00ff00) | (r as u32) << 16 | b as u32;
            }
        }
    }
}

fn unpack(
    data: &[u32],
    xsize: usize,
    width: usize,
    height: usize,
    bits: u32,
    palette: &[u32],
) -> Vec<u32> {
    let mut out = vec![0u32; width * height];
    if bits == 0 {
        for (o, &p) in out.iter_mut().zip(data) {
            *o = palette[((p >> 8) & 0xff) as usize];
        }
        return out;
    }
    let bpp = 8 >> bits;
    let per = 1usize << bits;
    let m = (1u32 << bpp) - 1;
    for (orow, irow) in out.chunks_exact_mut(width).zip(data.chunks_exact(xsize)) {
        for (oc, &p) in orow.chunks_mut(per).zip(irow) {
            let mut v = (p >> 8) & 0xff;
            for o in oc {
                *o = palette[(v & m) as usize];
                v >>= bpp;
            }
        }
    }
    out
}
