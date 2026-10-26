use crate::media::{Codec, TimeBase};
use crate::mp4::TrackKind;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Mastering {
    pub primaries: [(f64, f64); 3],
    pub white_point: (f64, f64),
    pub luminance_max: f64,
    pub luminance_min: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Colour {
    pub matrix: Option<u64>,
    pub bits_per_channel: Option<u64>,
    pub chroma_siting: (Option<u64>, Option<u64>),
    pub range: Option<u64>,
    pub transfer: Option<u64>,
    pub primaries: Option<u64>,
    pub max_cll: Option<u64>,
    pub max_fall: Option<u64>,
    pub mastering: Option<Mastering>,
}

impl Colour {
    pub fn full_range(&self) -> bool {
        self.range == Some(2)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Projection {
    pub kind: u64,
    pub yaw: f64,
    pub pitch: f64,
    pub roll: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VideoInfo {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub display_width: Option<u32>,
    pub display_height: Option<u32>,
    pub display_unit: u64,
    pub crop: (u32, u32, u32, u32),
    pub interlaced: u64,
    pub colour: Option<Colour>,
    pub projection: Option<Projection>,
    pub alpha_mode: u64,
}

impl VideoInfo {
    pub fn rotation(&self) -> Option<u16> {
        let Some(p) = &self.projection else {
            return Some(0);
        };
        if p.kind != 0 || p.yaw != 0.0 || p.pitch != 0.0 || p.roll % 90.0 != 0.0 {
            return None;
        }
        Some((-(p.roll / 90.0) as i64).rem_euclid(4) as u16 * 90)
    }

    pub fn pixel_aspect(&self) -> (u32, u32) {
        let pw = self
            .pixel_width
            .saturating_sub(self.crop.2.saturating_add(self.crop.3)) as u64;
        let ph = self
            .pixel_height
            .saturating_sub(self.crop.0.saturating_add(self.crop.1)) as u64;
        let (Some(dw), Some(dh)) = (self.display_width, self.display_height) else {
            return (1, 1);
        };
        if pw == 0 || ph == 0 || dw == 0 || dh == 0 || !matches!(self.display_unit, 0 | 3) {
            return (1, 1);
        }
        let (n, d) = (dw as u64 * ph, dh as u64 * pw);
        let g = gcd(n, d);
        ((n / g) as u32, (d / g) as u32)
    }
}

pub(crate) fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioInfo {
    pub sampling_frequency: f64,
    pub output_sampling_frequency: Option<f64>,
    pub channels: u64,
    pub bit_depth: Option<u64>,
}

impl Default for AudioInfo {
    fn default() -> Self {
        Self {
            sampling_frequency: 8000.0,
            output_sampling_frequency: None,
            channels: 1,
            bit_depth: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentEncoding {
    HeaderStripping { bytes: Vec<u8>, scope: u64 },
    Compression { algo: u64, scope: u64 },
    Encryption { scope: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    pub pts: i64,
    pub duration: u32,
    pub keyframe: bool,
    pub discard: i64,
    pub cluster: u32,
    pub block: u64,
    pub lace: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub number: u64,
    pub uid: u64,
    pub kind: TrackKind,
    pub enabled: bool,
    pub default: bool,
    pub name: String,
    pub language: String,
    pub codec_id: String,
    pub codec_private: Vec<u8>,
    pub default_duration: Option<u64>,
    pub codec_delay: u64,
    pub seek_pre_roll: u64,
    pub width: u32,
    pub height: u32,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    pub encodings: Vec<ContentEncoding>,
    pub time_base: TimeBase,
    pub samples: Vec<Sample>,
    pub(crate) sync: Vec<u32>,
    pub(crate) pts_order: Vec<u32>,
}

const AAC_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

pub fn codec_name(codec_id: &str) -> &str {
    match codec_id {
        "V_MPEG4/ISO/AVC" => "h264",
        "V_MPEGH/ISO/HEVC" => "hevc",
        "V_VP8" => "vp8",
        "V_VP9" => "vp9",
        "V_AV1" => "av1",
        "A_OPUS" => "opus",
        "A_VORBIS" => "vorbis",
        "A_FLAC" => "flac",
        "A_ALAC" => "alac",
        "A_AC3" => "ac3",
        "A_EAC3" => "eac3",
        "A_MPEG/L3" => "mp3",
        "A_MPEG/L2" => "mp2",
        "A_MPEG/L1" => "mp1",
        "S_TEXT/UTF8" => "subrip",
        "S_TEXT/WEBVTT" => "webvtt",
        "S_TEXT/ASS" => "ass",
        id if id.starts_with("A_AAC") => "aac",
        id if id.starts_with("A_PCM") => "pcm",
        id => id,
    }
}

impl Track {
    pub fn codec(&self) -> Option<Codec> {
        match codec_name(&self.codec_id) {
            "h264" => Some(Codec::H264),
            "hevc" => Some(Codec::Hevc),
            "aac" => Some(Codec::Aac),
            "vp9" => Some(Codec::Vp9),
            "av1" => Some(Codec::Av1),
            "opus" => Some(Codec::Opus),
            "ac3" => Some(Codec::Ac3),
            "eac3" => Some(Codec::Eac3),
            "mp3" | "mp2" | "mp1" => Some(Codec::Mp3),
            _ => None,
        }
    }

    pub fn codec_name(&self) -> &str {
        codec_name(&self.codec_id)
    }

    pub fn extradata(&self) -> Option<Vec<u8>> {
        if !self.codec_private.is_empty() {
            return Some(self.codec_private.clone());
        }
        let id = self.codec_id.as_str();
        if !id.starts_with("A_AAC/") {
            return None;
        }
        let a = self.audio.clone().unwrap_or_default();
        let object_type: u8 = if id.ends_with("/MAIN") {
            1
        } else if id.ends_with("/SSR") {
            3
        } else if id.ends_with("/LTP") {
            4
        } else {
            2
        };
        let index = |r: f64| AAC_RATES.iter().position(|&x| x as f64 == r).unwrap_or(4) as u8;
        let ri = index(a.sampling_frequency);
        let ch = a.channels.min(7) as u8;
        let mut v = vec![(object_type << 3) | (ri >> 1), ((ri & 1) << 7) | (ch << 3)];
        if id.ends_with("/SBR") {
            let ext = index(
                a.output_sampling_frequency
                    .unwrap_or(a.sampling_frequency * 2.0),
            );
            v.extend_from_slice(&[0x56, 0xE5, 0x80 | (ext << 3)]);
        }
        Some(v)
    }

    pub fn channels(&self) -> u32 {
        self.audio.as_ref().map_or(0, |a| a.channels as u32)
    }

    pub fn sample_rate(&self) -> u32 {
        self.audio.as_ref().map_or(0, |a| {
            a.output_sampling_frequency
                .filter(|_| self.codec_name() == "aac")
                .unwrap_or(a.sampling_frequency)
                .round() as u32
        })
    }

    pub fn rotation(&self) -> u16 {
        self.video
            .as_ref()
            .and_then(VideoInfo::rotation)
            .unwrap_or(0)
    }

    pub fn alpha(&self) -> bool {
        self.kind == TrackKind::Video && self.video.as_ref().is_some_and(|v| v.alpha_mode == 1)
    }

    pub fn seconds(&self, ticks: i64) -> f64 {
        self.time_base.seconds(ticks)
    }

    pub fn ticks(&self, seconds: f64) -> i64 {
        (seconds * self.time_base.den as f64 / self.time_base.num as f64).round() as i64
    }

    pub fn shown_at(&self, seconds: f64) -> i64 {
        (seconds * self.time_base.den as f64 / self.time_base.num as f64 - 0.5).ceil() as i64
    }

    pub fn nanos(&self, ticks: i64) -> i64 {
        (ticks as i128 * self.time_base.num as i128 * 1_000_000_000 / self.time_base.den as i128)
            as i64
    }

    pub fn end(&self) -> i64 {
        self.samples
            .iter()
            .map(|s| s.pts.saturating_add(s.duration as i64))
            .max()
            .unwrap_or(0)
    }

    pub fn duration_seconds(&self) -> f64 {
        let start = self.samples.iter().map(|s| s.pts).min().unwrap_or(0).min(0);
        self.seconds(self.end().saturating_sub(start))
    }

    pub fn fps(&self) -> f64 {
        if let Some(d) = self.default_duration.filter(|&d| d > 0) {
            return 1e9 / d as f64;
        }
        if self.pts_order.len() == self.samples.len() {
            if let Some(fps) = steady_rate(self.distinct_pts(), self.seconds(1)) {
                return fps;
            }
        } else {
            let mut pts: Vec<i64> = self.samples.iter().map(|s| s.pts).collect();
            pts.sort_unstable();
            pts.dedup();
            if let Some(fps) = steady_rate(pts.into_iter(), self.seconds(1)) {
                return fps;
            }
        }
        let n = self.samples.len();
        let span = self
            .end()
            .saturating_sub(self.samples.iter().map(|s| s.pts).min().unwrap_or(0));
        if n == 0 || span <= 0 {
            0.0
        } else {
            n as f64 / self.seconds(span)
        }
    }

    fn distinct_pts(&self) -> impl Iterator<Item = i64> + Clone + '_ {
        let pts = |i: &u32| self.samples.get(*i as usize).map_or(0, |s| s.pts);
        self.pts_order.first().map(pts).into_iter().chain(
            self.pts_order
                .windows(2)
                .filter(move |w| pts(&w[0]) != pts(&w[1]))
                .map(move |w| pts(&w[1])),
        )
    }

    pub fn fits_as_well(&self, fps: f64, own: f64) -> bool {
        let tick = self.seconds(1);
        if self.pts_order.len() == self.samples.len() {
            fits_as_well(self.distinct_pts(), tick, fps, own)
        } else {
            let mut pts: Vec<i64> = self.samples.iter().map(|s| s.pts).collect();
            pts.sort_unstable();
            pts.dedup();
            fits_as_well(pts.into_iter(), tick, fps, own)
        }
    }

    pub fn keyframes(&self) -> &[u32] {
        &self.sync
    }

    pub fn keyframe_before(&self, index: usize) -> usize {
        let k = self.sync.partition_point(|&i| i as usize <= index);
        if k == 0 { 0 } else { self.sync[k - 1] as usize }
    }

    pub fn sample_at(&self, pts: i64) -> Option<usize> {
        if self.pts_order.is_empty() {
            return None;
        }
        let k = self
            .pts_order
            .partition_point(|&i| self.samples[i as usize].pts <= pts);
        Some(self.pts_order[k.saturating_sub(1)] as usize)
    }

    pub fn keyframe_at(&self, pts: i64) -> Option<usize> {
        let mut best: Option<usize> = None;
        for &k in &self.sync {
            let s = &self.samples[k as usize];
            if s.pts <= pts && best.is_none_or(|b| s.pts >= self.samples[b].pts) {
                best = Some(k as usize);
            }
        }
        best.or_else(|| self.sync.first().map(|&k| k as usize))
    }

    pub fn stripped_header(&self) -> Option<&[u8]> {
        self.encodings.iter().find_map(|e| match e {
            ContentEncoding::HeaderStripping { bytes, scope } if scope & 1 != 0 => {
                Some(bytes.as_slice())
            }
            _ => None,
        })
    }

    pub fn frames_readable(&self) -> bool {
        self.encodings.iter().all(|e| match e {
            ContentEncoding::HeaderStripping { .. } => true,
            ContentEncoding::Compression { scope, .. } | ContentEncoding::Encryption { scope } => {
                scope & 1 == 0
            }
        })
    }

    pub(crate) fn finish_indexes(&mut self) {
        self.sync = self
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.keyframe)
            .map(|(i, _)| i as u32)
            .collect();
        let mut order: Vec<u32> = (0..self.samples.len() as u32).collect();
        order.sort_by_key(|&i| (self.samples[i as usize].pts, i));
        self.pts_order = order;
    }
}

pub(crate) fn steady_rate<I: Iterator<Item = i64> + Clone>(pts: I, tick: f64) -> Option<f64> {
    let first = pts.clone().next()?;
    let (last, n) = pts.clone().fold((first, 0usize), |(_, n), p| (p, n + 1));
    let span = last.checked_sub(first).filter(|&s| s > 0)? as f64 * tick;
    if !tick.is_finite() || tick <= 0.0 || !span.is_finite() {
        return None;
    }
    let measured = (n - 1) as f64 / span;
    let whole = measured.round();
    let ntsc = (measured * 1.001).round() * 1000.0 / 1001.0;
    let snapped = [whole, ntsc]
        .into_iter()
        .filter(|&fps| fps >= 1.0)
        .filter_map(|fps| rate_error(pts.clone(), tick, fps).map(|e| (e, fps)))
        .filter(|&(e, _)| e <= 1.0)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, fps)| fps);
    Some(snapped.unwrap_or(measured))
}

pub(crate) fn rate_error<I: Iterator<Item = i64> + Clone>(
    mut pts: I,
    tick: f64,
    fps: f64,
) -> Option<f64> {
    if !tick.is_finite() || tick <= 0.0 || !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let first = pts.next()?;
    let frame = 1.0 / (fps * tick);
    Some(
        pts.enumerate()
            .map(|(k, p)| (p.saturating_sub(first) as f64 - (k + 1) as f64 * frame).abs())
            .fold(0.0, f64::max),
    )
}

pub(crate) fn fits_as_well<I: Iterator<Item = i64> + Clone>(
    pts: I,
    tick: f64,
    fps: f64,
    own: f64,
) -> bool {
    match (
        rate_error(pts.clone(), tick, fps),
        rate_error(pts, tick, own),
    ) {
        (Some(e), Some(o)) => e <= 1.0 && e <= o.max(0.5) + 1e-6,
        (Some(e), None) => e <= 0.5 + 1e-6,
        _ => false,
    }
}

#[cfg(test)]
mod rate_tests {
    use super::{fits_as_well, rate_error, steady_rate};

    const RATES: [(u32, u32); 6] = [
        (24000, 1001),
        (24, 1),
        (25, 1),
        (30000, 1001),
        (30, 1),
        (60, 1),
    ];

    fn ms_times(frames: usize, (num, den): (u32, u32), first: i64) -> Vec<i64> {
        (0..frames as u64)
            .map(|k| first + ((k * 1000 * den as u64 * 2 + num as u64) / (2 * num as u64)) as i64)
            .collect()
    }

    fn exact(fps: f64, (num, den): (u32, u32)) -> bool {
        (fps - num as f64 / den as f64).abs() < 1e-9
    }

    #[test]
    fn a_rate_with_no_usable_own_rate_fits_on_its_error_alone() {
        let pts = ms_times(12, (24, 1), 0);
        for own in [0.0, -1.0, f64::NAN] {
            assert!(fits_as_well(pts.iter().copied(), 0.001, 24.0, own));
            assert!(!fits_as_well(pts.iter().copied(), 0.001, 25.0, own));
        }
        assert!(!fits_as_well(pts.iter().copied(), 0.001, 0.0, 24.0));
        assert!(!fits_as_well(std::iter::empty(), 0.001, 24.0, 24.0));
    }

    #[test]
    fn millisecond_times_of_twelve_and_sixty_frames_give_the_exact_rate() {
        for frames in [12, 60] {
            for rate in RATES {
                for first in [0, 7, 1_000_000] {
                    let fps = steady_rate(ms_times(frames, rate, first).into_iter(), 1e-3).unwrap();
                    assert!(
                        exact(fps, rate),
                        "{frames} frames at {rate:?} from {first}: {fps}"
                    );
                }
            }
        }
    }

    #[test]
    fn six_frames_give_the_exact_rate_where_the_millisecond_times_tell_rates_apart() {
        for rate in [(24000, 1001), (24, 1), (25, 1), (30, 1), (60, 1)] {
            let fps = steady_rate(ms_times(6, rate, 0).into_iter(), 1e-3).unwrap();
            assert!(exact(fps, rate), "{rate:?}: {fps}");
        }
        assert_eq!(ms_times(6, (30000, 1001), 0), ms_times(6, (30, 1), 0));
    }

    #[test]
    fn the_twelve_frame_part_that_read_as_24_05_reads_as_24() {
        let times = [0, 42, 83, 125, 167, 208, 250, 292, 333, 375, 417, 458];
        assert_eq!(steady_rate(times.iter().copied(), 1e-3), Some(24.0));
    }

    #[test]
    fn nanosecond_times_keep_a_standard_rate() {
        let times: Vec<i64> = (0..10).map(|k| k * 1_001_000_000 / 30).collect();
        let fps = steady_rate(times.iter().copied(), 1e-9).unwrap();
        assert!(exact(fps, (30000, 1001)), "{fps}");
    }

    #[test]
    fn uneven_times_keep_the_measured_rate() {
        let times = [0, 40, 80, 200, 240, 280];
        let fps = steady_rate(times.iter().copied(), 1e-3).unwrap();
        assert!((fps - 5.0 / 0.28).abs() < 1e-9, "{fps}");
        let odd = [0, 39, 78, 118, 157, 196, 235, 274, 314, 353];
        let fps = steady_rate(odd.iter().copied(), 1e-3).unwrap();
        assert!((fps - 9.0 / 0.353).abs() < 1e-9, "{fps}");
    }

    #[test]
    fn too_few_or_bad_times_give_nothing() {
        assert_eq!(steady_rate([].into_iter(), 1e-3), None);
        assert_eq!(steady_rate([5].into_iter(), 1e-3), None);
        assert_eq!(steady_rate([5, 5].into_iter(), 1e-3), None);
        assert_eq!(steady_rate([0, 40].into_iter(), 0.0), None);
        assert_eq!(steady_rate([0, 40].into_iter(), f64::NAN), None);
        assert_eq!(steady_rate([0, 40].into_iter(), -1e-3), None);
        assert_eq!(steady_rate([i64::MIN, i64::MAX].into_iter(), 1e-3), None);
    }

    #[test]
    fn a_rate_under_one_frame_a_second_is_kept_as_measured() {
        let fps = steady_rate([0, 2500, 5000].into_iter(), 1e-3).unwrap();
        assert!((fps - 0.4).abs() < 1e-9, "{fps}");
    }

    #[test]
    fn the_rate_error_is_the_worst_frame_off_its_slot_in_ticks() {
        let film = ms_times(12, (24, 1), 5);
        let own = rate_error(film.iter().copied(), 1e-3, 24.0).unwrap();
        assert!(own <= 0.5, "{own}");
        let other = rate_error(film.iter().copied(), 1e-3, 24000.0 / 1001.0).unwrap();
        assert!(other > own, "{other} against {own}");
        assert_eq!(rate_error([7].into_iter(), 1e-3, 24.0), Some(0.0));
        assert_eq!(rate_error([].into_iter(), 1e-3, 24.0), None);
        assert_eq!(rate_error([0, 42].into_iter(), 0.0, 24.0), None);
        assert_eq!(rate_error([0, 42].into_iter(), 1e-3, 0.0), None);
        assert_eq!(rate_error([0, 42].into_iter(), 1e-3, f64::NAN), None);
    }

    #[test]
    fn close_rates_fit_twelve_millisecond_times_only_when_the_times_cannot_tell_them_apart() {
        let pairs = [
            ((24000, 1001), (24, 1)),
            ((30000, 1001), (30, 1)),
            ((60000, 1001), (60, 1)),
        ];
        for frames in [12, 60] {
            for (a, b) in pairs {
                let (x, y) = (a.0 as f64 / a.1 as f64, b.0 as f64 / b.1 as f64);
                let ta = ms_times(frames, a, 0);
                let tb = ms_times(frames, b, 0);
                assert!(fits_as_well(ta.iter().copied(), 1e-3, x, x));
                assert!(
                    !fits_as_well(ta.into_iter(), 1e-3, y, x),
                    "{frames} at {a:?}"
                );
                assert!(
                    !fits_as_well(tb.into_iter(), 1e-3, x, y),
                    "{frames} at {b:?}"
                );
            }
        }
        let six = ms_times(6, (30, 1), 0);
        assert!(fits_as_well(
            six.iter().copied(),
            1e-3,
            30000.0 / 1001.0,
            30.0
        ));
        assert!(!fits_as_well(six.into_iter(), 1e-3, 25.0, 30.0));
        assert!(!fits_as_well([0].into_iter().skip(1), 1e-3, 30.0, 30.0));
    }
}
