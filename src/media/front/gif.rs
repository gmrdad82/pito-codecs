use std::fs::File;
use std::io::Read;
use std::ops::Range;
use std::path::Path;

use super::base::rational;
use super::probe::Probe;
use crate::gif::Decoder;
use crate::media::{Error, Result, Rgba};

const MIN_DELAY_CS: u16 = 1;
const DEFAULT_DELAY_CS: u16 = 10;
const SNAP: f64 = 0.01;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifInfo {
    pub width: u32,
    pub height: u32,
    pub loop_count: Option<u16>,
    pub delays_cs: Vec<u16>,
    pub transparent: bool,
}

pub struct GifFrame<'a> {
    pub image: &'a Rgba,
    pub index: usize,
    pub start: f64,
    pub ticks: Range<u64>,
}

impl GifInfo {
    pub fn frames(&self) -> u64 {
        self.delays_cs.len() as u64
    }

    pub fn total_cs(&self) -> u64 {
        self.delays_cs.iter().map(|&d| u64::from(d)).sum()
    }

    pub fn duration(&self) -> f64 {
        self.total_cs() as f64 / 100.0
    }

    pub fn starts_cs(&self) -> Vec<u64> {
        self.delays_cs
            .iter()
            .scan(0u64, |at, &d| {
                let start = *at;
                *at += u64::from(d);
                Some(start)
            })
            .collect()
    }

    pub fn fps(&self) -> (u32, u32) {
        gif_rate(&self.delays_cs)
    }

    pub fn every_frame_fps(&self) -> (u32, u32) {
        if let Some(rate) = grid_rate(&self.delays_cs)
            && self.ticks(Some(rate)).iter().all(|t| !t.is_empty())
        {
            return rate;
        }
        let step = self
            .delays_cs
            .iter()
            .fold(0u32, |g, &d| gcd(g, u32::from(d)))
            .max(1);
        let common = gcd(100, step);
        (100 / common, step / common)
    }

    pub fn ticks(&self, rate: Option<(u32, u32)>) -> Vec<Range<u64>> {
        let Some((num, den)) = rate.filter(|(n, d)| *n > 0 && *d > 0) else {
            return (0..self.frames()).map(|i| i..i + 1).collect();
        };
        let slot = |cs: u64| {
            let (num, den) = (u128::from(num), u128::from(den));
            ((u128::from(cs) * num * 2 + 100 * den) / (200 * den)).min(u128::from(u64::MAX)) as u64
        };
        let starts = self.starts_cs();
        let end = slot(self.total_cs()).max(1);
        let mut next = 0u64;
        let mut out = Vec::with_capacity(starts.len());
        for (i, &s) in starts.iter().enumerate() {
            let until = starts.get(i + 1).map_or(end, |&n| slot(n).min(end));
            let first = next.max(slot(s));
            if until > first {
                out.push(first..until);
                next = until;
            } else {
                out.push(first..first);
            }
        }
        out
    }
}

pub fn gif_rate(delays_cs: &[u16]) -> (u32, u32) {
    let total: u64 = delays_cs.iter().map(|&d| u64::from(d)).sum();
    if total == 0 {
        return (100 / u32::from(DEFAULT_DELAY_CS), 1);
    }
    if let Some(rate) = grid_rate(delays_cs) {
        return rate;
    }
    let mean = delays_cs.len() as f64 * 100.0 / total as f64;
    let mut candidates = steady_candidates(mean);
    candidates.sort_by(|a, b| (a.0 - mean).abs().total_cmp(&(b.0 - mean).abs()));
    candidates
        .into_iter()
        .find(|&(r, _)| r >= 0.999 && (r - mean).abs() <= mean * SNAP)
        .map_or_else(|| rational(mean), |(_, rate)| rate)
}

fn steady_candidates(mean: f64) -> [(f64, (u32, u32)); 2] {
    let whole = mean.round();
    let ntsc = (mean * 1.001).round();
    [
        (whole, (whole as u32, 1)),
        (ntsc / 1.001, ((ntsc as u32).saturating_mul(1000), 1001)),
    ]
}

