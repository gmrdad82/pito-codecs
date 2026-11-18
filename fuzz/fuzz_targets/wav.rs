#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::media::wav::{self, Format, Reader, Writer};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let _ = wav::layout(data);
    let _ = wav::decode(data);
    if let Ok(mut reader) = Reader::new(Cursor::new(data)) {
        let mut floats = Vec::new();
        let _ = reader.read(4096, &mut floats);
        reader.seek(0);
        let mut ints = Vec::new();
        let _ = reader.read_ints(4096, &mut ints);
    }
    if data.len() >= 4 {
        let format = [
            Format::Int8,
            Format::Int16,
            Format::Int24,
            Format::Int32,
            Format::Float32,
        ][usize::from(data[0]) % 5];
        let channels = u16::from(data[1] % 9);
        let rate = u32::from(u16::from_le_bytes([data[2], data[3]]));
        if let Ok(mut writer) = Writer::new(Cursor::new(Vec::new()), rate, channels, format) {
            let samples: Vec<f32> = data[4..]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let _ = writer.write(&samples);
            let _ = writer.finish();
        }
    }
});
