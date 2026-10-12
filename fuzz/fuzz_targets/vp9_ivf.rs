#![no_main]

use libfuzzer_sys::fuzz_target;
use pito_codecs::media::Packet;
use pito_codecs::vp9::Decoder;

fn frames(data: &[u8]) -> Vec<(&[u8], i64)> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        out.push((data, 0));
        return out;
    }
    let header = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut p = header.clamp(32, data.len());
    while p + 12 <= data.len() && out.len() < 64 {
        let size = u32::from_le_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
        let mut pts = [0u8; 8];
        pts.copy_from_slice(&data[p + 4..p + 12]);
        let pts = u64::from_le_bytes(pts) as i64;
        p += 12;
        let end = p.saturating_add(size).min(data.len());
        out.push((&data[p..end], pts));
        p = end;
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let mut dec = Decoder::with_threads(1);
    for (frame, pts) in frames(data) {
        let _ = dec.decode(&Packet {
            data: frame.to_vec(),
            pts,
            dts: pts,
            keyframe: false,
        });
    }
    let _ = dec.flush();
});
