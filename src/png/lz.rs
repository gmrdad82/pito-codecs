const WINDOW: usize = 32768;
const MASK: usize = WINDOW - 1;
const HASH_BITS: u32 = 15;
const NONE: u32 = u32::MAX;
const MIN_MATCH: usize = 4;
const MAX_MATCH: usize = 258;
const BLOCK_TOKENS: usize = 1 << 15;
const MATCH_FLAG: u32 = 1 << 31;

#[derive(Clone, Copy)]
pub struct Params {
    pub depth: u32,
    pub nice: usize,
    pub lazy: usize,
}

pub const BALANCED: Params = Params {
    depth: 192,
    nice: 258,
    lazy: 258,
};

pub const HIGH: Params = Params {
    depth: 512,
    nice: 258,
    lazy: 258,
};

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const PRECODE_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

const LEN_SYM: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut s = 0;
    while s < 28 {
        let mut l = LEN_BASE[s] as usize;
        while l < LEN_BASE[s] as usize + (1 << LEN_EXTRA[s]) && l <= 257 {
            t[l - 3] = s as u8;
            l += 1;
        }
        s += 1;
    }
    t[255] = 28;
    t
};

const FIXED_LIT: [u8; 288] = {
    let mut t = [8u8; 288];
    let mut i = 144;
    while i < 256 {
        t[i] = 9;
        i += 1;
    }
    while i < 280 {
        t[i] = 7;
        i += 1;
    }
    t
};

#[inline(always)]
fn dist_symbol(d: usize) -> (usize, u32, u32) {
    let d = d - 1;
    if d < 4 {
        (d, 0, 0)
    } else {
        let nb = 31 - (d as u32).leading_zeros();
        let sym = 2 * nb + ((d as u32 >> (nb - 1)) & 1);
        (sym as usize, nb - 1, d as u32 & ((1 << (nb - 1)) - 1))
    }
}

struct Bits {
    acc: u64,
    n: u32,
}

