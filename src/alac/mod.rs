mod ag;
mod bits;
mod dp;
mod matrix;

#[cfg(test)]
mod tests;

use ag::Params;
use bits::{Bits, overrun};

use crate::media::{Error, Result};

pub const MAX_CHANNELS: u8 = 8;
pub const MAX_FRAME_LENGTH: u32 = 1 << 18;
pub const DEFAULT_FRAME_LENGTH: u32 = 4096;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub frame_length: u32,
    pub compatible_version: u8,
    pub bit_depth: u8,
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub channels: u8,
    pub max_run: u16,
    pub max_frame_bytes: u32,
    pub avg_bit_rate: u32,
    pub sample_rate: u32,
}

impl Config {
    pub fn parse(cookie: &[u8]) -> Result<Config> {
        let mut c = cookie;
        if c.get(4..8) == Some(b"frma") {
            c = c.get(12..).unwrap_or_default();
        }
        if c.get(4..8) == Some(b"alac") {
            c = c.get(12..).unwrap_or_default();
        }
        let Some(
            &[
                f0,
                f1,
                f2,
                f3,
                version,
                depth,
                pb,
                mb,
                kb,
                channels,
                r0,
                r1,
                x0,
                x1,
                x2,
                x3,
                a0,
                a1,
                a2,
                a3,
                s0,
                s1,
                s2,
                s3,
            ],
        ) = c.get(..24)
        else {
            return Err(Error::Invalid(format!(
                "an ALAC magic cookie of {} bytes, not 24",
                cookie.len()
            )));
        };
        Ok(Config {
            frame_length: u32::from_be_bytes([f0, f1, f2, f3]),
            compatible_version: version,
            bit_depth: depth,
            pb,
            mb,
            kb,
            channels,
            max_run: u16::from_be_bytes([r0, r1]),
            max_frame_bytes: u32::from_be_bytes([x0, x1, x2, x3]),
            avg_bit_rate: u32::from_be_bytes([a0, a1, a2, a3]),
            sample_rate: u32::from_be_bytes([s0, s1, s2, s3]),
        })
    }

    pub fn check(&self) -> Result<()> {
        if self.compatible_version != 0 {
            return Err(Error::Unsupported(format!(
                "ALAC version {}",
                self.compatible_version
            )));
        }
        if !matches!(self.bit_depth, 16 | 20 | 24 | 32) {
            return Err(Error::Unsupported(format!("{}-bit ALAC", self.bit_depth)));
        }
        if !(1..=MAX_CHANNELS).contains(&self.channels) {
            return Err(Error::Unsupported(format!(
                "ALAC with {} channels",
                self.channels
            )));
        }
        if !(1..=MAX_FRAME_LENGTH).contains(&self.frame_length) {
            return Err(Error::Invalid(format!(
                "ALAC frames of {} samples",
                self.frame_length
            )));
        }
        if self.sample_rate == 0 {
            return Err(Error::Invalid("ALAC at 0 Hz".into()));
        }
        Ok(())
    }
}

fn wave_order(channels: u8) -> Option<&'static [usize]> {
    match channels {
        3 => Some(&[1, 2, 0]),
        4 => Some(&[1, 2, 0, 3]),
        5 => Some(&[1, 2, 0, 3, 4]),
        6 => Some(&[1, 2, 0, 5, 3, 4]),
        7 => Some(&[1, 2, 0, 6, 3, 4, 5]),
        8 => Some(&[3, 4, 0, 7, 5, 6, 1, 2]),
        _ => None,
    }
}

pub fn refused(cookie: Option<&[u8]>) -> Option<String> {
    let config = match cookie.filter(|c| !c.is_empty()).map(Config::parse) {
        None => return Some("ALAC without its magic cookie".into()),
        Some(Err(_)) => return Some("ALAC with a damaged magic cookie".into()),
        Some(Ok(c)) => c,
    };
    config.check().err().map(|e| match e {
        Error::Unsupported(m) | Error::Invalid(m) => m,
        e => e.to_string(),
    })
}

pub fn find_cookie(boxes: &[u8]) -> Option<&[u8]> {
    let mut rest = boxes;
    while rest.len() >= 8 {
        let size = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if size < 8 || size > rest.len() {
            return None;
        }
        match rest.get(4..8)? {
            b"alac" => return rest.get(..size),
            b"wave" => {
                if let Some(c) = rest.get(8..size).and_then(find_cookie) {
                    return Some(c);
                }
            }
            _ => {}
        }
        rest = rest.get(size..)?;
    }
    None
}

#[cfg(feature = "mp4")]
pub fn entry_cookie(e: &crate::mp4::SampleEntry) -> Option<&[u8]> {
    let extra = match e.audio.as_ref().map_or(0, |a| a.qt_version) {
        1 => 16,
        2 => 36,
        _ => 0,
    };
    [28 + extra, 28, 44, 64]
        .into_iter()
        .find_map(|at| e.raw.get(at..).and_then(find_cookie))
}

struct Element {
    partial: bool,
    bytes_shifted: u32,
    escape: bool,
}

