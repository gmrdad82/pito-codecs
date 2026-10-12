use std::collections::VecDeque;

use super::ebml::Size;
use super::ids::*;
use super::parse::Cluster;
use super::reader::{
    Cache, FrameRef, Io, Reader, expand, parse_block_elem, read_frame, scan_cluster,
};
use crate::media::{Error, Packet, Result};
use crate::mp4::ByteSource;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    pub track: usize,
    pub pts: i64,
    pub duration: u32,
    pub keyframe: bool,
}

#[derive(Clone, Copy, Debug)]
struct InCluster {
    ts: i64,
    end: Option<u64>,
}

pub(crate) struct Cursor {
    cache: Cache,
    pos: u64,
    cluster: Option<InCluster>,
    pending: VecDeque<FrameRef>,
    frames: Vec<(u64, u32)>,
    skip: Option<(u64, u16)>,
    done: bool,
}

pub struct Stream<'r, S: ByteSource> {
    reader: &'r Reader<S>,
    cursor: Cursor,
}

impl<S: ByteSource> Reader<S> {
    pub fn stream(&self) -> Stream<'_, S> {
        Stream {
            reader: self,
            cursor: Cursor::new(self),
        }
    }
}

impl<S: ByteSource> Stream<'_, S> {
    pub fn rewind(&mut self) {
        self.cursor.rewind(self.reader);
    }

    pub fn next_packet(&mut self) -> Result<Option<(usize, Packet)>> {
        let mut data = Vec::new();
        Ok(self.next_into(&mut data)?.map(|b| {
            (
                b.track,
                Packet {
                    data,
                    pts: b.pts,
                    dts: b.pts,
                    keyframe: b.keyframe,
                },
            )
        }))
    }

    pub fn next_into(&mut self, data: &mut Vec<u8>) -> Result<Option<Block>> {
        self.cursor.next_of(self.reader, None, data)
    }

    pub fn seek(&mut self, track: usize, seconds: f64) -> Result<f64> {
        self.cursor.seek(self.reader, track, seconds)
    }
}

impl Cursor {
    pub(crate) fn new<S: ByteSource>(r: &Reader<S>) -> Cursor {
        Cursor {
            cache: Cache::default(),
            pos: r.first_cluster.unwrap_or(r.segment_end),
            cluster: None,
            pending: VecDeque::new(),
            frames: Vec::new(),
            skip: None,
            done: false,
        }
    }

    pub(crate) fn rewind<S: ByteSource>(&mut self, r: &Reader<S>) {
        self.pos = r.first_cluster.unwrap_or(r.segment_end);
        self.cluster = None;
        self.pending.clear();
        self.skip = None;
        self.done = false;
    }

    pub(crate) fn next_of<S: ByteSource>(
        &mut self,
        r: &Reader<S>,
        track: Option<usize>,
        data: &mut Vec<u8>,
    ) -> Result<Option<Block>> {
        loop {
            if let Some(f) = self.pending.pop_front() {
                if track.is_some_and(|t| t != f.track) {
                    continue;
                }
                let t = r
                    .tracks
                    .get(f.track)
                    .ok_or_else(|| Error::Invalid(format!("no track {}", f.track)))?;
                read_frame(r.source(), r.len(), t, f.offset, f.size, data)?;
                return Ok(Some(Block {
                    track: f.track,
                    pts: f.pts,
                    duration: f.duration,
                    keyframe: f.keyframe,
                }));
            }
            if self.done || !self.fill(r)? {
                self.done = true;
                return Ok(None);
            }
        }
    }

