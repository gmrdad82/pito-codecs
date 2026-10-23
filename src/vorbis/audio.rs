use std::cmp::Ordering;
use std::fmt;

use super::bitpacking::{BitpackCursor, EndOfPacket};
use super::header::{
    Codebook, Floor, FloorTypeOne, FloorTypeZero, HuffmanVqReadErr, IdentHeader, Mapping, Residue,
    SetupHeader,
};
use super::ilog;
use super::imdct::inverse_mdct;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioReadError {
    EndOfPacket,
    AudioBadFormat,
    AudioIsHeader,
}

impl From<EndOfPacket> for AudioReadError {
    fn from(_: EndOfPacket) -> AudioReadError {
        AudioReadError::EndOfPacket
    }
}

impl std::error::Error for AudioReadError {}

impl fmt::Display for AudioReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioReadError::EndOfPacket => write!(f, "Vorbis audio packet cut short"),
            AudioReadError::AudioBadFormat => write!(f, "malformed Vorbis audio packet"),
            AudioReadError::AudioIsHeader => {
                write!(f, "Vorbis header where an audio packet was expected")
            }
        }
    }
}

impl From<AudioReadError> for crate::media::Error {
    fn from(e: AudioReadError) -> crate::media::Error {
        crate::media::Error::Invalid(e.to_string())
    }
}

enum DecodedFloor<'a> {
    TypeZero(Vec<f32>, u64, &'a FloorTypeZero),
    TypeOne(Vec<u32>, &'a FloorTypeOne),
    Unused,
}

enum FloorSpecialCase {
    Unused,
    PacketUndecodable,
}

impl From<EndOfPacket> for FloorSpecialCase {
    fn from(_: EndOfPacket) -> Self {
        FloorSpecialCase::Unused
    }
}

impl From<HuffmanVqReadErr> for FloorSpecialCase {
    fn from(e: HuffmanVqReadErr) -> Self {
        match e {
            HuffmanVqReadErr::EndOfPacket => FloorSpecialCase::Unused,
            HuffmanVqReadErr::NoVqLookupForCodebook => FloorSpecialCase::PacketUndecodable,
        }
    }
}

fn floor_zero_decode(
    rdr: &mut BitpackCursor,
    codebooks: &[Codebook],
    fl: &FloorTypeZero,
) -> Result<(Vec<f32>, u64), FloorSpecialCase> {
    let amplitude = rdr.read_dyn_u64(fl.floor0_amplitude_bits)?;
    if amplitude == 0 {
        return Err(FloorSpecialCase::Unused);
    }
    let booknumber = rdr.read_dyn_u32(ilog(fl.floor0_number_of_books as u64))?;
    let codebook = fl
        .floor0_book_list
        .get(booknumber as usize)
        .and_then(|&i| codebooks.get(i as usize))
        .ok_or(FloorSpecialCase::PacketUndecodable)?;
    if codebook.codebook_dimensions == 0 {
        return Err(FloorSpecialCase::PacketUndecodable);
    }
    let order = fl.floor0_order as usize;
    let mut coefficients = Vec::with_capacity(order);
    let mut last = 0.0;
    loop {
        let mut last_new = last;
        for &e in rdr.read_huffman_vq(codebook)? {
            coefficients.push((last + e).cos());
            last_new = e;
            if coefficients.len() == order {
                return Ok((coefficients, amplitude));
            }
        }
        last += last_new;
    }
}

fn floor_zero_compute_curve(
    cos_coefficients: &[f32],
    amplitude: u64,
    fl: &FloorTypeZero,
    blockflag: bool,
    n: usize,
) -> Vec<f32> {
    let bark = &fl.cached_bark_cos_omega[blockflag as usize];
    let full = 1u64
        .checked_shl(fl.floor0_amplitude_bits as u32)
        .map_or(u64::MAX, |v| v - 1);
    let lfv_common_term = amplitude as f32 * fl.floor0_amplitude_offset as f32 / full as f32;
    let odd = fl.floor0_order & 1 == 1;
    let mut output = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let cos_omega = bark.get(i).copied().unwrap_or(0.0);
        let (mut p, mut q) = if odd {
            (1.0 - cos_omega * cos_omega, 0.25)
        } else {
            ((1.0 - cos_omega) / 2.0, (1.0 + cos_omega) / 2.0)
        };
        for c in cos_coefficients.iter().skip(1).step_by(2) {
            let pm = c - cos_omega;
            p *= 4.0 * pm * pm;
        }
        for c in cos_coefficients.iter().step_by(2) {
            let qm = c - cos_omega;
            q *= 4.0 * qm * qm;
        }
        let linear_floor_value = (0.115_129_25
            * (lfv_common_term / (p + q).sqrt() - fl.floor0_amplitude_offset as f32))
            .exp();
        loop {
            output.push(linear_floor_value);
            i += 1;
            if i >= n || bark.get(i).copied() != Some(cos_omega) {
                break;
            }
        }
    }
    output
}

fn floor_one_decode(
    rdr: &mut BitpackCursor,
    codebooks: &[Codebook],
    fl: &FloorTypeOne,
) -> Result<Vec<u32>, FloorSpecialCase> {
    if !rdr.read_bit_flag()? {
        return Err(FloorSpecialCase::Unused);
    }
    let range = RANGES[(fl.floor1_multiplier as usize).clamp(1, 4) - 1];
    let b = ilog(range as u64 - 1);
    let mut floor1_y = Vec::with_capacity(fl.floor1_x_list.len());
    floor1_y.push(rdr.read_dyn_u8(b)? as u32);
    floor1_y.push(rdr.read_dyn_u8(b)? as u32);
    let book = |i: usize| {
        codebooks
            .get(i)
            .map(|c| &c.codebook_huffman_tree)
            .ok_or(FloorSpecialCase::PacketUndecodable)
    };
    for &class in &fl.floor1_partition_class {
        let class = class as usize;
        let (Some(&cdim), Some(&cbits), Some(subs)) = (
            fl.floor1_class_dimensions.get(class),
            fl.floor1_class_subclasses.get(class),
            fl.floor1_subclass_books.get(class),
        ) else {
            return Err(FloorSpecialCase::PacketUndecodable);
        };
        let csub = (1u32 << cbits) - 1;
        let mut cval = 0;
        if cbits > 0 {
            let master = fl
                .floor1_class_masterbooks
                .get(class)
                .ok_or(FloorSpecialCase::PacketUndecodable)?;
            cval = rdr.read_huffman(book(*master as usize)?)?;
        }
        for _ in 0..cdim {
            let sub = subs
                .get((cval & csub) as usize)
                .copied()
                .ok_or(FloorSpecialCase::PacketUndecodable)?;
            cval >>= cbits;
            if sub >= 0 {
                floor1_y.push(rdr.read_huffman(book(sub as usize)?)?);
            } else {
                floor1_y.push(0);
            }
        }
    }
    Ok(floor1_y)
}

const RANGES: [i32; 4] = [256, 128, 86, 64];

fn extr_neighbor(v: &[u32], max_idx: usize, cmp: fn(u32, u32) -> Ordering) -> Option<(usize, u32)> {
    let bound = *v.get(max_idx)?;
    let prefix = &v[..max_idx];
    let smaller = |a: u32, b: u32| cmp(a, b) == Ordering::Less;
    let min_idx = prefix.iter().position(|&val| smaller(val, bound))?;
    let (offset, best) = prefix[min_idx..]
        .iter()
        .copied()
        .enumerate()
        .rev()
        .filter(|&(_, val)| smaller(val, bound))
        .max_by(|&(_, a), &(_, b)| cmp(a, b))
        .unwrap_or((0, v[min_idx]));
    Some((min_idx + offset, best))
}

fn low_neighbor(v: &[u32], x: usize) -> Option<(usize, u32)> {
    extr_neighbor(v, x, |a, b| a.cmp(&b))
}

fn high_neighbor(v: &[u32], x: usize) -> Option<(usize, u32)> {
    extr_neighbor(v, x, |a, b| b.cmp(&a))
}

fn render_point(x0: u32, y0: i32, x1: u32, y1: i32, x: u32) -> i32 {
    let dy = y1 as i64 - y0 as i64;
    let adx = (x1 as i64 - x0 as i64).max(1);
    let off = dy.abs() * (x as i64 - x0 as i64) / adx;
    (if dy < 0 {
        y0 as i64 - off
    } else {
        y0 as i64 + off
    }) as i32
}

