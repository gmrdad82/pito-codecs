#![allow(clippy::needless_range_loop)]

mod aht;
mod aht_tables;
mod bits;
mod block;
mod eac3;
mod header;
mod imdct;
mod tables;

pub use header::frame_len;

use bits::Bits;
use block::{CHANNELS, LFE, State};
use header::{Header, Kind};
use imdct::Imdct;

use crate::media::{AudioBuffer, Error, Packet, Result};

const K: f32 = std::f32::consts::FRAC_1_SQRT_2;
const MAX_SAMPLES: usize = 1536;

const SL: usize = 6;
const SR: usize = 7;
const BL: usize = 4;
const BR: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub rate: u32,
    pub channels: u16,
    pub eac3: bool,
}

pub fn info(frame: &[u8]) -> Result<Info> {
    let h = header::parse(frame)?;
    Ok(Info {
        rate: h.rate,
        channels: h.nchans() as u16,
        eac3: h.eac3,
    })
}

pub fn layout(channels: u16) -> u16 {
    match channels {
        0 | 1 => 1,
        2 => 2,
        3..=6 => 6,
        _ => 8,
    }
}

fn locations(acmod: u8, lfeon: bool) -> ([u8; 6], usize) {
    let (front, n): ([u8; 5], usize) = match acmod {
        0 | 2 => ([0, 2, 0, 0, 0], 2),
        1 => ([1, 0, 0, 0, 0], 1),
        3 => ([0, 1, 2, 0, 0], 3),
        4 => ([0, 2, 16, 0, 0], 3),
        5 => ([0, 1, 2, 16, 0], 4),
        6 => ([0, 2, 3, 4, 0], 4),
        _ => ([0, 1, 2, 3, 4], 5),
    };
    let mut out = [0u8; 6];
    out[..5].copy_from_slice(&front);
    if lfeon {
        out[n] = 15;
    }
    (out, n + lfeon as usize)
}

fn chanmap_locations(map: u16, n: usize) -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    let mut k = 0;
    for bit in 0..16u8 {
        if map & (0x8000 >> bit) == 0 {
            continue;
        }
        let pair = matches!(bit, 5 | 6 | 9 | 10 | 11 | 13);
        for side in 0..1 + pair as u8 {
            if k >= 16 {
                return None;
            }
            out[k] = bit | (side << 5);
            k += 1;
        }
    }
    (k == n).then_some(out)
}

fn slots(loc: u8) -> &'static [(usize, f32)] {
    let right = loc & 0x20 != 0;
    match (loc & 0x1F, right) {
        (0, _) => &[(0, 1.0)],
        (1, _) => &[(2, 1.0)],
        (2, _) => &[(1, 1.0)],
        (3, _) => &[(SL, 1.0)],
        (4, _) => &[(SR, 1.0)],
        (5, false) => &[(0, K), (2, K)],
        (5, true) => &[(1, K), (2, K)],
        (6, false) => &[(BL, 1.0)],
        (6, true) => &[(BR, 1.0)],
        (7, _) => &[(BL, K), (BR, K)],
        (8, _) => &[(SL, 0.5), (SR, 0.5)],
        (9, false) => &[(SL, 1.0)],
        (9, true) => &[(SR, 1.0)],
        (10, false) => &[(0, 1.0)],
        (10, true) => &[(1, 1.0)],
        (11, false) => &[(0, K)],
        (11, true) => &[(1, K)],
        (12, _) => &[(2, K)],
        (13, false) => &[(SL, K)],
        (13, true) => &[(SR, K)],
        (16, _) => &[(SL, K), (SR, K)],
        _ => &[(3, 1.0)],
    }
}

struct Sub {
    state: State,
    pcm: Box<[[f32; MAX_SAMPLES]; CHANNELS]>,
    order: [usize; 6],
    locs: [u8; 16],
    n: usize,
    samples: usize,
    live: bool,
}

impl Sub {
    fn new() -> Sub {
        Sub {
            state: State::new(),
            pcm: Box::new([[0.0; MAX_SAMPLES]; CHANNELS]),
            order: [0; 6],
            locs: [0; 16],
            n: 0,
            samples: 0,
            live: false,
        }
    }
}

