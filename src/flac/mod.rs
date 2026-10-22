mod bits;
mod crc;
mod file;
mod frame;
mod meta;
mod subframe;

#[cfg(test)]
mod malformed;
#[cfg(test)]
mod synth;
#[cfg(test)]
mod tests;

pub use file::{Reader, decode_file, read_info};
pub use meta::{SeekPoint, StreamInfo, config};

use bits::Bits;
use frame::{Assignment, Number};

use crate::media::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Short,
    Bad(&'static str),
    Unsupported(&'static str),
}

impl From<Fault> for Error {
    fn from(f: Fault) -> Error {
        match f {
            Fault::Short => Error::Invalid("a FLAC frame cut short".to_string()),
            Fault::Bad(why) => Error::Invalid(format!("FLAC: {why}")),
            Fault::Unsupported(what) => Error::Unsupported(what.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub sample: u64,
    pub block_size: usize,
    pub channels: usize,
    pub bits: u32,
    pub rate: u32,
    pub bytes: usize,
}

pub struct Decoder {
    info: Option<StreamInfo>,
    fixed: u32,
    buf: Vec<i32>,
    last: Option<Frame>,
}

impl Decoder {
    pub fn new(info: Option<StreamInfo>) -> Decoder {
        Decoder {
            info,
            fixed: 0,
            buf: Vec::new(),
            last: None,
        }
    }

    pub fn from_config(config: &[u8]) -> Result<Decoder> {
        let info = meta::config(config)?;
        if info.bits > 31 {
            return Err(Error::Unsupported("32-bit FLAC".to_string()));
        }
        Ok(Decoder::new(Some(info)))
    }

    pub fn info(&self) -> Option<&StreamInfo> {
        self.info.as_ref()
    }

    pub fn rate(&self) -> u32 {
        self.info
            .map(|i| i.rate)
            .or(self.last.map(|f| f.rate))
            .unwrap_or(0)
    }

    pub fn channels(&self) -> u16 {
        self.info
            .map(|i| i.channels as u16)
            .or(self.last.map(|f| f.channels as u16))
            .unwrap_or(0)
    }

    pub fn decode_frame(&mut self, data: &[u8]) -> Result<Frame> {
        Ok(self.frame(data)?)
    }

    pub fn channel(&self, ch: usize) -> &[i32] {
        match self.last {
            Some(f) if ch < f.channels => &self.buf[ch * f.block_size..(ch + 1) * f.block_size],
            _ => &[],
        }
    }

    pub fn interleave_i32(&self, out: &mut Vec<i32>) {
        self.put_i32(0, usize::MAX, out);
    }

    pub fn interleave_f32(&self, out: &mut Vec<f32>) {
        self.put_f32(0, usize::MAX, out);
    }

    pub fn decode_packet(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<()> {
        out.clear();
        self.frame(data)?;
        self.interleave_f32(out);
        Ok(())
    }

    pub(crate) fn frame(&mut self, data: &[u8]) -> std::result::Result<Frame, Fault> {
        self.last = None;
        let h = frame::header(data, self.info.as_ref())?;
        if let Some(i) = &self.info
            && h.channels != i.channels as usize
        {
            return Err(Fault::Bad(
                "a frame whose channel count differs from its stream's",
            ));
        }
        let bs = h.block_size as usize;
        let need = bs * h.channels;
        if self.buf.len() < need {
            self.buf.resize(need, 0);
        }
        let body = data.get(h.len..).ok_or(Fault::Short)?;
        let mut bits = Bits::new(body);
        let buf = &mut self.buf[..need];
        match h.assignment {
            Assignment::Independent => {
                for c in buf.chunks_exact_mut(bs) {
                    subframe::decode(&mut bits, h.bits, c)?;
                }
            }
            Assignment::LeftSide | Assignment::MidSide => {
                let (a, b) = buf.split_at_mut(bs);
                subframe::decode(&mut bits, h.bits, a)?;
                subframe::decode(&mut bits, h.bits + 1, b)?;
            }
            Assignment::RightSide => {
                let (a, b) = buf.split_at_mut(bs);
                subframe::decode(&mut bits, h.bits + 1, a)?;
                subframe::decode(&mut bits, h.bits, b)?;
            }
        }
        if bits.align()? != 0 {
            return Err(Fault::Bad("non-zero padding after the subframes"));
        }
        let end = h.len + bits.bytes();
        let footer = data.get(end..end + 2).ok_or(Fault::Short)?;
        if crc::crc16(&data[..end]) != u16::from_be_bytes([footer[0], footer[1]]) {
            return Err(Fault::Bad("a frame CRC mismatch"));
        }
        if h.channels == 2 {
            let (a, b) = buf.split_at_mut(bs);
            match h.assignment {
                Assignment::LeftSide => frame::left_side(a, b),
                Assignment::RightSide => frame::right_side(a, b),
                Assignment::MidSide => frame::mid_side(a, b, h.bits),
                Assignment::Independent => {}
            }
        }
        if !frame::in_range(buf, h.bits) {
            return Err(Fault::Bad("a sample outside its sample size"));
        }
        let mut next_fixed = 0;
        let sample = match h.number {
            Number::Sample(s) => s,
            Number::Frame(x) => {
                if self.fixed != 0 {
                    self.fixed as u64 * x as u64
                } else if let Some(b) = self.info.and_then(|i| i.fixed_block()) {
                    next_fixed = b;
                    b as u64 * x as u64
                } else if x == 0 {
                    next_fixed = h.block_size;
                    0
                } else {
                    h.block_size as u64 * x as u64
                }
            }
        };
        if next_fixed != 0 {
            self.fixed = next_fixed;
        }
        let f = Frame {
            sample,
            block_size: bs,
            channels: h.channels,
            bits: h.bits,
            rate: h.rate,
            bytes: end + 2,
        };
        self.last = Some(f);
        Ok(f)
    }

    pub(crate) fn put_f32(&self, from: usize, to: usize, out: &mut Vec<f32>) {
        let Some(f) = self.last else {
            return;
        };
        let (bs, ch) = (f.block_size, f.channels);
        let to = to.min(bs);
        if from >= to {
            return;
        }
        let k = 1.0 / (1u32 << (f.bits - 1)) as f32;
        let start = out.len();
        out.resize(start + (to - from) * ch, 0.0);
        let dst = &mut out[start..];
        let src = &self.buf[..bs * ch];
        if ch == 2 {
            let (l, r) = src.split_at(bs);
            for ((d, &l), &r) in dst.chunks_exact_mut(2).zip(&l[from..to]).zip(&r[from..to]) {
                d[0] = l as f32 * k;
                d[1] = r as f32 * k;
            }
        } else {
            for (c, s) in src.chunks_exact(bs).enumerate() {
                for (d, &v) in dst[c..].iter_mut().step_by(ch).zip(&s[from..to]) {
                    *d = v as f32 * k;
                }
            }
        }
    }

    pub(crate) fn put_i32(&self, from: usize, to: usize, out: &mut Vec<i32>) {
        let Some(f) = self.last else {
            return;
        };
        let (bs, ch) = (f.block_size, f.channels);
        let to = to.min(bs);
        if from >= to {
            return;
        }
        let start = out.len();
        out.resize(start + (to - from) * ch, 0);
        let dst = &mut out[start..];
        for (c, s) in self.buf[..bs * ch].chunks_exact(bs).enumerate() {
            for (d, &v) in dst[c..].iter_mut().step_by(ch).zip(&s[from..to]) {
                *d = v;
            }
        }
    }
}