fn floor_one_curve_compute_amplitude(
    floor1_y: &[u32],
    fl: &FloorTypeOne,
) -> Option<(Vec<i32>, Vec<bool>)> {
    let range = RANGES[(fl.floor1_multiplier as usize).clamp(1, 4) - 1];
    let mut step2 = vec![true, true];
    let mut final_y = vec![
        (*floor1_y.first()? as i32).min(range - 1),
        (*floor1_y.get(1)? as i32).min(range - 1),
    ];
    for (i, &x) in fl.floor1_x_list.iter().enumerate().skip(2) {
        let lo = low_neighbor(&fl.floor1_x_list, i)?;
        let hi = high_neighbor(&fl.floor1_x_list, i)?;
        let predicted = render_point(lo.1, final_y[lo.0], hi.1, final_y[hi.0], x);
        let val = (*floor1_y.get(i)?).min(i32::MAX as u32 / 4) as i32;
        let highroom = range - predicted;
        let lowroom = predicted;
        let room = highroom.min(lowroom) * 2;
        if val > 0 {
            step2[lo.0] = true;
            step2[hi.0] = true;
            step2.push(true);
            let y = if val >= room {
                if highroom > lowroom {
                    predicted + val - lowroom
                } else {
                    predicted - val + highroom - 1
                }
            } else {
                predicted + (if val % 2 == 1 { -val - 1 } else { val } >> 1)
            };
            final_y.push(y.clamp(0, range - 1));
        } else {
            final_y.push(predicted.clamp(0, range - 1));
            step2.push(false);
        }
    }
    Some((final_y, step2))
}

fn render_line(x0: u32, y0: i32, x1: u32, y1: i32, v: &mut Vec<u32>) {
    let dy = y1 - y0;
    let adx = (x1 as i32 - x0 as i32).max(1);
    let base = dy / adx;
    let mut y = y0;
    let mut err = 0;
    let sy = base + if dy < 0 { -1 } else { 1 };
    let ady = dy.abs() - base.abs() * adx;
    v.push(y as u32);
    for _ in x0 + 1..x1 {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        v.push(y as u32);
    }
}

fn floor_one_curve_synthesis(
    final_y: &[i32],
    step2: &[bool],
    fl: &FloorTypeOne,
    n: usize,
    out: &mut Vec<f32>,
) {
    let mult = fl.floor1_multiplier as i32;
    let mut floor = Vec::with_capacity(n);
    let (mut hx, mut lx, mut hy) = (0u32, 0u32, 0i32);
    let mut ly = fl
        .floor1_x_list_sorted
        .first()
        .and_then(|s| final_y.get(s.0))
        .map_or(0, |y| y * mult);
    for &(idx, x) in fl.floor1_x_list_sorted.iter().skip(1) {
        if step2.get(idx).copied().unwrap_or(false) {
            hy = final_y.get(idx).copied().unwrap_or(0) * mult;
            hx = x;
            render_line(lx, ly, hx, hy, &mut floor);
            lx = hx;
            ly = hy;
        }
    }
    if (hx as usize) < n {
        render_line(hx, hy, n as u32, hy, &mut floor);
    }
    floor.resize(n, 0);
    out.clear();
    out.extend(
        floor
            .iter()
            .map(|&i| FLOOR1_INVERSE_DB_TABLE[(i as usize).min(255)]),
    );
}

fn residue_packet_read_partition(
    rdr: &mut BitpackCursor,
    codebook: &Codebook,
    resid: &Residue,
    vec_v: &mut [f32],
) -> Result<(), HuffmanVqReadErr> {
    let psize = resid.residue_partition_size as usize;
    if resid.residue_type == 0 {
        let dims = (codebook.codebook_dimensions as usize).max(1);
        let step = psize / dims;
        for i in 0..step {
            let entry = rdr.read_huffman_vq(codebook)?;
            for (j, e) in entry.iter().enumerate() {
                if let Some(v) = vec_v.get_mut(i + j * step) {
                    *v += *e;
                }
            }
        }
    } else {
        let mut i = 0;
        while i < psize {
            let entries = rdr.read_huffman_vq(codebook)?;
            let Some(vs) = vec_v.get_mut(i..i + entries.len()) else {
                break;
            };
            if entries.is_empty() {
                break;
            }
            for (v, e) in vs.iter_mut().zip(entries) {
                *v += *e;
            }
            i += entries.len();
        }
    }
    Ok(())
}

fn residue_packet_decode_inner(
    rdr: &mut BitpackCursor,
    actual_size: usize,
    do_not_decode: &[bool],
    resid: &Residue,
    codebooks: &[Codebook],
) -> Result<Vec<f32>, AudioReadError> {
    let ch = do_not_decode.len();
    let begin = (resid.residue_begin as usize).min(actual_size);
    let end = (resid.residue_end as usize).min(actual_size);
    let classbook = codebooks
        .get(resid.residue_classbook as usize)
        .ok_or(AudioReadError::AudioBadFormat)?;
    let cpc = classbook.codebook_dimensions as usize;
    let psize = resid.residue_partition_size as usize;
    let n_to_read = end - begin;
    let partitions = n_to_read / psize.max(1);
    let mut vectors = vec![0.0; ch * actual_size];
    if n_to_read == 0 || partitions == 0 {
        return Ok(vectors);
    }
    if cpc == 0 {
        return Err(AudioReadError::AudioBadFormat);
    }
    let classes = resid.residue_classifications as u32;
    let stride = partitions + cpc;
    let mut classifications = vec![0u32; ch * stride];
    for pass in 0..8u8 {
        let mut partition = 0;
        while partition < partitions {
            if pass == 0 {
                for (j, &skip) in do_not_decode.iter().enumerate() {
                    if skip {
                        continue;
                    }
                    let Ok(mut temp) = rdr.read_huffman(&classbook.codebook_huffman_tree) else {
                        return Ok(vectors);
                    };
                    for i in (0..cpc).rev() {
                        classifications[j * stride + i + partition] = temp % classes;
                        temp /= classes;
                    }
                }
            }
            for _ in 0..cpc {
                if partition >= partitions {
                    break;
                }
                for (j, &skip) in do_not_decode.iter().enumerate() {
                    if skip {
                        continue;
                    }
                    let offs = begin + partition * psize;
                    let vqclass = classifications[j * stride + partition] as usize;
                    let Some(book) = resid
                        .residue_books
                        .get(vqclass)
                        .and_then(|b| b.get_val(pass))
                    else {
                        continue;
                    };
                    let codebook = codebooks
                        .get(book as usize)
                        .ok_or(AudioReadError::AudioBadFormat)?;
                    let v = &mut vectors[j * actual_size + offs..(j + 1) * actual_size];
                    match residue_packet_read_partition(rdr, codebook, resid, v) {
                        Ok(()) => {}
                        Err(HuffmanVqReadErr::EndOfPacket) => return Ok(vectors),
                        Err(HuffmanVqReadErr::NoVqLookupForCodebook) => {
                            return Err(AudioReadError::AudioBadFormat);
                        }
                    }
                }
                partition += 1;
            }
        }
    }
    Ok(vectors)
}

fn residue_packet_decode(
    rdr: &mut BitpackCursor,
    half: usize,
    do_not_decode: &[bool],
    resid: &Residue,
    codebooks: &[Codebook],
) -> Result<Vec<f32>, AudioReadError> {
    let ch = do_not_decode.len();
    if resid.residue_type != 2 {
        return residue_packet_decode_inner(rdr, half, do_not_decode, resid, codebooks);
    }
    if do_not_decode.iter().all(|&d| d) {
        return Ok(vec![0.0; ch * half]);
    }
    let vectors = residue_packet_decode_inner(rdr, half * ch, &[false], resid, codebooks)?;
    let mut out = Vec::with_capacity(ch * half);
    for j in 0..ch {
        out.extend(vectors.chunks_exact(ch).map(|c| c[j]));
    }
    Ok(out)
}

fn inverse_couple(m: f32, a: f32) -> (f32, f32) {
    if m > 0. {
        if a > 0. { (m, m - a) } else { (m + a, m) }
    } else if a > 0. {
        (m, m + a)
    } else {
        (m - a, m)
    }
}

