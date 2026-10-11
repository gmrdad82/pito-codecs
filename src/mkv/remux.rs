use std::io::{Seek, Write};
use std::ops::Range;
use std::path::Path;

use super::reader::Reader;
use super::track::Track;
use super::writer::{Options, TrackConfig, Writer};
use crate::media::{Error, Result};
use crate::mp4::{ByteSource, Cut, Snap, Span, TrackKind};

const WINDOW: u64 = 8 << 20;

fn track_end(t: &Track) -> i64 {
    t.end()
}

fn video_range(t: &Track, from: f64, to: f64, snap: Snap) -> Result<(Range<usize>, i64, i64)> {
    let keys: Vec<usize> = t.keyframes().iter().map(|&k| k as usize).collect();
    if keys.is_empty() {
        return Err(Error::Invalid("video track has no keyframes".into()));
    }
    let n = t.samples.len();
    let pts = |i: usize| t.samples[i].pts;
    let gop_min: Vec<i64> = keys
        .iter()
        .enumerate()
        .map(|(j, &k)| {
            let end = keys.get(j + 1).copied().unwrap_or(n);
            (k..end).map(pts).min().unwrap_or(i64::MAX)
        })
        .collect();
    let (from_t, to_t) = (t.ticks(from), t.ticks(to));
    let end_t = track_end(t);
    match snap {
        Snap::Outward => {
            let ja = keys.iter().rposition(|&k| pts(k) <= from_t).unwrap_or(0);
            let b = (ja + 1..keys.len())
                .find(|&j| gop_min[j] >= to_t)
                .map_or(n, |j| keys[j]);
            let a = keys[ja];
            let copied_end = (a..b)
                .map(|i| pts(i).saturating_add(t.samples[i].duration as i64))
                .max()
                .unwrap_or(0);
            Ok((a..b, from_t.max(pts(a)), to_t.min(copied_end)))
        }
        Snap::Inward => {
            let ja = keys
                .iter()
                .position(|&k| pts(k) >= from_t)
                .ok_or_else(|| Error::Invalid(format!("no keyframe after {from:.3} s")))?;
            let start = pts(keys[ja]);
            if to_t >= end_t {
                return Ok((keys[ja]..n, start, end_t));
            }
            let jb = (ja + 1..keys.len())
                .rev()
                .find(|&j| pts(keys[j]) <= to_t)
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "no keyframe-to-keyframe stretch inside {from:.3}..{to:.3} s"
                    ))
                })?;
            Ok((keys[ja]..keys[jb], start, pts(keys[jb])))
        }
    }
}

fn audio_range(t: &Track, start: f64, end: f64) -> Range<usize> {
    let (s, e) = (t.ticks(start), t.ticks(end));
    let n = t.samples.len();
    let first = (0..n)
        .position(|i| {
            t.samples[i]
                .pts
                .saturating_add(t.samples[i].duration as i64)
                > s
        })
        .unwrap_or(n);
    let first = if first < n && t.samples[first].pts < s {
        first.saturating_sub(1)
    } else {
        first
    };
    let last = (first..n).find(|&i| t.samples[i].pts >= e).unwrap_or(n);
    first..last
}

pub fn plan<S: ByteSource>(reader: &Reader<S>, from: f64, to: f64, snap: Snap) -> Result<Cut> {
    let total = reader.duration_seconds();
    let (from, to) = (from.max(0.0), to.min(total.max(from)));
    if to.is_nan() || from.is_nan() || to <= from {
        return Err(Error::Invalid(format!("empty range {from:.3}..{to:.3} s")));
    }
    let mut tracks = Vec::new();
    let span = match reader.video() {
        Some(v) => {
            let t = &reader.tracks[v];
            let (range, start, end) = video_range(t, from, to, snap)?;
            tracks.push((v, range));
            Span {
                start: t.seconds(start),
                end: t.seconds(end),
            }
        }
        None => Span {
            start: from,
            end: to,
        },
    };
    for a in reader.audio() {
        tracks.push((a, audio_range(&reader.tracks[a], span.start, span.end)));
    }
    Ok(Cut { span, tracks })
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
        writer.push_sample(map[k], data, pts, None, s.keyframe)?;
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
