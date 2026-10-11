use crate::hevc::bits::{annexb_nals, unescape_rbsp};
use crate::hevc::params::Sps;
use crate::hevc::slice::nal_type;
use crate::media::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hvcc {
    pub profile: [u8; 12],
    pub chroma_format: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub temporal_layers: u8,
    pub temporal_id_nested: bool,
    pub nal_length_size: usize,
    pub width: u32,
    pub height: u32,
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub sei: Vec<Vec<u8>>,
}

fn invalid(msg: &str) -> Error {
    Error::Invalid(format!("hvcC: {msg}"))
}

impl Hvcc {
    pub fn parse(data: &[u8]) -> Result<Hvcc> {
        if data.len() < 23 {
            return Err(invalid("too short"));
        }
        if data[0] != 1 {
            return Err(Error::Unsupported(format!("hvcC version {}", data[0])));
        }
        let mut profile = [0u8; 12];
        profile.copy_from_slice(&data[1..13]);
        let mut config = Hvcc {
            profile,
            chroma_format: data[16] & 3,
            bit_depth_luma: (data[17] & 7) + 8,
            bit_depth_chroma: (data[18] & 7) + 8,
            temporal_layers: (data[21] >> 3) & 7,
            temporal_id_nested: data[21] & 4 != 0,
            nal_length_size: (data[21] & 3) as usize + 1,
            width: 0,
            height: 0,
            vps: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
            sei: Vec::new(),
        };
        let arrays = data[22] as usize;
        let mut pos = 23;
        for _ in 0..arrays {
            let head = data.get(pos..pos + 3).ok_or_else(|| invalid("truncated"))?;
            let kind = head[0] & 0x3f;
            let count = u16::from_be_bytes([head[1], head[2]]) as usize;
            pos += 3;
            for _ in 0..count {
                let len = data.get(pos..pos + 2).ok_or_else(|| invalid("truncated"))?;
                let len = u16::from_be_bytes([len[0], len[1]]) as usize;
                pos += 2;
                let nal = data
                    .get(pos..pos + len)
                    .ok_or_else(|| invalid("truncated"))?
                    .to_vec();
                pos += len;
                config.push(kind, nal);
            }
        }
        config.read_size();
        Ok(config)
    }

    pub fn from_annexb(stream: &[u8]) -> Result<Hvcc> {
        let mut config = Hvcc {
            profile: [0; 12],
            chroma_format: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            temporal_layers: 1,
            temporal_id_nested: false,
            nal_length_size: 4,
            width: 0,
            height: 0,
            vps: Vec::new(),
            sps: Vec::new(),
            pps: Vec::new(),
            sei: Vec::new(),
        };
        for nal in annexb_nals(stream) {
            let Some(&first) = nal.first() else { continue };
            let kind = (first >> 1) & 0x3f;
            if !(nal_type::VPS..=nal_type::PPS).contains(&kind) && kind != nal_type::PREFIX_SEI {
                continue;
            }
            let list = config.list(kind);
            if list.is_some_and(|l| !l.iter().any(|n| n == nal)) {
                config.push(kind, nal.to_vec());
            }
        }
        let sps = config
            .sps
            .first()
            .ok_or_else(|| invalid("no SPS in the stream"))?;
        let rbsp = unescape_rbsp(sps.get(2..).unwrap_or_default());
        let head = rbsp.get(..13).ok_or_else(|| invalid("SPS too short"))?;
        config.profile.copy_from_slice(&head[1..13]);
        config.temporal_layers = ((head[0] >> 1) & 7) + 1;
        config.temporal_id_nested = head[0] & 1 != 0;
        let parsed = Sps::parse(&rbsp)?;
        config.chroma_format = parsed.chroma_format_idc as u8;
        config.bit_depth_luma = parsed.bit_depth_luma as u8;
        config.bit_depth_chroma = parsed.bit_depth_chroma as u8;
        config.read_size();
        Ok(config)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![1];
        out.extend_from_slice(&self.profile);
        out.extend_from_slice(&[0xf0, 0x00, 0xfc]);
        out.push(0xfc | (self.chroma_format & 3));
        out.push(0xf8 | (self.bit_depth_luma.saturating_sub(8) & 7));
        out.push(0xf8 | (self.bit_depth_chroma.saturating_sub(8) & 7));
        out.extend_from_slice(&[0, 0]);
        let size = (self.nal_length_size.clamp(1, 4) - 1) as u8;
        out.push(((self.temporal_layers & 7) << 3) | ((self.temporal_id_nested as u8) << 2) | size);
        let arrays = [
            (nal_type::VPS, &self.vps),
            (nal_type::SPS, &self.sps),
            (nal_type::PPS, &self.pps),
            (nal_type::PREFIX_SEI, &self.sei),
        ];
        let present: Vec<_> = arrays.iter().filter(|(_, l)| !l.is_empty()).collect();
        out.push(present.len() as u8);
        for (kind, list) in present {
            out.push(0x80 | kind);
            out.extend_from_slice(&(list.len() as u16).to_be_bytes());
            for nal in list.iter() {
                out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
        out
    }

    pub fn annexb(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in self
            .vps
            .iter()
            .chain(&self.sps)
            .chain(&self.pps)
            .chain(&self.sei)
        {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    pub fn profile_idc(&self) -> u8 {
        self.profile[0] & 0x1f
    }

    pub fn level_idc(&self) -> u8 {
        self.profile[11]
    }

    fn list(&self, kind: u8) -> Option<&Vec<Vec<u8>>> {
        match kind {
            nal_type::VPS => Some(&self.vps),
            nal_type::SPS => Some(&self.sps),
            nal_type::PPS => Some(&self.pps),
            nal_type::PREFIX_SEI => Some(&self.sei),
            _ => None,
        }
    }

    fn push(&mut self, kind: u8, nal: Vec<u8>) {
        match kind {
            nal_type::VPS => self.vps.push(nal),
            nal_type::SPS => self.sps.push(nal),
            nal_type::PPS => self.pps.push(nal),
            nal_type::PREFIX_SEI => self.sei.push(nal),
            _ => {}
        }
    }

    fn read_size(&mut self) {
        let Some(sps) = self.sps.first() else { return };
        let Ok(parsed) = Sps::parse(&unescape_rbsp(sps.get(2..).unwrap_or_default())) else {
            return;
        };
        let (_, _, w, h) = parsed.crop_rect();
        self.width = w;
        self.height = h;
    }
}
