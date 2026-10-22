use crate::media::Codec;
use crate::mp4::{Av1Config, AvcConfig, HevcConfig, VpcConfig};
use crate::syntax::bits::{annexb_nals, length_prefixed_nals};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Chroma {
    Mono,
    Yuv420,
    Yuv422,
    Yuv440,
    Yuv444,
}

impl Chroma {
    pub fn from_idc(idc: u32) -> Option<Chroma> {
        match idc {
            0 => Some(Chroma::Mono),
            1 => Some(Chroma::Yuv420),
            2 => Some(Chroma::Yuv422),
            3 => Some(Chroma::Yuv444),
            _ => None,
        }
    }

    pub fn subsampled(x: bool, y: bool) -> Chroma {
        match (x, y) {
            (true, true) => Chroma::Yuv420,
            (true, false) => Chroma::Yuv422,
            (false, true) => Chroma::Yuv440,
            (false, false) => Chroma::Yuv444,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Chroma::Mono => "4:0:0",
            Chroma::Yuv420 => "4:2:0",
            Chroma::Yuv422 => "4:2:2",
            Chroma::Yuv440 => "4:4:0",
            Chroma::Yuv444 => "4:4:4",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Chroma::Mono => "monochrome",
            c => c.name(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Variant {
    pub bit_depth: u8,
    pub chroma: Option<Chroma>,
    pub interlaced: bool,
    pub tools: Option<&'static str>,
}

fn depth(bits: u32) -> u8 {
    u8::try_from(bits).unwrap_or(u8::MAX)
}

pub(crate) fn needs_keyframe(codec: Option<Codec>, config: &[u8]) -> bool {
    match codec {
        Some(Codec::H264) => AvcConfig::parse(config)
            .ok()
            .is_none_or(|c| !c.sps.iter().any(|n| is_sps(Codec::H264, n))),
        Some(Codec::Hevc) => HevcConfig::parse(config)
            .ok()
            .is_none_or(|c| c.sps().is_empty()),
        Some(Codec::Vp9) => VpcConfig::parse(config).is_err(),
        _ => false,
    }
}

pub(crate) fn of(codec: Option<Codec>, config: &[u8], keyframe: Option<&[u8]>) -> Option<Variant> {
    match codec? {
        Codec::H264 => {
            let c = AvcConfig::parse(config).ok();
            let length = c.as_ref().map(|c| usize::from(c.length_size));
            let sets: Vec<&[u8]> = c
                .iter()
                .flat_map(|c| c.sps.iter().chain(&c.pps))
                .map(Vec::as_slice)
                .collect();
            h264(&sets).or_else(|| h264(&keyframe_nals(Codec::H264, keyframe?, length)))
        }
        Codec::Hevc => {
            let c = HevcConfig::parse(config).ok();
            let length = c.as_ref().map(|c| usize::from(c.length_size));
            let sets: Vec<&[u8]> = c
                .iter()
                .flat_map(|c| c.sps().into_iter().chain(c.pps()))
                .collect();
            hevc(&sets)
                .or_else(|| hevc(&keyframe_nals(Codec::Hevc, keyframe?, length)))
                .or_else(|| c.as_ref().map(hvcc))
        }
        Codec::Vp9 => match VpcConfig::parse(config) {
            Ok(c) => Some(vpcc(&c)),
            Err(_) => vp9(keyframe?),
        },
        Codec::Av1 => Av1Config::parse(config).ok().map(|c| av1c(&c)),
        _ => None,
    }
}

fn nal_type(codec: Codec, nal: &[u8]) -> Option<u8> {
    let h = nal.first()?;
    match codec {
        Codec::H264 => Some(h & 0x1f),
        Codec::Hevc => Some((h >> 1) & 0x3f),
        _ => None,
    }
}

fn is_sps(codec: Codec, nal: &[u8]) -> bool {
    nal_type(codec, nal) == Some(if codec == Codec::H264 { 7 } else { 33 })
}

fn is_pps(codec: Codec, nal: &[u8]) -> bool {
    nal_type(codec, nal) == Some(if codec == Codec::H264 { 8 } else { 34 })
}

fn keyframe_nals(codec: Codec, data: &[u8], length: Option<usize>) -> Vec<&[u8]> {
    let has_sps = |nals: &Vec<&[u8]>| nals.iter().any(|n| is_sps(codec, n));
    let prefixed = || {
        length_prefixed_nals(data, length.unwrap_or(4))
            .ok()
            .filter(has_sps)
    };
    let annexb = || Some(annexb_nals(data)).filter(has_sps);
    let sniffed = data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1]);
    if length.is_none() && sniffed {
        annexb().or_else(prefixed)
    } else {
        prefixed().or_else(annexb)
    }
    .unwrap_or_default()
}

#[cfg(any(feature = "h264", feature = "gpu"))]
pub(crate) fn h264(nals: &[&[u8]]) -> Option<Variant> {
    use crate::syntax::bits::unescape_rbsp;
    use crate::syntax::h264::params::{Pps, Sps};
    let nal = nals.iter().find(|n| is_sps(Codec::H264, n))?;
    let sps = Sps::parse(&unescape_rbsp(nal.get(1..)?)).ok()?;
    let mut spss: Vec<Option<&Sps>> = vec![None; 32];
    if let Some(slot) = spss.get_mut(sps.id as usize) {
        *slot = Some(&sps);
    }
    let fmo = nals
        .iter()
        .filter(|n| is_pps(Codec::H264, n))
        .filter_map(|n| Pps::parse(&unescape_rbsp(n.get(1..)?), &spss).ok())
        .any(|p| p.sps_id == sps.id && p.num_slice_groups > 1);
    let tools = if sps.bit_depth_luma != sps.bit_depth_chroma {
        Some("mixed bit depths")
    } else if sps.qpprime_y_zero_transform_bypass {
        Some("lossless coding")
    } else if fmo {
        Some("slice groups")
    } else {
        None
    };
    Some(Variant {
        bit_depth: depth(sps.bit_depth_luma),
        chroma: Chroma::from_idc(sps.chroma_format_idc),
        interlaced: !sps.frame_mbs_only,
        tools,
    })
}

#[cfg(not(any(feature = "h264", feature = "gpu")))]
pub(crate) fn h264(_: &[&[u8]]) -> Option<Variant> {
    None
}

#[cfg(any(feature = "hevc", feature = "gpu"))]
pub(crate) fn hevc(nals: &[&[u8]]) -> Option<Variant> {
    use crate::syntax::bits::unescape_rbsp;
    use crate::syntax::hevc::params::{Pps, Sps};
    let nal = nals.iter().find(|n| is_sps(Codec::Hevc, n))?;
    let sps = Sps::parse(&unescape_rbsp(nal.get(2..)?)).ok()?;
    let pps = nals
        .iter()
        .filter(|n| is_pps(Codec::Hevc, n))
        .filter_map(|n| Pps::parse(&unescape_rbsp(n.get(2..)?)).ok())
        .filter(|p| p.sps_id == sps.id)
        .fold((false, false), |(range, layers), p| {
            (
                range || p.range_extension,
                layers || p.extension_flags & 0x7f != 0,
            )
        });
    let tools = if sps.bit_depth_luma != sps.bit_depth_chroma {
        Some("mixed bit depths")
    } else if sps.range_extension || pps.0 {
        Some("range extension tools")
    } else if sps.extension_flags & 0x7f != 0 || pps.1 {
        Some("multilayer extensions")
    } else {
        None
    };
    Some(Variant {
        bit_depth: depth(sps.bit_depth_luma),
        chroma: Chroma::from_idc(sps.chroma_format_idc),
        interlaced: sps.vui.as_ref().is_some_and(|v| v.field_seq),
        tools,
    })
}

#[cfg(not(any(feature = "hevc", feature = "gpu")))]
pub(crate) fn hevc(_: &[&[u8]]) -> Option<Variant> {
    None
}

fn hvcc(c: &HevcConfig) -> Variant {
    Variant {
        bit_depth: c.bit_depth_luma,
        chroma: Chroma::from_idc(u32::from(c.chroma_format)),
        interlaced: false,
        tools: (c.bit_depth_luma != c.bit_depth_chroma).then_some("mixed bit depths"),
    }
}

fn vpcc(c: &VpcConfig) -> Variant {
    Variant {
        bit_depth: c.bit_depth.max(8),
        chroma: match c.chroma_subsampling {
            0 | 1 => Some(Chroma::Yuv420),
            2 => Some(Chroma::Yuv422),
            3 => Some(Chroma::Yuv444),
            _ => None,
        },
        interlaced: false,
        tools: None,
    }
}

#[cfg(feature = "vp9")]
fn vp9(keyframe: &[u8]) -> Option<Variant> {
    let k = crate::vp9::keyframe_info(keyframe)?;
    Some(Variant {
        bit_depth: depth(k.bit_depth.max(8)),
        chroma: Some(Chroma::subsampled(k.subsampling_x, k.subsampling_y)),
        interlaced: false,
        tools: None,
    })
}

#[cfg(not(feature = "vp9"))]
fn vp9(_: &[u8]) -> Option<Variant> {
    None
}

fn av1c(c: &Av1Config) -> Variant {
    Variant {
        bit_depth: c.bit_depth(),
        chroma: Some(if c.monochrome {
            Chroma::Mono
        } else {
            Chroma::subsampled(c.chroma_subsampling_x, c.chroma_subsampling_y)
        }),
        interlaced: false,
        tools: None,
    }
}

#[cfg(all(test, feature = "h264", feature = "hevc"))]
mod tests {
    use super::*;
    use crate::mp4::NalArray;
    use crate::syntax::bits::{BitWriter, escape_rbsp};

    struct Avc {
        profile: u8,
        chroma: u32,
        luma: u32,
        chroma_bits: u32,
        frame_mbs_only: bool,
        lossless: bool,
    }

    const HIGH: Avc = Avc {
        profile: 100,
        chroma: 1,
        luma: 8,
        chroma_bits: 8,
        frame_mbs_only: true,
        lossless: false,
    };

    fn avc_sps(a: &Avc) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(u32::from(a.profile), 8);
        w.write_bits(0, 8);
        w.write_bits(40, 8);
        w.write_ue(0);
        if matches!(a.profile, 100 | 110 | 122 | 244) {
            w.write_ue(a.chroma);
            if a.chroma == 3 {
                w.write_bit(false);
            }
            w.write_ue(a.luma - 8);
            w.write_ue(a.chroma_bits - 8);
            w.write_bit(a.lossless);
            w.write_bit(false);
        }
        w.write_ue(0);
        w.write_ue(2);
        w.write_ue(1);
        w.write_bit(false);
        w.write_ue(19);
        w.write_ue(14);
        w.write_bit(a.frame_mbs_only);
        if !a.frame_mbs_only {
            w.write_bit(true);
        }
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.rbsp_trailing();
        let mut nal = vec![0x67];
        nal.extend(escape_rbsp(&w.finish()));
        nal
    }

    fn avcc(sps: Vec<u8>) -> Vec<u8> {
        AvcConfig::new(vec![sps], vec![vec![0x68, 0xce, 0x38, 0x80]]).to_bytes()
    }

    fn h264_of(a: &Avc) -> Variant {
        of(Some(Codec::H264), &avcc(avc_sps(a)), None).unwrap()
    }

    struct Hevc {
        profile: u32,
        chroma: u32,
        luma: u32,
        chroma_bits: u32,
        rext: bool,
    }

    const MAIN: Hevc = Hevc {
        profile: 1,
        chroma: 1,
        luma: 8,
        chroma_bits: 8,
        rext: false,
    };

    fn hevc_sps(h: &Hevc) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(0, 4);
        w.write_bits(0, 3);
        w.write_bits(1, 1);
        w.write_bits(0, 2);
        w.write_bits(0, 1);
        w.write_bits(h.profile, 5);
        w.write_bits(1 << (31 - h.profile), 32);
        w.write_bits(0b1001, 4);
        w.write_bits(0, 32);
        w.write_bits(0, 11);
        w.write_bits(0, 1);
        w.write_bits(93, 8);
        w.write_ue(0);
        w.write_ue(h.chroma);
        if h.chroma == 3 {
            w.write_bit(false);
        }
        w.write_ue(320);
        w.write_ue(240);
        w.write_bit(false);
        w.write_ue(h.luma - 8);
        w.write_ue(h.chroma_bits - 8);
        w.write_ue(4);
        w.write_bit(true);
        w.write_ue(4);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(0);
        w.write_ue(1);
        w.write_ue(0);
        w.write_ue(2);
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(h.rext);
        if h.rext {
            w.write_bits(0x80, 8);
            w.write_bits(1 << 8, 9);
        }
        w.rbsp_trailing();
        let mut nal = vec![0x42, 0x01];
        nal.extend(escape_rbsp(&w.finish()));
        nal
    }

