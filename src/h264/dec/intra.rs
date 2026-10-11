#[derive(Clone, Copy, Debug, Default)]
pub struct Avail {
    pub left: bool,
    pub top: bool,
    pub top_left: bool,
    pub top_right: bool,
}

#[derive(Clone, Copy)]
struct Edge {
    top: [i32; 16],
    left: [i32; 8],
    tl: i32,
}

impl Edge {
    #[inline(always)]
    fn t(&self, x: i32) -> i32 {
        if x < 0 { self.tl } else { self.top[x as usize] }
    }
    #[inline(always)]
    fn l(&self, y: i32) -> i32 {
        if y < 0 {
            self.tl
        } else {
            self.left[y as usize]
        }
    }
}

fn load_edge(plane: &[u8], stride: usize, x: usize, y: usize, n: usize, a: Avail) -> Edge {
    let mut e = Edge {
        top: [128; 16],
        left: [128; 8],
        tl: 128,
    };
    if a.top {
        let row = &plane[(y - 1) * stride + x..];
        for i in 0..n {
            e.top[i] = row[i] as i32;
        }
        if a.top_right {
            for i in n..2 * n {
                e.top[i] = row[i] as i32;
            }
        } else {
            let v = e.top[n - 1];
            for t in &mut e.top[n..2 * n] {
                *t = v;
            }
        }
    }
    if a.left {
        for i in 0..n {
            e.left[i] = plane[(y + i) * stride + x - 1] as i32;
        }
    }
    if a.top_left {
        e.tl = plane[(y - 1) * stride + x - 1] as i32;
    }
    e
}