    pub(crate) fn seek<S: ByteSource>(
        &mut self,
        r: &Reader<S>,
        track: usize,
        seconds: f64,
    ) -> Result<f64> {
        let t = r
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let ticks = t.ticks(seconds);
        if !t.samples.is_empty() {
            let k = t
                .keyframe_at(ticks)
                .ok_or_else(|| Error::Invalid(format!("track {track} has no keyframes")))?;
            let s = t.samples[k];
            let c = r
                .clusters
                .get(s.cluster as usize)
                .copied()
                .ok_or_else(|| Error::Invalid(format!("no cluster for track {track}")))?;
            self.position_at(c, s.block, s.lace);
            return Ok(t.seconds(s.pts));
        }
        let cue = r
            .cues
            .iter()
            .filter(|c| c.track == t.number)
            .take_while(|c| c.time as i64 <= ticks)
            .last()
            .or_else(|| r.cues.iter().find(|c| c.track == t.number));
        if let Some(at) = cue.and_then(|c| r.segment_start.checked_add(c.cluster))
            && let Some(pts) = self.seek_in(r, track, ticks, &[at])?
        {
            return Ok(t.seconds(pts));
        }
        self.seek_scan(r, track, ticks)
    }

    pub(crate) fn seek_near<S: ByteSource>(
        &mut self,
        r: &Reader<S>,
        track: usize,
        seconds: f64,
    ) -> Result<f64> {
        let t = r
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        if !t.samples.is_empty() {
            return self.seek(r, track, seconds);
        }
        let ticks = t.ticks(seconds);
        let mut near: Vec<u64> = Vec::with_capacity(3);
        for c in r.cues.iter().take_while(|c| c.time as i64 <= ticks) {
            let Some(at) = r.segment_start.checked_add(c.cluster) else {
                continue;
            };
            if near.last() != Some(&at) {
                near.retain(|&p| p != at);
                near.push(at);
                if near.len() > 2 {
                    near.remove(0);
                }
            }
        }
        near.reverse();
        if !near.is_empty()
            && let Some(pts) = self.seek_in(r, track, ticks, &near)?
        {
            return Ok(t.seconds(pts));
        }
        self.seek_scan(r, track, ticks)
    }

    fn seek_scan<S: ByteSource>(&mut self, r: &Reader<S>, track: usize, ticks: i64) -> Result<f64> {
        let t = r
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let candidates = self.clusters_before(r, ticks)?;
        match self.seek_in(r, track, ticks, &candidates)? {
            Some(pts) => Ok(t.seconds(pts)),
            None => Err(Error::Invalid(format!("track {track} has no keyframes"))),
        }
    }

    fn clusters_before<S: ByteSource>(&mut self, r: &Reader<S>, ticks: i64) -> Result<Vec<u64>> {
        let mut io = Io::new(r.source(), &mut self.cache);
        let mut pos = r.first_cluster.unwrap_or(r.segment_end);
        let mut out = Vec::new();
        while let Some(h) = io.header(pos, r.segment_end)? {
            if h.id != CLUSTER {
                if matches!(h.id, EBML | SEGMENT) || h.size == Size::Unknown {
                    break;
                }
                pos = h.end(r.segment_end);
                continue;
            }
            let end = match h.size {
                Size::Known(_) => h.end(r.segment_end),
                Size::Unknown => io.unknown_end(h.data_start(), r.segment_end)?,
            };
            let ts = match io.header(h.data_start(), end)? {
                Some(c) if c.id == TIMESTAMP => io.uint(&c, c.end(end))? as i64,
                _ => 0,
            };
            if ts > ticks && !out.is_empty() {
                break;
            }
            out.push(h.pos);
            if end <= pos {
                break;
            }
            pos = end;
        }
        out.reverse();
        Ok(out)
    }