    fn hvcc_of(h: &Hevc, sps: Vec<Vec<u8>>, pps: Vec<Vec<u8>>) -> Vec<u8> {
        HevcConfig {
            profile: h.profile as u8,
            chroma_format: h.chroma as u8,
            bit_depth_luma: h.luma as u8,
            bit_depth_chroma: h.chroma_bits as u8,
            length_size: 4,
            arrays: [(33, sps), (34, pps)]
                .into_iter()
                .filter(|(_, nalus)| !nalus.is_empty())
                .map(|(nal_type, nalus)| NalArray {
                    completeness: true,
                    nal_type,
                    nalus,
                })
                .collect(),
            ..HevcConfig::default()
        }
        .to_bytes()
    }

    fn hevc_of(h: &Hevc) -> Variant {
        of(
            Some(Codec::Hevc),
            &hvcc_of(h, vec![hevc_sps(h)], Vec::new()),
            None,
        )
        .unwrap()
    }

    fn v(bit_depth: u8, chroma: Chroma, interlaced: bool, tools: Option<&'static str>) -> Variant {
        Variant {
            bit_depth,
            chroma: Some(chroma),
            interlaced,
            tools,
        }
    }

    #[test]
    fn a_plain_high_sps_reads_8_bit_4_2_0_progressive() {
        assert_eq!(h264_of(&HIGH), v(8, Chroma::Yuv420, false, None));
    }

