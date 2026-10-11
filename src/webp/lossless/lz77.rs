use super::decode::PLANE;
use rayon::prelude::*;

pub(super) const MAX_LEN: usize = 4096;
const MAX_DIST: usize = (1 << 20) - 120;
const HASH_BITS: u32 = 18;

#[derive(Clone, Copy, Debug)]
pub(super) enum Token {
    Literal(u32),
    Cache(u32),
    Copy { len: u32, dist: u32 },
}

pub(super) struct Plane {
    lut: [u8; 16 * 9],
}

impl Plane {
    pub fn new() -> Self {
        let mut lut = [0u8; 16 * 9];
        for (i, &(xi, yi)) in PLANE.iter().enumerate() {
            lut[yi as usize * 16 + (xi + 8) as usize] = i as u8 + 1;
        }
        Plane { lut }
    }

    #[inline(always)]
    pub fn code(&self, dist: usize, width: usize) -> u32 {
        let yo = dist / width;
        let xo = dist - yo * width;
        if xo <= 8 && yo < 8 {
            let c = self.lut[yo * 16 + xo + 8];
            if c != 0 {
                return c as u32;
            }
        }
        if xo + 8 > width && xo > 0 && yo < 7 {
            let xi = xo as isize - width as isize;
            if xi >= -8 {
                let c = self.lut[(yo + 1) * 16 + (xi + 8) as usize];
                if c != 0 {
                    return c as u32;
                }
            }
        }
        dist as u32 + 120
    }
}

#[inline(always)]
fn match_len(px: &[u32], a: usize, b: usize, max: usize) -> usize {
    let x = &px[a..a + max];
    let y = &px[b..b + max];
    let mut n = 0;
    while n + 4 <= max {
        if x[n..n + 4] != y[n..n + 4] {
            break;
        }
        n += 4;
    }
    while n < max && x[n] == y[n] {
        n += 1;
    }
    n
}

#[inline(always)]
fn hash(px: &[u32], i: usize) -> usize {
    let h = px[i].wrapping_mul(0x9e3779b1) ^ px[i + 1].wrapping_mul(0x85ebca6b).rotate_left(13);
    (h.wrapping_mul(0xc2b2ae35) >> (32 - HASH_BITS)) as usize
}

pub(super) struct Search {
    pub depth: usize,
    pub nice: usize,
    pub window: usize,
}

#[inline(always)]
fn hash_long(px: &[u32], i: usize) -> usize {
    let mut h = 0u32;
    for &p in &px[i..i + LONG] {
        h = (h ^ p).wrapping_mul(0x9e3779b1).rotate_left(5);
    }
    (h.wrapping_mul(0x85ebca6b) >> (32 - HASH_BITS)) as usize
}

const LONG: usize = 8;

fn chain(px: &[u32], span: usize, hash: fn(&[u32], usize) -> usize) -> Vec<i32> {
    let n = px.len();
    let mut head = vec![-1i32; 1 << HASH_BITS];
    let mut prev = vec![-1i32; n];
    for (i, p) in prev
        .iter_mut()
        .enumerate()
        .take((n + 1).saturating_sub(span))
    {
        let h = hash(px, i);
        *p = head[h];
        head[h] = i as i32;
    }
    prev
}

