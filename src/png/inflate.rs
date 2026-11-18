use crate::media::{Error, Result};
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};

const LITERAL: u32 = 0x8000;
const EXCEPTIONAL: u32 = 0x4000;
const SECONDARY: u32 = 0x2000;
const INVALID: u32 = 0x1000;
const SLACK: usize = 6 + 258 + 48;
const WINDOW: usize = 32768;

const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const PRECODE_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

const LITLEN_ENTRIES: [u32; 288] = {
    let mut e = [EXCEPTIONAL | INVALID; 288];
    let mut i = 0;
    while i < 256 {
        e[i] = (i as u32) << 16 | LITERAL | (1 << 8);
        i += 1;
    }
    e[256] = EXCEPTIONAL;
    let mut i = 257;
    while i < 286 {
        e[i] = (LEN_BASE[i - 257] as u32) << 16 | (LEN_EXTRA[i - 257] as u32) << 8;
        i += 1;
    }
    e
};

const DIST_ENTRIES: [u32; 32] = {
    let mut e = [0; 32];
    let mut i = 0;
    while i < 30 {
        e[i] = (DIST_BASE[i] as u32) << 16 | (DIST_EXTRA[i] as u32) << 8 | LITERAL;
        i += 1;
    }
    e
};

fn corrupt(what: &str) -> Error {
    Error::Invalid(format!("PNG image data: {what}"))
}

fn next_codeword(mut codeword: u16, table_size: u16) -> u16 {
    if codeword == table_size - 1 {
        return codeword;
    }
    let adv = (u16::BITS - 1) - (codeword ^ (table_size - 1)).leading_zeros();
    let bit = 1 << adv;
    codeword &= bit - 1;
    codeword |= bit;
    codeword
}

fn build_table(
    lengths: &[u8],
    entries: &[u32],
    codes: &mut [u16],
    primary: &mut [u32],
    secondary: &mut Vec<u16>,
    distance: bool,
    double_literal: bool,
) -> bool {
    let mut histogram = [0usize; 16];
    for &length in lengths {
        histogram[usize::from(length)] += 1;
    }
    let mut max_length = 15;
    while max_length > 1 && histogram[max_length] == 0 {
        max_length -= 1;
    }
    if distance {
        if histogram[1..].iter().all(|&c| c == 0) {
            primary.fill(0);
            secondary.clear();
            return true;
        } else if max_length == 1 && histogram[1] == 1 {
            let Some(symbol) = lengths.iter().position(|&l| l == 1) else {
                return false;
            };
            codes[symbol] = 0;
            let entry = entries
                .get(symbol)
                .copied()
                .unwrap_or((symbol as u32) << 16)
                | 1;
            for chunk in primary.chunks_mut(2) {
                chunk[0] = entry;
                chunk[1] = 0;
            }
            return true;
        }
    }
    let mut offsets = [0usize; 16];
    let mut used = 0usize;
    offsets[1] = histogram[0];
    for i in 1..max_length {
        offsets[i + 1] = offsets[i] + histogram[i];
        used = (used << 1) + histogram[i];
    }
    used = (used << 1) + histogram[max_length];
    if used != 1 << max_length {
        return false;
    }
    let mut next_index = offsets;
    let mut sorted = [0usize; 288];
    for (symbol, &length) in lengths.iter().enumerate() {
        sorted[next_index[usize::from(length)]] = symbol;
        next_index[usize::from(length)] += 1;
    }
    let mut codeword = 0u16;
    let mut i = histogram[0];
    let bits = primary.len().ilog2() as usize;
    let mask = (1u16 << bits) - 1;
    for (length, &count) in histogram.iter().enumerate().take(bits + 1).skip(1) {
        let end = 1 << length;
        for _ in 0..count {
            let symbol = sorted[i];
            i += 1;
            primary[usize::from(codeword)] = entries
                .get(symbol)
                .copied()
                .unwrap_or((symbol as u32) << 16)
                | length as u32;
            codes[symbol] = codeword;
            codeword = next_codeword(codeword, end as u16);
        }
        if double_literal {
            for len1 in 1..length.saturating_sub(1) {
                let len2 = length - len1;
                for a in offsets[len1]..next_index[len1] {
                    for b in offsets[len2]..next_index[len2] {
                        let (s1, s2) = (sorted[a], sorted[b]);
                        if s1 < 256 && s2 < 256 {
                            let cw = codes[s1] | (codes[s2] << len1);
                            primary[usize::from(cw)] = (s1 as u32) << 16
                                | (s2 as u32) << 24
                                | LITERAL
                                | (2 << 8)
                                | length as u32;
                        }
                    }
                }
            }
        }
        if length < bits {
            primary.copy_within(0..end, end);
        }
    }
    secondary.clear();
    if max_length > bits {
        let mut start = 0;
        let mut prefix = u16::MAX;
        for (length, &count) in histogram
            .iter()
            .enumerate()
            .take(max_length + 1)
            .skip(bits + 1)
        {
            let size = 1usize << (length - bits);
            for _ in 0..count {
                if codeword & mask != prefix {
                    prefix = codeword & mask;
                    start = secondary.len();
                    primary[usize::from(prefix)] =
                        (start as u32) << 16 | EXCEPTIONAL | SECONDARY | (size as u32 - 1);
                    secondary.resize(start + size, 0);
                }
                let symbol = sorted[i];
                i += 1;
                codes[symbol] = codeword;
                secondary[start + usize::from(codeword >> bits)] =
                    ((symbol as u16) << 4) | length as u16;
                codeword = next_codeword(codeword, 1 << length);
            }
            if length < max_length && codeword & mask == prefix {
                secondary.extend_from_within(start..);
                let size = secondary.len() - start;
                primary[usize::from(prefix)] =
                    (start as u32) << 16 | EXCEPTIONAL | SECONDARY | (size as u32 - 1);
            }
        }
    }
    true
}

