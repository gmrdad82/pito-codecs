use crate::aac::bits::BitWriter;

use crate::aac::huffman::{self, ZERO_HCB};
use crate::aac::ics::IcsInfo;
use crate::aac::tables::pow43;
use crate::aac::tns::TnsData;

const ROUND: f32 = 0.4054;
const MAX_Q: i32 = 8191;

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub start: usize,
    pub end: usize,
    pub thr: f32,
    pub en: f32,
    pub max34: f32,
}

pub struct ChannelInput {
    pub info: IcsInfo,
    pub x: Vec<f32>,
    pub xabs: Vec<f32>,
    pub x34: Vec<f32>,
    pub bands: Vec<Band>,
    pub tns: Option<TnsData>,
}

impl ChannelInput {
    pub fn new(
        info: IcsInfo,
        swb: &[u16],
        spec: &[f32],
        thr: &[Vec<f32>],
        tns: Option<TnsData>,
    ) -> ChannelInput {
        let mut x = Vec::with_capacity(1024);
        let mut bands = Vec::new();
        for g in 0..info.num_groups {
            let w0 = info.group_start(g);
            for sfb in 0..info.max_sfb {
                let start = x.len();
                for w in w0..w0 + info.group_len[g] as usize {
                    x.extend_from_slice(
                        &spec[w * 128 + swb[sfb] as usize..w * 128 + swb[sfb + 1] as usize],
                    );
                }
                bands.push(Band {
                    start,
                    end: x.len(),
                    thr: thr[g][sfb],
                    en: 0.0,
                    max34: 0.0,
                });
            }
        }
        let xabs: Vec<f32> = x.iter().map(|v| v.abs()).collect();
        let x34: Vec<f32> = xabs.iter().map(|v| (v * v.sqrt()).sqrt()).collect();
        for b in &mut bands {
            b.en = x[b.start..b.end].iter().map(|v| v * v).sum();
            b.max34 = x34[b.start..b.end].iter().cloned().fold(0.0, f32::max);
        }
        ChannelInput {
            info,
            x,
            xabs,
            x34,
            bands,
            tns,
        }
    }
}

const INF: u32 = u32::MAX / 4;

fn sf_range(b: &Band) -> (i32, i32) {
    let l2 = b.max34.log2();
    let lo = ((100.0 - ((MAX_Q as f32 + 0.5).log2() - l2) / 0.1875).ceil() as i32).clamp(0, 255);
    let zero = ((100.0 - ((1.0 - ROUND).log2() - l2) / 0.1875).floor() as i32 + 1).clamp(0, 256);
    (lo, (zero - 1).clamp(lo, 255))
}

#[derive(Default)]
pub struct Memo {
    start: Vec<usize>,
    lo: Vec<i32>,
    len: Vec<usize>,
    dist: Vec<f32>,
    cost: Vec<[u32; 12]>,
    zero: Vec<[u32; 12]>,
    scratch: Vec<i32>,
}

impl Memo {
    pub fn reset(&mut self, ch: &ChannelInput) {
        self.start.clear();
        self.lo.clear();
        self.len.clear();
        self.dist.clear();
        self.cost.clear();
        self.zero.clear();
        self.zero.resize(ch.bands.len(), [INF + 1; 12]);
        for b in &ch.bands {
            let (lo, hi) = if b.max34 > 0.0 { sf_range(b) } else { (0, -1) };
            let n = (hi - lo + 1).max(0) as usize;
            self.start.push(self.dist.len());
            self.lo.push(lo);
            self.len.push(n);
            self.dist.resize(self.dist.len() + n, f32::NAN);
            self.cost.resize(self.cost.len() + n, [INF + 1; 12]);
        }
    }

    fn slot(&self, band: usize, sf: i32) -> Option<usize> {
        let k = sf - *self.lo.get(band)?;
        (k >= 0 && (k as usize) < self.len[band]).then(|| self.start[band] + k as usize)
    }

    fn dist(&mut self, ch: &ChannelInput, band: usize, sf: i32) -> f32 {
        let b = &ch.bands[band];
        let r = b.start..b.end;
        let slot = self.slot(band, sf);
        if let Some(d) = slot.map(|k| self.dist[k]).filter(|d| !d.is_nan()) {
            return d;
        }
        let d = quantise(&ch.x[r.clone()], &ch.xabs[r.clone()], &ch.x34[r], sf, None);
        if let Some(k) = slot {
            self.dist[k] = d;
        }
        d
    }