fn pair_mut<T>(v: &mut [T], a: usize, b: usize) -> Option<(&mut T, &mut T)> {
    match a.cmp(&b) {
        Ordering::Less => {
            let (lo, hi) = v.split_at_mut_checked(b)?;
            Some((lo.get_mut(a)?, hi.first_mut()?))
        }
        Ordering::Greater => {
            let (lo, hi) = v.split_at_mut_checked(a)?;
            Some((hi.first_mut()?, lo.get_mut(b)?))
        }
        Ordering::Equal => None,
    }
}

const ORDER: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[0, 2, 1],
    &[0, 1, 2, 3],
    &[0, 2, 1, 3, 4],
    &[0, 2, 1, 5, 3, 4],
    &[0, 2, 1, 6, 5, 3, 4],
    &[0, 2, 1, 7, 5, 6, 3, 4],
];

fn interleave(chans: &[Vec<f32>], out: &mut Vec<f32>) -> usize {
    let frames = chans.first().map_or(0, |c| c.len());
    out.reserve(frames * chans.len());
    match ORDER.get(chans.len().wrapping_sub(1)) {
        Some(idx) => (0..frames).for_each(|i| out.extend(idx.iter().map(|&c| chans[c][i]))),
        None => (0..frames).for_each(|i| out.extend(chans.iter().map(|c| c[i]))),
    }
    frames
}

pub struct Decoder {
    pub ident: IdentHeader,
    pub setup: SetupHeader,
    prev: Option<Vec<Vec<f32>>>,
    scratch: Vec<f32>,
}

impl Decoder {
    pub fn new(ident: IdentHeader, setup: SetupHeader) -> Decoder {
        Decoder {
            ident,
            setup,
            prev: None,
            scratch: Vec::new(),
        }
    }

    pub fn from_headers(
        ident: &[u8],
        comment: &[u8],
        setup: &[u8],
    ) -> crate::media::Result<Decoder> {
        let (i, _, s) = super::read_header_set(ident, comment, setup)?;
        Ok(Decoder::new(i, s))
    }

    pub fn rate(&self) -> u32 {
        self.ident.audio_sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.ident.audio_channels as u16
    }

    pub fn reset(&mut self) {
        self.prev = None;
    }

    pub(crate) fn shape(
        &self,
        packet: &[u8],
    ) -> Result<(usize, Option<(bool, bool)>), AudioReadError> {
        let mut rdr = BitpackCursor::new(packet);
        if rdr.read_bit_flag()? {
            return Err(AudioReadError::AudioIsHeader);
        }
        let bits = ilog(self.setup.modes.len().saturating_sub(1) as u64);
        let mode = self
            .setup
            .modes
            .get(rdr.read_dyn_u8(bits)? as usize)
            .ok_or(AudioReadError::AudioBadFormat)?;
        if !mode.mode_blockflag {
            return Ok((1 << self.ident.blocksize_0, None));
        }
        let flags = (rdr.read_bit_flag()?, rdr.read_bit_flag()?);
        Ok((1 << self.ident.blocksize_1, Some(flags)))
    }

    pub fn blocksize(&self, packet: &[u8]) -> Result<usize, AudioReadError> {
        Ok(self.shape(packet)?.0)
    }

    pub fn primed(&self) -> bool {
        self.prev.is_some()
    }

    pub fn lead_in(&self, packet: &[u8]) -> Result<usize, AudioReadError> {
        let (n, flags) = self.shape(packet)?;
        let prev = match flags {
            Some((false, _)) => 1usize << self.ident.blocksize_0,
            _ => n,
        };
        Ok(prev / 4 + n / 4)
    }

    pub fn decode_aligned(
        &mut self,
        packet: &[u8],
        out: &mut Vec<f32>,
    ) -> Result<usize, AudioReadError> {
        let mut lead = 0;
        if !self.primed() && !packet.is_empty() {
            lead = self.lead_in(packet)?;
            out.resize(out.len() + lead * self.channels() as usize, 0.0);
        }
        Ok(lead + self.decode_into(packet, out)?)
    }

    pub fn decode_into(
        &mut self,
        packet: &[u8],
        out: &mut Vec<f32>,
    ) -> Result<usize, AudioReadError> {
        if packet.is_empty() {
            return Ok(0);
        }
        let chans = self.decode_packet(packet)?;
        Ok(interleave(&chans, out))
    }

    fn decode_packet(&mut self, packet: &[u8]) -> Result<Vec<Vec<f32>>, AudioReadError> {
        let ident = &self.ident;
        let setup = &self.setup;
        let mut rdr = BitpackCursor::new(packet);
        if rdr.read_bit_flag()? {
            return Err(AudioReadError::AudioIsHeader);
        }
        let mode_bits = ilog(setup.modes.len().saturating_sub(1) as u64);
        let mode = setup
            .modes
            .get(rdr.read_dyn_u8(mode_bits)? as usize)
            .ok_or(AudioReadError::AudioBadFormat)?;
        let mapping = setup
            .mappings
            .get(mode.mode_mapping as usize)
            .ok_or(AudioReadError::AudioBadFormat)?;
        let bs = if mode.mode_blockflag {
            ident.blocksize_1
        } else {
            ident.blocksize_0
        };
        let n = 1usize << bs;
        let half = n / 2;
        let flags = if mode.mode_blockflag {
            Some((rdr.read_bit_flag()?, rdr.read_bit_flag()?))
        } else {
            None
        };
        let channels = ident.audio_channels as usize;

        let floors = floor_decode(&mut rdr, channels, mapping, setup)?;
        let mut no_residue: Vec<bool> = floors
            .iter()
            .map(|f| matches!(f, DecodedFloor::Unused))
            .collect();
        for (&mag, &angle) in mapping
            .mapping_magnitudes
            .iter()
            .zip(&mapping.mapping_angles)
        {
            let (m, a) = (mag as usize, angle as usize);
            if m >= channels || a >= channels {
                return Err(AudioReadError::AudioBadFormat);
            }
            if !(no_residue[m] && no_residue[a]) {
                no_residue[m] = false;
                no_residue[a] = false;
            }
        }

        let mut residues = vec![Vec::new(); channels];
        for (i, &residue_number) in mapping.mapping_submap_residues.iter().enumerate() {
            let flags_of: Vec<bool> = mapping
                .mapping_mux
                .iter()
                .zip(&no_residue)
                .filter(|(m, _)| **m as usize == i)
                .map(|(_, &d)| d)
                .collect();
            let resid = setup
                .residues
                .get(residue_number as usize)
                .ok_or(AudioReadError::AudioBadFormat)?;
            let vectors =
                residue_packet_decode(&mut rdr, half, &flags_of, resid, &setup.codebooks)?;
            let mut k = 0;
            for (j, &mux) in mapping.mapping_mux.iter().enumerate() {
                if mux as usize == i {
                    if let (Some(r), Some(v)) =
                        (residues.get_mut(j), vectors.get(half * k..half * (k + 1)))
                    {
                        r.clear();
                        r.extend_from_slice(v);
                    }
                    k += 1;
                }
            }
        }
        for r in &mut residues {
            r.resize(half, 0.0);
        }

        for (&mag, &angle) in mapping
            .mapping_magnitudes
            .iter()
            .rev()
            .zip(mapping.mapping_angles.iter().rev())
        {
            let (m, a) = pair_mut(&mut residues, mag as usize, angle as usize)
                .ok_or(AudioReadError::AudioBadFormat)?;
            for (m, a) in m.iter_mut().zip(a.iter_mut()) {
                (*m, *a) = inverse_couple(*m, *a);
            }
        }

        let mut spectra = Vec::with_capacity(channels);
        for (residue, floor) in residues.iter().zip(&floors) {
            let mut curve = match floor {
                DecodedFloor::TypeZero(coefficients, amplitude, fl) => floor_zero_compute_curve(
                    coefficients,
                    *amplitude,
                    fl,
                    mode.mode_blockflag,
                    half,
                ),
                DecodedFloor::TypeOne(y, fl) => {
                    let (final_y, step2) = floor_one_curve_compute_amplitude(y, fl)
                        .ok_or(AudioReadError::AudioBadFormat)?;
                    let mut c = Vec::with_capacity(n);
                    floor_one_curve_synthesis(&final_y, &step2, fl, half, &mut c);
                    c
                }
                DecodedFloor::Unused => vec![0.0; half],
            };
            curve.resize(half, 0.0);
            for (f, r) in curve.iter_mut().zip(residue) {
                *f *= *r;
            }
            curve.resize(n, 0.0);
            let cached = &ident.cached_bs_derived[mode.mode_blockflag as usize];
            inverse_mdct(cached, &mut curve, &mut self.scratch, bs);
            spectra.push(curve);
        }

        let center = n >> 1;
        let short = 1usize << ident.blocksize_0;
        let (left_start, left_long) = if flags.is_none_or(|(p, _)| p) {
            (0, mode.mode_blockflag)
        } else {
            ((n - short) >> 2, false)
        };
        let (right_start, right_end) = if flags.is_none_or(|(_, nx)| nx) {
            (center, n)
        } else {
            ((n * 3 - short) >> 2, (n * 3 + short) >> 2)
        };

        let mut next = Vec::with_capacity(channels);
        match self.prev.take() {
            Some(prev) => {
                let slope = &ident.cached_bs_derived[left_long as usize].window_slope;
                for (prev, chan) in prev.into_iter().zip(spectra.iter_mut()) {
                    let plen = prev.len();
                    let win = slope.get(..plen).ok_or(AudioReadError::AudioBadFormat)?;
                    let target = chan
                        .get_mut(left_start..left_start + plen)
                        .ok_or(AudioReadError::AudioBadFormat)?;
                    for ((v, (l, r)), p) in target
                        .iter_mut()
                        .zip(win.iter().zip(win.iter().rev()))
                        .zip(&prev)
                    {
                        *v = *v * l + p * r;
                    }
                    next.push(chan[right_start..right_end].to_vec());
                    chan.copy_within(left_start..right_start, 0);
                    chan.truncate(right_start - left_start);
                }
            }
            None => {
                for chan in spectra.iter_mut() {
                    next.push(chan[right_start..right_end].to_vec());
                    chan.copy_within(center..right_start, 0);
                    chan.truncate(right_start - center);
                }
            }
        }
        self.prev = Some(next);
        Ok(spectra)
    }
}

