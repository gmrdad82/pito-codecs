use crate::aac::bits::BitReader;

use crate::aac::config::{AudioSpecificConfig, ElementType, ProgramConfig};
use crate::aac::huffman::{self, INTENSITY_HCB, INTENSITY_HCB2, NOISE_HCB, ZERO_HCB};
use crate::aac::ics::{IcsInfo, WindowSequence};
use crate::aac::mdct::{WindowShape, mdct_long, mdct_short, window};
use crate::aac::tables::{pow43, sf_gain};
use crate::aac::tns::{self, TnsData};
use crate::media::{Error, Result};

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

const EXT_SBR_DATA: u32 = 13;
const EXT_SBR_DATA_CRC: u32 = 14;

struct Ics {
    info: IcsInfo,
    band_type: [[u8; 64]; 8],
    sf: [[i32; 64]; 8],
    tns: Option<TnsData>,
    spec: Vec<f32>,
}

#[derive(Clone)]
struct ChannelState {
    overlap: Vec<f32>,
    prev_shape: WindowShape,
}

pub struct Decoder {
    asc: AudioSpecificConfig,
    layout: Vec<(ElementType, u8)>,
    offsets: Vec<usize>,
    channels: Vec<ChannelState>,
    noise_state: u32,
    sbr_seen: bool,
}

impl Decoder {
    pub fn from_config(asc: AudioSpecificConfig) -> Result<Decoder> {
        if asc.object_type != 2 {
            return Err(Error::Unsupported(
                "audio object type other than AAC LC".into(),
            ));
        }
        let layout = asc.layout();
        if layout.is_empty() && asc.channel_config != 0 {
            return Err(Error::Invalid("unsupported channel configuration".into()));
        }
        let mut d = Decoder {
            asc,
            layout: Vec::new(),
            offsets: Vec::new(),
            channels: Vec::new(),
            noise_state: 0x2545_F491,
            sbr_seen: false,
        };
        d.set_layout(layout);
        Ok(d)
    }

    fn set_layout(&mut self, layout: Vec<(ElementType, u8)>) {
        let mut offsets = Vec::with_capacity(layout.len());
        let mut n = 0;
        for e in &layout {
            offsets.push(n);
            n += e.0.channels();
        }
        self.layout = layout;
        self.offsets = offsets;
        self.channels = vec![
            ChannelState {
                overlap: vec![0.0; 1024],
                prev_shape: WindowShape::Sine
            };
            n
        ];
    }

    pub fn sample_rate(&self) -> u32 {
        self.asc.sample_rate
    }
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    pub fn reset(&mut self) {
        for c in &mut self.channels {
            c.overlap.fill(0.0);
            c.prev_shape = WindowShape::Sine;
        }
    }

    fn sf_index(&self) -> u8 {
        self.asc.sf_index
    }

    pub fn decode(&mut self, au: &[u8]) -> Result<Vec<Vec<f32>>> {
        self.decode_block(au, true)
    }

