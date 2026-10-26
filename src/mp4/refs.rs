pub const AREA: &str = "mp4";
pub const SIZE: (u32, u32) = (160, 120);
pub const FPS: u32 = 30;
pub const SECONDS: u32 = 1;
pub const AUDIO_KBPS: u32 = 64;

pub const BFRAMES: &str = "bframes.mp4";
pub const BFRAMES_TWIN: &str = "bframes.flv";
pub const TONE: &str = "tone.m4a";
pub const TONE_TWIN: &str = "tone.aac";
pub const HEVC: &str = "hevc.mov";
pub const HEVC_STREAM: &str = "hevc.hevc";
pub const HEVC_POCS: &str = "hevc.mov.pocs";

pub fn adts_sizes(data: &[u8]) -> Vec<u32> {
    let mut at = 0;
    let mut out = Vec::new();
    while at + 7 <= data.len() && data[at] == 0xFF && data[at + 1] & 0xF0 == 0xF0 {
        let len = (usize::from(data[at + 3] & 3) << 11)
            | (usize::from(data[at + 4]) << 3)
            | usize::from(data[at + 5] >> 5);
        let head = if data[at + 1] & 1 == 0 { 9 } else { 7 };
        out.push((len - head) as u32);
        at += len.max(head);
    }
    out
}

pub fn pocs(text: &str) -> Vec<i64> {
    text.split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect()
}

pub type Packets = Vec<(i64, u32, bool)>;

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
