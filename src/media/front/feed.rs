use std::collections::VecDeque;
use std::sync::Arc;

use crate::mkv::{self, Block, Cursor};

use super::base::on_grid;
use super::demux::Demux;
use super::sound::Sound;
use crate::media::{Error, Result};

const LEAD: f64 = 0.5;
const TAIL: f64 = 10.0;
const GAP: f64 = 0.001;

pub struct Feed {
    sound: Sound,
    pub rate: u32,
    pub channels: u16,
    from: f64,
    base: f64,
    start: f64,
    origin: Option<f64>,
    lead: usize,
    skip: Option<usize>,
    skipped: usize,
    buf: Vec<u8>,
    pcm: Vec<f32>,
    at: At,
}

enum At {
    Indexed {
        d: Demux,
        i: usize,
        next: usize,
        count: usize,
        end: f64,
    },
    Mkv(Box<Lazy>),
}

impl Feed {
    pub fn open(d: &Demux, i: usize, from: f64) -> Result<Feed> {
        let sound = Sound::open(d, i)?;
        let starts = d.starts();
        let base = starts.file;
        let from = from.max(0.0) + base;
        let from_c = from;
        let (at, start, origin) = match d {
            Demux::Mp4(r) => {
                let t = r
                    .tracks
                    .get(i)
                    .ok_or_else(|| Error::Invalid(format!("no track {i}")))?;
                let from_t = t.ticks(from_c);
                let first = t
                    .samples
                    .partition_point(|s| {
                        s.pts.saturating_add(t.edit_offset) + s.duration as i64 <= from_t
                    })
                    .saturating_sub(1);
                let start = t.seconds(t.presentation(first).unwrap_or(0));
                let lead = starts.track(i).map_or(0.0, |s| (s - base).max(0.0));
                let at = At::Indexed {
                    d: d.clone(),
                    i,
                    next: first,
                    count: t.samples.len(),
                    end: (lead + t.duration_seconds()).max(0.0),
                };
                (at, start, None)
            }
            Demux::Mkv(r) => {
                let (lazy, start, origin) = Lazy::open(r.clone(), i, from_c, &sound)?;
                (At::Mkv(Box::new(lazy)), start, origin)
            }
        };
        let rate = sound.rate().max(1);
        let channels = sound.channels().max(1);
        let mut feed = Feed {
            sound,
            rate,
            channels,
            from,
            base,
            start,
            origin,
            lead: 0,
            skip: None,
            skipped: 0,
            buf: Vec::new(),
            pcm: Vec::new(),
            at,
        };
        if feed.origin.is_none() {
            feed.skip = Some(feed.skip_from(start));
            feed.lead = feed.lead_to(start);
        }
        Ok(feed)
    }

    fn lead_to(&self, start: f64) -> usize {
        if start - self.from >= GAP {
            ((start - self.from) * self.rate as f64).round() as usize
        } else {
            0
        }
    }

    fn skip_from(&self, start: f64) -> usize {
        if self.from > 0.0 || start < 0.0 {
            ((self.from.max(0.0) - start) * self.rate as f64)
                .round()
                .max(0.0) as usize
        } else {
            0
        }
    }

    pub fn end(&self) -> f64 {
        match &self.at {
            At::Indexed { end, .. } => *end,
            At::Mkv(l) => match l.end(&self.sound) {
                e if e > 0.0 => (e - self.base).max(0.0),
                _ => 0.0,
            },
        }
    }

    pub fn want(&self, to: f64) -> Option<usize> {
        let end = self.end();
        let to = if end > 0.0 { to.min(end) } else { to };
        to.is_finite().then(|| {
            ((to + self.base - self.from) * self.rate as f64)
                .round()
                .max(0.0) as usize
        })
    }

    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        let data = match &mut self.at {
            At::Indexed {
                d, i, next, count, ..
            } => {
                if *next >= *count {
                    return Ok(false);
                }
                d.read_into(*i, *next, &mut self.buf)?;
                *next += 1;
                std::mem::take(&mut self.buf)
            }
            At::Mkv(l) => match l.take(&self.sound)? {
                Some(data) => data,
                None => return Ok(false),
            },
        };
        let got = self.sound.decode(&data, &mut self.pcm);
        match &mut self.at {
            At::Indexed { .. } => self.buf = data,
            At::Mkv(l) => l.spare.push(data),
        }
        let got = got?;
        let ch = got.max(1) as usize;
        let frames = self.pcm.len() / ch;
        let skip = match self.skip {
            Some(s) => s,
            None => {
                let start = match self.origin {
                    Some(o) => on_grid(self.start, o, frames as f64 / self.rate as f64),
                    None => self.start,
                };
                let s = self.skip_from(start);
                self.skip = Some(s);
                self.lead = self.lead_to(start);
                s
            }
        };
        if self.lead > 0 && got == self.channels {
            out.resize(out.len() + self.lead * ch, 0.0);
            self.lead = 0;
        }
        let drop = skip.saturating_sub(self.skipped).min(frames);
        self.skipped += drop;
        if got == self.channels {
            out.extend_from_slice(self.pcm.get(drop * ch..frames * ch).unwrap_or_default());
        }
        Ok(true)
    }
}

