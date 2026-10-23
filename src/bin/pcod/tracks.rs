use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use pito_codecs::aac;
use pito_codecs::media::Packet;
use pito_codecs::mp4::{
    self, Brand, CodecConfig, Edit, Reader, Sample, Track, TrackConfig, TrackKind, Writer,
};

use super::Fail;
use super::convert::{Options, hash, land};
use super::manifest;
use super::pipeline::{Stopped, mkv_family};
use super::plan::{SAMPLE_RATE, partial, same_file};
use super::profile::Encoder;
use super::progress::Progress;
use super::signals;

pub const BED_KBPS: u32 = 160;
const MOVIE_TS: u32 = 1000;
const CHUNK_SECONDS: f64 = 0.5;
const FRAME_LEN: u32 = 1024;
const RATE: i32 = 0x10000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Add {
    pub wav: PathBuf,
    pub titles: Vec<String>,
}

pub fn brand_of(output: &Path) -> Result<Brand, String> {
    let extension = output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" => Ok(Brand::Mp4),
        "mov" => Ok(Brand::Mov),
        other => Err(format!(
            "--add-audio writes .mp4 or .mov, not .{other}: the WAV goes in as a second AAC track"
        )),
    }
}

pub fn titled(titles: &[String], tracks: usize) -> Result<Vec<Option<String>>, String> {
    if titles.is_empty() {
        return Ok(vec![None; tracks]);
    }
    if titles.len() != tracks {
        return Err(format!(
            "--title gives {} title{} for {tracks} audio track{}",
            titles.len(),
            if titles.len() == 1 { "" } else { "s" },
            if tracks == 1 { "" } else { "s" }
        ));
    }
    if titles.iter().any(|t| t.trim().is_empty()) {
        return Err("--title has an empty title".into());
    }
    Ok(titles.iter().map(|t| Some(t.clone())).collect())
}

pub fn ticks(seconds: f64, timescale: u32) -> i64 {
    (seconds * f64::from(timescale)).round().clamp(0.0, 9e15) as i64
}

pub fn kept(samples: &[Sample], offset: i64, limit: i64) -> usize {
    samples
        .iter()
        .rposition(|s| s.pts.saturating_add(offset) < limit)
        .map_or(0, |i| i + 1)
}

pub fn end_of(samples: &[Sample], offset: i64) -> i64 {
    samples
        .iter()
        .map(|s| s.pts.saturating_add(i64::from(s.duration)))
        .max()
        .unwrap_or(0)
        .saturating_add(offset)
}

fn movie(t: i64, timescale: u32) -> u64 {
    let t = u128::try_from(t.max(0)).unwrap_or(0);
    let ts = u128::from(timescale.max(1));
    u64::try_from((t * u128::from(MOVIE_TS) + ts / 2) / ts).unwrap_or(u64::MAX)
}

pub fn edits(offset: i64, end: i64, limit: i64, timescale: u32) -> Vec<Edit> {
    let lead = offset.max(0);
    let span = end.min(limit).saturating_sub(lead).max(0);
    let mut list = Vec::new();
    if lead > 0 {
        list.push(Edit {
            segment_duration: movie(lead, timescale),
            media_time: -1,
            media_rate: RATE,
        });
    }
    list.push(Edit {
        segment_duration: movie(span, timescale),
        media_time: offset.saturating_neg().max(0),
        media_rate: RATE,
    });
    list
}

enum From<'a> {
    Track(usize, &'a Track),
    Bed(&'a [Packet]),
}

struct Lane<'a> {
    out: usize,
    from: From<'a>,
    count: usize,
    next: usize,
    entry: u32,
}

impl Lane<'_> {
    fn at(&self) -> f64 {
        match &self.from {
            From::Track(_, t) => t.seconds(t.samples[self.next].dts),
            From::Bed(p) => p[self.next].dts as f64 / f64::from(SAMPLE_RATE),
        }
    }
}

fn copied(
    w: &mut Writer,
    track: &Track,
    title: Option<String>,
    limit: f64,
) -> Result<(usize, usize), Fail> {
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
    cfg.title = title;
    let out = w.add_track(cfg)?;
    for e in track.entries.iter().skip(1) {
        w.add_entry(out, e.clone())?;
    }
    Ok((out, count))
}

pub fn fits(brand: Brand, track: &Track) -> Result<(), String> {
    if brand != Brand::Mp4 {
        return Ok(());
    }
    let carried = |c: &CodecConfig| {
        matches!(
            c,
            CodecConfig::Avc(_)
                | CodecConfig::Hevc(_)
                | CodecConfig::Vp9(_)
                | CodecConfig::Av1(_)
                | CodecConfig::Aac(_)
                | CodecConfig::Mp3
        )
    };
    match track.entries.iter().find(|e| !carried(&e.codec)) {
        Some(e) => Err(format!(
            "--add-audio copies its {} track as it is, and .mp4 doesn't carry {}: write a .mov",
            if track.kind == TrackKind::Video {
                "video"
            } else {
                "audio"
            },
            e.codec.name()
        )),
        None => Ok(()),
    }
}

