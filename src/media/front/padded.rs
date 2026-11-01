use std::path::{Path, PathBuf};

use crate::aac::{Encoder, EncoderConfig};
use crate::media::{AudioBuffer, Packet};
use crate::mp4::{Edit, SampleEntry, TrackConfig, Writer};

use super::{AudioStream, Padding, Prefs, audio_mix_stream, audio_stream};

const RATE: u32 = 48_000;

fn dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/media-padded");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn packets(seconds: f64) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut enc = Encoder::new(EncoderConfig {
        rate: RATE,
        channels: 2,
        bitrate: 128_000,
    })
    .unwrap();
    let n = (seconds * RATE as f64) as usize;
    let mut buf = AudioBuffer::new(RATE, 2);
    buf.samples = (0..n)
        .flat_map(|k| {
            let v = 0.3 * (k as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin();
            [v, v * 0.5]
        })
        .collect();
    let mut out: Vec<Vec<u8>> = enc
        .encode(&buf)
        .unwrap()
        .into_iter()
        .map(|p| p.data)
        .collect();
    out.extend(enc.flush().into_iter().map(|p| p.data));
    (enc.extradata(), out)
}

fn mp4(name: &str, late: u64, last: u32) -> PathBuf {
    let path = dir().join(name);
    let (asc, aus) = packets(1.0);
    let n = aus.len();
    let mut w = Writer::create(&path).unwrap();
    w.set_movie_timescale(RATE);
    for late in [0, late] {
        let mut cfg = TrackConfig::new(SampleEntry::aac(asc.clone(), 2, RATE, 128_000), RATE);
        if late > 0 {
            let media = (n as u64 - 1) * 1024 + u64::from(last);
            cfg.edits = vec![edit(late, -1), edit(media, 0)];
        }
        let t = w.add_track(cfg).unwrap();
        for (k, au) in aus.iter().enumerate() {
            let at = k as i64 * 1024;
            let d = if k + 1 == n { last } else { 1024 };
            w.push_sample(t, au, at, at, d, true).unwrap();
        }
    }
    w.finish().unwrap();
    path
}

fn mkv(name: &str, first: i64, last: u32) -> PathBuf {
    let path = dir().join(name);
    let (asc, aus) = packets(1.0);
    let n = aus.len();
    let mut w = crate::mkv::Writer::create(&path).unwrap();
    let tracks = [0, first].map(|_| {
        w.add_track(crate::mkv::TrackConfig::aac(asc.clone(), RATE, 2))
            .unwrap()
    });
    for (k, au) in aus.iter().enumerate() {
        for (t, first) in tracks.into_iter().zip([0, first]) {
            let p = Packet {
                data: au.clone(),
                pts: first + k as i64 * 1024,
                dts: first + k as i64 * 1024,
                keyframe: true,
            };
            let d = if k + 1 == n && t == tracks[1] {
                last
            } else {
                1024
            };
            w.push_timed(t, &p, d).unwrap();
        }
    }
    w.finish().unwrap();
    path
}

