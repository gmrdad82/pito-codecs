use crate::media::{Error, Result};
use crate::mp4::reader::{Edit, Reader, Track, TrackKind};
use crate::mp4::source::ByteSource;
use crate::mp4::truncated;
use crate::mp4::writer::{Brand, Options, TrackConfig, Writer};
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

const RUN: u64 = 8 << 20;
const INTERLEAVE: f64 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Inward,
    Outward,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cut {
    pub span: Span,
    pub tracks: Vec<(usize, Range<usize>)>,
}

fn presentation(t: &Track, i: usize) -> i64 {
    t.presentation(i).unwrap_or(0)
}

fn track_end(t: &Track) -> i64 {
    (0..t.samples.len())
        .map(|i| presentation(t, i).saturating_add(t.samples[i].duration as i64))
        .max()
        .unwrap_or(0)
}

fn video_range(t: &Track, from: f64, to: f64, snap: Snap) -> Result<(Range<usize>, i64, i64)> {
    let keys: Vec<usize> = t.keyframes().iter().map(|&k| k as usize).collect();
    if keys.is_empty() {
        return Err(Error::Invalid("video track has no keyframes".into()));
    }
    let n = t.samples.len();
    let gop_min: Vec<i64> = keys
        .iter()
        .enumerate()
        .map(|(j, &k)| {
            let end = keys.get(j + 1).copied().unwrap_or(n);
            (k..end)
                .map(|i| presentation(t, i))
                .min()
                .unwrap_or(i64::MAX)
        })
        .collect();
    let (from_t, to_t) = (t.ticks(from), t.ticks(to));
    let end_t = track_end(t);
    match snap {
        Snap::Outward => {
            let ja = keys
                .iter()
                .rposition(|&k| presentation(t, k) <= from_t)
                .unwrap_or(0);
            let b = (ja + 1..keys.len())
                .find(|&j| gop_min[j] >= to_t)
                .map_or(n, |j| keys[j]);
            let a = keys[ja];
            let copied_end = (a..b)
                .map(|i| presentation(t, i).saturating_add(t.samples[i].duration as i64))
                .max()
                .unwrap_or(0);
            Ok((a..b, from_t.max(presentation(t, a)), to_t.min(copied_end)))
        }
        Snap::Inward => {
            let ja = keys
                .iter()
                .position(|&k| presentation(t, k) >= from_t)
                .ok_or_else(|| Error::Invalid(format!("no keyframe after {from:.3} s")))?;
            let start = presentation(t, keys[ja]);
            if to_t >= end_t {
                return Ok((keys[ja]..n, start, end_t));
            }
            let jb = (ja + 1..keys.len())
                .rev()
                .find(|&j| presentation(t, keys[j]) <= to_t)
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "no keyframe-to-keyframe stretch inside {from:.3}..{to:.3} s"
                    ))
                })?;
            Ok((keys[ja]..keys[jb], start, presentation(t, keys[jb])))
        }
    }
}

fn audio_range(t: &Track, start: f64, end: f64) -> Range<usize> {
    let (s, e) = (t.ticks(start), t.ticks(end));
    let n = t.samples.len();
    let first = (0..n)
        .position(|i| presentation(t, i).saturating_add(t.samples[i].duration as i64) > s)
        .unwrap_or(n);
    let first = if first < n && presentation(t, first) < s {
        first.saturating_sub(1)
    } else {
        first
    };
    let last = (first..n).find(|&i| presentation(t, i) >= e).unwrap_or(n);
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
    for i in reader.audio() {
        tracks.push((i, audio_range(&reader.tracks[i], span.start, span.end)));
    }
    Ok(Cut { span, tracks })
}

pub fn copy_samples<S: ByteSource, W: Write + Seek>(
    reader: &Reader<S>,
    track: usize,
    range: Range<usize>,
    writer: &mut Writer<W>,
    dst: usize,
    shift: i64,
) -> Result<()> {
    let mut buf = Vec::new();
    copy_run(reader, track, range, writer, dst, shift, &mut buf, false)
}

