const TABLE: [[u32; 256]; 16] = {
    let mut t = [[0u32; 256]; 16];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][n] = c;
        n += 1;
    }
    let mut s = 1;
    while s < 16 {
        let mut i = 0;
        while i < 256 {
            let prev = t[s - 1][i];
            t[s][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            i += 1;
        }
        s += 1;
    }
    t
};

#[inline(always)]
fn fold_16(crc: u32, b: &[u8; 16]) -> u32 {
    let w0 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) ^ crc;
    TABLE[0x0][b[0xf] as usize]
        ^ TABLE[0x1][b[0xe] as usize]
        ^ TABLE[0x2][b[0xd] as usize]
        ^ TABLE[0x3][b[0xc] as usize]
        ^ TABLE[0x4][b[0xb] as usize]
        ^ TABLE[0x5][b[0xa] as usize]
        ^ TABLE[0x6][b[0x9] as usize]
        ^ TABLE[0x7][b[0x8] as usize]
        ^ TABLE[0x8][b[0x7] as usize]
        ^ TABLE[0x9][b[0x6] as usize]
        ^ TABLE[0xa][b[0x5] as usize]
        ^ TABLE[0xb][b[0x4] as usize]
        ^ TABLE[0xc][(w0 >> 24) as usize]
        ^ TABLE[0xd][((w0 >> 16) & 0xff) as usize]
        ^ TABLE[0xe][((w0 >> 8) & 0xff) as usize]
        ^ TABLE[0xf][(w0 & 0xff) as usize]
}

fn update(mut crc: u32, buf: &[u8]) -> u32 {
    let mut blocks = buf.chunks_exact(16);
    for b in &mut blocks {
        if let Ok(b) = b.try_into() {
            crc = fold_16(crc, b);
        }
    }
    for &byte in blocks.remainder() {
        crc = TABLE[0][((crc as u8) ^ byte) as usize] ^ (crc >> 8);
    }
    crc
}

pub(super) fn crc(parts: &[&[u8]]) -> u32 {
    !parts.iter().fold(!0u32, |c, p| update(c, p))
}

#[cfg(test)]
mod tests {
    use super::crc;

    #[test]
    fn matches_the_reference_vectors_at_every_length() {
        assert_eq!(crc(&[]), 0);
        assert_eq!(crc(&[b"hello world"]), 0x0d4a_1185);
        assert_eq!(crc(&[b"hello", b" world"]), 0x0d4a_1185);
        assert_eq!(crc(&[b"123456789"]), 0xcbf4_3926);
        assert_eq!(crc(&[b"IEND"]), 0xae42_6082);
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 31 + 7) as u8).collect();
        for len in [0, 1, 15, 16, 17, 63, 64, 65, 999, 1000] {
            let mut slow = !0u32;
            for &b in &data[..len] {
                slow ^= b as u32;
                for _ in 0..8 {
                    slow = if slow & 1 != 0 {
                        0xedb8_8320 ^ (slow >> 1)
                    } else {
                        slow >> 1
                    };
                }
            }
            assert_eq!(crc(&[&data[..len]]), !slow, "len {len}");
            let (a, b) = data[..len].split_at(len / 3);
            assert_eq!(crc(&[a, b]), !slow, "split {len}");
        }
    }
}
