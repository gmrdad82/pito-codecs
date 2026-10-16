use super::bits::Bits;
use super::hdr::{HDR_SIZE, Head};
use super::layer3::{GrInfo, read_side_info};
use super::{Decoder, MAX_SAMPLES_PER_FRAME, find_frame};
use crate::media::{AudioBuffer, Error, Result};

const FRAMES_FLAG: u8 = 1;
const BYTES_FLAG: u8 = 2;
const TOC_FLAG: u8 = 4;
const VBR_SCALE_FLAG: u8 = 8;
const DECODER_DELAY: i64 = 528 + 1;
const ID3_DETECT_SIZE: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VbrTag {
    pub frames: u32,
    pub delay: usize,
    pub padding: usize,
}

pub fn vbr_tag(frame: &[u8]) -> Option<VbrTag> {
    let h = Head::at(frame, 0)?;
    if frame.len() < HDR_SIZE + 8 || h.layer() != 3 {
        return None;
    }
    let mut bs = Bits::new(&frame[HDR_SIZE..]);
    if h.crc() {
        bs.get(16);
    }
    let mut gr = [GrInfo::default(); 4];
    if read_side_info(&mut bs, &mut gr, h) < 0 {
        return None;
    }
    let mut at = HDR_SIZE + bs.pos / 8;
    let left = |at: usize| frame.len().saturating_sub(at);
    if at > frame.len() || left(at) < 8 {
        return None;
    }
    let tag = &frame[at..];
    if &tag[..4] != b"Xing" && &tag[..4] != b"Info" {
        return None;
    }
    let flags = tag[7];
    if flags & FRAMES_FLAG == 0 {
        return Some(VbrTag::default());
    }
    at += 8;
    if left(at) < 4 {
        return None;
    }
    let frames = u32::from_be_bytes([frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]);
    at += 4;
    for (flag, size) in [(BYTES_FLAG, 4), (TOC_FLAG, 100), (VBR_SCALE_FLAG, 4)] {
        if flags & flag != 0 {
            if left(at) < size {
                return None;
            }
            at += size;
        }
    }
    if left(at) < 1 {
        return None;
    }
    let (mut delay, mut padding) = (0i64, 0i64);
    if frame[at] != 0 {
        if left(at) <= 35 {
            return None;
        }
        at += 21;
        let t = &frame[at..at + 3];
        delay = ((i64::from(t[0]) << 4) | (i64::from(t[1]) >> 4)) + DECODER_DELAY;
        padding = (((i64::from(t[1]) & 0xF) << 8) | i64::from(t[2])) - DECODER_DELAY;
    }
    Some(VbrTag {
        frames,
        delay: delay.max(0) as usize,
        padding: padding.max(0) as usize,
    })
}

fn id3v2_size(b: &[u8]) -> usize {
    if b.len() >= ID3_DETECT_SIZE
        && &b[..3] == b"ID3"
        && b[5] & 15 == 0
        && b[6..10].iter().all(|&x| x & 0x80 == 0)
    {
        let size = (usize::from(b[6] & 0x7f) << 21)
            | (usize::from(b[7] & 0x7f) << 14)
            | (usize::from(b[8] & 0x7f) << 7)
            | usize::from(b[9] & 0x7f);
        return size + 10 + if b[5] & 16 != 0 { 10 } else { 0 };
    }
    0
}

fn without_trailing_tags(b: &[u8]) -> &[u8] {
    let mut n = b.len();
    if n >= 128 && &b[n - 128..n - 125] == b"TAG" {
        n -= 128;
        if n >= 227 && &b[n - 227..n - 223] == b"TAG+" {
            n -= 227;
        }
    }
    if n > 32 && &b[n - 32..n - 24] == b"APETAGEX" {
        n -= 32;
        let t = &b[n + 12..n + 16];
        let size = u32::from_le_bytes([t[0], t[1], t[2], t[3]]) as usize;
        if n >= size {
            n -= size;
        }
    }
    &b[..n]
}

pub fn decode_file(data: &[u8]) -> Result<AudioBuffer> {
    let skip = id3v2_size(data).min(data.len());
    let mut buf = without_trailing_tags(&data[skip..]);
    let mut free = 0;
    let (at, size) = find_frame(buf, &mut free);
    if size == 0 || at + size > buf.len() {
        return Err(Error::Invalid("MP3: no frame found".into()));
    }
    buf = &buf[at..];
    let Some(head) = Head::at(buf, 0) else {
        return Err(Error::Invalid("MP3: no frame found".into()));
    };
    let channels = head.channels();
    let rate = head.sample_rate();
    let layer = head.layer();
    let mut to_skip = 0usize;
    let mut detected = 0usize;
    if let Some(tag) = vbr_tag(&buf[..size]) {
        if tag.frames > 0 {
            to_skip = tag.delay * channels;
            detected = (head.frame_samples() * channels).saturating_mul(tag.frames as usize);
            detected = detected.saturating_sub(to_skip);
            let padding = tag.padding * channels;
            if padding > 0 && detected >= padding {
                detected -= padding;
            }
            if detected == 0 {
                return Ok(AudioBuffer::new(rate, channels as u16));
            }
        }
        buf = &buf[size..];
    }
    let mut out = AudioBuffer::new(rate, channels as u16);
    out.samples.reserve(if detected > 0 {
        detected
    } else {
        buf.len() / size.max(1) * head.frame_samples() * channels
    });
    let mut dec = Decoder::new();
    let mut pcm = vec![0f32; MAX_SAMPLES_PER_FRAME];
    loop {
        let info = dec.decode(buf, &mut pcm);
        if info.frame_bytes == 0 {
            break;
        }
        buf = &buf[info.frame_bytes.min(buf.len())..];
        if info.samples == 0 {
            continue;
        }
        if info.rate != rate || info.layer != layer || usize::from(info.channels) != channels {
            break;
        }
        let n = (info.samples * channels).min(pcm.len());
        let drop = to_skip.min(n);
        to_skip -= drop;
        out.samples.extend_from_slice(&pcm[drop..n]);
    }
    if detected > 0 && out.samples.len() > detected {
        out.samples.truncate(detected);
    }
    Ok(out)
}