fn element_head(b: &mut Bits) -> Result<Element> {
    b.read(4);
    if b.read(12) != 0 {
        return Err(Error::Invalid("ALAC element with a non-zero header".into()));
    }
    let head = b.read(4);
    let bytes_shifted = (head >> 1) & 3;
    if bytes_shifted == 3 {
        return Err(Error::Invalid("ALAC element shifted by 3 bytes".into()));
    }
    Ok(Element {
        partial: head >> 3 != 0,
        bytes_shifted,
        escape: head & 1 != 0,
    })
}

fn chan_bits(bits: i32) -> Result<u32> {
    if (1..=32).contains(&bits) {
        Ok(bits as u32)
    } else {
        Err(Error::Invalid(format!("ALAC element of {bits} bits")))
    }
}

struct Predictor {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    num: usize,
    coefs: [i16; 32],
}

fn predictor_head(b: &mut Bits) -> Predictor {
    let h = b.read(8);
    let mode = h >> 4;
    let den_shift = h & 0xf;
    let h = b.read(8);
    let pb_factor = h >> 5;
    let num = (h & 0x1f) as usize;
    let mut coefs = [0i16; 32];
    for c in coefs.iter_mut().take(num) {
        *c = b.read(16) as u16 as i16;
    }
    Predictor {
        mode,
        den_shift,
        pb_factor,
        num,
        coefs,
    }
}

fn params(c: &Config, p: &Predictor) -> Params {
    Params::new(c.mb as u32, (c.pb as u32 * p.pb_factor) / 4, c.kb as u32)
}

fn read_escape(b: &mut Bits, chan_bits: u32) -> i32 {
    let shift = 32 - chan_bits;
    if chan_bits <= 16 {
        (b.read(chan_bits) as i32)
            .wrapping_shl(shift)
            .wrapping_shr(shift)
    } else {
        let v = (b.read(16) as i32).wrapping_shl(16).wrapping_shr(shift);
        v | b.read(chan_bits - 16) as i32
    }
}

fn data_stream(b: &mut Bits) -> Result<()> {
    b.read(4);
    let align = b.one() != 0;
    let mut count = b.read(8) as usize;
    if count == 255 {
        count += b.read(8) as usize;
    }
    if align {
        b.align();
    }
    b.advance(count * 8);
    b.check()
}

fn fill(b: &mut Bits) -> Result<()> {
    let mut count = b.read(4) as usize;
    if count == 15 {
        count += b.read(8) as usize;
        count -= 1;
    }
    b.advance(count * 8);
    b.check()
}

pub struct Decoder {
    config: Config,
    order: Option<&'static [usize]>,
    predictor: Vec<i32>,
    mix_u: Vec<i32>,
    mix_v: Vec<i32>,
    shift: Vec<u16>,
    samples: Vec<i32>,
    apple: bool,
}

impl Decoder {
    pub fn new(cookie: &[u8]) -> Result<Decoder> {
        Decoder::from_config(Config::parse(cookie)?)
    }

    pub fn from_config(config: Config) -> Result<Decoder> {
        config.check()?;
        let n = config.frame_length as usize;
        Ok(Decoder {
            config,
            order: wave_order(config.channels),
            predictor: vec![0; n],
            mix_u: vec![0; n],
            mix_v: vec![0; n],
            shift: vec![0; n * 2],
            samples: Vec::with_capacity(n * config.channels as usize),
            apple: false,
        })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.config.channels as u16
    }

    pub fn bit_depth(&self) -> u8 {
        self.config.bit_depth
    }

    pub fn decode(&mut self, packet: &[u8]) -> Result<&[i32]> {
        self.apple = false;
        let num = match self.run(packet) {
            Err(e @ Error::Invalid(_)) => {
                self.apple = true;
                self.run(packet).map_err(|_| e)?
            }
            done => done?,
        };
        Ok(&self.samples[..num * self.config.channels as usize])
    }

    fn run(&mut self, packet: &[u8]) -> Result<usize> {
        let channels = self.config.channels as usize;
        let frame_length = self.config.frame_length as usize;
        self.samples.clear();
        self.samples.resize(frame_length * channels, 0);
        let mut b = Bits::new(packet);
        let mut num = frame_length;
        let mut index = 0usize;
        loop {
            if !b.more() {
                return Err(overrun());
            }
            match b.read(3) {
                ID_SCE | ID_LFE => {
                    self.single(&mut b, &mut num, index)?;
                    index += 1;
                }
                ID_CPE => {
                    if index + 2 > channels {
                        break;
                    }
                    self.pair(&mut b, &mut num, index)?;
                    index += 2;
                }
                ID_CCE | ID_PCE => {
                    return Err(Error::Unsupported(
                        "ALAC coupling or program config elements".into(),
                    ));
                }
                ID_DSE => data_stream(&mut b)?,
                ID_FIL => fill(&mut b)?,
                _ => {
                    b.align();
                    break;
                }
            }
            if index >= channels {
                break;
            }
        }
        Ok(num)
    }