struct Tables {
    litlen: Box<[u32; 4096]>,
    secondary: Vec<u16>,
    dist: Box<[u32; 512]>,
    dist_secondary: Vec<u16>,
    fixed: bool,
}

impl Tables {
    fn new() -> Self {
        Tables {
            litlen: Box::new([0; 4096]),
            secondary: Vec::new(),
            dist: Box::new([0; 512]),
            dist_secondary: Vec::new(),
            fixed: false,
        }
    }

    fn load_fixed(&mut self) -> Result<()> {
        if self.fixed {
            return Ok(());
        }
        let mut lens = [8u8; 288];
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        let mut codes = [0u16; 288];
        let ok = build_table(
            &lens,
            &LITLEN_ENTRIES,
            &mut codes,
            &mut self.litlen[..],
            &mut self.secondary,
            false,
            true,
        ) && build_table(
            &[5u8; 32],
            &DIST_ENTRIES,
            &mut [0u16; 32],
            &mut self.dist[..],
            &mut self.dist_secondary,
            true,
            false,
        );
        if !ok {
            return Err(corrupt("an invalid Huffman code"));
        }
        self.fixed = true;
        Ok(())
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u64,
    nbits: u32,
}

impl<'a> Bits<'a> {
    #[inline(always)]
    fn fill(&mut self) {
        match self.data.get(self.pos..self.pos + 8) {
            Some(c) => {
                let w = u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                self.buffer |= w << self.nbits;
                self.pos += ((63 - self.nbits) >> 3) as usize;
                self.nbits |= 56;
            }
            None => self.fill_tail(),
        }
    }

    #[inline(never)]
    fn fill_tail(&mut self) {
        while self.nbits <= 56 {
            match self.data.get(self.pos) {
                Some(&b) => {
                    self.buffer |= u64::from(b) << self.nbits;
                    self.pos += 1;
                    self.nbits += 8;
                }
                None => break,
            }
        }
    }

    #[inline(always)]
    fn take(&mut self, n: u32) {
        self.buffer >>= n;
        self.nbits -= n;
    }

    fn peek(&self, n: u32) -> u32 {
        (self.buffer & ((1u64 << n) - 1)) as u32
    }

    fn need(&mut self, n: u32) -> Result<()> {
        if self.nbits < n {
            self.fill();
        }
        if self.nbits < n {
            return Err(corrupt("the deflate stream ends early"));
        }
        Ok(())
    }

