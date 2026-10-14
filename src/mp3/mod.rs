#![allow(clippy::needless_range_loop, clippy::excessive_precision)]

mod bits;
mod hdr;
mod layer12;
mod layer3;
mod simd;
mod synth;
mod tables;

#[cfg(test)]
mod malformed;
#[cfg(test)]
mod tests;

use bits::Bits;
use hdr::{HDR_SIZE, Head};
use layer3::GrInfo;

use crate::media::{Error, Result};

pub const MAX_SAMPLES_PER_FRAME: usize = 1152 * 2;

const MAX_FREE_FORMAT_FRAME_SIZE: usize = 2304;
const MAX_FRAME_SYNC_MATCHES: usize = 10;
const MAX_L3_FRAME_PAYLOAD_BYTES: usize = MAX_FREE_FORMAT_FRAME_SIZE;
const MAX_BITRESERVOIR_BYTES: usize = 511;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameInfo {
    pub frame_bytes: usize,
    pub frame_offset: usize,
    pub channels: u16,
    pub rate: u32,
    pub layer: u8,
    pub bitrate_kbps: u32,
    pub samples: usize,
}

struct State {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: usize,
    free_format_bytes: usize,
    header: Head,
    reserv_buf: [u8; MAX_BITRESERVOIR_BYTES],
}

struct Scratch {
    maindata: [u8; MAX_BITRESERVOIR_BYTES + MAX_L3_FRAME_PAYLOAD_BYTES],
    gr_info: [GrInfo; 4],
    grbuf: [f32; 1156],
    scf: [f32; 40],
    syn: [f32; 2112],
    ist_pos: [[u8; 39]; 2],
    sci: layer12::ScaleInfo,
}