    #[test]
    fn a_baseline_sps_without_the_high_fields_reads_8_bit_4_2_0() {
        let a = Avc {
            profile: 66,
            ..HIGH
        };
        assert_eq!(h264_of(&a), v(8, Chroma::Yuv420, false, None));
    }

    #[test]
    fn a_high_4_2_2_10_bit_sps_reads_its_depth_and_chroma() {
        let a = Avc {
            profile: 122,
            chroma: 2,
            luma: 10,
            chroma_bits: 10,
            ..HIGH
        };
        assert_eq!(h264_of(&a), v(10, Chroma::Yuv422, false, None));
    }

    #[test]
    fn a_high_10_sps_at_8_bit_reads_8_bit() {
        let a = Avc {
            profile: 110,
            ..HIGH
        };
        assert_eq!(h264_of(&a), v(8, Chroma::Yuv420, false, None));
    }

    #[test]
    fn an_interlaced_sps_reads_interlaced() {
        let a = Avc {
            frame_mbs_only: false,
            ..HIGH
        };
        assert_eq!(h264_of(&a), v(8, Chroma::Yuv420, true, None));
    }

    #[test]
    fn a_4_4_4_lossless_sps_names_its_coding() {
        let a = Avc {
            profile: 244,
            chroma: 3,
            lossless: true,
            ..HIGH
        };
        assert_eq!(
            h264_of(&a),
            v(8, Chroma::Yuv444, false, Some("lossless coding"))
        );
    }

