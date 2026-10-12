#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::media::Pcm;
use pito_codecs::mp4::PcmConfig;

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    let bits = [8u16, 16, 24, 32, 64, 12, 0, 20][(data[0] & 7) as usize];
    let float = data[0] & 8 != 0;
    let signed = data[0] & 16 != 0;
    let big_endian = data[0] & 32 != 0;
    let channels = data[1] as u64;
    let rate = u16::from_le_bytes([data[2], data[3]]) as f64 * 12.0 + data[4] as f64 / 3.0;
    let _ = Pcm::from_mp4(&PcmConfig {
        bits,
        float,
        big_endian,
        signed,
        channels: channels as u32,
        sample_rate: rate,
    });
    let Ok(pcm) = Pcm::new(bits, float, signed, big_endian, channels, rate) else {
        return;
    };
    let mut out = Vec::new();
    let _ = pcm.decode(&data[6..], &mut out);
});
