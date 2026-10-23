use std::ops::Range;
use std::sync::OnceLock;

use super::audio::Decoder;
use crate::media::{AudioBuffer, Error, Result};

fn crc_table() -> &'static [u32; 256] {
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 {
                    (r << 1) ^ 0x04C1_1DB7
                } else {
                    r << 1
                };
            }
            *e = r;
        }
        t
    })
}

pub(crate) fn page_crc(page: &[u8]) -> u32 {
    let t = crc_table();
    page.iter().enumerate().fold(0u32, |c, (i, &b)| {
        let b = if (22..26).contains(&i) { 0 } else { b };
        (c << 8) ^ t[((c >> 24) as u8 ^ b) as usize]
    })
}

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 27
        && head.starts_with(b"OggS")
        && head[4] == 0
        && head
            .get(27 + usize::from(head[26])..)
            .is_some_and(|body| body.starts_with(b"\x01vorbis"))
}

pub struct OggVorbis {
    pub headers: [Vec<u8>; 3],
    pub serial: u32,
    data: Vec<u8>,
    pub packets: Vec<Range<usize>>,
    pub granules: Vec<Option<i64>>,
    pub warnings: Vec<String>,
}

impl OggVorbis {
    pub fn parse(src: &[u8]) -> Result<OggVorbis> {
        let mut pos = 0usize;
        let mut serial = None;
        let mut data = Vec::new();
        let mut packets: Vec<Range<usize>> = Vec::new();
        let mut granules = Vec::new();
        let mut warnings = Vec::new();
        let mut partial: Option<usize> = None;
        let mut found = false;
        let mut last_seq: Option<u32> = None;
        let mut ours_pages = 0usize;
        while pos + 27 <= src.len() {
            if !src[pos..].starts_with(b"OggS") || src[pos + 4] != 0 {
                match src[pos + 1..].windows(4).position(|w| w == b"OggS") {
                    Some(i) => {
                        if found {
                            warnings.push(format!("lost sync at byte {pos}"));
                        }
                        pos += 1 + i;
                        continue;
                    }
                    None => break,
                }
            }
            let nseg = src[pos + 26] as usize;
            let lacing_end = pos + 27 + nseg;
            let Some(lacing) = src.get(pos + 27..lacing_end) else {
                warnings.push(format!("truncated page at byte {pos}"));
                break;
            };
            let page_end = lacing_end + lacing.iter().map(|&b| b as usize).sum::<usize>();
            let Some(page) = src.get(pos..page_end) else {
                warnings.push(format!("truncated page at byte {pos}"));
                break;
            };
            if page_crc(page) != u32::from_le_bytes([page[22], page[23], page[24], page[25]]) {
                warnings.push(format!("bad CRC on page at byte {pos}"));
                pos += 1;
                continue;
            }
            found = true;
            let flags = page[5];
            let granule = i64::from_le_bytes([
                page[6], page[7], page[8], page[9], page[10], page[11], page[12], page[13],
            ]);
            let page_serial = u32::from_le_bytes([page[14], page[15], page[16], page[17]]);
            let seq = u32::from_le_bytes([page[18], page[19], page[20], page[21]]);
            let at = pos;
            pos = page_end;
            if flags & 0x02 != 0 && ours_pages >= 2 {
                return Err(Error::Unsupported(
                    "chained Ogg Vorbis files are not supported".into(),
                ));
            }
            match serial {
                None if flags & 0x02 != 0
                    && src[lacing_end..page_end].starts_with(b"\x01vorbis") =>
                {
                    serial = Some(page_serial)
                }
                Some(s) if s == page_serial => {}
                _ => continue,
            }
            ours_pages += 1;
            if last_seq.is_some_and(|l| l.wrapping_add(1) != seq) {
                warnings.push(format!("missing page before byte {at}"));
                if let Some(start) = partial.take() {
                    data.truncate(start);
                }
            }
            last_seq = Some(seq);
            let continued = flags & 0x01 != 0;
            if !continued && partial.is_some() {
                warnings.push(format!("incomplete packet before page at byte {at}"));
                if let Some(start) = partial.take() {
                    data.truncate(start);
                }
            }
            let mut skip = continued && partial.is_none();
            let mut off = lacing_end;
            let mut completed = false;
            for &l in lacing {
                let seg = &src[off..off + l as usize];
                off += l as usize;
                if skip {
                    skip = l == 255;
                    continue;
                }
                let start = *partial.get_or_insert(data.len());
                data.extend_from_slice(seg);
                if l < 255 {
                    packets.push(start..data.len());
                    granules.push(None);
                    partial = None;
                    completed = true;
                }
            }
            if completed
                && granule != -1
                && let Some(g) = granules.last_mut()
            {
                *g = Some(granule);
            }
        }
        if !found {
            return Err(Error::Invalid("not an Ogg file".into()));
        }
        if serial.is_none() {
            return Err(Error::Invalid("no Vorbis stream in the Ogg file".into()));
        }
        if packets.len() < 3 {
            return Err(Error::Invalid(
                "Ogg Vorbis stream ends before its three headers".into(),
            ));
        }
        let header = |i: usize| src_packet(&data, &packets[i]).to_vec();
        let headers = [header(0), header(1), header(2)];
        packets.drain(..3);
        granules.drain(..3);
        Ok(OggVorbis {
            headers,
            serial: serial.unwrap_or(0),
            data,
            packets,
            granules,
            warnings,
        })
    }

