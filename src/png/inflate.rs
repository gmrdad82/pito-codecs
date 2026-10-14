use crate::media::{Error, Result};

const NUM_PRECODE_SYMS: usize = 19;
const NUM_LITLEN_SYMS: usize = 288;
const NUM_OFFSET_SYMS: usize = 32;
const MAX_LENS_OVERRUN: usize = 137;
const PRECODE_LENS_PERMUTATION: [u8; NUM_PRECODE_SYMS] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];
const PRECODE_ENOUGH: usize = 128;
const MAX_CODEWORD_LENGTH: usize = 15;
const PRECODE_TABLE_BITS: usize = 7;
const LITLEN_TABLE_BITS: usize = 11;
const LITLEN_ENOUGH: usize = 2342;
const OFFSET_TABLE_BITS: usize = 8;
const OFFSET_ENOUGH: usize = 512;
const MAX_NUM_SYMS: usize = 288;
const MAX_PRE_CODEWORD_LEN: u8 = 7;

const HUFFDEC_LITERAL: u32 = 0x8000_0000;
const HUFFDEC_EXCEPTIONAL: u32 = 0x0000_8000;
const HUFFDEC_SUITABLE_POINTER: u32 = 0x0000_4000;
const HUFFDEC_END_OF_BLOCK: u32 = 0x0000_2000;

const BLOCK_UNCOMPRESSED: u64 = 0;
const BLOCK_STATIC: u64 = 1;
const BLOCK_DYNAMIC: u64 = 2;

const MAX_MATCH_LEN: usize = 258;
const FASTCOPY_BYTES: usize = 16;
const FASTLOOP_MAX_BYTES_WRITTEN: usize = 6 + MAX_MATCH_LEN + 2 * FASTCOPY_BYTES;
const RESIZE_BY: usize = 4096;

static PRECODE_DECODE_RESULTS: [u32; 19] = precode_results();
static LITLEN_DECODE_RESULTS: [u32; 288] = litlen_results();

const fn precode_results() -> [u32; 19] {
    let mut table = [0u32; 19];
    let mut i = 0;
    while i < 19 {
        table[i] = (i as u32) << 16;
        i += 1;
    }
    table
}

const fn litlen_results() -> [u32; 288] {
    const LENGTHS: [(u32, u32); 31] = [
        (3, 0),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 0),
        (11, 1),
        (13, 1),
        (15, 1),
        (17, 1),
        (19, 2),
        (23, 2),
        (27, 2),
        (31, 2),
        (35, 3),
        (43, 3),
        (51, 3),
        (59, 3),
        (67, 4),
        (83, 4),
        (99, 4),
        (115, 4),
        (131, 5),
        (163, 5),
        (195, 5),
        (227, 5),
        (258, 0),
        (258, 0),
        (258, 0),
    ];
    let mut results = [0u32; 288];
    let mut i = 0;
    while i < 256 {
        results[i] = ((i as u32) << 16) | HUFFDEC_LITERAL;
        i += 1;
    }
    results[256] = HUFFDEC_EXCEPTIONAL | HUFFDEC_END_OF_BLOCK;
    let mut j = 0;
    while j < 31 {
        results[257 + j] = (LENGTHS[j].0 << 16) | LENGTHS[j].1;
        j += 1;
    }
    results
}

const fn entry(base: u32, extra: u32) -> u32 {
    (base << 16) | extra
}

static OFFSET_DECODE_RESULTS: [u32; 32] = [
    entry(1, 0),
    entry(2, 0),
    entry(3, 0),
    entry(4, 0),
    entry(5, 1),
    entry(7, 1),
    entry(9, 2),
    entry(13, 2),
    entry(17, 3),
    entry(25, 3),
    entry(33, 4),
    entry(49, 4),
    entry(65, 5),
    entry(97, 5),
    entry(129, 6),
    entry(193, 6),
    entry(257, 7),
    entry(385, 7),
    entry(513, 8),
    entry(769, 8),
    entry(1025, 9),
    entry(1537, 9),
    entry(2049, 10),
    entry(3073, 10),
    entry(4097, 11),
    entry(6145, 11),
    entry(8193, 12),
    entry(12289, 12),
    entry(16385, 13),
    entry(24577, 13),
    entry(24577, 13),
    entry(24577, 13),
];

