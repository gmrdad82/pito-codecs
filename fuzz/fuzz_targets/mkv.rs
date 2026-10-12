#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::mkv::{ReadOptions, Reader};

fuzz_target!(|data: &[u8]| {
    for index in [false, true] {
        let Ok(r) = Reader::with_options(data.to_vec(), ReadOptions { index }) else {
            continue;
        };
        let _ = r.duration_seconds();
        let _ = r.video();
        let _ = r.audio();
        let _ = r.is_webm();
        let mut buf = Vec::new();
        for (ti, t) in r.tracks.iter().enumerate() {
            let _ = t.codec();
            let _ = t.extradata();
            let _ = t.rotation();
            let _ = t.channels();
            let _ = t.sample_rate();
            let _ = t.duration_seconds();
            let _ = t.fps();
            let _ = t.stripped_header();
            let _ = t.frames_readable();
            let _ = t.keyframes();
            if let Some(v) = &t.video {
                let _ = v.rotation();
                let _ = v.pixel_aspect();
            }
            for i in 0..t.samples.len().min(64) {
                let _ = r.read_into(ti, i, &mut buf);
                let _ = r.packet(ti, i);
                let _ = t.keyframe_before(i);
            }
            for s in [0.0, 0.5, 7.25, -1.0] {
                let ticks = t.ticks(s);
                let _ = t.sample_at(ticks);
                let _ = t.keyframe_at(ticks);
            }
        }
        let mut s = r.stream();
        let mut n = 0;
        while let Ok(Some(_)) = s.next_into(&mut buf) {
            n += 1;
            if n > 256 {
                break;
            }
        }
        for ti in 0..r.tracks.len().min(4) {
            for at in [0.0, 0.25, 7.0] {
                let _ = s.seek(ti, at);
                let mut k = 0;
                while let Ok(Some(_)) = s.next_into(&mut buf) {
                    k += 1;
                    if k > 16 {
                        break;
                    }
                }
            }
        }
        s.rewind();
        let _ = s.next_packet();
    }
});
