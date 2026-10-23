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
const CAL: usize = 32;
const ROUNDING: f64 = 0.0011;

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
    first_at: f64,
    decoded: u64,
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
            first_at: start,
            decoded: 0,
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
                    Some(o) => {
                        let step = self.sound.grid().unwrap_or(frames);
                        on_grid(self.start, o, step as f64 / self.rate as f64)
                    }
                    None => self.start,
                };
                let s = self.skip_from(start);
                self.skip = Some(s);
                self.first_at = start;
                self.lead = self.lead_to(start);
                s
            }
        };
        if self.lead > 0 && got == self.channels {
            out.resize(out.len() + self.lead * ch, 0.0);
            self.lead = 0;
        }
        self.decoded += frames as u64;
        if let At::Mkv(l) = &mut self.at {
            if self.sound.counts_frames() {
                l.tail = Some(self.first_at + self.decoded as f64 / self.rate.max(1) as f64);
            } else {
                let frames = if l.size.is_some() {
                    l.spans
                } else {
                    self.decoded
                };
                l.spanned = Some(l.spanned_to(self.first_at, frames, self.rate));
            }
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
    taken: i64,
    tail: Option<f64>,
    spanned: Option<f64>,
    cut: u64,
    spans: u64,
    size: Option<usize>,
    fix: f64,
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
        let raw = t.seconds(t.ticks(delay));
        let fix = raw - delay;
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
            taken: 0,
            tail: None,
            spanned: None,
            cut: 0,
            spans: 0,
            size: None,
            fix,
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
                let fix = match l.held.front().and_then(|(_, d)| sound.block(d)) {
                    Some((_, lead)) => raw - lead as f64 / rate,
                    None => fix,
                };
                let origin = first_s + fix;
                let from_t = ticks(from);
                let seeks = from - LEAD > first_s;
                let cal = match l.held.front().and_then(|(_, d)| sound.block(d)) {
                    Some(_) => l.calibrate(sound, seeks, from_t)?,
                    None => None,
                };
                let mut landed = false;
                if seeks && let Some(b) = l.landing(from - LEAD, ticks(from - LEAD))? {
                    l.held.push_back(b);
                    landed = true;
                }
                let mut popped = 0usize;
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
                    popped += 1;
                }
                if let Some(b) = before {
                    l.held.push_front(b);
                    popped -= 1;
                }
                if let Some(cal) = cal
                    && let Some(at) =
                        l.place(sound, &cal, (!landed).then_some(popped), time_base)?
                {
                    return Ok((l, origin + at as f64 / rate, None));
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

    fn calibrate(
        &mut self,
        sound: &Sound,
        seeks: bool,
        until: i64,
    ) -> Result<Option<Vec<(i64, u64)>>> {
        while self.held.len() < CAL
            || (!seeks && self.held.back().is_some_and(|(b, _)| b.pts <= until))
        {
            match self.pull()? {
                Some(b) => self.held.push_back(b),
                None => break,
            }
        }
        let cal = walk(sound, self.held.iter(), true);
        if seeks && self.held.len() > 1 {
            while self.held.len() > 1 {
                if let Some((_, d)) = self.held.pop_back() {
                    self.spare.push(d);
                }
            }
            if let Some((_, d)) = self.rewind()? {
                self.spare.push(d);
            }
        }
        Ok(cal)
    }

    fn place(
        &mut self,
        sound: &Sound,
        cal: &[(i64, u64)],
        index: Option<usize>,
        tb: crate::media::TimeBase,
    ) -> Result<Option<i64>> {
        let Some((_, lead)) = self.held.front().and_then(|(_, d)| sound.block(d)) else {
            return Ok(None);
        };
        if let Some(&(_, r)) = index.and_then(|b| cal.get(b + 1)) {
            return Ok(Some(r as i64 - lead as i64));
        }
        while self.held.len() <= CAL {
            match self.pull()? {
                Some(b) => self.held.push_back(b),
                None => break,
            }
        }
        let rate = i128::from(sound.rate().max(1));
        let tick = i128::from(tb.num) * rate;
        let sample = i128::from(tb.den);
        let found = walk(sound, self.held.iter(), false).and_then(|local| {
            let grid = sound.grid()? as i128;
            let exact = match (bounds(cal, tick, sample), bounds(&local, tick, sample)) {
                (Some(u), Some(s)) => resolve(u, s, sample, grid),
                _ => None,
            };
            exact.or_else(|| {
                nearest(
                    middle(cal, tick, sample)?,
                    middle(&local, tick, sample)?,
                    sample,
                    grid,
                )
            })
        });
        Ok(found.map(|a| a - lead as i64))
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
        let Some((b, data)) = next else {
            return Ok(None);
        };
        self.taken = b.pts;
        self.cut = match b.discard {
            d if d > 0 => {
                let rate = sound.rate().max(1) as f64;
                self.cut
                    .saturating_add((d as f64 * rate / 1e9).round() as u64)
            }
            _ => 0,
        };
        if let Some(c) = self.clock.as_mut() {
            c.pos += sound.pcm_frames(data.len() as u32).unwrap_or(0);
        }
        if let Some((n, lead)) = sound.block(&data) {
            self.spans += match self.size {
                Some(p) => (p / 4 + n / 4) as u64,
                None => lead as u64,
            };
            self.size = Some(n);
        }
        Ok(Some(data))
    }

    fn spanned_to(&self, from: f64, frames: u64, rate: u32) -> f64 {
        from + (frames as f64 - self.cut as f64) / rate.max(1) as f64
    }

    fn end(&self, sound: &Sound) -> f64 {
        if !self.eof || !self.held.is_empty() {
            return 0.0;
        }
        match &self.clock {
            Some(c) => (c.origin + c.pos as f64 / sound.rate().max(1) as f64).max(0.0),
            None => self.r.tracks.get(self.i).map_or(0.0, |t| {
                let stamped = t.seconds(self.end_ticks);
                match (self.tail, self.spanned) {
                    (Some(e), _) => e,
                    (None, Some(e))
                        if self.cut > 0
                            || self.size.is_some()
                            || (e - stamped).abs() <= ROUNDING =>
                    {
                        e
                    }
                    _ => stamped,
                }
                .max(0.0)
            }),
        }
    }
}

