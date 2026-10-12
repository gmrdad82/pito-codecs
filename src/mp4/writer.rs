use crate::media::{Error, Packet, Result};
use crate::mp4::boxes::BoxBuf;
use crate::mp4::entry::SampleEntry;
use crate::mp4::reader::Edit;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const IDENTITY: [i32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];
const FLUSH_AT: usize = 4 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Brand {
    #[default]
    Mp4,
    Mov,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Options {
    pub brand: Brand,
    pub reserve: u64,
}

pub fn rotation_matrix(degrees: u16) -> [i32; 9] {
    match degrees % 360 {
        90 => [0, 0x10000, 0, -0x10000, 0, 0, 0, 0, 0x4000_0000],
        180 => [-0x10000, 0, 0, 0, -0x10000, 0, 0, 0, 0x4000_0000],
        270 => [0, -0x10000, 0, 0x10000, 0, 0, 0, 0, 0x4000_0000],
        _ => IDENTITY,
    }
}

#[derive(Clone, Debug)]
pub struct TrackConfig {
    pub entry: SampleEntry,
    pub timescale: u32,
    pub language: String,
    pub name: Option<String>,
    pub matrix: [i32; 9],
    pub edits: Vec<Edit>,
    pub media_start: Option<i64>,
}

impl TrackConfig {
    pub fn new(entry: SampleEntry, timescale: u32) -> Self {
        TrackConfig {
            entry,
            timescale,
            language: "und".into(),
            name: None,
            matrix: IDENTITY,
            edits: Vec::new(),
            media_start: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handler {
    Video,
    Audio,
}

#[derive(Clone, Debug, Default)]
struct Tables {
    stts: Vec<(u32, u32)>,
    ctts: Vec<(u32, i32)>,
    sizes: Vec<u32>,
    sync: Vec<u32>,
    all_sync: bool,
    chunks: Vec<(u64, u32, u32)>,
    media_duration: u64,
    comp_start: Option<i64>,
    comp_end: i64,
}

impl Tables {
    fn reach_end(&mut self) {
        let end = u64::try_from(self.comp_end).unwrap_or(0);
        let Some(extra) = end.checked_sub(self.media_duration).filter(|&e| e > 0) else {
            return;
        };
        let Some(last) = self.stts.last_mut() else {
            return;
        };
        let Ok(d) = u32::try_from(last.1 as u64 + extra) else {
            return;
        };
        if last.0 > 1 {
            last.0 -= 1;
            self.stts.push((1, d));
        } else {
            last.1 = d;
        }
        self.media_duration = end;
    }
}

fn push_run<T: PartialEq + Copy>(runs: &mut Vec<(u32, T)>, v: T) {
    if let Some(last) = runs.last_mut()
        && last.1 == v
        && last.0 < u32::MAX
    {
        last.0 += 1;
        return;
    }
    runs.push((1, v));
}

struct Pending {
    data: Vec<u8>,
    pts: i64,
    dts: i64,
    keyframe: bool,
}

struct WTrack {
    cfg: TrackConfig,
    entries: Vec<SampleEntry>,
    entry: u32,
    id: u32,
    handler: Handler,
    t: Tables,
    origin: Option<i64>,
    last_duration: u32,
    pending: Option<Pending>,
    spare: Vec<u8>,
}

impl WTrack {
    fn new(cfg: TrackConfig, id: u32) -> Result<Self> {
        if cfg.timescale == 0 {
            return Err(Error::Invalid("track timescale must be non-zero".into()));
        }
        let handler = if cfg.entry.is_video() {
            Handler::Video
        } else if cfg.entry.is_audio() {
            Handler::Audio
        } else {
            return Err(Error::Unsupported(
                "tracks other than audio and video".into(),
            ));
        };
        Ok(WTrack {
            entries: vec![cfg.entry.clone()],
            entry: 0,
            cfg,
            id,
            handler,
            t: Tables {
                all_sync: true,
                ..Default::default()
            },
            origin: None,
            last_duration: 0,
            pending: None,
            spare: Vec::new(),
        })
    }

    fn record(
        &mut self,
        size: u32,
        pts: i64,
        dts: i64,
        duration: u32,
        keyframe: bool,
    ) -> Result<i64> {
        let origin = *self.origin.get_or_insert(dts);
        let offset = pts
            .checked_sub(dts)
            .and_then(|d| i32::try_from(d).ok())
            .ok_or_else(|| Error::Invalid("composition offset out of range".into()))?;
        let t = &mut self.t;
        push_run(&mut t.stts, duration);
        push_run(&mut t.ctts, offset);
        t.sizes.push(size);
        if keyframe {
            t.sync.push(t.sizes.len() as u32);
        } else {
            t.all_sync = false;
        }
        let start = pts.saturating_sub(origin);
        t.comp_start = Some(t.comp_start.map_or(start, |s| s.min(start)));
        t.comp_end = t.comp_end.max(start.saturating_add(duration as i64));
        t.media_duration = t.media_duration.saturating_add(duration as u64);
        self.last_duration = duration;
        Ok(origin)
    }
}

fn pack_language(lang: &str) -> u16 {
    let b = lang.as_bytes();
    if b.len() == 3 && b.iter().all(|c| c.is_ascii_lowercase()) {
        (((b[0] - 0x60) as u16) << 10) | (((b[1] - 0x60) as u16) << 5) | ((b[2] - 0x60) as u16)
    } else {
        0x55C4
    }
}

fn rescale(v: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    (v as u128 * to as u128 / from as u128).min(u64::MAX as u128) as u64
}

fn write_ftyp(b: &mut BoxBuf, brand: Brand) {
    let m = b.start(b"ftyp");
    match brand {
        Brand::Mp4 => {
            b.bytes(b"isom");
            b.u32(0x200);
            for c in [b"isom", b"iso2", b"avc1", b"mp41"] {
                b.bytes(c);
            }
        }
        Brand::Mov => {
            b.bytes(b"qt  ");
            b.u32(0x200);
            b.bytes(b"qt  ");
        }
    }
    b.end(m);
}

fn effective_edits(t: &WTrack, movie_ts: u32) -> Vec<Edit> {
    if !t.cfg.edits.is_empty() {
        return t.cfg.edits.clone();
    }
    let start = t
        .cfg
        .media_start
        .unwrap_or_else(|| t.t.comp_start.unwrap_or(0).max(0));
    if start == 0 {
        return Vec::new();
    }
    let end = t.t.comp_end.max(t.t.media_duration as i64);
    let rest = end.saturating_sub(start).max(0) as u64;
    vec![Edit {
        segment_duration: rescale(rest, t.cfg.timescale, movie_ts),
        media_time: start,
        media_rate: 0x10000,
    }]
}

fn presentation_duration(t: &WTrack, movie_ts: u32) -> u64 {
    let e = effective_edits(t, movie_ts);
    if e.is_empty() {
        rescale(t.t.media_duration, t.cfg.timescale, movie_ts)
    } else {
        e.iter()
            .fold(0u64, |a, e| a.saturating_add(e.segment_duration))
    }
}

fn write_times(b: &mut BoxBuf, v1: bool, timescale: u32, duration: u64) {
    b.zeros(if v1 { 16 } else { 8 });
    b.u32(timescale);
    write_duration(b, v1, duration);
}

fn write_duration(b: &mut BoxBuf, v1: bool, duration: u64) {
    if v1 {
        b.u64(duration);
    } else {
        b.u32(duration as u32);
    }
}

fn write_mvhd(b: &mut BoxBuf, timescale: u32, duration: u64, next_id: u32) {
    let v1 = duration > u32::MAX as u64;
    let m = b.start_full(b"mvhd", v1 as u8, 0);
    write_times(b, v1, timescale, duration);
    b.u32(0x10000);
    b.u16(0x100);
    b.zeros(10);
    for v in IDENTITY {
        b.i32(v);
    }
    b.zeros(24);
    b.u32(next_id);
    b.end(m);
}

fn write_trak(
    b: &mut BoxBuf,
    t: &WTrack,
    movie_ts: u32,
    shift: u64,
    co64: bool,
    fragmented: bool,
    qt: bool,
) -> Result<()> {
    let trak = b.start(b"trak");
    let dur = if fragmented {
        0
    } else {
        presentation_duration(t, movie_ts)
    };
    let v1 = dur > u32::MAX as u64;
    let m = b.start_full(b"tkhd", v1 as u8, 3);
    b.zeros(if v1 { 16 } else { 8 });
    b.u32(t.id);
    b.u32(0);
    write_duration(b, v1, dur);
    b.zeros(8);
    b.i16(0);
    b.u16(0);
    b.i16(if t.handler == Handler::Audio {
        0x100
    } else {
        0
    });
    b.u16(0);
    for v in t.cfg.matrix {
        b.i32(v);
    }
    let (w, h) = match &t.cfg.entry.video {
        Some(v) => {
            let w = match v.pixel_aspect {
                Some((hs, vs)) if hs > 0 && vs > 0 => {
                    (v.width as u64 * hs as u64 / vs as u64) as u32
                }
                _ => v.width as u32,
            };
            (w, v.height as u32)
        }
        None => (0, 0),
    };
    b.u32(w << 16);
    b.u32(h << 16);
    b.end(m);
    let edits = if fragmented {
        Vec::new()
    } else {
        effective_edits(t, movie_ts)
    };
    if !edits.is_empty() {
        let e = b.start(b"edts");
        let v1 = edits.iter().any(|e| {
            e.segment_duration > u32::MAX as u64
                || e.media_time > i32::MAX as i64
                || e.media_time < i32::MIN as i64
        });
        let l = b.start_full(b"elst", v1 as u8, 0);
        b.u32(edits.len() as u32);
        for ed in &edits {
            if v1 {
                b.u64(ed.segment_duration);
                b.i64(ed.media_time);
            } else {
                b.u32(ed.segment_duration as u32);
                b.i32(ed.media_time as i32);
            }
            b.i32(ed.media_rate);
        }
        b.end(l);
        b.end(e);
    }
    let mdia = b.start(b"mdia");
    let mv1 = t.t.media_duration > u32::MAX as u64;
    let m = b.start_full(b"mdhd", mv1 as u8, 0);
    write_times(b, mv1, t.cfg.timescale, t.t.media_duration);
    b.u16(pack_language(&t.cfg.language));
    b.u16(0);
    b.end(m);
    let (htype, default_name) = match t.handler {
        Handler::Video => (b"vide", "VideoHandler"),
        Handler::Audio => (b"soun", "SoundHandler"),
    };
    let name = t.cfg.name.as_deref().unwrap_or(default_name).as_bytes();
    let m = b.start_full(b"hdlr", 0, 0);
    b.bytes(if qt { b"mhlr" } else { &[0, 0, 0, 0] });
    b.bytes(htype);
    b.zeros(12);
    if qt {
        let n = name.len().min(255);
        b.u8(n as u8);
        b.bytes(&name[..n]);
    } else {
        b.bytes(name);
        b.u8(0);
    }
    b.end(m);
    let minf = b.start(b"minf");
    match t.handler {
        Handler::Video => {
            let m = b.start_full(b"vmhd", 0, 1);
            b.zeros(8);
            b.end(m);
        }
        Handler::Audio => {
            let m = b.start_full(b"smhd", 0, 0);
            b.zeros(4);
            b.end(m);
        }
    }
    if qt {
        let m = b.start_full(b"hdlr", 0, 0);
        b.bytes(b"dhlr");
        b.bytes(b"url ");
        b.zeros(12);
        b.u8(0);
        b.end(m);
    }
    let d = b.start(b"dinf");
    let r = b.start_full(b"dref", 0, 0);
    b.u32(1);
    let u = b.start_full(b"url ", 0, 1);
    b.end(u);
    b.end(r);
    b.end(d);
    let stbl = b.start(b"stbl");
    let m = b.start_full(b"stsd", 0, 0);
    b.u32(t.entries.len() as u32);
    for e in &t.entries {
        e.write(b)?;
    }
    b.end(m);
    let tb = &t.t;
    let m = b.start_full(b"stts", 0, 0);
    b.u32(tb.stts.len() as u32);
    for &(c, d) in &tb.stts {
        b.u32(c);
        b.u32(d);
    }
    b.end(m);
    if tb.ctts.iter().any(|&(_, o)| o != 0) {
        let neg = tb.ctts.iter().any(|&(_, o)| o < 0);
        let m = b.start_full(b"ctts", neg as u8, 0);
        b.u32(tb.ctts.len() as u32);
        for &(c, o) in &tb.ctts {
            b.u32(c);
            b.i32(o);
        }
        b.end(m);
    }
    if !tb.all_sync {
        let m = b.start_full(b"stss", 0, 0);
        b.u32(tb.sync.len() as u32);
        for &s in &tb.sync {
            b.u32(s);
        }
        b.end(m);
    }
    let m = b.start_full(b"stsc", 0, 0);
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    for (i, &(_, n, e)) in tb.chunks.iter().enumerate() {
        if runs.last().is_none_or(|r| r.1 != n || r.2 != e + 1) {
            runs.push((i as u32 + 1, n, e + 1));
        }
    }
    b.u32(runs.len() as u32);
    for (first, n, e) in runs {
        b.u32(first);
        b.u32(n);
        b.u32(e);
    }
    b.end(m);
    let m = b.start_full(b"stsz", 0, 0);
    let first = tb.sizes.first().copied().unwrap_or(0);
    if first != 0 && tb.sizes.iter().all(|&s| s == first) {
        b.u32(first);
        b.u32(tb.sizes.len() as u32);
    } else {
        b.u32(0);
        b.u32(tb.sizes.len() as u32);
        for &s in &tb.sizes {
            b.u32(s);
        }
    }
    b.end(m);
    let co64 = co64
        || tb
            .chunks
            .iter()
            .any(|&(o, _, _)| o + shift > u32::MAX as u64);
    let m = b.start_full(if co64 { b"co64" } else { b"stco" }, 0, 0);
    b.u32(tb.chunks.len() as u32);
    for &(o, _, _) in &tb.chunks {
        if co64 {
            b.u64(o + shift);
        } else {
            b.u32((o + shift) as u32);
        }
    }
    b.end(m);
    b.end(stbl);
    b.end(minf);
    b.end(mdia);
    b.end(trak);
    Ok(())
}

pub struct Writer<W: Write + Seek = File> {
    w: W,
    brand: Brand,
    reserve_pos: u64,
    reserve: u64,
    tracks: Vec<WTrack>,
    movie_timescale: u32,
    pad_pos: u64,
    mdat_pos: u64,
    pos: u64,
    buf: Vec<u8>,
    last_track: Option<usize>,
}

impl Writer<File> {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        Self::create_with(path, Options::default())
    }

    pub fn create_with(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let f = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.as_ref())
            .map_err(|e| Error::at(path.as_ref(), e))?;
        Self::new(f, options)
    }
}

impl<W: Write + Seek> Writer<W> {
    pub fn new(mut w: W, options: Options) -> Result<Self> {
        let start = w.stream_position()?;
        let mut b = BoxBuf::new();
        write_ftyp(&mut b, options.brand);
        let reserve_pos = start + b.len() as u64;
        let reserve = if options.reserve == 0 {
            0
        } else {
            options.reserve.max(8)
        };
        if reserve > 0 {
            b.u32(
                u32::try_from(reserve)
                    .map_err(|_| Error::Invalid("moov reserve too large".into()))?,
            );
            b.bytes(b"free");
            b.zeros(reserve as usize - 8);
        }
        let pad_pos = start + b.len() as u64;
        b.leaf(
            if options.brand == Brand::Mov {
                b"wide"
            } else {
                b"free"
            },
            &[],
        );
        let mdat_pos = start + b.len() as u64;
        b.leaf(b"mdat", &[]);
        w.write_all(&b.buf)?;
        let pos = start + b.len() as u64;
        Ok(Writer {
            w,
            brand: options.brand,
            reserve_pos,
            reserve,
            tracks: Vec::new(),
            movie_timescale: 1000,
            pad_pos,
            mdat_pos,
            pos,
            buf: Vec::with_capacity(FLUSH_AT + (1 << 20)),
            last_track: None,
        })
    }

    pub fn set_movie_timescale(&mut self, timescale: u32) {
        if timescale > 0 {
            self.movie_timescale = timescale;
        }
    }

    pub fn add_track(&mut self, cfg: TrackConfig) -> Result<usize> {
        let t = WTrack::new(cfg, self.tracks.len() as u32 + 1)?;
        self.tracks.push(t);
        Ok(self.tracks.len() - 1)
    }

    pub fn add_entry(&mut self, track: usize, entry: SampleEntry) -> Result<u32> {
        let t = self.track(track)?;
        t.entries.push(entry);
        Ok(t.entries.len() as u32 - 1)
    }

    pub fn use_entry(&mut self, track: usize, entry: u32) -> Result<()> {
        self.flush_pending(track, None)?;
        let t = self.track(track)?;
        if entry as usize >= t.entries.len() {
            return Err(Error::Invalid(format!(
                "no sample entry {entry} in track {track}"
            )));
        }
        t.entry = entry;
        Ok(())
    }

    fn track(&mut self, track: usize) -> Result<&mut WTrack> {
        self.tracks
            .get_mut(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))
    }

    pub fn push(&mut self, track: usize, packet: &Packet) -> Result<()> {
        self.flush_pending(track, Some(packet.dts))?;
        let t = self.track(track)?;
        let mut data = std::mem::take(&mut t.spare);
        data.clear();
        data.extend_from_slice(&packet.data);
        t.pending = Some(Pending {
            data,
            pts: packet.pts,
            dts: packet.dts,
            keyframe: packet.keyframe,
        });
        Ok(())
    }

    pub fn push_timed(&mut self, track: usize, packet: &Packet, duration: u32) -> Result<()> {
        self.flush_pending(track, Some(packet.dts))?;
        self.write_sample(
            track,
            &packet.data,
            packet.pts,
            packet.dts,
            duration,
            packet.keyframe,
        )
    }

    pub fn push_sample(
        &mut self,
        track: usize,
        data: &[u8],
        pts: i64,
        dts: i64,
        duration: u32,
        keyframe: bool,
    ) -> Result<()> {
        self.flush_pending(track, Some(dts))?;
        self.write_sample(track, data, pts, dts, duration, keyframe)
    }

    fn flush_pending(&mut self, track: usize, next_dts: Option<i64>) -> Result<()> {
        let t = self.track(track)?;
        let Some(p) = t.pending.take() else {
            return Ok(());
        };
        let duration = match next_dts {
            Some(d) => d
                .checked_sub(p.dts)
                .and_then(|d| u32::try_from(d).ok())
                .ok_or_else(|| Error::Invalid("packets must come in decode order".into()))?,
            None => t.last_duration,
        };
        self.write_sample(track, &p.data, p.pts, p.dts, duration, p.keyframe)?;
        self.tracks[track].spare = p.data;
        Ok(())
    }

    fn write_sample(
        &mut self,
        track: usize,
        data: &[u8],
        pts: i64,
        dts: i64,
        duration: u32,
        keyframe: bool,
    ) -> Result<()> {
        let size = u32::try_from(data.len())
            .map_err(|_| Error::Invalid("sample larger than 4 GiB".into()))?;
        let offset = self.pos;
        let contiguous = self.last_track == Some(track);
        self.last_track = Some(track);
        let t = self.track(track)?;
        t.record(size, pts, dts, duration, keyframe)?;
        let entry = t.entry;
        match t.t.chunks.last_mut() {
            Some(c) if contiguous && c.2 == entry => c.1 += 1,
            _ => t.t.chunks.push((offset, 1, entry)),
        }
        self.buf.extend_from_slice(data);
        self.pos += data.len() as u64;
        if self.buf.len() >= FLUSH_AT {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    fn finish_mdat(&mut self) -> Result<()> {
        for i in 0..self.tracks.len() {
            self.flush_pending(i, None)?;
        }
        for t in &mut self.tracks {
            t.t.reach_end();
        }
        self.w.write_all(&self.buf)?;
        self.buf = Vec::new();
        let size = self.pos - self.mdat_pos;
        if size <= u32::MAX as u64 {
            self.w.seek(SeekFrom::Start(self.mdat_pos))?;
            self.w.write_all(&(size as u32).to_be_bytes())?;
        } else {
            let mut h = [0u8; 16];
            h[..4].copy_from_slice(&1u32.to_be_bytes());
            h[4..8].copy_from_slice(b"mdat");
            h[8..].copy_from_slice(&(self.pos - self.pad_pos).to_be_bytes());
            self.w.seek(SeekFrom::Start(self.pad_pos))?;
            self.w.write_all(&h)?;
        }
        self.w.seek(SeekFrom::Start(self.pos))?;
        Ok(())
    }

    fn build_moov(&self, shift: u64, co64: bool) -> Result<Vec<u8>> {
        let ts = self.movie_timescale;
        let mut b = BoxBuf::new();
        let m = b.start(b"moov");
        let duration = self
            .tracks
            .iter()
            .map(|t| presentation_duration(t, ts))
            .max()
            .unwrap_or(0);
        write_mvhd(&mut b, ts, duration, self.tracks.len() as u32 + 1);
        for t in &self.tracks {
            write_trak(&mut b, t, ts, shift, co64, false, self.brand == Brand::Mov)?;
        }
        b.end(m);
        Ok(b.buf)
    }

    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    pub(crate) fn moov_len(&mut self) -> Result<u64> {
        self.finish_mdat()?;
        Ok(self.build_moov(0, false)?.len() as u64)
    }

    pub fn finish(mut self) -> Result<W> {
        self.finish_mdat()?;
        let moov = self.build_moov(0, false)?;
        let room = self.reserve;
        if room > 0 && (moov.len() as u64 == room || moov.len() as u64 + 8 <= room) {
            self.w.seek(SeekFrom::Start(self.reserve_pos))?;
            self.w.write_all(&moov)?;
            let rest = room - moov.len() as u64;
            if rest > 0 {
                self.w.write_all(&(rest as u32).to_be_bytes())?;
                self.w.write_all(b"free")?;
            }
            self.w.seek(SeekFrom::Start(self.pos))?;
        } else {
            self.w.write_all(&moov)?;
        }
        self.w.flush()?;
        Ok(self.w)
    }
}

impl<W: Read + Write + Seek> Writer<W> {
    pub fn finish_faststart(mut self) -> Result<W> {
        self.finish_mdat()?;
        let mut moov = self.build_moov(0, false)?;
        let mut co64 = false;
        loop {
            let next = self.build_moov(moov.len() as u64, co64)?;
            if next.len() == moov.len() {
                moov = next;
                break;
            }
            co64 = true;
            moov = next;
        }
        let shift = moov.len() as u64;
        let start = self.pad_pos;
        let end = self.pos;
        let mut buf = vec![0u8; 8 << 20];
        let mut cur = end;
        while cur > start {
            let n = (cur - start).min(buf.len() as u64) as usize;
            cur -= n as u64;
            self.w.seek(SeekFrom::Start(cur))?;
            self.w.read_exact(&mut buf[..n])?;
            self.w.seek(SeekFrom::Start(cur + shift))?;
            self.w.write_all(&buf[..n])?;
        }
        self.w.seek(SeekFrom::Start(start))?;
        self.w.write_all(&moov)?;
        self.w.seek(SeekFrom::Start(end + shift))?;
        self.w.flush()?;
        Ok(self.w)
    }
}

struct FragSample {
    data: Vec<u8>,
    duration: u32,
    offset: i32,
    keyframe: bool,
}

pub struct FragmentedWriter<W: Write = File> {
    w: W,
    tracks: Vec<WTrack>,
    pending: Vec<Vec<FragSample>>,
    next_dts: Vec<u64>,
    fragment: f64,
    seq: u32,
    init_written: bool,
}

impl FragmentedWriter<File> {
    pub fn create(path: impl AsRef<Path>, fragment_seconds: f64) -> Result<Self> {
        let path = path.as_ref();
        let f = File::create(path).map_err(|e| Error::at(path, e))?;
        Ok(Self::new(f, fragment_seconds))
    }
}

impl<W: Write> FragmentedWriter<W> {
    pub fn new(w: W, fragment_seconds: f64) -> Self {
        FragmentedWriter {
            w,
            tracks: Vec::new(),
            pending: Vec::new(),
            next_dts: Vec::new(),
            fragment: fragment_seconds.max(0.0),
            seq: 0,
            init_written: false,
        }
    }

    pub fn add_track(&mut self, cfg: TrackConfig) -> Result<usize> {
        if self.init_written {
            return Err(Error::Invalid(
                "tracks must be added before the first fragment".into(),
            ));
        }
        let t = WTrack::new(cfg, self.tracks.len() as u32 + 1)?;
        self.tracks.push(t);
        self.pending.push(Vec::new());
        self.next_dts.push(0);
        Ok(self.tracks.len() - 1)
    }

    fn write_init(&mut self) -> Result<()> {
        let ts = 1000;
        let mut b = BoxBuf::new();
        write_ftyp(&mut b, Brand::Mp4);
        let m = b.start(b"moov");
        write_mvhd(&mut b, ts, 0, self.tracks.len() as u32 + 1);
        for t in &self.tracks {
            write_trak(&mut b, t, ts, 0, false, true, false)?;
        }
        let mv = b.start(b"mvex");
        for t in &self.tracks {
            let x = b.start_full(b"trex", 0, 0);
            b.u32(t.id);
            b.u32(1);
            b.zeros(12);
            b.end(x);
        }
        b.end(mv);
        b.end(m);
        self.w.write_all(&b.buf)?;
        self.init_written = true;
        Ok(())
    }

    pub fn push(&mut self, track: usize, packet: &Packet) -> Result<()> {
        let t = self
            .tracks
            .get_mut(track)
            .ok_or_else(|| Error::Invalid(format!("no track {track}")))?;
        if let Some(p) = t.pending.take() {
            let duration = packet
                .dts
                .checked_sub(p.dts)
                .and_then(|d| u32::try_from(d).ok())
                .ok_or_else(|| Error::Invalid("packets must come in decode order".into()))?;
            self.queue(track, p, duration)?;
        }
        let t = &self.tracks[track];
        let pending_ticks: u64 = self.pending[track].iter().map(|s| s.duration as u64).sum();
        if packet.keyframe
            && t.handler == Handler::Video
            && pending_ticks as f64 >= self.fragment * t.cfg.timescale as f64
            && !self.pending[track].is_empty()
        {
            self.flush_fragment()?;
        }
        self.tracks[track].pending = Some(Pending {
            data: packet.data.clone(),
            pts: packet.pts,
            dts: packet.dts,
            keyframe: packet.keyframe,
        });
        Ok(())
    }

    fn queue(&mut self, track: usize, p: Pending, duration: u32) -> Result<()> {
        let t = &mut self.tracks[track];
        let offset = p
            .pts
            .checked_sub(p.dts)
            .and_then(|d| i32::try_from(d).ok())
            .ok_or_else(|| Error::Invalid("composition offset out of range".into()))?;
        t.last_duration = duration;
        self.pending[track].push(FragSample {
            data: p.data,
            duration,
            offset,
            keyframe: p.keyframe,
        });
        Ok(())
    }

    fn build_moof(&self, data_offsets: &[i32]) -> Vec<u8> {
        let mut b = BoxBuf::new();
        let m = b.start(b"moof");
        let h = b.start_full(b"mfhd", 0, 0);
        b.u32(self.seq);
        b.end(h);
        for (i, t) in self.tracks.iter().enumerate() {
            let p = &self.pending[i];
            if p.is_empty() {
                continue;
            }
            let tf = b.start(b"traf");
            let h = b.start_full(b"tfhd", 0, 0x20000);
            b.u32(t.id);
            b.end(h);
            let h = b.start_full(b"tfdt", 1, 0);
            b.u64(self.next_dts[i]);
            b.end(h);
            let neg = p.iter().any(|s| s.offset < 0);
            let h = b.start_full(b"trun", neg as u8, 0x1 | 0x100 | 0x200 | 0x400 | 0x800);
            b.u32(p.len() as u32);
            b.i32(data_offsets[i]);
            for s in p {
                b.u32(s.duration);
                b.u32(s.data.len() as u32);
                b.u32(if s.keyframe { 0x0200_0000 } else { 0x0101_0000 });
                b.i32(s.offset);
            }
            b.end(h);
            b.end(tf);
        }
        b.end(m);
        b.buf
    }

    pub fn flush_fragment(&mut self) -> Result<()> {
        if !self.init_written {
            self.write_init()?;
        }
        if self.pending.iter().all(Vec::is_empty) {
            return Ok(());
        }
        self.seq += 1;
        let probe = self.build_moof(&vec![0; self.tracks.len()]);
        let data_len: u64 = self
            .pending
            .iter()
            .flatten()
            .map(|s| s.data.len() as u64)
            .sum();
        let large = data_len + 8 > u32::MAX as u64;
        let mut offs = Vec::with_capacity(self.tracks.len());
        let mut acc = probe.len() as u64 + if large { 16 } else { 8 };
        for p in &self.pending {
            offs.push(i32::try_from(acc).map_err(|_| Error::Invalid("fragment too large".into()))?);
            acc += p.iter().map(|s| s.data.len() as u64).sum::<u64>();
        }
        let moof = self.build_moof(&offs);
        self.w.write_all(&moof)?;
        if large {
            self.w.write_all(&1u32.to_be_bytes())?;
            self.w.write_all(b"mdat")?;
            self.w.write_all(&(data_len + 16).to_be_bytes())?;
        } else {
            self.w.write_all(&((data_len + 8) as u32).to_be_bytes())?;
            self.w.write_all(b"mdat")?;
        }
        for (i, p) in self.pending.iter_mut().enumerate() {
            for s in p.drain(..) {
                self.w.write_all(&s.data)?;
                self.next_dts[i] += s.duration as u64;
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<W> {
        for i in 0..self.tracks.len() {
            if let Some(p) = self.tracks[i].pending.take() {
                let d = self.tracks[i].last_duration;
                self.queue(i, p, d)?;
            }
        }
        self.flush_fragment()?;
        self.w.flush()?;
        Ok(self.w)
    }
}

impl TrackConfig {
    pub fn from_track(track: &crate::mp4::Track) -> Result<Self> {
        let entry = track
            .entry()
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("track {} has no sample entry", track.id)))?;
        Ok(TrackConfig {
            entry,
            timescale: track.timescale,
            language: track.language.clone(),
            name: (!track.name.is_empty()).then(|| track.name.clone()),
            matrix: track.matrix,
            edits: Vec::new(),
            media_start: None,
        })
    }
}
