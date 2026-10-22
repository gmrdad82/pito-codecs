use crate::media::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct StreamInfo {
    pub min_block: u16,
    pub max_block: u16,
    pub min_frame: u32,
    pub max_frame: u32,
    pub rate: u32,
    pub channels: u8,
    pub bits: u8,
    pub samples: u64,
    pub md5: [u8; 16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekPoint {
    pub sample: u64,
    pub offset: u64,
    pub samples: u16,
}

pub(super) const STREAMINFO: u8 = 0;
pub(super) const SEEKTABLE: u8 = 3;
pub(super) const INVALID: u8 = 127;

impl StreamInfo {
    pub fn parse(b: &[u8]) -> Result<StreamInfo> {
        let Some(b) = b.get(..34) else {
            return Err(Error::Invalid(format!(
                "a FLAC STREAMINFO of {} bytes, not 34",
                b.len()
            )));
        };
        let u24 = |i: usize| u32::from_be_bytes([0, b[i], b[i + 1], b[i + 2]]);
        let mut md5 = [0u8; 16];
        md5.copy_from_slice(&b[18..34]);
        let info = StreamInfo {
            min_block: u16::from_be_bytes([b[0], b[1]]),
            max_block: u16::from_be_bytes([b[2], b[3]]),
            min_frame: u24(4),
            max_frame: u24(7),
            rate: (b[10] as u32) << 12 | (b[11] as u32) << 4 | (b[12] as u32) >> 4,
            channels: ((b[12] >> 1) & 7) + 1,
            bits: (((b[12] & 1) << 4) | (b[13] >> 4)) + 1,
            samples: ((b[13] & 0x0f) as u64) << 32
                | u32::from_be_bytes([b[14], b[15], b[16], b[17]]) as u64,
            md5,
        };
        if info.rate == 0 {
            return Err(Error::Invalid(
                "a FLAC STREAMINFO with no sample rate".to_string(),
            ));
        }
        if info.bits < 4 {
            return Err(Error::Invalid(format!(
                "a FLAC STREAMINFO of {}-bit samples",
                info.bits
            )));
        }
        Ok(info)
    }

    pub fn seconds(&self) -> f64 {
        self.samples as f64 / self.rate.max(1) as f64
    }

    pub(super) fn fixed_block(&self) -> Option<u32> {
        (self.min_block == self.max_block && self.max_block > 0).then_some(self.max_block as u32)
    }
}

pub(super) fn seek_table(b: &[u8]) -> Vec<SeekPoint> {
    let mut points: Vec<SeekPoint> = b
        .chunks_exact(18)
        .map(|p| SeekPoint {
            sample: u64::from_be_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]]),
            offset: u64::from_be_bytes([p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15]]),
            samples: u16::from_be_bytes([p[16], p[17]]),
        })
        .filter(|p| p.sample != u64::MAX)
        .collect();
    points.sort_by_key(|p| p.sample);
    points
}

pub fn config(data: &[u8]) -> Result<StreamInfo> {
    let mut rest = if let Some(r) = data.strip_prefix(b"fLaC") {
        r
    } else if data.len() > 8 && data[..4] == [0; 4] && data[4] & 0x7f == STREAMINFO {
        &data[4..]
    } else if data.len() == 34 {
        return StreamInfo::parse(data);
    } else {
        data
    };
    while rest.len() >= 4 {
        let kind = rest[0] & 0x7f;
        let last = rest[0] & 0x80 != 0;
        let len = u32::from_be_bytes([0, rest[1], rest[2], rest[3]]) as usize;
        let Some(body) = rest.get(4..4 + len) else {
            return Err(Error::Invalid(
                "a FLAC config with a cut metadata block".to_string(),
            ));
        };
        if kind == STREAMINFO {
            return StreamInfo::parse(body);
        }
        if last || kind == INVALID {
            break;
        }
        rest = &rest[4 + len..];
    }
    Err(Error::Invalid(
        "a FLAC config without STREAMINFO".to_string(),
    ))
}

