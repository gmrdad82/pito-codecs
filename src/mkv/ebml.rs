use crate::media::{Error, Result};

fn invalid<T>(msg: &str) -> Result<T> {
    Err(Error::Invalid(msg.into()))
}

#[inline]
pub fn vint_len(b: u8) -> Option<usize> {
    if b == 0 {
        None
    } else {
        Some(b.leading_zeros() as usize + 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Known(u64),
    Unknown,
}

impl Size {
    pub fn known(self) -> Option<u64> {
        match self {
            Size::Known(n) => Some(n),
            Size::Unknown => None,
        }
    }
}

pub fn read_vint(data: &[u8]) -> Option<(u64, usize, bool)> {
    let first = *data.first()?;
    let len = vint_len(first)?;
    if data.len() < len {
        return None;
    }
    let mask = if len == 8 { 0 } else { 0xFFu8 >> len };
    let mut v = (first & mask) as u64;
    for &b in &data[1..len] {
        v = (v << 8) | b as u64;
    }
    let all_ones = v == (1u64 << (7 * len)) - 1;
    Some((v, len, all_ones))
}

pub fn read_size(data: &[u8]) -> Option<(Size, usize)> {
    let (v, len, ones) = read_vint(data)?;
    Some((if ones { Size::Unknown } else { Size::Known(v) }, len))
}

pub fn read_id(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    let len = vint_len(first)?;
    if len > 4 || data.len() < len {
        return None;
    }
    let (v, _, ones) = read_vint(&data[..len])?;
    if ones || (v == 0 && len > 1) {
        return None;
    }
    let mut id = 0u32;
    for &b in &data[..len] {
        id = (id << 8) | b as u32;
    }
    Some((id, len))
}

pub fn read_svint(data: &[u8]) -> Option<(i64, usize)> {
    let (v, len, _) = read_vint(data)?;
    let bias = (1i64 << (7 * len - 1)) - 1;
    Some((v as i64 - bias, len))
}

pub fn write_id(out: &mut Vec<u8>, id: u32) {
    let n = if id >= 0x100_0000 {
        4
    } else if id >= 0x1_0000 {
        3
    } else if id >= 0x100 {
        2
    } else {
        1
    };
    out.extend_from_slice(&id.to_be_bytes()[4 - n..]);
}

pub fn write_size(out: &mut Vec<u8>, size: u64, min_len: usize) {
    let mut len = 1;
    while len < 8 && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let len = len.max(min_len).min(8);
    let v = size | (1u64 << (7 * len));
    out.extend_from_slice(&v.to_be_bytes()[8 - len..]);
}

pub fn write_unknown_size(out: &mut Vec<u8>, len: usize) {
    out.push(0xFFu8 >> (len - 1));
    out.extend(std::iter::repeat_n(0xFF, len - 1));
}

pub fn uint(data: &[u8]) -> u64 {
    data.iter().take(8).fold(0u64, |a, &b| (a << 8) | b as u64)
}

pub fn float(data: &[u8]) -> f64 {
    match data.len() {
        4 => f32::from_be_bytes(data.try_into().unwrap_or([0; 4])) as f64,
        8 => f64::from_be_bytes(data.try_into().unwrap_or([0; 8])),
        _ => 0.0,
    }
}

pub fn string(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).into_owned()
}

#[derive(Clone, Copy, Debug)]
pub struct Elem<'a> {
    pub id: u32,
    pub data: &'a [u8],
}

pub struct Children<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Children<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
}

