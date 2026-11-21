use std::path::Path;

use pito_codecs::media::Codec;
use pito_codecs::mkv;
use pito_codecs::mp4::{self, Brand, CodecConfig, SampleEntry, TrackConfig, Writer};

use super::Fail;
use super::tracks::{edits, end_of, kept, ticks};

const MOVIE_TS: u32 = 1000;
const CHUNK_SECONDS: f64 = 0.5;
const FRAME_LEN: u32 = 1024;

pub enum Source {
    Mp4(Box<mp4::Reader>, usize),
    Mkv(Box<mkv::Reader>, usize),
}

pub struct Taken {
    pub codec: String,
    pub seconds: f64,
}

fn carried(c: &CodecConfig) -> bool {
    matches!(c, CodecConfig::Aac(_) | CodecConfig::Mp3)
}

fn refused(codec: &str) -> String {
    format!(
        "--audio-copy copies {codec} as it is, and .mp4 doesn't carry {codec}: use --audio with a WAV"
    )
}

pub fn open(path: &Path) -> Result<(Source, Taken), Fail> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if super::pipeline::mkv_family(path) {
        let reader = mkv::Reader::open(path).map_err(|e| at(&e))?;
        let index = *reader
            .audio()
            .first()
            .ok_or_else(|| at(&"no audio track to copy"))?;
        let track = &reader.tracks[index];
        let name = track.codec_name().to_string();
        if track.codec() != Some(Codec::Aac) {
            let why = if track.codec() == Some(Codec::Mp3) {
                "--audio-copy takes AAC from Matroska, not mp3".to_string()
            } else {
                refused(&name)
            };
            return Err(at(&why).into());
        }
        let seconds = track.duration_seconds();
        return Ok((
            Source::Mkv(Box::new(reader), index),
            Taken {
                codec: name,
                seconds,
            },
        ));
    }
    let reader = mp4::Reader::open(path).map_err(|e| at(&e))?;
    let index = *reader
        .audio()
        .first()
        .ok_or_else(|| at(&"no audio track to copy"))?;
    let track = &reader.tracks[index];
    if let Some(e) = track.entries.iter().find(|e| !carried(&e.codec)) {
        return Err(at(&refused(&e.codec.name())).into());
    }
    if track.samples.is_empty() {
        return Err(at(&"the audio track has no packets").into());
    }
    let seconds = track.seconds(end_of(&track.samples, track.edit_offset));
    let codec = track
        .entry()
        .map_or_else(|| "aac".to_string(), |e| e.codec.name());
    Ok((
        Source::Mp4(Box::new(reader), index),
        Taken { codec, seconds },
    ))
}

struct Lane<'a> {
    out: usize,
    count: usize,
    next: usize,
    entry: u32,
    from: From<'a>,
}

enum From<'a> {
    Mp4(&'a mp4::Reader, usize),
    Mkv(&'a mkv::Reader, usize, Vec<(i64, u32)>),
}

impl Lane<'_> {
    fn at(&self) -> f64 {
        match &self.from {
            From::Mp4(r, t) => {
                let track = &r.tracks[*t];
                track.seconds(track.samples[self.next].dts)
            }
            From::Mkv(r, t, times) => {
                times[self.next].0 as f64 / f64::from(r.tracks[*t].sample_rate().max(1))
            }
        }
    }
}

fn copied(w: &mut Writer, track: &mp4::Track, limit: f64) -> Result<(usize, usize), Fail> {
    let ts = track.timescale.max(1);
    let offset = track.edit_offset;
    let count = kept(&track.samples, offset, ticks(limit, ts));
    let mut cfg = TrackConfig::from_track(track)?;
    cfg.edits = edits(
        offset,
        end_of(&track.samples[..count], offset),
        ticks(limit, ts),
        ts,
    );
    cfg.media_start = None;
    let out = w.add_track(cfg)?;
    for e in track.entries.iter().skip(1) {
        w.add_entry(out, e.clone())?;
    }
    Ok((out, count))
}

pub struct Joined {
    pub video_seconds: f64,
    pub audio_seconds: f64,
}