fn corrupt(what: &str) -> Error {
    Error::Invalid(format!("PNG image data: {what}"))
}

struct Bits<'a> {
    src: &'a [u8],
    position: usize,
    bits_left: u8,
    buffer: u64,
    over_read: usize,
}

impl<'a> Bits<'a> {
    fn new(src: &'a [u8]) -> Self {
        Bits {
            src,
            position: 0,
            bits_left: 0,
            buffer: 0,
            over_read: 0,
        }
    }

    #[inline(always)]
    fn refill(&mut self) {
        match self.src.get(self.position..self.position + 8) {
            Some(bytes) => self.take(bytes),
            None => self.refill_slow(),
        }
    }

    #[inline(always)]
    fn refill_inner(&mut self) {
        if let Some(bytes) = self.src.get(self.position..self.position + 8) {
            self.take(bytes);
        }
    }

    #[inline(always)]
    fn take(&mut self, bytes: &[u8]) {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(bytes);
        let fresh = u64::from_le_bytes(buf);
        self.position += ((63 ^ self.bits_left) >> 3) as usize;
        self.buffer |= fresh << self.bits_left;
        self.bits_left |= 56;
    }

    #[inline(never)]
    fn refill_slow(&mut self) {
        let rest = self.src.get(self.position..).unwrap_or(&[]);
        for byte in rest {
            if self.bits_left >= 56 {
                break;
            }
            self.buffer |= u64::from(*byte) << self.bits_left;
            self.bits_left += 8;
            self.position += 1;
        }
        while self.bits_left < 56 {
            self.bits_left += 8;
            self.over_read += 1;
        }
    }

    #[inline(always)]
    fn peek<const N: usize>(&self) -> usize {
        (self.buffer & ((1 << N) - 1)) as usize
    }

    #[inline(always)]
    fn peek_var(&self, n: usize) -> usize {
        (self.buffer & ((1u64 << (n & 63)) - 1)) as usize
    }

    #[inline(always)]
    fn get(&mut self, n: u8) -> u64 {
        let value = self.buffer & ((1u64 << n) - 1);
        self.skip(n);
        value
    }

    #[inline(always)]
    fn skip(&mut self, n: u8) {
        let n = n.min(self.bits_left);
        self.bits_left -= n;
        self.buffer >>= n;
    }

    fn has(&self, n: u8) -> bool {
        self.bits_left >= n
    }

    fn consumed(&self) -> usize {
        self.position
            .saturating_sub(usize::from(self.bits_left >> 3))
    }

    fn remaining(&self) -> usize {
        self.src.len().saturating_sub(self.position)
    }

    fn overrun(&self) -> bool {
        self.over_read > usize::from(self.bits_left >> 3)
    }
}

struct Tables {
    litlen: [u32; LITLEN_ENOUGH],
    offset: [u32; OFFSET_ENOUGH],
    static_loaded: bool,
}

pub fn zlib(data: &[u8], out: &mut Vec<u8>, size_hint: usize, limit: usize) -> Result<usize> {
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
    let body = &data[2..];
    let (written, end) = deflate(body, out, size_hint, limit)?;
    let Some(sum) = body.get(end..end + 4) else {
        return Err(corrupt("the zlib stream ends before its Adler-32"));
    };
    let expected = u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]);
    let found = adler32(&out[..written]);
    if expected != found {
        return Err(corrupt(&format!(
            "Adler-32 mismatch, {expected:08x} stored, {found:08x} computed"
        )));
    }
    Ok(written)
}