pub(super) fn matches(px: &[u32], width: usize, s: &Search) -> Vec<(u32, u32)> {
    let n = px.len();
    let (prev, long) = if s.depth > 0 {
        rayon::join(|| chain(px, 2, hash), || chain(px, LONG, hash_long))
    } else {
        (Vec::new(), Vec::new())
    };
    let window = s.window.min(MAX_DIST);
    let nice = s.nice.max(2);
    let mut out = vec![(0u32, 0u32); n];
    out.par_chunks_mut(8192)
        .enumerate()
        .for_each(|(ci, chunk)| {
            let start = ci * 8192;
            let mut k = 0;
            while k < chunk.len() {
                let i = start + k;
                let (len, dist) = best(px, width, i, &prev, &long, s.depth, nice, window);
                chunk[k] = (len as u32, dist as u32);
                k += 1;
                if len > 16 {
                    let run = (len - 16).min(chunk.len() - k);
                    for j in 0..run {
                        chunk[k + j] = ((len - 1 - j) as u32, dist as u32);
                    }
                    k += run;
                }
            }
        });
    out
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn best(
    px: &[u32],
    width: usize,
    i: usize,
    prev: &[i32],
    long: &[i32],
    depth: usize,
    nice: usize,
    window: usize,
) -> (usize, usize) {
    let n = px.len();
    let max = (n - i).min(MAX_LEN);
    if max < 2 {
        return (0, 0);
    }
    let mut best_len = 0;
    let mut best_dist = 0;
    if i >= 1 {
        best_len = match_len(px, i, i - 1, max);
        best_dist = 1;
    }
    if i >= width && best_len < max {
        let l = match_len(px, i, i - width, max);
        if l > best_len + 1 || (l > best_len && best_dist != 1) {
            best_len = l;
            best_dist = width;
        }
    }
    let floor = i.saturating_sub(window) as i32;
    if depth > 0 && best_len < max.min(nice) && max >= LONG {
        let mut cand = long[i];
        let mut left = depth;
        while cand >= floor && cand >= 0 && left > 0 {
            let c = cand as usize;
            let k = best_len.min(max - 1);
            if px[c + k] == px[i + k] {
                let l = match_len(px, i, c, max);
                if l > best_len + 1 || (l > best_len && i - c < best_dist) {
                    best_len = l;
                    best_dist = i - c;
                    if l >= max.min(nice) {
                        break;
                    }
                }
            }
            cand = long[c];
            left -= 1;
        }
    }
    if depth > 0 && best_len < max.min(nice) && best_len < LONG {
        let mut cand = prev[i];
        let mut left = depth;
        while cand >= floor && cand >= 0 && left > 0 {
            let c = cand as usize;
            let k = best_len.min(max - 1);
            if px[c + k] == px[i + k] {
                let l = match_len(px, i, c, max);
                if l > best_len + 1 {
                    best_len = l;
                    best_dist = i - c;
                    if l >= max.min(nice) {
                        break;
                    }
                }
            }
            cand = prev[c];
            left -= 1;
        }
    }
    if best_len >= 3 || (best_len == 2 && (best_dist == 1 || best_dist == width)) {
        (best_len, best_dist)
    } else {
        (0, 0)
    }
}

pub(super) fn greedy(px: &[u32], matches: &[(u32, u32)], lazy: bool) -> Vec<Token> {
    let n = px.len();
    let mut out = Vec::with_capacity(n / 2);
    let mut i = 0;
    while i < n {
        let (len, dist) = matches[i];
        if len >= 2 {
            if lazy && len < 32 && i + 1 < n && matches[i + 1].0 > len + 1 {
                out.push(Token::Literal(px[i]));
                i += 1;
                continue;
            }
            out.push(Token::Copy { len, dist });
            i += len as usize;
        } else {
            out.push(Token::Literal(px[i]));
            i += 1;
        }
    }
    out
}

pub(super) fn parse(
    px: &[u32],
    width: usize,
    matches: &[(u32, u32)],
    lit: impl Fn(usize) -> f32,
    len_cost: &[f32; 24],
    dist_cost: &[f32; 40],
    plane: &Plane,
) -> Vec<Token> {
    let n = px.len();
    let mut cost = vec![f32::MAX; n + 1];
    let mut from = vec![0u32; n + 1];
    cost[0] = 0.0;
    let prefix_cost = |v: u32, table: &[f32]| -> f32 {
        let d = v - 1;
        if d < 4 {
            return table[d as usize];
        }
        let h = 31 - d.leading_zeros();
        let second = (d >> (h - 1)) & 1;
        table[(2 * h + second) as usize] + (h - 1) as f32
    };
    let mut lcost = [0f32; MAX_LEN + 1];
    for (l, c) in lcost.iter_mut().enumerate().skip(1) {
        *c = prefix_cost(l as u32, len_cost);
    }
    for i in 0..n {
        let base = cost[i];
        let c = base + lit(i);
        if c < cost[i + 1] {
            cost[i + 1] = c;
            from[i + 1] = 1;
        }
        let (len, dist) = matches[i];
        if len >= 2 {
            let len = len as usize;
            let code = plane.code(dist as usize, width);
            let dc = base + prefix_cost(code, dist_cost);
            let top = len.min(32);
            for l in 2..=top {
                let c = dc + lcost[l];
                if c < cost[i + l] {
                    cost[i + l] = c;
                    from[i + l] = l as u32;
                }
            }
            if len > top {
                let c = dc + lcost[len];
                if c < cost[i + len] {
                    cost[i + len] = c;
                    from[i + len] = len as u32;
                }
            }
        }
    }
    let mut steps = Vec::new();
    let mut i = n;
    while i > 0 {
        let l = from[i] as usize;
        steps.push(l);
        i -= l;
    }
    let mut out = Vec::with_capacity(steps.len());
    let mut pos = 0;
    for &l in steps.iter().rev() {
        if l == 1 {
            out.push(Token::Literal(px[pos]));
        } else {
            out.push(Token::Copy {
                len: l as u32,
                dist: matches[pos].1,
            });
        }
        pos += l;
    }
    out
}
