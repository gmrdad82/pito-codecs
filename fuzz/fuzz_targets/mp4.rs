#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::mp4::Reader;

fuzz_target!(|data: &[u8]| {
    let Ok(r) = Reader::new(data.to_vec()) else {
        return;
    };
    let _ = r.duration_seconds();
    let _ = r.video();
    let _ = r.audio();
    let mut buf = Vec::new();
    for (ti, t) in r.tracks.iter().enumerate() {
        let _ = t.codec();
        let _ = t.extradata();
        let _ = t.rotation();
        let _ = t.channels();
        let _ = t.sample_rate();
        let _ = t.duration_seconds();
        let _ = t.bitrate();
        let _ = t.fps();
        let _ = t.keyframes();
        for i in 0..t.samples.len().min(64) {
            let _ = r.read_into(ti, i, &mut buf);
            let _ = r.packet(ti, i);
            let _ = t.presentation(i);
            let _ = t.keyframe_before(i);
        }
        for s in [0.0, 0.5, 7.25, -1.0, 1e12] {
            if let Some(i) = t.sample_at(t.ticks(s)) {
                let _ = t.keyframe_before(i);
            }
        }
    }
});