fn deflate(
    data: &[u8],
    out: &mut Vec<u8>,
    size_hint: usize,
    limit: usize,
) -> Result<(usize, usize)> {
    let mut tables = Box::new(Tables {
        litlen: [0; LITLEN_ENOUGH],
        offset: [0; OFFSET_ENOUGH],
        static_loaded: false,
    });
    let mut stream = Bits::new(data);
    let wanted = size_hint.saturating_add(FASTLOOP_MAX_BYTES_WRITTEN);
    if out.len() < wanted {
        out.resize(wanted, 0);
    }
    let mut dest = 0usize;
    loop {
        stream.refill();
        let last = stream.get(1) == 1;
        let kind = stream.get(2);
        if kind == BLOCK_UNCOMPRESSED {
            if stream.overrun() {
                return Err(corrupt("the stream ends inside a stored block"));
            }
            let partial = stream.bits_left & 7;
            stream.skip(partial);
            let len = stream.get(16) as u16;
            let nlen = stream.get(16) as u16;
            if len != !nlen {
                return Err(corrupt("a stored block's length check failed"));
            }
            let len = usize::from(len);
            let start = stream.consumed() + stream.over_read;
            let Some(source) = data.get(start..start + len) else {
                return Err(corrupt("a stored block runs past the end"));
            };
            if dest + len > limit {
                return Err(corrupt("more image data than the header allows"));
            }
            if dest + len + FASTCOPY_BYTES > out.len() {
                out.resize(out.len() + RESIZE_BY + len, 0);
            }
            out[dest..dest + len].copy_from_slice(source);
            dest += len;
            stream.position = start + len;
            stream.buffer = 0;
            stream.bits_left = 0;
            stream.over_read = 0;
            if last {
                break;
            }
            continue;
        }
        if kind == 3 {
            return Err(corrupt("a reserved deflate block type"));
        }
        build_tables(&mut stream, kind, &mut tables)?;
        let litlen = &tables.litlen;
        let offsets = &tables.offset;
        let mut entry;
        let mut saved;
        'block: {
            if 3 * FASTCOPY_BYTES < stream.remaining() {
                stream.refill_inner();
                entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                loop {
                    if dest + FASTLOOP_MAX_BYTES_WRITTEN > out.len() {
                        out.resize(out.len() + FASTLOOP_MAX_BYTES_WRITTEN + RESIZE_BY, 0);
                    }
                    if stream.src.len() < stream.position + 8 {
                        break;
                    }
                    stream.refill_inner();
                    saved = stream.buffer;
                    stream.skip((entry & 0xff) as u8);
                    if entry & HUFFDEC_LITERAL != 0 {
                        let literal = entry >> 16;
                        entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                        saved = stream.buffer;
                        stream.skip(entry as u8);
                        out[dest] = literal as u8;
                        dest += 1;
                        if entry & HUFFDEC_LITERAL != 0 {
                            let literal = entry >> 16;
                            entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                            out[dest] = literal as u8;
                            dest += 1;
                            continue;
                        }
                    }
                    if entry & HUFFDEC_EXCEPTIONAL != 0 {
                        if entry & HUFFDEC_END_OF_BLOCK != 0 {
                            break 'block;
                        }
                        let bits = ((entry >> 8) & 0x3f) as usize;
                        let pos = (entry >> 16) as usize + stream.peek_var(bits);
                        saved = stream.buffer;
                        entry = litlen[pos.min(LITLEN_ENOUGH - 1)];
                        stream.skip(entry as u8);
                        if entry & HUFFDEC_LITERAL != 0 {
                            let literal = entry >> 16;
                            entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                            out[dest] = literal as u8;
                            dest += 1;
                            continue;
                        }
                        if entry & HUFFDEC_END_OF_BLOCK != 0 {
                            break 'block;
                        }
                    }
                    let length_entry = entry;
                    entry = offsets[stream.peek::<OFFSET_TABLE_BITS>()];
                    let mut length = (length_entry >> 16) as usize;
                    let mask = (1u64 << (length_entry as u8 & 63)) - 1;
                    length += (saved & mask) as usize >> ((length_entry >> 8) as u8 & 63);
                    if entry & HUFFDEC_EXCEPTIONAL != 0 {
                        stream.skip(OFFSET_TABLE_BITS as u8);
                        let extra = stream.peek_var(((entry >> 8) & 0x3f) as usize);
                        entry = offsets[((entry >> 16) as usize + extra) & 511];
                    }
                    saved = stream.buffer;
                    stream.skip((entry & 0xff) as u8);
                    let mask = (1u64 << (entry as u8 & 63)) - 1;
                    let mut offset = (entry >> 16) as usize;
                    offset += (saved & mask) as usize >> (((entry >> 8) & 0xff) as u8 & 63);
                    if offset > dest || offset == 0 {
                        return Err(corrupt("a match reaches before the start"));
                    }
                    let src = dest - offset;
                    if stream.bits_left < 11 {
                        stream.refill_inner();
                    }
                    out.copy_within(src..src + FASTCOPY_BYTES, dest);
                    entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                    let mut at = dest;
                    dest += length;
                    if offset == 1 {
                        let byte = out[src];
                        out[at..dest].fill(byte);
                    } else if offset <= FASTCOPY_BYTES && at + offset < dest {
                        let mut from = src + offset;
                        let mut to = at + offset;
                        loop {
                            out.copy_within(from..from + FASTCOPY_BYTES, to);
                            from += offset;
                            to += offset;
                            if to > dest {
                                break;
                            }
                        }
                    } else if length > FASTCOPY_BYTES {
                        at += FASTCOPY_BYTES;
                        let mut from = src + FASTCOPY_BYTES;
                        loop {
                            out.copy_within(from..from + FASTCOPY_BYTES, at);
                            from += FASTCOPY_BYTES;
                            at += FASTCOPY_BYTES;
                            if at > dest {
                                break;
                            }
                        }
                    }
                    if dest > limit {
                        return Err(corrupt("more image data than the header allows"));
                    }
                    if stream.src.len() < stream.position + 8 {
                        break;
                    }
                }
            }
            loop {
                stream.refill();
                if stream.overrun() {
                    return Err(corrupt("the deflate stream ends early"));
                }
                entry = litlen[stream.peek::<LITLEN_TABLE_BITS>()];
                saved = stream.buffer;
                stream.skip((entry & 0xff) as u8);
                if entry & HUFFDEC_SUITABLE_POINTER != 0 {
                    let extra = stream.peek_var(((entry >> 8) & 0x3f) as usize);
                    entry = litlen[((entry >> 16) as usize + extra).min(LITLEN_ENOUGH - 1)];
                    saved = stream.buffer;
                    stream.skip((entry & 0xff) as u8);
                }
                let mut length = (entry >> 16) as usize;
                if entry & HUFFDEC_LITERAL != 0 {
                    if dest >= out.len() {
                        out.resize(out.len() + RESIZE_BY, 0);
                    }
                    out[dest] = length as u8;
                    dest += 1;
                    if dest > limit {
                        return Err(corrupt("more image data than the header allows"));
                    }
                    continue;
                }
                if entry & HUFFDEC_END_OF_BLOCK != 0 {
                    break 'block;
                }
                let mask = (1u64 << (entry as u8 & 63)) - 1;
                length += (saved & mask) as usize >> ((entry >> 8) as u8 & 63);
                stream.refill();
                entry = offsets[stream.peek::<OFFSET_TABLE_BITS>()];
                if entry & HUFFDEC_EXCEPTIONAL != 0 {
                    stream.skip(OFFSET_TABLE_BITS as u8);
                    let extra = stream.peek_var(((entry >> 8) & 0x3f) as usize);
                    entry = offsets[((entry >> 16) as usize + extra) & 511];
                }
                if dest + length + FASTCOPY_BYTES > out.len() {
                    out.resize(out.len() + RESIZE_BY + length, 0);
                }
                saved = stream.buffer;
                let mask = (1u64 << ((entry & 0xff) as u8 & 63)) - 1;
                let mut offset = (entry >> 16) as usize;
                offset += (saved & mask) as usize >> ((entry >> 8) as u8 & 63);
                if offset > dest || offset == 0 {
                    return Err(corrupt("a match reaches before the start"));
                }
                let src = dest - offset;
                stream.skip(entry as u8);
                if src + length + FASTCOPY_BYTES > dest {
                    for i in 0..length {
                        out[dest + i] = out[src + i];
                    }
                } else {
                    let (head, tail) = out.split_at_mut(dest);
                    tail[..length].copy_from_slice(&head[src..src + length]);
                }
                dest += length;
                if dest > limit {
                    return Err(corrupt("more image data than the header allows"));
                }
            }
        }
        if stream.overrun() {
            return Err(corrupt("the deflate stream ends early"));
        }
        if last {
            break;
        }
    }
    Ok((dest, stream.consumed() + stream.over_read))
}

