pub const AREA: &str = "png";

pub const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
pub const XORIG: [usize; 7] = [0, 4, 0, 2, 0, 1, 0];
pub const YORIG: [usize; 7] = [0, 0, 4, 0, 2, 0, 1];
pub const XSPC: [usize; 7] = [8, 8, 4, 4, 2, 2, 1];
pub const YSPC: [usize; 7] = [8, 8, 8, 4, 4, 2, 2];

pub fn crc(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for p in parts {
        for &b in *p {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 == 1 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
    }
    !c
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &v in data {
        a = (a + u32::from(v)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

pub struct Lcg(pub u32);

impl Lcg {
    pub fn byte(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 24) as u8
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

pub fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut c = (body.len() as u32).to_be_bytes().to_vec();
    c.extend_from_slice(kind);
    c.extend_from_slice(body);
    c.extend_from_slice(&crc(&[kind, body]).to_be_bytes());
    c
}

pub fn stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(b) = blocks.next() {
        let n = b.len() as u16;
        z.push(blocks.peek().is_none() as u8);
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

pub fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn filtered(kind: u8, line: &[u8], prev: &[u8], bpp: usize) -> Vec<u8> {
    let mut out = vec![kind];
    for i in 0..line.len() {
        let a = if i >= bpp { line[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        let pred = match kind {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((a as u16 + b as u16) / 2) as u8,
            _ => paeth(a, b, c),
        };
        out.push(line[i].wrapping_sub(pred));
    }
    out
}

pub fn samples(color: u8) -> usize {
    match color {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    }
}

pub struct Image {
    pub w: u32,
    pub h: u32,
    pub color: u8,
    pub depth: u8,
    pub interlaced: bool,
    pub key: Vec<u8>,
}

impl Image {
    pub fn new(w: u32, h: u32, color: u8, depth: u8, interlaced: bool) -> Self {
        Image {
            w,
            h,
            color,
            depth,
            interlaced,
            key: Vec::new(),
        }
    }

    pub fn ihdr(&self) -> Vec<u8> {
        let mut b = self.w.to_be_bytes().to_vec();
        b.extend_from_slice(&self.h.to_be_bytes());
        b.extend_from_slice(&[self.depth, self.color, 0, 0, self.interlaced as u8]);
        b
    }

    pub fn rows(&self, rng: &mut Lcg) -> Vec<u8> {
        let bits = samples(self.color) * self.depth as usize;
        let bpp = bits.div_ceil(8);
        let (w, h) = (self.w as usize, self.h as usize);
        let passes: Vec<(usize, usize)> = if self.interlaced {
            (0..7)
                .map(|p| {
                    (
                        w.saturating_sub(XORIG[p]).div_ceil(XSPC[p]),
                        h.saturating_sub(YORIG[p]).div_ceil(YSPC[p]),
                    )
                })
                .collect()
        } else {
            vec![(w, h)]
        };
        let mut out = Vec::new();
        for (pw, ph) in passes {
            if pw == 0 || ph == 0 {
                continue;
            }
            let stride = (pw * bits).div_ceil(8);
            let mut prev = vec![0u8; stride];
            for y in 0..ph {
                let mut line = rng.bytes(stride);
                if self.color == 3 && self.depth == 8 {
                    line.iter_mut().for_each(|v| *v %= 200);
                }
                if out.is_empty() && y == 0 {
                    line[..self.key.len()].copy_from_slice(&self.key);
                }
                out.extend(filtered((y % 5) as u8, &line, &prev, bpp));
                prev = line;
            }
        }
        out
    }

    pub fn file(&self, rng: &mut Lcg, before_idat: &[Vec<u8>]) -> Vec<u8> {
        let rows = self.rows(rng);
        self.wrap(&rows, before_idat)
    }

    pub fn wrap(&self, rows: &[u8], before_idat: &[Vec<u8>]) -> Vec<u8> {
        let z = stored(rows);
        let mut f = SIGNATURE.to_vec();
        f.extend(chunk(b"IHDR", &self.ihdr()));
        for c in before_idat {
            f.extend_from_slice(c);
        }
        let third = z.len() / 3;
        f.extend(chunk(b"IDAT", &z[..third]));
        f.extend(chunk(b"IDAT", &[]));
        f.extend(chunk(b"IDAT", &z[third..]));
        f.extend(chunk(b"IEND", &[]));
        f
    }
}

pub fn palette(rng: &mut Lcg) -> Vec<u8> {
    chunk(b"PLTE", &rng.bytes(200 * 3))
}

pub const CLAIMED: [(u8, &[u8]); 5] = [
    (0, &[1, 2, 4, 8, 16]),
    (2, &[8, 16]),
    (3, &[1, 2, 4, 8]),
    (4, &[8, 16]),
    (6, &[8, 16]),
];

pub const TRNS: [(u8, u8, &[u8]); 7] = [
    (0, 1, &[0, 1]),
    (0, 2, &[0, 2]),
    (0, 4, &[0, 9]),
    (0, 8, &[0, 77]),
    (0, 16, &[0x12, 0x34]),
    (2, 8, &[0, 7, 0, 7, 0, 7]),
    (2, 16, &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
];

pub struct Case {
    pub name: String,
    pub color: u8,
    pub depth: u8,
    pub file: Vec<u8>,
}

impl Case {
    pub fn wide(&self) -> bool {
        self.depth == 16
    }

    pub fn sub_byte_grey(&self) -> bool {
        self.color == 0 && self.depth < 8
    }
}

pub fn claimed() -> Vec<Case> {
    let mut rng = Lcg(99);
    let mut out = Vec::new();
    for (color, depths) in CLAIMED {
        for &depth in depths {
            for interlaced in [false, true] {
                let img = Image::new(17, 11, color, depth, interlaced);
                let extra = if color == 3 {
                    vec![palette(&mut rng), chunk(b"tRNS", &rng.bytes(64))]
                } else {
                    vec![]
                };
                out.push(Case {
                    name: format!("claimed-c{color}-d{depth}-i{}", u8::from(interlaced)),
                    color,
                    depth,
                    file: img.file(&mut rng, &extra),
                });
            }
        }
    }
    out
}

pub fn keyed() -> Vec<Case> {
    let mut rng = Lcg(31);
    let mut out = Vec::new();
    for (color, depth, key) in TRNS {
        for interlaced in [false, true] {
            let mut img = Image::new(13, 9, color, depth, interlaced);
            if depth >= 8 {
                img.key = if depth == 16 {
                    key.to_vec()
                } else {
                    key.iter().skip(1).step_by(2).copied().collect()
                };
            }
            out.push(Case {
                name: format!("trns-c{color}-d{depth}-i{}", u8::from(interlaced)),
                color,
                depth,
                file: img.file(&mut rng, &[chunk(b"tRNS", key)]),
            });
        }
    }
    out
}

pub fn cases() -> Vec<Case> {
    let mut all = claimed();
    all.extend(keyed());
    all
}

pub fn file_of(name: &str) -> Result<Vec<u8>, String> {
    cases()
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| c.file)
        .ok_or_else(|| format!("no PNG case named {name}"))
}