pub struct Decoder {
    st: Box<State>,
    sc: Box<Scratch>,
    pcm: Box<[f32; MAX_SAMPLES_PER_FRAME]>,
    rate: u32,
    channels: u16,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn match_frame(b: &[u8], frame_bytes: usize) -> bool {
    let Some(first) = Head::at(b, 0) else {
        return false;
    };
    let mut i = 0usize;
    for nmatch in 0..MAX_FRAME_SYNC_MATCHES {
        let Some(h) = Head::at(b, i) else {
            return nmatch > 0;
        };
        i += h.frame_bytes(frame_bytes) + h.padding();
        let Some(next) = Head::at(b, i) else {
            return nmatch > 0;
        };
        if !first.compare(next) {
            return false;
        }
    }
    true
}

fn find_frame(mp3: &[u8], free_format_bytes: &mut usize) -> (usize, usize) {
    let n = mp3.len();
    for i in 0..n.saturating_sub(HDR_SIZE) {
        let Some(h) = Head::at(mp3, i) else {
            break;
        };
        if !h.valid() {
            continue;
        }
        let mut frame_bytes = h.frame_bytes(*free_format_bytes);
        let mut frame_and_padding = frame_bytes + h.padding();
        let mut k = HDR_SIZE;
        while frame_bytes == 0 && k < MAX_FREE_FORMAT_FRAME_SIZE && i + 2 * k < n - HDR_SIZE {
            if let Some(hk) = Head::at(mp3, i + k)
                && h.compare(hk)
            {
                let fb = k.saturating_sub(h.padding());
                let nextfb = fb + hk.padding();
                if let Some(hn) = Head::at(mp3, i + k + nextfb)
                    && h.compare(hn)
                {
                    frame_and_padding = k;
                    frame_bytes = fb;
                    *free_format_bytes = fb;
                }
            }
            k += 1;
        }
        if (frame_bytes != 0 && i + frame_and_padding <= n && match_frame(&mp3[i..], frame_bytes))
            || (i == 0 && frame_and_padding == n)
        {
            return (i, frame_and_padding);
        }
        *free_format_bytes = 0;
    }
    (n, 0)
}

impl State {
    fn reset(&mut self) {
        self.mdct_overlap = [[0.0; 288]; 2];
        self.qmf_state = [0.0; 960];
        self.reserv = 0;
        self.free_format_bytes = 0;
        self.header = Head::default();
        self.reserv_buf = [0; MAX_BITRESERVOIR_BYTES];
    }
}

fn decode_at(st: &mut State, sc: &mut Scratch, frame: &[u8], pcm: &mut [f32]) -> usize {
    let Some(h) = Head::at(frame, 0) else {
        return 0;
    };
    let nch = h.channels();
    if pcm.len() < h.frame_samples() * nch {
        return 0;
    }
    st.header = h;
    let Scratch {
        maindata,
        gr_info,
        grbuf,
        scf,
        syn,
        ist_pos,
        sci,
    } = sc;
    let mut bs = Bits::new(&frame[HDR_SIZE..]);
    if h.crc() {
        bs.get(16);
    }
    if h.layer() == 3 {
        let main_data_begin = layer3::read_side_info(&mut bs, gr_info, h);
        if main_data_begin < 0 || bs.pos > bs.limit {
            st.header = Head::default();
            return 0;
        }
        let main_data_begin = main_data_begin as usize;
        let frame_bytes = (bs.limit - bs.pos) / 8;
        let have = st.reserv.min(main_data_begin);
        let start = bs.pos / 8;
        if have + frame_bytes > maindata.len() {
            st.header = Head::default();
            return 0;
        }
        let from = st.reserv.saturating_sub(main_data_begin);
        maindata[..have].copy_from_slice(&st.reserv_buf[from..from + have]);
        maindata[have..have + frame_bytes].copy_from_slice(&bs.buf[start..start + frame_bytes]);
        let success = st.reserv >= main_data_begin;
        let mut w = layer3::Work {
            bs: Bits::new(&maindata[..have + frame_bytes]),
            grbuf,
            scf,
            syn,
            ist_pos,
        };
        if success {
            let granules = if h.mpeg1() { 2 } else { 1 };
            for igr in 0..granules {
                w.grbuf.fill(0.0);
                layer3::decode(&mut w, h, &mut st.mdct_overlap, &gr_info[igr * nch..], nch);
                synth::granule(
                    &mut st.qmf_state,
                    &mut w.grbuf[..],
                    18,
                    nch,
                    &mut pcm[576 * nch * igr..],
                    &mut w.syn[..],
                );
            }
        }
        let mut pos = w.bs.pos.div_ceil(8);
        let limit = w.bs.limit / 8;
        let mut remains = limit as isize - pos as isize;
        if remains > MAX_BITRESERVOIR_BYTES as isize {
            pos += remains as usize - MAX_BITRESERVOIR_BYTES;
            remains = MAX_BITRESERVOIR_BYTES as isize;
        }
        if remains > 0 {
            let r = remains as usize;
            st.reserv_buf[..r].copy_from_slice(&maindata[pos..pos + r]);
            st.reserv = r;
        } else {
            st.reserv = 0;
        }
        if success { h.frame_samples() } else { 0 }
    } else {
        layer12::read_scale_info(h, &mut bs, sci);
        grbuf.fill(0.0);
        let group = (h.layer() | 1) as usize;
        let mut i = 0;
        let mut out = 0;
        for igr in 0..3 {
            i += layer12::dequantize_granule(&mut grbuf[..], i, &mut bs, sci, group);
            if i == 12 {
                i = 0;
                layer12::apply_scf_384(sci, igr, &mut grbuf[..]);
                synth::granule(
                    &mut st.qmf_state,
                    &mut grbuf[..],
                    12,
                    nch,
                    &mut pcm[out..],
                    &mut syn[..],
                );
                grbuf.fill(0.0);
                out += 384 * nch;
            }
            if bs.pos > bs.limit {
                st.header = Head::default();
                return 0;
            }
        }
        h.frame_samples()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            st: Box::new(State {
                mdct_overlap: [[0.0; 288]; 2],
                qmf_state: [0.0; 960],
                reserv: 0,
                free_format_bytes: 0,
                header: Head::default(),
                reserv_buf: [0; MAX_BITRESERVOIR_BYTES],
            }),
            sc: Box::new(Scratch {
                maindata: [0; MAX_BITRESERVOIR_BYTES + MAX_L3_FRAME_PAYLOAD_BYTES],
                gr_info: [GrInfo::default(); 4],
                grbuf: [0.0; 1156],
                scf: [0.0; 40],
                syn: [0.0; 2112],
                ist_pos: [[0; 39]; 2],
                sci: layer12::ScaleInfo::default(),
            }),
            pcm: Box::new([0.0; MAX_SAMPLES_PER_FRAME]),
            rate: 0,
            channels: 0,
        }
    }

