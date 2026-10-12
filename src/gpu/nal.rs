use crate::media::{Codec, Error, Result};
use crate::syntax::bits::unescape_rbsp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    Length(usize),
    AnnexB,
}

impl Framing {
    pub fn split<'a>(&self, data: &'a [u8]) -> Result<Vec<&'a [u8]>> {
        match *self {
            Framing::AnnexB => Ok(annexb(data)),
            Framing::Length(size) => length_prefixed(data, size),
        }
    }
}

pub fn is_annexb(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])
}

pub fn annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &(_, body)) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |&(code, _)| code);
        while end > body && data[end - 1] == 0 {
            end -= 1;
        }
        if end > body {
            out.push(&data[body..end]);
        }
    }
    out
}

pub fn length_prefixed(data: &[u8], size: usize) -> Result<Vec<&[u8]>> {
    if !(1..=4).contains(&size) {
        return Err(Error::Invalid(format!("NAL length size {size}")));
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let head = data
            .get(i..i + size)
            .ok_or_else(|| Error::Invalid("truncated NAL length".into()))?;
        let len = head.iter().fold(0usize, |a, &b| (a << 8) | usize::from(b));
        i += size;
        let nal = data
            .get(i..i + len)
            .ok_or_else(|| Error::Invalid("NAL unit past the end of the packet".into()))?;
        if !nal.is_empty() {
            out.push(nal);
        }
        i += len;
    }
    Ok(out)
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn length_prefix(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nals.iter().map(|n| n.len() + 4).sum());
    for n in nals {
        out.extend_from_slice(&(n.len() as u32).to_be_bytes());
        out.extend_from_slice(n);
    }
    out
}

pub fn nal_type(codec: Codec, nal: &[u8]) -> u8 {
    match (codec, nal.first()) {
        (Codec::Hevc, Some(&b)) => (b >> 1) & 0x3f,
        (_, Some(&b)) => b & 0x1f,
        _ => 0xff,
    }
}

pub fn is_parameter_set(codec: Codec, nal: &[u8]) -> bool {
    let t = nal_type(codec, nal);
    match codec {
        Codec::Hevc => (32..=34).contains(&t),
        _ => t == 7 || t == 8,
    }
}

pub fn is_keyframe(codec: Codec, nal: &[u8]) -> bool {
    let t = nal_type(codec, nal);
    match codec {
        Codec::Hevc => (16..=21).contains(&t),
        _ => t == 5,
    }
}

#[cfg(test)]
pub fn is_vcl(codec: Codec, nal: &[u8]) -> bool {
    let t = nal_type(codec, nal);
    match codec {
        Codec::Hevc => t < 32,
        _ => (1..=5).contains(&t),
    }
}

pub struct Config {
    pub framing: Framing,
    pub parameter_sets: Vec<Vec<u8>>,
}

pub fn parse_extradata(codec: Codec, data: &[u8]) -> Result<Config> {
    if data.is_empty() || is_annexb(data) {
        return Ok(Config {
            framing: Framing::AnnexB,
            parameter_sets: annexb(data).into_iter().map(<[u8]>::to_vec).collect(),
        });
    }
    match codec {
        Codec::H264 => parse_avcc(data),
        Codec::Hevc => parse_hvcc(data),
        c => Err(Error::Unsupported(format!("{c} has no NAL extradata"))),
    }
}

fn take<'a>(data: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8]> {
    let s = data
        .get(*at..*at + n)
        .ok_or_else(|| Error::Invalid("configuration record truncated".into()))?;
    *at += n;
    Ok(s)
}