    fn used(&self) -> usize {
        self.pos - (self.nbits / 8) as usize
    }
}

pub trait Rows {
    fn rows(&mut self, data: &[u8]) -> Result<()>;
}

struct Job {
    buf: Vec<u8>,
    rows: std::ops::Range<usize>,
    sum: std::ops::Range<usize>,
}

struct Pipe {
    tx: Option<SyncSender<Job>>,
    back: Receiver<Vec<u8>>,
    spares: Vec<Vec<u8>>,
}

enum Flow<'a> {
    Whole,
    Inline(&'a mut dyn Rows),
    Piped(Pipe),
}

struct Ring<'a> {
    buf: Vec<u8>,
    flow: Flow<'a>,
    base: usize,
    start: usize,
    limit: usize,
    adler: u32,
    adler_to: usize,
    line: usize,
    rows_left: usize,
}

impl Ring<'_> {
    fn hand_off(&mut self, dest: &mut usize, last: bool) -> Result<()> {
        let avail = *dest - self.start;
        let n = avail
            .checked_div(self.line)
            .map_or(0, |rows| rows.min(self.rows_left));
        self.rows_left -= n;
        let rows = self.start..self.start + n * self.line;
        let next = if self.rows_left == 0 { *dest } else { rows.end };
        let keep = next.min(dest.saturating_sub(WINDOW));
        match &mut self.flow {
            Flow::Whole => {}
            Flow::Inline(sink) => {
                sink.rows(&self.buf[rows])?;
                self.adler = adler32_update(self.adler, &self.buf[self.adler_to..*dest]);
                if !last && keep > 0 {
                    self.buf.copy_within(keep..*dest, 0);
                }
            }
            Flow::Piped(pipe) => {
                let sum = self.adler_to..*dest;
                let mut fresh = Vec::new();
                if !last {
                    fresh = match pipe.spares.pop() {
                        Some(v) => v,
                        None => pipe
                            .back
                            .recv()
                            .map_err(|_| corrupt("the row decoder stopped"))?,
                    };
                    fresh[..*dest - keep].copy_from_slice(&self.buf[keep..*dest]);
                }
                let job = Job {
                    buf: std::mem::replace(&mut self.buf, fresh),
                    rows,
                    sum,
                };
                let Some(tx) = pipe.tx.as_ref() else {
                    return Err(corrupt("the row decoder stopped"));
                };
                tx.send(job)
                    .map_err(|_| corrupt("the row decoder stopped"))?;
                if last {
                    pipe.tx = None;
                }
            }
        }
        if !last {
            *dest -= keep;
            self.start = next - keep;
            self.base += keep;
            self.adler_to = *dest;
        }
        Ok(())
    }
}

fn room(r: &mut Ring, dest: &mut usize, extra: usize) -> Result<()> {
    let need = *dest + SLACK + extra;
    if r.buf.len() >= need {
        return Ok(());
    }
    if matches!(r.flow, Flow::Whole) {
        let cap = r.limit.saturating_add(SLACK);
        if need > cap {
            return Err(corrupt("more image data than the header allows"));
        }
        let len = (r.buf.len() + r.buf.len() / 2).max(need).min(cap);
        r.buf.resize(len, 0);
        return Ok(());
    }
    r.hand_off(dest, false)?;
    if *dest + SLACK + extra > r.buf.len() {
        return Err(corrupt("a row is longer than the decode buffer"));
    }
    Ok(())
}

pub fn ring_len(line: usize) -> usize {
    (1 << 20).max(line + WINDOW + 4 * SLACK + 65536)
}