#[cfg(test)]
pub(super) fn streaminfo_bytes(i: &StreamInfo) -> [u8; 34] {
    let mut b = [0u8; 34];
    b[0..2].copy_from_slice(&i.min_block.to_be_bytes());
    b[2..4].copy_from_slice(&i.max_block.to_be_bytes());
    b[4..7].copy_from_slice(&i.min_frame.to_be_bytes()[1..]);
    b[7..10].copy_from_slice(&i.max_frame.to_be_bytes()[1..]);
    b[10] = (i.rate >> 12) as u8;
    b[11] = (i.rate >> 4) as u8;
    b[12] = ((i.rate & 0x0f) as u8) << 4 | (i.channels - 1) << 1 | (i.bits - 1) >> 4;
    b[13] = ((i.bits - 1) & 0x0f) << 4 | (i.samples >> 32) as u8 & 0x0f;
    b[14..18].copy_from_slice(&(i.samples as u32).to_be_bytes());
    b[18..34].copy_from_slice(&i.md5);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StreamInfo {
        StreamInfo {
            min_block: 4096,
            max_block: 4096,
            min_frame: 14,
            max_frame: 12_345,
            rate: 96_000,
            channels: 6,
            bits: 24,
            samples: 0x1_2345_6789,
            md5: [7; 16],
        }
    }

    #[test]
    fn streaminfo_round_trips_every_field() {
        let i = sample();
        assert_eq!(StreamInfo::parse(&streaminfo_bytes(&i)).unwrap(), i);
        assert_eq!(i.fixed_block(), Some(4096));
        let mono = StreamInfo {
            channels: 1,
            bits: 16,
            rate: 44_100,
            ..i
        };
        assert_eq!(StreamInfo::parse(&streaminfo_bytes(&mono)).unwrap(), mono);
    }

    #[test]
    fn streaminfo_refuses_short_or_empty_fields() {
        let b = streaminfo_bytes(&sample());
        assert!(StreamInfo::parse(&b[..33]).is_err());
        let mut z = b;
        z[10] = 0;
        z[11] = 0;
        z[12] &= 0x0f;
        assert!(StreamInfo::parse(&z).is_err());
        let wide = StreamInfo {
            bits: 32,
            ..sample()
        };
        assert_eq!(StreamInfo::parse(&streaminfo_bytes(&wide)).unwrap(), wide);
        let narrow = streaminfo_bytes(&StreamInfo {
            bits: 3,
            ..sample()
        });
        assert!(StreamInfo::parse(&narrow).is_err());
    }

    #[test]
    fn config_reads_every_container_form() {
        let i = sample();
        let si = streaminfo_bytes(&i);
        let mut mkv = b"fLaC".to_vec();
        mkv.extend_from_slice(&[0x04, 0, 0, 2, 0xaa, 0xbb]);
        mkv.extend_from_slice(&[0x80, 0, 0, 34]);
        mkv.extend_from_slice(&si);
        assert_eq!(config(&mkv).unwrap(), i);
        let mut dfla = vec![0, 0, 0, 0, 0x80, 0, 0, 34];
        dfla.extend_from_slice(&si);
        assert_eq!(config(&dfla).unwrap(), i);
        assert_eq!(config(&si).unwrap(), i);
        let mut blocks = vec![0x80, 0, 0, 34];
        blocks.extend_from_slice(&si);
        assert_eq!(config(&blocks).unwrap(), i);
    }

    #[test]
    fn config_refuses_cut_or_missing_streaminfo() {
        let si = streaminfo_bytes(&sample());
        let mut cut = b"fLaC".to_vec();
        cut.extend_from_slice(&[0x80, 0, 0, 34]);
        cut.extend_from_slice(&si[..20]);
        assert!(config(&cut).is_err());
        assert!(config(b"fLaC").is_err());
        assert!(config(&[]).is_err());
        assert!(config(b"fLaC\x84\x00\x00\x02ab").is_err());
        assert!(config(&[0xff; 100]).is_err());
    }

    #[test]
    fn seek_table_drops_placeholders_and_sorts() {
        let mut b = Vec::new();
        for (s, o) in [(8192u64, 900u64), (u64::MAX, 0), (0, 0), (4096, 400)] {
            b.extend_from_slice(&s.to_be_bytes());
            b.extend_from_slice(&o.to_be_bytes());
            b.extend_from_slice(&4096u16.to_be_bytes());
        }
        b.push(0);
        let t = seek_table(&b);
        assert_eq!(
            t.iter().map(|p| (p.sample, p.offset)).collect::<Vec<_>>(),
            [(0, 0), (4096, 400), (8192, 900)]
        );
    }
}
