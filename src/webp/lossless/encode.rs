use super::cluster::{Layout, NUM_LENGTH, add, bits, cluster, prefix, token_len};
use super::lz77::{Plane, Search, Token, greedy, matches, parse};
use super::transform::{color_delta, predict};
use super::writer::{BitWriter, HuffCode, write_code};
use rayon::prelude::*;

pub(crate) struct Policy {
    pub effort: u8,
    pub palette: bool,
    pub predict: bool,
}

#[inline(always)]
fn sub_pixels(a: u32, b: u32) -> u32 {
    let ag = (0x00ff00ff | (a & 0xff00ff00)).wrapping_sub(b & 0xff00ff00) & 0xff00ff00;
    let rb = (0xff00ff00 | (a & 0x00ff00ff)).wrapping_sub(b & 0x00ff00ff) & 0x00ff00ff;
    ag | rb
}

#[derive(Clone, Copy)]
struct Recipe {
    green: bool,
    predict: bool,
}

pub(super) fn encode_image(px: Vec<u32>, width: usize, height: usize, policy: &Policy) -> Vec<u8> {
    let palette = if policy.palette {
        palette_of(&px).map(|mut p| {
            p.sort_unstable();
            p
        })
    } else {
        None
    };
    let effort = policy.effort;
    let plain = if policy.predict {
        let (predict, green) = analyze(&px, width);
        Recipe { green, predict }
    } else {
        Recipe {
            green: false,
            predict: false,
        }
    };
    let mut candidates: Vec<(Option<&[u32]>, Recipe)> = Vec::new();
    if let Some(p) = &palette {
        candidates.push((Some(p), plain));
    }
    if palette.is_none() || effort >= 2 {
        candidates.push((None, plain));
    }
    if effort >= 5 && policy.predict {
        for green in [false, true] {
            let r = Recipe {
                green,
                predict: true,
            };
            if !(plain.predict && plain.green == green) {
                candidates.push((None, r));
            }
        }
    }
    let (last, rest) = candidates.split_last().unwrap();
    std::thread::scope(|s| {
        let others: Vec<_> = rest
            .iter()
            .map(|&(pal, recipe)| {
                let px = &px;
                s.spawn(move || body(px, width, height, pal, recipe, effort))
            })
            .collect();
        let mut best = body(&px, width, height, last.0, last.1, effort);
        for h in others {
            let b = h.join().unwrap();
            if b.len() < best.len() {
                best = b;
            }
        }
        best
    })
}

fn analyze(px: &[u32], width: usize) -> (bool, bool) {
    let mut h = vec![[0u32; 256]; 12];
    let mut left = px[0];
    for (i, &p) in px.iter().enumerate() {
        let d = if i % width == 0 && i >= width {
            sub_pixels(p, px[i - width])
        } else {
            sub_pixels(p, left)
        };
        left = p;
        let (a, r, g, b) = (p >> 24, p >> 16 & 0xff, p >> 8 & 0xff, p & 0xff);
        let (da, dr, dg, db) = (d >> 24, d >> 16 & 0xff, d >> 8 & 0xff, d & 0xff);
        h[0][a as usize] += 1;
        h[1][r as usize] += 1;
        h[2][g as usize] += 1;
        h[3][b as usize] += 1;
        h[4][da as usize] += 1;
        h[5][dr as usize] += 1;
        h[6][dg as usize] += 1;
        h[7][db as usize] += 1;
        h[8][(r.wrapping_sub(g) & 0xff) as usize] += 1;
        h[9][(b.wrapping_sub(g) & 0xff) as usize] += 1;
        h[10][(dr.wrapping_sub(dg) & 0xff) as usize] += 1;
        h[11][(db.wrapping_sub(dg) & 0xff) as usize] += 1;
    }
    let e: Vec<f64> = h
        .iter()
        .map(|c| {
            let n: u64 = c.iter().map(|&v| v as u64).sum();
            let s: f64 = c
                .iter()
                .filter(|&&v| v > 0)
                .map(|&v| v as f64 * (v as f64).log2())
                .sum();
            n as f64 * (n as f64).log2() - s
        })
        .collect();
    let direct = e[0] + e[1] + e[2] + e[3];
    let spatial = e[4] + e[5] + e[6] + e[7];
    let green = e[0] + e[8] + e[2] + e[9];
    let both = e[4] + e[10] + e[6] + e[11];
    let costs = [direct, spatial, green, both];
    let best = (0..4)
        .min_by(|&a, &b| costs[a].total_cmp(&costs[b]))
        .unwrap();
    (best == 1 || best == 3, best >= 2)
}

