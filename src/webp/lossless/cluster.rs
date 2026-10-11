use super::lz77::Token;
use rayon::prelude::*;

pub(super) const NUM_LENGTH: usize = 24;

#[derive(Clone, Copy)]
pub(super) struct Layout {
    pub green: usize,
}

impl Layout {
    pub fn new(cache_bits: u32) -> Self {
        Layout {
            green: 256 + NUM_LENGTH + if cache_bits > 0 { 1 << cache_bits } else { 0 },
        }
    }

    pub fn len(&self) -> usize {
        self.green + 3 * 256 + 40
    }

    pub fn segments(&self) -> [std::ops::Range<usize>; 5] {
        let g = self.green;
        [
            0..g,
            g..g + 256,
            g + 256..g + 512,
            g + 512..g + 768,
            g + 768..g + 808,
        ]
    }
}

#[inline(always)]
pub(super) fn prefix(v: u32) -> (u32, u32, u32) {
    let d = v - 1;
    if d < 4 {
        return (d, 0, 0);
    }
    let h = 31 - d.leading_zeros();
    let second = (d >> (h - 1)) & 1;
    (2 * h + second, h - 1, d & ((1 << (h - 1)) - 1))
}

#[inline(always)]
pub(super) fn add(h: &mut [u32], g: usize, t: &Token) -> u32 {
    match *t {
        Token::Literal(p) => {
            h[(p >> 8 & 0xff) as usize] += 1;
            h[g + (p >> 16 & 0xff) as usize] += 1;
            h[g + 256 + (p & 0xff) as usize] += 1;
            h[g + 512 + (p >> 24) as usize] += 1;
            0
        }
        Token::Cache(i) => {
            h[256 + NUM_LENGTH + i as usize] += 1;
            0
        }
        Token::Copy { len, dist } => {
            let (lp, le, _) = prefix(len);
            let (dp, de, _) = prefix(dist);
            h[256 + lp as usize] += 1;
            h[g + 768 + dp as usize] += 1;
            le + de
        }
    }
}

#[inline(always)]
pub(super) fn token_len(t: &Token) -> usize {
    match *t {
        Token::Copy { len, .. } => len as usize,
        _ => 1,
    }
}

static NLOGN: std::sync::LazyLock<Vec<f32>> = std::sync::LazyLock::new(|| {
    (0..4096)
        .map(|c| {
            if c == 0 {
                0.0
            } else {
                c as f32 * (c as f32).log2()
            }
        })
        .collect()
});

#[inline(always)]
fn nlogn(c: u32) -> f32 {
    if (c as usize) < 4096 {
        NLOGN[c as usize]
    } else {
        c as f32 * (c as f32).log2()
    }
}

pub(super) fn segment_bits(h: &[u32]) -> f32 {
    let mut total = 0u32;
    let mut sum = 0.0;
    let mut used = 0u32;
    for &c in h {
        if c > 0 {
            total += c;
            sum += nlogn(c);
            used += 1;
        }
    }
    if used <= 1 {
        return 12.0;
    }
    nlogn(total) - sum + used as f32 * 5.0 + 40.0
}

pub(super) fn bits(h: &[u32], layout: &Layout) -> f32 {
    layout
        .segments()
        .into_iter()
        .map(|r| segment_bits(&h[r]))
        .sum()
}

fn merged_bits(a: &[u32], b: &[u32], layout: &Layout, scratch: &mut Vec<u32>) -> f32 {
    scratch.clear();
    scratch.extend(a.iter().zip(b).map(|(x, y)| x + y));
    bits(scratch, layout)
}

pub(super) struct Clusters {
    pub groups: Vec<Vec<u32>>,
    pub map: Vec<u32>,
}