    fn cost(&mut self, ch: &ChannelInput, band: usize, sf: i32, maxq: i32) -> [u32; 12] {
        let slot = self.slot(band, sf);
        let cached = if maxq == 0 {
            self.zero[band]
        } else {
            slot.map_or([INF + 1; 12], |k| self.cost[k])
        };
        if cached[0] <= INF {
            return cached;
        }
        let b = &ch.bands[band];
        let r = b.start..b.end;
        let mut out = [INF; 12];
        if maxq > 0 {
            self.scratch.resize(r.len(), 0);
            quantise(
                &ch.x[r.clone()],
                &ch.xabs[r.clone()],
                &ch.x34[r],
                sf,
                Some(&mut self.scratch),
            );
        } else {
            self.scratch.clear();
            self.scratch.resize(r.len(), 0);
        }
        for &c in candidates(maxq) {
            out[c as usize] = if c == 0 {
                0
            } else {
                huffman::count_bits(c, &self.scratch) + if maxq == 0 { 1 } else { 0 }
            };
        }
        if maxq == 0 {
            self.zero[band] = out;
        } else if let Some(k) = slot {
            self.cost[k] = out;
        }
        out
    }
}

fn max_q(b: &Band, sf: i32) -> i32 {
    ((b.max34 * gain34(sf) + ROUND) as i32).min(MAX_Q)
}

#[derive(Clone, Default)]
pub struct QuantChannel {
    pub q: Vec<i32>,
    pub sf: Vec<i32>,
    pub cb: Vec<u8>,
    pub sections: Vec<Vec<(u8, usize)>>,
    pub global_gain: i32,
    pub bits: u32,
    maxq: Vec<i32>,
}

#[inline]
fn gain34(sf: i32) -> f32 {
    (-0.1875 * (sf as f32 - 100.0)).exp2()
}
#[inline]
fn gain(sf: i32) -> f32 {
    (0.25 * (sf as f32 - 100.0)).exp2()
}

fn quantise(x: &[f32], xabs: &[f32], x34: &[f32], sf: i32, out: Option<&mut [i32]>) -> f32 {
    let g34 = gain34(sf);
    let g = gain(sf);
    let mut d = 0f32;
    match out {
        Some(out) => {
            for i in 0..x.len() {
                let q = ((x34[i] * g34 + ROUND) as i32).min(MAX_Q);
                let e = xabs[i] - pow43(q as u32) * g;
                d += e * e;
                out[i] = if x[i] < 0.0 { -q } else { q };
            }
        }
        None => {
            for i in 0..x.len() {
                let q = ((x34[i] * g34 + ROUND) as i32).min(MAX_Q);
                let e = xabs[i] - pow43(q as u32) * g;
                d += e * e;
            }
        }
    }
    d
}

fn search_sf(ch: &ChannelInput, band: usize, allowed: f32, memo: &mut Memo) -> i32 {
    let (lo, hi) = sf_range(&ch.bands[band]);
    let mut a = lo;
    let mut hi = hi;
    if memo.dist(ch, band, a) > allowed {
        return a;
    }
    while a < hi {
        let mid = (a + hi + 1) / 2;
        if memo.dist(ch, band, mid) <= allowed {
            a = mid;
        } else {
            hi = mid - 1;
        }
    }
    a
}

fn candidates(m: i32) -> &'static [u8] {
    match m {
        0 => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        1 => &[1, 2, 3, 4, 5, 6],
        2 => &[3, 4, 5, 6, 7, 8],
        3..=4 => &[5, 6, 7, 8, 9, 10],
        5..=7 => &[7, 8, 9, 10, 11],
        8..=12 => &[9, 10, 11],
        _ => &[11],
    }
}