fn pred_nxn(mode: u8, e: &Edge, n: i32, a: Avail, out: &mut [u8], stride: usize) {
    let mut put = |x: i32, y: i32, v: i32| out[y as usize * stride + x as usize] = v as u8;
    match mode {
        0 => {
            for y in 0..n {
                for x in 0..n {
                    put(x, y, e.t(x));
                }
            }
        }
        1 => {
            for y in 0..n {
                for x in 0..n {
                    put(x, y, e.l(y));
                }
            }
        }
        2 => {
            let shift = if n == 4 { 2 } else { 3 };
            let st: i32 = (0..n).map(|x| e.t(x)).sum();
            let sl: i32 = (0..n).map(|y| e.l(y)).sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + n) >> (shift + 1),
                (false, true) => (sl + (n >> 1)) >> shift,
                (true, false) => (st + (n >> 1)) >> shift,
                _ => 128,
            };
            for y in 0..n {
                for x in 0..n {
                    put(x, y, dc);
                }
            }
        }
        3 => {
            for y in 0..n {
                for x in 0..n {
                    let v = if x == n - 1 && y == n - 1 {
                        (e.t(2 * n - 2) + 3 * e.t(2 * n - 1) + 2) >> 2
                    } else {
                        (e.t(x + y) + 2 * e.t(x + y + 1) + e.t(x + y + 2) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        4 => {
            for y in 0..n {
                for x in 0..n {
                    let v = if x > y {
                        (e.t(x - y - 2) + 2 * e.t(x - y - 1) + e.t(x - y) + 2) >> 2
                    } else if x < y {
                        (e.l(y - x - 2) + 2 * e.l(y - x - 1) + e.l(y - x) + 2) >> 2
                    } else {
                        (e.t(0) + 2 * e.tl + e.l(0) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        5 => {
            for y in 0..n {
                for x in 0..n {
                    let z = 2 * x - y;
                    let v = if z >= 0 && z % 2 == 0 {
                        (e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (e.t(x - (y >> 1) - 2) + 2 * e.t(x - (y >> 1) - 1) + e.t(x - (y >> 1)) + 2)
                            >> 2
                    } else if z == -1 {
                        (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
                    } else {
                        (e.l(y - 2 * x - 1) + 2 * e.l(y - 2 * x - 2) + e.l(y - 2 * x - 3) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        6 => {
            for y in 0..n {
                for x in 0..n {
                    let z = 2 * y - x;
                    let v = if z >= 0 && z % 2 == 0 {
                        (e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 1) >> 1
                    } else if z > 0 {
                        (e.l(y - (x >> 1) - 2) + 2 * e.l(y - (x >> 1) - 1) + e.l(y - (x >> 1)) + 2)
                            >> 2
                    } else if z == -1 {
                        (e.l(0) + 2 * e.tl + e.t(0) + 2) >> 2
                    } else {
                        (e.t(x - 2 * y - 1) + 2 * e.t(x - 2 * y - 2) + e.t(x - 2 * y - 3) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        7 => {
            for y in 0..n {
                for x in 0..n {
                    let i = x + (y >> 1);
                    let v = if y % 2 == 0 {
                        (e.t(i) + e.t(i + 1) + 1) >> 1
                    } else {
                        (e.t(i) + 2 * e.t(i + 1) + e.t(i + 2) + 2) >> 2
                    };
                    put(x, y, v);
                }
            }
        }
        _ => {
            let zmax = 2 * n - 3;
            for y in 0..n {
                for x in 0..n {
                    let z = x + 2 * y;
                    let i = y + (x >> 1);
                    let v = if z < zmax && z % 2 == 0 {
                        (e.l(i) + e.l(i + 1) + 1) >> 1
                    } else if z < zmax {
                        (e.l(i) + 2 * e.l(i + 1) + e.l(i + 2) + 2) >> 2
                    } else if z == zmax {
                        (e.l(n - 2) + 3 * e.l(n - 1) + 2) >> 2
                    } else {
                        e.l(n - 1)
                    };
                    put(x, y, v);
                }
            }
        }
    }
}

pub fn pred4x4(plane: &mut [u8], stride: usize, x: usize, y: usize, mode: u8, a: Avail) {
    let e = load_edge(plane, stride, x, y, 4, a);
    pred_nxn(mode, &e, 4, a, &mut plane[y * stride + x..], stride);
}

pub fn pred8x8(plane: &mut [u8], stride: usize, x: usize, y: usize, mode: u8, a: Avail) {
    let p = load_edge(plane, stride, x, y, 8, a);
    let mut f = p;
    if a.top {
        f.top[0] = if a.top_left {
            (p.tl + 2 * p.top[0] + p.top[1] + 2) >> 2
        } else {
            (3 * p.top[0] + p.top[1] + 2) >> 2
        };
        for i in 1..15 {
            f.top[i] = (p.top[i - 1] + 2 * p.top[i] + p.top[i + 1] + 2) >> 2;
        }
        f.top[15] = (p.top[14] + 3 * p.top[15] + 2) >> 2;
    }
    if a.top_left {
        f.tl = match (a.top, a.left) {
            (true, true) => (p.top[0] + 2 * p.tl + p.left[0] + 2) >> 2,
            (true, false) => (3 * p.tl + p.top[0] + 2) >> 2,
            (false, true) => (3 * p.tl + p.left[0] + 2) >> 2,
            _ => p.tl,
        };
    }
    if a.left {
        f.left[0] = if a.top_left {
            (p.tl + 2 * p.left[0] + p.left[1] + 2) >> 2
        } else {
            (3 * p.left[0] + p.left[1] + 2) >> 2
        };
        for i in 1..7 {
            f.left[i] = (p.left[i - 1] + 2 * p.left[i] + p.left[i + 1] + 2) >> 2;
        }
        f.left[7] = (p.left[6] + 3 * p.left[7] + 2) >> 2;
    }
    pred_nxn(mode, &f, 8, a, &mut plane[y * stride + x..], stride);
}

#[inline(always)]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

pub fn pred16x16(plane: &mut [u8], stride: usize, x: usize, y: usize, mode: u8, a: Avail) {
    let mut top = [0i32; 16];
    let mut left = [0i32; 16];
    if a.top {
        for i in 0..16 {
            top[i] = plane[(y - 1) * stride + x + i] as i32;
        }
    }
    if a.left {
        for i in 0..16 {
            left[i] = plane[(y + i) * stride + x - 1] as i32;
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            for r in 0..16 {
                for c in 0..16 {
                    plane[base + r * stride + c] = top[c] as u8;
                }
            }
        }
        1 => {
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(left[r] as u8);
            }
        }
        2 => {
            let st: i32 = top.iter().sum();
            let sl: i32 = left.iter().sum();
            let dc = match (a.top, a.left) {
                (true, true) => (st + sl + 16) >> 5,
                (false, true) => (sl + 8) >> 4,
                (true, false) => (st + 8) >> 4,
                _ => 128,
            } as u8;
            for r in 0..16 {
                plane[base + r * stride..base + r * stride + 16].fill(dc);
            }
        }
        _ => {
            let tl = if a.top_left {
                plane[(y - 1) * stride + x - 1] as i32
            } else {
                128
            };
            let t = |i: i32| if i < 0 { tl } else { top[i as usize] };
            let l = |i: i32| if i < 0 { tl } else { left[i as usize] };
            let mut h = 0;
            let mut v = 0;
            for k in 0..8 {
                h += (k + 1) * (t(8 + k) - t(6 - k));
                v += (k + 1) * (l(8 + k) - l(6 - k));
            }
            let aa = 16 * (left[15] + top[15]);
            let b = (5 * h + 32) >> 6;
            let c = (5 * v + 32) >> 6;
            for r in 0..16 {
                for cc in 0..16 {
                    plane[base + r * stride + cc] =
                        clip((aa + b * (cc as i32 - 7) + c * (r as i32 - 7) + 16) >> 5);
                }
            }
        }
    }
}

pub fn pred_chroma(plane: &mut [u8], stride: usize, x: usize, y: usize, mode: u8, a: Avail) {
    const W: usize = 8;
    const H: usize = 8;
    let mut top = [0i32; W];
    let mut left = [0i32; H];
    if a.top {
        for i in 0..W {
            top[i] = plane[(y - 1) * stride + x + i] as i32;
        }
    }
    if a.left {
        for i in 0..H {
            left[i] = plane[(y + i) * stride + x - 1] as i32;
        }
    }
    let base = y * stride + x;
    match mode {
        0 => {
            for by in 0..H / 4 {
                for bx in 0..W / 4 {
                    let st: i32 = top[bx * 4..bx * 4 + 4].iter().sum();
                    let sl: i32 = left[by * 4..by * 4 + 4].iter().sum();
                    let dc = if (bx == 0 && by == 0) || (bx > 0 && by > 0) {
                        match (a.top, a.left) {
                            (true, true) => (st + sl + 4) >> 3,
                            (false, true) => (sl + 2) >> 2,
                            (true, false) => (st + 2) >> 2,
                            _ => 128,
                        }
                    } else if bx > 0 {
                        if a.top {
                            (st + 2) >> 2
                        } else if a.left {
                            (sl + 2) >> 2
                        } else {
                            128
                        }
                    } else if a.left {
                        (sl + 2) >> 2
                    } else if a.top {
                        (st + 2) >> 2
                    } else {
                        128
                    };
                    for r in 0..4 {
                        let o = base + (by * 4 + r) * stride + bx * 4;
                        plane[o..o + 4].fill(dc as u8);
                    }
                }
            }
        }
        1 => {
            for r in 0..H {
                plane[base + r * stride..base + r * stride + W].fill(left[r] as u8);
            }
        }
        2 => {
            for r in 0..H {
                for c in 0..W {
                    plane[base + r * stride + c] = top[c] as u8;
                }
            }
        }
        _ => {
            let tl = if a.top_left {
                plane[(y - 1) * stride + x - 1] as i32
            } else {
                128
            };
            let t = |i: i32| if i < 0 { tl } else { top[i as usize] };
            let l = |i: i32| if i < 0 { tl } else { left[i as usize] };
            let mut h = 0;
            let mut v = 0;
            for k in 0..4 {
                h += (k + 1) * (t(4 + k) - t(2 - k));
                v += (k + 1) * (l(4 + k) - l(2 - k));
            }
            let aa = 16 * (left[H - 1] + top[W - 1]);
            let b = (34 * h + 32) >> 6;
            let c = (34 * v + 32) >> 6;
            for r in 0..H {
                for cc in 0..W {
                    plane[base + r * stride + cc] =
                        clip((aa + b * (cc as i32 - 3) + c * (r as i32 - 3) + 16) >> 5);
                }
            }
        }
    }
}