fn dynamic(st: &mut Bits, t: &mut Tables) -> Result<()> {
    st.need(14)?;
    let hlit = st.peek(5) as usize + 257;
    st.take(5);
    let hdist = st.peek(5) as usize + 1;
    st.take(5);
    let hclen = st.peek(4) as usize + 4;
    st.take(4);
    if hlit > 286 {
        return Err(corrupt("too many literal and length codes"));
    }
    if hdist > 30 {
        return Err(corrupt("too many distance codes"));
    }
    let mut pre = [0u8; 19];
    for &o in &PRECODE_ORDER[..hclen] {
        st.need(3)?;
        pre[o] = st.peek(3) as u8;
        st.take(3);
    }
    let mut pre_table = [0u32; 128];
    let mut pre_codes = [0u16; 19];
    if !build_table(
        &pre,
        &[],
        &mut pre_codes,
        &mut pre_table,
        &mut Vec::new(),
        false,
        false,
    ) {
        return Err(corrupt("an invalid Huffman code"));
    }
    let total = hlit + hdist;
    let mut lens = [0u8; 320];
    let mut i = 0;
    while i < total {
        st.need(7)?;
        let e = pre_table[st.peek(7) as usize];
        let (len, sym) = (e & 7, (e >> 16) as u8);
        if len == 0 {
            return Err(corrupt("an invalid Huffman code"));
        }
        st.take(len);
        let (value, count) = match sym {
            0..=15 => (sym, 1),
            16 => {
                st.need(2)?;
                let n = 3 + st.peek(2) as usize;
                st.take(2);
                if i == 0 {
                    return Err(corrupt("a repeat with no previous code length"));
                }
                (lens[i - 1], n)
            }
            17 => {
                st.need(3)?;
                let n = 3 + st.peek(3) as usize;
                st.take(3);
                (0, n)
            }
            _ => {
                st.need(7)?;
                let n = 11 + st.peek(7) as usize;
                st.take(7);
                (0, n)
            }
        };
        if i + count > total {
            return Err(corrupt("the code lengths run past their count"));
        }
        lens[i..i + count].fill(value);
        i += count;
    }
    if lens[256] == 0 {
        return Err(corrupt("a block with no end-of-block code"));
    }
    t.fixed = false;
    let mut codes = [0u16; 288];
    if !build_table(
        &lens[..hlit],
        &LITLEN_ENTRIES,
        &mut codes,
        &mut t.litlen[..],
        &mut t.secondary,
        false,
        true,
    ) {
        return Err(corrupt("an invalid Huffman code"));
    }
    let mut dist = [0u8; 32];
    dist[..hdist].copy_from_slice(&lens[hlit..hlit + hdist]);
    if !build_table(
        &dist,
        &DIST_ENTRIES,
        &mut [0u16; 32],
        &mut t.dist[..],
        &mut t.dist_secondary,
        true,
        false,
    ) {
        return Err(corrupt("an invalid Huffman code"));
    }
    Ok(())
}

fn copy_match(output: &mut [u8], d: usize, dist: usize, length: usize) {
    if dist == 1 {
        let last = output[d - 1];
        output[d..d + length].fill(last);
    } else {
        let start = d - dist;
        output.copy_within(start..start + 16, d);
        if length > 16 || dist < 16 {
            for i in (0..length).step_by(dist.min(16)).skip(1) {
                output.copy_within(start + i..start + i + 16, d + i);
            }
        }
    }
}