fn body(
    px: &[u32],
    width: usize,
    height: usize,
    palette: Option<&[u32]>,
    recipe: Recipe,
    effort: u8,
) -> Vec<u8> {
    let mut w = BitWriter::with_capacity(px.len());
    let mut xsize = width;
    let mut px = px.to_vec();
    if let Some(palette) = palette {
        let bits = match palette.len() {
            0..=2 => 3,
            3..=4 => 2,
            5..=16 => 1,
            _ => 0,
        };
        w.put(1, 1);
        w.put(3, 2);
        w.put(palette.len() as u32 - 1, 8);
        let mut delta = palette.to_vec();
        for i in (1..delta.len()).rev() {
            delta[i] = sub_pixels(delta[i], delta[i - 1]);
        }
        let n = delta.len();
        encode_sub(&mut w, delta, n, effort);
        px = pack(&px, width, height, palette, bits);
        xsize = width.div_ceil(1 << bits);
    } else {
        if recipe.green {
            for p in px.iter_mut() {
                let g = (*p >> 8) & 0xff;
                *p = sub_pixels(*p, g << 16 | g);
            }
            w.put(1, 1);
            w.put(2, 2);
        }
        if recipe.predict {
            let bits = if effort >= 7 { 4 } else { 5 };
            let (res, modes) = forward_predict(&px, width, height, bits, effort);
            let mw = width.div_ceil(1 << bits);
            w.put(1, 1);
            w.put(0, 2);
            w.put(bits - 2, 3);
            encode_sub(&mut w, modes, mw, effort);
            px = res;
            if effort >= 2 {
                let elems = forward_color(&mut px, width, height, bits, effort);
                w.put(1, 1);
                w.put(1, 2);
                w.put(bits - 2, 3);
                encode_sub(&mut w, elems, mw, effort);
            }
        }
    }
    w.put(0, 1);
    encode_stream(&mut w, &px, xsize, true, effort);
    w.finish()
}

fn palette_of(px: &[u32]) -> Option<Vec<u32>> {
    let mut table = [0u32; 1024];
    let mut used = [false; 1024];
    let mut colors = Vec::with_capacity(257);
    let mut last = px[0] ^ 1;
    for &p in px {
        if p == last {
            continue;
        }
        last = p;
        let mut h = (p.wrapping_mul(0x1e35a7bd) >> 22) as usize;
        loop {
            if !used[h] {
                used[h] = true;
                table[h] = p;
                colors.push(p);
                if colors.len() > 256 {
                    return None;
                }
                break;
            }
            if table[h] == p {
                break;
            }
            h = (h + 1) & 1023;
        }
    }
    Some(colors)
}

fn pack(px: &[u32], width: usize, height: usize, palette: &[u32], bits: u32) -> Vec<u32> {
    let index = |p: u32| palette.binary_search(&p).unwrap() as u32;
    let per = 1usize << bits;
    let bpp = 8 >> bits;
    let xsize = width.div_ceil(per);
    let mut out = vec![0u32; xsize * height];
    let mut last = (px[0] ^ 1, 0);
    for (orow, irow) in out.chunks_exact_mut(xsize).zip(px.chunks_exact(width)) {
        for (o, chunk) in orow.iter_mut().zip(irow.chunks(per)) {
            let mut v = 0u32;
            for (k, &p) in chunk.iter().enumerate() {
                if p != last.0 {
                    last = (p, index(p));
                }
                v |= last.1 << (k as u32 * bpp);
            }
            *o = 0xff000000 | v << 8;
        }
    }
    out
}

static COST: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let v = i as u8 as i8 as i32;
        let a = if v < 0 { -v } else { v };
        let mut lg = 0;
        let mut x = a;
        while x > 0 {
            lg += 1;
            x >>= 1;
        }
        t[i] = if a == 0 {
            0
        } else {
            (lg * 16 + ((a * 16) >> (lg - 1)) - 16 + 8) as u16
        };
        i += 1;
    }
    t
};

type Tables = [[u16; 256]; 4];

fn abs_tables() -> Tables {
    [COST; 4]
}

