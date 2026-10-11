use crate::media::{Error, Result};
use crate::opus::decoder::StreamDecoder;
use crate::opus::header::OpusHead;
use crate::opus::packet::Packet;

pub struct Decoder {
    head: OpusHead,
    rate: u32,
    streams: Vec<StreamDecoder>,
    trim_pre_skip: bool,
    skip_remaining: usize,
    scratch: Vec<Vec<f32>>,
    reframed: Vec<u8>,
}

impl Decoder {
    pub fn new(opus_head: &[u8]) -> Result<Decoder> {
        Self::from_head(OpusHead::parse(opus_head)?, 48000)
    }

    pub fn simple(sample_rate: u32, channels: usize) -> Result<Decoder> {
        Self::from_head(OpusHead::simple(channels)?, sample_rate)
    }

    pub fn from_head(head: OpusHead, sample_rate: u32) -> Result<Decoder> {
        let mut streams = Vec::with_capacity(head.streams);
        for s in 0..head.streams {
            let ch = if s < head.coupled_streams { 2 } else { 1 };
            let mut d = StreamDecoder::new(sample_rate, ch)?;
            d.set_gain_q8(head.output_gain);
            streams.push(d);
        }
        Ok(Decoder {
            rate: sample_rate,
            streams,
            trim_pre_skip: false,
            skip_remaining: 0,
            scratch: vec![Vec::new(); head.streams],
            reframed: Vec::new(),
            head,
        })
    }

    pub fn head(&self) -> &OpusHead {
        &self.head
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> usize {
        self.head.channels
    }

    pub fn pre_skip(&self) -> usize {
        self.head.pre_skip as usize * self.rate as usize / 48000
    }

    pub fn set_trim_pre_skip(&mut self, enable: bool) {
        self.trim_pre_skip = enable;
        self.skip_remaining = if enable { self.pre_skip() } else { 0 };
    }

    pub fn set_phase_inversion_disabled(&mut self, disabled: bool) {
        for s in &mut self.streams {
            s.set_phase_inversion_disabled(disabled);
        }
    }

    pub fn final_range(&self) -> u32 {
        self.streams.first().map_or(0, |s| s.final_range())
    }

    pub fn reset(&mut self) {
        for s in &mut self.streams {
            s.reset();
        }
        self.skip_remaining = if self.trim_pre_skip {
            self.pre_skip()
        } else {
            0
        };
    }

    pub fn decode_into(&mut self, packet: Option<&[u8]>, out: &mut Vec<f32>) -> Result<usize> {
        let n = self.streams.len();
        let mut samples = None;
        if n == 1 {
            let start = out.len();
            let r = match packet {
                Some(data) if !data.is_empty() => {
                    self.streams[0].decode_interleaved(Some(data), out)?
                }
                _ => {
                    let dur = self.conceal_len();
                    self.streams[0].conceal(dur, out)?
                }
            };
            if self.head.mapping_family != 0 || self.head.mapping != [0, 1][..self.head.channels] {
                let ch = self.head.channels;
                let tmp = &mut self.scratch[0];
                tmp.clear();
                tmp.extend_from_slice(&out[start..]);
                out.resize(start + r * ch, 0.0);
                let nch = self.streams[0].channels();
                for i in 0..r {
                    for c in 0..ch {
                        let m = self.head.mapping[c] as usize;
                        out[start + i * ch + c] = if m == 255 { 0.0 } else { tmp[i * nch + m] };
                    }
                }
            }
            return Ok(self.trim(out, start, r));
        }
        for b in &mut self.scratch {
            b.clear();
        }
        match packet {
            Some(data) if !data.is_empty() => {
                let mut off = 0;
                for s in 0..n {
                    let rest = &data[off..];
                    if rest.is_empty() {
                        return Err(Error::Invalid("truncated multistream packet".into()));
                    }
                    let used = if s + 1 < n {
                        Packet::parse_self_delimited(rest)?.1
                    } else {
                        rest.len()
                    };
                    let pkt = &rest[..used];
                    let r = if s + 1 < n {
                        let (p, _) = Packet::parse_self_delimited(pkt)?;
                        reframe(&p, &mut self.reframed);
                        self.streams[s]
                            .decode_interleaved(Some(&self.reframed), &mut self.scratch[s])?
                    } else {
                        self.streams[s].decode_interleaved(Some(pkt), &mut self.scratch[s])?
                    };
                    if *samples.get_or_insert(r) != r {
                        return Err(Error::Invalid("multistream frame durations differ".into()));
                    }
                    off += used;
                }
            }
            _ => {
                let dur = self.conceal_len();
                for s in 0..n {
                    self.streams[s].conceal(dur, &mut self.scratch[s])?;
                }
                samples = Some(dur);
            }
        }
        let samples = samples.unwrap_or(0);
        let ch = self.head.channels;
        let start = out.len();
        out.resize(start + samples * ch, 0.0);
        for c in 0..ch {
            let m = self.head.mapping[c] as usize;
            if m == 255 {
                continue;
            }
            let (s, sc, nch) = if m < 2 * self.head.coupled_streams {
                (m / 2, m % 2, 2)
            } else {
                (m - self.head.coupled_streams, 0, 1)
            };
            let src = &self.scratch[s];
            for i in 0..samples {
                out[start + i * ch + c] = src[i * nch + sc];
            }
        }
        Ok(self.trim(out, start, samples))
    }

    fn conceal_len(&self) -> usize {
        match self.streams[0].last_packet_duration() {
            0 => self.rate as usize / 50,
            d => d,
        }
    }

    fn trim(&mut self, out: &mut Vec<f32>, start: usize, samples: usize) -> usize {
        if self.skip_remaining == 0 {
            return samples;
        }
        let k = self.skip_remaining.min(samples);
        let ch = self.head.channels;
        out.drain(start..start + k * ch);
        self.skip_remaining -= k;
        samples - k
    }
}

fn reframe(p: &Packet, v: &mut Vec<u8>) {
    v.clear();
    v.push((p.toc.config << 3) | if p.toc.stereo { 4 } else { 0 } | 3);
    v.push(0x80 | p.frames.len() as u8);
    for f in &p.frames[..p.frames.len() - 1] {
        let s = f.len();
        if s < 252 {
            v.push(s as u8);
        } else {
            let b0 = 252 + (s & 3);
            v.push(b0 as u8);
            v.push(((s - b0) >> 2) as u8);
        }
    }
    for f in &p.frames {
        v.extend_from_slice(f);
    }
}
