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

enum Stage<'a> {
    Predict {
        bits: u32,
        modes: &'a [u32],
        mw: usize,
        prev: Vec<u32>,
    },
    Color {
        bits: u32,
        elems: &'a [u32],
        mw: usize,
    },
    SubtractGreen,
    Index {
        bits: u32,
        palette: &'a [u32],
        width: usize,
    },
}

pub(super) struct Rows<'a> {
    stages: Vec<Stage<'a>>,
    a: Vec<u32>,
    b: Vec<u32>,
}

impl<'a> Rows<'a> {
    pub fn new(transforms: &'a [Transform], width: usize) -> Self {
        let mut sizes = Vec::with_capacity(transforms.len());
        let mut xsize = width;
        for t in transforms {
            sizes.push(xsize);
            if let Transform::Index { bits, .. } = t {
                xsize = xsize.div_ceil(1 << bits);
            }
        }
        let stages = transforms
            .iter()
            .zip(sizes)
            .rev()
            .map(|(t, xs)| match t {
                Transform::Predict { bits, data } => Stage::Predict {
                    bits: *bits,
                    modes: data,
                    mw: xs.div_ceil(1 << bits),
                    prev: vec![0; xs],
                },
                Transform::Color { bits, data } => Stage::Color {
                    bits: *bits,
                    elems: data,
                    mw: xs.div_ceil(1 << bits),
                },
                Transform::SubtractGreen => Stage::SubtractGreen,
                Transform::Index {
                    bits,
                    palette,
                    width,
                } => Stage::Index {
                    bits: *bits,
                    palette,
                    width: *width,
                },
            })
            .collect();
        Rows {
            stages,
            a: Vec::with_capacity(width),
            b: Vec::with_capacity(width),
        }
    }

    pub fn row(&mut self, y: usize, input: &[u32]) -> &[u32] {
        self.a.clear();
        self.a.extend_from_slice(input);
        for st in self.stages.iter_mut() {
            match st {
                Stage::Predict {
                    bits,
                    modes,
                    mw,
                    prev,
                } => {
                    predict_row(&mut self.a, prev, y, *bits, &modes[(y >> *bits) * *mw..]);
                    prev.copy_from_slice(&self.a);
                }
                Stage::Color { bits, elems, mw } => {
                    color_row(&mut self.a, *bits, &elems[(y >> *bits) * *mw..]);
                }
                Stage::SubtractGreen => {
                    for p in self.a.iter_mut() {
                        let g = (*p >> 8) & 0xff;
                        *p = add_pixels(*p, g << 16 | g);
                    }
                }
                Stage::Index {
                    bits,
                    palette,
                    width,
                } => {
                    self.b.clear();
                    self.b.resize(*width, 0);
                    unpack_row(&self.a, &mut self.b, *bits, palette);
                    std::mem::swap(&mut self.a, &mut self.b);
                }
            }
        }
        &self.a
    }
}

fn predict_row(cur: &mut [u32], prev: &[u32], y: usize, bits: u32, modes: &[u32]) {
    let width = cur.len();
    if y == 0 {
        cur[0] = add_pixels(cur[0], 0xff000000);
        for x in 1..width {
            cur[x] = add_pixels(cur[x], cur[x - 1]);
        }
        return;
    }
    cur[0] = add_pixels(cur[0], prev[0]);
    let bw = 1usize << bits;
    let mut x = 1;
    while x < width {
        let end = ((x & !(bw - 1)) + bw).min(width);
        let mode = (modes[x >> bits] >> 8) & 0xf;
        predict_run(mode, prev, cur, x, end);
        x = end;
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

fn color_row(row: &mut [u32], bits: u32, elems: &[u32]) {
    for (seg, &e) in row.chunks_mut(1 << bits).zip(elems) {
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

fn unpack_row(input: &[u32], out: &mut [u32], bits: u32, palette: &[u32]) {
    if bits == 0 {
        for (o, &p) in out.iter_mut().zip(input) {
            *o = palette[((p >> 8) & 0xff) as usize];
        }
        return;
    }
    let bpp = 8 >> bits;
    let m = (1u32 << bpp) - 1;
    for (oc, &p) in out.chunks_mut(1 << bits).zip(input) {
        let mut v = (p >> 8) & 0xff;
        for o in oc {
            *o = palette[(v & m) as usize];
            v >>= bpp;
        }
    }
}
