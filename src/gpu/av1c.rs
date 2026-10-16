use crate::media::{Error, Result};
use crate::syntax::bits::{BitError, BitReader};

const SEQUENCE_HEADER: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Av1Stream {
    pub profile: u8,
    pub bit_depth: u8,
    pub monochrome: bool,
    pub subsampling: (bool, bool),
    pub size: (u32, u32),
    pub sequence_header: Vec<u8>,
}

impl Av1Stream {
    pub fn is_420(&self) -> bool {
        !self.monochrome && self.subsampling == (true, true)
    }
}

fn invalid(what: &str) -> Error {
    Error::Invalid(format!("av1C: {what}"))
}

fn leb128(data: &[u8], at: &mut usize) -> Option<usize> {
    let mut v = 0u64;
    for i in 0..8 {
        let b = *data.get(*at)?;
        *at += 1;
        v |= u64::from(b & 0x7f) << (i * 7);
        if b & 0x80 == 0 {
            return usize::try_from(v).ok();
        }
    }
    None
}

fn put_leb128(mut v: usize, out: &mut Vec<u8>) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub(crate) fn obus(data: &[u8]) -> impl Iterator<Item = (u8, &[u8], &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let start = at;
        let head = *data.get(at)?;
        at += 1;
        if head & 0x04 != 0 {
            at += 1;
        }
        let len = if head & 0x02 != 0 {
            leb128(data, &mut at)?
        } else {
            data.len().checked_sub(at)?
        };
        let end = at.checked_add(len)?;
        let payload = data.get(at..end)?;
        let header = data.get(start..at)?;
        at = end;
        Some(((head >> 3) & 0x0f, header, payload))
    })
}

pub(crate) fn has_sequence_header(sample: &[u8]) -> bool {
    obus(sample).any(|(t, _, _)| t == SEQUENCE_HEADER)
}

fn max_frame_size(payload: &[u8]) -> std::result::Result<(u32, u32), BitError> {
    let mut r = BitReader::new(payload);
    r.skip(4)?;
    let reduced = r.read_flag()?;
    if reduced {
        r.skip(5)?;
    } else {
        let mut buffer_delay_bits = 0;
        let mut decoder_model = false;
        if r.read_flag()? {
            r.skip(64)?;
            if r.read_flag()? {
                let mut zeros = 0;
                while !r.read_flag()? {
                    zeros += 1;
                    if zeros >= 32 {
                        return Err(BitError::InvalidExpGolomb);
                    }
                }
                r.skip(zeros)?;
            }
            decoder_model = r.read_flag()?;
            if decoder_model {
                buffer_delay_bits = r.read_bits(5)? as usize + 1;
                r.skip(32 + 5 + 5)?;
            }
        }
        let display_delay = r.read_flag()?;
        let points = r.read_bits(5)? + 1;
        for _ in 0..points {
            r.skip(12)?;
            if r.read_bits(5)? > 7 {
                r.skip(1)?;
            }
            if decoder_model && r.read_flag()? {
                r.skip(2 * buffer_delay_bits + 1)?;
            }
            if display_delay && r.read_flag()? {
                r.skip(4)?;
            }
        }
    }
    let width_bits = r.read_bits(4)? + 1;
    let height_bits = r.read_bits(4)? + 1;
    let width = r.read_bits(width_bits)? + 1;
    let height = r.read_bits(height_bits)? + 1;
    Ok((width, height))
}

pub(crate) fn parse(av1c: &[u8]) -> Result<Av1Stream> {
    let [head, a, b, _, configs @ ..] = av1c else {
        return Err(invalid("truncated"));
    };
    if *head != 0x81 {
        return Err(Error::Unsupported(format!(
            "av1C marker and version {head:#04x}"
        )));
    }
    let bit_depth = match (b & 0x40 != 0, b & 0x20 != 0) {
        (true, true) => 12,
        (true, false) => 10,
        _ => 8,
    };
    let (_, header, payload) = obus(configs)
        .find(|(t, _, _)| *t == SEQUENCE_HEADER)
        .ok_or_else(|| invalid("no sequence header OBU in configOBUs"))?;
    let size = max_frame_size(payload).map_err(|e| invalid(&e.to_string()))?;
    let mut sequence_header = Vec::with_capacity(header.len() + payload.len() + 4);
    let first = header.first().copied().unwrap_or(0x0a);
    sequence_header.push(first | 0x02);
    if let Some(&ext) = header.get(1).filter(|_| first & 0x04 != 0) {
        sequence_header.push(ext);
    }
    put_leb128(payload.len(), &mut sequence_header);
    sequence_header.extend_from_slice(payload);
    Ok(Av1Stream {
        profile: a >> 5,
        bit_depth,
        monochrome: b & 0x10 != 0,
        subsampling: (b & 0x08 != 0, b & 0x04 != 0),
        size,
        sequence_header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_svt_av1c_gives_its_size_depth_and_a_sized_sequence_header() {
        let ten = [
            0x81, 0x01, 0x4c, 0x00, 0x0a, 0x0c, 0x02, 0x00, 0x00, 0x0d, 0x26, 0x27, 0xfb, 0x38,
            0x05, 0xf0, 0x80, 0x80,
        ];
        let s = parse(&ten).unwrap();
        assert_eq!(
            (s.profile, s.bit_depth, s.is_420(), s.size),
            (0, 10, true, (640, 360))
        );
        assert_eq!(s.sequence_header, ten[4..]);
        assert!(has_sequence_header(&s.sequence_header));
        assert!(parse(&ten[..10]).is_err());
        assert!(parse(&[]).is_err());
    }
}
