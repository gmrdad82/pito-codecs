use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};

use super::base::{Key, key};
use super::route::Prefs;
use super::variant::Chroma;
use crate::media::{Codec, Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub duration: f64,
    pub start: f64,
    pub video_start: Option<f64>,
    pub fps: (u32, u32),
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
    pub video_codec: Option<Codec>,
    pub video_codec_name: String,
    pub profile: String,
    pub bit_depth: u8,
    pub chroma: Option<Chroma>,
    pub interlaced: bool,
    pub(crate) tools: Option<&'static str>,
    pub frames: Option<u64>,
    pub audio: Vec<AudioTrack>,
    pub alpha: bool,
}

impl Probe {
    pub fn fps(&self) -> f64 {
        self.fps.0 as f64 / self.fps.1.max(1) as f64
    }

    pub fn shown_size(&self) -> (u32, u32) {
        if self.rotation % 180 == 90 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    pub fn video_native(&self) -> bool {
        self.codec_native() && self.variant_native()
    }

    fn codec_native(&self) -> bool {
        match self.video_codec {
            Some(Codec::H264) => cfg!(feature = "h264") || gpu_decodes(Codec::H264),
            Some(Codec::Hevc) => cfg!(feature = "hevc") || gpu_decodes(Codec::Hevc),
            Some(Codec::Vp9) => cfg!(feature = "vp9"),
            Some(Codec::Av1) => cfg!(feature = "av1") || gpu_decodes(Codec::Av1),
            _ => false,
        }
    }

    pub(crate) fn variant_native(&self) -> bool {
        let plain = matches!(self.chroma, None | Some(Chroma::Yuv420)) && self.tools.is_none();
        match self.video_codec {
            Some(Codec::H264) => plain && !self.interlaced && self.bit_depth <= 8,
            Some(_) => plain && matches!(self.bit_depth, 0..=8 | 10),
            None => false,
        }
    }

    pub fn video_variant(&self) -> String {
        let codec = match self.video_codec {
            Some(Codec::H264) => "H.264",
            Some(Codec::Hevc) => "HEVC",
            Some(Codec::Vp9) => "VP9",
            Some(Codec::Av1) => "AV1",
            _ => plain(&self.video_codec_name),
        };
        if codec.is_empty() {
            return String::new();
        }
        let mut words: Vec<String> = Vec::new();
        if self.interlaced {
            words.push("interlaced".into());
        }
        if self.bit_depth > 0 {
            words.push(format!("{}-bit", self.bit_depth));
        }
        if let Some(c) = self.chroma {
            words.push(c.word().into());
        }
        words.push(codec.into());
        if let Some(t) = self.tools {
            words.push(format!("with {t}"));
        }
        words.join(" ")
    }

    pub fn unsupported(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let video = (self.has_video() && !self.video_native()).then(|| {
            if self.codec_native() {
                self.video_variant()
            } else {
                plain(&self.video_codec_name).to_string()
            }
        });
        let audio = self
            .audio
            .iter()
            .filter(|a| !a.native())
            .map(AudioTrack::plain_variant);
        for name in video.into_iter().chain(audio) {
            let name = if name.is_empty() {
                "unknown".to_string()
            } else {
                name
            };
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }

    fn has_video(&self) -> bool {
        self.video_codec.is_some() || !self.video_codec_name.is_empty() || self.width > 0
    }

    pub(crate) fn known_video(&self) -> Result<Codec> {
        if let Some(codec) = self.video_codec {
            return Ok(codec);
        }
        if !self.has_video() {
            return Err(Error::Invalid("no video track".to_string()));
        }
        let name = match plain(&self.video_codec_name) {
            "" => "unknown",
            name => name,
        };
        Err(Error::Unsupported(format!("{name} video is not supported")))
    }

    pub(crate) fn decodable(&self) -> Result<()> {
        self.known_video()?;
        if self.variant_native() {
            return Ok(());
        }
        Err(Error::Unsupported(format!(
            "{} video is not supported",
            self.video_variant()
        )))
    }
}

const PLAIN: [(&[&str], &str); 24] = [
    (
        &[
            "mp4v",
            "mpeg4",
            "V_MPEG4/ISO/ASP",
            "V_MPEG4/ISO/SP",
            "V_MPEG4/ISO/AP",
            "XVID",
            "xvid",
            "DIVX",
            "divx",
            "DX50",
            "FMP4",
            "MP4V",
        ],
        "MPEG-4 Part 2",
    ),
    (
        &[
            "mpeg2video",
            "V_MPEG2",
            "mp2v",
            "m2v1",
            "MPG2",
            "hdv2",
            "hdv3",
            "hdv5",
            "hdv6",
        ],
        "MPEG-2",
    ),
    (&["mpeg1video", "V_MPEG1", "m1v1", "MPG1"], "MPEG-1"),
    (
        &[
            "apch", "apcn", "apcs", "apco", "ap4h", "ap4x", "V_PRORES", "prores",
        ],
        "ProRes",
    ),
    (
        &[
            "mjpeg", "jpeg", "mjpa", "mjpb", "MJPG", "AVRn", "dmb1", "V_MJPEG",
        ],
        "MJPEG",
    ),
    (&["AVdn", "AVdh", "dnxhd", "V_DNXHD"], "DNxHD"),
    (&["vp08", "vp8", "VP80"], "VP8"),
    (&["V_THEORA", "theora"], "Theora"),
    (&["s263", "h263", "H263"], "H.263"),
    (&["vc-1", "vc1", "WVC1", "WMV3"], "VC-1"),
    (&["WMV1", "WMV2"], "Windows Media Video"),
    (&["cvid"], "Cinepak"),
    (&["SVQ1", "SVQ3"], "Sorenson Video"),
    (&["V_FFV1", "FFV1"], "FFV1"),
    (&["H264", "X264", "x264", "AVC1", "DAVC"], "VFW-mode H.264"),
    (&["HEVC", "H265", "X265", "x265", "HVC1"], "VFW-mode HEVC"),
    (
        &["V_UNCOMPRESSED", "raw", "2vuy", "yuv2", "v210", "v410"],
        "uncompressed video",
    ),
    (&["vorbis", "A_VORBIS"], "Vorbis"),
    (&["flac", "fLaC", "A_FLAC"], "FLAC"),
    (&["alac", "A_ALAC"], "ALAC"),
    (&["A_TRUEHD", "mlpa", "truehd"], "TrueHD"),
    (&["A_DTS", "dtsc", "dtsh", "dtsl", "dtse", "dts"], "DTS"),
    (&["samr"], "AMR-NB"),
    (&["sawb"], "AMR-WB"),
];

pub(crate) fn plain(id: &str) -> &str {
    PLAIN
        .iter()
        .find(|(ids, _)| ids.contains(&id))
        .map_or(id, |(_, name)| name)
}

fn gpu_decodes(codec: Codec) -> bool {
    super::base::gpu()
        .0
        .contains(&(codec, crate::media::Direction::Decode))
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioTrack {
    pub index: usize,
    pub stream: usize,
    pub title: Option<String>,
    pub language: Option<String>,
    pub codec: String,
    pub channels: u16,
    pub rate: u32,
    pub bitrate: Option<u32>,
    pub default: bool,
    pub start: Option<f64>,
    pub bits: u8,
    pub refused: Option<String>,
}

impl AudioTrack {
    pub fn variant(&self) -> String {
        if let Some(r) = &self.refused {
            r.clone()
        } else if self.bits > 0 && self.codec.eq_ignore_ascii_case("flac") {
            format!("{}-bit FLAC", self.bits)
        } else {
            self.codec.clone()
        }
    }

    fn plain_variant(&self) -> String {
        match self.variant() {
            v if v == self.codec => plain(&self.codec).to_string(),
            v => v,
        }
    }

    pub(crate) fn decodable(&self) -> Result<()> {
        if self.native() {
            return Ok(());
        }
        Err(Error::Unsupported(match self.variant() {
            v if v == self.codec => match plain(&self.codec) {
                "" => "unknown audio is not supported".to_string(),
                name => format!("{name} audio is not supported"),
            },
            v => v,
        }))
    }

    pub fn native(&self) -> bool {
        let native: &[&str] = &["aac", "pcm", "alaw", "ulaw"];
        let ac3: &[&str] = if cfg!(feature = "ac3") {
            &["ac3", "eac3"]
        } else {
            &[]
        };
        let mp3: &[&str] = if cfg!(feature = "mp3") {
            &["mp3", "mp2", "mp1"]
        } else {
            &[]
        };
        let flac: &[&str] = if cfg!(feature = "flac") {
            &["flac"]
        } else {
            &[]
        };
        let alac: &[&str] = if cfg!(feature = "alac") {
            &["alac"]
        } else {
            &[]
        };
        let refused =
            self.refused.is_some() || (self.codec.eq_ignore_ascii_case("flac") && self.bits > 31);
        !refused
            && native
                .iter()
                .chain(["opus"].iter().filter(|_| cfg!(feature = "opus")))
                .chain(ac3)
                .chain(mp3)
                .chain(flac)
                .chain(alac)
                .any(|c| self.codec.eq_ignore_ascii_case(c))
    }
}

#[cfg(feature = "alac")]
fn alac(cookie: Option<&[u8]>, rate: u32) -> (String, u32, Option<String>) {
    match crate::alac::refused(cookie) {
        Some(variant) => ("alac".into(), rate, Some(variant)),
        None => {
            let cookie_rate = cookie
                .and_then(|c| crate::alac::Config::parse(c).ok())
                .map_or(rate, |c| c.sample_rate);
            ("alac".into(), cookie_rate, None)
        }
    }
}

pub(super) fn mp4_audio_codec(t: &crate::mp4::Track) -> (String, u32, Option<String>) {
    let name = t.config().map(|c| c.name()).unwrap_or_default();
    #[cfg(feature = "alac")]
    if name == "alac" {
        return alac(
            t.entry().and_then(crate::alac::entry_cookie),
            t.sample_rate(),
        );
    }
    #[cfg(feature = "flac")]
    if name == "flac"
        && let Some(i) = t
            .entry()
            .and_then(|e| super::sound::child(e, b"dfLa"))
            .and_then(|c| crate::flac::config(c).ok())
    {
        return (name, i.rate, None);
    }
    (name, t.sample_rate(), None)
}

pub(super) fn mkv_audio_codec(t: &crate::mkv::Track) -> (String, u32, Option<String>) {
    let name = t.codec_name();
    if name == "A_MS/ACM" {
        let rate = super::sound::acm_format(&t.codec_private).map_or(t.sample_rate(), |f| f.2);
        return (
            super::sound::acm_name(&t.codec_private).to_string(),
            rate,
            super::sound::acm_refused(&t.codec_private),
        );
    }
    #[cfg(feature = "alac")]
    if name == "alac" {
        return alac(Some(&t.codec_private), t.sample_rate());
    }
    (name.to_string(), t.sample_rate(), None)
}

pub(super) fn mkv_channels(t: &crate::mkv::Track) -> u16 {
    let container = t.channels() as u16;
    if t.codec_name() == "A_MS/ACM" {
        super::sound::acm_channels(&t.codec_private, container)
    } else {
        container
    }
}

#[derive(Default)]
struct Known {
    probe: Option<Probe>,
    keys: Option<Vec<f64>>,
}

fn known<T: Clone>(
    path: &Path,
    field: fn(&mut Known) -> &mut Option<T>,
    make: impl FnOnce() -> Result<T>,
) -> Result<T> {
    static KNOWN: LazyLock<Mutex<HashMap<Key, Known>>> = LazyLock::new(Default::default);
    let k = key(path)?;
    if let Some(v) = KNOWN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_mut(&k)
        .and_then(|e| field(e).clone())
    {
        return Ok(v);
    }
    let v = make()?;
    let mut map = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() > 4096 {
        map.clear();
    }
    *field(map.entry(k).or_default()) = Some(v.clone());
    Ok(v)
}

pub fn probe(path: &Path, _: &Prefs) -> Result<Probe> {
    known(path, |k| &mut k.probe, || super::base::probe(path))
}

pub fn keyframes(path: &Path, _: &Prefs) -> Result<Vec<f64>> {
    known(path, |k| &mut k.keys, || super::base::keyframes(path))
}

pub fn frame_count(path: &Path, _: &Prefs) -> Result<u64> {
    super::demux::frame_count(path)
}

#[cfg(feature = "flac")]
pub(super) fn flac_bits(config: &[u8]) -> u8 {
    crate::flac::config(config).map_or(0, |i| i.bits)
}

#[cfg(not(feature = "flac"))]
pub(super) fn flac_bits(_: &[u8]) -> u8 {
    0
}

#[cfg(all(test, feature = "h264", feature = "hevc"))]
mod tests {
    use super::*;

    fn video(codec: Codec, bit_depth: u8, chroma: Option<Chroma>, interlaced: bool) -> Probe {
        Probe {
            duration: 1.0,
            start: 0.0,
            video_start: Some(0.0),
            fps: (30, 1),
            width: 352,
            height: 240,
            rotation: 0,
            video_codec: Some(codec),
            video_codec_name: match codec {
                Codec::H264 => "h264",
                Codec::Hevc => "hevc",
                Codec::Vp9 => "vp9",
                _ => "av1",
            }
            .into(),
            profile: String::new(),
            bit_depth,
            chroma,
            interlaced,
            tools: None,
            frames: Some(30),
            audio: Vec::new(),
            alpha: false,
        }
    }

    fn native(p: &Probe) -> bool {
        p.variant_native()
    }

    #[test]
    fn plain_variants_name_depth_chroma_and_codec() {
        let cases = [
            (Codec::H264, 8, Chroma::Yuv420, false, "8-bit 4:2:0 H.264"),
            (Codec::H264, 10, Chroma::Yuv422, false, "10-bit 4:2:2 H.264"),
            (
                Codec::H264,
                8,
                Chroma::Yuv420,
                true,
                "interlaced 8-bit 4:2:0 H.264",
            ),
            (Codec::Hevc, 12, Chroma::Yuv444, false, "12-bit 4:4:4 HEVC"),
            (Codec::Hevc, 10, Chroma::Yuv420, false, "10-bit 4:2:0 HEVC"),
            (Codec::Vp9, 8, Chroma::Yuv440, false, "8-bit 4:4:0 VP9"),
            (Codec::Av1, 8, Chroma::Mono, false, "8-bit monochrome AV1"),
        ];
        for (codec, depth, chroma, interlaced, name) in cases {
            assert_eq!(
                video(codec, depth, Some(chroma), interlaced).video_variant(),
                name
            );
        }
        let mut p = video(Codec::Hevc, 10, Some(Chroma::Yuv420), false);
        p.tools = Some("range extension tools");
        assert_eq!(
            p.video_variant(),
            "10-bit 4:2:0 HEVC with range extension tools"
        );
        assert_eq!(video(Codec::H264, 0, None, false).video_variant(), "H.264");
        let mut other = video(Codec::H264, 8, Some(Chroma::Yuv420), false);
        other.video_codec = None;
        other.video_codec_name = "mpeg2video".into();
        assert_eq!(other.video_variant(), "8-bit 4:2:0 MPEG-2");
        other.video_codec_name.clear();
        assert_eq!(other.video_variant(), "");
    }

    #[test]
    fn h264_takes_only_progressive_8_bit_4_2_0() {
        let yes = |d, c, i| native(&video(Codec::H264, d, c, i));
        assert!(yes(8, Some(Chroma::Yuv420), false));
        assert!(yes(8, None, false));
        assert!(!yes(8, Some(Chroma::Yuv420), true));
        assert!(!yes(10, Some(Chroma::Yuv420), false));
        assert!(!yes(10, Some(Chroma::Yuv422), false));
        assert!(!yes(8, Some(Chroma::Yuv422), false));
        assert!(!yes(8, Some(Chroma::Yuv444), false));
        assert!(!yes(8, Some(Chroma::Mono), false));
        let mut lossless = video(Codec::H264, 8, Some(Chroma::Yuv420), false);
        lossless.tools = Some("lossless coding");
        assert!(!native(&lossless));
        lossless.tools = Some("slice groups");
        assert!(!native(&lossless));
    }

    #[test]
    fn hevc_vp9_and_av1_take_8_and_10_bit_4_2_0_only() {
        for codec in [Codec::Hevc, Codec::Vp9, Codec::Av1] {
            let yes = |d, c| native(&video(codec, d, c, false));
            assert!(yes(8, Some(Chroma::Yuv420)), "{codec:?}");
            assert!(yes(10, Some(Chroma::Yuv420)), "{codec:?}");
            assert!(yes(10, None), "{codec:?}");
            assert!(!yes(12, Some(Chroma::Yuv420)), "{codec:?}");
            assert!(!yes(8, Some(Chroma::Yuv422)), "{codec:?}");
            assert!(!yes(10, Some(Chroma::Yuv444)), "{codec:?}");
            assert!(!yes(8, Some(Chroma::Yuv440)), "{codec:?}");
            assert!(!yes(8, Some(Chroma::Mono)), "{codec:?}");
        }
        let mut tools = video(Codec::Hevc, 8, Some(Chroma::Yuv420), false);
        tools.tools = Some("multilayer extensions");
        assert!(!native(&tools));
    }

    #[test]
    fn unsupported_names_the_variant_when_the_codec_has_a_route() {
        let p = video(Codec::H264, 10, Some(Chroma::Yuv422), false);
        assert!(!p.video_native());
        assert_eq!(p.unsupported(), ["10-bit 4:2:2 H.264"]);
        let p = video(Codec::Hevc, 12, Some(Chroma::Yuv444), false);
        assert_eq!(p.unsupported(), ["12-bit 4:4:4 HEVC"]);
        let mut p = video(Codec::H264, 8, Some(Chroma::Yuv420), false);
        assert!(p.video_native());
        assert!(p.unsupported().is_empty());
        p.video_codec = None;
        p.video_codec_name = "mpeg2video".into();
        assert_eq!(p.unsupported(), ["MPEG-2"]);
    }

    fn track(codec: &str) -> AudioTrack {
        AudioTrack {
            index: 0,
            stream: 1,
            title: None,
            language: None,
            codec: codec.into(),
            channels: 2,
            rate: 48_000,
            bitrate: None,
            default: true,
            start: None,
            bits: 0,
            refused: None,
        }
    }

    fn audio_only(codecs: &[&str]) -> Probe {
        let mut p = video(Codec::H264, 0, None, false);
        p.video_codec = None;
        p.video_codec_name.clear();
        p.width = 0;
        p.height = 0;
        p.audio = codecs.iter().map(|c| track(c)).collect();
        p
    }

    #[test]
    fn readers_ids_read_as_names_a_person_knows() {
        for (id, name) in [
            ("mp4v", "MPEG-4 Part 2"),
            ("V_MPEG4/ISO/ASP", "MPEG-4 Part 2"),
            ("XVID", "MPEG-4 Part 2"),
            ("mpeg2video", "MPEG-2"),
            ("V_MPEG2", "MPEG-2"),
            ("mpeg1video", "MPEG-1"),
            ("apch", "ProRes"),
            ("ap4x", "ProRes"),
            ("jpeg", "MJPEG"),
            ("mjpeg", "MJPEG"),
            ("MJPG", "MJPEG"),
            ("AVdn", "DNxHD"),
            ("vp8", "VP8"),
            ("V_THEORA", "Theora"),
            ("WVC1", "VC-1"),
            ("vorbis", "Vorbis"),
            ("A_TRUEHD", "TrueHD"),
            ("A_DTS", "DTS"),
            ("alac", "ALAC"),
            ("H264", "VFW-mode H.264"),
            ("x264", "VFW-mode H.264"),
            ("HEVC", "VFW-mode HEVC"),
        ] {
            assert_eq!(plain(id), name, "{id}");
        }
        for id in [
            "h264",
            "hevc",
            "avc1",
            "hvc1",
            "hev1",
            "av1",
            "aac",
            "opus",
            "",
            "zzzz",
            "V_SOMETHING",
        ] {
            assert_eq!(plain(id), id);
        }
        for (ids, name) in PLAIN {
            assert!(!name.is_empty());
            assert!(ids.iter().all(|id| plain(id) == name), "{name}");
        }
    }

    #[test]
    fn a_codec_no_reader_knows_is_refused_by_its_plain_name() {
        let mut p = video(Codec::H264, 8, Some(Chroma::Yuv420), false);
        p.video_codec = None;
        for (id, said) in [
            ("mp4v", "unsupported: MPEG-4 Part 2 video is not supported"),
            ("V_MPEG2", "unsupported: MPEG-2 video is not supported"),
            ("apcn", "unsupported: ProRes video is not supported"),
            ("H264", "unsupported: VFW-mode H.264 video is not supported"),
            (
                "V_SOMETHING",
                "unsupported: V_SOMETHING video is not supported",
            ),
            ("", "unsupported: unknown video is not supported"),
        ] {
            p.video_codec_name = id.into();
            assert_eq!(p.known_video().unwrap_err().to_string(), said);
            assert_eq!(p.decodable().unwrap_err().to_string(), said);
            assert!(!p.video_native());
        }
        p.video_codec_name = "mp4v".into();
        assert_eq!(p.unsupported(), ["MPEG-4 Part 2"]);
    }

    #[test]
    fn a_variant_no_decoder_takes_is_refused_at_open_and_a_plain_one_opens() {
        let p = video(Codec::H264, 10, Some(Chroma::Yuv422), false);
        assert_eq!(p.known_video().unwrap(), Codec::H264);
        assert_eq!(
            p.decodable().unwrap_err().to_string(),
            "unsupported: 10-bit 4:2:2 H.264 video is not supported"
        );
        let p = video(Codec::H264, 8, Some(Chroma::Yuv420), true);
        assert_eq!(
            p.decodable().unwrap_err().to_string(),
            "unsupported: interlaced 8-bit 4:2:0 H.264 video is not supported"
        );
        for codec in [Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1] {
            assert!(
                video(codec, 8, Some(Chroma::Yuv420), false)
                    .decodable()
                    .is_ok()
            );
            assert!(video(codec, 8, None, false).decodable().is_ok());
        }
        assert!(
            video(Codec::Hevc, 10, Some(Chroma::Yuv420), false)
                .decodable()
                .is_ok()
        );
    }

    #[test]
    fn a_file_with_no_video_says_so_and_lists_only_its_audio() {
        let p = audio_only(&["aac"]);
        assert_eq!(
            p.decodable().unwrap_err().to_string(),
            "invalid data: no video track"
        );
        assert_eq!(
            p.known_video().unwrap_err().to_string(),
            "invalid data: no video track"
        );
        assert!(p.unsupported().is_empty(), "{:?}", p.unsupported());
        let p = audio_only(&["aac", "vorbis", "A_TRUEHD", "vorbis"]);
        assert_eq!(p.unsupported(), ["Vorbis", "TrueHD"]);
        let mut p = audio_only(&[]);
        p.width = 352;
        assert_eq!(
            p.decodable().unwrap_err().to_string(),
            "unsupported: unknown video is not supported"
        );
        assert_eq!(p.unsupported(), ["unknown"]);
    }
}