    #[test]
    fn a_monochrome_sps_and_mixed_depths_read_as_such() {
        let mono = Avc { chroma: 0, ..HIGH };
        assert_eq!(h264_of(&mono), v(8, Chroma::Mono, false, None));
        let mixed = Avc {
            profile: 110,
            chroma_bits: 10,
            ..HIGH
        };
        assert_eq!(
            h264_of(&mixed),
            v(8, Chroma::Yuv420, false, Some("mixed bit depths"))
        );
    }

    #[test]
    fn an_avcc_without_an_sps_reads_the_keyframe_in_either_framing() {
        let a = Avc {
            profile: 122,
            chroma: 2,
            luma: 10,
            chroma_bits: 10,
            ..HIGH
        };
        let sps = avc_sps(&a);
        let empty = AvcConfig::new(Vec::new(), Vec::new()).to_bytes();
        assert!(needs_keyframe(Some(Codec::H264), &empty));
        assert!(needs_keyframe(Some(Codec::H264), &[]));
        assert!(!needs_keyframe(Some(Codec::H264), &avcc(sps.clone())));
        let mut prefixed = (sps.len() as u32).to_be_bytes().to_vec();
        prefixed.extend(&sps);
        prefixed.extend([0, 0, 0, 2, 0x65, 0x88]);
        let mut annexb = vec![0, 0, 0, 1];
        annexb.extend(&sps);
        annexb.extend([0, 0, 1, 0x65, 0x88]);
        for key in [prefixed, annexb] {
            assert_eq!(
                of(Some(Codec::H264), &empty, Some(&key)),
                Some(v(10, Chroma::Yuv422, false, None))
            );
            assert_eq!(
                of(Some(Codec::H264), &[], Some(&key)),
                Some(v(10, Chroma::Yuv422, false, None))
            );
        }
        assert_eq!(of(Some(Codec::H264), &empty, None), None);
        assert_eq!(
            of(Some(Codec::H264), &empty, Some(&[0, 0, 0, 2, 0x65, 0x88])),
            None
        );
    }