fn grid_rate(delays_cs: &[u16]) -> Option<(u32, u32)> {
    let total: u64 = delays_cs.iter().map(|&d| u64::from(d)).sum();
    let &first = delays_cs.first().filter(|_| total > 0)?;
    if delays_cs.iter().all(|&d| d == first) {
        let common = gcd(100, u32::from(first));
        return Some((100 / common, u32::from(first) / common));
    }
    let on_grid = |r: f64| {
        let mut at = 0u64;
        delays_cs.iter().enumerate().all(|(i, &d)| {
            at += u64::from(d);
            (at as f64 - (i + 1) as f64 * 100.0 / r).abs() <= 0.5 + 1e-9
        })
    };
    steady_candidates(delays_cs.len() as f64 * 100.0 / total as f64)
        .into_iter()
        .find(|&(r, _)| r >= 0.999 && on_grid(r))
        .map(|(_, rate)| rate)
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn effective_delay(cs: u16) -> u16 {
    if cs < MIN_DELAY_CS {
        DEFAULT_DELAY_CS
    } else {
        cs
    }
}

pub fn is_gif(head: &[u8]) -> bool {
    head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a")
}

pub fn is_gif_file(path: &Path) -> bool {
    let mut head = [0u8; 6];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| is_gif(&head))
}

fn invalid(what: &str) -> Error {
    Error::Invalid(format!("gif: {what}"))
}

fn sub_blocks(data: &[u8], mut pos: usize) -> Result<usize> {
    loop {
        let n = *data.get(pos).ok_or_else(|| invalid("truncated GIF"))? as usize;
        pos += 1;
        if n == 0 {
            return Ok(pos);
        }
        pos += n;
        if pos > data.len() {
            return Err(invalid("truncated GIF"));
        }
    }
}

pub fn scan_gif(data: &[u8]) -> Result<GifInfo> {
    if data.len() < 13 || !is_gif(data) {
        return Err(invalid("not a GIF file"));
    }
    let width = u32::from(u16::from_le_bytes([data[6], data[7]]));
    let height = u32::from(u16::from_le_bytes([data[8], data[9]]));
    if width == 0 || height == 0 {
        return Err(invalid("zero canvas size"));
    }
    let table = |packed: u8| 3usize << ((packed & 7) + 1);
    let mut pos = 13;
    if data[10] & 0x80 != 0 {
        pos += table(data[10]);
    }
    let mut info = GifInfo {
        width,
        height,
        loop_count: None,
        delays_cs: Vec::new(),
        transparent: false,
    };
    let mut control = None;
    loop {
        if pos > data.len() {
            return Err(invalid("truncated GIF"));
        }
        let Some(&b) = data.get(pos) else {
            break;
        };
        pos += 1;
        match b {
            0x21 => {
                let label = *data.get(pos).ok_or_else(|| invalid("truncated GIF"))?;
                pos += 1;
                let body = data.get(pos..).unwrap_or_default();
                if label == 0xF9 && body.len() >= 4 && body[0] >= 4 {
                    control = Some((u16::from_le_bytes([body[2], body[3]]), body[1] & 1 != 0));
                }
                if label == 0x01 {
                    control = None;
                }
                if label == 0xFF
                    && body.len() >= 16
                    && body[0] == 11
                    && (&body[1..12] == b"NETSCAPE2.0" || &body[1..12] == b"ANIMEXTS1.0")
                    && body[12] >= 3
                    && body[13] == 1
                {
                    info.loop_count = Some(u16::from_le_bytes([body[14], body[15]]));
                }
                pos = sub_blocks(data, pos)?;
            }
            0x2C => {
                let d = data
                    .get(pos..pos + 9)
                    .ok_or_else(|| invalid("truncated GIF"))?;
                pos += 9;
                if d[8] & 0x80 != 0 {
                    pos += table(d[8]);
                }
                if pos >= data.len() {
                    return Err(invalid("truncated GIF"));
                }
                pos = sub_blocks(data, pos + 1)?;
                let (delay, transparent) = control.take().unwrap_or((0, false));
                info.transparent |= transparent;
                info.delays_cs.push(effective_delay(delay));
            }
            0x3B => break,
            _ if !info.delays_cs.is_empty() => break,
            other => return Err(invalid(&format!("unknown block 0x{other:02x}"))),
        }
    }
    if info.delays_cs.is_empty() {
        return Err(invalid("no frames"));
    }
    Ok(info)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::at(path, e))
}

pub fn gif_info(path: &Path) -> Result<GifInfo> {
    scan_gif(&read(path)?)
}

