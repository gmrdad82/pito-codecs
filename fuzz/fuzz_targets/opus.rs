#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::media::Result;
use pito_codecs::opus::{Decoder, OpusHead, StreamDecoder, ogg};

fn feed(mut data: &[u8], mut decode: impl FnMut(Option<&[u8]>, &mut Vec<f32>) -> Result<usize>) {
    let mut out = Vec::new();
    let mut n = 0;
    while data.len() >= 2 && n < 64 {
        let len = (u16::from_le_bytes([data[0], data[1]]) as usize).min(data.len() - 2);
        let (packet, rest) = data[2..].split_at(len);
        let _ = decode((len > 0).then_some(packet), &mut out);
        out.clear();
        data = rest;
        n += 1;
    }
}

fuzz_target!(|data: &[u8]| {
    if ogg::sniff(data) {
        if let Ok(file) = ogg::OggOpus::parse(data) {
            let _ = file.decode();
        }
        return;
    }
    if data.starts_with(b"OpusHead") {
        let Ok(head) = OpusHead::parse(data) else {
            return;
        };
        let used = 19
            + if head.mapping_family != 0 {
                2 + head.channels
            } else {
                0
            };
        let Ok(mut dec) = Decoder::from_head(head, 48000) else {
            return;
        };
        feed(&data[used.min(data.len())..], |p, out| {
            dec.decode_into(p, out)
        });
        return;
    }
    let Some((&config, rest)) = data.split_first() else {
        return;
    };
    let rate = [8000, 12000, 16000, 24000, 48000][config as usize % 5];
    let channels = 1 + (config as usize >> 3) % 2;
    let Ok(mut dec) = StreamDecoder::new(rate, channels) else {
        return;
    };
    feed(rest, |p, out| dec.decode_interleaved(p, out));
});