fn learned(h: &[[u32; 256]; 4]) -> Tables {
    let mut t = [[0u16; 256]; 4];
    for (tc, hc) in t.iter_mut().zip(h) {
        let total: u32 = hc.iter().sum();
        let lt = (total as f32 + 128.0).log2();
        for (v, &c) in tc.iter_mut().zip(hc) {
            *v = ((lt - (c as f32 + 0.5).log2()) * 16.0)
                .round()
                .clamp(0.0, 65535.0) as u16;
        }
    }
    t
}

#[inline(always)]
fn cost_in(t: &Tables, r: u32) -> u32 {
    t[0][(r >> 24) as usize] as u32
        + t[1][(r >> 16 & 0xff) as usize] as u32
        + t[2][(r >> 8 & 0xff) as usize] as u32
        + t[3][(r & 0xff) as usize] as u32
}

#[inline(always)]
fn block_cost<const M: u32>(
    px: &[u32],
    width: usize,
    (x0, x1, y0, y1): (usize, usize, usize, usize),
    t: &Tables,
    limit: u32,
) -> u32 {
    let mut c = 0;
    for y in y0.max(1)..y1 {
        let row = &px[y * width..(y + 1) * width];
        let prev = &px[(y - 1) * width..y * width];
        for x in x0.max(1)..x1 {
            let tr = if x + 1 < width { prev[x + 1] } else { row[0] };
            c += cost_in(
                t,
                sub_pixels(row[x], predict(M, row[x - 1], prev[x], prev[x - 1], tr)),
            );
        }
        if c >= limit {
            break;
        }
    }
    c
}

fn mode_cost(
    mode: u32,
    px: &[u32],
    width: usize,
    r: (usize, usize, usize, usize),
    t: &Tables,
    limit: u32,
) -> u32 {
    match mode {
        0 => block_cost::<0>(px, width, r, t, limit),
        1 => block_cost::<1>(px, width, r, t, limit),
        2 => block_cost::<2>(px, width, r, t, limit),
        3 => block_cost::<3>(px, width, r, t, limit),
        4 => block_cost::<4>(px, width, r, t, limit),
        5 => block_cost::<5>(px, width, r, t, limit),
        6 => block_cost::<6>(px, width, r, t, limit),
        7 => block_cost::<7>(px, width, r, t, limit),
        8 => block_cost::<8>(px, width, r, t, limit),
        9 => block_cost::<9>(px, width, r, t, limit),
        10 => block_cost::<10>(px, width, r, t, limit),
        11 => block_cost::<11>(px, width, r, t, limit),
        12 => block_cost::<12>(px, width, r, t, limit),
        _ => block_cost::<13>(px, width, r, t, limit),
    }
}

#[inline(always)]
fn predicted(px: &[u32], width: usize, x: usize, y: usize, mode: u32) -> u32 {
    let i = y * width + x;
    if y == 0 {
        if x == 0 { 0xff000000 } else { px[i - 1] }
    } else if x == 0 {
        px[i - width]
    } else {
        let tr = if x + 1 < width {
            px[i - width + 1]
        } else {
            px[y * width]
        };
        predict(mode, px[i - 1], px[i - width], px[i - width - 1], tr)
    }
}

fn choose_modes(
    px: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    try_modes: &[u32],
    t: &Tables,
) -> Vec<u32> {
    let bw = 1usize << bits;
    let mw = width.div_ceil(bw);
    let mh = height.div_ceil(bw);
    let mut modes = vec![11u32; mw * mh];
    modes.par_chunks_mut(mw).enumerate().for_each(|(by, mrow)| {
        let y0 = by * bw;
        let y1 = (y0 + bw).min(height);
        for (bx, m) in mrow.iter_mut().enumerate() {
            let x0 = bx * bw;
            let x1 = (x0 + bw).min(width);
            if y1 <= 1 {
                continue;
            }
            let mut best = (u32::MAX, 11);
            for &mode in try_modes {
                let c = mode_cost(mode, px, width, (x0, x1, y0, y1), t, best.0);
                if c < best.0 {
                    best = (c, mode);
                }
            }
            *m = best.1;
        }
    });
    modes
}