impl Bits {
    #[inline(always)]
    fn put(&mut self, out: &mut Vec<u8>, v: u32, bits: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += bits;
        if self.n >= 32 {
            out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    fn align(&mut self, out: &mut Vec<u8>) {
        if !self.n.is_multiple_of(8) {
            let pad = 8 - self.n % 8;
            self.put(out, 0, pad);
        }
        while self.n >= 8 {
            out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
}

fn lengths(freq: &[u32], max: u8, lens: &mut [u8]) {
    lens.fill(0);
    let mut leaves = [(0u32, 0u16); 288];
    let mut n = 0;
    for (s, &f) in freq.iter().enumerate() {
        if f > 0 {
            leaves[n] = (f, s as u16);
            n += 1;
        }
    }
    if n == 0 {
        return;
    }
    if n == 1 {
        lens[usize::from(leaves[0].1)] = 1;
        return;
    }
    let leaves = &mut leaves[..n];
    leaves.sort_unstable();
    let mut weight = [0u64; 576];
    let mut parent = [0u16; 576];
    for (i, l) in leaves.iter().enumerate() {
        weight[i] = u64::from(l.0);
    }
    let (mut li, mut ni, mut made) = (0usize, n, n);
    for _ in 0..n - 1 {
        let pick = |li: &mut usize, ni: &mut usize, made: usize| -> usize {
            let take_leaf = *li < n && (*ni >= made || weight[*li] <= weight[*ni]);
            if take_leaf {
                *li += 1;
                *li - 1
            } else {
                *ni += 1;
                *ni - 1
            }
        };
        let a = pick(&mut li, &mut ni, made);
        let b = pick(&mut li, &mut ni, made);
        weight[made] = weight[a] + weight[b];
        parent[a] = made as u16;
        parent[b] = made as u16;
        made += 1;
    }
    let mut depth = [0u8; 576];
    let mut count = [0u32; 64];
    for node in (0..made - 1).rev() {
        depth[node] = depth[usize::from(parent[node])] + 1;
    }
    for &d in &depth[..n] {
        count[usize::from(d).min(usize::from(max))] += 1;
    }
    let max = usize::from(max);
    let mut kraft: u64 = (1..=max).map(|l| u64::from(count[l]) << (max - l)).sum();
    while kraft > 1 << max {
        let mut l = max - 1;
        while l > 1 && count[l] == 0 {
            l -= 1;
        }
        count[l] -= 1;
        count[l + 1] += 2;
        count[max] -= 1;
        kraft -= 1;
    }
    let mut at = 0;
    for l in (1..=max).rev() {
        for _ in 0..count[l] {
            lens[usize::from(leaves[at].1)] = l as u8;
            at += 1;
        }
    }
}

fn codes(lens: &[u8], out: &mut [u16]) {
    let mut count = [0u16; 16];
    for &l in lens {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for l in 1..16 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    for (s, &l) in lens.iter().enumerate() {
        if l > 0 {
            let c = next[usize::from(l)];
            next[usize::from(l)] += 1;
            out[s] = c.reverse_bits() >> (16 - l);
        } else {
            out[s] = 0;
        }
    }
}

struct Header {
    hlit: usize,
    hdist: usize,
    hclen: usize,
    rle: Vec<(u8, u8)>,
    pre_lens: [u8; 19],
    pre_codes: [u16; 19],
    bits: u64,
}

impl Header {
    fn new() -> Self {
        Header {
            hlit: 0,
            hdist: 0,
            hclen: 0,
            rle: Vec::with_capacity(320),
            pre_lens: [0; 19],
            pre_codes: [0; 19],
            bits: 0,
        }
    }

    fn plan(&mut self, lit: &[u8; 286], dist: &[u8; 30]) {
        self.hlit = lit
            .iter()
            .rposition(|&l| l > 0)
            .map_or(257, |p| (p + 1).max(257));
        self.hdist = dist.iter().rposition(|&l| l > 0).map_or(1, |p| p + 1);
        let mut seq = [0u8; 316];
        seq[..self.hlit].copy_from_slice(&lit[..self.hlit]);
        seq[self.hlit..self.hlit + self.hdist].copy_from_slice(&dist[..self.hdist]);
        let seq = &seq[..self.hlit + self.hdist];
        self.rle.clear();
        let mut i = 0;
        while i < seq.len() {
            let v = seq[i];
            let mut run = 1;
            while i + run < seq.len() && seq[i + run] == v {
                run += 1;
            }
            i += run;
            if v == 0 {
                while run >= 11 {
                    let take = run.min(138);
                    self.rle.push((18, (take - 11) as u8));
                    run -= take;
                }
                if run >= 3 {
                    self.rle.push((17, (run - 3) as u8));
                } else {
                    for _ in 0..run {
                        self.rle.push((0, 0));
                    }
                }
            } else {
                self.rle.push((v, 0));
                run -= 1;
                while run >= 3 {
                    let take = run.min(6);
                    self.rle.push((16, (take - 3) as u8));
                    run -= take;
                }
                for _ in 0..run {
                    self.rle.push((v, 0));
                }
            }
        }
        let mut freq = [0u32; 19];
        for &(s, _) in &self.rle {
            freq[usize::from(s)] += 1;
        }
        lengths(&freq, 7, &mut self.pre_lens);
        codes(&self.pre_lens, &mut self.pre_codes);
        self.hclen = 19;
        while self.hclen > 4 && self.pre_lens[PRECODE_ORDER[self.hclen - 1]] == 0 {
            self.hclen -= 1;
        }
        let mut bits = 14 + 3 * self.hclen as u64;
        for &(s, _) in &self.rle {
            bits += u64::from(self.pre_lens[usize::from(s)])
                + match s {
                    16 => 2,
                    17 => 3,
                    18 => 7,
                    _ => 0,
                };
        }
        self.bits = bits;
    }

    fn write(&self, bits: &mut Bits, out: &mut Vec<u8>) {
        bits.put(out, (self.hlit - 257) as u32, 5);
        bits.put(out, (self.hdist - 1) as u32, 5);
        bits.put(out, (self.hclen - 4) as u32, 4);
        for &o in &PRECODE_ORDER[..self.hclen] {
            bits.put(out, u32::from(self.pre_lens[o]), 3);
        }
        for &(s, extra) in &self.rle {
            bits.put(
                out,
                u32::from(self.pre_codes[usize::from(s)]),
                u32::from(self.pre_lens[usize::from(s)]),
            );
            match s {
                16 => bits.put(out, u32::from(extra), 2),
                17 => bits.put(out, u32::from(extra), 3),
                18 => bits.put(out, u32::from(extra), 7),
                _ => {}
            }
        }
    }
}

pub struct Lz {
    head: Vec<u32>,
    prev: Vec<u32>,
    tokens: Vec<u32>,
    lit_freq: [u32; 286],
    dist_freq: [u32; 30],
    header: Header,
    fixed_lit_codes: [u16; 288],
    fixed_dist_codes: [u16; 30],
}

impl Lz {
    pub fn new() -> Self {
        let mut fixed_lit_codes = [0u16; 288];
        codes(&FIXED_LIT, &mut fixed_lit_codes);
        let mut fixed_dist_codes = [0u16; 30];
        for (s, c) in fixed_dist_codes.iter_mut().enumerate() {
            *c = (s as u16).reverse_bits() >> 11;
        }
        Lz {
            head: vec![NONE; 1 << HASH_BITS],
            prev: vec![NONE; WINDOW],
            tokens: Vec::with_capacity(BLOCK_TOKENS + 1),
            lit_freq: [0; 286],
            dist_freq: [0; 30],
            header: Header::new(),
            fixed_lit_codes,
            fixed_dist_codes,
        }
    }

    #[inline(always)]
    fn hash(buf: &[u8], pos: usize) -> usize {
        let v = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    #[inline(always)]
    fn insert(&mut self, buf: &[u8], pos: usize) {
        let h = Self::hash(buf, pos);
        self.prev[pos & MASK] = self.head[h];
        self.head[h] = pos as u32;
    }

    #[inline(always)]
    fn common(buf: &[u8], a: usize, b: usize, max: usize) -> usize {
        let mut n = 0;
        while n + 8 <= max {
            let x = u64::from_le_bytes([
                buf[a + n],
                buf[a + n + 1],
                buf[a + n + 2],
                buf[a + n + 3],
                buf[a + n + 4],
                buf[a + n + 5],
                buf[a + n + 6],
                buf[a + n + 7],
            ]);
            let y = u64::from_le_bytes([
                buf[b + n],
                buf[b + n + 1],
                buf[b + n + 2],
                buf[b + n + 3],
                buf[b + n + 4],
                buf[b + n + 5],
                buf[b + n + 6],
                buf[b + n + 7],
            ]);
            let d = x ^ y;
            if d != 0 {
                return n + (d.trailing_zeros() / 8) as usize;
            }
            n += 8;
        }
        while n < max && buf[a + n] == buf[b + n] {
            n += 1;
        }
        n
    }

    #[inline(always)]
    fn find(&mut self, buf: &[u8], pos: usize, end: usize, p: &Params) -> (usize, usize) {
        let max = (end - pos).min(MAX_MATCH);
        let h = Self::hash(buf, pos);
        let mut cand = self.head[h];
        self.prev[pos & MASK] = cand;
        self.head[h] = pos as u32;
        let (mut best, mut dist) = (MIN_MATCH - 1, 0);
        let mut chain = p.depth;
        let first = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        while cand != NONE && chain > 0 {
            let c = cand as usize;
            if pos - c > WINDOW {
                break;
            }
            if buf[c + best] == buf[pos + best]
                && u32::from_le_bytes([buf[c], buf[c + 1], buf[c + 2], buf[c + 3]]) == first
            {
                let l = Self::common(buf, c, pos, max);
                if l > best {
                    best = l;
                    dist = pos - c;
                    if l >= p.nice || l >= max {
                        break;
                    }
                }
            }
            let next = self.prev[c & MASK];
            if next >= cand {
                break;
            }
            cand = next;
            chain -= 1;
        }
        if best >= MIN_MATCH {
            (best, dist)
        } else {
            (0, 0)
        }
    }

    #[inline(always)]
    fn literal(&mut self, b: u8) {
        self.tokens.push(u32::from(b));
    }

    #[inline(always)]
    fn matched(&mut self, len: usize, dist: usize) {
        self.tokens
            .push(MATCH_FLAG | ((len - 3) as u32) << 16 | (dist - 1) as u32);
    }

    fn skip_insert(&mut self, buf: &[u8], from: usize, to: usize, end: usize) {
        let to = to.min(end.saturating_sub(3));
        let from = if to > from + 4096 { to - 16 } else { from };
        for q in from..to {
            self.insert(buf, q);
        }
    }

    pub fn parse(&mut self, buf: &[u8], start: usize, end: usize, p: &Params) {
        self.head.fill(NONE);
        self.prev.fill(NONE);
        self.tokens.clear();
        let dict_from = start.saturating_sub(WINDOW);
        let hashable = end.saturating_sub(3);
        for q in dict_from..start.min(hashable) {
            self.insert(buf, q);
        }
        let mut pos = start;
        let mut pending: Option<(usize, usize)> = None;
        while pos < end {
            if pos >= hashable {
                self.literal(buf[pos]);
                pos += 1;
                continue;
            }
            let (len, dist) = match pending.take() {
                Some(m) => m,
                None => self.find(buf, pos, end, p),
            };
            if len == 0 {
                self.literal(buf[pos]);
                pos += 1;
                continue;
            }
            if len < p.lazy && pos + 1 < hashable {
                let (len2, dist2) = self.find(buf, pos + 1, end, p);
                if len2 > len {
                    self.literal(buf[pos]);
                    pos += 1;
                    pending = Some((len2, dist2));
                    continue;
                }
                self.matched(len, dist);
                self.skip_insert(buf, pos + 2, pos + len, end);
            } else {
                self.matched(len, dist);
                self.skip_insert(buf, pos + 1, pos + len, end);
            }
            pos += len;
        }
    }

    pub fn tokens(&self) -> &[u32] {
        &self.tokens
    }

    pub fn segment(
        &mut self,
        buf: &[u8],
        start: usize,
        end: usize,
        p: &Params,
        single: bool,
        out: &mut Vec<u8>,
    ) {
        self.parse(buf, start, end, p);
        let tokens = std::mem::take(&mut self.tokens);
        let mut bits = Bits { acc: 0, n: 0 };
        self.blocks(&tokens, Some((buf, start)), single, &mut bits, out);
        self.tokens = tokens;
        if single {
            bits.align(out);
        } else {
            bits.put(out, 0, 3);
            bits.align(out);
            out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        }
    }

    pub fn merged(&mut self, tokens: &[u32], out: &mut Vec<u8>) {
        let mut bits = Bits { acc: 0, n: 0 };
        self.blocks(tokens, None, true, &mut bits, out);
        bits.align(out);
    }

    fn blocks(
        &mut self,
        tokens: &[u32],
        raw: Option<(&[u8], usize)>,
        last: bool,
        bits: &mut Bits,
        out: &mut Vec<u8>,
    ) {
        let mut raw_at = raw.map_or(0, |r| r.1);
        let mut i = 0;
        while i < tokens.len() {
            let end = (i + BLOCK_TOKENS).min(tokens.len());
            let chunk = &tokens[i..end];
            self.lit_freq = [0; 286];
            self.dist_freq = [0; 30];
            let mut covered = 0;
            for &t in chunk {
                if t & MATCH_FLAG == 0 {
                    self.lit_freq[t as usize] += 1;
                    covered += 1;
                } else {
                    let li = ((t >> 16) & 0xff) as usize;
                    self.lit_freq[257 + usize::from(LEN_SYM[li])] += 1;
                    self.dist_freq[dist_symbol((t & 0x7fff) as usize + 1).0] += 1;
                    covered += li + 3;
                }
            }
            let from = raw_at;
            raw_at += covered;
            let bytes = raw.map(|(b, _)| &b[from..raw_at]);
            self.block(chunk, bytes, last && end == tokens.len(), bits, out);
            i = end;
        }
    }

    fn block(
        &mut self,
        tokens: &[u32],
        raw: Option<&[u8]>,
        last: bool,
        bits: &mut Bits,
        out: &mut Vec<u8>,
    ) {
        self.lit_freq[256] = 1;
        let mut lit_lens = [0u8; 286];
        let mut dist_lens = [0u8; 30];
        lengths(&self.lit_freq, 15, &mut lit_lens);
        lengths(&self.dist_freq, 15, &mut dist_lens);
        if dist_lens.iter().all(|&l| l == 0) {
            dist_lens[0] = 1;
        }
        let mut extra = 0u64;
        for (f, e) in self.lit_freq[257..].iter().zip(LEN_EXTRA) {
            extra += u64::from(*f) * u64::from(e);
        }
        for (f, e) in self.dist_freq.iter().zip(DIST_EXTRA) {
            extra += u64::from(*f) * u64::from(e);
        }
        let mut dynamic = 3 + extra;
        let mut fixed = 3 + extra;
        for ((f, d), x) in self.lit_freq.iter().zip(lit_lens).zip(FIXED_LIT) {
            dynamic += u64::from(*f) * u64::from(d);
            fixed += u64::from(*f) * u64::from(x);
        }
        for (f, d) in self.dist_freq.iter().zip(dist_lens) {
            dynamic += u64::from(*f) * u64::from(d);
            fixed += u64::from(*f) * 5;
        }
        self.header.plan(&lit_lens, &dist_lens);
        dynamic += self.header.bits;
        let stored_bits = raw.map_or(u64::MAX, |r| {
            8 * (r.len() as u64 + 5 * r.len().div_ceil(65535) as u64) + 7
        });
        if let Some(r) = raw.filter(|_| stored_bits <= dynamic.min(fixed)) {
            let mut at = 0;
            while at < r.len() {
                let n = (r.len() - at).min(65535);
                let fin = last && at + n == r.len();
                bits.put(out, u32::from(fin), 3);
                bits.align(out);
                out.extend_from_slice(&(n as u16).to_le_bytes());
                out.extend_from_slice(&(!(n as u16)).to_le_bytes());
                out.extend_from_slice(&r[at..at + n]);
                at += n;
            }
        } else if fixed <= dynamic {
            bits.put(out, u32::from(last) | 2, 3);
            let (lit_codes, dist_codes) = (self.fixed_lit_codes, self.fixed_dist_codes);
            Self::write(
                tokens,
                &lit_codes[..286],
                &FIXED_LIT[..286],
                &dist_codes,
                &[5; 30],
                bits,
                out,
            );
        } else {
            bits.put(out, u32::from(last) | 4, 3);
            self.header.write(bits, out);
            let mut lit_codes = [0u16; 286];
            let mut dist_codes = [0u16; 30];
            codes(&lit_lens, &mut lit_codes);
            codes(&dist_lens, &mut dist_codes);
            Self::write(
                tokens,
                &lit_codes,
                &lit_lens,
                &dist_codes,
                &dist_lens,
                bits,
                out,
            );
        }
    }

    fn write(
        tokens: &[u32],
        lit_codes: &[u16],
        lit_lens: &[u8],
        dist_codes: &[u16],
        dist_lens: &[u8],
        bits: &mut Bits,
        out: &mut Vec<u8>,
    ) {
        out.reserve(tokens.len() * 3 + 64);
        for &t in tokens {
            if t & MATCH_FLAG == 0 {
                let s = t as usize;
                bits.put(out, u32::from(lit_codes[s]), u32::from(lit_lens[s]));
            } else {
                let li = ((t >> 16) & 0xff) as usize;
                let ls = usize::from(LEN_SYM[li]);
                let sym = 257 + ls;
                bits.put(out, u32::from(lit_codes[sym]), u32::from(lit_lens[sym]));
                let eb = u32::from(LEN_EXTRA[ls]);
                if eb > 0 {
                    bits.put(
                        out,
                        (li as u32 + 3 - u32::from(LEN_BASE[ls])) & ((1 << eb) - 1),
                        eb,
                    );
                }
                let (ds, deb, dextra) = dist_symbol((t & 0x7fff) as usize + 1);
                bits.put(out, u32::from(dist_codes[ds]), u32::from(dist_lens[ds]));
                if deb > 0 {
                    bits.put(out, dextra, deb);
                }
            }
        }
        bits.put(out, u32::from(lit_codes[256]), u32::from(lit_lens[256]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_symbols_cover_every_length_with_the_right_extra_bits() {
        for len in 3..=258usize {
            let s = usize::from(LEN_SYM[len - 3]);
            let base = usize::from(LEN_BASE[s]);
            assert!(len >= base && len - base < 1 << LEN_EXTRA[s], "{len}");
        }
        assert_eq!(LEN_SYM[255], 28);
    }

    #[test]
    fn distance_symbols_cover_every_distance() {
        let base =
            |s: usize| -> usize { 1 + DIST_EXTRA[..s].iter().map(|&e| 1usize << e).sum::<usize>() };
        for d in 1..=WINDOW {
            let (s, eb, extra) = dist_symbol(d);
            assert_eq!(eb, u32::from(DIST_EXTRA[s]), "{d}");
            assert_eq!(base(s) + extra as usize, d, "{d}");
        }
    }

    #[test]
    fn code_lengths_are_complete_within_the_limit() {
        let mut state = 12345u32;
        for n in [2usize, 3, 7, 40, 286] {
            for skew in [1u32, 4, 12] {
                let freq: Vec<u32> = (0..n)
                    .map(|_| {
                        state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        1 + ((state >> 8) % 100).pow(skew.min(4)) % 100_000
                    })
                    .collect();
                let mut lens = vec![0u8; n];
                lengths(&freq, 15, &mut lens);
                let kraft: u64 = lens.iter().map(|&l| 1u64 << (15 - l)).sum();
                assert_eq!(kraft, 1 << 15, "{n} {skew}");
                assert!(lens.iter().all(|&l| (1..=15).contains(&l)));
            }
        }
        let mut freq = vec![0u32; 20];
        for (i, f) in freq.iter_mut().enumerate() {
            *f = 1 << i.min(30);
        }
        let mut lens = vec![0u8; 20];
        lengths(&freq, 7, &mut lens);
        assert_eq!(lens.iter().map(|&l| 1u64 << (7 - l)).sum::<u64>(), 1 << 7);
        assert!(lens.iter().all(|&l| l <= 7));
    }

    #[test]
    fn a_single_used_symbol_gets_a_one_bit_code() {
        let mut freq = [0u32; 30];
        freq[5] = 9;
        let mut lens = [0u8; 30];
        lengths(&freq, 15, &mut lens);
        assert_eq!(lens[5], 1);
        assert_eq!(lens.iter().filter(|&&l| l > 0).count(), 1);
    }
}
