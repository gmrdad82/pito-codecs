use rayon::prelude::*;

use crate::h264::enc::dsp::{sad, satd};
use crate::h264::enc::lookahead::LowRes;

const STRENGTH: f32 = 2.0;
const SEARCH_STEPS: usize = 16;

#[derive(Clone, Copy, Default)]
struct Block {
    intra: u32,
    inter: u32,
    list: u8,
    mv: [(i16, i16); 2],
}

pub struct Costs {
    blocks: Vec<Block>,
}

fn intra_cost(cur: &LowRes, x0: usize, y0: usize) -> u32 {
    let mut blk = [0u8; 64];
    let mut best = u32::MAX;
    let base = y0 * cur.w + x0;
    for mode in 0..3 {
        if (mode == 1 && x0 == 0) || (mode == 2 && y0 == 0) {
            continue;
        }
        for y in 0..8 {
            for x in 0..8 {
                blk[y * 8 + x] = match mode {
                    0 => {
                        if x0 > 0 && y0 > 0 {
                            ((cur.data[(y0 - 1) * cur.w + x0 + x] as u32
                                + cur.data[(y0 + y) * cur.w + x0 - 1] as u32)
                                / 2) as u8
                        } else {
                            128
                        }
                    }
                    1 => cur.data[(y0 + y) * cur.w + x0 - 1],
                    _ => cur.data[(y0 - 1) * cur.w + x0 + x],
                };
            }
        }
        best = best.min(satd(&cur.data[base..], cur.w, &blk, 8, 8, 8));
    }
    best + 16
}

fn search(cur: &LowRes, r: &LowRes, x0: usize, y0: usize, seeds: &[(i16, i16)]) -> (i16, i16) {
    let base = y0 * cur.w + x0;
    let (xmax, ymax) = (r.w as i32 - 8, r.h as i32 - 8);
    let cost = |m: (i16, i16)| -> u32 {
        let (x, y) = (x0 as i32 + m.0 as i32, y0 as i32 + m.1 as i32);
        if x < 0 || y < 0 || x > xmax || y > ymax {
            return u32::MAX;
        }
        let mvc = (m.0.unsigned_abs() + m.1.unsigned_abs()) as u32;
        sad(
            &cur.data[base..],
            cur.w,
            &r.data[y as usize * r.w + x as usize..],
            r.w,
            8,
            8,
        ) + mvc
    };
    let mut best = (0i16, 0i16);
    let mut bc = cost(best);
    for &s in seeds {
        let c = cost(s);
        if c < bc {
            bc = c;
            best = s;
        }
    }
    for _ in 0..SEARCH_STEPS {
        let c = best;
        for d in [
            (-1, 0),
            (1, 0),
            (0, -1),
            (0, 1),
            (-2, -2),
            (2, -2),
            (-2, 2),
            (2, 2),
        ] {
            let m = (c.0 + d.0, c.1 + d.1);
            let k = cost(m);
            if k < bc {
                bc = k;
                best = m;
            }
        }
        if best == c {
            break;
        }
    }
    best
}

fn fetch(r: &LowRes, x0: usize, y0: usize, m: (i16, i16), out: &mut [u8; 64]) {
    let x = (x0 as i32 + m.0 as i32) as usize;
    let y = (y0 as i32 + m.1 as i32) as usize;
    for row in 0..8 {
        let o = (y + row) * r.w + x;
        out[row * 8..row * 8 + 8].copy_from_slice(&r.data[o..o + 8]);
    }
}

