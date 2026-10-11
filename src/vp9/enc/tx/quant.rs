use super::tables::{CAT_PROBS, ENERGY_CLASS};
use super::{Quant, TokenCosts};
use std::sync::LazyLock;

pub const MAX_LEVEL: i32 = 16450;

struct Costs {
    bit: [[u16; 2]; 256],
    extra: [u32; 67],
    cat6: [[u32; 2]; 14],
}

static COSTS: LazyLock<Costs> = LazyLock::new(|| {
    let mut bit = [[0u16; 2]; 256];
    for (p, b) in bit.iter_mut().enumerate().skip(1) {
        let p0 = p as f64 / 256.0;
        b[0] = (-p0.log2() * 256.0).round() as u16;
        b[1] = (-(1.0 - p0).log2() * 256.0).round() as u16;
    }
    let mut extra = [0u32; 67];
    for (l, e) in extra.iter_mut().enumerate() {
        let (cat, base) = match l {
            0..=4 => (0usize, 0usize),
            5..=6 => (1, 5),
            7..=10 => (2, 7),
            11..=18 => (3, 11),
            19..=34 => (4, 19),
            _ => (5, 35),
        };
        let probs = CAT_PROBS[cat];
        let v = l.saturating_sub(base);
        *e = probs
            .iter()
            .enumerate()
            .map(|(k, &p)| bit[p as usize][(v >> (probs.len() - 1 - k)) & 1] as u32)
            .sum();
    }
    let mut cat6 = [[0u32; 2]; 14];
    for (k, c) in cat6.iter_mut().enumerate() {
        let p = CAT_PROBS[6][k] as usize;
        *c = [bit[p][0] as u32, bit[p][1] as u32];
    }
    Costs { bit, extra, cat6 }
});

#[inline(always)]
pub fn token(l: u32) -> usize {
    match l {
        0..=4 => l as usize,
        5..=6 => 5,
        7..=10 => 6,
        11..=18 => 7,
        19..=34 => 8,
        35..=66 => 9,
        _ => 10,
    }
}

#[inline(always)]
fn level_cost(c: &Costs, tok_cost: &[u32; 11], l: u32) -> u32 {
    if l == 0 {
        return tok_cost[0];
    }
    let t = token(l);
    let extra = if t < 10 {
        c.extra[l as usize]
    } else {
        let v = l - 67;
        (0..14)
            .map(|k| c.cat6[k][((v >> (13 - k)) & 1) as usize])
            .sum()
    };
    tok_cost[t] + extra + 256
}

pub fn bit_cost(p: u8, bit: bool) -> u32 {
    COSTS.bit[p as usize][bit as usize] as u32
}

pub struct Scan<'a> {
    pub scan: &'a [u16],
    pub nb: &'a [(u16, u16)],
    pub bands: &'a [u8],
    pub iscan_t: &'a [u16],
    pub log2n: usize,
}

impl Scan<'_> {
    #[inline(always)]
    pub fn tpos(&self, pos: usize) -> usize {
        let m = (1 << self.log2n) - 1;
        ((pos & m) << self.log2n) | (pos >> self.log2n)
    }
}

#[inline(always)]
fn recip(dq: i32) -> (u32, u32) {
    let shift = 14 + (dq as u32).ilog2();
    (((1u64 << shift) / dq as u64) as u32 + 1, shift)
}

#[inline(always)]
fn ctx_at(tc: &[u8], s: &Scan, c: usize, ctx0: usize) -> usize {
    if c == 0 {
        ctx0
    } else {
        let (a, b) = s.nb[c];
        ((1 + tc[a as usize] + tc[b as usize]) >> 1) as usize
    }
}