#[allow(clippy::too_many_arguments)]
fn copy_run<S: ByteSource, W: Write + Seek>(
    reader: &Reader<S>,
    track: usize,
    range: Range<usize>,
    writer: &mut Writer<W>,
    dst: usize,
    shift: i64,
    buf: &mut Vec<u8>,
    dry: bool,
) -> Result<()> {
    let t = reader
        .tracks
        .get(track)
        .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
    let samples = t
        .samples
        .get(range.clone())
        .ok_or_else(|| Error::Invalid(format!("no samples {range:?} in track {track}")))?;
    let len = reader.source().len();
    let mut entry = None;
    let mut i = 0;
    while i < samples.len() {
        let start = samples[i].offset;
        let mut end = start;
        let mut j = i;
        while j < samples.len()
            && samples[j].offset == end
            && (j == i || end - start + samples[j].size as u64 <= RUN)
        {
            end = end
                .checked_add(samples[j].size as u64)
                .filter(|&e| e <= len)
                .ok_or_else(|| truncated("sample data"))?;
            j += 1;
        }
        buf.resize((end - start) as usize, 0);
        if !dry {
            reader.read_span(start, buf)?;
        }
        let mut at = 0usize;
        for s in &samples[i..j] {
            if entry != Some(s.entry) {
                writer.use_entry(dst, s.entry)?;
                entry = Some(s.entry);
            }
            let size = s.size as usize;
            writer.push_sample(
                dst,
                &buf[at..at + size],
                s.pts.saturating_add(shift),
                s.dts.saturating_add(shift),
                s.duration,
                s.keyframe,
            )?;
            at += size;
        }
        i = j;
    }
    Ok(())
}

struct Sink {
    pos: u64,
    end: u64,
}

impl Write for Sink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.pos += data.len() as u64;
        self.end = self.end.max(self.pos);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Sink {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.pos = match to {
            SeekFrom::Start(p) => p,
            SeekFrom::Current(d) => self.pos.saturating_add_signed(d),
            SeekFrom::End(d) => self.end.saturating_add_signed(d),
        };
        Ok(self.pos)
    }
}

fn feed<S: ByteSource, W: Write + Seek>(
    reader: &Reader<S>,
    cut: &Cut,
    writer: &mut Writer<W>,
    dry: bool,
) -> Result<()> {
    let segment = ((cut.span.end - cut.span.start) * 1000.0).round().max(0.0) as u64;
    let mut lanes = Vec::new();
    for (ti, range) in &cut.tracks {
        let t = &reader.tracks[*ti];
        if range.is_empty() || !matches!(t.kind, TrackKind::Video | TrackKind::Audio) {
            continue;
        }
        let mut cfg = TrackConfig::from_track(t)?;
        let origin = t.samples[range.start].dts;
        let start = t
            .ticks(cut.span.start)
            .saturating_sub(t.edit_offset)
            .saturating_sub(origin);
        cfg.edits = vec![Edit {
            segment_duration: segment,
            media_time: start.max(0),
            media_rate: 0x10000,
        }];
        let dst = writer.add_track(cfg)?;
        for e in t.entries.iter().skip(1) {
            writer.add_entry(dst, e.clone())?;
        }
        lanes.push((*ti, range.clone(), dst, origin, range.start));
    }
    let mut buf = Vec::new();
    let mut window = INTERLEAVE;
    loop {
        let mut left = false;
        let mut ahead = f64::INFINITY;
        for (ti, range, dst, origin, next) in &mut lanes {
            let t = &reader.tracks[*ti];
            let stop = (*next..range.end)
                .find(|&i| t.seconds(t.samples[i].dts.saturating_sub(*origin)) >= window)
                .unwrap_or(range.end);
            if stop > *next {
                copy_run(
                    reader,
                    *ti,
                    *next..stop,
                    writer,
                    *dst,
                    -*origin,
                    &mut buf,
                    dry,
                )?;
                *next = stop;
            }
            if *next < range.end {
                left = true;
                ahead = ahead.min(t.seconds(t.samples[*next].dts.saturating_sub(*origin)));
            }
        }
        if !left {
            return Ok(());
        }
        window = (window + INTERLEAVE).max(ahead);
    }
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
    let brand = if reader.quicktime {
        Brand::Mov
    } else {
        Brand::Mp4
    };
    let mut dry = Writer::new(Sink { pos: 0, end: 0 }, Options { brand, reserve: 0 })?;
    feed(&reader, &cut, &mut dry, true)?;
    let moov = dry.moov_len()?;
    let reserve = moov
        + 8
        + if dry.position() > u32::MAX as u64 / 2 {
            moov
        } else {
            0
        };
    let mut writer = Writer::create_with(dst, Options { brand, reserve })?;
    feed(&reader, &cut, &mut writer, false)?;
    writer.finish()?;
    Ok(cut.span)
}
