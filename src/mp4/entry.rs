use crate::media::{Error, Result};
use crate::mp4::boxes::{BoxBuf, Cur, FourCc, boxes};

#[derive(Clone, Debug, PartialEq)]
pub struct SampleEntry {
    pub format: FourCc,
    pub codec: CodecConfig,
    pub video: Option<VideoParams>,
    pub audio: Option<AudioParams>,
    pub bitrate: Option<BitRate>,
    pub raw: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VideoParams {
    pub width: u16,
    pub height: u16,
    pub pixel_aspect: Option<(u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BitRate {
    pub buffer_size: u32,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct AudioParams {
    pub qt_version: u16,
    pub channels: u32,
    pub sample_size: u16,
    pub sample_rate: f64,
    pub samples_per_packet: u32,
    pub bytes_per_frame: u32,
    pub const_bits_per_channel: u32,
    pub lpcm_flags: u32,
    pub little_endian: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CodecConfig {
    Avc(AvcConfig),
    Hevc(HevcConfig),
    Vp9(VpcConfig),
    Aac(AacConfig),
    Mp3,
    Pcm(PcmConfig),
    Other(FourCc),
}

impl CodecConfig {
    pub fn name(&self) -> String {
        match self {
            CodecConfig::Avc(_) => "h264".into(),
            CodecConfig::Hevc(_) => "hevc".into(),
            CodecConfig::Vp9(_) => "vp9".into(),
            CodecConfig::Aac(_) => "aac".into(),
            CodecConfig::Mp3 => "mp3".into(),
            CodecConfig::Pcm(_) => "pcm".into(),
            CodecConfig::Other(f) => f.to_string_lossy().trim().to_string(),
        }
    }

    pub fn extradata(&self) -> Option<Vec<u8>> {
        match self {
            CodecConfig::Avc(c) => Some(c.to_bytes()),
            CodecConfig::Hevc(c) => Some(c.to_bytes()),
            CodecConfig::Vp9(c) => Some(c.to_bytes()),
            CodecConfig::Aac(c) => Some(c.asc.clone()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AvcConfig {
    pub profile: u8,
    pub compatibility: u8,
    pub level: u8,
    pub length_size: u8,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub ext: Vec<u8>,
}

impl AvcConfig {
    pub fn new(sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Self {
        let (profile, compatibility, level) = sps
            .first()
            .filter(|s| s.len() >= 4)
            .map(|s| (s[1], s[2], s[3]))
            .unwrap_or((0, 0, 0));
        let ext = if matches!(profile, 100 | 110 | 122 | 144) {
            vec![0xFD, 0xF8, 0xF8, 0]
        } else {
            Vec::new()
        };
        AvcConfig {
            profile,
            compatibility,
            level,
            length_size: 4,
            sps,
            pps,
            ext,
        }
    }

    pub fn parameter_sets_annexb(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in self.sps.iter().chain(&self.pps) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "avcC");
        let _version = c.u8()?;
        let profile = c.u8()?;
        let compatibility = c.u8()?;
        let level = c.u8()?;
        let length_size = (c.u8()? & 3) + 1;
        let nsps = (c.u8()? & 0x1F) as usize;
        let mut sps = Vec::with_capacity(nsps);
        for _ in 0..nsps {
            let n = c.u16()? as usize;
            sps.push(c.bytes(n)?.to_vec());
        }
        let npps = c.u8()? as usize;
        let mut pps = Vec::with_capacity(npps);
        for _ in 0..npps {
            let n = c.u16()? as usize;
            pps.push(c.bytes(n)?.to_vec());
        }
        Ok(AvcConfig {
            profile,
            compatibility,
            level,
            length_size,
            sps,
            pps,
            ext: c.rest().to_vec(),
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(1);
        b.u8(self.profile);
        b.u8(self.compatibility);
        b.u8(self.level);
        b.u8(0xFC | (self.length_size.clamp(1, 4) - 1));
        b.u8(0xE0 | (self.sps.len().min(31) as u8));
        for s in self.sps.iter().take(31) {
            b.u16(s.len() as u16);
            b.bytes(s);
        }
        b.u8(self.pps.len().min(255) as u8);
        for p in self.pps.iter().take(255) {
            b.u16(p.len() as u16);
            b.bytes(p);
        }
        b.bytes(&self.ext);
        b.buf
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct NalArray {
    pub completeness: bool,
    pub nal_type: u8,
    pub nalus: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HevcConfig {
    pub profile_space: u8,
    pub tier: bool,
    pub profile: u8,
    pub compatibility: u32,
    pub constraints: u64,
    pub level: u8,
    pub min_spatial_segmentation: u16,
    pub parallelism: u8,
    pub chroma_format: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub avg_frame_rate: u16,
    pub constant_frame_rate: u8,
    pub temporal_layers: u8,
    pub temporal_id_nested: bool,
    pub length_size: u8,
    pub arrays: Vec<NalArray>,
}

impl HevcConfig {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "hvcC");
        let _version = c.u8()?;
        let b = c.u8()?;
        let compatibility = c.u32()?;
        let hi = c.u16()? as u64;
        let lo = c.u32()? as u64;
        let level = c.u8()?;
        let min_seg = c.u16()? & 0x0FFF;
        let par = c.u8()? & 3;
        let chroma = c.u8()? & 3;
        let bdl = (c.u8()? & 7) + 8;
        let bdc = (c.u8()? & 7) + 8;
        let afr = c.u16()?;
        let x = c.u8()?;
        let narrays = c.u8()? as usize;
        let mut arrays = Vec::new();
        for _ in 0..narrays {
            let t = c.u8()?;
            let n = c.u16()? as usize;
            c.check_count(n, 2)?;
            let mut nalus = Vec::with_capacity(n);
            for _ in 0..n {
                let len = c.u16()? as usize;
                nalus.push(c.bytes(len)?.to_vec());
            }
            arrays.push(NalArray {
                completeness: t & 0x80 != 0,
                nal_type: t & 0x3F,
                nalus,
            });
        }
        Ok(HevcConfig {
            profile_space: b >> 6,
            tier: b & 0x20 != 0,
            profile: b & 0x1F,
            compatibility,
            constraints: (hi << 32) | lo,
            level,
            min_spatial_segmentation: min_seg,
            parallelism: par,
            chroma_format: chroma,
            bit_depth_luma: bdl,
            bit_depth_chroma: bdc,
            avg_frame_rate: afr,
            constant_frame_rate: x >> 6,
            temporal_layers: (x >> 3) & 7,
            temporal_id_nested: x & 4 != 0,
            length_size: (x & 3) + 1,
            arrays,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = BoxBuf::new();
        b.u8(1);
        b.u8((self.profile_space << 6) | ((self.tier as u8) << 5) | (self.profile & 0x1F));
        b.u32(self.compatibility);
        b.u16((self.constraints >> 32) as u16);
        b.u32(self.constraints as u32);
        b.u8(self.level);
        b.u16(0xF000 | (self.min_spatial_segmentation & 0x0FFF));
        b.u8(0xFC | (self.parallelism & 3));
        b.u8(0xFC | (self.chroma_format & 3));
        b.u8(0xF8 | (self.bit_depth_luma.saturating_sub(8) & 7));
        b.u8(0xF8 | (self.bit_depth_chroma.saturating_sub(8) & 7));
        b.u16(self.avg_frame_rate);
        b.u8((self.constant_frame_rate << 6)
            | ((self.temporal_layers & 7) << 3)
            | ((self.temporal_id_nested as u8) << 2)
            | (self.length_size.clamp(1, 4) - 1));
        b.u8(self.arrays.len() as u8);
        for a in &self.arrays {
            b.u8(((a.completeness as u8) << 7) | (a.nal_type & 0x3F));
            b.u16(a.nalus.len() as u16);
            for n in &a.nalus {
                b.u16(n.len() as u16);
                b.bytes(n);
            }
        }
        b.buf
    }

    fn nalus(&self, t: u8) -> Vec<&[u8]> {
        self.arrays
            .iter()
            .filter(|a| a.nal_type == t)
            .flat_map(|a| a.nalus.iter().map(Vec::as_slice))
            .collect()
    }

    pub fn vps(&self) -> Vec<&[u8]> {
        self.nalus(32)
    }

    pub fn sps(&self) -> Vec<&[u8]> {
        self.nalus(33)
    }

    pub fn pps(&self) -> Vec<&[u8]> {
        self.nalus(34)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct VpcConfig {
    pub profile: u8,
    pub level: u8,
    pub bit_depth: u8,
    pub chroma_subsampling: u8,
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
}

const VP9_LEVELS: [(u8, u64, u64, u32); 14] = [
    (10, 829_440, 36_864, 512),
    (11, 2_764_800, 73_728, 768),
    (20, 4_608_000, 122_880, 1_536),
    (21, 9_216_000, 245_760, 2_816),
    (30, 20_736_000, 552_960, 6_656),
    (31, 36_864_000, 983_040, 10_496),
    (40, 83_558_400, 2_228_224, 16_384),
    (41, 160_432_128, 2_228_224, 16_384),
    (50, 311_951_360, 8_912_896, 32_768),
    (51, 588_251_136, 8_912_896, 32_768),
    (52, 1_176_502_272, 8_912_896, 32_768),
    (60, 1_176_502_272, 35_651_584, 65_536),
    (61, 2_353_004_544, 35_651_584, 65_536),
    (62, 4_706_009_088, 35_651_584, 65_536),
];

impl VpcConfig {
    pub fn new(
        (profile, bit_depth, chroma_subsampling): (u8, u8, u8),
        (color_space, full_range): (u8, bool),
        (width, height): (u32, u32),
        fps: (u32, u32),
    ) -> Self {
        let (colour_primaries, transfer_characteristics, matrix_coefficients) = match color_space {
            1 => (5, 6, 5),
            2 => (1, 1, 1),
            3 => (6, 6, 6),
            4 => (7, 7, 7),
            5 => (9, 2, 9),
            7 => (1, 13, 0),
            _ => (2, 2, 2),
        };
        VpcConfig {
            profile,
            level: vp9_level((width, height), fps),
            bit_depth,
            chroma_subsampling,
            full_range,
            colour_primaries,
            transfer_characteristics,
            matrix_coefficients,
        }
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cur::new(data, "vpcC");
        let (version, _) = c.full_header()?;
        if version != 1 {
            return Err(Error::Unsupported(format!("vpcC version {version}")));
        }
        let profile = c.u8()?;
        let level = c.u8()?;
        let packed = c.u8()?;
        Ok(VpcConfig {
            profile,
            level,
            bit_depth: packed >> 4,
            chroma_subsampling: (packed >> 1) & 7,
            full_range: packed & 1 == 1,
            colour_primaries: c.u8()?,
            transfer_characteristics: c.u8()?,
            matrix_coefficients: c.u8()?,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        vec![
            1,
            0,
            0,
            0,
            self.profile,
            self.level,
            (self.bit_depth << 4) | ((self.chroma_subsampling & 7) << 1) | self.full_range as u8,
            self.colour_primaries,
            self.transfer_characteristics,
            self.matrix_coefficients,
            0,
            0,
        ]
    }
}

fn vp9_level((width, height): (u32, u32), (num, den): (u32, u32)) -> u8 {
    let picture = width as u64 * height as u64;
    let rate = picture * num as u64 / den.max(1) as u64;
    let breadth = width.max(height);
    VP9_LEVELS
        .iter()
        .find(|l| rate <= l.1 && picture <= l.2 && breadth <= l.3)
        .map_or(62, |l| l.0)
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AacConfig {
    pub asc: Vec<u8>,
    pub object_type: u8,
    pub sample_rate: u32,
    pub channel_config: u8,
    pub max_bitrate: u32,
    pub avg_bitrate: u32,
    pub buffer_size: u32,
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

impl AacConfig {
    pub fn from_asc(asc: Vec<u8>) -> Self {
        let (object_type, sample_rate, channel_config) = parse_asc(&asc).unwrap_or((0, 0, 0));
        AacConfig {
            asc,
            object_type,
            sample_rate,
            channel_config,
            ..Default::default()
        }
    }

    pub fn channels(&self) -> u32 {
        match self.channel_config {
            1..=6 => self.channel_config as u32,
            7 => 8,
            _ => 0,
        }
    }
}

fn parse_asc(asc: &[u8]) -> Option<(u8, u32, u8)> {
    let bit = |i: usize| asc.get(i / 8).map(|b| (b >> (7 - i % 8)) & 1);
    let bits = |at: &mut usize, n: usize| -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | bit(*at)? as u32;
            *at += 1;
        }
        Some(v)
    };
    let mut at = 0;
    let mut t = bits(&mut at, 5)? as u8;
    if t == 31 {
        t = 32 + bits(&mut at, 6)? as u8;
    }
    let i = bits(&mut at, 4)? as usize;
    let sr = if i == 15 {
        bits(&mut at, 24)?
    } else {
        *AAC_RATES.get(i)?
    };
    let ch = bits(&mut at, 4)? as u8;
    Some((t, sr, ch))
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct PcmConfig {
    pub bits: u16,
    pub float: bool,
    pub big_endian: bool,
    pub signed: bool,
    pub channels: u32,
    pub sample_rate: f64,
}

impl PcmConfig {
    pub fn bytes_per_frame(&self) -> u32 {
        (self.bits as u32)
            .div_ceil(8)
            .saturating_mul(self.channels.max(1))
    }
}

fn is_video_fourcc(f: &[u8; 4]) -> bool {
    matches!(
        f,
        b"avc1"
            | b"avc3"
            | b"hvc1"
            | b"hev1"
            | b"av01"
            | b"vp09"
            | b"vp08"
            | b"apch"
            | b"apcn"
            | b"apcs"
            | b"apco"
            | b"ap4h"
            | b"ap4x"
            | b"jpeg"
            | b"mjpa"
            | b"mjpb"
            | b"AVdn"
            | b"AVdh"
            | b"mp4v"
            | b"encv"
            | b"2vuy"
            | b"v210"
    )
}

fn is_audio_fourcc(f: &[u8; 4]) -> bool {
    matches!(
        f,
        b"mp4a"
            | b"lpcm"
            | b"sowt"
            | b"twos"
            | b"in24"
            | b"in32"
            | b"fl32"
            | b"fl64"
            | b"ipcm"
            | b"fpcm"
            | b"alac"
            | b"Opus"
            | b"fLaC"
            | b"ac-3"
            | b"ec-3"
            | b".mp3"
            | b".mp2"
            | b"enca"
            | b"ulaw"
            | b"alaw"
            | b"NONE"
    )
}

pub(crate) fn parse_sample_entry(
    format: FourCc,
    payload: &[u8],
    handler: FourCc,
    is_qt: bool,
) -> Result<SampleEntry> {
    let mut c = Cur::new(payload, "sample entry");
    c.skip(8)?;
    let f = &format.0;
    let mut entry = SampleEntry {
        format,
        codec: CodecConfig::Other(format),
        video: None,
        audio: None,
        bitrate: None,
        raw: payload.to_vec(),
    };
    let video_like = is_video_fourcc(f) || (handler == b"vide" && !is_audio_fourcc(f));
    let audio_like = is_audio_fourcc(f) || (handler == b"soun" && !video_like);
    if handler == b"tmcd" || f == b"tmcd" {
        return Ok(entry);
    }
    if video_like {
        let mut v = VideoParams::default();
        c.skip(16)?;
        v.width = c.u16()?;
        v.height = c.u16()?;
        c.skip(50)?;
        for b in boxes(c.rest()).map_while(|b| b.ok()) {
            let p = b.payload;
            match &b.kind.0 {
                b"avcC" if matches!(f, b"avc1" | b"avc3") => {
                    entry.codec = CodecConfig::Avc(AvcConfig::parse(p)?)
                }
                b"hvcC" if matches!(f, b"hvc1" | b"hev1") => {
                    entry.codec = CodecConfig::Hevc(HevcConfig::parse(p)?)
                }
                b"vpcC" if f == b"vp09" => entry.codec = CodecConfig::Vp9(VpcConfig::parse(p)?),
                b"pasp" => {
                    let mut c = Cur::new(p, "pasp");
                    v.pixel_aspect = Some((c.u32()?, c.u32()?));
                }
                b"btrt" => entry.bitrate = Some(parse_btrt(p)?),
                _ => {}
            }
        }
        entry.video = Some(v);
        return Ok(entry);
    }
    if !audio_like {
        return Ok(entry);
    }
    let mut a = AudioParams {
        qt_version: c.u16()?,
        ..Default::default()
    };
    c.skip(6)?;
    a.channels = c.u16()? as u32;
    a.sample_size = c.u16()?;
    c.skip(4)?;
    a.sample_rate = c.u32()? as f64 / 65536.0;
    if is_qt && a.qt_version == 1 {
        a.samples_per_packet = c.u32()?;
        c.skip(4)?;
        a.bytes_per_frame = c.u32()?;
        c.skip(4)?;
    } else if is_qt && a.qt_version == 2 {
        c.skip(4)?;
        a.sample_rate = c.f64()?;
        a.channels = c.u32()?;
        c.skip(4)?;
        a.const_bits_per_channel = c.u32()?;
        a.lpcm_flags = c.u32()?;
        c.skip(8)?;
    }
    let mut ext: Vec<(FourCc, &[u8])> = Vec::new();
    for b in boxes(c.rest()).map_while(|b| b.ok()) {
        if b.kind == b"wave" {
            for w in boxes(b.payload).map_while(|b| b.ok()) {
                ext.push((w.kind, w.payload));
            }
        } else {
            ext.push((b.kind, b.payload));
        }
    }
    let get = |k: &[u8; 4]| ext.iter().find(|(kk, _)| kk == k).map(|(_, p)| *p);
    if let Some(p) = get(b"enda") {
        let v = if p.len() >= 2 {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            p.first().copied().unwrap_or(0) as u16
        };
        a.little_endian = Some(v != 0);
    }
    entry.bitrate = get(b"btrt").and_then(|p| parse_btrt(p).ok());
    entry.codec = match f {
        b"mp4a" => match get(b"esds") {
            Some(p) => parse_esds(p)?,
            None => CodecConfig::Other(format),
        },
        b".mp3" | b".mp2" => CodecConfig::Mp3,
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"raw " | b"NONE" => {
            CodecConfig::Pcm(qt_pcm(f, &a))
        }
        b"ipcm" | b"fpcm" => {
            let mut pcm = PcmConfig {
                bits: a.sample_size,
                float: f == b"fpcm",
                big_endian: true,
                signed: true,
                channels: a.channels,
                sample_rate: a.sample_rate,
            };
            if let Some(p) = get(b"pcmC") {
                let mut c = Cur::new(p, "pcmC");
                c.full_header()?;
                pcm.big_endian = c.u8()? & 1 == 0;
                pcm.bits = c.u8()? as u16;
            }
            CodecConfig::Pcm(pcm)
        }
        _ => CodecConfig::Other(format),
    };
    entry.audio = Some(a);
    Ok(entry)
}

fn qt_pcm(f: &[u8; 4], a: &AudioParams) -> PcmConfig {
    let mut p = PcmConfig {
        channels: a.channels,
        sample_rate: a.sample_rate,
        signed: true,
        ..Default::default()
    };
    match f {
        b"lpcm" => {
            p.bits = a.const_bits_per_channel as u16;
            p.float = a.lpcm_flags & 1 != 0;
            p.big_endian = a.lpcm_flags & 2 != 0;
            p.signed = a.lpcm_flags & 4 != 0 || p.float;
            if p.bits == 0 {
                p.bits = a.sample_size;
            }
        }
        b"sowt" => {
            p.bits = if a.sample_size == 0 {
                16
            } else {
                a.sample_size
            }
        }
        b"twos" => {
            p.bits = if a.sample_size == 0 {
                16
            } else {
                a.sample_size
            };
            p.big_endian = true;
        }
        b"raw " | b"NONE" => {
            p.bits = if a.sample_size == 0 { 8 } else { a.sample_size };
            p.signed = p.bits != 8;
            p.big_endian = true;
        }
        _ => {
            p.bits = match f {
                b"in24" => 24,
                b"in32" | b"fl32" => 32,
                _ => 64,
            };
            p.float = f[0] == b'f';
            p.big_endian = !a.little_endian.unwrap_or(false);
        }
    }
    p
}

fn parse_btrt(p: &[u8]) -> Result<BitRate> {
    let mut c = Cur::new(p, "btrt");
    Ok(BitRate {
        buffer_size: c.u32()?,
        max_bitrate: c.u32()?,
        avg_bitrate: c.u32()?,
    })
}

fn descriptor(c: &mut Cur) -> Result<(u8, usize)> {
    let tag = c.u8()?;
    let mut len = 0usize;
    for _ in 0..4 {
        let b = c.u8()?;
        len = (len << 7) | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    Ok((tag, len))
}

fn parse_esds(p: &[u8]) -> Result<CodecConfig> {
    let mut c = Cur::new(p, "esds");
    c.full_header()?;
    let (tag, len) = descriptor(&mut c)?;
    if tag != 3 {
        return Err(Error::Invalid(format!(
            "esds: expected ES_Descriptor, got tag {tag}"
        )));
    }
    let es = c.bytes(len.min(c.remaining()))?;
    let mut c = Cur::new(es, "ES_Descriptor");
    c.u16()?;
    let fl = c.u8()?;
    if fl & 0x80 != 0 {
        c.u16()?;
    }
    if fl & 0x40 != 0 {
        let n = c.u8()? as usize;
        c.skip(n)?;
    }
    if fl & 0x20 != 0 {
        c.u16()?;
    }
    while c.remaining() > 0 {
        let (tag, len) = descriptor(&mut c)?;
        let body = c.bytes(len.min(c.remaining()))?;
        if tag != 4 {
            continue;
        }
        let mut d = Cur::new(body, "DecoderConfigDescriptor");
        let oti = d.u8()?;
        let _stream_type = d.u8()?;
        let buffer_size = d.u24()?;
        let max_bitrate = d.u32()?;
        let avg_bitrate = d.u32()?;
        let mut asc = Vec::new();
        while d.remaining() > 0 {
            let (t, l) = descriptor(&mut d)?;
            let b = d.bytes(l.min(d.remaining()))?;
            if t == 5 {
                asc = b.to_vec();
                break;
            }
        }
        return Ok(match oti {
            0x69 | 0x6B => CodecConfig::Mp3,
            _ => {
                let mut a = AacConfig::from_asc(asc);
                a.max_bitrate = max_bitrate;
                a.avg_bitrate = avg_bitrate;
                a.buffer_size = buffer_size;
                CodecConfig::Aac(a)
            }
        });
    }
    Err(Error::Invalid(
        "esds without DecoderConfigDescriptor".into(),
    ))
}

fn put_descriptor(b: &mut BoxBuf, tag: u8, body: &[u8]) {
    b.u8(tag);
    let n = body.len() as u32;
    b.u8(0x80 | ((n >> 21) & 0x7F) as u8);
    b.u8(0x80 | ((n >> 14) & 0x7F) as u8);
    b.u8(0x80 | ((n >> 7) & 0x7F) as u8);
    b.u8((n & 0x7F) as u8);
    b.bytes(body);
}

fn esds_payload(asc: &[u8], max_bitrate: u32, avg_bitrate: u32, buffer_size: u32) -> Vec<u8> {
    let mut dcd = BoxBuf::new();
    dcd.u8(0x40);
    dcd.u8(0x15);
    dcd.u8((buffer_size >> 16) as u8);
    dcd.u16(buffer_size as u16);
    dcd.u32(max_bitrate);
    dcd.u32(avg_bitrate);
    if !asc.is_empty() {
        put_descriptor(&mut dcd, 5, asc);
    }
    let mut es = BoxBuf::new();
    es.u16(1);
    es.u8(0);
    put_descriptor(&mut es, 4, &dcd.buf);
    put_descriptor(&mut es, 6, &[2]);
    let mut b = BoxBuf::new();
    b.u32(0);
    put_descriptor(&mut b, 3, &es.buf);
    b.buf
}

impl SampleEntry {
    fn video(format: &[u8; 4], codec: CodecConfig, width: u16, height: u16) -> Self {
        SampleEntry {
            format: FourCc(*format),
            codec,
            video: Some(VideoParams {
                width,
                height,
                pixel_aspect: None,
            }),
            audio: None,
            bitrate: None,
            raw: Vec::new(),
        }
    }

    pub fn h264(config: AvcConfig, width: u16, height: u16) -> Self {
        Self::video(b"avc1", CodecConfig::Avc(config), width, height)
    }

    pub fn hevc(config: HevcConfig, width: u16, height: u16) -> Self {
        Self::video(b"hvc1", CodecConfig::Hevc(config), width, height)
    }

    pub fn vp9(config: VpcConfig, width: u16, height: u16) -> Self {
        Self::video(b"vp09", CodecConfig::Vp9(config), width, height)
    }

    pub fn aac(asc: Vec<u8>, channels: u32, sample_rate: u32, bitrate: u32) -> Self {
        let mut config = AacConfig::from_asc(asc);
        config.avg_bitrate = bitrate;
        config.max_bitrate = bitrate;
        SampleEntry {
            format: FourCc(*b"mp4a"),
            codec: CodecConfig::Aac(config),
            video: None,
            audio: Some(AudioParams {
                channels,
                sample_rate: sample_rate as f64,
                sample_size: 16,
                ..Default::default()
            }),
            bitrate: None,
            raw: Vec::new(),
        }
    }

    pub fn is_video(&self) -> bool {
        self.video.is_some()
    }

    pub fn is_audio(&self) -> bool {
        self.audio.is_some()
    }

    pub(crate) fn write(&self, b: &mut BoxBuf) -> Result<()> {
        if !self.raw.is_empty() {
            let m = b.start(&self.format.0);
            b.bytes(&self.raw);
            b.end(m);
            return Ok(());
        }
        if let Some(v) = &self.video {
            let m = b.start(&self.format.0);
            b.zeros(6);
            b.u16(1);
            b.zeros(16);
            b.u16(v.width);
            b.u16(v.height);
            b.u32(0x0048_0000);
            b.u32(0x0048_0000);
            b.u32(0);
            b.u16(1);
            b.zeros(32);
            b.u16(24);
            b.i16(-1);
            match &self.codec {
                CodecConfig::Avc(c) => b.leaf(b"avcC", &c.to_bytes()),
                CodecConfig::Hevc(c) => b.leaf(b"hvcC", &c.to_bytes()),
                CodecConfig::Vp9(c) => b.leaf(b"vpcC", &c.to_bytes()),
                _ => {
                    return Err(Error::Unsupported(format!(
                        "writing {} video",
                        self.codec.name()
                    )));
                }
            }
            if let Some((h, vv)) = v.pixel_aspect {
                let m = b.start(b"pasp");
                b.u32(h);
                b.u32(vv);
                b.end(m);
            }
            b.end(m);
            return Ok(());
        }
        let (Some(a), CodecConfig::Aac(c)) = (&self.audio, &self.codec) else {
            return Err(Error::Unsupported(format!(
                "writing {} audio",
                self.codec.name()
            )));
        };
        let m = b.start(&self.format.0);
        b.zeros(6);
        b.u16(1);
        b.zeros(8);
        b.u16(a.channels.min(u16::MAX as u32) as u16);
        b.u16(16);
        b.zeros(4);
        b.u32(if a.sample_rate < 65536.0 {
            (a.sample_rate * 65536.0).round() as u32
        } else {
            0
        });
        b.leaf(
            b"esds",
            &esds_payload(&c.asc, c.max_bitrate, c.avg_bitrate, c.buffer_size),
        );
        if c.avg_bitrate > 0 {
            let m = b.start(b"btrt");
            b.u32(c.buffer_size);
            b.u32(c.max_bitrate);
            b.u32(c.avg_bitrate);
            b.end(m);
        }
        b.end(m);
        Ok(())
    }
}