fn floor_decode<'a>(
    rdr: &mut BitpackCursor,
    channels: usize,
    mapping: &Mapping,
    setup: &'a SetupHeader,
) -> Result<Vec<DecodedFloor<'a>>, AudioReadError> {
    let mut out = Vec::with_capacity(channels);
    for i in 0..channels {
        let floor = mapping
            .mapping_mux
            .get(i)
            .and_then(|&s| mapping.mapping_submap_floors.get(s as usize))
            .and_then(|&f| setup.floors.get(f as usize))
            .ok_or(AudioReadError::AudioBadFormat)?;
        let res = match floor {
            Floor::TypeZero(fl) => match floor_zero_decode(rdr, &setup.codebooks, fl) {
                Ok((c, a)) => DecodedFloor::TypeZero(c, a, fl),
                Err(FloorSpecialCase::Unused) => DecodedFloor::Unused,
                Err(FloorSpecialCase::PacketUndecodable) => {
                    return Err(AudioReadError::AudioBadFormat);
                }
            },
            Floor::TypeOne(fl) => match floor_one_decode(rdr, &setup.codebooks, fl) {
                Ok(y) => DecodedFloor::TypeOne(y, fl),
                Err(FloorSpecialCase::Unused) => DecodedFloor::Unused,
                Err(FloorSpecialCase::PacketUndecodable) => {
                    return Err(AudioReadError::AudioBadFormat);
                }
            },
        };
        out.push(res);
    }
    Ok(out)
}