struct Clock {
    origin: f64,
    pos: u64,
}

struct Lazy {
    r: Arc<mkv::Reader>,
    cur: Cursor,
    i: usize,
    held: VecDeque<(Block, Vec<u8>)>,
    spare: Vec<Vec<u8>>,
    clock: Option<Clock>,
    prev: Option<Block>,
    last_dur: i64,
    end_ticks: i64,
    eof: bool,
}

impl Lazy {
    fn open(
        r: Arc<mkv::Reader>,
        i: usize,
        from: f64,
        sound: &Sound,
    ) -> Result<(Lazy, f64, Option<f64>)> {
        let t = r
            .tracks
            .get(i)
            .ok_or_else(|| Error::Invalid(format!("no track {i}")))?;
        let delay = t.codec_delay as f64 / 1e9;
        let fix = t.seconds(t.ticks(delay)) - delay;
        let time_base = t.time_base;
        let cur = Cursor::new(&r);
        let mut l = Lazy {
            r,
            cur,
            i,
            held: VecDeque::new(),
            spare: Vec::new(),
            clock: None,
            prev: None,
            last_dur: 0,
            end_ticks: 0,
            eof: false,
        };
        let Some(first) = l.pull()? else {
            return Ok((l, 0.0, None));
        };
        let b0 = first.0;
        let n0 = sound.pcm_frames(first.1.len() as u32);
        l.held.push_back(first);
        let rate = sound.rate().max(1) as f64;
        let ticks = |s: f64| (s * time_base.den as f64 / time_base.num as f64).round() as i64;
        let first_s = time_base.seconds(b0.pts);
        match n0 {
            Some(n0) => {
                let origin = first_s;
                let mut pos = 0u64;
                let before = from - time_base.seconds(2);
                if from > origin + n0 as f64 / rate
                    && let Some((b, data)) = l.landing(before, ticks(before))?
                {
                    let n = sound.pcm_frames(data.len() as u32).unwrap_or(0);
                    let at = on_grid(time_base.seconds(b.pts), origin, n as f64 / rate);
                    if at <= from {
                        pos = ((at - origin) * rate).round().max(0.0) as u64;
                        l.held.push_back((b, data));
                    } else {
                        l.spare.push(data);
                        if let Some(b) = l.rewind()? {
                            l.held.push_back(b);
                        }
                    }
                }
                loop {
                    if l.held.is_empty() {
                        match l.pull()? {
                            Some(b) => l.held.push_back(b),
                            None => break,
                        }
                    }
                    let Some((_, data)) = l.held.front() else {
                        break;
                    };
                    let n = sound.pcm_frames(data.len() as u32).unwrap_or(0);
                    if origin + (pos + n) as f64 / rate > from {
                        break;
                    }
                    pos += n;
                    if let Some((_, data)) = l.held.pop_front() {
                        l.spare.push(data);
                    }
                }
                l.clock = Some(Clock { origin, pos });
                Ok((l, origin + pos as f64 / rate, None))
            }
            None => {
                let origin = first_s + fix;
                let from_t = ticks(from);
                if from - LEAD > first_s
                    && let Some(landed) = l.landing(from - LEAD, ticks(from - LEAD))?
                {
                    l.held.push_back(landed);
                }
                let mut before: Option<(Block, Vec<u8>)> = None;
                loop {
                    while l.held.len() < 2 && !l.eof {
                        match l.pull()? {
                            Some(b) => l.held.push_back(b),
                            None => break,
                        }
                    }
                    let Some((a, _)) = l.held.front() else {
                        break;
                    };
                    let dur = if a.duration > 0 {
                        a.duration as i64
                    } else {
                        match l.held.get(1) {
                            Some((b, _)) => b.pts.saturating_sub(a.pts).max(0),
                            None => l.last_dur,
                        }
                    };
                    if a.pts.saturating_add(dur) > from_t {
                        break;
                    }
                    if let Some(old) = before.take() {
                        l.spare.push(old.1);
                    }
                    before = l.held.pop_front();
                }
                if let Some(b) = before {
                    l.held.push_front(b);
                }
                let start = l
                    .held
                    .front()
                    .map_or(first_s, |(b, _)| time_base.seconds(b.pts))
                    + fix;
                Ok((l, start, Some(origin)))
            }
        }
    }