    fn seek_in<S: ByteSource>(
        &mut self,
        r: &Reader<S>,
        track: usize,
        ticks: i64,
        clusters: &[u64],
    ) -> Result<Option<i64>> {
        let scale = r.info.timestamp_scale;
        let t = r
            .tracks
            .get(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        let last = clusters.last().copied();
        for &at in clusters {
            let mut io = Io::new(r.source(), &mut self.cache);
            let Some(h) = io.header(at, r.segment_end)? else {
                continue;
            };
            if h.id != CLUSTER {
                continue;
            }
            let mut warnings = Vec::new();
            let mut keys: Vec<FrameRef> = Vec::new();
            let ci = scan_cluster(
                &mut io,
                &h,
                r.segment_end,
                &r.tracks,
                &mut warnings,
                &mut self.frames,
                |b, frames| {
                    if b.track == track && b.keyframe {
                        expand(b, frames, t, scale, 0, |f| keys.push(f));
                    }
                },
            )?;
            let best = keys
                .iter()
                .filter(|f| f.pts <= ticks)
                .max_by_key(|f| f.pts)
                .or_else(|| {
                    (Some(at) == last)
                        .then(|| keys.iter().min_by_key(|f| f.pts))
                        .flatten()
                })
                .copied();
            if let Some(f) = best {
                self.position_at(ci, f.block, f.lace);
                return Ok(Some(f.pts));
            }
        }
        Ok(None)
    }

    fn position_at(&mut self, c: Cluster, block: u64, lace: u16) {
        self.pending.clear();
        self.done = false;
        self.pos = block;
        self.cluster = Some(InCluster {
            ts: c.timestamp as i64,
            end: Some(c.end),
        });
        self.skip = (lace > 0).then_some((block, lace));
    }

    fn fill<S: ByteSource>(&mut self, r: &Reader<S>) -> Result<bool> {
        let seg_end = r.segment_end;
        let scale = r.info.timestamp_scale;
        let mut io = Io::new(r.source(), &mut self.cache);
        loop {
            if let Some(c) = self.cluster {
                let lim = c.end.unwrap_or(seg_end);
                if self.pos >= lim {
                    self.cluster = None;
                    continue;
                }
                let h = match io.header(self.pos, lim)? {
                    Some(h) if !(h.size == Size::Unknown || h.overruns(lim)) => h,
                    Some(h) if c.end.is_none() && is_top_level(h.id) => h,
                    _ => {
                        self.cluster = None;
                        match io.resync(self.pos + 1, seg_end)? {
                            Some(p) => {
                                self.pos = p;
                                continue;
                            }
                            None => return Ok(false),
                        }
                    }
                };
                if c.end.is_none() && is_top_level(h.id) {
                    self.cluster = None;
                    continue;
                }
                let he = h.end(lim);
                self.pos = he;
                match h.id {
                    TIMESTAMP => {
                        let ts = io.uint(&h, he)? as i64;
                        self.cluster = Some(InCluster { ts, end: c.end });
                    }
                    SIMPLE_BLOCK | BLOCK_GROUP => {
                        if let Some(b) =
                            parse_block_elem(&mut io, &h, he, c.ts, &r.tracks, &mut self.frames)?
                        {
                            let skip = match self.skip.take() {
                                Some((off, n)) if off == h.pos => n,
                                _ => 0,
                            };
                            let pending = &mut self.pending;
                            if let Some(t) = r.tracks.get(b.track) {
                                expand(&b, &self.frames, t, scale, skip, |f| pending.push_back(f));
                            }
                            if !self.pending.is_empty() {
                                return Ok(true);
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            if self.pos >= seg_end {
                return Ok(false);
            }
            let Some(h) = io.header(self.pos, seg_end)? else {
                match io.resync(self.pos + 1, seg_end)? {
                    Some(p) => {
                        self.pos = p;
                        continue;
                    }
                    None => return Ok(false),
                }
            };
            match h.id {
                CLUSTER => {
                    self.cluster = Some(InCluster {
                        ts: 0,
                        end: h.size.known().map(|_| h.end(seg_end)),
                    });
                    self.pos = h.data_start();
                }
                EBML | SEGMENT => return Ok(false),
                _ => {
                    self.pos = if h.size == Size::Unknown {
                        io.unknown_end(h.data_start(), seg_end)?
                    } else {
                        h.end(seg_end)
                    };
                    if self.pos <= h.pos {
                        return Ok(false);
                    }
                }
            }
        }
    }
}