pub fn join(silent: &Path, source: &Source, part: &Path) -> Result<Joined, Fail> {
    let at = |p: &Path, e: &dyn std::fmt::Display| format!("{}: {e}", p.display());
    let video = mp4::Reader::open(silent).map_err(|e| at(silent, &e))?;
    let vi = video.video().ok_or_else(|| at(silent, &"no video track"))?;
    let vt = &video.tracks[vi];
    let video_end = vt.seconds(end_of(&vt.samples, vt.edit_offset));
    let mut w = Writer::create_with(
        part,
        mp4::Options {
            brand: Brand::Mp4,
            reserve: 0,
        },
    )?;
    w.set_movie_timescale(MOVIE_TS);
    let (vout, vcount) = copied(&mut w, vt, video_end)?;
    let mut lanes = vec![Lane {
        out: vout,
        count: vcount,
        next: 0,
        entry: 0,
        from: From::Mp4(&video, vi),
    }];
    let audio_end = match source {
        Source::Mp4(reader, index) => {
            let track = &reader.tracks[*index];
            let (out, count) = copied(&mut w, track, video_end)?;
            let end = track
                .seconds(end_of(&track.samples[..count], track.edit_offset))
                .min(video_end);
            lanes.push(Lane {
                out,
                count,
                next: 0,
                entry: 0,
                from: From::Mp4(reader, *index),
            });
            end
        }
        Source::Mkv(reader, index) => {
            let track = &reader.tracks[*index];
            let rate = track.sample_rate().max(1);
            let pts: Vec<i64> = track
                .samples
                .iter()
                .map(|s| ticks(track.seconds(s.pts), rate))
                .collect();
            let count = pts
                .iter()
                .rposition(|&p| p < ticks(video_end, rate))
                .map_or(0, |i| i + 1);
            let times: Vec<(i64, u32)> = (0..count)
                .map(|k| {
                    let d = pts
                        .get(k + 1)
                        .map_or(FRAME_LEN as i64, |n| n - pts[k])
                        .clamp(1, i64::from(u32::MAX));
                    (pts[k], d as u32)
                })
                .collect();
            let asc = track
                .extradata()
                .ok_or_else(|| at(Path::new("audio"), &"the AAC track has no config"))?;
            let mut cfg = TrackConfig::new(SampleEntry::aac(asc, track.channels(), rate, 0), rate);
            let end = times.last().map_or(0, |t| t.0 + i64::from(t.1));
            cfg.edits = edits(0, end, ticks(video_end, rate), rate);
            let out = w.add_track(cfg)?;
            let seconds = (end as f64 / f64::from(rate)).min(video_end);
            lanes.push(Lane {
                out,
                count,
                next: 0,
                entry: 0,
                from: From::Mkv(reader, *index, times),
            });
            seconds
        }
    };
    let mut data = Vec::new();
    while let Some(l) = lanes
        .iter()
        .enumerate()
        .filter(|(_, l)| l.next < l.count)
        .min_by(|a, b| a.1.at().total_cmp(&b.1.at()))
        .map(|(i, _)| i)
    {
        let lane = &mut lanes[l];
        let until = lane.at() + CHUNK_SECONDS;
        while lane.next < lane.count && lane.at() < until {
            match &lane.from {
                From::Mp4(r, t) => {
                    let s = r.tracks[*t].samples[lane.next];
                    if s.entry != lane.entry {
                        w.use_entry(lane.out, s.entry)?;
                        lane.entry = s.entry;
                    }
                    r.read_into(*t, lane.next, &mut data)
                        .map_err(|e| format!("{e}"))?;
                    w.push_sample(lane.out, &data, s.pts, s.dts, s.duration, s.keyframe)?;
                }
                From::Mkv(r, t, times) => {
                    let (pts, duration) = times[lane.next];
                    r.read_into(*t, lane.next, &mut data)
                        .map_err(|e| format!("{e}"))?;
                    w.push_sample(lane.out, &data, pts, pts, duration, true)?;
                }
            }
            lane.next += 1;
        }
    }
    w.finish_faststart()?;
    Ok(Joined {
        video_seconds: video_end,
        audio_seconds: audio_end,
    })
}
