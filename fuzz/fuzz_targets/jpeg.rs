#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::jpeg;

fuzz_target!(|data: &[u8]| {
    let _ = jpeg::decode::info(data);
    let _ = jpeg::decode(data);
    let _ = jpeg::decode_into(data, &mut pito_codecs::media::Rgba::new(0, 0));
    let _ = pito_codecs::image::decode(data);
    let _ = pito_codecs::image::dimensions(data);
});