pub fn rate<const N2: usize>(
    levels: &[i16],
    eob: usize,
    s: &Scan,
    ctx0: usize,
    costs: &TokenCosts,
) -> u32 {
    let c = &*COSTS;
    let n2 = s.scan.len();
    let mut tc = [0u8; N2];
    let mut r = 0u32;
    let mut check = true;
    for (k, &lv) in levels[..eob].iter().enumerate() {
        let band = s.bands[k] as usize;
        let ctx = ctx_at(&tc, s, k, ctx0);
        if check {
            r += costs.more[band][ctx][1];
        }
        let l = lv.unsigned_abs() as u32;
        r += level_cost(c, &costs.token[band][ctx], l);
        tc[s.scan[k] as usize] = ENERGY_CLASS[token(l)];
        check = l != 0;
    }
    if eob < n2 {
        let band = s.bands[eob] as usize;
        r += costs.more[band][ctx_at(&tc, s, eob, ctx0)][0];
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub fn quantise<const N2: usize>(
    coef: &[i16],
    dqbuf: &mut [i16],
    s: &Scan,
    q: Quant,
    deadzone: i32,
    levels: &mut [i16; 1024],
) -> (usize, usize) {
    let n = 1usize << s.log2n;
    let k32 = (s.log2n == 5) as u32;
    let lmax_ac = MAX_LEVEL.min((32767 << k32) / q.ac) as u32;
    let lmax_dc = MAX_LEVEL.min((32767 << k32) / q.dc) as u32;
    let inv_ac = recip(q.ac);
    let bias_ac = ((q.ac * deadzone) >> 6) as u32;
    let coef = &coef[..N2];
    let dqbuf = &mut dqbuf[..N2];
    let iscan = &s.iscan_t[..N2];
    let mask = n as u32 - 1;
    let mut eob = 0u32;
    let mut rows = 0u32;
    let mut lev = [0i16; N2];
    for i in 0..N2 {
        let x = coef[i] as i32;
        let a = (x.unsigned_abs() << k32) + bias_ac;
        let l = ((a * inv_ac.0) >> inv_ac.1).min(lmax_ac);
        let d = ((l * q.ac as u32) >> k32) as i32;
        let sgn = x >> 31;
        dqbuf[i] = ((d ^ sgn) - sgn) as i16;
        lev[i] = ((l as i32 ^ sgn) - sgn) as i16;
        let nz = if l != 0 { u32::MAX } else { 0 };
        eob = eob.max((iscan[i] as u32 + 1) & nz);
        rows = rows.max(i as u32 & mask & nz);
    }
    let mut eob = eob as usize;
    let rows = rows as usize;
    {
        let x = coef[0] as i32;
        let a = (x.unsigned_abs() << k32) + ((q.dc * deadzone) >> 6) as u32;
        let r = recip(q.dc);
        let l = ((a * r.0) >> r.1).min(lmax_dc);
        let d = ((l * q.dc as u32) >> k32) as i32;
        dqbuf[0] = (if x < 0 { -d } else { d }) as i16;
        lev[0] = (if x < 0 { -(l as i32) } else { l as i32 }) as i16;
        if l != 0 {
            eob = eob.max(1);
        }
    }
    for (c, l) in levels[..eob].iter_mut().enumerate() {
        *l = lev[s.tpos(s.scan[c] as usize)];
    }
    (eob, if eob == 0 { 0 } else { rows + 1 })
}

#[allow(clippy::too_many_arguments)]
pub fn trellis<const N2: usize>(
    coef: &[i16],
    s: &Scan,
    q: Quant,
    ctx0: usize,
    costs: &TokenCosts,
    lambda: u32,
    levels: &mut [i16; 1024],
) -> (usize, u32) {
    let c = &*COSTS;
    let n2 = N2;
    let k32 = (s.log2n == 5) as u32;
    let w: u64 = if k32 == 1 { 16 } else { 4 };
    let lam = lambda as u64;
    let lmax_ac = MAX_LEVEL.min((32767 << k32) / q.ac) as u32;
    let lmax_dc = MAX_LEVEL.min((32767 << k32) / q.dc) as u32;
    let inv_ac = recip(q.ac);
    let inv_dc = recip(q.dc);
    let mut eob0 = 0;
    let mut abs = [0u16; N2];
    let mut l0 = [0u16; N2];
    for (k, &pos) in s.scan.iter().enumerate() {
        let i = s.tpos(pos as usize);
        let a = coef[i].unsigned_abs() as u32;
        let (dq, inv, lmax) = if i == 0 {
            (q.dc, inv_dc, lmax_dc)
        } else {
            (q.ac, inv_ac, lmax_ac)
        };
        let l = ((((a << k32) + (dq as u32 >> 1)) * inv.0) >> inv.1).min(lmax);
        abs[k] = a as u16;
        l0[k] = l as u16;
        if l != 0 {
            eob0 = k + 1;
        }
    }
    let mut tail = [0u64; N2];
    let mut acc = 0u64;
    for k in (0..eob0).rev() {
        let a = abs[k] as u64;
        acc += a * a * w;
        tail[k] = acc;
    }
    let mut tc = [0u8; N2];
    let band0 = s.bands[0] as usize;
    let mut best_j = tail[0] + lam * costs.more[band0][ctx0][0] as u64;
    let mut best_e = 0usize;
    let mut best_r = costs.more[band0][ctx0][0];
    let mut cum_d = 0u64;
    let mut cum_r = 0u32;
    let mut check = true;
    for k in 0..eob0 {
        let band = s.bands[k] as usize;
        let ctx = ctx_at(&tc, s, k, ctx0);
        if check {
            cum_r += costs.more[band][ctx][1];
        }
        let dq = if k == 0 { q.dc } else { q.ac } as i64;
        let a = abs[k] as i64;
        let tok = &costs.token[band][ctx];
        let next_more = if k + 1 < n2 {
            costs.more[s.bands[k + 1] as usize][ctx][1]
        } else {
            0
        };
        let lv = l0[k] as u32;
        let mut pick = lv;
        let mut pick_r = level_cost(c, tok, lv);
        let err = a - ((lv as i64 * dq) >> k32);
        let mut pick_d = (err * err) as u64 * w;
        if lv > 0 {
            let base_j = pick_d + lam * (pick_r + next_more) as u64;
            let l1 = lv - 1;
            let r1 = level_cost(c, tok, l1);
            let e1 = a - ((l1 as i64 * dq) >> k32);
            let d1 = (e1 * e1) as u64 * w;
            let j1 = d1 + lam * (r1 + if l1 > 0 { next_more } else { 0 }) as u64;
            if j1 < base_j {
                pick = l1;
                pick_r = r1;
                pick_d = d1;
            }
        }
        cum_r += pick_r;
        cum_d += pick_d;
        l0[k] = pick as u16;
        tc[s.scan[k] as usize] = ENERGY_CLASS[token(pick)];
        check = pick != 0;
        if pick != 0 {
            let r_end = if k + 1 < n2 {
                costs.more[s.bands[k + 1] as usize][ctx_at(&tc, s, k + 1, ctx0)][0]
            } else {
                0
            };
            let j = cum_d + tail.get(k + 1).copied().unwrap_or(0) + lam * (cum_r + r_end) as u64;
            if j < best_j {
                best_j = j;
                best_e = k + 1;
                best_r = cum_r + r_end;
            }
        }
    }
    for k in 0..best_e {
        let i = s.tpos(s.scan[k] as usize);
        let l = l0[k] as i16;
        levels[k] = if coef[i] < 0 { -l } else { l };
    }
    (best_e, best_r)
}

pub fn dequantise(levels: &[i16], eob: usize, s: &Scan, q: Quant, dqbuf: &mut [i16]) -> usize {
    let n = 1usize << s.log2n;
    let k32 = (s.log2n == 5) as u32;
    dqbuf[..n * n].fill(0);
    let mut rows = 0;
    for (k, &lv) in levels[..eob].iter().enumerate() {
        let i = s.tpos(s.scan[k] as usize);
        let dq = if i == 0 { q.dc } else { q.ac };
        let d = ((lv.unsigned_abs() as u32 * dq as u32) >> k32) as i32;
        dqbuf[i] = (if lv < 0 { -d } else { d }) as i16;
        if lv != 0 {
            rows = rows.max(i & (n - 1));
        }
    }
    rows + 1
}