fn sound_seconds(frames: usize) -> f64 {
    frames as f64 / f64::from(SAMPLE_RATE)
}

pub fn run(
    options: &Options,
    add: &Add,
    partial_path: &mut Option<PathBuf>,
) -> Result<String, Fail> {
    let started = Instant::now();
    let request = &options.request;
    let brand = brand_of(&request.output)?;
    let input = &request.input;
    if same_file(input, &request.output) || same_file(&add.wav, &request.output) {
        return Err("input and output are the same file".into());
    }
    if !request.overwrite && request.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            request.output.display()
        )
        .into());
    }
    if mkv_family(input) {
        return Err(format!(
            "{}: --add-audio copies the tracks of an MP4 or MOV, not Matroska",
            input.display()
        )
        .into());
    }
    let at = |path: &Path, e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let reader = Reader::open(input).map_err(|e| at(input, &e))?;
    let video = reader
        .video()
        .ok_or_else(|| at(input, &"no video track to keep"))?;
    let vt = &reader.tracks[video];
    if vt.samples.is_empty() {
        return Err(at(input, &"the video track has no frames").into());
    }
    let first = reader.audio().first().copied();
    for t in std::iter::once(video).chain(first) {
        fits(brand, &reader.tracks[t]).map_err(|e| at(input, &e))?;
    }
    let titles = titled(&add.titles, usize::from(first.is_some()) + 1)?;
    let mut bed = super::frames::sound(&add.wav)?;
    if bed.frames() == 0 {
        return Err(at(&add.wav, &"the WAV holds no audio frames").into());
    }
    let ts = vt.timescale.max(1);
    let video_end = vt.seconds(end_of(&vt.samples, vt.edit_offset));
    let want = sound_seconds(bed.frames()).min(video_end);
    let vcount = kept(&vt.samples, vt.edit_offset, ticks(want, ts));
    if vcount == 0 {
        return Err(at(&add.wav, &"the WAV ends before the video's first frame").into());
    }
    let limit = want.min(vt.seconds(end_of(&vt.samples[..vcount], vt.edit_offset)));
    let frames = usize::try_from(ticks(limit, SAMPLE_RATE))
        .unwrap_or(usize::MAX)
        .min(bed.frames());
    bed.samples.truncate(frames * usize::from(bed.channels));
    let progress = Progress::new(options.progress);
    progress.step("encode", 1, 2, None);
    let kbps = request.audio_bitrate.unwrap_or(BED_KBPS);
    let mut enc = aac::Encoder::new(aac::EncoderConfig {
        rate: SAMPLE_RATE,
        channels: bed.channels,
        bitrate: kbps * 1000,
    })?;
    let mut packets = enc.encode(&bed)?;
    packets.extend(enc.flush());
    drop(bed);
    let part = partial(&request.output)?;
    *partial_path = Some(part.clone());
    let mut w = Writer::create_with(&part, mp4::Options { brand, reserve: 0 })?;
    w.set_movie_timescale(MOVIE_TS);
    let mut titles = titles.into_iter();
    let (vout, _) = copied(&mut w, vt, None, limit)?;
    let mut lanes = vec![Lane {
        out: vout,
        from: From::Track(video, vt),
        count: vcount,
        next: 0,
        entry: 0,
    }];
    if let Some(a) = first {
        let track = &reader.tracks[a];
        let (out, count) = copied(&mut w, track, titles.next().flatten(), limit)?;
        lanes.push(Lane {
            out,
            from: From::Track(a, track),
            count,
            next: 0,
            entry: 0,
        });
    }
    let priming = i64::from(enc.priming());
    let end = packets
        .last()
        .map_or(0, |p| p.pts.saturating_add(i64::from(FRAME_LEN)));
    let mut cfg = TrackConfig::new(
        mp4::SampleEntry::aac(
            enc.extradata(),
            u32::from(enc.config().channels),
            SAMPLE_RATE,
            kbps * 1000,
        ),
        SAMPLE_RATE,
    );
    cfg.edits = edits(
        -priming,
        end - priming,
        i64::try_from(frames).unwrap_or(i64::MAX),
        SAMPLE_RATE,
    );
    cfg.title = titles.next().flatten();
    let bout = w.add_track(cfg)?;
    lanes.push(Lane {
        out: bout,
        from: From::Bed(&packets),
        count: packets.len(),
        next: 0,
        entry: 0,
    });
    let total: usize = lanes.iter().map(|l| l.count).sum();
    progress.step("mux", 2, 2, Some(total as u64));
    let mut data = Vec::new();
    let mut done = 0u64;
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
            if signals::STOP.load(Ordering::Relaxed) {
                progress.clear();
                return Err(Box::new(Stopped));
            }
            match &lane.from {
                From::Track(index, t) => {
                    let s = t.samples[lane.next];
                    if s.entry != lane.entry {
                        w.use_entry(lane.out, s.entry)?;
                        lane.entry = s.entry;
                    }
                    reader
                        .read_into(*index, lane.next, &mut data)
                        .map_err(|e| at(input, &e))?;
                    w.push_sample(lane.out, &data, s.pts, s.dts, s.duration, s.keyframe)?;
                }
                From::Bed(p) => {
                    let pk = &p[lane.next];
                    w.push_sample(lane.out, &pk.data, pk.pts, pk.dts, FRAME_LEN, true)?;
                }
            }
            lane.next += 1;
            done += 1;
            progress.frame(done);
        }
    }
    w.finish_faststart()?;
    progress.clear();
    land(&part, &request.output, request.overwrite)?;
    *partial_path = None;
    let shorter = if sound_seconds(frames) < video_end - 1e-9 {
        "the WAV"
    } else {
        "the video"
    };
    let copied = if first.is_some() {
        "the video and the first audio track copied"
    } else {
        "the video copied"
    };
    let left = match reader.audio().len().saturating_sub(1) {
        0 => String::new(),
        1 => ", 1 more audio track left out".to_string(),
        n => format!(", {n} more audio tracks left out"),
    };
    let name = add
        .wav
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if !options.json {
        return Ok(format!(
            "{}\nnote: {copied}{left}, {name} encoded as AAC at {kbps} kbps, {limit:.3} s long ({shorter} is shorter)",
            request.output.display()
        ));
    }
    let codec = vt.codec().map_or_else(
        || "unknown".to_string(),
        |c| c.to_string().to_ascii_lowercase().replace('.', ""),
    );
    let seconds = started.elapsed().as_secs_f64();
    let document = manifest::Manifest {
        sources: vec![
            manifest::Source {
                path: input.display().to_string(),
                sha256: hash(input)?,
                loudness: None,
            },
            manifest::Source {
                path: add.wav.display().to_string(),
                sha256: hash(&add.wav)?,
                loudness: None,
            },
        ],
        params: manifest::Params {
            profile: "copy".into(),
            encoder: Encoder::Copy { codec }.manifest(),
            quality: None,
            width: vt.width,
            height: vt.height,
            fps: Some(vt.fps()),
            tune: None,
            loudness: None,
            audio: Some(manifest::Audio {
                encoder: "aac".into(),
                codec: None,
                kbps: Some(kbps),
                layout: Some("stereo".into()),
                sample_rate: Some(SAMPLE_RATE),
            }),
        },
        device: "CPU".into(),
        seconds: (seconds * 100.0).round() / 100.0,
        files: vec![manifest::Output {
            file: request.output.display().to_string(),
            sha256: hash(&request.output)?,
            bytes: std::fs::metadata(&request.output).map_or(0, |m| m.len()),
            loudness: None,
        }],
        join: None,
        cut: None,
    };
    Ok(document.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pts: i64, dts: i64, duration: u32) -> Sample {
        Sample {
            pts,
            dts,
            duration,
            size: 1,
            keyframe: true,
            ..Sample::default()
        }
    }

    #[test]
    fn titles_match_the_audio_tracks_or_are_refused() {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(titled(&[], 2).unwrap(), [None, None]);
        assert_eq!(
            titled(&words(&["Voice", "Music"]), 2).unwrap(),
            [Some("Voice".into()), Some("Music".into())]
        );
        assert_eq!(
            titled(&words(&["Voice"]), 2).unwrap_err(),
            "--title gives 1 title for 2 audio tracks"
        );
        assert_eq!(
            titled(&words(&["A", "B", "C"]), 1).unwrap_err(),
            "--title gives 3 titles for 1 audio track"
        );
        assert_eq!(
            titled(&words(&["Voice", " "]), 2).unwrap_err(),
            "--title has an empty title"
        );
    }

    #[test]
    fn only_mp4_and_mov_take_an_added_track() {
        assert_eq!(brand_of(Path::new("a.MP4")).unwrap(), Brand::Mp4);
        assert_eq!(brand_of(Path::new("a.mov")).unwrap(), Brand::Mov);
        for bad in ["a.webm", "a.mkv", "a"] {
            let e = brand_of(Path::new(bad)).unwrap_err();
            assert!(e.starts_with("--add-audio writes .mp4 or .mov"), "{e}");
            assert!(!e.contains('\n'));
        }
    }

    #[test]
    fn the_kept_prefix_holds_every_sample_shown_before_the_limit() {
        let plain: Vec<Sample> = (0..10).map(|k| sample(k * 10, k * 10, 10)).collect();
        assert_eq!(kept(&plain, 0, 45), 5);
        assert_eq!(kept(&plain, 0, 50), 5);
        assert_eq!(kept(&plain, 0, 51), 6);
        assert_eq!(kept(&plain, 0, 1000), 10);
        assert_eq!(kept(&plain, 0, 0), 0);
        assert_eq!(kept(&plain, -20, 45), 7);
        let reordered = [
            sample(10, 0, 10),
            sample(40, 10, 10),
            sample(20, 20, 10),
            sample(30, 30, 10),
        ];
        assert_eq!(kept(&reordered, -10, 25), 4);
        assert_eq!(kept(&reordered, -10, 15), 3);
        assert_eq!(kept(&reordered, -10, 5), 1);
        assert_eq!(end_of(&reordered, -10), 40);
        assert_eq!(end_of(&reordered[..1], -10), 10);
        assert_eq!(end_of(&[], 5), 5);
    }

    #[test]
    fn edits_skip_the_priming_and_stop_at_the_limit() {
        let e = edits(-2112, 48_000 + 2048, 24_000, 48_000);
        assert_eq!(
            e,
            [Edit {
                segment_duration: 500,
                media_time: 2112,
                media_rate: RATE
            }]
        );
        let whole = edits(0, 900, 10_000, 90_000);
        assert_eq!(whole[0].segment_duration, 10);
        assert_eq!(whole[0].media_time, 0);
        let late = edits(4800, 48_000, 96_000, 48_000);
        assert_eq!(late.len(), 2);
        assert_eq!(late[0].media_time, -1);
        assert_eq!(late[0].segment_duration, 100);
        assert_eq!(late[1].segment_duration, 900);
        assert_eq!(late[1].media_time, 0);
        assert_eq!(edits(0, 100, -5, 1000)[0].segment_duration, 0);
    }

    #[test]
    fn ticks_round_and_never_go_negative() {
        assert_eq!(ticks(1.5, 48_000), 72_000);
        assert_eq!(ticks(-1.0, 48_000), 0);
        assert_eq!(ticks(f64::INFINITY, 1000), 9_000_000_000_000_000);
        assert_eq!(movie(1001, 30_000), 33);
        assert_eq!(movie(-5, 30_000), 0);
    }

    fn tiny_input(dir: &Path) -> (PathBuf, PathBuf) {
        let mp4 = dir.join("in.mp4");
        let mut w = Writer::create_with(
            &mp4,
            mp4::Options {
                brand: Brand::Mp4,
                reserve: 0,
            },
        )
        .unwrap();
        let entry = mp4::SampleEntry::h264(
            mp4::AvcConfig::new(
                vec![vec![0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50]],
                vec![vec![0x68, 0xEB, 0xE3, 0xCB]],
            ),
            352,
            240,
        );
        let v = w.add_track(TrackConfig::new(entry, 24)).unwrap();
        for i in 0..24i64 {
            w.push_sample(v, &[0, 0, 0, 2, 0x65, i as u8], i, i, 1, i % 12 == 0)
                .unwrap();
        }
        w.finish().unwrap();
        let wav = dir.join("bed.wav");
        let pcm = vec![0u8; 48_000 * 4];
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        data.extend_from_slice(b"WAVEfmt ");
        for v in [
            16u32.to_le_bytes(),
            [1, 0, 2, 0],
            48_000u32.to_le_bytes(),
            192_000u32.to_le_bytes(),
            [4, 0, 16, 0],
        ] {
            data.extend_from_slice(&v);
        }
        data.extend_from_slice(b"data");
        data.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        data.extend_from_slice(&pcm);
        std::fs::write(&wav, data).unwrap();
        (mp4, wav)
    }

    #[test]
    fn a_stop_signal_while_muxing_leaves_no_output_and_names_the_partial() {
        if std::env::var_os("NEXTEST").is_none() {
            return;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/unit-tracks-stop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (input, wav) = tiny_input(&dir);
        let output = dir.join("out.mp4");
        let options = Options {
            request: super::super::plan::Request::new(&input, &output),
            json: false,
            progress: false,
            wav: None,
            add: None,
            range: None,
            still: None,
        };
        let add = Add {
            wav,
            titles: Vec::new(),
        };
        let mut part = None;
        assert!(run(&options, &add, &mut part).is_ok());
        assert!(output.exists());
        std::fs::remove_file(&output).unwrap();
        let mut part = None;
        signals::STOP.store(true, Ordering::Relaxed);
        let stopped = run(&options, &add, &mut part);
        signals::STOP.store(false, Ordering::Relaxed);
        assert!(stopped.unwrap_err().downcast_ref::<Stopped>().is_some());
        assert!(!output.exists());
        let part = part.unwrap();
        assert!(part.exists());
        std::fs::remove_file(&part).unwrap();
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