fn residual_histogram(
    px: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    modes: &[u32],
) -> [[u32; 256]; 4] {
    let mw = width.div_ceil(1 << bits);
    let band = 16;
    let mut parts = vec![0u32; height.div_ceil(band) * 1024];
    parts.par_chunks_mut(1024).enumerate().for_each(|(bi, h)| {
        for y in bi * band..((bi + 1) * band).min(height) {
            for x in 0..width {
                let mode = modes[(y >> bits) * mw + (x >> bits)];
                let r = sub_pixels(px[y * width + x], predicted(px, width, x, y, mode));
                h[(r >> 24) as usize] += 1;
                h[256 + (r >> 16 & 0xff) as usize] += 1;
                h[512 + (r >> 8 & 0xff) as usize] += 1;
                h[768 + (r & 0xff) as usize] += 1;
            }
        }
    });
    let mut out = [[0u32; 256]; 4];
    for part in parts.chunks_exact(1024) {
        for (c, o) in out.iter_mut().enumerate() {
            for (v, &p) in o.iter_mut().zip(&part[c * 256..(c + 1) * 256]) {
                *v += p;
            }
        }
    }
    out
}

fn forward_predict(
    px: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    effort: u8,
) -> (Vec<u32>, Vec<u32>) {
    let try_modes: &[u32] = match effort {
        0 => &[11],
        1..=2 => &[1, 2, 11, 12, 13],
        _ => &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
    };
    let mut modes = choose_modes(px, width, height, bits, try_modes, &abs_tables());
    let rounds = match effort {
        0..=2 => 0,
        3..=4 => 1,
        _ => 2,
    };
    for _ in 0..rounds {
        let t = learned(&residual_histogram(px, width, height, bits, &modes));
        modes = choose_modes(px, width, height, bits, try_modes, &t);
    }
    let mw = width.div_ceil(1 << bits);
    let mut res = vec![0u32; width * height];
    res.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, r) in row.iter_mut().enumerate() {
            let mode = modes[(y >> bits) * mw + (x >> bits)];
            *r = sub_pixels(px[y * width + x], predicted(px, width, x, y, mode));
        }
    });
    (
        res,
        modes.into_iter().map(|m| 0xff000000 | m << 8).collect(),
    )
}