    #[test]
    fn a_broken_sps_reads_nothing_rather_than_panicking() {
        for bad in [
            &[][..],
            &[0x67][..],
            &[0x67, 0xff][..],
            &[0x67, 100, 0, 40][..],
        ] {
            assert_eq!(h264(&[bad]), None, "{bad:?}");
        }
        assert_eq!(hevc(&[&[0x42]]), None);
        assert_eq!(hevc(&[&[0x42, 0x01, 0xff]]), None);
        assert_eq!(hevc(&[]), None);
        assert_eq!(h264(&[]), None);
        assert_eq!(
            of(Some(Codec::H264), &[1, 2, 3], Some(&[0, 0, 0, 9, 1])),
            None
        );
    }

    #[test]
    fn a_main_hvcc_reads_8_bit_4_2_0() {
        assert_eq!(hevc_of(&MAIN), v(8, Chroma::Yuv420, false, None));
    }

    #[test]
    fn a_main_10_hvcc_reads_10_bit_4_2_0() {
        let h = Hevc {
            profile: 2,
            luma: 10,
            chroma_bits: 10,
            ..MAIN
        };
        assert_eq!(hevc_of(&h), v(10, Chroma::Yuv420, false, None));
    }

    #[test]
    fn an_rext_4_2_2_hvcc_reads_its_chroma_and_tools() {
        let plain = Hevc {
            profile: 4,
            chroma: 2,
            luma: 10,
            chroma_bits: 10,
            rext: false,
        };
        assert_eq!(hevc_of(&plain), v(10, Chroma::Yuv422, false, None));
        let tools = Hevc {
            rext: true,
            ..plain
        };
        assert_eq!(
            hevc_of(&tools),
            v(10, Chroma::Yuv422, false, Some("range extension tools"))
        );
    }