pub fn mkv_end(r: &Arc<mkv::Reader>, i: usize) -> Result<f64> {
    let d = Demux::Mkv(r.clone());
    let mut sound = Sound::open(&d, i)?;
    let last_cue = r
        .cues
        .last()
        .and_then(|c| r.tracks.get(i).map(|t| t.seconds(c.time as i64)))
        .unwrap_or(0.0);
    let guess = r.duration_seconds().max(last_cue);
    let (mut l, start, origin) = Lazy::open(r.clone(), i, (guess - TAIL).max(0.0), &sound)?;
    let mut prev: Option<Vec<u8>> = None;
    let mut last: Option<Vec<u8>> = None;
    while let Some(data) = l.take(&sound)? {
        if let Some(old) = std::mem::replace(&mut prev, last.replace(data)) {
            l.spare.push(old);
        }
    }
    let rate = sound.rate().max(1);
    if l.size.is_some() {
        let start = match (origin, sound.grid()) {
            (Some(o), Some(g)) => on_grid(start, o, g as f64 / rate as f64),
            _ => start,
        };
        l.spanned = Some(l.spanned_to(start, l.spans, rate));
    } else if l.clock.is_none()
        && let Some(data) = last
        && let Some(t) = r.tracks.get(i)
    {
        let mut pcm = Vec::new();
        let full = prev.and_then(|p| frames_of(&mut sound, &p, &mut pcm));
        if let Some(n) = frames_of(&mut sound, &data, &mut pcm) {
            let at = t.seconds(l.taken) + l.fix;
            let at = match (origin, full) {
                (Some(o), Some(f)) => on_grid(at, o, f as f64 / rate as f64),
                _ => at,
            };
            if sound.counts_frames() {
                l.tail = Some(at + n as f64 / rate as f64);
            } else {
                l.spanned = Some(l.spanned_to(at, n, rate));
            }
        }
    }
    Ok(l.end(&sound))
}

fn walk<'a>(
    sound: &Sound,
    mut blocks: impl Iterator<Item = &'a (Block, Vec<u8>)>,
    origin: bool,
) -> Option<Vec<(i64, u64)>> {
    let (b0, d0) = blocks.next()?;
    let (s0, lead) = sound.block(d0)?;
    let mut at = 0u64;
    let mut out = Vec::new();
    if origin {
        out.push((b0.pts, 0));
        at = lead as u64;
    }
    let mut sizes = (s0, s0);
    for (k, (b, d)) in blocks.enumerate() {
        let (n, _) = sound.block(d)?;
        if k > 0 {
            at += (sizes.0 / 4 + sizes.1 / 4) as u64;
        }
        out.push((b.pts, at));
        sizes = (sizes.1, n);
    }
    Some(out)
}

fn bounds(points: &[(i64, u64)], tick: i128, sample: i128) -> Option<(i128, i128)> {
    let mut lo = i128::MIN;
    let mut hi = i128::MAX;
    for &(t, r) in points {
        let base = i128::from(t) * tick - i128::from(r) * sample;
        lo = lo.max(base);
        hi = hi.min(base + tick);
    }
    (hi > lo && !points.is_empty()).then_some((lo, hi))
}