fn forward_color(px: &mut [u32], width: usize, height: usize, bits: u32, effort: u8) -> Vec<u32> {
    let bw = 1usize << bits;
    let mw = width.div_ceil(bw);
    let mut h = [[0u32; 256]; 4];
    for &p in px.iter() {
        h[1][(p >> 16 & 0xff) as usize] += 1;
        h[3][(p & 0xff) as usize] += 1;
    }
    let t = if effort >= 3 {
        learned(&h)
    } else {
        abs_tables()
    };
    let (tr, tb) = (&t[1], &t[3]);
    let mut elems = vec![0u32; mw * height.div_ceil(bw)];
    px.par_chunks_mut(width * bw)
        .zip(elems.par_chunks_mut(mw))
        .for_each(|(rows, erow)| {
            let h = rows.len() / width;
            let mut left = (0i32, 0i32, 0i32);
            for (bx, e) in erow.iter_mut().enumerate() {
                let x0 = bx * bw;
                let x1 = (x0 + bw).min(width);
                let pixels = || (0..h).flat_map(move |y| y * width + x0..y * width + x1);
                let (mut gg, mut gr, mut gb, mut rr, mut rb) = (0i64, 0i64, 0i64, 0i64, 0i64);
                for i in pixels() {
                    let p = rows[i];
                    let g = (p >> 8) as u8 as i8 as i64;
                    let r = (p >> 16) as u8 as i8 as i64;
                    let b = p as u8 as i8 as i64;
                    gg += g * g;
                    gr += g * r;
                    gb += g * b;
                    rr += r * r;
                    rb += r * b;
                }
                let q = |v: f64| (v * 32.0).round().clamp(-128.0, 127.0) as i32;
                let ls_r = if gg == 0 { 0 } else { q(gr as f64 / gg as f64) };
                let det = gg * rr - gr * gr;
                let (ls_gb, ls_rb) = if det > 0 {
                    (
                        q((gb * rr - rb * gr) as f64 / det as f64),
                        q((rb * gg - gb * gr) as f64 / det as f64),
                    )
                } else if gg > 0 {
                    (q(gb as f64 / gg as f64), 0)
                } else {
                    (0, 0)
                };
                let red_cost = |g2r: i32| -> u32 {
                    pixels()
                        .map(|i| {
                            let p = rows[i];
                            let nr =
                                ((p >> 16) as i32 - color_delta(g2r as u8, (p >> 8) as u8)) as u8;
                            tr[nr as usize] as u32
                        })
                        .sum()
                };
                let blue_cost = |g2b: i32, r2b: i32| -> u32 {
                    pixels()
                        .map(|i| {
                            let p = rows[i];
                            let nb = (p as u8 as i32
                                - color_delta(g2b as u8, (p >> 8) as u8)
                                - color_delta(r2b as u8, (p >> 16) as u8))
                                as u8;
                            tb[nb as usize] as u32
                        })
                        .sum()
                };
                let search = |cost: &dyn Fn(i32) -> u32, seeds: &[i32]| -> (u32, i32) {
                    let mut best = (u32::MAX, 0);
                    for &v in seeds {
                        let v = v.clamp(-128, 127);
                        let c = cost(v);
                        if c < best.0 {
                            best = (c, v);
                        }
                    }
                    if effort >= 3 {
                        for step in [16, 8, 4, 2, 1] {
                            for v in [best.1 - step, best.1 + step] {
                                if (-128..=127).contains(&v) {
                                    let c = cost(v);
                                    if c < best.0 {
                                        best = (c, v);
                                    }
                                }
                            }
                        }
                    }
                    best
                };
                let best_r = search(&|v| red_cost(v), &[0, ls_r, left.0]);
                let mut best_b = (blue_cost(0, 0), 0, 0);
                for &(a, b) in &[(ls_gb, ls_rb), (left.1, left.2)] {
                    let c = blue_cost(a, b);
                    if c < best_b.0 {
                        best_b = (c, a, b);
                    }
                }
                let (mut gb, mut rb) = (best_b.1, best_b.2);
                for _ in 0..if effort >= 3 { 2 } else { 0 } {
                    gb = search(&|v| blue_cost(v, rb), &[gb]).1;
                    rb = search(&|v| blue_cost(gb, v), &[rb]).1;
                }
                best_b = (0, gb, rb);
                left = (best_r.1, best_b.1, best_b.2);
                let (g2r, g2b, r2b) = (best_r.1 as u8, best_b.1 as u8, best_b.2 as u8);
                *e = 0xff000000 | (r2b as u32) << 16 | (g2b as u32) << 8 | g2r as u32;
                for i in pixels() {
                    let v = rows[i];
                    let g = (v >> 8) as u8;
                    let r = (v >> 16) as u8;
                    let nr = (r as i32 - color_delta(g2r, g)) as u8;
                    let nb = (v as u8 as i32 - color_delta(g2b, g) - color_delta(r2b, r)) as u8;
                    rows[i] = (v & 0xff00ff00) | (nr as u32) << 16 | nb as u32;
                }
            }
        });
    elems
}

fn encode_sub(w: &mut BitWriter, px: Vec<u32>, width: usize, effort: u8) {
    encode_stream(w, &px, width, false, effort.min(3));
}

fn apply_cache(tokens: &[Token], px: &[u32], cache_bits: u32, out: Option<&mut Vec<Token>>) -> f32 {
    let layout = Layout::new(cache_bits);
    let mut h = vec![0u32; layout.len()];
    let mut extra = 0u64;
    let mut cache = vec![0u32; 1 << cache_bits.max(1)];
    let shift = 32 - cache_bits;
    let mut pos = 0usize;
    let mut out = out;
    if let Some(o) = out.as_mut() {
        o.clear();
    }
    for t in tokens {
        let t2 = match *t {
            Token::Literal(p) if cache_bits > 0 => {
                let k = (p.wrapping_mul(0x1e35a7bd) >> shift) as usize;
                let r = if cache[k] == p {
                    Token::Cache(k as u32)
                } else {
                    Token::Literal(p)
                };
                cache[k] = p;
                pos += 1;
                r
            }
            Token::Copy { len, .. } => {
                if cache_bits > 0 {
                    for &p in &px[pos..pos + len as usize] {
                        cache[(p.wrapping_mul(0x1e35a7bd) >> shift) as usize] = p;
                    }
                }
                pos += len as usize;
                *t
            }
            _ => {
                pos += 1;
                *t
            }
        };
        extra += add(&mut h, layout.green, &t2) as u64;
        if let Some(o) = out.as_mut() {
            o.push(t2);
        }
    }
    bits(&h, &layout) + extra as f32
}

