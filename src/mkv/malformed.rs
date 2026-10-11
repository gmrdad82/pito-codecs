use super::*;
use crate::media::TimeBase;
use std::io::Cursor;

const MUTATIONS: usize = 2000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn good(webm: bool) -> Vec<u8> {
    let mut w = Writer::new(
        Cursor::new(Vec::new()),
        Options {
            webm,
            cluster_seconds: 0.3,
        },
    );
    let (video, audio) = if webm {
        (
            TrackConfig::vp9(64, 48, TimeBase::new(1, 1000)),
            TrackConfig::opus(vec![
                b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 2, 0x38, 1, 0x80, 0xBB, 0, 0, 0,
                0, 0,
            ]),
        )
    } else {
        let avcc = vec![
            1, 0x64, 0, 0x1F, 0xFF, 0xE1, 0, 4, 0x67, 0x64, 0, 0x1F, 1, 0, 2, 0x68, 0xEB,
        ];
        let mut v = TrackConfig::h264(avcc, 64, 48, TimeBase::new(1, 90_000));
        v.rotation = 90;
        (v, TrackConfig::aac(vec![0x12, 0x10], 48_000, 2))
    };
    let v = w.add_track(video).unwrap();
    let a = w.add_track(audio).unwrap();
    let (vt, at) = if webm { (40i64, 960i64) } else { (3600, 1024) };
    for i in 0..24i64 {
        let data = [0x65, i as u8, 7, 9];
        if i % 3 == 0 {
            w.push_sample(v, &data, i * vt, Some(vt as u32), i % 8 == 0)
                .unwrap();
        } else {
            w.push_sample(v, &data, i * vt, None, i % 8 == 0).unwrap();
        }
        w.push_sample(a, &[0x21, i as u8, 0x5A], i * at, None, true)
            .unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn exercise(data: &[u8]) {
    for index in [true, false] {
        let Ok(r) = Reader::with_options(data, ReadOptions { index }) else {
            continue;
        };
        let _ = (r.duration_seconds(), r.video(), r.audio());
        for (ti, t) in r.tracks.iter().enumerate() {
            let _ = (t.codec(), t.extradata(), t.rotation(), t.channels());
            let _ = (t.sample_rate(), t.duration_seconds(), t.fps(), t.end());
            let _ = (
                t.sample_at(0),
                t.keyframe_at(i64::MAX),
                t.keyframe_before(usize::MAX),
            );
            let _ = t.video.as_ref().map(|v| v.pixel_aspect());
            for i in 0..t.samples.len() {
                let _ = r.packet(ti, i);
            }
        }
        let mut s = r.stream();
        let mut n = 0;
        while let Ok(Some(_)) = s.next_packet() {
            n += 1;
            assert!(n <= data.len(), "stream does not end");
        }
        for ti in 0..r.tracks.len() {
            for at in [0.0, 0.25, 1e9] {
                if s.seek(ti, at).is_ok() {
                    let _ = s.next_packet();
                }
            }
        }
        for snap in [Snap::Outward, Snap::Inward] {
            if let Ok(cut) = plan(&r, 0.1, 0.6, snap) {
                let mut w = Writer::new(Cursor::new(Vec::new()), Options::default());
                if copy(&r, &cut, &mut w).is_ok() {
                    let _ = w.finish();
                }
            }
        }
    }
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut d = base.to_vec();
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(d.len());
        match rng.below(4) {
            0 => d[at] ^= 1 << rng.below(8),
            1 => d[at] = rng.next() as u8,
            2 => d[at] = [0x00, 0x01, 0x08, 0x10, 0x40, 0x7F, 0x80, 0xFF][rng.below(8)],
            _ => {
                let v: u32 = match rng.below(4) {
                    0 => 0x0100_0000,
                    1 => u32::MAX,
                    2 => 0x1F43_B675,
                    _ => rng.next() as u32,
                };
                let end = (at + 4).min(d.len());
                d[at..end].copy_from_slice(&v.to_be_bytes()[..end - at]);
            }
        }
    }
    d
}

#[test]
fn malformed_files_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for webm in [false, true] {
        let base = good(webm);
        let r = Reader::new(base.as_slice()).unwrap();
        assert_eq!(r.tracks.len(), 2);
        assert_eq!(r.tracks[0].samples.len(), 24);
        for cut in (0..base.len()).filter(|&c| c < 400 || c % 7 == 0) {
            exercise(&base[..cut]);
        }
        for _ in 0..MUTATIONS {
            exercise(&mutate(&mut rng, &base));
        }
    }
}
