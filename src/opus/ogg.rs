use std::io::Write;
use std::ops::Range;
use std::sync::OnceLock;

use crate::media::{AudioBuffer, Error, Result};
use crate::opus::header::OpusHead;
use crate::opus::multistream::Decoder;
use crate::opus::packet::packet_samples_48k;

const MAX_PAGE_BODY: usize = 4096;
const MAX_PAGE_PACKETS: usize = 50;

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

pub fn crc32(data: &[u8]) -> u32 {
    let t = crc_table();
    data.iter()
        .fold(0u32, |c, &b| (c << 8) ^ t[((c >> 24) as u8 ^ b) as usize])
}

pub fn sniff(head: &[u8]) -> bool {
    head.len() >= 27 && &head[..4] == b"OggS" && head[4] == 0
}

pub struct OggOpus {
    pub head: OpusHead,
    pub tags: Vec<u8>,
    pub serial: u32,
    data: Vec<u8>,
    pub packets: Vec<Range<usize>>,
    pub granules: Vec<Option<i64>>,
    pub warnings: Vec<String>,
}

impl OggOpus {
    pub fn parse(src: &[u8]) -> Result<OggOpus> {
        let mut pos = 0usize;
        let mut serial = None;
        let mut headers: Vec<Vec<u8>> = Vec::new();
        let mut data = Vec::new();
        let mut packets = Vec::new();
        let mut granules = Vec::new();
        let mut warnings = Vec::new();
        let mut partial: Option<usize> = None;
        let mut hdr_buf = Vec::new();
        let mut found = false;
        while pos + 27 <= src.len() {
            if !sniff(&src[pos..]) {
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
            if lacing_end > src.len() {
                warnings.push(format!("truncated page at byte {pos}"));
                break;
            }
            let lacing = &src[pos + 27..lacing_end];
            let body_len: usize = lacing.iter().map(|&b| b as usize).sum();
            let page_end = lacing_end + body_len;
            if page_end > src.len() {
                warnings.push(format!("truncated page at byte {pos}"));
                break;
            }
            let page = &src[pos..page_end];
            let stored = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
            let t = crc_table();
            let mut crc = 0u32;
            for (i, &b) in page.iter().enumerate() {
                let b = if (22..26).contains(&i) { 0 } else { b };
                crc = (crc << 8) ^ t[((crc >> 24) as u8 ^ b) as usize];
            }
            if crc != stored {
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
            pos = page_end;
            match serial {
                None if flags & 0x02 != 0 && page[lacing_end..].starts_with(b"OpusHead") => {
                    serial = Some(page_serial)
                }
                Some(s) if s == page_serial => {}
                _ => continue,
            }
            if flags & 0x01 == 0 && partial.is_some() {
                warnings.push(format!("incomplete packet before page at byte {pos}"));
                partial = None;
                hdr_buf.clear();
            }
            let mut skip = flags & 0x01 != 0 && partial.is_none() && headers.len() == 2;
            let mut off = lacing_end;
            let mut completed_any = false;
            for &l in lacing {
                let seg = &src[off..off + l as usize];
                off += l as usize;
                if skip {
                    if l < 255 {
                        skip = false;
                    }
                    continue;
                }
                if headers.len() < 2 {
                    hdr_buf.extend_from_slice(seg);
                    partial = Some(0);
                    if l < 255 {
                        headers.push(std::mem::take(&mut hdr_buf));
                        partial = None;
                    }
                    continue;
                }
                let start = *partial.get_or_insert(data.len());
                data.extend_from_slice(seg);
                if l < 255 {
                    packets.push(start..data.len());
                    granules.push(None);
                    partial = None;
                    completed_any = true;
                }
            }
            if completed_any
                && granule != -1
                && let Some(g) = granules.last_mut()
            {
                *g = Some(granule);
            }
        }
        if !found {
            return Err(Error::Invalid("not an Ogg file".into()));
        }
        let mut headers = headers.into_iter();
        let head_bytes = headers
            .next()
            .ok_or_else(|| Error::Invalid("no Opus stream in the Ogg file".into()))?;
        let head = OpusHead::parse(&head_bytes)?;
        let tags = headers.next().unwrap_or_default();
        Ok(OggOpus {
            head,
            tags,
            serial: serial.unwrap_or(0),
            data,
            packets,
            granules,
            warnings,
        })
    }

    pub fn packet(&self, i: usize) -> &[u8] {
        &self.data[self.packets[i].clone()]
    }

    pub fn end_granule(&self) -> Option<i64> {
        self.granules.iter().rev().find_map(|g| *g)
    }

    pub fn samples(&self) -> u64 {
        let coded: u64 = (0..self.packets.len())
            .map(|i| packet_samples_48k(self.packet(i)).unwrap_or(0) as u64)
            .sum();
        let end = self.end_granule().map_or(coded, |g| g.max(0) as u64);
        end.min(coded).saturating_sub(self.head.pre_skip as u64)
    }

    pub fn decode(&self) -> Result<AudioBuffer> {
        let mut dec = Decoder::from_head(self.head.clone(), 48000)?;
        let channels = dec.channels();
        let total = self.samples() as usize;
        let skip = self.head.pre_skip as usize;
        let mut out = AudioBuffer::new(48000, channels as u16);
        out.samples.reserve((total + skip + 5760) * channels);
        for i in 0..self.packets.len() {
            dec.decode_into(Some(self.packet(i)), &mut out.samples)?;
        }
        let have = out.samples.len() / channels.max(1);
        out.samples.drain(..skip.min(have) * channels);
        out.samples.truncate(total * channels);
        Ok(out)
    }
}

pub fn decode(src: &[u8]) -> Result<AudioBuffer> {
    OggOpus::parse(src)?.decode()
}

pub struct Writer<W: Write> {
    w: W,
    serial: u32,
    sequence: u32,
    body: Vec<u8>,
    lacing: Vec<u8>,
    packets: usize,
    granule: i64,
    page: Vec<u8>,
}

impl<W: Write> Writer<W> {
    pub fn new(w: W, serial: u32, head: &OpusHead, vendor: &str) -> Result<Self> {
        let mut s = Writer {
            w,
            serial,
            sequence: 0,
            body: Vec::new(),
            lacing: Vec::new(),
            packets: 0,
            granule: 0,
            page: Vec::new(),
        };
        s.add(&head.to_bytes());
        s.flush_page(0x02, 0)?;
        let mut tags = b"OpusTags".to_vec();
        tags.extend((vendor.len() as u32).to_le_bytes());
        tags.extend(vendor.as_bytes());
        tags.extend(0u32.to_le_bytes());
        s.add(&tags);
        s.flush_page(0, 0)?;
        Ok(s)
    }

    fn add(&mut self, packet: &[u8]) {
        let mut n = packet.len();
        while n >= 255 {
            self.lacing.push(255);
            n -= 255;
        }
        self.lacing.push(n as u8);
        self.body.extend_from_slice(packet);
        self.packets += 1;
    }

    pub fn push(&mut self, packet: &[u8], granule: i64) -> Result<()> {
        if self.lacing.len() + packet.len() / 255 + 1 > 255 {
            self.flush_page(0, self.granule)?;
        }
        self.add(packet);
        self.granule = granule;
        if self.body.len() >= MAX_PAGE_BODY || self.packets >= MAX_PAGE_PACKETS {
            self.flush_page(0, granule)?;
        }
        Ok(())
    }

    fn flush_page(&mut self, flags: u8, granule: i64) -> Result<()> {
        let mut segs = &self.lacing[..];
        let mut body = &self.body[..];
        let mut continued = false;
        loop {
            let n = segs.len().min(255);
            let (now, rest) = segs.split_at(n);
            let len: usize = now.iter().map(|&b| b as usize).sum();
            let last = rest.is_empty();
            let page_granule = if last && now.last().is_none_or(|&l| l < 255) {
                granule
            } else {
                -1
            };
            let page = &mut self.page;
            page.clear();
            page.extend_from_slice(b"OggS\0");
            page.push(flags | if continued { 0x01 } else { 0 });
            page.extend(page_granule.to_le_bytes());
            page.extend(self.serial.to_le_bytes());
            page.extend(self.sequence.to_le_bytes());
            page.extend([0u8; 4]);
            page.push(n as u8);
            page.extend_from_slice(now);
            page.extend_from_slice(&body[..len]);
            let crc = crc32(page);
            page[22..26].copy_from_slice(&crc.to_le_bytes());
            self.w.write_all(page)?;
            self.sequence += 1;
            body = &body[len..];
            segs = rest;
            continued = now.last().is_some_and(|&l| l == 255);
            if last {
                break;
            }
        }
        self.lacing.clear();
        self.body.clear();
        self.packets = 0;
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        self.flush_page(0x04, self.granule)?;
        self.w.flush()?;
        Ok(self.w)
    }
}