fn u16_at(data: &[u8], at: &mut usize) -> Result<usize> {
    let b = take(data, at, 2)?;
    Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

fn parse_avcc(data: &[u8]) -> Result<Config> {
    let mut at = 0;
    let head = take(data, &mut at, 6)?;
    if head[0] != 1 {
        return Err(Error::Invalid("avcC version is not 1".into()));
    }
    let length = usize::from(head[4] & 3) + 1;
    let mut sets = Vec::new();
    for _ in 0..(head[5] & 0x1f) {
        let n = u16_at(data, &mut at)?;
        sets.push(take(data, &mut at, n)?.to_vec());
    }
    let pps = take(data, &mut at, 1)?[0];
    for _ in 0..pps {
        let n = u16_at(data, &mut at)?;
        sets.push(take(data, &mut at, n)?.to_vec());
    }
    Ok(Config {
        framing: Framing::Length(length),
        parameter_sets: sets,
    })
}

fn parse_hvcc(data: &[u8]) -> Result<Config> {
    let mut at = 0;
    let head = take(data, &mut at, 23)?;
    if head[0] != 1 {
        return Err(Error::Invalid("hvcC version is not 1".into()));
    }
    let length = usize::from(head[21] & 3) + 1;
    let mut sets = Vec::new();
    for _ in 0..head[22] {
        let kind = take(data, &mut at, 1)?[0] & 0x3f;
        let count = u16_at(data, &mut at)?;
        for _ in 0..count {
            let n = u16_at(data, &mut at)?;
            let nal = take(data, &mut at, n)?;
            if (32..=34).contains(&kind) {
                sets.push(nal.to_vec());
            }
        }
    }
    Ok(Config {
        framing: Framing::Length(length),
        parameter_sets: sets,
    })
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn build_avcc(sps: &[Vec<u8>], pps: &[Vec<u8>]) -> Result<Vec<u8>> {
    let first = sps
        .first()
        .filter(|s| s.len() >= 4)
        .ok_or_else(|| Error::Invalid("no SPS for avcC".into()))?;
    let mut out = vec![
        1,
        first[1],
        first[2],
        first[3],
        0xff,
        0xe0 | sps.len() as u8,
    ];
    for s in sps {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    }
    out.push(pps.len() as u8);
    for p in pps {
        out.extend_from_slice(&(p.len() as u16).to_be_bytes());
        out.extend_from_slice(p);
    }
    if matches!(first[1], 100 | 110 | 122 | 144) {
        let rbsp = unescape_rbsp(&first[1..]);
        let parsed = crate::syntax::h264::params::Sps::parse(&rbsp)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        out.push(0xfc | (parsed.chroma_format_idc as u8 & 3));
        out.push(0xf8 | (parsed.bit_depth_luma.saturating_sub(8) as u8 & 7));
        out.push(0xf8 | (parsed.bit_depth_chroma.saturating_sub(8) as u8 & 7));
        out.push(0);
    }
    Ok(out)
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
pub fn build_hvcc(vps: &[Vec<u8>], sps: &[Vec<u8>], pps: &[Vec<u8>]) -> Result<Vec<u8>> {
    let first = sps
        .first()
        .ok_or_else(|| Error::Invalid("no SPS for hvcC".into()))?;
    let rbsp = unescape_rbsp(first.get(2..).unwrap_or_default());
    let ptl = rbsp
        .get(1..13)
        .ok_or_else(|| Error::Invalid("SPS too short for hvcC".into()))?;
    let parsed = crate::syntax::hevc::params::Sps::parse(&rbsp)
        .map_err(|e| Error::Invalid(e.to_string()))?;
    let sub_layers = (rbsp[0] >> 1) & 7;
    let nesting = rbsp[0] & 1;
    let mut out = vec![1];
    out.extend_from_slice(&ptl[..12]);
    out.extend_from_slice(&[0xf0, 0x00, 0xfc]);
    out.push(0xfc | (parsed.chroma_format_idc as u8 & 3));
    out.push(0xf8 | (parsed.bit_depth_luma.saturating_sub(8) as u8 & 7));
    out.push(0xf8 | (parsed.bit_depth_chroma.saturating_sub(8) as u8 & 7));
    out.extend_from_slice(&[0, 0]);
    out.push(((sub_layers + 1) << 3) | (nesting << 2) | 3);
    let arrays = [(32u8, vps), (33, sps), (34, pps)];
    out.push(arrays.iter().filter(|(_, l)| !l.is_empty()).count() as u8);
    for (kind, list) in arrays {
        if list.is_empty() {
            continue;
        }
        out.push(0x80 | kind);
        out.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for n in list {
            out.extend_from_slice(&(n.len() as u16).to_be_bytes());
            out.extend_from_slice(n);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_splits_on_three_and_four_byte_start_codes() {
        let data = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 0,
        ];
        let nals = annexb(&data);
        assert_eq!(nals, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4]]);
    }

    #[test]
    fn avcc_round_trips_its_parameter_sets() {
        let sps = vec![
            0x67, 0x42, 0xc0, 0x1f, 0xda, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40,
        ];
        let pps = vec![0x68, 0xce, 0x3c, 0x80];
        let avcc = build_avcc(std::slice::from_ref(&sps), std::slice::from_ref(&pps)).unwrap();
        let parsed = parse_avcc(&avcc).unwrap();
        assert_eq!(parsed.framing, Framing::Length(4));
        assert_eq!(parsed.parameter_sets, vec![sps, pps]);
    }
}
