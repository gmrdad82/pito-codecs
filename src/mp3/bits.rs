pub struct Bits<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
    pub limit: usize,
}

impl<'a> Bits<'a> {
    pub fn new(buf: &'a [u8]) -> Bits<'a> {
        Bits {
            buf,
            pos: 0,
            limit: buf.len() * 8,
        }
    }

    #[inline(always)]
    fn byte(&self, i: usize) -> u32 {
        self.buf.get(i).copied().unwrap_or(0) as u32
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        let s = (self.pos & 7) as i32;
        let mut shl = n as i32 + s;
        let mut p = self.pos >> 3;
        self.pos += n as usize;
        if self.pos > self.limit {
            return 0;
        }
        let mut next = self.byte(p) & (255 >> s);
        p += 1;
        let mut cache = 0u32;
        loop {
            shl -= 8;
            if shl <= 0 {
                break;
            }
            cache |= next << shl;
            next = self.byte(p);
            p += 1;
        }
        cache | (next >> -shl)
    }
}
