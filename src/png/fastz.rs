const LENGTHS: [u8; 286] = [
    2, 3, 4, 5, 5, 6, 6, 7, 7, 7, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 10, 10, 10, 10, 10, 10, 10,
    10, 10, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 11, 11, 11, 11, 11, 11, 11,
    11, 11, 11, 10, 11, 10, 10, 10, 10, 10, 10, 10, 10, 10, 9, 9, 9, 9, 9, 8, 9, 8, 8, 8, 8, 8, 7,
    7, 7, 6, 6, 6, 5, 4, 3, 12, 12, 12, 9, 9, 11, 10, 11, 11, 10, 11, 11, 11, 11, 11, 11, 12, 11,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 9,
];

const CODES: [u16; 286] = codes(&LENGTHS);

const LENGTH_TO_SYMBOL: [u16; 256] = [
    257, 258, 259, 260, 261, 262, 263, 264, 265, 265, 266, 266, 267, 267, 268, 268, 269, 269, 269,
    269, 270, 270, 270, 270, 271, 271, 271, 271, 272, 272, 272, 272, 273, 273, 273, 273, 273, 273,
    273, 273, 274, 274, 274, 274, 274, 274, 274, 274, 275, 275, 275, 275, 275, 275, 275, 275, 276,
    276, 276, 276, 276, 276, 276, 276, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277, 277,
    277, 277, 277, 277, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278, 278,
    278, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 279, 280, 280,
    280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 280, 281, 281, 281, 281, 281,
    281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281, 281,
    281, 281, 281, 281, 281, 281, 281, 281, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282,
    282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282, 282,
    282, 282, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283,
    283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 283, 284, 284, 284, 284,
    284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284, 284,
    284, 284, 284, 284, 284, 284, 284, 284, 285,
];

const LENGTH_TO_LEN_EXTRA: [u8; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 0,
];

const HEADER: [u8; 54] = [
    120, 1, 237, 192, 3, 160, 36, 89, 150, 198, 241, 255, 119, 238, 141, 200, 204, 167, 114, 75,
    99, 174, 109, 219, 182, 109, 219, 182, 109, 219, 182, 109, 105, 140, 158, 150, 74, 175, 158,
    50, 51, 34, 238, 249, 118, 183, 106, 122, 166, 135, 59, 107, 213, 15,
];

const fn codes(lengths: &[u8; 286]) -> [u16; 286] {
    let mut codes = [0u16; 286];
    let mut code = 0u32;
    let mut len = 1;
    while len <= 16 {
        let mut i = 0;
        while i < lengths.len() {
            if lengths[i] == len {
                codes[i] = (code as u16).reverse_bits() >> (16 - len);
                code += 1;
            }
            i += 1;
        }
        code <<= 1;
        len += 1;
    }
    codes
}

pub struct Fast {
    out: Vec<u8>,
    buffer: u64,
    nbits: u8,
}

impl Fast {
    pub fn new(capacity: usize, last: bool) -> Self {
        let mut d = Fast {
            out: Vec::with_capacity(capacity),
            buffer: 0,
            nbits: 0,
        };
        d.out.push(if last { HEADER[2] } else { HEADER[2] & !1 });
        d.out.extend_from_slice(&HEADER[3..53]);
        d.bits(HEADER[53] as u64, 5);
        d
    }

    fn bits(&mut self, bits: u64, nbits: u8) {
        self.buffer |= bits << self.nbits;
        self.nbits += nbits;
        if self.nbits >= 64 {
            self.out.extend_from_slice(&self.buffer.to_le_bytes());
            self.nbits -= 64;
            self.buffer = bits.checked_shr((nbits - self.nbits) as u32).unwrap_or(0);
        }
    }

    fn literal(&mut self, b: u8) {
        self.bits(CODES[b as usize] as u64, LENGTHS[b as usize]);
    }

    fn run(&mut self, mut run: u32) {
        self.bits(CODES[0] as u64, LENGTHS[0]);
        run -= 1;
        while run >= 258 {
            self.bits(CODES[285] as u64, LENGTHS[285] + 1);
            run -= 258;
        }
        if run > 4 {
            let sym = LENGTH_TO_SYMBOL[run as usize - 3] as usize;
            self.bits(CODES[sym] as u64, LENGTHS[sym]);
            let extra_len = LENGTH_TO_LEN_EXTRA[run as usize - 3];
            let extra = ((run - 3) & ((1u32 << extra_len) - 1)) as u64;
            self.bits(extra, extra_len + 1);
        } else {
            self.bits(0, run as u8 * LENGTHS[0]);
        }
    }

    pub fn data(&mut self, data: &[u8]) {
        let mut run = 0;
        let mut chunks = data.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            let ichunk = u64::from_le_bytes(word);
            if ichunk == 0 {
                run += 8;
                continue;
            } else if run > 0 {
                let run_extra = ichunk.trailing_zeros() / 8;
                self.run(run + run_extra);
                run = 0;
                if run_extra > 0 {
                    run = ichunk.leading_zeros() / 8;
                    for &b in &word[run_extra as usize..8 - run as usize] {
                        self.literal(b);
                    }
                    continue;
                }
            }
            let run_start = ichunk.leading_zeros() / 8;
            if run_start > 0 {
                for &b in &word[..8 - run_start as usize] {
                    self.literal(b);
                }
                run = run_start;
                continue;
            }
            let [c0, c1, c2, c3, c4, c5, c6, c7] = word.map(|b| b as usize);
            let (n0, n1, n2, n3) = (LENGTHS[c0], LENGTHS[c1], LENGTHS[c2], LENGTHS[c3]);
            let bits = CODES[c0] as u64
                | ((CODES[c1] as u64) << n0)
                | ((CODES[c2] as u64) << (n0 + n1))
                | ((CODES[c3] as u64) << (n0 + n1 + n2));
            self.bits(bits, n0 + n1 + n2 + n3);
            let (n4, n5, n6, n7) = (LENGTHS[c4], LENGTHS[c5], LENGTHS[c6], LENGTHS[c7]);
            let bits = CODES[c4] as u64
                | ((CODES[c5] as u64) << n4)
                | ((CODES[c6] as u64) << (n4 + n5))
                | ((CODES[c7] as u64) << (n4 + n5 + n6));
            self.bits(bits, n4 + n5 + n6 + n7);
        }
        if run > 0 {
            self.run(run);
        }
        for &b in chunks.remainder() {
            self.literal(b);
        }
    }

    pub fn finish(mut self, last: bool) -> Vec<u8> {
        self.bits(CODES[256] as u64, LENGTHS[256]);
        if !last {
            self.bits(0, 3);
        }
        if !self.nbits.is_multiple_of(8) {
            self.bits(0, 8 - self.nbits % 8);
        }
        if self.nbits > 0 {
            let bytes = self.buffer.to_le_bytes();
            self.out
                .extend_from_slice(&bytes[..self.nbits as usize / 8]);
        }
        if !last {
            self.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        }
        self.out
    }
}