fn fast(st: &mut Bits, t: &Tables, output: &mut [u8], dest: &mut usize) -> Result<bool> {
    let end = output.len() - SLACK;
    let mut d = *dest;
    st.fill();
    let mut entry = t.litlen[(st.buffer & 0xfff) as usize];
    let mut eob = false;
    while d <= end && st.pos + 8 <= st.data.len() {
        let mut bits;
        let mut code_bits = u32::from(entry as u8);
        if entry & LITERAL != 0 {
            let e2 = t.litlen[((st.buffer >> code_bits) & 0xfff) as usize];
            let c2 = u32::from(e2 as u8);
            let e3 = t.litlen[((st.buffer >> (code_bits + c2)) & 0xfff) as usize];
            let c3 = u32::from(e3 as u8);
            let e4 = t.litlen[((st.buffer >> (code_bits + c2 + c3)) & 0xfff) as usize];
            output[d] = (entry >> 16) as u8;
            output[d + 1] = (entry >> 24) as u8;
            d += ((entry & 0xf00) >> 8) as usize;
            if e2 & LITERAL != 0 {
                output[d] = (e2 >> 16) as u8;
                output[d + 1] = (e2 >> 24) as u8;
                d += ((e2 & 0xf00) >> 8) as usize;
                if e3 & LITERAL != 0 {
                    output[d] = (e3 >> 16) as u8;
                    output[d + 1] = (e3 >> 24) as u8;
                    d += ((e3 & 0xf00) >> 8) as usize;
                    entry = e4;
                    st.take(code_bits + c2 + c3);
                    st.fill();
                    continue;
                } else {
                    st.take(code_bits + c2);
                    entry = e3;
                    code_bits = c3;
                    st.fill();
                    bits = st.buffer;
                }
            } else {
                st.take(code_bits);
                bits = st.buffer;
                entry = e2;
                code_bits = c2;
                if st.nbits < 48 {
                    st.fill();
                }
            }
        } else {
            bits = st.buffer;
        }
        let (base, extra, code_bits) = if entry & EXCEPTIONAL == 0 {
            (entry >> 16, (entry >> 8) as u8 as u32, code_bits)
        } else if entry & SECONDARY != 0 {
            let idx = (entry >> 16) + ((bits >> 12) as u32 & (entry & 0xff));
            let sec = t.secondary[idx as usize];
            let sym = usize::from(sec >> 4);
            let cb = u32::from(sec & 0xf);
            match sym {
                0..=255 => {
                    st.take(cb);
                    entry = t.litlen[(st.buffer & 0xfff) as usize];
                    st.fill();
                    output[d] = sym as u8;
                    d += 1;
                    continue;
                }
                256 => {
                    st.take(cb);
                    eob = true;
                    break;
                }
                286.. => return Err(corrupt("an invalid Huffman code")),
                _ => (
                    u32::from(LEN_BASE[sym - 257]),
                    u32::from(LEN_EXTRA[sym - 257]),
                    cb,
                ),
            }
        } else if entry & INVALID != 0 || code_bits == 0 {
            return Err(corrupt("an invalid Huffman code"));
        } else {
            st.take(code_bits);
            eob = true;
            break;
        };
        bits >>= code_bits;
        let length = base as usize + (bits & ((1u64 << extra) - 1)) as usize;
        bits >>= extra;
        let dist_entry = t.dist[(bits & 0x1ff) as usize];
        let (dist_base, dist_extra, dist_bits) = if dist_entry & LITERAL != 0 {
            (
                (dist_entry >> 16) as usize,
                (dist_entry >> 8) & 0xf,
                u32::from(dist_entry as u8),
            )
        } else if dist_entry >> 8 == 0 {
            return Err(corrupt("an invalid Huffman code"));
        } else {
            let idx = (dist_entry >> 16) + ((bits >> 9) as u32 & (dist_entry & 0xff));
            let sec = t.dist_secondary[idx as usize];
            let sym = usize::from(sec >> 4);
            if sym >= 30 {
                return Err(corrupt("an invalid Huffman code"));
            }
            (
                usize::from(DIST_BASE[sym]),
                u32::from(DIST_EXTRA[sym]),
                u32::from(sec & 0xf),
            )
        };
        bits >>= dist_bits;
        let dist = dist_base + (bits & ((1u64 << dist_extra) - 1)) as usize;
        if dist > d {
            return Err(corrupt("a match reaches before the start"));
        }
        st.take(code_bits + extra + dist_bits + dist_extra);
        st.fill();
        entry = t.litlen[(st.buffer & 0xfff) as usize];
        copy_match(output, d, dist, length);
        d += length;
    }
    *dest = d;
    Ok(eob)
}