#[cfg(feature = "opus")]
fn opus_mkv(name: &str, first: i64, last: u32) -> PathBuf {
    use crate::opus::enc::{Encoder, EncoderConfig};
    let path = dir().join(name);
    let mut enc = Encoder::new(EncoderConfig::new(2, 96_000)).unwrap();
    let mut buf = AudioBuffer::new(RATE, 2);
    buf.samples = (0..RATE as usize)
        .flat_map(|k| {
            let v = 0.3 * (k as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin();
            [v, v * 0.5]
        })
        .collect();
    let mut aus: Vec<Vec<u8>> = enc
        .encode(&buf)
        .unwrap()
        .into_iter()
        .map(|p| p.data)
        .collect();
    aus.extend(enc.flush().unwrap().into_iter().map(|p| p.data));
    let step = enc.frame_samples() as i64;
    let n = aus.len();
    let mut w = crate::mkv::Writer::create(&path).unwrap();
    let tracks = [0, first].map(|_| {
        w.add_track(crate::mkv::TrackConfig::opus(enc.codec_private()))
            .unwrap()
    });
    for (k, au) in aus.iter().enumerate() {
        for (t, first) in tracks.into_iter().zip([0, first]) {
            let p = Packet {
                data: au.clone(),
                pts: first + k as i64 * step,
                dts: first + k as i64 * step,
                keyframe: true,
            };
            let d = if k + 1 == n && t == tracks[1] {
                last
            } else {
                step as u32
            };
            w.push_timed(t, &p, d).unwrap();
        }
    }
    w.finish().unwrap();
    path
}

fn edit(segment_duration: u64, media_time: i64) -> Edit {
    Edit {
        segment_duration,
        media_time,
        media_rate: 1 << 16,
    }
}

fn exact() -> Prefs {
    Prefs {
        pad_audio: false,
        ..Prefs::default()
    }
}

fn drain(s: AudioStream) -> Vec<f32> {
    let blocks: Vec<AudioBuffer> = s.collect::<crate::media::Result<_>>().unwrap();
    blocks.into_iter().flat_map(|b| b.samples).collect()
}

fn agrees(path: &Path, from: f64, rate: u32) -> Padding {
    let on = audio_stream(path, 1, from, rate, 2, &Prefs::default()).unwrap();
    let off = audio_stream(path, 1, from, rate, 2, &exact()).unwrap();
    let p = on.padded().unwrap();
    assert_eq!(off.padded().unwrap(), p);
    assert_eq!(p.rate, rate);
    let (on, off) = (drain(on), drain(off));
    let (lead, tail) = (p.lead_frames as usize * 2, p.tail_frames as usize * 2);
    assert_eq!(on.len(), off.len() + lead + tail, "{p:?}");
    if rate == RATE {
        assert!(on[..lead].iter().all(|&s| s == 0.0));
        assert!(on[on.len() - tail..].iter().all(|&s| s == 0.0));
        assert_eq!(&on[lead..on.len() - tail], &off[..]);
    }
    p
}

#[test]
fn a_plain_track_pads_nothing() {
    let path = mp4("plain.mp4", 0, 1024);
    for from in [0.0, 0.3] {
        assert!(agrees(&path, from, RATE).is_none());
        assert!(agrees(&path, from, 44_100).is_none());
    }
}

#[test]
fn a_track_that_starts_late_pads_the_lead_and_reports_it() {
    let path = mp4("lead.mp4", 24_000, 1024);
    let p = agrees(&path, 0.0, RATE);
    assert_eq!((p.lead_frames, p.tail_frames), (24_000, 0));
    assert_eq!(agrees(&path, 0.25, RATE).lead_frames, 12_000);
    assert_eq!(agrees(&path, 0.75, RATE).lead_frames, 0);
    assert_eq!(p.lead_seconds(), 0.5);
    assert!(agrees(&path, 0.0, 44_100).lead_frames.abs_diff(22_050) <= 1);
}

#[test]
fn a_packet_declared_longer_than_it_decodes_pads_the_tail_and_reports_it() {
    let path = mp4("tail.mp4", 0, 1024 + RATE);
    let p = agrees(&path, 0.0, RATE);
    assert_eq!((p.lead_frames, p.tail_frames), (0, RATE as u64));
    assert_eq!(p.tail_seconds(), 1.0);
    assert_eq!(agrees(&path, 0.5, RATE).tail_frames, RATE as u64);
    agrees(&path, 0.0, 44_100);
}

#[test]
fn a_late_mkv_track_with_a_long_last_block_pads_and_reports() {
    let path = mkv("late.mkv", 24_000, 1024 + 12_000);
    let p = agrees(&path, 0.0, RATE);
    assert_eq!((p.lead_frames, p.tail_frames), (24_000, 12_000));
    assert_eq!(agrees(&path, 0.2, RATE).lead_frames, 14_400);
    agrees(&path, 0.0, 44_100);
}

#[test]
fn a_track_declaring_hours_it_does_not_hold_streams_without_one_huge_buffer() {
    let hours = RATE as u64 * 36_000;
    let path = mp4("hours.mp4", hours, 1024 + hours as u32);
    let off = audio_stream(&path, 1, 0.0, RATE, 2, &exact()).unwrap();
    let p = off.padded().unwrap();
    assert_eq!((p.lead_frames, p.tail_frames), (hours, hours));
    assert!(drain(off).len() < RATE as usize * 4);
    let mut on = audio_stream(&path, 1, 0.0, RATE, 2, &Prefs::default()).unwrap();
    for _ in 0..3 {
        let b = on.next().unwrap().unwrap();
        assert_eq!(b.frames(), RATE as usize);
        assert!(b.samples.iter().all(|&s| s == 0.0));
    }
}

#[test]
fn a_tail_of_an_hour_comes_out_a_block_at_a_time() {
    let hour = RATE as u64 * 3_600;
    let path = mp4("tail-hour.mp4", 0, 1024 + hour as u32);
    let mut on = audio_stream(&path, 1, 0.0, RATE, 2, &Prefs::default()).unwrap();
    assert_eq!(on.padded().unwrap().tail_frames, hour);
    let mut silent = 0;
    for _ in 0..8 {
        let b = on.next().unwrap().unwrap();
        assert_eq!(b.frames(), RATE as usize);
        if b.samples.iter().all(|&s| s == 0.0) {
            silent += 1;
        }
    }
    assert!(silent >= 6, "{silent}");
}

#[test]
fn a_mix_reports_its_ends_and_trims_them_when_asked() {
    let path = mp4("mix.mp4", 24_000, 1024 + RATE);
    let on = audio_mix_stream(&path, &[0.0, 1.0], 0.0, RATE, &Prefs::default()).unwrap();
    let off = audio_mix_stream(&path, &[0.0, 1.0], 0.0, RATE, &exact()).unwrap();
    let p = on.padded().unwrap();
    assert_eq!((p.lead_frames, p.tail_frames), (24_000, RATE as u64));
    assert_eq!(off.padded().unwrap(), p);
    let (on, off) = (drain(on), drain(off));
    let (lead, tail) = (p.lead_frames as usize * 2, p.tail_frames as usize * 2);
    assert_eq!(on.len(), off.len() + lead + tail);
    assert_eq!(&on[lead..on.len() - tail], &off[..]);
}

#[cfg(feature = "opus")]
#[test]
fn a_late_opus_track_counts_its_packets_and_reports() {
    let path = opus_mkv("late-opus.mkv", 24_000, 960 + 12_000);
    let p = agrees(&path, 0.0, RATE);
    assert!(p.lead_frames.abs_diff(24_000) <= 48, "{p:?}");
    assert_eq!(p.lead_frames + p.tail_frames, 36_000);
    let q = agrees(&path, 0.3, RATE);
    assert_eq!(q.lead_frames, p.lead_frames - 14_400);
    assert_eq!(q.tail_frames, p.tail_frames);
    agrees(&path, 0.0, 44_100);
}