pub fn analyse(cur: &LowRes, l0: Option<&LowRes>, l1: Option<&LowRes>) -> Costs {
    let (bw, bh) = (cur.w / 8, cur.h / 8);
    let mut blocks = vec![Block::default(); bw * bh];
    blocks
        .par_chunks_mut(bw.max(1))
        .enumerate()
        .for_each(|(by, row)| {
            let mut left = [(0i16, 0i16); 2];
            let mut p0 = [0u8; 64];
            let mut p1 = [0u8; 64];
            let mut bi = [0u8; 64];
            for (bx, b) in row.iter_mut().enumerate() {
                let (x0, y0) = (bx * 8, by * 8);
                let base = y0 * cur.w + x0;
                let intra = intra_cost(cur, x0, y0);
                let mut best = (intra, 0u8, [(0i16, 0i16); 2]);
                let mut mvs = [(0i16, 0i16); 2];
                for (l, r) in [l0, l1].into_iter().enumerate() {
                    let Some(r) = r else { continue };
                    let m = search(cur, r, x0, y0, &[left[l]]);
                    mvs[l] = m;
                    let p = if l == 0 { &mut p0 } else { &mut p1 };
                    fetch(r, x0, y0, m, p);
                    let c = satd(&cur.data[base..], cur.w, p, 8, 8, 8) + 8;
                    if c < best.0 {
                        best = (c, 1 + l as u8, mvs);
                    }
                }
                if l0.is_some() && l1.is_some() {
                    for i in 0..64 {
                        bi[i] = ((p0[i] as u16 + p1[i] as u16 + 1) >> 1) as u8;
                    }
                    let c = satd(&cur.data[base..], cur.w, &bi, 8, 8, 8) + 12;
                    if c < best.0 {
                        best = (c, 3, mvs);
                    }
                }
                left = mvs;
                *b = Block {
                    intra,
                    inter: best.0,
                    list: best.1,
                    mv: best.2,
                };
            }
        });
    Costs { blocks }
}

pub struct Node<'a> {
    pub costs: &'a Costs,
    pub refs: [Option<usize>; 2],
}

pub fn propagate(nodes: &[Node], bw: usize, bh: usize) -> Vec<Vec<f32>> {
    let mut prop: Vec<Vec<f32>> = nodes.iter().map(|_| vec![0f32; bw * bh]).collect();
    for i in (0..nodes.len()).rev() {
        let n = &nodes[i];
        let (before, rest) = prop.split_at_mut(i);
        let mine = &rest[0];
        for by in 0..bh {
            for bx in 0..bw {
                let k = by * bw + bx;
                let b = n.costs.blocks[k];
                if b.list == 0 || b.intra == 0 {
                    continue;
                }
                let intra = b.intra as f32;
                let inter = (b.inter as f32).min(intra);
                let amount = (intra + mine[k]) * (intra - inter) / intra;
                if amount <= 0.0 {
                    continue;
                }
                let lists: &[(usize, f32)] = match b.list {
                    1 => &[(0, 1.0)],
                    2 => &[(1, 1.0)],
                    _ => &[(0, 0.5), (1, 0.5)],
                };
                for &(l, w) in lists {
                    let Some(r) = n.refs[l] else { continue };
                    if r >= i {
                        continue;
                    }
                    let dst = &mut before[r];
                    let x = (bx * 8) as i32 + b.mv[l].0 as i32;
                    let y = (by * 8) as i32 + b.mv[l].1 as i32;
                    let (cx, cy) = (x.div_euclid(8), y.div_euclid(8));
                    let (fx, fy) = (x.rem_euclid(8), y.rem_euclid(8));
                    let a = amount * w / 64.0;
                    for (dx, dy, area) in [
                        (0, 0, (8 - fx) * (8 - fy)),
                        (1, 0, fx * (8 - fy)),
                        (0, 1, (8 - fx) * fy),
                        (1, 1, fx * fy),
                    ] {
                        let (tx, ty) = (cx + dx, cy + dy);
                        if area == 0 || tx < 0 || ty < 0 || tx >= bw as i32 || ty >= bh as i32 {
                            continue;
                        }
                        dst[ty as usize * bw + tx as usize] += a * area as f32;
                    }
                }
            }
        }
    }
    prop
}

pub fn offsets(costs: &Costs, prop: &[f32]) -> Vec<f32> {
    costs
        .blocks
        .iter()
        .zip(prop)
        .map(|(b, &p)| {
            let intra = b.intra.max(1) as f32;
            -STRENGTH * ((intra + p) / intra).log2()
        })
        .collect()
}