    fn decode_block(&mut self, au: &[u8], allow_reconfig: bool) -> Result<Vec<Vec<f32>>> {
        let mut br = BitReader::new(au);
        let nch = self.channels.len();
        let mut saw_element = false;
        let mut out: Vec<Option<Vec<f32>>> = vec![None; nch];
        let mut used = vec![false; self.layout.len()];
        loop {
            let id = br.read_bits(3)?;
            saw_element |= matches!(id, ID_SCE | ID_CPE | ID_LFE);
            match id {
                ID_SCE | ID_LFE => {
                    let tag = br.read_bits(4)? as u8;
                    let ty = if id == ID_SCE {
                        ElementType::Sce
                    } else {
                        ElementType::Lfe
                    };
                    let slot = self.find_slot(ty, tag, &used)?;
                    used[slot] = true;
                    let mut ics = self.decode_ics(&mut br, None)?;
                    let ch = self.offsets[slot];
                    self.fill_noise(&mut ics, None);
                    out[ch] = Some(self.finish_channel(ch, &mut ics));
                }
                ID_CPE => {
                    let tag = br.read_bits(4)? as u8;
                    let slot = self.find_slot(ElementType::Cpe, tag, &used)?;
                    used[slot] = true;
                    let ch = self.offsets[slot];
                    let [a, b] = self.decode_cpe(&mut br, ch)?;
                    out[ch] = Some(a);
                    out[ch + 1] = Some(b);
                }
                ID_CCE => return Err(Error::Unsupported("coupling channel element".into())),
                ID_DSE => {
                    br.skip(4)?;
                    let align = br.read_bits(1)? == 1;
                    let mut count = br.read_bits(8)? as usize;
                    if count == 255 {
                        count += br.read_bits(8)? as usize;
                    }
                    if align {
                        br.byte_align();
                    }
                    br.skip(count * 8)?;
                }
                ID_PCE => {
                    let pce = ProgramConfig::parse(&mut br)?;
                    if allow_reconfig
                        && !saw_element
                        && self.asc.channel_config == 0
                        && self.asc.pce.as_ref() != Some(&pce)
                    {
                        let layout = pce.layout();
                        if !layout.is_empty()
                            && layout.iter().map(|e| e.0.channels()).sum::<usize>() <= 64
                        {
                            self.asc.pce = Some(pce);
                            self.set_layout(layout);
                            return self.decode_block(au, false);
                        }
                    }
                }
                ID_FIL => {
                    let mut count = br.read_bits(4)? as usize;
                    if count == 15 {
                        count += br.read_bits(8)? as usize;
                        count -= 1;
                    }
                    if count > 0 {
                        let ext = br.read_bits(4)?;
                        self.sbr_seen |= matches!(ext, EXT_SBR_DATA | EXT_SBR_DATA_CRC);
                        br.skip(count * 8 - 4)?;
                    }
                }
                _ => break,
            }
            if id == ID_END {
                break;
            }
        }
        if nch == 0 && !saw_element {
            return Err(Error::Invalid(
                "channel configuration 0 without a program config element".into(),
            ));
        }
        let mut res = Vec::with_capacity(nch);
        for o in out {
            res.push(o.ok_or(Error::Invalid(
                "access unit is missing a channel element".into(),
            ))?);
        }
        Ok(res)
    }

    fn find_slot(&self, ty: ElementType, tag: u8, used: &[bool]) -> Result<usize> {
        let compatible =
            |t: ElementType| t == ty || (ty != ElementType::Cpe && t != ElementType::Cpe);
        if let Some(i) = self
            .layout
            .iter()
            .enumerate()
            .position(|(i, &(t, g))| t == ty && g == tag && !used[i])
        {
            return Ok(i);
        }
        if let Some(i) = self
            .layout
            .iter()
            .enumerate()
            .position(|(i, &(t, _))| t == ty && !used[i])
        {
            return Ok(i);
        }
        self.layout
            .iter()
            .enumerate()
            .position(|(i, &(t, _))| compatible(t) && !used[i])
            .ok_or(Error::Invalid(
                "unexpected channel element for the channel configuration".into(),
            ))
    }

