pub const AREA: &str = "mkv";
pub const SIZE: (u32, u32) = (160, 120);
pub const FPS: u32 = 15;
pub const SECONDS: u32 = 2;
pub const GOP: u32 = 7;

pub const FOREIGN: [(&str, &str); 3] = [
    ("h264.mkv", "h264.flv"),
    ("vp9.webm", "vp9.ivf"),
    ("piped.mkv", "h264.flv"),
];

pub type Packets = Vec<(i64, u32, bool)>;

pub fn ivf_packets(data: &[u8]) -> Packets {
    let at = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    let (den, num) = (i64::from(at(16)), i64::from(at(20)));
    let mut pos = usize::from(u16::from_le_bytes([data[6], data[7]]));
    let mut out = Vec::new();
    while pos + 12 <= data.len() {
        let size = at(pos);
        let pts = i64::from_le_bytes(data[pos + 4..pos + 12].try_into().unwrap_or_default());
        let first = data.get(pos + 12).copied().unwrap_or_default();
        let profile = (first >> 5 & 1) | (first >> 3 & 2);
        let skip = usize::from(profile == 3);
        let key = first >> (3 - skip) & 1 == 0 && first >> (2 - skip) & 1 == 0;
        out.push(((pts * num * 1000 + den / 2) / den.max(1), size, key));
        pos += 12 + size as usize;
    }
    out
}

pub fn flv_packets(data: &[u8]) -> Packets {
    let be = |at: usize, n: usize| {
        data.get(at..at + n)
            .map_or(0, |b| b.iter().fold(0u32, |v, &x| v << 8 | u32::from(x)))
    };
    let mut pos = 13;
    let mut out = Vec::new();
    while pos + 11 <= data.len() {
        let size = be(pos + 1, 3) as usize;
        let dts = i64::from(be(pos + 4, 3) | be(pos + 7, 1) << 24);
        let body = pos + 11;
        if data[pos] == 9 && size > 5 && data.get(body + 1) == Some(&1) {
            let cts = (be(body + 2, 3) << 8) as i32 >> 8;
            let key = data[body] >> 4 == 1;
            out.push((dts + i64::from(cts), (size - 5) as u32, key));
        }
        pos = body + size + 4;
    }
    out
}

pub fn normalized(mut packets: Packets) -> Packets {
    let first = packets.iter().map(|p| p.0).min().unwrap_or(0);
    for p in &mut packets {
        p.0 -= first;
    }
    packets
}