fn careful(st: &mut Bits, t: &Tables, r: &mut Ring, dest: &mut usize) -> Result<()> {
    loop {
        st.fill();
        room(r, dest, 0)?;
        let out = &mut r.buf;
        let bits = st.buffer;
        let entry = t.litlen[(bits & 0xfff) as usize];
        let code_bits = u32::from(entry as u8);
        if entry & LITERAL != 0 {
            if st.nbits < code_bits {
                return Err(corrupt("the deflate stream ends early"));
            }
            out[*dest] = (entry >> 16) as u8;
            out[*dest + 1] = (entry >> 24) as u8;
            *dest += ((entry & 0xf00) >> 8) as usize;
            st.take(code_bits);
            if r.base + *dest > r.limit {
                return Err(corrupt("more image data than the header allows"));
            }
            continue;
        }
        let (base, extra, code_bits) = if entry & EXCEPTIONAL == 0 {
            (entry >> 16, (entry >> 8) as u8 as u32, code_bits)
        } else if entry & SECONDARY != 0 {
            let idx = (entry >> 16) + ((bits >> 12) as u32 & (entry & 0xff));
            let sec = t.secondary[idx as usize];
            let sym = usize::from(sec >> 4);
            let cb = u32::from(sec & 0xf);
            if st.nbits < cb {
                return Err(corrupt("the deflate stream ends early"));
            }
            match sym {
                0..=255 => {
                    st.take(cb);
                    out[*dest] = sym as u8;
                    *dest += 1;
                    if r.base + *dest > r.limit {
                        return Err(corrupt("more image data than the header allows"));
                    }
                    continue;
                }
                256 => {
                    st.take(cb);
                    return Ok(());
                }
                286.. => return Err(corrupt("an invalid Huffman code")),
                _ => (
                    u32::from(LEN_BASE[sym - 257]),
                    u32::from(LEN_EXTRA[sym - 257]),
                    cb,
                ),
            }
        } else if entry & INVALID != 0 || code_bits == 0 {
            return Err(corrupt("an invalid Huffman code"));
        } else {
            if st.nbits < code_bits {
                return Err(corrupt("the deflate stream ends early"));
            }
            st.take(code_bits);
            return Ok(());
        };
        let mut b = bits >> code_bits;
        let length = base as usize + (b & ((1u64 << extra) - 1)) as usize;
        b >>= extra;
        let dist_entry = t.dist[(b & 0x1ff) as usize];
        let (dist_base, dist_extra, dist_bits) = if dist_entry & LITERAL != 0 {
            (
                (dist_entry >> 16) as usize,
                (dist_entry >> 8) & 0xf,
                u32::from(dist_entry as u8),
            )
        } else if dist_entry >> 8 == 0 {
            return Err(corrupt("an invalid Huffman code"));
        } else {
            let idx = (dist_entry >> 16) + ((b >> 9) as u32 & (dist_entry & 0xff));
            let sec = t.dist_secondary[idx as usize];
            let sym = usize::from(sec >> 4);
            if sym >= 30 {
                return Err(corrupt("an invalid Huffman code"));
            }
            (
                usize::from(DIST_BASE[sym]),
                u32::from(DIST_EXTRA[sym]),
                u32::from(sec & 0xf),
            )
        };
        b >>= dist_bits;
        let dist = dist_base + (b & ((1u64 << dist_extra) - 1)) as usize;
        let total = code_bits + extra + dist_bits + dist_extra;
        if st.nbits < total {
            return Err(corrupt("the deflate stream ends early"));
        }
        if dist > *dest {
            return Err(corrupt("a match reaches before the start"));
        }
        st.take(total);
        let d = *dest;
        for i in 0..length {
            out[d + i] = out[d + i - dist];
        }
        *dest += length;
        if r.base + *dest > r.limit {
            return Err(corrupt("more image data than the header allows"));
        }
    }
}

fn block(st: &mut Bits, t: &Tables, r: &mut Ring, dest: &mut usize) -> Result<()> {
    loop {
        room(r, dest, 0)?;
        let eob = fast(st, t, &mut r.buf[..], dest)?;
        if r.base + *dest > r.limit {
            return Err(corrupt("more image data than the header allows"));
        }
        if eob {
            return Ok(());
        }
        if st.pos + 8 > st.data.len() {
            return careful(st, t, r, dest);
        }
    }
}

fn zlib_body(data: &[u8]) -> Result<&[u8]> {
    let [cmf, flg, ..] = *data else {
        return Err(corrupt("the zlib stream is empty"));
    };
    if data.len() < 6 {
        return Err(corrupt("the zlib stream is too short"));
    }
    if cmf & 0xf != 8 {
        return Err(corrupt(&format!(
            "unknown zlib compression method {}",
            cmf & 0xf
        )));
    }
    if cmf >> 4 > 7 {
        return Err(corrupt("a zlib window larger than 32 KiB"));
    }
    if (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
        return Err(corrupt("the zlib header check failed"));
    }
    if flg & 0x20 != 0 {
        return Err(corrupt("a zlib preset dictionary"));
    }
    Ok(&data[2..])
}