    #[test]
    fn a_12_bit_4_4_4_hvcc_reads_12_bit_4_4_4() {
        let h = Hevc {
            profile: 4,
            chroma: 3,
            luma: 12,
            chroma_bits: 12,
            rext: false,
        };
        assert_eq!(hevc_of(&h), v(12, Chroma::Yuv444, false, None));
    }

    #[test]
    fn an_hvcc_with_an_unreadable_sps_falls_back_to_its_own_fields() {
        let h = Hevc {
            profile: 4,
            chroma: 2,
            luma: 12,
            chroma_bits: 10,
            rext: false,
        };
        let config = hvcc_of(&h, vec![vec![0x42, 0x01, 0xff]], Vec::new());
        assert_eq!(
            of(Some(Codec::Hevc), &config, None),
            Some(v(12, Chroma::Yuv422, false, Some("mixed bit depths")))
        );
    }

    #[test]
    fn an_hvcc_without_an_sps_reads_the_keyframe() {
        let h = Hevc {
            profile: 2,
            luma: 10,
            chroma_bits: 10,
            ..MAIN
        };
        let sps = hevc_sps(&h);
        let config = HevcConfig {
            length_size: 4,
            ..HevcConfig::default()
        }
        .to_bytes();
        assert!(needs_keyframe(Some(Codec::Hevc), &config));
        assert!(!needs_keyframe(
            Some(Codec::Hevc),
            &hvcc_of(&h, vec![sps.clone()], Vec::new())
        ));
        let mut key = (sps.len() as u32).to_be_bytes().to_vec();
        key.extend(&sps);
        assert_eq!(
            of(Some(Codec::Hevc), &config, Some(&key)),
            Some(v(10, Chroma::Yuv420, false, None))
        );
    }

    #[test]
    fn vpcc_and_av1c_read_their_chroma_and_depth() {
        let vp9 = |profile, bit_depth, chroma| {
            VpcConfig::new(
                (profile, bit_depth, chroma),
                (2, false),
                (640, 360),
                (30, 1),
            )
            .to_bytes()
        };
        assert!(!needs_keyframe(Some(Codec::Vp9), &vp9(0, 8, 1)));
        assert!(needs_keyframe(Some(Codec::Vp9), &[]));
        for (bytes, want) in [
            (vp9(0, 8, 1), v(8, Chroma::Yuv420, false, None)),
            (vp9(2, 10, 0), v(10, Chroma::Yuv420, false, None)),
            (vp9(1, 8, 3), v(8, Chroma::Yuv444, false, None)),
            (vp9(3, 12, 2), v(12, Chroma::Yuv422, false, None)),
        ] {
            assert_eq!(of(Some(Codec::Vp9), &bytes, None), Some(want));
        }
        let av1 = |high, twelve, mono, x, y| {
            Av1Config {
                high_bitdepth: high,
                twelve_bit: twelve,
                monochrome: mono,
                chroma_subsampling_x: x,
                chroma_subsampling_y: y,
                ..Av1Config::default()
            }
            .to_bytes()
        };
        for (bytes, want) in [
            (
                av1(false, false, false, true, true),
                v(8, Chroma::Yuv420, false, None),
            ),
            (
                av1(true, false, false, true, true),
                v(10, Chroma::Yuv420, false, None),
            ),
            (
                av1(true, true, false, false, false),
                v(12, Chroma::Yuv444, false, None),
            ),
            (
                av1(false, false, true, true, true),
                v(8, Chroma::Mono, false, None),
            ),
            (
                av1(true, false, false, true, false),
                v(10, Chroma::Yuv422, false, None),
            ),
        ] {
            assert_eq!(of(Some(Codec::Av1), &bytes, None), Some(want));
        }
        assert_eq!(of(Some(Codec::Av1), &[], None), None);
        assert_eq!(of(None, &[], None), None);
        assert!(!needs_keyframe(Some(Codec::Av1), &[]));
    }