#[allow(clippy::excessive_precision)]
static FLOOR1_INVERSE_DB_TABLE: [f32; 256] = [
    1.0649863e-07,
    1.1341951e-07,
    1.2079015e-07,
    1.2863978e-07,
    1.3699951e-07,
    1.4590251e-07,
    1.5538408e-07,
    1.6548181e-07,
    1.7623575e-07,
    1.8768855e-07,
    1.9988561e-07,
    2.1287530e-07,
    2.2670913e-07,
    2.4144197e-07,
    2.5713223e-07,
    2.7384213e-07,
    2.9163793e-07,
    3.1059021e-07,
    3.3077411e-07,
    3.5226968e-07,
    3.7516214e-07,
    3.9954229e-07,
    4.2550680e-07,
    4.5315863e-07,
    4.8260743e-07,
    5.1396998e-07,
    5.4737065e-07,
    5.8294187e-07,
    6.2082472e-07,
    6.6116941e-07,
    7.0413592e-07,
    7.4989464e-07,
    7.9862701e-07,
    8.5052630e-07,
    9.0579828e-07,
    9.6466216e-07,
    1.0273513e-06,
    1.0941144e-06,
    1.1652161e-06,
    1.2409384e-06,
    1.3215816e-06,
    1.4074654e-06,
    1.4989305e-06,
    1.5963394e-06,
    1.7000785e-06,
    1.8105592e-06,
    1.9282195e-06,
    2.0535261e-06,
    2.1869758e-06,
    2.3290978e-06,
    2.4804557e-06,
    2.6416497e-06,
    2.8133190e-06,
    2.9961443e-06,
    3.1908506e-06,
    3.3982101e-06,
    3.6190449e-06,
    3.8542308e-06,
    4.1047004e-06,
    4.3714470e-06,
    4.6555282e-06,
    4.9580707e-06,
    5.2802740e-06,
    5.6234160e-06,
    5.9888572e-06,
    6.3780469e-06,
    6.7925283e-06,
    7.2339451e-06,
    7.7040476e-06,
    8.2047000e-06,
    8.7378876e-06,
    9.3057248e-06,
    9.9104632e-06,
    1.0554501e-05,
    1.1240392e-05,
    1.1970856e-05,
    1.2748789e-05,
    1.3577278e-05,
    1.4459606e-05,
    1.5399272e-05,
    1.6400004e-05,
    1.7465768e-05,
    1.8600792e-05,
    1.9809576e-05,
    2.1096914e-05,
    2.2467911e-05,
    2.3928002e-05,
    2.5482978e-05,
    2.7139006e-05,
    2.8902651e-05,
    3.0780908e-05,
    3.2781225e-05,
    3.4911534e-05,
    3.7180282e-05,
    3.9596466e-05,
    4.2169667e-05,
    4.4910090e-05,
    4.7828601e-05,
    5.0936773e-05,
    5.4246931e-05,
    5.7772202e-05,
    6.1526565e-05,
    6.5524908e-05,
    6.9783085e-05,
    7.4317983e-05,
    7.9147585e-05,
    8.4291040e-05,
    8.9768747e-05,
    9.5602426e-05,
    0.00010181521,
    0.00010843174,
    0.00011547824,
    0.00012298267,
    0.00013097477,
    0.00013948625,
    0.00014855085,
    0.00015820453,
    0.00016848555,
    0.00017943469,
    0.00019109536,
    0.00020351382,
    0.00021673929,
    0.00023082423,
    0.00024582449,
    0.00026179955,
    0.00027881276,
    0.00029693158,
    0.00031622787,
    0.00033677814,
    0.00035866388,
    0.00038197188,
    0.00040679456,
    0.00043323036,
    0.00046138411,
    0.00049136745,
    0.00052329927,
    0.00055730621,
    0.00059352311,
    0.00063209358,
    0.00067317058,
    0.00071691700,
    0.00076350630,
    0.00081312324,
    0.00086596457,
    0.00092223983,
    0.00098217216,
    0.0010459992,
    0.0011139742,
    0.0011863665,
    0.0012634633,
    0.0013455702,
    0.0014330129,
    0.0015261382,
    0.0016253153,
    0.0017309374,
    0.0018434235,
    0.0019632195,
    0.0020908006,
    0.0022266726,
    0.0023713743,
    0.0025254795,
    0.0026895994,
    0.0028643847,
    0.0030505286,
    0.0032487691,
    0.0034598925,
    0.0036847358,
    0.0039241906,
    0.0041792066,
    0.0044507950,
    0.0047400328,
    0.0050480668,
    0.0053761186,
    0.0057254891,
    0.0060975636,
    0.0064938176,
    0.0069158225,
    0.0073652516,
    0.0078438871,
    0.0083536271,
    0.0088964928,
    0.009474637,
    0.010090352,
    0.010746080,
    0.011444421,
    0.012188144,
    0.012980198,
    0.013823725,
    0.014722068,
    0.015678791,
    0.016697687,
    0.017782797,
    0.018938423,
    0.020169149,
    0.021479854,
    0.022875735,
    0.024362330,
    0.025945531,
    0.027631618,
    0.029427276,
    0.031339626,
    0.033376252,
    0.035545228,
    0.037855157,
    0.040315199,
    0.042935108,
    0.045725273,
    0.048696758,
    0.051861348,
    0.055231591,
    0.058820850,
    0.062643361,
    0.066714279,
    0.071049749,
    0.075666962,
    0.080584227,
    0.085821044,
    0.091398179,
    0.097337747,
    0.10366330,
    0.11039993,
    0.11757434,
    0.12521498,
    0.13335215,
    0.14201813,
    0.15124727,
    0.16107617,
    0.17154380,
    0.18269168,
    0.19456402,
    0.20720788,
    0.22067342,
    0.23501402,
    0.25028656,
    0.26655159,
    0.28387361,
    0.30232132,
    0.32196786,
    0.34289114,
    0.36517414,
    0.38890521,
    0.41417847,
    0.44109412,
    0.46975890,
    0.50028648,
    0.53279791,
    0.56742212,
    0.60429640,
    0.64356699,
    0.68538959,
    0.72993007,
    0.77736504,
    0.82788260,
    0.88168307,
    0.9389798,
    1.,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_and_high_neighbors_match_lewton() {
        let v = [1, 4, 2, 3, 6, 5];
        assert_eq!(low_neighbor(&v, 1), Some((0, 1)));
        assert_eq!(low_neighbor(&v, 2), Some((0, 1)));
        assert_eq!(low_neighbor(&v, 3), Some((2, 2)));
        assert_eq!(low_neighbor(&v, 4), Some((1, 4)));
        assert_eq!(low_neighbor(&v, 5), Some((1, 4)));
        assert_eq!(high_neighbor(&v, 2), Some((1, 4)));
        assert_eq!(high_neighbor(&v, 3), Some((1, 4)));
        assert_eq!(high_neighbor(&v, 5), Some((4, 6)));
        let v = [
            0, 128, 12, 46, 4, 8, 16, 23, 33, 70, 2, 6, 10, 14, 19, 28, 39, 58, 90,
        ];
        let want = [
            (1, 128),
            (1, 128),
            (2, 12),
            (2, 12),
            (3, 46),
            (3, 46),
            (3, 46),
            (1, 128),
            (4, 4),
            (5, 8),
            (2, 12),
            (6, 16),
            (7, 23),
            (8, 33),
            (3, 46),
            (9, 70),
            (1, 128),
        ];
        for (i, w) in want.iter().enumerate() {
            assert_eq!(high_neighbor(&v, i + 2), Some(*w), "{}", i + 2);
        }
    }

    #[test]
    fn neighbors_that_do_not_exist_are_none_not_a_panic() {
        assert_eq!(high_neighbor(&[1, 4, 3, 2, 6, 5], 4), None);
        assert_eq!(low_neighbor(&[2, 4, 3, 1, 6, 5], 3), None);
        assert_eq!(low_neighbor(&[2, 4], 0), None);
        assert_eq!(low_neighbor(&[2, 4], 9), None);
    }

    #[test]
    fn render_point_matches_lewton_on_real_file_values() {
        let cases = [
            ((0, 28, 128, 67, 12), 31),
            ((12, 38, 128, 67, 46), 46),
            ((0, 28, 12, 38, 4), 31),
            ((4, 33, 12, 38, 8), 35),
            ((12, 38, 46, 31, 16), 38),
            ((16, 30, 46, 31, 23), 30),
            ((23, 40, 46, 31, 33), 37),
            ((46, 31, 128, 67, 70), 41),
            ((0, 28, 4, 33, 2), 30),
            ((4, 33, 8, 43, 6), 38),
            ((8, 43, 12, 38, 10), 41),
            ((12, 38, 16, 30, 14), 34),
            ((16, 30, 23, 40, 19), 34),
            ((23, 40, 33, 26, 28), 33),
            ((33, 26, 46, 31, 39), 28),
            ((46, 31, 70, 20, 58), 26),
            ((70, 20, 128, 67, 90), 36),
        ];
        for ((x0, y0, x1, y1, x), want) in cases {
            assert_eq!(
                render_point(x0, y0, x1, y1, x),
                want,
                "{x0} {y0} {x1} {y1} {x}"
            );
        }
        assert_eq!(render_point(5, 10, 5, 20, 5), 10);
    }

    #[test]
    fn render_line_draws_the_spec_line() {
        let mut v = Vec::new();
        render_line(0, 0, 4, 8, &mut v);
        assert_eq!(v, [0, 2, 4, 6]);
        v.clear();
        render_line(0, 10, 5, 7, &mut v);
        assert_eq!(v, [10, 10, 9, 9, 8]);
        v.clear();
        render_line(3, 5, 6, 5, &mut v);
        assert_eq!(v, [5, 5, 5]);
    }

    #[test]
    fn inverse_coupling_covers_all_four_sign_cases() {
        assert_eq!(inverse_couple(3.0, 1.0), (3.0, 2.0));
        assert_eq!(inverse_couple(3.0, -1.0), (2.0, 3.0));
        assert_eq!(inverse_couple(-3.0, 1.0), (-3.0, -2.0));
        assert_eq!(inverse_couple(-3.0, -1.0), (-2.0, -3.0));
        assert_eq!(inverse_couple(0.0, 0.0), (0.0, 0.0));
    }

    #[test]
    fn pair_mut_gives_both_orders_and_refuses_one_index() {
        let mut v = [1, 2, 3];
        let (a, b) = pair_mut(&mut v, 0, 2).unwrap();
        assert_eq!((*a, *b), (1, 3));
        let (a, b) = pair_mut(&mut v, 2, 0).unwrap();
        assert_eq!((*a, *b), (3, 1));
        assert!(pair_mut(&mut v, 1, 1).is_none());
        assert!(pair_mut(&mut v, 0, 5).is_none());
    }

    #[test]
    fn the_inverse_db_table_rises_to_one() {
        assert!(FLOOR1_INVERSE_DB_TABLE.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(FLOOR1_INVERSE_DB_TABLE[255], 1.0);
    }

    fn floor0(order: u8, n: usize, bark: Vec<f32>) -> FloorTypeZero {
        FloorTypeZero {
            floor0_order: order,
            floor0_amplitude_bits: 6,
            floor0_amplitude_offset: 100,
            floor0_number_of_books: 1,
            floor0_book_list: vec![0],
            cached_bark_cos_omega: [bark.clone(), {
                let mut b = bark;
                b.resize(n, 0.0);
                b
            }],
        }
    }

    #[test]
    fn floor_zero_curves_repeat_over_equal_bark_values_and_fill_n() {
        let bark = vec![0.9, 0.9, 0.5, 0.5, 0.5, -0.2, -0.7, -0.7];
        for order in [1u8, 2, 3, 4] {
            let fl = floor0(order, 8, bark.clone());
            let coeffs: Vec<f32> = (0..order).map(|i| (i as f32 * 0.3).cos()).collect();
            let c = floor_zero_compute_curve(&coeffs, 1, &fl, false, 8);
            assert_eq!(c.len(), 8, "order {order}");
            assert!(c.iter().all(|v| !v.is_nan() && *v >= 0.0), "{c:?}");
            assert_eq!(c[0], c[1]);
            assert_eq!(c[2], c[4]);
            assert_eq!(c[6], c[7]);
        }
    }

    #[test]
    fn floor_zero_with_a_nan_bark_map_still_ends() {
        let fl = floor0(2, 4, vec![f32::NAN; 4]);
        let c = floor_zero_compute_curve(&[0.1, 0.2], 3, &fl, false, 4);
        assert_eq!(c.len(), 4);
        let fl = floor0(2, 4, Vec::new());
        assert_eq!(
            floor_zero_compute_curve(&[0.1, 0.2], 3, &fl, false, 4).len(),
            4
        );
    }

    fn floor1(mult: u8, xs: Vec<u32>) -> FloorTypeOne {
        let mut sorted: Vec<(usize, u32)> = xs.iter().copied().enumerate().collect();
        sorted.sort_by_key(|a| a.1);
        FloorTypeOne {
            floor1_multiplier: mult,
            floor1_partition_class: Vec::new(),
            floor1_class_dimensions: Vec::new(),
            floor1_class_subclasses: Vec::new(),
            floor1_subclass_books: Vec::new(),
            floor1_class_masterbooks: Vec::new(),
            floor1_x_list: xs,
            floor1_x_list_sorted: sorted,
        }
    }

    #[test]
    fn floor_one_amplitudes_follow_the_spec_and_stay_in_range() {
        let fl = floor1(1, vec![0, 128, 64, 32, 96]);
        let (y, step2) = floor_one_curve_compute_amplitude(&[100, 200, 0, 3, 9999], &fl).unwrap();
        assert_eq!(y[0], 100);
        assert_eq!(y[1], 200);
        assert_eq!(y[2], 150);
        assert_eq!(step2, [true; 5]);
        assert_eq!(y[4], 0);
        assert!(y.iter().all(|&v| (0..256).contains(&v)), "{y:?}");
        assert_eq!(y[3], 125 - 2);
        assert!(floor_one_curve_compute_amplitude(&[1], &fl).is_none());
    }

    #[test]
    fn floor_one_synthesis_is_n_long_and_reads_the_db_table() {
        for mult in 1..=4u8 {
            let fl = floor1(mult, vec![0, 128, 64]);
            let range = RANGES[mult as usize - 1];
            let (y, step2) =
                floor_one_curve_compute_amplitude(&[range as u32 - 1, 0, 0], &fl).unwrap();
            for n in [64usize, 128, 256] {
                let mut out = Vec::new();
                floor_one_curve_synthesis(&y, &step2, &fl, n, &mut out);
                assert_eq!(out.len(), n, "mult {mult} n {n}");
                assert!(out[0] > out[n.min(128) - 1], "falls from its first point");
                let top = FLOOR1_INVERSE_DB_TABLE[((range - 1) * mult as i32) as usize];
                assert_eq!(out[0], top);
            }
        }
    }

    use super::super::header::{ModeInfo, ResidueBook};
    use super::super::header_cached::{CachedBlocksizeDerived, compute_bark_map_cos_omega};
    use super::super::huffman_tree::VorbisHuffmanTree;

    fn bits(fields: &[(u64, u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut n = 0u32;
        for &(v, w) in fields {
            for i in 0..w {
                if n.is_multiple_of(8) {
                    out.push(0);
                }
                let last = out.len() - 1;
                out[last] |= (((v >> i) & 1) as u8) << (n % 8);
                n += 1;
            }
        }
        out
    }

    fn book(dims: u16, vq: Option<Vec<f32>>) -> Codebook {
        Codebook {
            codebook_dimensions: dims,
            codebook_entries: 2,
            codebook_vq_lookup_vec: vq,
            codebook_huffman_tree: VorbisHuffmanTree::load_from_array(&[1, 1]).unwrap(),
        }
    }

    fn books() -> Vec<Codebook> {
        vec![book(1, None), book(2, Some(vec![1.0, 2.0, 10.0, 20.0]))]
    }

    fn residue(kind: u8, end: u32, psize: u32, stages: Vec<ResidueBook>) -> Residue {
        Residue {
            residue_type: kind,
            residue_begin: 0,
            residue_end: end,
            residue_partition_size: psize,
            residue_classifications: 1,
            residue_classbook: 0,
            residue_books: stages,
        }
    }

    fn mono_mapping() -> Mapping {
        Mapping {
            mapping_magnitudes: vec![],
            mapping_angles: vec![],
            mapping_mux: vec![0],
            mapping_submap_floors: vec![0],
            mapping_submap_residues: vec![0],
        }
    }

    fn decoder(channels: u8, floor: Floor, resid: Residue, mapping: Mapping) -> Decoder {
        let ident = IdentHeader {
            audio_channels: channels,
            audio_sample_rate: 8000,
            bitrate_maximum: 0,
            bitrate_nominal: 0,
            bitrate_minimum: 0,
            blocksize_0: 6,
            blocksize_1: 8,
            cached_bs_derived: [
                CachedBlocksizeDerived::from_blocksize(6),
                CachedBlocksizeDerived::from_blocksize(8),
            ],
        };
        let modes = vec![
            ModeInfo {
                mode_blockflag: false,
                mode_mapping: 0,
            },
            ModeInfo {
                mode_blockflag: true,
                mode_mapping: 0,
            },
        ];
        Decoder::new(
            ident,
            SetupHeader {
                codebooks: books(),
                floors: vec![floor],
                residues: vec![resid],
                mappings: vec![mapping],
                modes,
            },
        )
    }

    fn quiet() -> Decoder {
        decoder(
            1,
            Floor::TypeOne(floor1(1, vec![0, 128])),
            residue(1, 0, 8, vec![]),
            mono_mapping(),
        )
    }

    const SHORT: [u8; 1] = [0x00];

    fn long(prev: bool, next: bool) -> [u8; 1] {
        [0x02 | (prev as u8) << 2 | (next as u8) << 3]
    }

    fn refused(d: &mut Decoder, p: &[u8]) -> String {
        d.decode_into(p, &mut Vec::new()).unwrap_err().to_string()
    }

    const MALFORMED: &str = "malformed Vorbis audio packet";

    #[test]
    fn floor_zero_reads_its_amplitude_book_and_cumulative_coefficients() {
        let bk = vec![book(2, Some(vec![0.1, 0.2, 0.3, 0.4]))];
        let data = bits(&[(5, 6), (0, 1), (0, 1), (1, 1)]);
        let fl = floor0(4, 8, vec![0.0; 8]);
        let Ok((c, amp)) = floor_zero_decode(&mut BitpackCursor::new(&data), &bk, &fl) else {
            panic!("floor 0 did not decode");
        };
        assert_eq!(amp, 5);
        assert_eq!(c, [0.1f32, 0.2, 0.2 + 0.3, 0.2 + 0.4].map(f32::cos));
        let fl3 = floor0(3, 8, vec![0.0; 8]);
        let Ok((c, _)) = floor_zero_decode(&mut BitpackCursor::new(&data), &bk, &fl3) else {
            panic!("odd floor 0 did not decode");
        };
        assert_eq!(c, [0.1f32, 0.2, 0.2 + 0.3].map(f32::cos));
        let silent = bits(&[(0, 6)]);
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&silent), &bk, &fl),
            Err(FloorSpecialCase::Unused)
        ));
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&[]), &bk, &fl),
            Err(FloorSpecialCase::Unused)
        ));
        let mut two = floor0(4, 8, vec![0.0; 8]);
        two.floor0_number_of_books = 2;
        let past = bits(&[(5, 6), (1, 2)]);
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&past), &bk, &two),
            Err(FloorSpecialCase::PacketUndecodable)
        ));
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&data), &[book(2, None)], &fl),
            Err(FloorSpecialCase::PacketUndecodable)
        ));
        let mut flat = book(2, Some(vec![]));
        flat.codebook_dimensions = 0;
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&data), &[flat], &fl),
            Err(FloorSpecialCase::PacketUndecodable)
        ));
    }

    #[test]
    fn floor_zero_curve_values_match_the_spec_worked_by_hand() {
        let mut fl = floor0(2, 2, vec![0.0, 0.5]);
        let c = floor_zero_compute_curve(&[0.5, -0.5], 63, &fl, false, 2);
        assert!((c[0] - 1.0).abs() < 1e-5, "{c:?}");
        assert!((c[1] - 1.0).abs() < 1e-5, "{c:?}");
        fl.floor0_order = 1;
        let c = floor_zero_compute_curve(&[0.5], 63, &fl, false, 1);
        assert!((c[0] - 0.296_576).abs() < 1e-5, "{c:?}");
        fl.floor0_order = 2;
        let c = floor_zero_compute_curve(&[0.5, -0.5], 0, &fl, false, 1);
        assert!((c[0] - (-11.512_925f32).exp()).abs() < 1e-9, "{c:?}");
    }

    #[test]
    fn residue_zero_interleaves_each_vector_across_its_partition() {
        let b = &books()[1];
        let data = bits(&[(0, 1), (1, 1)]);
        let mut v = [0.5f32; 4];
        let r = residue(0, 0, 4, vec![]);
        residue_packet_read_partition(&mut BitpackCursor::new(&data), b, &r, &mut v).unwrap();
        assert_eq!(v, [1.5, 10.5, 2.5, 20.5]);
        let mut v = [0.0f32; 5];
        let r = residue(0, 0, 5, vec![]);
        residue_packet_read_partition(&mut BitpackCursor::new(&data), b, &r, &mut v).unwrap();
        assert_eq!(v, [1.0, 10.0, 2.0, 20.0, 0.0]);
        let one = bits(&[(1, 1)]);
        let mut rdr = BitpackCursor::new(&one);
        let mut v = [0.0f32; 1];
        let r = residue(0, 0, 1, vec![]);
        residue_packet_read_partition(&mut rdr, b, &r, &mut v).unwrap();
        assert_eq!(v, [0.0]);
        assert_eq!(rdr.read_bit_flag().ok(), Some(true));
    }

    #[test]
    fn residue_one_lays_vectors_end_to_end_and_refuses_a_book_without_values() {
        let bk = books();
        let data = bits(&[(1, 1), (0, 1)]);
        let mut v = [0.0f32; 4];
        let r = residue(1, 0, 4, vec![]);
        residue_packet_read_partition(&mut BitpackCursor::new(&data), &bk[1], &r, &mut v).unwrap();
        assert_eq!(v, [10.0, 20.0, 1.0, 2.0]);
        for kind in [0, 1] {
            let r = residue(kind, 0, 4, vec![]);
            assert_eq!(
                residue_packet_read_partition(&mut BitpackCursor::new(&data), &bk[0], &r, &mut v),
                Err(HuffmanVqReadErr::NoVqLookupForCodebook)
            );
        }
    }

    #[test]
    fn mixed_block_sizes_return_the_spans_between_overlaps() {
        let mut d = quiet();
        let mut out = Vec::new();
        let packets = [
            &SHORT[..],
            &long(false, false),
            &SHORT,
            &SHORT,
            &long(false, true),
            &long(true, true),
            &long(true, false),
        ];
        let got: Vec<usize> = packets
            .iter()
            .map(|p| d.decode_into(p, &mut out).unwrap())
            .collect();
        assert_eq!(got, [0, 128, 32, 32, 80, 128, 176]);
        assert_eq!(out.len(), got.iter().sum::<usize>());
        assert!(out.iter().all(|&v| v == 0.0));
        let n = |p: &[u8]| d.blocksize(p).unwrap();
        let formula: usize = packets.windows(2).map(|w| n(w[0]) / 4 + n(w[1]) / 4).sum();
        let tail = 256 / 4 - 64 / 4;
        assert_eq!(got.iter().sum::<usize>(), formula + tail);
        assert_eq!(d.lead_in(&SHORT), Ok(32));
        assert_eq!(d.lead_in(&long(true, true)), Ok(128));
        assert_eq!(d.lead_in(&long(true, false)), Ok(128));
        assert_eq!(d.lead_in(&long(false, true)), Ok(80));
    }

    #[test]
    fn a_window_that_does_not_fit_the_previous_half_is_refused() {
        let mut d = quiet();
        assert_eq!(d.decode_into(&long(true, true), &mut Vec::new()), Ok(0));
        assert_eq!(refused(&mut d, &SHORT), MALFORMED);
        d.reset();
        let mut out = Vec::new();
        assert_eq!(d.decode_into(&SHORT, &mut out), Ok(0));
        assert_eq!(d.decode_into(&long(true, true), &mut out), Ok(128));
    }

    #[test]
    fn a_mode_or_mapping_past_its_list_is_refused() {
        let mut d = quiet();
        d.setup.modes.push(ModeInfo {
            mode_blockflag: false,
            mode_mapping: 0,
        });
        let past = bits(&[(0, 1), (3, 2)]);
        assert_eq!(refused(&mut d, &past), MALFORMED);
        assert_eq!(d.blocksize(&past), Err(AudioReadError::AudioBadFormat));
        assert_eq!(d.lead_in(&past), Err(AudioReadError::AudioBadFormat));
        let last = bits(&[(0, 1), (2, 2)]);
        assert_eq!(d.decode_into(&last, &mut Vec::new()), Ok(0));
        d.setup.modes[2].mode_mapping = 4;
        assert_eq!(refused(&mut d, &last), MALFORMED);
        assert_eq!(
            refused(&mut d, &[0x01]),
            "Vorbis header where an audio packet was expected"
        );
        assert_eq!(d.decode_into(&[], &mut Vec::new()), Ok(0));
    }

    #[test]
    fn coupling_a_channel_with_itself_or_past_the_channels_is_refused() {
        let couple = |m: u8, a: u8| Mapping {
            mapping_magnitudes: vec![m],
            mapping_angles: vec![a],
            mapping_mux: vec![0, 0],
            mapping_submap_floors: vec![0],
            mapping_submap_residues: vec![0],
        };
        let stereo = |m: u8, a: u8| {
            decoder(
                2,
                Floor::TypeOne(floor1(1, vec![0, 128])),
                residue(2, 0, 8, vec![]),
                couple(m, a),
            )
        };
        assert_eq!(refused(&mut stereo(0, 0), &SHORT), MALFORMED);
        assert_eq!(refused(&mut stereo(0, 2), &SHORT), MALFORMED);
        assert_eq!(refused(&mut stereo(2, 1), &SHORT), MALFORMED);
        assert_eq!(stereo(0, 1).decode_into(&SHORT, &mut Vec::new()), Ok(0));
        assert_eq!(stereo(1, 0).decode_into(&SHORT, &mut Vec::new()), Ok(0));
    }

    fn loud_floor_packet() -> Vec<u8> {
        let mut p = bits(&[(0, 1), (0, 1), (1, 1), (40, 8), (50, 8)]);
        p.extend_from_slice(&[0xff; 8]);
        p
    }

    #[test]
    fn floor_one_without_its_classes_books_or_values_is_refused() {
        let p = loud_floor_packet();
        let run = |fl: &FloorTypeOne| {
            decoder(
                1,
                Floor::TypeOne(fl.clone()),
                residue(1, 0, 8, vec![]),
                mono_mapping(),
            )
            .decode_into(&p, &mut Vec::new())
        };
        let mut fl = floor1(1, vec![0, 128, 64]);
        assert_eq!(run(&fl), Err(AudioReadError::AudioBadFormat));
        fl.floor1_partition_class = vec![0];
        fl.floor1_class_dimensions = vec![1];
        fl.floor1_class_subclasses = vec![1];
        fl.floor1_subclass_books = vec![vec![0, 0]];
        fl.floor1_class_masterbooks = vec![0];
        assert_eq!(run(&fl), Ok(0));
        fl.floor1_class_masterbooks = vec![9];
        assert_eq!(run(&fl), Err(AudioReadError::AudioBadFormat));
        fl.floor1_class_masterbooks = vec![0];
        fl.floor1_subclass_books = vec![vec![9, 9]];
        assert_eq!(run(&fl), Err(AudioReadError::AudioBadFormat));
        fl.floor1_subclass_books = vec![vec![0, 0]];
        fl.floor1_partition_class = vec![3];
        assert_eq!(run(&fl), Err(AudioReadError::AudioBadFormat));
        fl.floor1_partition_class = vec![0];
        fl.floor1_class_masterbooks = vec![];
        assert_eq!(run(&fl), Err(AudioReadError::AudioBadFormat));
    }

    #[test]
    fn a_residue_without_its_books_or_values_is_refused() {
        let p = bits(&[
            (0, 1),
            (0, 1),
            (1, 1),
            (40, 8),
            (50, 8),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (1, 1),
        ]);
        let mut d = decoder(
            1,
            Floor::TypeOne(floor1(1, vec![0, 128])),
            residue(1, 8, 8, vec![ResidueBook::first_stage(1)]),
            mono_mapping(),
        );
        assert_eq!(d.decode_into(&p, &mut Vec::new()), Ok(0));
        d.reset();
        d.setup.codebooks[1].codebook_vq_lookup_vec = None;
        assert_eq!(refused(&mut d, &p), MALFORMED);
        d.reset();
        d.setup.codebooks.truncate(1);
        assert_eq!(refused(&mut d, &p), MALFORMED);
        d.reset();
        d.setup.residues[0].residue_classbook = 5;
        assert_eq!(refused(&mut d, &p), MALFORMED);
        d.reset();
        d.setup.mappings[0].mapping_submap_residues = vec![3];
        assert_eq!(refused(&mut d, &p), MALFORMED);
    }

    #[test]
    fn floor_zero_streams_decode_through_the_packet_path() {
        let fl = FloorTypeZero {
            floor0_order: 4,
            floor0_amplitude_bits: 6,
            floor0_amplitude_offset: 100,
            floor0_number_of_books: 1,
            floor0_book_list: vec![1],
            cached_bark_cos_omega: [
                compute_bark_map_cos_omega(32, 8000, 256),
                compute_bark_map_cos_omega(128, 8000, 256),
            ],
        };
        let mut d = decoder(
            1,
            Floor::TypeZero(fl.clone()),
            residue(1, 8, 8, vec![ResidueBook::first_stage(1)]),
            mono_mapping(),
        );
        let p = bits(&[
            (0, 1),
            (0, 1),
            (5, 6),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (1, 1),
        ]);
        let mut out = Vec::new();
        assert_eq!(d.decode_into(&p, &mut out), Ok(0));
        assert_eq!(d.decode_into(&p, &mut out), Ok(32));
        assert!(out.iter().all(|v| v.is_finite()), "{out:?}");
        assert!(out.iter().any(|&v| v != 0.0), "{out:?}");
        let silent = bits(&[(0, 1), (0, 1), (0, 6)]);
        let mut quiet_out = Vec::new();
        assert_eq!(d.decode_into(&silent, &mut quiet_out), Ok(32));
        let mut bad = fl;
        bad.floor0_book_list = vec![7];
        d.setup.floors = vec![Floor::TypeZero(bad)];
        assert_eq!(refused(&mut d, &p), MALFORMED);
    }

    #[test]
    fn a_first_long_block_before_a_short_one_returns_its_flat_part() {
        let mut d = decoder(
            1,
            Floor::TypeZero(FloorTypeZero {
                floor0_order: 4,
                floor0_amplitude_bits: 6,
                floor0_amplitude_offset: 100,
                floor0_number_of_books: 1,
                floor0_book_list: vec![1],
                cached_bark_cos_omega: [
                    compute_bark_map_cos_omega(32, 8000, 256),
                    compute_bark_map_cos_omega(128, 8000, 256),
                ],
            }),
            residue(1, 8, 8, vec![ResidueBook::first_stage(1)]),
            mono_mapping(),
        );
        let body = [
            (5, 6),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (0, 1),
            (1, 1),
            (0, 1),
            (1, 1),
        ];
        let packet = |head: &[(u64, u32)]| bits(&[head, &body[..]].concat());
        let first = packet(&[(0, 1), (1, 1), (1, 1), (0, 1)]);
        let short = packet(&[(0, 1), (0, 1)]);
        assert_eq!(d.shape(&first), Ok((256, Some((true, false)))));
        let flat = (256 - 64) / 4;
        let mut out = Vec::new();
        assert_eq!(d.decode_into(&first, &mut out), Ok(flat));
        assert_eq!(out.len(), flat);
        assert!(out.iter().any(|&v| v != 0.0), "{out:?}");
        assert_eq!(d.decode_into(&short, &mut out), Ok(32));
        assert_eq!(out.len(), 256 / 4 + 64 / 4);
        d.reset();
        let mut aligned = Vec::new();
        assert_eq!(d.decode_aligned(&first, &mut aligned), Ok(128 + flat));
        assert!(aligned[..128].iter().all(|&v| v == 0.0));
        assert_eq!(aligned[128..], out[..flat]);
    }

    #[test]
    fn a_packet_cut_inside_its_block_flags_is_refused_by_name() {
        let mut d = quiet();
        d.setup.modes = (0..64)
            .map(|_| ModeInfo {
                mode_blockflag: true,
                mode_mapping: 0,
            })
            .collect();
        let cut = bits(&[(0, 1), (5, 6)]);
        assert_eq!(cut.len(), 1);
        assert_eq!(refused(&mut d, &cut), "Vorbis audio packet cut short");
        assert_eq!(d.blocksize(&cut), Err(AudioReadError::EndOfPacket));
        assert_eq!(d.lead_in(&cut), Err(AudioReadError::EndOfPacket));
        assert_eq!(
            crate::media::Error::from(AudioReadError::EndOfPacket).to_string(),
            "invalid data: Vorbis audio packet cut short"
        );
        let whole = bits(&[(0, 1), (5, 6), (1, 1), (1, 1)]);
        assert_eq!(d.blocksize(&whole), Ok(256));
        assert_eq!(d.decode_into(&whole, &mut Vec::new()), Ok(0));
    }

    #[test]
    fn floor_zero_cut_inside_its_coefficients_is_an_unused_floor() {
        let bk = vec![book(2, Some(vec![0.1, 0.2, 0.3, 0.4]))];
        let fl = floor0(40, 8, vec![0.0; 8]);
        let data = bits(&[(5, 6), (0, 1), (1, 1)]);
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&data), &bk, &fl),
            Err(FloorSpecialCase::Unused)
        ));
        let fl = floor0(2, 8, vec![0.0; 8]);
        assert!(matches!(
            floor_zero_decode(&mut BitpackCursor::new(&data), &bk, &fl),
            Ok((_, 5))
        ));
    }

    #[test]
    fn residue_one_stops_at_the_vector_end_and_at_an_empty_entry() {
        let bk = books();
        let data = bits(&[(1, 1), (0, 1), (1, 1), (1, 1)]);
        let mut v = [0.0f32; 5];
        let r = residue(1, 0, 5, vec![]);
        let mut rdr = BitpackCursor::new(&data);
        residue_packet_read_partition(&mut rdr, &bk[1], &r, &mut v).unwrap();
        assert_eq!(v, [10.0, 20.0, 1.0, 2.0, 0.0]);
        assert_eq!(rdr.read_bit_flag().ok(), Some(true));
        let mut empty = book(0, Some(Vec::new()));
        empty.codebook_entries = 2;
        let mut v = [0.5f32; 4];
        let r = residue(1, 0, 4, vec![]);
        let mut rdr = BitpackCursor::new(&data);
        residue_packet_read_partition(&mut rdr, &empty, &r, &mut v).unwrap();
        assert_eq!(v, [0.5; 4]);
        assert_eq!(rdr.read_bit_flag().ok(), Some(false));
    }

    #[test]
    fn a_classbook_with_no_dimensions_is_refused_instead_of_looping() {
        let mut bk = books();
        let r = residue(1, 8, 4, vec![ResidueBook::first_stage(1)]);
        let data = [0xffu8; 8];
        bk[0].codebook_dimensions = 0;
        assert_eq!(
            residue_packet_decode_inner(&mut BitpackCursor::new(&data), 8, &[false], &r, &bk),
            Err(AudioReadError::AudioBadFormat)
        );
        bk[0].codebook_dimensions = 1;
        let v = residue_packet_decode_inner(&mut BitpackCursor::new(&data), 8, &[false], &r, &bk)
            .unwrap();
        assert_eq!(v, [10.0, 20.0, 10.0, 20.0, 10.0, 20.0, 10.0, 20.0]);
    }

    #[test]
    fn past_eight_channels_the_order_is_the_streams_own() {
        let chans: Vec<Vec<f32>> = (0..9).map(|c| vec![c as f32, 10.0 + c as f32]).collect();
        let mut out = Vec::new();
        assert_eq!(interleave(&chans, &mut out), 2);
        let first: Vec<f32> = (0..9).map(|c| c as f32).collect();
        let second: Vec<f32> = (0..9).map(|c| 10.0 + c as f32).collect();
        assert_eq!(out, [first, second].concat());
        let mut out = Vec::new();
        assert_eq!(interleave(&chans[..6], &mut out), 2);
        assert_eq!(out[..6], [0.0, 2.0, 1.0, 5.0, 3.0, 4.0]);
        assert_eq!(interleave(&[], &mut out), 0);
        let mut d = decoder(
            9,
            Floor::TypeOne(floor1(1, vec![0, 128])),
            residue(1, 0, 8, vec![]),
            Mapping {
                mapping_mux: vec![0; 9],
                ..mono_mapping()
            },
        );
        let mut out = Vec::new();
        assert_eq!(d.decode_into(&SHORT, &mut out), Ok(0));
        assert_eq!(d.decode_into(&SHORT, &mut out), Ok(32));
        assert_eq!(out.len(), 32 * 9);
    }
}