    pub fn with_format(rate: u32, channels: u16) -> Decoder {
        let mut d = Decoder::new();
        d.rate = rate;
        d.channels = channels.clamp(1, 2);
        d
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels.max(1)
    }

    pub fn reset(&mut self) {
        self.st.reset();
    }

    pub fn decode(&mut self, mp3: &[u8], pcm: &mut [f32]) -> FrameInfo {
        let n = mp3.len();
        let mut i = 0;
        let mut frame_size = 0;
        if n > HDR_SIZE
            && self.st.header.0[0] == 0xFF
            && let Some(h) = Head::at(mp3, 0)
            && self.st.header.compare(h)
        {
            frame_size = h.frame_bytes(self.st.free_format_bytes) + h.padding();
            if frame_size != n && Head::at(mp3, frame_size).is_none_or(|next| !h.compare(next)) {
                frame_size = 0;
            }
        }
        if frame_size == 0 {
            self.st.reset();
            (i, frame_size) = find_frame(mp3, &mut self.st.free_format_bytes);
            if frame_size == 0 || i + frame_size > n {
                return FrameInfo {
                    frame_bytes: i,
                    ..FrameInfo::default()
                };
            }
        }
        let Some(h) = Head::at(mp3, i) else {
            return FrameInfo {
                frame_bytes: n,
                ..FrameInfo::default()
            };
        };
        let samples = decode_at(&mut self.st, &mut self.sc, &mp3[i..i + frame_size], pcm);
        FrameInfo {
            frame_bytes: i + frame_size,
            frame_offset: i,
            channels: h.channels() as u16,
            rate: h.sample_rate(),
            layer: h.layer(),
            bitrate_kbps: h.bitrate_kbps(),
            samples,
        }
    }

    pub fn decode_packet(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<()> {
        out.clear();
        let mut p = 0;
        let mut frames = 0;
        while p + HDR_SIZE <= data.len() {
            let rest = &data[p..];
            let Some(h) = Head::at(rest, 0) else {
                break;
            };
            let size = if h.valid() {
                let fb = h.frame_bytes(0);
                if fb == 0 {
                    rest.len()
                } else {
                    fb + h.padding()
                }
            } else {
                0
            };
            let (at, size) = if size != 0 && size <= rest.len() {
                (0, size)
            } else {
                let mut free = self.st.free_format_bytes;
                let (i, size) = find_frame(rest, &mut free);
                if size == 0 || i + size > rest.len() {
                    break;
                }
                (i, size)
            };
            let frame = &rest[at..at + size];
            let Some(h) = Head::at(frame, 0) else {
                break;
            };
            if !self.st.header.compare(h) {
                self.st.reset();
            }
            if self.rate == 0 {
                self.rate = h.sample_rate();
            }
            if self.channels == 0 {
                self.channels = h.channels() as u16;
            }
            let n = decode_at(&mut self.st, &mut self.sc, frame, &mut self.pcm[..]);
            push(&self.pcm[..], n, h, self.channels() as usize, out);
            p += at + size;
            frames += 1;
        }
        if frames == 0 {
            return Err(Error::Invalid(format!(
                "an MP3 packet of {} bytes without a frame",
                data.len()
            )));
        }
        Ok(())
    }
}

fn push(pcm: &[f32], n: usize, h: Head, want: usize, out: &mut Vec<f32>) {
    let have = h.channels();
    if n == 0 {
        out.resize(out.len() + h.frame_samples() * want, 0.0);
        return;
    }
    let s = &pcm[..(n * have).min(pcm.len())];
    match (have, want) {
        (1, 2) => {
            out.reserve(s.len() * 2);
            for &x in s {
                out.push(x);
                out.push(x);
            }
        }
        (2, 1) => out.extend(s.chunks_exact(2).map(|c| 0.5 * (c[0] + c[1]))),
        _ => out.extend_from_slice(s),
    }
}