pub(super) fn cluster(
    tokens: &[Token],
    width: usize,
    height: usize,
    hbits: u32,
    layout: Layout,
    max_groups: usize,
) -> Clusters {
    let mw = width.div_ceil(1 << hbits);
    let mh = height.div_ceil(1 << hbits);
    let n = mw * mh;
    let len = layout.len();
    let g = layout.green;
    let mut blocks = vec![0u32; n * len];
    let (mut x, mut y) = (0usize, 0usize);
    for t in tokens {
        let b = (y >> hbits) * mw + (x >> hbits);
        add(&mut blocks[b * len..(b + 1) * len], g, t);
        x += token_len(t);
        while x >= width {
            x -= width;
            y += 1;
        }
    }
    let sparse: Vec<Vec<(u16, u32)>> = blocks
        .par_chunks(len)
        .map(|h| {
            h.iter()
                .enumerate()
                .filter(|&(_, &c)| c > 0)
                .map(|(i, &c)| (i as u16, c))
                .collect()
        })
        .collect();
    let live: Vec<usize> = (0..n).filter(|&b| !sparse[b].is_empty()).collect();
    let k = max_groups.min(live.len()).max(1);
    let mut order: Vec<(f32, usize)> = live
        .par_iter()
        .map(|&b| {
            let h = &blocks[b * len..(b + 1) * len];
            let toks: u32 = h[..g].iter().sum();
            (bits(h, &layout) / toks.max(1) as f32, b)
        })
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut assign = vec![0u32; n];
    for (i, &(_, b)) in order.iter().enumerate() {
        assign[b] = (i * k / order.len()) as u32;
    }
    let mut groups = vec![vec![0u32; len]; k];
    rebuild(&live, &sparse, &assign, &mut groups);
    refine(&mut groups, &mut assign, &live, &sparse, &layout, 3);
    let mut alive: Vec<bool> = groups.iter().map(|gr| gr.iter().any(|&c| c > 0)).collect();
    let mut cost: Vec<f32> = groups.par_iter().map(|gr| bits(gr, &layout)).collect();
    let kk = groups.len();
    let mut pair = vec![f32::MAX; kk * kk];
    let pairs: Vec<(usize, usize)> = (0..kk)
        .flat_map(|i| (i + 1..kk).map(move |j| (i, j)))
        .filter(|&(i, j)| alive[i] && alive[j])
        .collect();
    let deltas: Vec<f32> = pairs
        .par_iter()
        .map_init(Vec::new, |s, &(i, j)| {
            merged_bits(&groups[i], &groups[j], &layout, s) - cost[i] - cost[j]
        })
        .collect();
    for (&(i, j), &d) in pairs.iter().zip(&deltas) {
        pair[i * kk + j] = d;
    }
    let mut scratch = Vec::new();
    loop {
        let mut best = (0.0f32, 0, 0);
        for i in 0..kk {
            if !alive[i] {
                continue;
            }
            for j in i + 1..kk {
                if alive[j] && pair[i * kk + j] < best.0 {
                    best = (pair[i * kk + j], i, j);
                }
            }
        }
        if best.0 >= 0.0 {
            break;
        }
        let (_, i, j) = best;
        let (lo, hi) = groups.split_at_mut(j);
        for (a, b) in lo[i].iter_mut().zip(hi[0].iter()) {
            *a += b;
        }
        alive[j] = false;
        for a in assign.iter_mut() {
            if *a == j as u32 {
                *a = i as u32;
            }
        }
        cost[i] = bits(&groups[i], &layout);
        for (o, &live) in alive.iter().enumerate() {
            if o == i || !live {
                continue;
            }
            let (a, b) = (o.min(i), o.max(i));
            pair[a * kk + b] =
                merged_bits(&groups[a], &groups[b], &layout, &mut scratch) - cost[a] - cost[b];
        }
    }
    let mut remap = vec![u32::MAX; kk];
    let mut out = Vec::new();
    for (i, gr) in groups.into_iter().enumerate() {
        if alive[i] {
            remap[i] = out.len() as u32;
            out.push(gr);
        }
    }
    for a in assign.iter_mut() {
        *a = remap[*a as usize].min(out.len() as u32 - 1);
    }
    refine(&mut out, &mut assign, &live, &sparse, &layout, 2);
    let used: Vec<bool> = out.iter().map(|g| g.iter().any(|&c| c > 0)).collect();
    if used.iter().all(|&u| u) {
        return Clusters {
            groups: out,
            map: assign,
        };
    }
    let mut remap = vec![0u32; out.len()];
    let mut kept = Vec::new();
    for (i, g) in out.into_iter().enumerate() {
        if used[i] {
            remap[i] = kept.len() as u32;
            kept.push(g);
        }
    }
    let map = assign.iter().map(|&a| remap[a as usize]).collect();
    Clusters { groups: kept, map }
}

fn rebuild(live: &[usize], sparse: &[Vec<(u16, u32)>], assign: &[u32], groups: &mut [Vec<u32>]) {
    for gr in groups.iter_mut() {
        gr.fill(0);
    }
    for &b in live {
        let gr = &mut groups[assign[b] as usize];
        for &(i, c) in &sparse[b] {
            gr[i as usize] += c;
        }
    }
}

fn refine(
    groups: &mut [Vec<u32>],
    assign: &mut [u32],
    live: &[usize],
    sparse: &[Vec<(u16, u32)>],
    layout: &Layout,
    rounds: usize,
) {
    let len = layout.len();
    for _ in 0..rounds {
        let tables: Vec<Vec<f32>> = groups
            .par_iter()
            .map(|gr| {
                let mut t = vec![0f32; len];
                for r in layout.segments() {
                    let total: u32 = gr[r.clone()].iter().sum();
                    let lt = ((total as f32) + 1.0).log2();
                    for i in r {
                        t[i] = lt - (gr[i] as f32 + 0.15).log2();
                    }
                }
                t
            })
            .collect();
        let next: Vec<(usize, u32)> = live
            .par_iter()
            .map(|&b| {
                let mut best = (f32::MAX, 0u32);
                for (gi, t) in tables.iter().enumerate() {
                    let mut c = 0.0;
                    for &(i, n) in &sparse[b] {
                        c += n as f32 * t[i as usize];
                    }
                    if c < best.0 {
                        best = (c, gi as u32);
                    }
                }
                (b, best.1)
            })
            .collect();
        let mut changed = false;
        for (b, gi) in next {
            changed |= assign[b] != gi;
            assign[b] = gi;
        }
        rebuild(live, sparse, assign, groups);
        if !changed {
            break;
        }
    }
}