    fn avc_pps(slice_groups: u32) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(slice_groups - 1);
        if slice_groups > 1 {
            w.write_ue(0);
            for _ in 0..slice_groups {
                w.write_ue(0);
            }
        }
        w.write_ue(0);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bits(0, 2);
        w.write_se(0);
        w.write_se(0);
        w.write_se(0);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.rbsp_trailing();
        let mut nal = vec![0x68];
        nal.extend(escape_rbsp(&w.finish()));
        nal
    }

    #[test]
    fn a_baseline_avcc_with_slice_groups_names_them_and_one_without_does_not() {
        let base = Avc {
            profile: 66,
            ..HIGH
        };
        let config =
            |groups| AvcConfig::new(vec![avc_sps(&base)], vec![avc_pps(groups)]).to_bytes();
        assert_eq!(
            of(Some(Codec::H264), &config(2), None),
            Some(v(8, Chroma::Yuv420, false, Some("slice groups")))
        );
        assert_eq!(
            of(Some(Codec::H264), &config(1), None),
            Some(v(8, Chroma::Yuv420, false, None))
        );
        let mut key = Vec::new();
        for nal in [avc_sps(&base), avc_pps(3)] {
            key.extend((nal.len() as u32).to_be_bytes());
            key.extend(nal);
        }
        assert_eq!(
            of(Some(Codec::H264), &[], Some(&key)),
            Some(v(8, Chroma::Yuv420, false, Some("slice groups")))
        );
    }

    fn hevc_pps(sps_id: u32, range: bool, layers: bool) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_ue(0);
        w.write_ue(sps_id);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bits(0, 3);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(0);
        w.write_ue(0);
        w.write_se(0);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_se(0);
        w.write_se(0);
        for _ in 0..6 {
            w.write_bit(false);
        }
        w.write_bit(true);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_ue(0);
        w.write_bit(false);
        w.write_bit(range || layers);
        if range || layers {
            w.write_bits(u32::from(range) << 7 | u32::from(layers) << 6, 8);
            if range {
                w.write_bit(false);
                w.write_bit(true);
            }
        }
        w.rbsp_trailing();
        let mut nal = vec![0x44, 0x01];
        nal.extend(escape_rbsp(&w.finish()));
        nal
    }

    #[test]
    fn an_hvcc_pps_with_range_or_multilayer_extensions_names_them() {
        let with = |pps| {
            of(
                Some(Codec::Hevc),
                &hvcc_of(&MAIN, vec![hevc_sps(&MAIN)], vec![pps]),
                None,
            )
        };
        assert_eq!(
            with(hevc_pps(0, false, false)),
            Some(v(8, Chroma::Yuv420, false, None))
        );
        assert_eq!(
            with(hevc_pps(0, true, false)),
            Some(v(8, Chroma::Yuv420, false, Some("range extension tools")))
        );
        assert_eq!(
            with(hevc_pps(0, false, true)),
            Some(v(8, Chroma::Yuv420, false, Some("multilayer extensions")))
        );
        assert_eq!(
            with(hevc_pps(1, true, true)),
            Some(v(8, Chroma::Yuv420, false, None))
        );
        assert_eq!(
            with(vec![0x44, 0x01, 0xff]),
            Some(v(8, Chroma::Yuv420, false, None))
        );
    }

    #[test]
    fn a_length_prefixed_keyframe_that_looks_like_annex_b_still_reads_its_sps() {
        let a = Avc {
            profile: 122,
            chroma: 2,
            luma: 10,
            chroma_bits: 10,
            ..HIGH
        };
        let sps = avc_sps(&a);
        let mut filler = vec![0x0c];
        filler.resize(300, 0xff);
        let mut key = Vec::new();
        for nal in [filler, sps] {
            key.extend((nal.len() as u32).to_be_bytes());
            key.extend(nal);
        }
        assert!(key.starts_with(&[0, 0, 1]));
        let empty = AvcConfig::new(Vec::new(), Vec::new()).to_bytes();
        assert_eq!(
            of(Some(Codec::H264), &empty, Some(&key)),
            Some(v(10, Chroma::Yuv422, false, None))
        );
        let h = Hevc {
            profile: 4,
            chroma: 3,
            luma: 12,
            chroma_bits: 12,
            rext: false,
        };
        let mut filler = vec![0x4c, 0x01];
        filler.resize(400, 0xff);
        let mut key = Vec::new();
        for nal in [filler, hevc_sps(&h)] {
            key.extend((nal.len() as u32).to_be_bytes());
            key.extend(nal);
        }
        assert!(key.starts_with(&[0, 0, 1]));
        let config = HevcConfig {
            length_size: 4,
            ..HevcConfig::default()
        }
        .to_bytes();
        assert_eq!(
            of(Some(Codec::Hevc), &config, Some(&key)),
            Some(v(12, Chroma::Yuv444, false, None))
        );
    }

    #[cfg(feature = "vp9")]
    fn vp9_key(profile: u32, twelve: bool, x: bool, y: bool) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(2, 2);
        w.write_bit(profile & 1 != 0);
        w.write_bit(profile & 2 != 0);
        if profile == 3 {
            w.write_bit(false);
        }
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(true);
        w.write_bit(false);
        w.write_bits(0x49_83_42, 24);
        if profile >= 2 {
            w.write_bit(twelve);
        }
        w.write_bits(2, 3);
        w.write_bit(false);
        if profile == 1 || profile == 3 {
            w.write_bit(x);
            w.write_bit(y);
            w.write_bit(false);
        }
        w.write_bits(351, 16);
        w.write_bits(239, 16);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bit(false);
        w.write_bits(0, 2);
        w.write_bits(10, 6);
        w.write_bits(0, 3);
        w.write_bit(false);
        w.write_bits(60, 8);
        for _ in 0..4 {
            w.write_bit(false);
        }
        w.write_bit(false);
        w.write_bits(1, 16);
        w.align_zero();
        let mut frame = w.finish();
        frame.extend([0; 8]);
        frame
    }

    #[cfg(feature = "vp9")]
    #[test]
    fn a_vp9_keyframe_reads_its_profile_depth_and_chroma() {
        for (key, want) in [
            (
                vp9_key(0, false, true, true),
                v(8, Chroma::Yuv420, false, None),
            ),
            (
                vp9_key(1, false, false, false),
                v(8, Chroma::Yuv444, false, None),
            ),
            (
                vp9_key(1, false, true, false),
                v(8, Chroma::Yuv422, false, None),
            ),
            (
                vp9_key(1, false, false, true),
                v(8, Chroma::Yuv440, false, None),
            ),
            (
                vp9_key(2, false, true, true),
                v(10, Chroma::Yuv420, false, None),
            ),
            (
                vp9_key(2, true, true, true),
                v(12, Chroma::Yuv420, false, None),
            ),
            (
                vp9_key(3, true, true, false),
                v(12, Chroma::Yuv422, false, None),
            ),
        ] {
            assert_eq!(of(Some(Codec::Vp9), &[], Some(&key)), Some(want));
        }
        let mut inter = vp9_key(0, false, true, true);
        inter[0] |= 0x04;
        assert_eq!(of(Some(Codec::Vp9), &[], Some(&inter)), None);
        assert_eq!(of(Some(Codec::Vp9), &[], Some(&[0xff; 4])), None);
        assert_eq!(of(Some(Codec::Vp9), &[], Some(&[])), None);
        assert_eq!(of(Some(Codec::Vp9), &[], None), None);
    }
}