fn check_adler(body: &[u8], end: usize, found: u32) -> Result<()> {
    let Some(sum) = body.get(end..end + 4) else {
        return Err(corrupt("the zlib stream ends before its Adler-32"));
    };
    let expected = u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]);
    if expected != found {
        return Err(corrupt(&format!(
            "Adler-32 mismatch, {expected:08x} stored, {found:08x} computed"
        )));
    }
    Ok(())
}

pub fn zlib(data: &[u8], out: &mut Vec<u8>, size_hint: usize, limit: usize) -> Result<usize> {
    let body = zlib_body(data)?;
    let want = size_hint.min(limit).saturating_add(SLACK);
    let mut buf = std::mem::take(out);
    if buf.len() < want {
        if buf.capacity() < want && buf.is_empty() {
            buf = vec![0; want];
        } else {
            buf.resize(want, 0);
        }
    }
    let mut ring = Ring {
        buf,
        flow: Flow::Whole,
        base: 0,
        start: 0,
        limit,
        adler: 1,
        adler_to: 0,
        line: 0,
        rows_left: 0,
    };
    let done = deflate(body, &mut ring);
    let found = match &done {
        Ok((written, _)) => adler32(&ring.buf[..*written]),
        Err(_) => 0,
    };
    *out = std::mem::take(&mut ring.buf);
    let (written, end) = done?;
    check_adler(body, end, found)?;
    Ok(written)
}

pub fn zlib_rows(
    data: &[u8],
    ring: &mut Vec<u8>,
    limit: usize,
    line: usize,
    rows: usize,
    sink: &mut dyn Rows,
) -> Result<usize> {
    let body = zlib_body(data)?;
    let mut r = Ring {
        buf: std::mem::take(ring),
        flow: Flow::Inline(sink),
        base: 0,
        start: 0,
        limit,
        adler: 1,
        adler_to: 0,
        line,
        rows_left: rows,
    };
    let done = deflate(body, &mut r);
    *ring = std::mem::take(&mut r.buf);
    let (written, end) = done?;
    check_adler(body, end, r.adler)?;
    Ok(written)
}

pub fn zlib_piped<S: Rows + Send>(
    data: &[u8],
    limit: usize,
    line: usize,
    rows: usize,
    sink: &mut S,
) -> Result<usize> {
    let body = zlib_body(data)?;
    let len = ring_len(line);
    std::thread::scope(|scope| {
        let (tx, rx) = sync_channel::<Job>(1);
        let (back_tx, back_rx) = channel::<Vec<u8>>();
        let consumer = scope.spawn(move || -> Result<u32> {
            let mut adler = 1;
            for job in rx {
                sink.rows(&job.buf[job.rows.clone()])?;
                adler = adler32_update(adler, &job.buf[job.sum.clone()]);
                let _ = back_tx.send(job.buf);
            }
            Ok(adler)
        });
        let mut r = Ring {
            buf: vec![0; len],
            flow: Flow::Piped(Pipe {
                tx: Some(tx),
                back: back_rx,
                spares: vec![vec![0; len], vec![0; len]],
            }),
            base: 0,
            start: 0,
            limit,
            adler: 1,
            adler_to: 0,
            line,
            rows_left: rows,
        };
        let done = deflate(body, &mut r);
        drop(r);
        let joined = consumer
            .join()
            .map_err(|_| corrupt("the row decoder stopped"))?;
        let adler = joined?;
        let (written, end) = done?;
        check_adler(body, end, adler)?;
        Ok(written)
    })
}

