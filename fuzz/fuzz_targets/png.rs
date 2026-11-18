#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::png::{self, Decoder, Transform};

fuzz_target!(|data: &[u8]| {
    let _ = png::info(data);
    let _ = png::decode(data);
    let _ = png::metadata(data);
    let mut decoder = Decoder::new();
    for t in [Transform::Identity, Transform::Expand, Transform::Normalize8] {
        let _ = png::output(data, t);
        if let Ok(picture) = decoder.decode_picture(data, t) {
            let _ = picture.to_rgba();
            let _ = picture.to_rgba_rounded();
            let _ = picture.samples16();
            let _ = png::encode_picture(&picture, &png::Options::default());
        }
        let mut small = [0u8; 256];
        let _ = decoder.decode_slice(data, t, &mut small);
    }
});