fn build_tables(stream: &mut Bits, kind: u64, tables: &mut Tables) -> Result<()> {
    const COUNT: usize = NUM_LITLEN_SYMS + NUM_OFFSET_SYMS + MAX_LENS_OVERRUN;
    let mut lens = [0u8; COUNT];
    let num_litlen;
    let num_offset;
    if kind == BLOCK_DYNAMIC {
        tables.static_loaded = false;
        num_litlen = 257 + stream.get(5) as usize;
        num_offset = 1 + stream.get(5) as usize;
        let explicit = 4 + stream.get(4) as usize;
        stream.refill();
        let mut precode_lens = [0u8; NUM_PRECODE_SYMS];
        precode_lens[usize::from(PRECODE_LENS_PERMUTATION[0])] = stream.get(3) as u8;
        stream.refill();
        for i in PRECODE_LENS_PERMUTATION[1..].iter().take(explicit - 1) {
            precode_lens[usize::from(*i)] = stream.get(3) as u8;
        }
        let mut precode = [0u32; PRECODE_ENOUGH];
        build_table(
            &precode_lens,
            &PRECODE_DECODE_RESULTS,
            &mut precode,
            PRECODE_TABLE_BITS,
            NUM_PRECODE_SYMS,
            MAX_CODEWORD_LENGTH,
        )?;
        let total = num_litlen + num_offset;
        let mut i = 0;
        while i < total {
            if !stream.has(MAX_PRE_CODEWORD_LEN + 7) {
                stream.refill();
            }
            let e = precode[stream.peek::<{ MAX_PRE_CODEWORD_LEN as usize }>()];
            let presym = e >> 16;
            if !stream.has(e as u8) {
                return Err(corrupt("the code lengths end early"));
            }
            stream.skip(e as u8);
            match presym {
                0..=15 => {
                    lens[i] = presym as u8;
                    i += 1;
                }
                16 => {
                    if i == 0 {
                        return Err(corrupt("a repeat with no previous code length"));
                    }
                    let value = lens[i - 1];
                    let count = 3 + stream.get(2) as usize;
                    lens[i..i + 6].fill(value);
                    i += count;
                }
                17 => {
                    let count = 3 + stream.get(3) as usize;
                    lens[i..i + 10].fill(0);
                    i += count;
                }
                _ => {
                    let count = 11 + stream.get(7) as usize;
                    lens[i..i + count].fill(0);
                    i += count;
                }
            }
        }
        if stream.overrun() {
            return Err(corrupt("the code lengths end early"));
        }
    } else if kind == BLOCK_STATIC {
        if tables.static_loaded {
            return Ok(());
        }
        tables.static_loaded = true;
        lens[0..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..288].fill(8);
        lens[288..320].fill(5);
        num_litlen = 288;
        num_offset = 32;
    } else {
        return Err(corrupt("a reserved deflate block type"));
    }
    build_table(
        &lens[num_litlen..],
        &OFFSET_DECODE_RESULTS,
        &mut tables.offset,
        OFFSET_TABLE_BITS,
        num_offset,
        MAX_CODEWORD_LENGTH,
    )?;
    build_table(
        &lens,
        &LITLEN_DECODE_RESULTS,
        &mut tables.litlen,
        LITLEN_TABLE_BITS,
        num_litlen,
        MAX_CODEWORD_LENGTH,
    )
}