fn deflate(data: &[u8], r: &mut Ring) -> Result<(usize, usize)> {
    let mut st = Bits {
        data,
        pos: 0,
        buffer: 0,
        nbits: 0,
    };
    let mut tables = Box::new(Tables::new());
    let mut dest = 0usize;
    loop {
        st.need(3)?;
        let last = st.peek(1) == 1;
        let kind = (st.buffer >> 1) & 3;
        st.take(3);
        match kind {
            0 => {
                let skip = st.nbits % 8;
                st.take(skip);
                st.need(32)?;
                let len = st.peek(16) as u16;
                st.take(16);
                let nlen = st.peek(16) as u16;
                st.take(16);
                if len != !nlen {
                    return Err(corrupt("a stored block's length check failed"));
                }
                let mut left = usize::from(len);
                let mut at = st.used();
                if data.len() < at + left {
                    return Err(corrupt("a stored block runs past the end"));
                }
                if r.base + dest + left > r.limit {
                    return Err(corrupt("more image data than the header allows"));
                }
                while left > 0 {
                    let n = left.min(16384);
                    room(r, &mut dest, n)?;
                    r.buf[dest..dest + n].copy_from_slice(&data[at..at + n]);
                    dest += n;
                    at += n;
                    left -= n;
                }
                st.pos = at;
                st.buffer = 0;
                st.nbits = 0;
            }
            1 => {
                tables.load_fixed()?;
                block(&mut st, &tables, r, &mut dest)?;
            }
            2 => {
                dynamic(&mut st, &mut tables)?;
                block(&mut st, &tables, r, &mut dest)?;
            }
            _ => return Err(corrupt("a reserved deflate block type")),
        }
        if last {
            break;
        }
    }
    let total = r.base + dest;
    if !matches!(r.flow, Flow::Whole) {
        r.hand_off(&mut dest, true)?;
        if r.rows_left > 0 {
            return Err(corrupt("the image data ends early"));
        }
    }
    Ok((total, st.used()))
}

pub fn adler32(bytes: &[u8]) -> u32 {
    adler32_update(1, bytes)
}

pub fn adler32_update(state: u32, bytes: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    const CHUNK: usize = 5552 * 4;
    let (mut a, mut b) = (state & 0xffff, state >> 16);
    let mut av = [0u32; 4];
    let mut bv = [0u32; 4];
    let (body, rest) = bytes.split_at(bytes.len() - bytes.len() % 4);
    let chunks = body.chunks_exact(CHUNK);
    let tail = chunks.remainder();
    let lanes = |chunk: &[u8], av: &mut [u32; 4], bv: &mut [u32; 4]| {
        for v in chunk.chunks_exact(4) {
            for k in 0..4 {
                av[k] += u32::from(v[k]);
                bv[k] += av[k];
            }
        }
    };
    for chunk in chunks {
        lanes(chunk, &mut av, &mut bv);
        b += CHUNK as u32 * a;
        for k in 0..4 {
            av[k] %= MOD;
            bv[k] %= MOD;
        }
        b %= MOD;
    }
    lanes(tail, &mut av, &mut bv);
    b += tail.len() as u32 * a;
    for k in 0..4 {
        av[k] %= MOD;
        bv[k] %= MOD;
    }
    b %= MOD;
    for v in &mut bv {
        *v *= 4;
    }
    bv[1] += MOD - av[1];
    bv[2] += (MOD - av[2]) * 2;
    bv[3] += (MOD - av[3]) * 3;
    for v in av {
        a += v;
    }
    for v in bv {
        b += v;
    }
    for &byte in rest {
        a += u32::from(byte);
        b += a;
    }
    ((b % MOD) << 16) | (a % MOD)
}

pub fn adler32_combine(first: u32, second: u32, len2: usize) -> u32 {
    const MOD: u64 = 65521;
    let rem = len2 as u64 % MOD;
    let mut sum1 = u64::from(first & 0xffff);
    let mut sum2 = rem * sum1 % MOD;
    sum1 += u64::from(second & 0xffff) + MOD - 1;
    sum2 += u64::from(first >> 16) + u64::from(second >> 16) + MOD - rem;
    if sum1 >= MOD {
        sum1 -= MOD;
    }
    if sum1 >= MOD {
        sum1 -= MOD;
    }
    if sum2 >= MOD << 1 {
        sum2 -= MOD << 1;
    }
    if sum2 >= MOD {
        sum2 -= MOD;
    }
    (sum1 | sum2 << 16) as u32
}