impl<'a> Iterator for Children<'a> {
    type Item = Elem<'a>;
    fn next(&mut self) -> Option<Elem<'a>> {
        let rest = self.data.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let Some((id, il)) = read_id(rest) else {
            self.pos = self.data.len();
            return None;
        };
        let Some((size, sl)) = read_size(&rest[il..]) else {
            self.pos = self.data.len();
            return None;
        };
        let hl = il + sl;
        let avail = (rest.len() - hl) as u64;
        let len = match size {
            Size::Known(n) if n <= avail => n as usize,
            _ => avail as usize,
        };
        let e = Elem {
            id,
            data: &rest[hl..hl + len],
        };
        self.pos += hl + len;
        Some(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lacing {
    None,
    Xiph,
    FixedSize,
    Ebml,
}

impl Lacing {
    pub fn from_flags(flags: u8) -> Lacing {
        match (flags >> 1) & 3 {
            0 => Lacing::None,
            1 => Lacing::Xiph,
            2 => Lacing::FixedSize,
            _ => Lacing::Ebml,
        }
    }
}

pub fn parse_lacing(lacing: Lacing, data: &[u8]) -> Result<Vec<(usize, usize)>> {
    if lacing == Lacing::None {
        return Ok(vec![(0, data.len())]);
    }
    let Some(&n1) = data.first() else {
        return invalid("laced block without frame count");
    };
    let count = n1 as usize + 1;
    let mut pos = 1usize;
    let mut sizes = Vec::with_capacity(count);
    match lacing {
        Lacing::None => return Ok(vec![(0, data.len())]),
        Lacing::Xiph => {
            for _ in 0..count - 1 {
                let mut s = 0usize;
                loop {
                    let Some(&b) = data.get(pos) else {
                        return invalid("truncated Xiph lace header");
                    };
                    pos += 1;
                    s += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        Lacing::Ebml => {
            if count > 1 {
                let Some((first, l, _)) = read_vint(&data[pos..]) else {
                    return invalid("bad EBML lace size");
                };
                pos += l;
                let mut prev = first as i64;
                sizes.push(first as usize);
                for _ in 1..count - 1 {
                    let Some((d, l)) = read_svint(&data[pos..]) else {
                        return invalid("bad EBML lace delta");
                    };
                    pos += l;
                    prev += d;
                    if prev < 0 {
                        return invalid("negative EBML lace size");
                    }
                    sizes.push(prev as usize);
                }
            }
        }
        Lacing::FixedSize => {
            let rest = data.len() - pos;
            if rest < count {
                return invalid("fixed-size lacing with empty frames");
            }
            if !rest.is_multiple_of(count) {
                return invalid("fixed-size lacing does not divide block");
            }
            sizes.extend(std::iter::repeat_n(rest / count, count - 1));
        }
    }
    let used: usize = sizes
        .iter()
        .try_fold(0usize, |a, &s| a.checked_add(s))
        .unwrap_or(usize::MAX);
    if pos > data.len() || used > data.len() - pos {
        return invalid("lace sizes exceed block");
    }
    sizes.push(data.len() - pos - used);
    let mut out = Vec::with_capacity(count);
    for s in sizes {
        out.push((pos, s));
        pos += s;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vint_roundtrip() {
        for &v in &[
            0u64,
            1,
            126,
            127,
            128,
            16382,
            16383,
            16384,
            1 << 20,
            (1 << 56) - 2,
        ] {
            let mut b = Vec::new();
            write_size(&mut b, v, 1);
            let (s, l) = read_size(&b).unwrap();
            assert_eq!(s, Size::Known(v), "value {v}");
            assert_eq!(l, b.len());
        }
        let mut b = Vec::new();
        write_size(&mut b, 127, 1);
        assert_eq!(b, [0x40, 0x7F]);
        let mut b = Vec::new();
        write_size(&mut b, 5, 8);
        assert_eq!(b, [1, 0, 0, 0, 0, 0, 0, 5]);
        assert_eq!(read_size(&b).unwrap(), (Size::Known(5), 8));
    }

    #[test]
    fn vint_unknown_and_invalid() {
        assert_eq!(read_size(&[0xFF]).unwrap(), (Size::Unknown, 1));
        assert_eq!(
            read_size(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]).unwrap(),
            (Size::Unknown, 8)
        );
        for l in 1..=8 {
            let mut b = Vec::new();
            write_unknown_size(&mut b, l);
            assert_eq!(read_size(&b).unwrap(), (Size::Unknown, l));
        }
        assert!(read_size(&[0x00, 1, 2]).is_none());
        assert!(read_size(&[0x40]).is_none());
        assert_eq!(read_vint(&[0x81]).unwrap(), (1, 1, false));
        assert_eq!(read_vint(&[0x10, 0x00, 0x00, 0x01]).unwrap(), (1, 4, false));
    }

    #[test]
    fn signed_vint() {
        assert_eq!(read_svint(&[0xBF]).unwrap(), (0, 1));
        assert_eq!(read_svint(&[0x80]).unwrap(), (-63, 1));
        assert_eq!(read_svint(&[0xC0]).unwrap(), (1, 1));
        assert_eq!(read_svint(&[0x5F, 0xFF]).unwrap(), (0, 2));
        assert_eq!(read_svint(&[0x60, 0x00]).unwrap(), (1, 2));
    }

    fn frames<'a>(d: &'a [u8], f: &[(usize, usize)]) -> Vec<&'a [u8]> {
        f.iter().map(|&(o, l)| &d[o..o + l]).collect()
    }

    #[test]
    fn lacing_xiph() {
        let mut d = vec![2u8, 255, 45, 2];
        d.extend(std::iter::repeat_n(7u8, 300));
        d.extend_from_slice(&[1, 2]);
        d.extend_from_slice(&[3, 4, 5]);
        let f = parse_lacing(Lacing::Xiph, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [300, 2, 3]);
        assert_eq!(frames(&d, &f)[2], &[3, 4, 5]);
        let mut d = vec![1u8, 255, 0];
        d.extend(std::iter::repeat_n(1u8, 255));
        d.push(9);
        let f = parse_lacing(Lacing::Xiph, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [255, 1]);
        assert!(parse_lacing(Lacing::Xiph, &[1, 200, 1]).is_err());
    }

    #[test]
    fn lacing_ebml() {
        let mut d = vec![3u8, 0x85, 0xBF - 2, 0xBF + 1];
        d.extend_from_slice(&[1; 5]);
        d.extend_from_slice(&[2; 3]);
        d.extend_from_slice(&[3; 4]);
        d.extend_from_slice(&[4; 2]);
        let f = parse_lacing(Lacing::Ebml, &d).unwrap();
        assert_eq!(f.iter().map(|x| x.1).collect::<Vec<_>>(), [5, 3, 4, 2]);
        assert_eq!(frames(&d, &f), [&[1u8; 5][..], &[2; 3], &[3; 4], &[4; 2]]);
        assert!(parse_lacing(Lacing::Ebml, &[1, 0x90]).is_err());
    }

    #[test]
    fn lacing_fixed_and_none() {
        let d = [2u8, 1, 1, 2, 2, 3, 3];
        let f = parse_lacing(Lacing::FixedSize, &d).unwrap();
        assert_eq!(frames(&d, &f), [&[1u8, 1][..], &[2, 2], &[3, 3]]);
        assert!(parse_lacing(Lacing::FixedSize, &[2, 1, 1]).is_err());
        assert!(parse_lacing(Lacing::FixedSize, &[254, 7]).is_err());
        assert!(parse_lacing(Lacing::FixedSize, &[3]).is_err());
        assert_eq!(parse_lacing(Lacing::None, &[9, 9]).unwrap(), [(0, 2)]);
        assert_eq!(Lacing::from_flags(0x80 | 0x06), Lacing::Ebml);
        assert_eq!(Lacing::from_flags(0x02), Lacing::Xiph);
        assert_eq!(Lacing::from_flags(0x04), Lacing::FixedSize);
    }
}