fn make_entry(results: &[u32], sym: usize, len: u32) -> u32 {
    results.get(sym).copied().unwrap_or(0) + (len << 8) + len
}

fn build_table(
    lens: &[u8],
    results: &[u32],
    table: &mut [u32],
    table_bits: usize,
    num_syms: usize,
    mut max_len: usize,
) -> Result<()> {
    const BITS: u32 = usize::BITS - 1;
    let bad = || corrupt("an invalid Huffman code");
    let mut len_counts = [0u32; MAX_CODEWORD_LENGTH + 1];
    let mut offsets = [0u32; MAX_CODEWORD_LENGTH + 1];
    let mut sorted = [0u16; MAX_NUM_SYMS];
    let lens = lens.get(..num_syms).ok_or_else(bad)?;
    for &l in lens {
        *len_counts.get_mut(usize::from(l)).ok_or_else(bad)? += 1;
    }
    while max_len > 1 && len_counts[max_len] == 0 {
        max_len -= 1;
    }
    offsets[1] = len_counts[0];
    let mut used = 0u32;
    for len in 1..max_len {
        offsets[len + 1] = offsets[len] + len_counts[len];
        used = (used << 1) + len_counts[len];
    }
    used = (used << 1) + len_counts[max_len];
    for (sym, &l) in lens.iter().enumerate() {
        let pos = usize::from(l);
        *sorted.get_mut(offsets[pos] as usize).ok_or_else(bad)? = sym as u16;
        offsets[pos] += 1;
    }
    let mut i = offsets[0] as usize;
    if used > 1 << max_len {
        return Err(bad());
    }
    if used < 1 << max_len {
        let e = if used == 0 {
            make_entry(results, 0, 1)
        } else {
            if used != 1 << (max_len - 1) || len_counts[1] != 1 {
                return Err(bad());
            }
            make_entry(results, usize::from(sorted[i]), 1)
        };
        table.fill(e);
        return Ok(());
    }
    let mut codeword = 0usize;
    let mut len = 1usize;
    let mut count = len_counts[1];
    while count == 0 {
        len += 1;
        count = *len_counts.get(len).ok_or_else(bad)?;
    }
    let mut end = 1usize << len;
    while len <= table_bits {
        loop {
            let e = make_entry(results, usize::from(sorted[i]), len as u32);
            i += 1;
            *table.get_mut(codeword).ok_or_else(bad)? = e;
            if codeword == end - 1 {
                for _ in len..table_bits {
                    table.copy_within(0..end, end);
                    end <<= 1;
                }
                return Ok(());
            }
            let bit = 1usize << (BITS - (codeword ^ (end - 1)).leading_zeros());
            codeword &= bit - 1;
            codeword |= bit;
            count -= 1;
            if count == 0 {
                break;
            }
        }
        loop {
            len += 1;
            if len <= table_bits {
                table.copy_within(0..end, end);
                end <<= 1;
            }
            count = *len_counts.get(len).ok_or_else(bad)?;
            if count != 0 {
                break;
            }
        }
    }
    end = 1 << table_bits;
    let mut prefix = usize::MAX;
    let mut start = 0usize;
    loop {
        if codeword & ((1 << table_bits) - 1) != prefix {
            prefix = codeword & ((1 << table_bits) - 1);
            start = end;
            let mut sub_bits = len - table_bits;
            used = count;
            while used < (1 << sub_bits) {
                sub_bits += 1;
                if sub_bits + table_bits > MAX_CODEWORD_LENGTH {
                    return Err(bad());
                }
                used = (used << 1) + len_counts[table_bits + sub_bits];
            }
            *table.get_mut(prefix).ok_or_else(bad)? = ((start as u32) << 16)
                | HUFFDEC_EXCEPTIONAL
                | HUFFDEC_SUITABLE_POINTER
                | ((sub_bits as u32) << 8)
                | table_bits as u32;
            end = start + (1 << sub_bits);
            if end > table.len() {
                return Err(bad());
            }
        }
        let stride = 1 << (len - table_bits);
        let mut j = start + (codeword >> table_bits);
        let e = make_entry(
            results,
            usize::from(*sorted.get(i).ok_or_else(bad)?),
            (len - table_bits) as u32,
        );
        i += 1;
        while j < end {
            table[j] = e;
            j += stride;
        }
        if codeword == (1 << len) - 1 {
            return Ok(());
        }
        let bit = 1usize << (BITS - (codeword ^ ((1 << len) - 1)).leading_zeros());
        codeword &= bit - 1;
        codeword |= bit;
        count -= 1;
        while count == 0 {
            len += 1;
            count = *len_counts.get(len).ok_or_else(bad)?;
        }
    }
}

pub fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    const CHUNK: usize = 5552 * 4;
    let (mut a, mut b) = (1u32, 0u32);
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
