#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::vorbis::Stream;

fuzz_target!(|data: &[u8]| {
    let Ok(mut stream) = Stream::from_bytes(data) else {
        return;
    };
    for _ in 0..2 {
        let mut blocks = 0;
        while let Ok(Some(block)) = stream.next_block() {
            let _ = block.len();
            blocks += 1;
            if blocks > 4096 {
                break;
            }
        }
        if stream.rewind().is_err() {
            break;
        }
    }
});