    fn decode_cpe(&mut self, br: &mut BitReader, ch: usize) -> Result<[Vec<f32>; 2]> {
        let common_window = br.read_bits(1)? == 1;
        let mut ms_mask_present = 0;
        let mut ms_used = [[false; 64]; 8];
        let mut info = None;
        if common_window {
            let i = IcsInfo::parse(br, self.sf_index())?;
            ms_mask_present = br.read_bits(2)?;
            if ms_mask_present == 3 {
                return Err(Error::Invalid("reserved ms_mask_present".into()));
            }
            if ms_mask_present == 1 {
                for g in 0..i.num_groups {
                    for sfb in 0..i.max_sfb {
                        ms_used[g][sfb] = br.read_bits(1)? == 1;
                    }
                }
            } else if ms_mask_present == 2 {
                ms_used = [[true; 64]; 8];
            }
            info = Some(i);
        }
        let mut l = self.decode_ics(br, info)?;
        let mut r = self.decode_ics(br, info)?;
        let ms = if ms_mask_present > 0 {
            Some(&ms_used)
        } else {
            None
        };
        self.fill_noise(&mut l, None);
        self.fill_noise(&mut r, if common_window { Some((&l, ms)) } else { None });
        if common_window {
            let swb = l.info.swb_offsets(self.sf_index());
            for g in 0..l.info.num_groups {
                let w0 = l.info.group_start(g);
                for sfb in 0..l.info.max_sfb {
                    let (bl, br_) = (l.band_type[g][sfb], r.band_type[g][sfb]);
                    let range = swb[sfb] as usize..swb[sfb + 1] as usize;
                    if br_ == INTENSITY_HCB || br_ == INTENSITY_HCB2 {
                        let mut c: f32 = if br_ == INTENSITY_HCB { 1.0 } else { -1.0 };
                        if ms_mask_present > 0 && ms_used[g][sfb] {
                            c = -c;
                        }
                        let scale =
                            c * (-0.25 * r.sf[g][sfb].clamp(-400, 400) as f64).exp2() as f32;
                        for w in w0..w0 + l.info.group_len[g] as usize {
                            for k in range.clone() {
                                r.spec[w * 128 + k] = l.spec[w * 128 + k] * scale;
                            }
                        }
                    } else if ms_mask_present > 0
                        && ms_used[g][sfb]
                        && bl < NOISE_HCB
                        && br_ < NOISE_HCB
                    {
                        for w in w0..w0 + l.info.group_len[g] as usize {
                            for k in range.clone() {
                                let (m, s) = (l.spec[w * 128 + k], r.spec[w * 128 + k]);
                                l.spec[w * 128 + k] = m + s;
                                r.spec[w * 128 + k] = m - s;
                            }
                        }
                    }
                }
            }
        }
        Ok([
            self.finish_channel(ch, &mut l),
            self.finish_channel(ch + 1, &mut r),
        ])
    }