fn costs(h: &[u32], layout: &Layout) -> Vec<f32> {
    let mut t = vec![0f32; h.len()];
    for r in layout.segments() {
        let total: u32 = h[r.clone()].iter().sum();
        if total == 0 {
            continue;
        }
        let lt = (total as f32).log2();
        let used = h[r.clone()].iter().filter(|&&c| c > 0).count();
        for i in r {
            t[i] = if used <= 1 && h[i] > 0 {
                0.0
            } else if h[i] == 0 {
                lt + 2.0
            } else {
                lt - (h[i] as f32).log2()
            };
        }
    }
    t
}

fn prune(tokens: &mut Vec<Token>, px: &[u32]) -> bool {
    let layout = Layout::new(0);
    let g = layout.green;
    let mut h = vec![0u32; layout.len()];
    for t in tokens.iter() {
        add(&mut h, g, t);
    }
    let c = costs(&h, &layout);
    let lit = |p: u32| -> f32 {
        c[(p >> 8 & 0xff) as usize]
            + c[g + (p >> 16 & 0xff) as usize]
            + c[g + 256 + (p & 0xff) as usize]
            + c[g + 512 + (p >> 24) as usize]
    };
    let mut out = Vec::with_capacity(tokens.len());
    let mut pos = 0;
    let mut changed = false;
    for t in tokens.iter() {
        match *t {
            Token::Copy { len, dist } => {
                let (lp, le, _) = prefix(len);
                let (dp, de, _) = prefix(dist);
                let copy = c[256 + lp as usize] + le as f32 + c[g + 768 + dp as usize] + de as f32;
                let span = &px[pos..pos + len as usize];
                let mut literal = 0.0;
                for &p in span {
                    literal += lit(p);
                    if literal >= copy {
                        break;
                    }
                }
                if literal < copy {
                    out.extend(span.iter().map(|&p| Token::Literal(p)));
                    changed = true;
                } else {
                    out.push(*t);
                }
                pos += len as usize;
            }
            _ => {
                out.push(*t);
                pos += 1;
            }
        }
    }
    *tokens = out;
    changed
}

fn cache_hits(px: &[u32], cache_bits: u32) -> Vec<u16> {
    if cache_bits == 0 {
        return Vec::new();
    }
    let mut cache = vec![0u32; 1 << cache_bits];
    let shift = 32 - cache_bits;
    px.iter()
        .map(|&p| {
            let k = (p.wrapping_mul(0x1e35a7bd) >> shift) as usize;
            let hit = if cache[k] == p { k as u16 } else { u16::MAX };
            cache[k] = p;
            hit
        })
        .collect()
}

fn histo_bits(effort: u8, width: usize, height: usize) -> u32 {
    let mut b = 7u32.saturating_sub(effort as u32).clamp(2, 9);
    while b < 9 && width.div_ceil(1 << b) * height.div_ceil(1 << b) > 2600 {
        b += 1;
    }
    b
}

