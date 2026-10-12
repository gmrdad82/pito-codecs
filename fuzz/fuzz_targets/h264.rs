#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::h264::Decoder;
use pito_codecs::media::Packet;

fn packet(data: &[u8], pts: i64) -> Packet {
    Packet {
        data: data.to_vec(),
        pts,
        dts: pts,
        keyframe: false,
    }
}

fn start_codes(data: &[u8]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let mut dec = Decoder::with_threads(1);
    if mode & 1 == 0 {
        let _ = dec.decode(&packet(rest, 0));
    } else {
        let starts = start_codes(rest);
        let mut pts = 0;
        for w in starts.windows(2) {
            let _ = dec.decode(&packet(&rest[w[0]..w[1]], pts));
            pts += 1;
        }
        if let Some(&s) = starts.last() {
            let _ = dec.decode(&packet(&rest[s..], pts));
        }
    }
    let _ = dec.flush();
});