pub struct Decoder {
    rate: u32,
    channels: u16,
    imdct: Imdct,
    subs: Vec<Sub>,
    time: Box<[[[f32; 512]; CHANNELS]; 6]>,
    slots: Box<[[f32; MAX_SAMPLES]; 8]>,
    nodither: bool,
}

impl Decoder {
    pub fn new(rate: u32, channels: u16) -> Decoder {
        Decoder {
            rate,
            channels: layout(channels),
            imdct: Imdct::new(),
            subs: Vec::new(),
            time: Box::new([[[0.0; 512]; CHANNELS]; 6]),
            slots: Box::new([[0.0; MAX_SAMPLES]; 8]),
            nodither: false,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    pub fn reset(&mut self) {
        self.subs.clear();
    }

    #[cfg(test)]
    pub(crate) fn without_dither(mut self) -> Decoder {
        self.nodither = true;
        self
    }

    #[cfg(test)]
    pub(crate) fn holes(&mut self) -> Vec<usize> {
        self.subs
            .first_mut()
            .map(|s| std::mem::take(&mut s.state.holes))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn marks(&mut self) -> Vec<usize> {
        self.subs
            .first_mut()
            .map(|s| std::mem::take(&mut s.state.marks))
            .unwrap_or_default()
    }

    pub fn decode(&mut self, packet: &Packet) -> Result<AudioBuffer> {
        self.decode_frame(&packet.data)
    }

    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioBuffer> {
        let mut out = AudioBuffer::new(self.rate, self.channels);
        self.decode_into(data, &mut out.samples)?;
        Ok(out)
    }

    pub fn decode_into(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<()> {
        out.clear();
        let mut pos = 0;
        let mut open = false;
        while data.len() - pos >= 6 {
            let rest = &data[pos..];
            let h = header::parse(rest)?;
            let frame = rest.get(..h.bytes).ok_or_else(|| {
                Error::Invalid(format!(
                    "AC-3 frame of {} bytes in {} remaining",
                    h.bytes,
                    rest.len()
                ))
            })?;
            pos += h.bytes;
            let slot = match (h.kind, h.substream) {
                (Kind::Independent | Kind::Convert, 0) => 0,
                (Kind::Dependent, k) if open => 1 + k as usize,
                _ => continue,
            };
            if slot == 0 {
                if open {
                    self.render(out);
                }
                for s in self.subs.iter_mut() {
                    s.live = false;
                }
                self.rate = h.rate;
                open = true;
            }
            while self.subs.len() <= slot {
                self.subs.push(Sub::new());
            }
            if let Err(e) = self.substream(slot, &h, frame) {
                self.subs[slot] = Sub::new();
                return Err(e);
            }
        }
        if !open {
            return Err(Error::Invalid(
                "AC-3 packet without an independent frame".into(),
            ));
        }
        self.render(out);
        Ok(())
    }

    fn substream(&mut self, slot: usize, h: &Header, frame: &[u8]) -> Result<()> {
        let samples = h.blocks * 256;
        if slot > 0 && self.subs[0].samples != samples {
            return Err(Error::Invalid(
                "E-AC-3 dependent frame with a different block count".into(),
            ));
        }
        let sub = &mut self.subs[slot];
        sub.state.nodither = self.nodither;
        let mut b = Bits::new(frame);
        b.skip(h.bits)?;
        if h.eac3 {
            eac3::decode(&mut sub.state, &mut b, h)?;
        } else {
            for blk in 0..6 {
                sub.state.blk = blk;
                block::read_block_ac3(&mut sub.state, &mut b, h, blk)?;
                block::dsp(&mut sub.state, h.nfchans, h.acmod);
                sub.state.mark(h.nfchans, h.lfeon);
            }
        }
        let chans: Vec<usize> = (0..h.nfchans).chain(h.lfeon.then_some(LFE)).collect();
        let mut spec = Vec::with_capacity(36);
        let mut short = Vec::with_capacity(36);
        let mut time = Vec::with_capacity(36);
        for (blk, t) in self.time.iter_mut().enumerate().take(h.blocks) {
            for (ch, t) in t.iter_mut().enumerate() {
                if chans.contains(&ch) {
                    spec.push(&sub.state.spec[blk][ch]);
                    short.push(sub.state.short[blk][ch]);
                    time.push(t);
                }
            }
        }
        self.imdct.run(&spec, &short, &mut time);
        for &ch in &chans {
            for blk in 0..h.blocks {
                let c = &mut sub.state.ch[ch];
                self.imdct.overlap(
                    &self.time[blk][ch],
                    &mut c.delay,
                    sub.state.gain[blk][ch],
                    &mut sub.pcm[ch][blk * 256..blk * 256 + 256],
                );
            }
        }
        let (locs, n) = locations(h.acmod, h.lfeon);
        sub.order = std::array::from_fn(|i| chans.get(i).copied().unwrap_or(0));
        sub.n = n;
        sub.locs = [0; 16];
        match h.chanmap.filter(|_| slot > 0) {
            Some(map) => {
                sub.locs = chanmap_locations(map, n).ok_or_else(|| {
                    Error::Invalid("E-AC-3 channel map does not match its channels".into())
                })?;
            }
            None => sub.locs[..6].copy_from_slice(&locs),
        }
        sub.samples = samples;
        sub.live = true;
        Ok(())
    }

    fn render(&mut self, out: &mut Vec<f32>) {
        let Some(main) = self.subs.first() else {
            return;
        };
        let samples = main.samples;
        let mut sources: Vec<(u8, usize, usize)> = (0..main.n)
            .map(|i| (main.locs[i], 0, main.order[i]))
            .collect();
        for (k, sub) in self.subs.iter().enumerate().skip(1) {
            if !sub.live || sub.samples != samples {
                continue;
            }
            for i in 0..sub.n.min(6) {
                let src = (sub.locs[i], k, sub.order[i]);
                match sources.iter_mut().find(|s| s.0 == src.0) {
                    Some(s) => *s = src,
                    None => sources.push(src),
                }
            }
        }
        let only_center = sources.iter().all(|s| s.0 == 1 || s.0 == 15);
        for s in self.slots.iter_mut() {
            s[..samples].fill(0.0);
        }
        for &(loc, k, ch) in &sources {
            let pcm = &self.subs[k].pcm[ch];
            for &(slot, g) in slots(loc) {
                for (o, &v) in self.slots[slot][..samples].iter_mut().zip(&pcm[..samples]) {
                    *o += v * g;
                }
            }
        }
        let s = &self.slots;
        let n = self.channels as usize;
        let base = out.len();
        out.resize(base + samples * n, 0.0);
        let dst = &mut out[base..];
        match n {
            8 => {
                for (i, f) in dst.chunks_exact_mut(8).enumerate() {
                    for (c, v) in f.iter_mut().enumerate() {
                        *v = s[c][i];
                    }
                }
            }
            6 => {
                for (i, f) in dst.chunks_exact_mut(6).enumerate() {
                    f[0] = s[0][i];
                    f[1] = s[1][i];
                    f[2] = s[2][i];
                    f[3] = s[3][i];
                    f[4] = s[SL][i] + K * s[BL][i];
                    f[5] = s[SR][i] + K * s[BR][i];
                }
            }
            _ if only_center => {
                for (i, f) in dst.chunks_exact_mut(n).enumerate() {
                    f.fill(s[2][i]);
                }
            }
            2 => {
                for (i, f) in dst.chunks_exact_mut(2).enumerate() {
                    let c = K * s[2][i];
                    f[0] = s[0][i] + c + K * (s[SL][i] + s[BL][i]);
                    f[1] = s[1][i] + c + K * (s[SR][i] + s[BR][i]);
                }
            }
            _ => {
                for (i, v) in dst.iter_mut().enumerate() {
                    let c = K * s[2][i];
                    let l = s[0][i] + c + K * (s[SL][i] + s[BL][i]);
                    let r = s[1][i] + c + K * (s[SR][i] + s[BR][i]);
                    *v = 0.5 * (l + r);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