fn encode_stream(w: &mut BitWriter, px: &[u32], width: usize, top: bool, effort: u8) {
    let (depth, lazy, nice, rows) = match effort {
        0 => (0, false, 0, 0),
        1 => (4, false, 32, 16),
        2 => (8, false, 64, 32),
        3 => (16, false, 128, 64),
        4 => (32, true, 256, 256),
        5 => (64, true, 1024, 1024),
        _ => (256, true, 4096, 4096),
    };
    let search = Search {
        depth,
        nice,
        window: rows * width,
    };
    let found = matches(px, width, &search);
    let plane = Plane::new();
    let to_codes = |raw: &mut Vec<Token>| {
        for t in raw.iter_mut() {
            if let Token::Copy { dist, .. } = t {
                *dist = plane.code(*dist as usize, width);
            }
        }
    };
    let mut raw = greedy(px, &found, lazy);
    to_codes(&mut raw);
    if effort > 0 {
        for _ in 0..2 {
            if !prune(&mut raw, px) {
                break;
            }
        }
    }
    let candidates: &[u32] = if effort == 0 || px.len() < 64 {
        &[0]
    } else if effort <= 2 {
        &[0, 8]
    } else {
        &[0, 4, 6, 8, 10]
    };
    let pick_cache = |raw: &[Token]| -> u32 {
        candidates
            .par_iter()
            .map(|&b| {
                (
                    apply_cache(raw, px, b, None)
                        + if b > 0 { (1u64 << b) as f32 * 0.5 } else { 0.0 },
                    b,
                )
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|x| x.1)
            .unwrap_or(0)
    };
    let mut cache_bits = pick_cache(&raw);
    if effort >= 3 && top {
        for _ in 0..if effort >= 5 { 2 } else { 1 } {
            let mut cached = Vec::with_capacity(raw.len());
            apply_cache(&raw, px, cache_bits, Some(&mut cached));
            let layout = Layout::new(cache_bits);
            let g = layout.green;
            let mut h = vec![0u32; layout.len()];
            for t in cached.iter() {
                add(&mut h, g, t);
            }
            drop(cached);
            let c = costs(&h, &layout);
            let hits = cache_hits(px, cache_bits);
            let lit = |i: usize| -> f32 {
                if let Some(&k) = hits.get(i)
                    && k != u16::MAX
                {
                    return c[256 + NUM_LENGTH + k as usize];
                }
                let p = px[i];
                c[(p >> 8 & 0xff) as usize]
                    + c[g + (p >> 16 & 0xff) as usize]
                    + c[g + 256 + (p & 0xff) as usize]
                    + c[g + 512 + (p >> 24) as usize]
            };
            let len_cost: [f32; 24] = std::array::from_fn(|i| c[256 + i]);
            let dist_cost: [f32; 40] = std::array::from_fn(|i| c[g + 768 + i]);
            raw = parse(px, width, &found, lit, &len_cost, &dist_cost, &plane);
            to_codes(&mut raw);
            cache_bits = pick_cache(&raw);
        }
    }
    let mut tokens = Vec::with_capacity(raw.len());
    apply_cache(&raw, px, cache_bits, Some(&mut tokens));
    drop(raw);
    let layout = Layout::new(cache_bits);
    let height = px.len() / width;
    let max_groups = match effort {
        0 | 1 => 1,
        2 => 16,
        3 => 32,
        4 => 64,
        _ => 128,
    };
    let hbits = histo_bits(effort, width, height);
    let clusters = if top && max_groups > 1 {
        let c = cluster(&tokens, width, height, hbits, layout, max_groups);
        Some(c)
    } else {
        None
    };
    if cache_bits > 0 {
        w.put(1, 1);
        w.put(cache_bits, 4);
    } else {
        w.put(0, 1);
    }
    let (groups, map) = match clusters {
        Some(c) if c.groups.len() > 1 => {
            w.put(1, 1);
            w.put(hbits - 2, 3);
            let meta = c.map.iter().map(|&g| 0xff000000 | g << 8).collect();
            encode_sub(w, meta, width.div_ceil(1 << hbits), effort);
            (c.groups, Some(c.map))
        }
        _ => {
            if top {
                w.put(0, 1);
            }
            let mut h = vec![0u32; layout.len()];
            for t in &tokens {
                add(&mut h, layout.green, t);
            }
            (vec![h], None)
        }
    };
    let segs = layout.segments();
    let codes: Vec<[HuffCode; 5]> = groups
        .iter()
        .map(|h| std::array::from_fn(|i| HuffCode::from_histogram(&h[segs[i].clone()], 15)))
        .collect();
    for c in codes.iter().flatten() {
        write_code(w, c);
    }
    let mw = width.div_ceil(1 << hbits);
    let (mut x, mut y) = (0usize, 0usize);
    for t in &tokens {
        let c = match &map {
            Some(m) => &codes[m[(y >> hbits) * mw + (x >> hbits)] as usize],
            None => &codes[0],
        };
        match *t {
            Token::Literal(p) => {
                c[0].put(w, (p >> 8 & 0xff) as usize);
                c[1].put(w, (p >> 16 & 0xff) as usize);
                c[2].put(w, (p & 0xff) as usize);
                c[3].put(w, (p >> 24) as usize);
            }
            Token::Cache(i) => c[0].put(w, 256 + NUM_LENGTH + i as usize),
            Token::Copy { len, dist } => {
                let (lp, le, lx) = prefix(len);
                c[0].put(w, 256 + lp as usize);
                w.put(lx, le);
                let (dp, de, dx) = prefix(dist);
                c[4].put(w, dp as usize);
                w.put(dx, de);
            }
        }
        if map.is_some() {
            x += token_len(t);
            while x >= width {
                x -= width;
                y += 1;
            }
        }
    }
}