    pub fn packet(&self, i: usize) -> &[u8] {
        src_packet(&self.data, &self.packets[i])
    }

    pub fn decoder(&self) -> Result<Decoder> {
        Decoder::from_headers(&self.headers[0], &self.headers[1], &self.headers[2])
    }

    fn counts(&self, dec: &Decoder) -> Vec<u64> {
        let mut prev: Option<usize> = None;
        (0..self.packets.len())
            .map(|i| match dec.blocksize(self.packet(i)) {
                Ok(n) => {
                    let c = prev.map_or(0, |p| (p / 4 + n / 4) as u64);
                    prev = Some(n);
                    c
                }
                Err(_) => 0,
            })
            .collect()
    }

    fn span(&self, counts: &[u64]) -> (u64, Option<u64>) {
        span(counts, &self.granules)
    }

    pub fn samples(&self) -> Result<u64> {
        Ok(self.samples_with(&self.decoder()?))
    }

    pub fn samples_with(&self, dec: &Decoder) -> u64 {
        let counts = self.counts(dec);
        let (lead, end) = self.span(&counts);
        end.unwrap_or_else(|| counts.iter().sum()) - lead
    }

    pub fn decode(&self) -> Result<AudioBuffer> {
        let mut dec = self.decoder()?;
        let ch = (dec.channels() as usize).max(1);
        let counts = self.counts(&dec);
        let (lead, end) = self.span(&counts);
        let total: u64 = counts.iter().sum();
        let mut out = AudioBuffer::new(dec.rate(), dec.channels());
        let long = 1u64 << dec.ident.blocksize_1;
        let frames = end.map_or(total, |e| e.min(total)).saturating_add(long);
        reserve(&mut out.samples, frames, ch)?;
        for i in 0..self.packets.len() {
            if end.is_some_and(|e| (out.samples.len() / ch) as u64 >= e) {
                break;
            }
            dec.decode_into(self.packet(i), &mut out.samples)?;
        }
        let have = (out.samples.len() / ch.max(1)) as u64;
        if let Some(e) = end {
            out.samples.truncate((e.min(have) as usize) * ch);
        }
        out.samples
            .drain(..(lead.min(have) as usize * ch).min(out.samples.len()));
        Ok(out)
    }
}

pub(crate) fn reserve(out: &mut Vec<f32>, frames: u64, ch: usize) -> Result<()> {
    usize::try_from(frames)
        .ok()
        .and_then(|f| f.checked_mul(ch))
        .and_then(|n| out.try_reserve(n).ok())
        .ok_or_else(|| Error::Invalid("Ogg Vorbis stream too long to decode in memory".into()))
}

pub(crate) fn span(counts: &[u64], granules: &[Option<i64>]) -> (u64, Option<u64>) {
    let total: u64 = counts.iter().sum();
    let Some(first) = granules.iter().position(|g| g.is_some()) else {
        return (0, None);
    };
    let upto: u64 = counts.iter().take(first + 1).sum();
    let start = if first + 1 == granules.len() {
        0
    } else {
        granules[first].unwrap_or(0) as i128 - upto as i128
    };
    let lead = (-start).clamp(0, total as i128) as u64;
    let end = match granules.last() {
        Some(Some(g)) => Some((*g as i128 - start).clamp(lead as i128, total as i128) as u64),
        _ => None,
    };
    (lead, end)
}

fn src_packet<'a>(data: &'a [u8], r: &Range<usize>) -> &'a [u8] {
    data.get(r.clone()).unwrap_or_default()
}

pub fn decode(src: &[u8]) -> Result<AudioBuffer> {
    OggVorbis::parse(src)?.decode()
}

pub fn xiph_headers(private: &[u8]) -> Result<[&[u8]; 3]> {
    let bad = || Error::Invalid("Vorbis track without its three headers in CodecPrivate".into());
    let (&count, mut rest) = private.split_first().ok_or_else(bad)?;
    if count != 2 {
        return Err(bad());
    }
    let mut sizes = [0usize; 2];
    for s in &mut sizes {
        loop {
            let (&b, r) = rest.split_first().ok_or_else(bad)?;
            rest = r;
            *s += b as usize;
            if b < 255 {
                break;
            }
        }
    }
    if sizes[0] + sizes[1] > rest.len() {
        return Err(bad());
    }
    let (ident, rest) = rest.split_at(sizes[0]);
    let (comment, setup) = rest.split_at(sizes[1]);
    Ok([ident, comment, setup])
}
