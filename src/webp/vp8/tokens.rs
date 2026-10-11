use super::bits::BoolEncoder;
use super::dec::Probs;
use super::tables::{BANDS, CAT3, CAT4, CAT5, CAT6, COEFF_UPDATE_PROBS, ZIGZAG};
use std::sync::LazyLock;

pub trait Sink {
    fn node(&mut self, t: usize, n: usize, ctx: usize, i: usize, bit: bool);
    fn fixed(&mut self, bit: bool, prob: u8);
}

#[inline(always)]
fn extra<S: Sink>(s: &mut S, v: u32, probs: &[u8]) {
    let k = probs.len();
    for (j, &p) in probs.iter().enumerate() {
        s.fixed((v >> (k - 1 - j)) & 1 != 0, p);
    }
}

#[inline(always)]
pub fn last_nonzero(levels: &[i16; 16], first: usize) -> Option<usize> {
    let m = super::dsp::nz_zigzag(levels) & (0xffffu16 << first);
    (m != 0).then(|| 15 - m.leading_zeros() as usize)
}

#[inline(always)]
pub fn block<S: Sink>(s: &mut S, t: usize, levels: &[i16; 16], first: usize, ctx: usize) -> bool {
    let Some(last) = last_nonzero(levels, first) else {
        s.node(t, first, ctx, 0, false);
        return false;
    };
    let mut ctx = ctx;
    let mut after_zero = false;
    for n in first..=last {
        if !after_zero {
            s.node(t, n, ctx, 0, true);
        }
        let l = levels[ZIGZAG[n]];
        let v = l.unsigned_abs() as u32;
        if v == 0 {
            s.node(t, n, ctx, 1, false);
            ctx = 0;
            after_zero = true;
            continue;
        }
        after_zero = false;
        s.node(t, n, ctx, 1, true);
        if v == 1 {
            s.node(t, n, ctx, 2, false);
        } else {
            s.node(t, n, ctx, 2, true);
            if v <= 4 {
                s.node(t, n, ctx, 3, false);
                if v == 2 {
                    s.node(t, n, ctx, 4, false);
                } else {
                    s.node(t, n, ctx, 4, true);
                    s.node(t, n, ctx, 5, v == 4);
                }
            } else if v <= 10 {
                s.node(t, n, ctx, 3, true);
                s.node(t, n, ctx, 6, false);
                if v <= 6 {
                    s.node(t, n, ctx, 7, false);
                    s.fixed(v == 6, 159);
                } else {
                    s.node(t, n, ctx, 7, true);
                    extra(s, v - 7, &[165, 145]);
                }
            } else {
                s.node(t, n, ctx, 3, true);
                s.node(t, n, ctx, 6, true);
                if v <= 34 {
                    s.node(t, n, ctx, 8, false);
                    if v <= 18 {
                        s.node(t, n, ctx, 9, false);
                        extra(s, v - 11, &CAT3);
                    } else {
                        s.node(t, n, ctx, 9, true);
                        extra(s, v - 19, &CAT4);
                    }
                } else {
                    s.node(t, n, ctx, 8, true);
                    if v <= 66 {
                        s.node(t, n, ctx, 10, false);
                        extra(s, v - 35, &CAT5);
                    } else {
                        s.node(t, n, ctx, 10, true);
                        extra(s, v - 67, &CAT6);
                    }
                }
            }
        }
        s.fixed(l < 0, 128);
        ctx = if v == 1 { 1 } else { 2 };
    }
    if last < 15 {
        s.node(t, last + 1, ctx, 0, false);
    }
    true
}

pub struct Writer<'a> {
    pub bw: &'a mut BoolEncoder,
    pub probs: &'a Probs,
}

impl Sink for Writer<'_> {
    #[inline(always)]
    fn node(&mut self, t: usize, n: usize, ctx: usize, i: usize, bit: bool) {
        self.bw.put(bit, self.probs[t][n][ctx][i]);
    }

    #[inline(always)]
    fn fixed(&mut self, bit: bool, prob: u8) {
        self.bw.put(bit, prob);
    }
}

pub type Counts = [[[[[u32; 2]; 11]; 3]; 8]; 4];
pub type Table = [[[[u8; 11]; 3]; 8]; 4];
pub type Flags = [[[[bool; 11]; 3]; 8]; 4];

pub struct Counter<'a> {
    pub counts: &'a mut Counts,
}

impl Sink for Counter<'_> {
    #[inline(always)]
    fn node(&mut self, t: usize, n: usize, ctx: usize, i: usize, bit: bool) {
        self.counts[t][BANDS[n]][ctx][i][bit as usize] += 1;
    }

    #[inline(always)]
    fn fixed(&mut self, _bit: bool, _prob: u8) {}
}

pub static COST: LazyLock<[[u16; 256]; 2]> = LazyLock::new(|| {
    let mut t = [[0u16; 256]; 2];
    let [zero, one] = &mut t;
    for (p, (z, o)) in zero.iter_mut().zip(one.iter_mut()).enumerate() {
        let q = (p.max(1) as f64) / 256.0;
        *z = (-q.log2() * 256.0).round() as u16;
        *o = (-(1.0 - q).log2() * 256.0).round() as u16;
    }
    t
});

#[inline(always)]
pub fn bit_cost(bit: bool, p: u8) -> u32 {
    COST[bit as usize][p as usize] as u32
}

pub struct Coster<'a> {
    pub probs: &'a [[[u8; 11]; 3]; 17],
    pub cost: u32,
}

impl Sink for Coster<'_> {
    #[inline(always)]
    fn node(&mut self, _t: usize, n: usize, ctx: usize, i: usize, bit: bool) {
        self.cost += bit_cost(bit, self.probs[n][ctx][i]);
    }

    #[inline(always)]
    fn fixed(&mut self, bit: bool, prob: u8) {
        self.cost += bit_cost(bit, prob);
    }
}

#[inline(always)]
pub fn block_cost(
    probs: &[[[u8; 11]; 3]; 17],
    levels: &[i16; 16],
    first: usize,
    ctx: usize,
) -> u32 {
    let mut c = Coster { probs, cost: 0 };
    block(&mut c, 0, levels, first, ctx);
    c.cost
}

fn branch_cost(c0: u32, c1: u32, p: u8) -> u64 {
    c0 as u64 * bit_cost(false, p) as u64 + c1 as u64 * bit_cost(true, p) as u64
}

pub fn choose_probs(counts: &Counts, base: &Table) -> (Table, Flags) {
    let mut probs = *base;
    let mut update = [[[[false; 11]; 3]; 8]; 4];
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for i in 0..11 {
                    let [c0, c1] = counts[t][b][c][i];
                    let total = c0 + c1;
                    let up = COEFF_UPDATE_PROBS[t][b][c][i];
                    if total == 0 {
                        continue;
                    }
                    let p_new =
                        ((255 * c0 as u64 + total as u64 / 2) / total as u64).clamp(1, 255) as u8;
                    let old = branch_cost(c0, c1, base[t][b][c][i]) + bit_cost(false, up) as u64;
                    let new = branch_cost(c0, c1, p_new) + bit_cost(true, up) as u64 + 8 * 256;
                    if new < old {
                        probs[t][b][c][i] = p_new;
                        update[t][b][c][i] = true;
                    }
                }
            }
        }
    }
    (probs, update)
}

pub fn expand(p: &Table) -> Probs {
    super::dec::expand(p)
}
