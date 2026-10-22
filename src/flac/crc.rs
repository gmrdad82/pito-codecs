const fn crc8_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut j = 0;
        while j < 8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
            j += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn crc16_tables() -> [[u16; 256]; 8] {
    let mut t = [[0u16; 256]; 8];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut j = 0;
        while j < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
            j += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut j = 1;
    while j < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = t[j - 1][i];
            t[j][i] = t[0][(prev >> 8) as usize] ^ (prev << 8);
            i += 1;
        }
        j += 1;
    }
    t
}

static CRC8: [u8; 256] = crc8_table();
static CRC16: [[u16; 256]; 8] = crc16_tables();

pub(super) fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8[(c ^ b) as usize])
}

pub(super) fn crc16(data: &[u8]) -> u16 {
    let t = &CRC16;
    let mut crc = 0u16;
    let mut chunks = data.chunks_exact(8);
    for d in &mut chunks {
        crc ^= u16::from_be_bytes([d[0], d[1]]);
        crc = t[7][(crc >> 8) as usize]
            ^ t[6][(crc & 0xff) as usize]
            ^ t[5][d[2] as usize]
            ^ t[4][d[3] as usize]
            ^ t[3][d[4] as usize]
            ^ t[2][d[5] as usize]
            ^ t[1][d[6] as usize]
            ^ t[0][d[7] as usize];
    }
    for &b in chunks.remainder() {
        crc = (crc << 8) ^ t[0][((crc >> 8) as u8 ^ b) as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc16_bytewise(data: &[u8]) -> u16 {
        data.iter().fold(0u16, |c, &b| {
            (c << 8) ^ CRC16[0][((c >> 8) as u8 ^ b) as usize]
        })
    }

    #[test]
    fn crc8_matches_the_reference_vectors() {
        assert_eq!(crc8(&[0x1f]), 0x5d);
        assert_eq!(crc8(&[0x04, 0x01]), 0x53);
        assert_eq!(crc8(b"abc"), 0x5f);
        assert_eq!(crc8(&[]), 0);
    }

    #[test]
    fn crc16_matches_the_reference_vectors() {
        assert_eq!(crc16(&[0x1f]), 0x8041);
        assert_eq!(crc16(&[0x04, 0x01]), 0x1806);
        assert_eq!(crc16(b"abc"), 0xcadb);
        assert_eq!(crc16(b"123456789"), 0xfee8);
    }

    #[test]
    fn sliced_crc16_equals_the_bytewise_one_at_every_length() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 131 + 7) as u8).collect();
        for n in 0..data.len() {
            assert_eq!(crc16(&data[..n]), crc16_bytewise(&data[..n]), "length {n}");
        }
    }

    #[test]
    fn tables_match_libflac_spot_values() {
        assert_eq!(CRC8[1], 0x07);
        assert_eq!(CRC8[255], 0xf3);
        assert_eq!(CRC16[0][1], 0x8005);
        assert_eq!(CRC16[0][255], 0x0202);
    }
}
