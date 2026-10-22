use super::crc::crc8;
use super::{Fault, StreamInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Assignment {
    Independent,
    LeftSide,
    RightSide,
    MidSide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Number {
    Frame(u32),
    Sample(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub block_size: u32,
    pub rate: u32,
    pub assignment: Assignment,
    pub channels: usize,
    pub bits: u32,
    pub number: Number,
    pub len: usize,
}

fn byte(data: &[u8], at: usize) -> Result<u8, Fault> {
    data.get(at).copied().ok_or(Fault::Short)
}

fn utf8(data: &[u8], at: &mut usize, wide: bool) -> Result<u64, Fault> {
    let x = byte(data, *at)?;
    *at += 1;
    let (mut v, n) = match x {
        _ if x & 0x80 == 0 => (x as u64, 0),
        _ if x & 0xe0 == 0xc0 => ((x & 0x1f) as u64, 1),
        _ if x & 0xf0 == 0xe0 => ((x & 0x0f) as u64, 2),
        _ if x & 0xf8 == 0xf0 => ((x & 0x07) as u64, 3),
        _ if x & 0xfc == 0xf8 => ((x & 0x03) as u64, 4),
        _ if x & 0xfe == 0xfc => ((x & 0x01) as u64, 5),
        0xfe if wide => (0, 6),
        _ => return Err(Fault::Bad("a frame number that is not a UTF-8 style code")),
    };
    for _ in 0..n {
        let b = byte(data, *at)?;
        *at += 1;
        if b & 0xc0 != 0x80 {
            return Err(Fault::Bad("a frame number that is not a UTF-8 style code"));
        }
        v = (v << 6) | (b & 0x3f) as u64;
    }
    Ok(v)
}

pub(super) fn header(data: &[u8], info: Option<&StreamInfo>) -> Result<Header, Fault> {
    let b0 = byte(data, 0)?;
    let b1 = byte(data, 1)?;
    if b0 != 0xff || b1 & 0xfe != 0xf8 {
        return Err(Fault::Bad("no frame sync code"));
    }
    let b2 = byte(data, 2)?;
    let b3 = byte(data, 3)?;
    if b2 == 0xff || b3 == 0xff {
        return Err(Fault::Bad("a frame header holding a sync byte"));
    }
    let mut block_size = 0u32;
    let mut block_hint = 0u8;
    match b2 >> 4 {
        0 => return Err(Fault::Bad("a reserved block size code")),
        1 => block_size = 192,
        x @ 2..=5 => block_size = 576 << (x - 2),
        x @ 6..=7 => block_hint = x,
        x => block_size = 256 << (x - 8),
    }
    let mut rate = 0u32;
    let mut rate_hint = 0u8;
    match b2 & 0x0f {
        0 => match info {
            Some(i) => rate = i.rate,
            None => {
                return Err(Fault::Bad(
                    "a frame taking its sample rate from a missing STREAMINFO",
                ));
            }
        },
        1 => rate = 88_200,
        2 => rate = 176_400,
        3 => rate = 192_000,
        4 => rate = 8_000,
        5 => rate = 16_000,
        6 => rate = 22_050,
        7 => rate = 24_000,
        8 => rate = 32_000,
        9 => rate = 44_100,
        10 => rate = 48_000,
        11 => rate = 96_000,
        x @ 12..=14 => rate_hint = x,
        _ => return Err(Fault::Bad("an invalid sample rate code")),
    }
    let c = b3 >> 4;
    let (assignment, channels) = match c {
        0..=7 => (Assignment::Independent, c as usize + 1),
        8 => (Assignment::LeftSide, 2),
        9 => (Assignment::RightSide, 2),
        10 => (Assignment::MidSide, 2),
        _ => return Err(Fault::Bad("a reserved channel assignment")),
    };
    let bits = match (b3 & 0x0e) >> 1 {
        0 => match info {
            Some(i) => i.bits as u32,
            None => {
                return Err(Fault::Bad(
                    "a frame taking its sample size from a missing STREAMINFO",
                ));
            }
        },
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(Fault::Bad("a reserved sample size code")),
    };
    if b3 & 1 != 0 {
        return Err(Fault::Bad("a frame header with its reserved bit set"));
    }
    let variable = b1 & 1 != 0 || info.is_some_and(|i| i.min_block != i.max_block);
    let mut at = 4;
    let number = if variable {
        Number::Sample(utf8(data, &mut at, true)?)
    } else {
        Number::Frame(utf8(data, &mut at, false)? as u32)
    };
    if block_hint != 0 {
        let mut x = byte(data, at)? as u32;
        at += 1;
        if block_hint == 7 {
            x = (x << 8) | byte(data, at)? as u32;
            at += 1;
        }
        block_size = x + 1;
        if block_size > 65535 {
            return Err(Fault::Bad("a block size of 65536"));
        }
    }
    if rate_hint != 0 {
        let mut x = byte(data, at)? as u32;
        at += 1;
        if rate_hint != 12 {
            x = (x << 8) | byte(data, at)? as u32;
            at += 1;
        }
        rate = match rate_hint {
            12 => x * 1000,
            13 => x,
            _ => x * 10,
        };
    }
    let crc = byte(data, at)?;
    if crc8(&data[..at]) != crc {
        return Err(Fault::Bad("a frame header CRC mismatch"));
    }
    if bits > 31 {
        return Err(Fault::Unsupported("32-bit FLAC"));
    }
    Ok(Header {
        block_size,
        rate,
        assignment,
        channels,
        bits,
        number,
        len: at + 1,
    })
}

pub(super) fn left_side(left: &[i32], side: &mut [i32]) {
    for (s, &l) in side.iter_mut().zip(left) {
        *s = l.wrapping_sub(*s);
    }
}

pub(super) fn right_side(side: &mut [i32], right: &[i32]) {
    for (s, &r) in side.iter_mut().zip(right) {
        *s = s.wrapping_add(r);
    }
}

pub(super) fn mid_side(mid: &mut [i32], side: &mut [i32], bits: u32) {
    if bits <= 29 {
        for (m, s) in mid.iter_mut().zip(side.iter_mut()) {
            let side = *s;
            let mid = m.wrapping_shl(1) | (side & 1);
            *m = mid.wrapping_add(side) >> 1;
            *s = mid.wrapping_sub(side) >> 1;
        }
    } else {
        for (m, s) in mid.iter_mut().zip(side.iter_mut()) {
            let side = *s as i64;
            let mid = ((*m as i64) << 1) | (side & 1);
            *m = ((mid + side) >> 1) as i32;
            *s = ((mid - side) >> 1) as i32;
        }
    }
}

pub(super) fn in_range(samples: &[i32], bits: u32) -> bool {
    let lo = i32::MIN >> (32 - bits);
    let hi = i32::MAX >> (32 - bits);
    samples
        .iter()
        .fold(true, |ok, &s| ok & (lo <= s) & (s <= hi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flac::crc::crc8;

    fn with_crc(mut h: Vec<u8>) -> Vec<u8> {
        let c = crc8(&h);
        h.push(c);
        h
    }

    fn info(min: u16, max: u16) -> StreamInfo {
        StreamInfo {
            min_block: min,
            max_block: max,
            min_frame: 0,
            max_frame: 0,
            rate: 44_100,
            channels: 2,
            bits: 16,
            samples: 0,
            md5: [0; 16],
        }
    }

    #[test]
    fn reads_a_fixed_blocksize_header() {
        let h = with_crc(vec![0xff, 0xf8, 0xc9, 0x88, 0x00]);
        let got = header(&h, None).unwrap();
        assert_eq!(got.block_size, 4096);
        assert_eq!(got.rate, 44_100);
        assert_eq!(got.assignment, Assignment::LeftSide);
        assert_eq!(got.channels, 2);
        assert_eq!(got.bits, 16);
        assert_eq!(got.number, Number::Frame(0));
        assert_eq!(got.len, 6);
    }

    #[test]
    fn reads_uncommon_block_sizes_and_rates() {
        let h = with_crc(vec![0xff, 0xf9, 0x7c, 0x0c, 0xc2, 0xa2, 0x03, 0xe8, 0x0b]);
        let got = header(&h, None).unwrap();
        assert_eq!(got.number, Number::Sample(0xa2));
        assert_eq!(got.block_size, 1001);
        assert_eq!(got.rate, 11_000);
        assert_eq!(got.channels, 1);
        assert_eq!(got.bits, 24);
        let h = with_crc(vec![0xff, 0xf8, 0x6d, 0x08, 0x05, 0x10, 0x2b, 0x11]);
        let got = header(&h, None).unwrap();
        assert_eq!(got.block_size, 17);
        assert_eq!(got.rate, 11_025);
        let h = with_crc(vec![0xff, 0xf8, 0x1e, 0x08, 0x05, 0x04, 0x4c]);
        let got = header(&h, None).unwrap();
        assert_eq!(got.block_size, 192);
        assert_eq!(got.rate, 11_000);
    }

    #[test]
    fn takes_rate_and_size_from_streaminfo() {
        let h = with_crc(vec![0xff, 0xf8, 0xc0, 0x10, 0x03]);
        assert!(matches!(header(&h, None), Err(Fault::Bad(_))));
        let got = header(&h, Some(&info(4096, 4096))).unwrap();
        assert_eq!(got.rate, 44_100);
        assert_eq!(got.bits, 16);
        assert_eq!(got.number, Number::Frame(3));
    }

    #[test]
    fn variable_streaminfo_reads_a_sample_number() {
        let h = with_crc(vec![0xff, 0xf8, 0xc9, 0x18, 0xe2, 0x82, 0xac]);
        let got = header(&h, Some(&info(1024, 4096))).unwrap();
        assert_eq!(got.number, Number::Sample(0x20ac));
    }

    #[test]
    fn refuses_bad_headers_cleanly() {
        let bad: [Vec<u8>; 9] = [
            vec![0xff, 0xf8, 0x09, 0x18, 0x00],
            vec![0xff, 0xf8, 0xcf, 0x18, 0x00],
            vec![0xff, 0xf8, 0xc9, 0xb8, 0x00],
            vec![0xff, 0xf8, 0xc9, 0x16, 0x00],
            vec![0xff, 0xf8, 0xc9, 0x19, 0x00],
            vec![0xff, 0xf8, 0xc9, 0x18, 0xff],
            vec![0xff, 0xf8, 0xc9, 0x18, 0x80],
            vec![
                0xff, 0xf8, 0xc9, 0x18, 0xfe, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
            ],
            vec![0xff, 0xf8, 0x79, 0x18, 0x00, 0xff, 0xff],
        ];
        for (i, h) in bad.into_iter().enumerate() {
            assert!(
                matches!(header(&with_crc(h), None), Err(Fault::Bad(_))),
                "case {i}"
            );
        }
        let mut h = with_crc(vec![0xff, 0xf8, 0xc9, 0x18, 0x00]);
        h[5] ^= 1;
        assert!(matches!(header(&h, None), Err(Fault::Bad(_))));
        assert!(matches!(
            header(&[0xff, 0xf9, 0xc9, 0x18], None),
            Err(Fault::Short)
        ));
        assert!(matches!(header(&[0xff], None), Err(Fault::Short)));
        assert!(matches!(header(&[0x00, 0xf8], None), Err(Fault::Bad(_))));
        let h = with_crc(vec![0xff, 0xf8, 0xc9, 0x1e, 0x00]);
        assert!(matches!(header(&h, None), Err(Fault::Unsupported(_))));
    }

    #[test]
    fn stereo_decorrelation_matches_claxon_vectors() {
        let left = [2, 5, 83, 113, 127, -63, -45, -15];
        let right = [-5, -33, -59, -125, 127, 89, 7, 3];
        let mut side = [7, 38, 142, 238, 0, -152, -52, -18];
        left_side(&left, &mut side);
        assert_eq!(side, right);
        let mut side = [7, 38, 142, 238, 0, -152, -52, -18];
        right_side(&mut side, &right);
        assert_eq!(side, left);
        for bits in [16, 30] {
            let mut mid = [-2, -14, 12, -6, 127, 13, -19, -6];
            let mut side = [7, 38, 142, 238, 0, -152, -52, -18];
            mid_side(&mut mid, &mut side, bits);
            assert_eq!(mid, left, "bits {bits}");
            assert_eq!(side, right, "bits {bits}");
        }
    }

    #[test]
    fn range_check_follows_the_sample_size() {
        assert!(in_range(&[-32768, 32767, 0], 16));
        assert!(!in_range(&[32768], 16));
        assert!(!in_range(&[-32769], 16));
        assert!(in_range(&[i32::MIN, i32::MAX], 32));
        assert!(in_range(&[-8, 7], 4));
        assert!(!in_range(&[8], 4));
    }
}