    fn decode_ics(&mut self, br: &mut BitReader, common: Option<IcsInfo>) -> Result<Ics> {
        let sfi = self.sf_index();
        let global_gain = br.read_bits(8)? as i32;
        let info = match common {
            Some(i) => i,
            None => IcsInfo::parse(br, sfi)?,
        };
        let swb = info.swb_offsets(sfi);
        let num_swb = swb.len() - 1;
        let mut ics = Ics {
            info,
            band_type: [[0; 64]; 8],
            sf: [[0; 64]; 8],
            tns: None,
            spec: vec![0.0; 1024],
        };

        let (sect_bits, esc) = if info.is_short() { (3, 7) } else { (5, 31) };
        for g in 0..info.num_groups {
            let mut k = 0;
            while k < info.max_sfb {
                let cb = br.read_bits(4)? as u8;
                if cb == 12 {
                    return Err(Error::Invalid("reserved codebook 12".into()));
                }
                let mut len = 0usize;
                loop {
                    let inc = br.read_bits(sect_bits)?;
                    len += inc as usize;
                    if inc != esc {
                        break;
                    }
                }
                if k + len > info.max_sfb {
                    return Err(Error::Invalid("section exceeds max_sfb".into()));
                }
                if len == 0 && br.bits_left() == 0 {
                    return Err(Error::Invalid("unexpected end of access unit".into()));
                }
                for sfb in k..k + len {
                    ics.band_type[g][sfb] = cb;
                }
                k += len;
            }
        }

        let mut sf = global_gain;
        let mut noise = global_gain - 90;
        let mut is_pos = 0i32;
        let mut noise_first = true;
        for g in 0..info.num_groups {
            for sfb in 0..info.max_sfb {
                match ics.band_type[g][sfb] {
                    ZERO_HCB => ics.sf[g][sfb] = 0,
                    INTENSITY_HCB | INTENSITY_HCB2 => {
                        is_pos += huffman::decode_sf(br)?;
                        ics.sf[g][sfb] = is_pos;
                    }
                    NOISE_HCB => {
                        if noise_first {
                            noise += br.read_bits(9)? as i32 - 256;
                            noise_first = false;
                        } else {
                            noise += huffman::decode_sf(br)?;
                        }
                        ics.sf[g][sfb] = noise;
                    }
                    _ => {
                        sf += huffman::decode_sf(br)?;
                        if !(0..=255).contains(&sf) {
                            return Err(Error::Invalid("scalefactor out of range".into()));
                        }
                        ics.sf[g][sfb] = sf;
                    }
                }
            }
        }

        let mut pulses: Vec<(usize, i32)> = Vec::new();
        if br.read_bits(1)? == 1 {
            if info.is_short() {
                return Err(Error::Invalid("pulse data in short window".into()));
            }
            let n = br.read_bits(2)? as usize + 1;
            let start = br.read_bits(6)? as usize;
            if start >= num_swb {
                return Err(Error::Invalid("pulse_start_sfb out of range".into()));
            }
            let mut k = swb[start] as usize;
            for _ in 0..n {
                k += br.read_bits(5)? as usize;
                let amp = br.read_bits(4)? as i32;
                if k >= 1024 {
                    return Err(Error::Invalid("pulse position out of range".into()));
                }
                pulses.push((k, amp));
            }
        }
        if br.read_bits(1)? == 1 {
            ics.tns = Some(TnsData::parse(br, &info)?);
        }
        if br.read_bits(1)? == 1 {
            return Err(Error::Unsupported("gain control data (AAC SSR)".into()));
        }

        let mut q = vec![0i32; 1024];
        let mut vals = [0i32; 4];
        for g in 0..info.num_groups {
            let w0 = info.group_start(g);
            let glen = info.group_len[g] as usize;
            for sfb in 0..info.max_sfb {
                let cb = ics.band_type[g][sfb];
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let dim = huffman::book(cb).dim;
                let (lo, hi) = (swb[sfb] as usize, swb[sfb + 1] as usize);
                for w in w0..w0 + glen {
                    let base = w * 128;
                    let mut k = lo;
                    while k < hi {
                        huffman::decode_spectral(br, cb, &mut vals)?;
                        q[base + k..base + k + dim].copy_from_slice(&vals[..dim]);
                        k += dim;
                    }
                }
            }
        }
        for (k, amp) in pulses {
            if q[k] > 0 {
                q[k] += amp;
            } else {
                q[k] -= amp;
            }
        }

        for g in 0..info.num_groups {
            let w0 = info.group_start(g);
            for sfb in 0..info.max_sfb {
                let cb = ics.band_type[g][sfb];
                if cb == ZERO_HCB || cb >= NOISE_HCB {
                    continue;
                }
                let gain = sf_gain(ics.sf[g][sfb]);
                for w in w0..w0 + info.group_len[g] as usize {
                    for k in swb[sfb] as usize..swb[sfb + 1] as usize {
                        let v = q[w * 128 + k];
                        let m = pow43(v.unsigned_abs()) * gain;
                        ics.spec[w * 128 + k] = if v < 0 { -m } else { m };
                    }
                }
            }
        }
        Ok(ics)
    }

    fn next_random(&mut self) -> f32 {
        self.noise_state = self
            .noise_state
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        (self.noise_state as i32) as f32
    }

