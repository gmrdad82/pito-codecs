use std::io::{Seek, Write};
use std::path::Path;

use super::reader::Reader;
use super::track::Track;
use super::writer::{Options, TrackConfig, Writer};
use crate::media::{Error, Result};
use crate::mp4::remux::{Timeline, plan_tracks};
use crate::mp4::{ByteSource, Cut, Snap, Span, TrackKind};

const WINDOW: u64 = 8 << 20;

impl Timeline for Track {
    fn len(&self) -> usize {
        self.samples.len()
    }
    fn keyframes(&self) -> &[u32] {
        Track::keyframes(self)
    }
    fn pts(&self, i: usize) -> i64 {
        self.samples[i].pts
    }
    fn duration(&self, i: usize) -> i64 {
        self.samples[i].duration as i64
    }
    fn ticks(&self, seconds: f64) -> i64 {
        Track::ticks(self, seconds)
    }
    fn seconds(&self, ticks: i64) -> f64 {
        Track::seconds(self, ticks)
    }
    fn end(&self) -> i64 {
        Track::end(self)
    }
}

pub fn plan<S: ByteSource>(reader: &Reader<S>, from: f64, to: f64, snap: Snap) -> Result<Cut> {
    plan_tracks(
        reader.duration_seconds(),
        &reader.tracks,
        reader.video(),
        reader.audio(),
        (from, to),
        snap,
    )
}

pub fn copy<S: ByteSource, W: Write + Seek>(
    reader: &Reader<S>,
    cut: &Cut,
    writer: &mut Writer<W>,
) -> Result<()> {
    let mut map = Vec::new();
    let mut origin_ns = i64::MAX;
    for (src, range) in &cut.tracks {
        let t = reader
            .tracks
            .get(*src)
            .ok_or_else(|| Error::Invalid(format!("no track {src}")))?;
        let samples = t
            .samples
            .get(range.clone())
            .ok_or_else(|| Error::Invalid(format!("no samples {range:?} in track {src}")))?;
        map.push(writer.add_track(TrackConfig::from_track(t))?);
        if let Some(first) = samples.iter().map(|s| s.pts).min() {
            origin_ns = origin_ns.min(t.nanos(first).saturating_add(t.codec_delay as i64));
        }
    }
    if origin_ns == i64::MAX {
        origin_ns = 0;
    }
    let mut order: Vec<(u64, usize, usize)> = cut
        .tracks
        .iter()
        .enumerate()
        .flat_map(|(k, (src, range))| {
            let t = &reader.tracks[*src];
            range.clone().map(move |i| (t.samples[i].offset, k, i))
        })
        .collect();
    order.sort_unstable();
    let src = reader.source();
    let len = reader.len();
    let mut window = Vec::new();
    let mut window_start = 0u64;
    let mut frame = Vec::new();
    let mut alpha = Vec::new();
    for (offset, k, i) in order {
        let t = &reader.tracks[cut.tracks[k].0];
        let s = t.samples[i];
        let shift =
            origin_ns as i128 * t.time_base.den as i128 / (t.time_base.num as i128 * 1_000_000_000);
        let pts = s.pts.saturating_sub(shift as i64);
        let prefix = t.stripped_header().unwrap_or(&[]);
        let end = offset
            .checked_add(s.size as u64)
            .filter(|&e| e <= len)
            .ok_or_else(|| Error::Invalid("truncated Matroska data".into()))?;
        let data: &[u8] = if prefix.is_empty() && s.size as u64 <= WINDOW {
            if offset < window_start || end > window_start + window.len() as u64 {
                let n = (len.saturating_sub(offset)).min(WINDOW.max(s.size as u64));
                window.resize(n as usize, 0);
                src.read_at(offset, &mut window)?;
                window_start = offset;
            }
            let at = (offset - window_start) as usize;
            &window[at..at + s.size as usize]
        } else {
            super::reader::read_frame(src, len, t, offset, s.size, &mut frame)?;
            &frame
        };
        let additional = if t.alpha() && reader.read_alpha(cut.tracks[k].0, i, &mut alpha)? {
            Some(&alpha[..])
        } else {
            None
        };
        writer.push_block(map[k], data, pts, None, s.keyframe, additional)?;
    }
    Ok(())
}

pub fn remux(
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
    from: f64,
    to: f64,
    snap: Snap,
) -> Result<Span> {
    let reader = Reader::open(src)?;
    let cut = plan(&reader, from, to, snap)?;
    let options = Options {
        webm: reader.is_webm(),
        ..Options::default()
    };
    let mut writer = Writer::create_with(dst, options)?;
    copy(&reader, &cut, &mut writer)?;
    writer.finish()?;
    Ok(cut.span)
}

pub fn remux_mp4(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> Result<()> {
    let reader = crate::mp4::Reader::open(src)?;
    let mut writer = Writer::create(dst)?;
    let mut tracks = Vec::new();
    for (i, t) in reader.tracks.iter().enumerate() {
        if matches!(t.kind, TrackKind::Video | TrackKind::Audio) {
            tracks.push((i, writer.add_track(TrackConfig::from_mp4(t)?)?));
        }
    }
    let mut order: Vec<(u64, usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(k, &(i, _))| {
            reader.tracks[i]
                .samples
                .iter()
                .enumerate()
                .map(move |(n, s)| (s.offset, k, n))
        })
        .collect();
    order.sort_unstable();
    let shift: Vec<i64> = tracks
        .iter()
        .map(|&(i, _)| {
            let t = &reader.tracks[i];
            (0..t.samples.len())
                .filter_map(|n| t.presentation(n))
                .min()
                .unwrap_or(0)
                .min(0)
        })
        .collect();
    let mut data = Vec::new();
    for (_, k, n) in order {
        let (i, dst) = tracks[k];
        let t = &reader.tracks[i];
        reader.read_into(i, n, &mut data)?;
        let pts = t.presentation(n).unwrap_or(0).saturating_sub(shift[k]);
        writer.push_sample(dst, &data, pts, None, t.samples[n].keyframe)?;
    }
    writer.finish()?;
    Ok(())
}
