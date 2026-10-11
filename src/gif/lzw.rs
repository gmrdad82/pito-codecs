const MAX_CODES: usize = 4096;

pub struct LzwDecoder {
    offset: Vec<u32>,
    len: Vec<u32>,
}

impl Default for LzwDecoder {
    fn default() -> Self {
        LzwDecoder {
            offset: vec![0; MAX_CODES],
            len: vec![0; MAX_CODES],
        }
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    #[inline(always)]
    fn refill(&mut self) {
        if self.pos + 8 <= self.data.len() {
            let word = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into().unwrap());
            self.acc |= word << self.bits;
            let take = (63 - self.bits) >> 3;
            self.pos += take as usize;
            self.bits += take << 3;
        } else {
            while self.bits <= 56 && self.pos < self.data.len() {
                self.acc |= (self.data[self.pos] as u64) << self.bits;
                self.pos += 1;
                self.bits += 8;
            }
        }
    }

    #[inline(always)]
    fn read(&mut self, n: u32) -> Option<u32> {
        if self.bits < n {
            self.refill();
            if self.bits < n {
                return None;
            }
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.bits -= n;
        Some(v)
    }
}

impl LzwDecoder {
    pub fn decode(&mut self, min_code_size: u8, data: &[u8], out: &mut [u8]) -> usize {
        let min = min_code_size.clamp(1, 11) as u32;
        let clear = 1u32 << min;
        let end = clear + 1;
        let mut size = min + 1;
        let mut next = clear + 2;
        let mut prev: Option<(usize, usize)> = None;
        let mut at = 0usize;
        let total = out.len();
        let mut bits = BitReader {
            data,
            pos: 0,
            acc: 0,
            bits: 0,
        };
        let offset = &mut self.offset[..];
        let lens = &mut self.len[..];
        while at < total {
            let Some(code) = bits.read(size) else { break };
            if code == clear {
                size = min + 1;
                next = clear + 2;
                prev = None;
                continue;
            }
            if code == end {
                break;
            }
            let start = at;
            match prev {
                None => {
                    if code >= clear {
                        break;
                    }
                    out[at] = code as u8;
                    at += 1;
                }
                Some((p, plen)) => {
                    if code < clear {
                        out[at] = code as u8;
                        at += 1;
                    } else if code < next {
                        let src = offset[code as usize] as usize;
                        let n = (lens[code as usize] as usize).min(total - at);
                        out.copy_within(src..src + n, at);
                        at += n;
                    } else if code == next {
                        let n = (plen + 1).min(total - at);
                        for i in 0..n {
                            out[at + i] = out[p + i];
                        }
                        at += n;
                    } else {
                        break;
                    }
                    if (next as usize) < MAX_CODES {
                        offset[next as usize] = p as u32;
                        lens[next as usize] = plen as u32 + 1;
                        next += 1;
                        if next == 1 << size && size < 12 {
                            size += 1;
                        }
                    }
                }
            }
            prev = Some((start, at - start));
        }
        at
    }
}

pub struct LzwEncoder {
    child: Vec<u16>,
    used: Vec<u32>,
    stream: Vec<u8>,
}

impl Default for LzwEncoder {
    fn default() -> Self {
        LzwEncoder {
            child: vec![0; MAX_CODES * 256],
            used: Vec::with_capacity(MAX_CODES),
            stream: Vec::new(),
        }
    }
}

struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter<'_> {
    #[inline(always)]
    fn put(&mut self, code: u32, n: u32) {
        self.acc |= (code as u64) << self.bits;
        self.bits += n;
        if self.bits >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.bits -= 32;
        }
    }

    fn flush(&mut self) {
        while self.bits > 0 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.bits = self.bits.saturating_sub(8);
        }
    }
}

impl LzwEncoder {
    pub fn encode<'r>(
        &mut self,
        min_code_size: u8,
        rows: impl Iterator<Item = &'r [u8]>,
        out: &mut Vec<u8>,
    ) {
        out.push(min_code_size);
        self.stream.clear();
        let min = min_code_size as u32;
        let clear = 1u32 << min;
        let end = clear + 1;
        let mut size = min + 1;
        let mut next = clear + 2;
        for &k in &self.used {
            self.child[k as usize] = 0;
        }
        self.used.clear();
        let child = &mut self.child[..];
        let used = &mut self.used;
        let mut w = BitWriter {
            out: &mut self.stream,
            acc: 0,
            bits: 0,
        };
        w.put(clear, size);
        let mut current: Option<u32> = None;
        for row in rows {
            let mut bytes = row.iter();
            let mut prefix = match current {
                Some(p) => p,
                None => match bytes.next() {
                    Some(&b) => b as u32,
                    None => continue,
                },
            };
            for &b in bytes {
                let key = ((prefix as usize) << 8) | b as usize;
                let c = child[key];
                if c != 0 {
                    prefix = c as u32;
                    continue;
                }
                w.put(prefix, size);
                if next as usize == MAX_CODES {
                    w.put(clear, size);
                    for &k in used.iter() {
                        child[k as usize] = 0;
                    }
                    used.clear();
                    size = min + 1;
                    next = clear + 2;
                } else {
                    child[key] = next as u16;
                    used.push(key as u32);
                    if next == 1 << size {
                        size += 1;
                    }
                    next += 1;
                }
                prefix = b as u32;
            }
            current = Some(prefix);
        }
        if let Some(c) = current {
            w.put(c, size);
            if (next as usize) < MAX_CODES && next == 1 << size {
                size += 1;
            }
        }
        w.put(end, size);
        w.flush();
        for chunk in self.stream.chunks(255) {
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
        out.push(0);
    }
}
