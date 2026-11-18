#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::loudness::LoudnessMeter;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let channels = u16::from(data[0] % 9);
    let rate = u32::from(u16::from_le_bytes([data[1], data[2]])) * 4;
    let Ok(meter) = LoudnessMeter::new(rate, channels) else {
        return;
    };
    let Ok(mut meter) = meter.windows() else {
        return;
    };
    let samples: Vec<f32> = data[3..]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let _ = meter.add(&samples);
    let _ = meter.integrated();
    let _ = meter.momentary_now();
    let _ = meter.short_term_now();
    let _ = meter.window(400);
    let _ = meter.range();
    let _ = meter.summary();
});