    fn fill_noise(&mut self, ics: &mut Ics, pair: Option<(&Ics, Option<&[[bool; 64]; 8]>)>) {
        let info = ics.info;
        let swb = info.swb_offsets(self.sf_index());
        for g in 0..info.num_groups {
            let w0 = info.group_start(g);
            for sfb in 0..info.max_sfb {
                if ics.band_type[g][sfb] != NOISE_HCB {
                    continue;
                }
                let (lo, hi) = (swb[sfb] as usize, swb[sfb + 1] as usize);
                let target = (0.25 * ics.sf[g][sfb].clamp(-400, 400) as f64).exp2();
                let correlated = match pair {
                    Some((l, Some(ms))) => ms[g][sfb] && l.band_type[g][sfb] == NOISE_HCB,
                    _ => false,
                };
                for w in w0..w0 + info.group_len[g] as usize {
                    let band = w * 128 + lo..w * 128 + hi;
                    if correlated && let Some((l, _)) = pair {
                        ics.spec[band.clone()].copy_from_slice(&l.spec[band.clone()]);
                    } else {
                        for k in band.clone() {
                            ics.spec[k] = self.next_random();
                        }
                    }
                    let energy: f64 = ics.spec[band.clone()]
                        .iter()
                        .map(|&v| v as f64 * v as f64)
                        .sum();
                    let scale = if energy > 0.0 {
                        (target / energy.sqrt()) as f32
                    } else {
                        0.0
                    };
                    for k in band {
                        ics.spec[k] *= scale;
                    }
                }
            }
        }
    }

    fn finish_channel(&mut self, ch: usize, ics: &mut Ics) -> Vec<f32> {
        if let Some(t) = &ics.tns {
            tns::apply_decoder(&mut ics.spec, &ics.info, self.sf_index(), t);
        }
        synth(&mut self.channels[ch], &ics.info, &ics.spec)
    }
}

fn synth(state: &mut ChannelState, info: &IcsInfo, spec: &[f32]) -> Vec<f32> {
    let mut buf = vec![0f32; 2048];
    let prev = state.prev_shape;
    let cur = info.window_shape;
    let norm = 1.0 / 32768.0;
    match info.window_sequence {
        WindowSequence::EightShort => {
            let t = mdct_short();
            let mut y = vec![0f32; 256];
            let wc = window(cur, false);
            for w in 0..8 {
                t.inverse(&spec[w * 128..(w + 1) * 128], &mut y);
                let wl = window(if w == 0 { prev } else { cur }, false);
                let off = 448 + 128 * w;
                let s = norm / 128.0;
                for n in 0..128 {
                    buf[off + n] += y[n] * wl[n] * s;
                    buf[off + 128 + n] += y[128 + n] * wc[127 - n] * s;
                }
            }
        }
        seq => {
            let mut y = vec![0f32; 2048];
            mdct_long().inverse(&spec[..1024], &mut y);
            let s = norm / 1024.0;
            match seq {
                WindowSequence::LongStop => {
                    let ws = window(prev, false);
                    for n in 448..576 {
                        buf[n] = y[n] * ws[n - 448] * s;
                    }
                    for n in 576..1024 {
                        buf[n] = y[n] * s;
                    }
                }
                _ => {
                    let wl = window(prev, true);
                    for n in 0..1024 {
                        buf[n] = y[n] * wl[n] * s;
                    }
                }
            }
            match seq {
                WindowSequence::LongStart => {
                    let ws = window(cur, false);
                    for n in 1024..1472 {
                        buf[n] = y[n] * s;
                    }
                    for n in 1472..1600 {
                        buf[n] = y[n] * ws[127 - (n - 1472)] * s;
                    }
                }
                _ => {
                    let wl = window(cur, true);
                    for n in 1024..2048 {
                        buf[n] = y[n] * wl[2047 - n] * s;
                    }
                }
            }
        }
    }
    let out: Vec<f32> = (0..1024).map(|n| state.overlap[n] + buf[n]).collect();
    state.overlap.copy_from_slice(&buf[1024..]);
    state.prev_shape = cur;
    out
}