fn middle(points: &[(i64, u64)], tick: i128, sample: i128) -> Option<i128> {
    let mut bases: Vec<i128> = points
        .iter()
        .map(|&(t, r)| i128::from(t) * tick - i128::from(r) * sample)
        .collect();
    bases.sort_unstable();
    bases.get(bases.len() / 2).map(|b| b + tick / 2)
}

fn nearest(u: i128, s: i128, sample: i128, grid: i128) -> Option<i64> {
    let step = sample * grid;
    if step <= 0 {
        return None;
    }
    let k = (s - u + step / 2).div_euclid(step);
    (k >= 0).then(|| i64::try_from(k * grid).ok()).flatten()
}

fn resolve(u: (i128, i128), s: (i128, i128), sample: i128, grid: i128) -> Option<i64> {
    let lo = s.0 - (u.1 - 1);
    let hi = (s.1 - 1) - u.0;
    let step = sample * grid;
    if step <= 0 {
        return None;
    }
    let k = lo.div_euclid(step) + i128::from(lo.rem_euclid(step) != 0);
    let a = k * step;
    (a <= hi && a + step > hi && k >= 0)
        .then(|| i64::try_from(k * grid).ok())
        .flatten()
}

fn frames_of(sound: &mut Sound, data: &[u8], pcm: &mut Vec<f32>) -> Option<u64> {
    let ch = sound.decode(data, pcm).ok()?;
    Some((pcm.len() / ch.max(1) as usize) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: i128 = 96_000;
    const SAMPLE: i128 = 1_000;

    fn stamps(u: i128, at: &[u64]) -> Vec<(i64, u64)> {
        at.iter()
            .map(|&r| {
                let t = (u + i128::from(r) * SAMPLE).div_euclid(TICK);
                (t as i64, r)
            })
            .collect()
    }

    fn longs(first: u64, n: u64) -> Vec<u64> {
        (0..n).map(|k| first + k * 1024).collect()
    }

    #[test]
    fn millisecond_stamps_pin_a_96k_block_to_its_sample() {
        for (u, a) in [
            (0, 64 * 1371),
            (95_999, 64 * 1372),
            (48_000, 64 * 7),
            (12_345, 0),
        ] {
            let mut cal = vec![0];
            cal.extend(longs(128, 31));
            let cal = stamps(u, &cal);
            let local: Vec<(i64, u64)> = stamps(u + i128::from(a) * SAMPLE, &longs(0, 32));
            let got = resolve(
                bounds(&cal, TICK, SAMPLE).unwrap(),
                bounds(&local, TICK, SAMPLE).unwrap(),
                SAMPLE,
                64,
            );
            assert_eq!(got, Some(a as i64), "u {u}");
        }
    }

    #[test]
    fn stamps_off_by_more_than_rounding_still_place_a_block_from_their_middle() {
        for (u, a) in [(0, 64 * 1371), (95_999, 64 * 1372), (48_000, 64 * 7)] {
            let mut cal = vec![0];
            cal.extend(longs(128, 31));
            let mut cal = stamps(u, &cal);
            let mut local: Vec<(i64, u64)> = stamps(u + i128::from(a) * SAMPLE, &longs(0, 32));
            cal[5].0 += 3;
            local[9].0 -= 2;
            local[20].0 += 4;
            assert_eq!(bounds(&local, TICK, SAMPLE), None);
            let got = nearest(
                middle(&cal, TICK, SAMPLE).unwrap(),
                middle(&local, TICK, SAMPLE).unwrap(),
                SAMPLE,
                64,
            );
            assert_eq!(got, Some(a as i64), "u {u}");
        }
        assert_eq!(middle(&[], TICK, SAMPLE), None);
        assert_eq!(nearest(0, 1, 0, 64), None);
        assert_eq!(nearest(64 * SAMPLE * 10, 0, SAMPLE, 64), None);
    }

    #[test]
    fn too_few_or_inconsistent_stamps_place_nothing() {
        assert_eq!(bounds(&[], TICK, SAMPLE), None);
        assert_eq!(bounds(&[(0, 0), (5, 0)], TICK, SAMPLE), None);
        let one = bounds(&[(10, 0)], TICK, SAMPLE).unwrap();
        assert_eq!(resolve(one, one, SAMPLE, 64), None);
        assert_eq!(resolve((0, 1), (0, 1), 0, 64), None);
        assert_eq!(resolve((1_000_000, 1_000_001), (0, 1), SAMPLE, 64), None);
    }
}
