use std::path::PathBuf;

use crate::h264::{Encoder, EncoderConfig};
use crate::mp4::{AvcConfig, SampleEntry, TrackConfig, Writer};

use super::types::{Frame, PixelFormat, TimeBase};
use super::route::Choice;
use super::{Prefs, audio_pcm, decode_video, first_frame, keyframes, probe};

fn dir() -> PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test/media-malformed");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn good_mp4() -> Vec<u8> {
    let (w, h) = (64, 48);
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        fps: (10, 1),
        gop: 4,
        b_frames: 0,
        threads: 1,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut packets = Vec::new();
    for i in 0..8 {
        let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i, TimeBase::new(1, 10));
        for (k, v) in f.planes[0].data.iter_mut().enumerate() {
            *v = (k as i64 * 3 + i * 17) as u8;
        }
        packets.extend(enc.encode(&f).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    let path = dir().join("good.mp4");
    let mut out = Writer::create(&path).unwrap();
    let entry = SampleEntry::h264(AvcConfig::parse(&enc.avcc()).unwrap(), w as u16, h as u16);
    let track = out.add_track(TrackConfig::new(entry, 10)).unwrap();
    for p in &packets {
        out.push(track, p).unwrap();
    }
    out.finish().unwrap();
    std::fs::read(&path).unwrap()
}

#[test]
fn damaged_files_are_errors_not_crashes() {
    let good = good_mp4();
    let prefs = Prefs {
        decode: Choice::Cpu,
        encode: Choice::Cpu,
        ..Prefs::default()
    };
    assert!(probe(&dir().join("good.mp4"), &prefs).is_ok());
    let mut seed = 0x2545_F491u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as usize
    };
    let mut files: Vec<Vec<u8>> = vec![Vec::new(), b"garbage, not a container".to_vec()];
    files.extend((0..good.len()).step_by(good.len() / 40 + 1).map(|n| good[..n].to_vec()));
    for _ in 0..40 {
        let mut bad = good.clone();
        for _ in 0..4 {
            let at = bad.len() - 1 - next() % bad.len().min(600);
            bad[at] = next() as u8;
        }
        files.push(bad);
    }
    for (i, data) in files.iter().enumerate() {
        let path = dir().join(format!("bad-{i}.mp4"));
        std::fs::write(&path, data).unwrap();
        let _ = probe(&path, &prefs);
        let _ = keyframes(&path, &prefs);
        let _ = first_frame(&path, 32, &prefs);
        let _ = audio_pcm(&path, 0, 48_000, true, &prefs);
        if let Ok(v) = decode_video(&path, 0.0, None, &prefs) {
            for f in v.take(16) {
                if f.is_err() {
                    break;
                }
            }
        }
    }
    let empty = dir().join("bad-0.mp4");
    assert!(probe(&empty, &prefs).is_err());
    assert!(first_frame(&empty, 32, &prefs).is_err());
}
