use crate::aac::bits::{BitReader, BitWriter};

use crate::aac::tables::{SAMPLE_RATES, table_index_for_rate};
use crate::media::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Sce,
    Cpe,
    Lfe,
}

impl ElementType {
    pub fn channels(self) -> usize {
        if self == ElementType::Cpe { 2 } else { 1 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProgramConfig {
    pub element_instance_tag: u8,
    pub object_type: u8,
    pub sf_index: u8,
    pub front: Vec<(bool, u8)>,
    pub side: Vec<(bool, u8)>,
    pub back: Vec<(bool, u8)>,
    pub lfe: Vec<u8>,
    pub assoc_data: Vec<u8>,
    pub cc: Vec<(bool, u8)>,
    pub comment: Vec<u8>,
}

impl ProgramConfig {
    pub fn parse(br: &mut BitReader) -> Result<ProgramConfig> {
        let mut p = ProgramConfig {
            element_instance_tag: br.read_bits(4)? as u8,
            object_type: br.read_bits(2)? as u8,
            sf_index: br.read_bits(4)? as u8,
            ..Default::default()
        };
        let nf = br.read_bits(4)?;
        let ns = br.read_bits(4)?;
        let nb = br.read_bits(4)?;
        let nl = br.read_bits(2)?;
        let na = br.read_bits(3)?;
        let nc = br.read_bits(4)?;
        if br.read_bits(1)? == 1 {
            br.skip(4)?;
        }
        if br.read_bits(1)? == 1 {
            br.skip(4)?;
        }
        if br.read_bits(1)? == 1 {
            br.skip(3)?;
        }
        for (n, v) in [(nf, &mut p.front), (ns, &mut p.side), (nb, &mut p.back)] {
            for _ in 0..n {
                let cpe = br.read_bits(1)? == 1;
                v.push((cpe, br.read_bits(4)? as u8));
            }
        }
        for _ in 0..nl {
            p.lfe.push(br.read_bits(4)? as u8);
        }
        for _ in 0..na {
            p.assoc_data.push(br.read_bits(4)? as u8);
        }
        for _ in 0..nc {
            let ind = br.read_bits(1)? == 1;
            p.cc.push((ind, br.read_bits(4)? as u8));
        }
        br.byte_align();
        let len = br.read_bits(8)?;
        for _ in 0..len {
            p.comment.push(br.read_bits(8)? as u8);
        }
        Ok(p)
    }

    pub fn write(&self, bw: &mut BitWriter) {
        bw.write_bits(self.element_instance_tag as u32, 4);
        bw.write_bits(self.object_type as u32, 2);
        bw.write_bits(self.sf_index as u32, 4);
        bw.write_bits(self.front.len() as u32, 4);
        bw.write_bits(self.side.len() as u32, 4);
        bw.write_bits(self.back.len() as u32, 4);
        bw.write_bits(self.lfe.len() as u32, 2);
        bw.write_bits(self.assoc_data.len() as u32, 3);
        bw.write_bits(self.cc.len() as u32, 4);
        bw.write_bits(0, 3);
        for v in [&self.front, &self.side, &self.back] {
            for &(cpe, tag) in v {
                bw.write_bits(cpe as u32, 1);
                bw.write_bits(tag as u32, 4);
            }
        }
        for &t in &self.lfe {
            bw.write_bits(t as u32, 4);
        }
        for &t in &self.assoc_data {
            bw.write_bits(t as u32, 4);
        }
        for &(ind, t) in &self.cc {
            bw.write_bits(ind as u32, 1);
            bw.write_bits(t as u32, 4);
        }
        bw.align_zero();
        bw.write_bits(self.comment.len().min(255) as u32, 8);
        for &b in self.comment.iter().take(255) {
            bw.write_bits(b as u32, 8);
        }
    }

    pub fn layout(&self) -> Vec<(ElementType, u8)> {
        let mut out = Vec::new();
        for v in [&self.front, &self.side, &self.back] {
            for &(cpe, tag) in v {
                out.push((
                    if cpe {
                        ElementType::Cpe
                    } else {
                        ElementType::Sce
                    },
                    tag,
                ));
            }
        }
        for &t in &self.lfe {
            out.push((ElementType::Lfe, t));
        }
        out
    }
}

pub fn config_layout(channel_config: u8) -> Option<Vec<(ElementType, u8)>> {
    use ElementType::*;
    Some(match channel_config {
        1 => vec![(Sce, 0)],
        2 => vec![(Cpe, 0)],
        3 => vec![(Sce, 0), (Cpe, 0)],
        4 => vec![(Sce, 0), (Cpe, 0), (Sce, 1)],
        5 => vec![(Sce, 0), (Cpe, 0), (Cpe, 1)],
        6 => vec![(Sce, 0), (Cpe, 0), (Cpe, 1), (Lfe, 0)],
        7 => vec![(Sce, 0), (Cpe, 0), (Cpe, 1), (Cpe, 2), (Lfe, 0)],
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSpecificConfig {
    pub object_type: u8,
    pub sample_rate: u32,
    pub sf_index: u8,
    pub channel_config: u8,
    pub frame_length_flag: bool,
    pub sbr: bool,
    pub ps: bool,
    pub extension_sample_rate: Option<u32>,
    pub pce: Option<ProgramConfig>,
}

fn read_aot(br: &mut BitReader) -> Result<u8> {
    let a = br.read_bits(5)? as u8;
    Ok(if a == 31 {
        32 + br.read_bits(6)? as u8
    } else {
        a
    })
}

fn read_rate(br: &mut BitReader) -> Result<(u8, u32)> {
    let i = br.read_bits(4)? as u8;
    if i == 15 {
        let r = br.read_bits(24)?;
        return Ok((table_index_for_rate(r), r));
    }
    let r = *SAMPLE_RATES
        .get(i as usize)
        .ok_or(Error::Invalid("reserved sampling frequency index".into()))?;
    Ok((i, r))
}

impl AudioSpecificConfig {
    pub fn lc(sf_index: u8, channel_config: u8) -> AudioSpecificConfig {
        AudioSpecificConfig {
            object_type: 2,
            sample_rate: SAMPLE_RATES[sf_index as usize],
            sf_index,
            channel_config,
            frame_length_flag: false,
            sbr: false,
            ps: false,
            extension_sample_rate: None,
            pce: None,
        }
    }

    pub fn parse(data: &[u8]) -> Result<AudioSpecificConfig> {
        let mut br = BitReader::new(data);
        let mut aot = read_aot(&mut br)?;
        let (sf_index, sample_rate) = read_rate(&mut br)?;
        let channel_config = br.read_bits(4)? as u8;
        let (mut sbr, mut ps, mut extension_sample_rate) = (false, false, None);
        if aot == 5 || aot == 29 {
            sbr = true;
            ps = aot == 29;
            let (_, ext) = read_rate(&mut br)?;
            extension_sample_rate = Some(ext);
            aot = read_aot(&mut br)?;
        }
        if aot != 2 {
            return Err(Error::Unsupported(
                match aot {
                    1 => "AAC Main profile",
                    3 => "AAC SSR profile",
                    4 => "AAC LTP profile",
                    23 => "AAC-LD",
                    39 => "AAC-ELD",
                    _ => "audio object type other than AAC LC",
                }
                .into(),
            ));
        }
        let frame_length_flag = br.read_bits(1)? == 1;
        if br.read_bits(1)? == 1 {
            br.skip(14)?;
        }
        let _extension_flag = br.read_bits(1)?;
        let pce = if channel_config == 0 {
            Some(ProgramConfig::parse(&mut br)?)
        } else {
            None
        };
        if frame_length_flag {
            return Err(Error::Unsupported("960-sample frames".into()));
        }
        let cfg = AudioSpecificConfig {
            object_type: aot,
            sample_rate,
            sf_index,
            channel_config,
            frame_length_flag,
            sbr,
            ps,
            extension_sample_rate,
            pce,
        };
        if cfg.channels() == 0 {
            return Err(Error::Invalid("no channels".into()));
        }
        Ok(cfg)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bw = BitWriter::new();
        bw.write_bits(self.object_type as u32, 5);
        match crate::aac::tables::sample_rate_index(self.sample_rate) {
            Some(i) => bw.write_bits(i as u32, 4),
            None => {
                bw.write_bits(15, 4);
                bw.write_bits(self.sample_rate, 24);
            }
        }
        bw.write_bits(self.channel_config as u32, 4);
        bw.write_bits(0, 3);
        if self.channel_config == 0
            && let Some(p) = &self.pce
        {
            p.write(&mut bw);
        }
        bw.finish()
    }

    pub fn layout(&self) -> Vec<(ElementType, u8)> {
        match &self.pce {
            Some(p) if self.channel_config == 0 => p.layout(),
            _ => config_layout(self.channel_config).unwrap_or_default(),
        }
    }

    pub fn channels(&self) -> usize {
        self.layout().iter().map(|e| e.0.channels()).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    pub object_type: u8,
    pub sf_index: u8,
    pub channel_config: u8,
    pub frame_length: usize,
    pub buffer_fullness: u16,
    pub header_length: usize,
    pub raw_data_blocks: u8,
}

impl AdtsHeader {
    pub fn parse(data: &[u8]) -> Result<AdtsHeader> {
        let mut br = BitReader::new(data);
        if br.read_bits(12)? != 0xFFF {
            return Err(Error::Invalid("missing ADTS syncword".into()));
        }
        br.skip(1)?;
        if br.read_bits(2)? != 0 {
            return Err(Error::Invalid("ADTS layer must be 0".into()));
        }
        let protection_absent = br.read_bits(1)? == 1;
        let object_type = br.read_bits(2)? as u8 + 1;
        let sf_index = br.read_bits(4)? as u8;
        if sf_index as usize >= SAMPLE_RATES.len() {
            return Err(Error::Invalid(
                "invalid ADTS sampling frequency index".into(),
            ));
        }
        br.skip(1)?;
        let channel_config = br.read_bits(3)? as u8;
        br.skip(4)?;
        let frame_length = br.read_bits(13)? as usize;
        let buffer_fullness = br.read_bits(11)? as u16;
        let raw_data_blocks = br.read_bits(2)? as u8 + 1;
        let header_length = if protection_absent { 7 } else { 9 };
        if frame_length < header_length {
            return Err(Error::Invalid("ADTS frame shorter than its header".into()));
        }
        Ok(AdtsHeader {
            object_type,
            sf_index,
            channel_config,
            frame_length,
            buffer_fullness,
            header_length,
            raw_data_blocks,
        })
    }

    pub fn to_asc(&self) -> AudioSpecificConfig {
        AudioSpecificConfig {
            object_type: self.object_type,
            ..AudioSpecificConfig::lc(self.sf_index, self.channel_config)
        }
    }

    pub fn write(
        object_type: u8,
        sf_index: u8,
        channel_config: u8,
        payload_len: usize,
        buffer_fullness: u16,
    ) -> [u8; 7] {
        let len = (payload_len + 7) as u32;
        let mut bw = BitWriter::new();
        bw.write_bits(0xFFF, 12);
        bw.write_bits(0, 1);
        bw.write_bits(0, 2);
        bw.write_bits(1, 1);
        bw.write_bits((object_type.max(1) - 1) as u32 & 3, 2);
        bw.write_bits(sf_index as u32, 4);
        bw.write_bits(0, 1);
        bw.write_bits(channel_config as u32 & 7, 3);
        bw.write_bits(0, 4);
        bw.write_bits(len & 0x1FFF, 13);
        bw.write_bits(buffer_fullness as u32 & 0x7FF, 11);
        bw.write_bits(0, 2);
        let v = bw.finish();
        let mut out = [0u8; 7];
        out.copy_from_slice(&v);
        out
    }
}

pub fn split_adts(mut data: &[u8]) -> Result<Vec<(AdtsHeader, &[u8])>> {
    let mut out = Vec::new();
    while data.len() >= 7 {
        let h = match AdtsHeader::parse(data) {
            Ok(h) => h,
            Err(e) => {
                if out.is_empty() {
                    return Err(e);
                }
                break;
            }
        };
        if h.frame_length > data.len() {
            break;
        }
        out.push((h, &data[h.header_length..h.frame_length]));
        data = &data[h.frame_length..];
    }
    if out.is_empty() {
        return Err(Error::Invalid("no ADTS frames".into()));
    }
    Ok(out)
}