    fn landing(&mut self, seconds: f64, ticks: i64) -> Result<Option<(Block, Vec<u8>)>> {
        let first = self.held.front().map(|(b, _)| b.pts);
        if self.cur.seek_near(&self.r, self.i, seconds).is_err() {
            self.restart()?;
            return Ok(None);
        }
        self.prev = None;
        self.eof = false;
        match self.pull()? {
            Some((b, data)) if b.pts <= ticks && first.is_none_or(|f| b.pts > f) => {
                while let Some((_, d)) = self.held.pop_front() {
                    self.spare.push(d);
                }
                Ok(Some((b, data)))
            }
            other => {
                if let Some((_, d)) = other {
                    self.spare.push(d);
                }
                self.restart()?;
                Ok(None)
            }
        }
    }

    fn restart(&mut self) -> Result<()> {
        if let Some((_, d)) = self.rewind()? {
            self.spare.push(d);
        }
        Ok(())
    }

    fn rewind(&mut self) -> Result<Option<(Block, Vec<u8>)>> {
        self.cur.rewind(&self.r);
        self.eof = false;
        self.prev = None;
        self.pull()
    }

    fn pull(&mut self) -> Result<Option<(Block, Vec<u8>)>> {
        if self.eof {
            return Ok(None);
        }
        let mut data = self.spare.pop().unwrap_or_default();
        match self.cur.next_of(&self.r, Some(self.i), &mut data)? {
            Some(b) => {
                if let Some(p) = self.prev.take() {
                    let d = if p.duration > 0 {
                        p.duration as i64
                    } else {
                        b.pts.saturating_sub(p.pts).max(0)
                    };
                    self.close(p, d);
                }
                self.prev = Some(b);
                Ok(Some((b, data)))
            }
            None => {
                self.eof = true;
                if let Some(p) = self.prev.take() {
                    let d = if p.duration > 0 {
                        p.duration as i64
                    } else {
                        self.last_dur
                    };
                    self.close(p, d);
                }
                self.spare.push(data);
                Ok(None)
            }
        }
    }

    fn close(&mut self, b: Block, d: i64) {
        if d > 0 {
            self.last_dur = d;
        }
        self.end_ticks = self.end_ticks.max(b.pts.saturating_add(d));
    }

    fn take(&mut self, sound: &Sound) -> Result<Option<Vec<u8>>> {
        let next = match self.held.pop_front() {
            Some(b) => Some(b),
            None => self.pull()?,
        };
        let Some((_, data)) = next else {
            return Ok(None);
        };
        if let Some(c) = self.clock.as_mut() {
            c.pos += sound.pcm_frames(data.len() as u32).unwrap_or(0);
        }
        Ok(Some(data))
    }

    fn end(&self, sound: &Sound) -> f64 {
        if !self.eof || !self.held.is_empty() {
            return 0.0;
        }
        match &self.clock {
            Some(c) => (c.origin + c.pos as f64 / sound.rate().max(1) as f64).max(0.0),
            None => self
                .r
                .tracks
                .get(self.i)
                .map_or(0.0, |t| t.seconds(self.end_ticks).max(0.0)),
        }
    }
}

pub fn mkv_end(r: &Arc<mkv::Reader>, i: usize) -> Result<f64> {
    let d = Demux::Mkv(r.clone());
    let sound = Sound::open(&d, i)?;
    let last_cue = r
        .cues
        .last()
        .and_then(|c| r.tracks.get(i).map(|t| t.seconds(c.time as i64)))
        .unwrap_or(0.0);
    let guess = r.duration_seconds().max(last_cue);
    let (mut l, _, _) = Lazy::open(r.clone(), i, (guess - TAIL).max(0.0), &sound)?;
    while let Some(data) = l.take(&sound)? {
        l.spare.push(data);
    }
    Ok(l.end(&sound))
}