pub(super) fn probe(path: &Path) -> Result<Probe> {
    let info = gif_info(path)?;
    Ok(Probe {
        duration: info.duration(),
        start: 0.0,
        video_start: Some(0.0),
        fps: info.fps(),
        width: info.width,
        height: info.height,
        rotation: 0,
        video_codec: None,
        video_codec_name: "gif".into(),
        profile: String::new(),
        bit_depth: 8,
        chroma: None,
        interlaced: false,
        tools: None,
        frames: Some(info.frames()),
        audio: Vec::new(),
        alpha: false,
    })
}

pub fn decode_gif<E: From<Error>>(
    data: &[u8],
    rate: Option<(u32, u32)>,
    each: impl FnMut(GifFrame<'_>) -> std::result::Result<(), E>,
) -> std::result::Result<GifInfo, E> {
    let info = scan_gif(data)?;
    decode_scanned(data, &info, rate, each)?;
    Ok(info)
}

pub fn decode_scanned<E: From<Error>>(
    data: &[u8],
    info: &GifInfo,
    rate: Option<(u32, u32)>,
    mut each: impl FnMut(GifFrame<'_>) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    let ticks = info.ticks(rate);
    let starts = info.starts_cs();
    let mut decoder = Decoder::new(data).map_err(Error::from)?;
    let mut index = 0;
    while let Some(frame) = decoder.next_frame().map_err(Error::from)? {
        let Some(span) = ticks.get(index) else {
            return Err(invalid(&format!(
                "more frames decoded than the {} scanned",
                ticks.len()
            ))
            .into());
        };
        each(GifFrame {
            image: frame.image,
            index,
            start: starts[index] as f64 / 100.0,
            ticks: span.clone(),
        })?;
        index += 1;
    }
    if index < ticks.len() {
        return Err(invalid(&format!("{index} of {} frames decoded", ticks.len())).into());
    }
    Ok(())
}

pub fn gif_frames<E: From<Error>>(
    path: &Path,
    rate: Option<(u32, u32)>,
    each: impl FnMut(GifFrame<'_>) -> std::result::Result<(), E>,
) -> std::result::Result<GifInfo, E> {
    decode_gif(&read(path)?, rate, each)
}

pub fn flatten_alpha(image: &mut Rgba) {
    for px in image.data.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a < 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * a + 127) / 255) as u8;
            }
            px[3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gif::{Dither, EncodeOptions, PaletteMode, encode};

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const NONE: [u8; 4] = [0, 0, 0, 0];
    const W: u32 = 16;
    const H: u32 = 8;

    fn opts() -> EncodeOptions {
        EncodeOptions {
            dither: Dither::None,
            palette: PaletteMode::Global,
            diff: false,
            ..EncodeOptions::default()
        }
    }

    fn filled(c: [u8; 4]) -> Rgba {
        let mut img = Rgba::new(W, H);
        for px in img.data.chunks_exact_mut(4) {
            px.copy_from_slice(&c);
        }
        img
    }

    fn with_square(base: [u8; 4], square: [u8; 4], x0: u32) -> Rgba {
        paint(filled(base), square, x0)
    }

    fn paint(mut img: Rgba, c: [u8; 4], x0: u32) -> Rgba {
        for y in 2..6 {
            for x in x0..x0 + 4 {
                let at = ((y * W + x) * 4) as usize;
                img.data[at..at + 4].copy_from_slice(&c);
            }
        }
        img
    }

    fn gce_offsets(data: &[u8]) -> Vec<usize> {
        let mut found = Vec::new();
        let mut pos = 13
            + if data[10] & 0x80 != 0 {
                3usize << ((data[10] & 7) + 1)
            } else {
                0
            };
        while let Some(&b) = data.get(pos) {
            pos += 1;
            match b {
                0x21 => {
                    if data[pos] == 0xF9 {
                        found.push(pos + 1);
                    }
                    pos = sub_blocks(data, pos + 1).unwrap();
                }
                0x2C => {
                    let packed = data[pos + 8];
                    pos += 9;
                    if packed & 0x80 != 0 {
                        pos += 3usize << ((packed & 7) + 1);
                    }
                    pos = sub_blocks(data, pos + 1).unwrap();
                }
                _ => break,
            }
        }
        found
    }

    fn with_disposal(mut data: Vec<u8>, frame: usize, disposal: u8) -> Vec<u8> {
        let at = gce_offsets(&data)[frame];
        data[at + 1] = (data[at + 1] & !0x1C) | (disposal << 2);
        data
    }

    fn rgb(img: &Rgba) -> Vec<u8> {
        let mut flat = img.clone();
        flatten_alpha(&mut flat);
        flat.data
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect()
    }

    fn composited(data: &[u8], rate: Option<(u32, u32)>) -> Vec<(Vec<u8>, Range<u64>)> {
        let mut out = Vec::new();
        decode_gif(data, rate, |f| {
            out.push((rgb(f.image), f.ticks));
            Ok::<(), Error>(())
        })
        .unwrap();
        out
    }

    fn three() -> Vec<u8> {
        let frames = [
            filled(RED),
            with_square(NONE, BLUE, 2),
            with_square(NONE, GREEN, 10),
        ];
        encode(&frames, &[7, 10, 13], opts()).unwrap()
    }

    #[test]
    fn a_zero_delay_takes_the_default() {
        assert_eq!(effective_delay(0), 10);
        assert_eq!(effective_delay(1), 1);
        assert_eq!(effective_delay(2), 2);
        assert_eq!(effective_delay(13), 13);
    }

    #[test]
    fn the_scan_reads_size_delays_loop_and_duration() {
        let info = scan_gif(&three()).unwrap();
        assert_eq!((info.width, info.height), (W, H));
        assert_eq!(info.delays_cs, vec![7, 10, 13]);
        assert_eq!(info.loop_count, Some(0));
        assert_eq!(info.frames(), 3);
        assert_eq!(info.total_cs(), 30);
        assert!((info.duration() - 0.3).abs() < 1e-12);
        assert_eq!(info.starts_cs(), vec![0, 7, 17]);
        assert_eq!(info.fps(), (10, 1));
    }

    fn delays(d: &[u16]) -> GifInfo {
        GifInfo {
            width: 4,
            height: 4,
            loop_count: None,
            delays_cs: d.to_vec(),
            transparent: false,
        }
    }

    #[test]
    fn the_every_frame_rate_puts_each_start_on_a_tick() {
        assert_eq!(delays(&[4, 4, 4]).every_frame_fps(), (25, 1));
        assert_eq!(delays(&[7, 10, 13, 4, 25, 3]).every_frame_fps(), (100, 1));
        assert_eq!(delays(&[2, 100]).every_frame_fps(), (50, 1));
        assert_eq!(delays(&[6, 9]).every_frame_fps(), (100, 3));
        assert_eq!(delays(&[300]).every_frame_fps(), (1, 3));
        assert_eq!(delays(&[65535]).every_frame_fps(), (20, 13107));
    }

    #[test]
    fn no_frame_is_dropped_at_the_every_frame_rate() {
        for d in [
            vec![7, 10, 13, 4, 25, 3],
            vec![2, 100],
            vec![6, 9, 12],
            vec![300],
            vec![5, 5, 5],
        ] {
            let info = delays(&d);
            let ticks = info.ticks(Some(info.every_frame_fps()));
            assert!(ticks.iter().all(|t| !t.is_empty()), "{d:?}: {ticks:?}");
            let last = ticks.last().unwrap();
            let rate = info.every_frame_fps();
            let end = info.total_cs() * u64::from(rate.0) / (100 * u64::from(rate.1));
            assert_eq!(last.end, end, "{d:?}");
        }
    }

    fn made_at(frames: u64, num: u32, den: u32) -> GifInfo {
        delays(
            &(0..frames)
                .map(|n| crate::gif::delay_cs(n, num, den))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn a_gif_rate_is_its_uniform_delay_or_its_mean_snapped_to_a_steady_rate() {
        let cases: [(Vec<u16>, (u32, u32)); 14] = [
            (vec![10], (10, 1)),
            (vec![4; 10], (25, 1)),
            (vec![3; 3], (100, 3)),
            (vec![3; 20], (100, 3)),
            (vec![20; 30], (5, 1)),
            (vec![200; 3], (1, 2)),
            (vec![13; 17], (100, 13)),
            (vec![7, 7, 6], (15, 1)),
            (vec![8, 8, 9], (12, 1)),
            (vec![3, 3, 4], (30, 1)),
            (vec![7, 6, 7, 7], (15, 1)),
            (vec![7, 7, 6, 7], (14815, 1000)),
            (vec![10, 10, 9], (10345, 1000)),
            (vec![2, 100], (1961, 1000)),
        ];
        for (d, want) in cases {
            assert_eq!(gif_rate(&d), want, "{d:?}");
        }
        assert_eq!(gif_rate(&[]), (10, 1));
    }

    #[test]
    fn a_mean_near_an_ntsc_rate_snaps_to_it() {
        let info = made_at(300, 30000, 1001);
        assert_eq!(info.fps(), (30000, 1001));
        assert_eq!(made_at(240, 24000, 1001).fps(), (24000, 1001));
    }

    #[test]
    fn a_gif_from_our_encoder_converts_at_its_own_rate_with_every_frame() {
        for (rate, frames) in [(15, 47), (12, 50), (30, 61), (15, 3), (24, 100)] {
            let info = made_at(frames, rate, 1);
            assert_eq!(info.fps(), (rate, 1), "{rate}");
            assert_eq!(info.every_frame_fps(), (rate, 1), "{rate}");
            let ticks = info.ticks(Some((rate, 1)));
            assert_eq!(
                ticks,
                (0..frames).map(|i| i..i + 1).collect::<Vec<_>>(),
                "{rate}"
            );
        }
    }

    #[test]
    fn an_irregular_gif_keeps_the_rate_every_start_falls_on() {
        assert_eq!(delays(&[6, 9]).fps(), (13333, 1000));
        assert_eq!(delays(&[6, 9]).every_frame_fps(), (100, 3));
        assert_eq!(delays(&[10, 10, 5, 10]).fps(), (11429, 1000));
        assert_eq!(delays(&[10, 10, 5, 10]).every_frame_fps(), (20, 1));
    }

    #[test]
    fn the_average_rate_would_drop_frames_the_every_frame_rate_keeps() {
        let info = delays(&[2, 100]);
        assert!(info.ticks(Some(info.fps()))[0].is_empty());
        assert_eq!(info.ticks(Some(info.every_frame_fps())), vec![0..1, 1..51]);
    }

    fn plain_text_between(control: u8, index: u8) -> Vec<u8> {
        let mut data = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        data.extend_from_slice(&[0x21, 0xf9, 4, control, 50, 0, index, 0]);
        data.extend_from_slice(&[
            0x21, 0x01, 12, 0, 0, 0, 0, 1, 0, 1, 0, 1, 1, 0, 1, 1, b'A', 0,
        ]);
        data.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0x80]);
        data.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        data.extend_from_slice(&[2, 2, 0x44, 0x01, 0, 0x3b]);
        data
    }

    #[test]
    fn a_control_block_spent_on_plain_text_leaves_the_next_image_its_default_delay() {
        let data = plain_text_between(0x01, 0);
        let info = scan_gif(&data).unwrap();
        assert_eq!(info.delays_cs, vec![DEFAULT_DELAY_CS]);
        assert!(!info.transparent);
        let mut pixels = Vec::new();
        decode_gif::<Error>(&data, None, |f| {
            pixels.push(f.image.data.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(pixels, vec![vec![0, 0, 0, 255]]);
    }

    #[test]
    fn a_control_block_on_an_image_marks_it_transparent() {
        let mut data = plain_text_between(0x01, 0);
        data.drain(21..39);
        let info = scan_gif(&data).unwrap();
        assert_eq!(info.delays_cs, vec![50]);
        assert!(info.transparent);
        let mut pixels = Vec::new();
        decode_gif::<Error>(&data, None, |f| {
            pixels.push(f.image.data.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(pixels, vec![vec![0, 0, 0, 0]]);
    }

    #[test]
    fn a_decoder_that_disagrees_with_the_scan_is_refused_either_way() {
        let data = three();
        let mut info = scan_gif(&data).unwrap();
        info.delays_cs.pop();
        let mut seen = 0;
        let more = decode_scanned::<Error>(&data, &info, None, |_| {
            seen += 1;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(seen, 2);
        assert!(
            more.to_string()
                .contains("more frames decoded than the 2 scanned"),
            "{more}"
        );
        info.delays_cs.extend([10, 10]);
        let mut seen = 0;
        let fewer = decode_scanned::<Error>(&data, &info, None, |_| {
            seen += 1;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(seen, 3);
        assert!(
            fewer.to_string().contains("3 of 4 frames decoded"),
            "{fewer}"
        );
    }

    #[test]
    fn a_padded_tail_after_the_last_image_keeps_the_frames_and_the_flag() {
        let mut data = three();
        assert_eq!(data.pop(), Some(0x3b));
        data.push(0);
        let info = scan_gif(&data).unwrap();
        assert_eq!(info.delays_cs, vec![7, 10, 13]);
        assert!(info.transparent);
    }

    #[test]
    fn source_ticks_are_one_per_frame() {
        let info = scan_gif(&three()).unwrap();
        assert_eq!(info.ticks(None), vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn ticks_follow_the_fps_filter_rounding() {
        let info = scan_gif(&three()).unwrap();
        assert_eq!(info.ticks(Some((10, 1))), vec![0..1, 1..2, 2..3]);
        assert_eq!(info.ticks(Some((30, 1))), vec![0..2, 2..5, 5..9]);
        assert_eq!(info.ticks(Some((100, 1))), vec![0..7, 7..17, 17..30]);
    }

    #[test]
    fn a_frame_shorter_than_a_tick_gives_way_to_the_next() {
        let info = GifInfo {
            width: 4,
            height: 4,
            loop_count: None,
            delays_cs: vec![4, 2, 2, 12],
            transparent: false,
        };
        assert_eq!(info.ticks(Some((10, 1))), vec![0..0, 0..1, 1..1, 1..2]);
        let total: u64 = info
            .ticks(Some((10, 1)))
            .iter()
            .map(|r| r.end - r.start)
            .sum();
        assert_eq!(total, 2);
    }

    #[test]
    fn a_tie_rounds_to_the_later_tick() {
        let info = GifInfo {
            width: 4,
            height: 4,
            loop_count: None,
            delays_cs: vec![5, 5],
            transparent: false,
        };
        assert_eq!(info.ticks(Some((10, 1))), vec![0..1, 1..1]);
        assert_eq!(info.ticks(Some((30, 1))), vec![0..2, 2..3]);
    }

    #[test]
    fn a_one_frame_gif_holds_at_least_one_tick() {
        let bytes = encode(&[filled(GREEN)], &[3], opts()).unwrap();
        let info = scan_gif(&bytes).unwrap();
        assert_eq!(info.frames(), 1);
        assert_eq!(info.ticks(Some((10, 1))), vec![0..1]);
        assert_eq!(info.ticks(Some((1, 1))), vec![0..1]);
        assert_eq!(info.ticks(Some((30, 1))), vec![0..1]);
        let frames = composited(&bytes, Some((10, 1)));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, rgb(&filled(GREEN)));
    }

    #[test]
    fn keep_disposal_layers_each_frame_over_the_last() {
        let bytes = with_disposal(with_disposal(three(), 1, 1), 0, 1);
        let frames = composited(&bytes, None);
        let second = paint(filled(RED), BLUE, 2);
        let third = paint(second.clone(), GREEN, 10);
        assert_eq!(frames[0].0, rgb(&filled(RED)));
        assert_eq!(frames[1].0, rgb(&second));
        assert_eq!(frames[2].0, rgb(&third));
    }

    #[test]
    fn background_disposal_clears_the_frame_to_transparent_black() {
        let bytes = with_disposal(with_disposal(three(), 0, 1), 1, 2);
        let frames = composited(&bytes, None);
        assert_eq!(frames[1].0, rgb(&paint(filled(RED), BLUE, 2)));
        assert_eq!(frames[2].0, rgb(&with_square(NONE, GREEN, 10)));
    }

    #[test]
    fn previous_disposal_restores_what_was_under_the_frame() {
        let bytes = with_disposal(with_disposal(three(), 0, 1), 1, 3);
        let frames = composited(&bytes, None);
        assert_eq!(frames[1].0, rgb(&paint(filled(RED), BLUE, 2)));
        assert_eq!(frames[2].0, rgb(&paint(filled(RED), GREEN, 10)));
    }

    #[test]
    fn a_transparent_first_frame_flattens_to_black() {
        let bytes = encode(&[with_square(NONE, BLUE, 4), filled(RED)], &[5, 5], opts()).unwrap();
        let frames = composited(&bytes, None);
        assert_eq!(frames[0].0, rgb(&with_square([0, 0, 0, 255], BLUE, 4)));
        assert_eq!(frames[1].0, rgb(&filled(RED)));
    }

    #[test]
    fn resampled_frames_carry_their_ticks() {
        let bytes = with_disposal(with_disposal(three(), 0, 1), 1, 1);
        let frames = composited(&bytes, Some((30, 1)));
        let ticks: Vec<_> = frames.iter().map(|f| f.1.clone()).collect();
        assert_eq!(ticks, vec![0..2, 2..5, 5..9]);
    }

    #[test]
    fn a_truncated_gif_is_refused() {
        let bytes = three();
        let cut = &bytes[..bytes.len() - 3];
        let e = scan_gif(cut).unwrap_err().to_string();
        assert!(e.contains("truncated GIF"), "{e}");
        assert!(!e.contains('\n'));
        assert!(decode_gif(cut, None, |_| Ok::<(), Error>(())).is_err());
    }

    #[test]
    fn a_gif_with_no_frames_is_refused() {
        let bytes = three();
        let first = gce_offsets(&bytes)[0] - 2;
        let mut empty = bytes[..first].to_vec();
        empty.push(0x3B);
        let e = scan_gif(&empty).unwrap_err().to_string();
        assert!(e.contains("no frames"), "{e}");
        assert!(
            scan_gif(&bytes[..first])
                .unwrap_err()
                .to_string()
                .contains("no frames")
        );
    }

    #[test]
    fn a_missing_trailer_after_whole_frames_still_reads() {
        let mut bytes = three();
        assert_eq!(bytes.pop(), Some(0x3B));
        assert_eq!(scan_gif(&bytes).unwrap().frames(), 3);
    }

    #[test]
    fn not_a_gif_and_a_zero_canvas_are_refused() {
        assert!(scan_gif(b"PNG not a gif at all").is_err());
        let mut bytes = three();
        bytes[6] = 0;
        bytes[7] = 0;
        assert!(
            scan_gif(&bytes)
                .unwrap_err()
                .to_string()
                .contains("zero canvas")
        );
    }

    #[test]
    fn the_probe_reads_one_video_track() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/gif-front");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("three.gif");
        std::fs::write(&path, three()).unwrap();
        assert!(is_gif_file(&path));
        let p = probe(&path).unwrap();
        assert_eq!((p.width, p.height), (W, H));
        assert_eq!(p.frames, Some(3));
        assert_eq!(p.fps, (10, 1));
        assert_eq!(p.video_codec_name, "gif");
        assert!(p.audio.is_empty());
        assert!((p.duration - 0.3).abs() < 1e-12);
        let mut seen = Vec::new();
        gif_frames(&path, None, |f| {
            seen.push((f.index, f.start));
            Ok::<(), Error>(())
        })
        .unwrap();
        assert_eq!(seen, vec![(0, 0.0), (1, 0.07), (2, 0.17)]);
        let not_gif = dir.join("not.gif");
        std::fs::write(&not_gif, b"\x00\x00\x00\x18ftypmp42").unwrap();
        assert!(!is_gif_file(&not_gif));
        assert!(!is_gif_file(&dir.join("missing.gif")));
    }

    #[test]
    fn a_scanned_decode_gives_the_same_frames_without_a_second_scan() {
        let bytes = three();
        let info = scan_gif(&bytes).unwrap();
        let mut ticks = Vec::new();
        decode_scanned(&bytes, &info, Some((30, 1)), |f| {
            ticks.push(f.ticks);
            Ok::<(), Error>(())
        })
        .unwrap();
        assert_eq!(ticks, info.ticks(Some((30, 1))));
    }

    #[test]
    fn a_gif_cut_inside_its_tables_or_before_its_image_data_is_truncated() {
        let mut table_cut = b"GIF89a\x04\x00\x04\x00\x80\x00\x00".to_vec();
        table_cut.extend_from_slice(&[0, 0]);
        let mut descriptor_only = b"GIF89a\x04\x00\x04\x00\x00\x00\x00".to_vec();
        descriptor_only.extend_from_slice(&[0x2c, 0, 0, 0, 0, 4, 0, 4, 0, 0]);
        for data in [table_cut, descriptor_only] {
            let e = scan_gif(&data).unwrap_err().to_string();
            assert!(e.contains("truncated GIF"), "{e}");
        }
    }
}