pub fn quantise_channel(ch: &ChannelInput, lambda: f32, memo: &mut Memo) -> QuantChannel {
    let nb = ch.bands.len();
    let mut sf = vec![-1i32; nb];
    for (i, b) in ch.bands.iter().enumerate() {
        let allowed = lambda * b.thr;
        if b.max34 <= 0.0 || b.en <= allowed {
            continue;
        }
        sf[i] = search_sf(ch, i, allowed, memo);
    }
    let mut maxq = vec![0i32; nb];
    loop {
        for _ in 0..8 {
            let mut changed = false;
            let coded: Vec<usize> = (0..nb).filter(|&i| sf[i] >= 0).collect();
            for w in coded.windows(2) {
                if sf[w[1]] > sf[w[0]] + 60 {
                    sf[w[1]] = sf[w[0]] + 60;
                    changed = true;
                }
            }
            for w in coded.windows(2).rev() {
                if sf[w[0]] > sf[w[1]] + 60 {
                    sf[w[0]] = sf[w[1]] + 60;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut removed = false;
        for (i, b) in ch.bands.iter().enumerate() {
            if sf[i] < 0 {
                maxq[i] = 0;
                continue;
            }
            maxq[i] = max_q(b, sf[i]);
            if maxq[i] == 0 {
                sf[i] = -1;
                removed = true;
            }
        }
        if !removed {
            break;
        }
    }

    let info = &ch.info;
    let (sect_bits, esc) = if info.is_short() {
        (3u32, 7usize)
    } else {
        (5u32, 31usize)
    };
    let mut cb = vec![ZERO_HCB; nb];
    let mut sections = Vec::with_capacity(info.num_groups);
    let mut spectral_bits = 0u32;
    let mut section_bits = 0u32;
    let per_group = info.max_sfb;
    for g in 0..info.num_groups {
        let base = g * per_group;
        let n = per_group;
        if n == 0 {
            sections.push(Vec::new());
            continue;
        }
        let mut cost = vec![[INF; 12]; n];
        for (i, c) in cost.iter_mut().enumerate() {
            let bi = base + i;
            *c = memo.cost(ch, bi, sf[bi], maxq[bi]);
        }
        let new_sec = 4 + sect_bits;
        let mut best = [INF; 12];
        let mut back = vec![[0u8; 12]; n];
        for c in 0..12 {
            if cost[0][c] < INF {
                best[c] = new_sec + cost[0][c];
            }
        }
        for i in 1..n {
            let (mut min_c, mut min_v) = (0usize, INF);
            for c in 0..12 {
                if best[c] < min_v {
                    min_v = best[c];
                    min_c = c;
                }
            }
            let mut nb_ = [INF; 12];
            for c in 0..12 {
                if cost[i][c] >= INF {
                    continue;
                }
                let stay = best[c];
                let switch = min_v + new_sec;
                if stay <= switch {
                    nb_[c] = stay + cost[i][c];
                    back[i][c] = c as u8;
                } else {
                    nb_[c] = switch + cost[i][c];
                    back[i][c] = min_c as u8;
                }
            }
            best = nb_;
        }
        let mut c = (0..12).min_by_key(|&c| best[c]).unwrap_or(0);
        for i in (0..n).rev() {
            cb[base + i] = c as u8;
            c = back[i][c] as usize;
        }
        let mut secs: Vec<(u8, usize)> = Vec::new();
        for i in 0..n {
            let c = cb[base + i];
            match secs.last_mut() {
                Some((lc, len)) if *lc == c => *len += 1,
                _ => secs.push((c, 1)),
            }
            if c != ZERO_HCB {
                let bi = base + i;
                spectral_bits += cost[i][c as usize] - if maxq[bi] == 0 { 1 } else { 0 };
            }
        }
        for &(_, len) in &secs {
            section_bits += 4 + sect_bits * (len / esc + 1) as u32;
        }
        sections.push(secs);
    }

    let first = (0..nb).find(|&i| sf[i] >= 0).map(|i| sf[i]).unwrap_or(100);
    let mut prev = first;
    let mut sf_bits = 0u32;
    for i in 0..nb {
        if cb[i] == ZERO_HCB {
            continue;
        }
        if sf[i] < 0 {
            sf[i] = prev;
        }
        sf_bits += huffman::sf_bits(sf[i] - prev);
        prev = sf[i];
    }
    let tns_bits = 1 + ch.tns.as_ref().map(|t| t.bit_count(info)).unwrap_or(0);
    let bits = 8 + section_bits + sf_bits + 1 + tns_bits + 1 + spectral_bits;
    QuantChannel {
        q: Vec::new(),
        sf,
        cb,
        sections,
        global_gain: first,
        bits,
        maxq,
    }
}

pub fn fill(ch: &ChannelInput, qc: &mut QuantChannel) {
    qc.q.clear();
    qc.q.resize(ch.x.len(), 0);
    for (i, b) in ch.bands.iter().enumerate() {
        if qc.cb[i] == ZERO_HCB || qc.maxq[i] == 0 {
            continue;
        }
        let r = b.start..b.end;
        quantise(
            &ch.x[r.clone()],
            &ch.xabs[r.clone()],
            &ch.x34[r.clone()],
            qc.sf[i],
            Some(&mut qc.q[r]),
        );
    }
}

pub fn write_ics(bw: &mut BitWriter, ch: &ChannelInput, qc: &QuantChannel, write_info: bool) {
    let info = &ch.info;
    bw.write_bits(qc.global_gain as u32, 8);
    if write_info {
        info.write(bw);
    }
    let (sect_bits, esc) = if info.is_short() {
        (3u32, 7usize)
    } else {
        (5u32, 31usize)
    };
    for secs in &qc.sections {
        for &(c, len) in secs {
            bw.write_bits(c as u32, 4);
            let mut l = len;
            while l >= esc {
                bw.write_bits(esc as u32, sect_bits);
                l -= esc;
            }
            bw.write_bits(l as u32, sect_bits);
        }
    }
    let mut prev = qc.global_gain;
    for i in 0..ch.bands.len() {
        if qc.cb[i] == ZERO_HCB {
            continue;
        }
        huffman::write_sf(bw, qc.sf[i] - prev);
        prev = qc.sf[i];
    }
    bw.write_bits(0, 1);
    match &ch.tns {
        Some(t) => {
            bw.write_bits(1, 1);
            t.write(bw, info);
        }
        None => bw.write_bits(0, 1),
    }
    bw.write_bits(0, 1);
    for (i, b) in ch.bands.iter().enumerate() {
        if qc.cb[i] != ZERO_HCB {
            huffman::write_spectral(bw, qc.cb[i], &qc.q[b.start..b.end]);
        }
    }
}
