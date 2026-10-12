#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::aac::{Decoder, split_adts};

fuzz_target!(|data: &[u8]| {
    match split_adts(data) {
        Ok(frames) => {
            let Some((head, _)) = frames.first() else {
                return;
            };
            let Ok(mut dec) = Decoder::from_adts(head) else {
                return;
            };
            for (_, frame) in frames.iter().take(64) {
                let _ = dec.decode_frame(frame);
            }
        }
        Err(_) => {
            let Some((&n, rest)) = data.split_first() else {
                return;
            };
            let n = (n as usize % 16).min(rest.len());
            let (asc, au) = rest.split_at(n);
            let Ok(mut dec) = Decoder::new(asc) else {
                return;
            };
            let _ = dec.decode_frame(au);
            let _ = dec.decode_frame(au);
            dec.reset();
            let _ = dec.decode_frame(au);
        }
    }
});