    pub fn decode_into(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize> {
        let scale = 1.0 / (1u64 << (self.config.bit_depth - 1)) as f32;
        let channels = self.config.channels as usize;
        let order = self.order;
        let samples = self.decode(packet)?;
        out.clear();
        match order {
            Some(order) => out.extend(
                samples
                    .chunks_exact(channels)
                    .flat_map(|f| order.iter().map(move |&c| f[c] as f32 * scale)),
            ),
            None => out.extend(samples.iter().map(|&s| s as f32 * scale)),
        }
        Ok(samples.len() / channels)
    }

    fn partial(&self, b: &mut Bits, e: &Element, num: &mut usize) -> Result<()> {
        if !e.partial {
            return Ok(());
        }
        let n = ((b.read(16) << 16) | b.read(16)) as usize;
        if n > self.config.frame_length as usize {
            return Err(Error::Invalid(format!(
                "an ALAC frame of {n} samples, over its {}",
                self.config.frame_length
            )));
        }
        *num = n;
        Ok(())
    }

    fn single(&mut self, b: &mut Bits, num: &mut usize, index: usize) -> Result<()> {
        let e = element_head(b)?;
        let depth = self.config.bit_depth as i32;
        let mut bytes_shifted = e.bytes_shifted;
        let bits = chan_bits(depth - (bytes_shifted * 8) as i32)?;
        self.partial(b, &e, num)?;
        let n = *num;
        let mut shift_bits = *b;
        if !e.escape {
            b.read(8);
            b.read(8);
            let mut u = predictor_head(b);
            if bytes_shifted != 0 {
                shift_bits = *b;
                b.advance(bytes_shifted as usize * 8 * n);
            }
            let params = params(&self.config, &u);
            ag::dyn_decomp(&params, b, &mut self.predictor, n, bits, self.apple)?;
            if u.mode != 0 {
                dp::first_order(&mut self.predictor, n, bits);
            }
            dp::unpc_block(
                &self.predictor,
                &mut self.mix_u,
                n,
                &mut u.coefs,
                u.num,
                bits,
                u.den_shift,
            );
        } else {
            for v in &mut self.mix_u[..n] {
                *v = read_escape(b, bits);
            }
            bytes_shifted = 0;
        }
        let shift = bytes_shifted * 8;
        if bytes_shifted != 0 {
            for s in &mut self.shift[..n] {
                *s = shift_bits.read(shift) as u16;
            }
        }
        b.check()?;
        let channels = self.config.channels as usize;
        let depth = self.config.bit_depth;
        let out = self.samples.get_mut(index..).ok_or_else(overrun)?;
        for (i, (&u, o)) in self.mix_u[..n]
            .iter()
            .zip(out.iter_mut().step_by(channels))
            .enumerate()
        {
            *o = matrix::emit(u, self.shift[i], shift, depth);
        }
        Ok(())
    }

    fn pair(&mut self, b: &mut Bits, num: &mut usize, index: usize) -> Result<()> {
        let e = element_head(b)?;
        let depth = self.config.bit_depth as i32;
        let mut bytes_shifted = e.bytes_shifted;
        self.partial(b, &e, num)?;
        let n = *num;
        let mut shift_bits = *b;
        let mut mix_bits = 0u32;
        let mut mix_res = 0i32;
        if !e.escape {
            let bits = chan_bits(depth - (bytes_shifted * 8) as i32 + 1)?;
            mix_bits = b.read(8);
            mix_res = b.read(8) as u8 as i8 as i32;
            let mut u = predictor_head(b);
            let mut v = predictor_head(b);
            if bytes_shifted != 0 {
                shift_bits = *b;
                b.advance(bytes_shifted as usize * 8 * 2 * n);
            }
            for (p, mix) in [(&mut u, &mut self.mix_u), (&mut v, &mut self.mix_v)] {
                let params = params(&self.config, p);
                ag::dyn_decomp(&params, b, &mut self.predictor, n, bits, self.apple)?;
                if p.mode != 0 {
                    dp::first_order(&mut self.predictor, n, bits);
                }
                dp::unpc_block(
                    &self.predictor,
                    mix,
                    n,
                    &mut p.coefs,
                    p.num,
                    bits,
                    p.den_shift,
                );
            }
        } else {
            let bits = chan_bits(depth)?;
            for (u, v) in self.mix_u[..n].iter_mut().zip(&mut self.mix_v[..n]) {
                *u = read_escape(b, bits);
                *v = read_escape(b, bits);
            }
            bytes_shifted = 0;
        }
        let shift = bytes_shifted * 8;
        if bytes_shifted != 0 {
            for s in &mut self.shift[..n * 2] {
                *s = shift_bits.read(shift) as u16;
            }
        }
        b.check()?;
        let channels = self.config.channels as usize;
        let out = self.samples.get_mut(index..).ok_or_else(overrun)?;
        matrix::unmix(
            &self.mix_u[..n],
            &self.mix_v[..n],
            out,
            channels,
            mix_bits,
            mix_res,
            &self.shift[..n * 2],
            shift,
            self.config.bit_depth,
        );
        Ok(())
    }
}
