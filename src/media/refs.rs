#![allow(dead_code)]

pub const AREA: &str = "media";

pub const WINDOW: usize = 256;

pub fn window(file: &str, at: &str) -> String {
    format!("{file}.{at}.f32")
}

pub fn decoded(file: &str) -> String {
    format!("{file}.f32")
}

pub const LINEUP_RATE: u32 = 48_000;
pub const LINEUP_FRAMES: usize = 48_000;
pub const LINEUP_KBPS: u32 = 96;
pub const LINEUP_AT: [usize; 2] = [10_000, 25_440];
pub const LINEUP_AAC: &str = "tones-aac.mkv";
pub const LINEUP_OPUS: &str = "tones-opus.webm";
pub const LINEUPS: [&str; 2] = [LINEUP_AAC, LINEUP_OPUS];

pub const OFF_MS_SECONDS: f64 = 1.0123;
pub const OFF_MS_HEAD: usize = 2_000;
pub const OFF_MS_KBPS: u32 = 32;

pub struct OffMs {
    pub file: &'static str,
    pub rate: u32,
    pub limit: f32,
    pub silent: bool,
}

pub const OFF_MS: [OffMs; 4] = [
    OffMs {
        file: "off-ms-opus.webm",
        rate: 48_000,
        limit: 2e-3,
        silent: false,
    },
    OffMs {
        file: "off-ms-opus.mkv",
        rate: 48_000,
        limit: 2e-3,
        silent: false,
    },
    OffMs {
        file: "off-ms-ac3.mkv",
        rate: 48_000,
        limit: 1e-3,
        silent: true,
    },
    OffMs {
        file: "off-ms-mp3.mkv",
        rate: 44_100,
        limit: 1e-3,
        silent: false,
    },
];

pub const OFF_MS_MP3: &str = "off-ms-mp3.mp3";

pub fn off_ms_frames(rate: u32) -> usize {
    (OFF_MS_SECONDS * f64::from(rate)).round() as usize
}

pub const VP9_ALTREF: &str = "vp9-g10.webm";
pub const VP9_ALTREF_SIZE: (u32, u32) = (352, 240);
pub const VP9_ALTREF_FRAMES: u64 = 60;

pub const PYRAMID: &str = "pyramid-hevc.mp4";
pub const PYRAMID_SIZE: (u32, u32) = (352, 240);
pub const PYRAMID_FRAMES: usize = 45;

pub fn pyramid_recon() -> String {
    format!("{PYRAMID}.yuv")
}

pub const AV1_709: &str = "av1-709.mp4";
pub const VP9_709: &str = "vp9-709.mp4";
pub const TAGGED_SIZE: (u32, u32) = (352, 240);
pub const TAGGED_FRAMES: u64 = 30;

pub type Key = (
    (u32, u32),
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

pub const VP9_KEYS: [Key; 4] = [
    ((848, 480), "bt709", "bt709", "tv", "bt601"),
    ((848, 480), "bt709", "bt709", "pc", "bt601"),
    ((1920, 1080), "bt601", "bt601", "tv", "bt709"),
    ((1920, 1080), "bt601", "bt601", "pc", "bt709"),
];

pub fn vp9_key(h: u32, space: &str, range: &str) -> String {
    format!("vp9-key-{h}-{space}-{range}.webm")
}

pub const AV1S: [(&str, u8); 4] = [
    ("av1-8.mp4", 8),
    ("av1-8.mkv", 8),
    ("av1-8.webm", 8),
    ("av1-10.mp4", 10),
];
pub const AV1_SIZE: (u32, u32) = (352, 240);
pub const AV1_FRAMES: usize = 10;

pub const VARIANT_SIZE: (u32, u32) = (64, 64);
pub const VARIANT_FPS: u32 = 10;
pub const VARIANT_FRAMES: u64 = 10;

pub const VP9_PROFILE2: &str = "vp9-profile2-g10.webm";
pub const VP9_PROFILE2_SIZE: (u32, u32) = (352, 240);
pub const VP9_PROFILE2_FRAMES: u64 = 30;

pub const MP3: &str = "bare-sine.mp3";
pub const MP3_RATE: u32 = 44_100;

pub mod ebml {
    pub const EBML: u32 = 0x1A45_DFA3;
    pub const SEGMENT: u32 = 0x1853_8067;
    pub const INFO: u32 = 0x1549_A966;
    pub const TRACKS: u32 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u32 = 0xAE;
    pub const CLUSTER: u32 = 0x1F43_B675;
    pub const TIMESTAMP: u32 = 0xE7;
    pub const SIMPLE_BLOCK: u32 = 0xA3;
    pub const BLOCK_GROUP: u32 = 0xA0;
    pub const BLOCK: u32 = 0xA1;
    pub const DISCARD_PADDING: u32 = 0x75A2;

    pub fn id(out: &mut Vec<u8>, id: u32) {
        let n = match id {
            0..=0xFF => 1,
            0x100..=0xFFFF => 2,
            0x1_0000..=0xFF_FFFF => 3,
            _ => 4,
        };
        out.extend_from_slice(&id.to_be_bytes()[4 - n..]);
    }

    pub fn el(id_: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + 12);
        id(&mut out, id_);
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    pub fn unknown(id_: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(body.len() + 12);
        id(&mut out, id_);
        out.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        out.extend_from_slice(body);
        out
    }

    pub fn uint(id_: u32, v: u64) -> Vec<u8> {
        el(id_, &v.to_be_bytes())
    }

    pub fn sint(id_: u32, v: i64) -> Vec<u8> {
        el(id_, &v.to_be_bytes())
    }

    pub fn float(id_: u32, v: f64) -> Vec<u8> {
        el(id_, &v.to_be_bytes())
    }

    pub fn text(id_: u32, v: &str) -> Vec<u8> {
        el(id_, v.as_bytes())
    }

    pub fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }
}

#[derive(Clone, Debug, Default)]
pub struct MkvTrack {
    pub video: bool,
    pub codec: &'static str,
    pub private: Vec<u8>,
    pub size: (u32, u32),
    pub rate: f64,
    pub channels: u64,
    pub bits: u64,
    pub delay_ns: u64,
    pub preroll_ns: u64,
}

#[derive(Clone, Debug)]
pub struct MkvBlock {
    pub track: usize,
    pub ms: i64,
    pub key: bool,
    pub data: Vec<u8>,
    pub discard_ns: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sizes {
    Known,
    Unknown,
}

pub fn matroska(
    doctype: &str,
    tracks: &[MkvTrack],
    blocks: &[MkvBlock],
    duration_ms: f64,
    sizes: Sizes,
) -> Vec<u8> {
    use ebml::*;
    let head = el(
        EBML,
        &cat(&[
            uint(0x4286, 1),
            uint(0x42F7, 1),
            uint(0x42F2, 4),
            uint(0x42F3, 8),
            text(0x4282, doctype),
            uint(0x4287, 4),
            uint(0x4285, 2),
        ]),
    );
    let info = el(
        INFO,
        &cat(&[
            uint(0x2A_D7B1, 1_000_000),
            text(0x4D80, "pito-codecs examples/record_refs"),
            text(0x5741, "pito-codecs examples/record_refs"),
            float(0x4489, duration_ms),
        ]),
    );
    let entries: Vec<Vec<u8>> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let n = i as u64 + 1;
            let mut parts = vec![
                uint(0xD7, n),
                uint(0x73C5, n),
                uint(0x83, if t.video { 1 } else { 2 }),
                uint(0x9C, 0),
                text(0x86, t.codec),
            ];
            if !t.private.is_empty() {
                parts.push(el(0x63A2, &t.private));
            }
            if t.delay_ns > 0 {
                parts.push(uint(0x56AA, t.delay_ns));
            }
            if t.preroll_ns > 0 {
                parts.push(uint(0x56BB, t.preroll_ns));
            }
            if t.video {
                parts.push(el(
                    0xE0,
                    &cat(&[
                        uint(0xB0, u64::from(t.size.0)),
                        uint(0xBA, u64::from(t.size.1)),
                    ]),
                ));
            } else {
                let mut audio = vec![float(0xB5, t.rate), uint(0x9F, t.channels)];
                if t.bits > 0 {
                    audio.push(uint(0x6264, t.bits));
                }
                parts.push(el(0xE1, &cat(&audio)));
            }
            el(TRACK_ENTRY, &cat(&parts))
        })
        .collect();
    let mut body = cat(&[info, el(TRACKS, &cat(&entries))]);
    let mut at = 0;
    while at < blocks.len() {
        let start = blocks[at].ms;
        let mut cluster = uint(TIMESTAMP, start.max(0) as u64);
        while at < blocks.len() && blocks[at].ms - start < 1000 {
            let b = &blocks[at];
            let mut raw = vec![0x80 | (b.track as u8 + 1)];
            raw.extend_from_slice(&((b.ms - start) as i16).to_be_bytes());
            if b.discard_ns == 0 {
                raw.push(if b.key { 0x80 } else { 0 });
                raw.extend_from_slice(&b.data);
                cluster.extend(el(SIMPLE_BLOCK, &raw));
            } else {
                raw.push(0);
                raw.extend_from_slice(&b.data);
                cluster.extend(el(
                    BLOCK_GROUP,
                    &cat(&[el(BLOCK, &raw), sint(DISCARD_PADDING, b.discard_ns)]),
                ));
            }
            at += 1;
        }
        body.extend(match sizes {
            Sizes::Known => el(CLUSTER, &cluster),
            Sizes::Unknown => unknown(CLUSTER, &cluster),
        });
    }
    let segment = match sizes {
        Sizes::Known => el(SEGMENT, &body),
        Sizes::Unknown => unknown(SEGMENT, &body),
    };
    cat(&[head, segment])
}

pub const AC3_FRAME: usize = 1536;
pub const AC3_FRAME_BYTES: usize = 256;

struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if v >> i & 1 == 1 {
                let at = self.bits / 8;
                self.bytes[at] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn ac3_silent_frame() -> Vec<u8> {
    let mut w = BitWriter {
        bytes: Vec::new(),
        bits: 0,
    };
    w.put(0x0B77, 16);
    w.put(0, 16);
    w.put(0, 2);
    w.put(8, 6);
    w.put(8, 5);
    w.put(0, 3);
    w.put(2, 3);
    w.put(0, 2);
    w.put(0, 1);
    w.put(27, 5);
    w.put(0, 3);
    w.put(0, 1);
    w.put(1, 1);
    w.put(0, 3);
    for blk in 0..6 {
        w.put(0, 2);
        w.put(0, 2);
        w.put(0, 1);
        if blk == 0 {
            w.put(1, 1);
            w.put(0, 1);
            w.put(1, 1);
            w.put(0, 4);
            w.put(1, 2);
            w.put(1, 2);
            for _ in 0..2 {
                w.put(0, 6);
            }
            for _ in 0..2 {
                w.put(12, 4);
                for _ in 0..24 {
                    w.put(62, 7);
                }
                w.put(0, 2);
            }
            w.put(1, 1);
            w.put(2, 2);
            w.put(1, 2);
            w.put(1, 2);
            w.put(2, 2);
            w.put(4, 3);
            w.put(1, 1);
            w.put(0, 6);
            for _ in 0..2 {
                w.put(0, 4);
                w.put(0, 3);
            }
        } else {
            w.put(0, 1);
            w.put(0, 1);
            w.put(0, 2);
            w.put(0, 2);
            w.put(0, 1);
            w.put(0, 1);
        }
        w.put(0, 1);
        w.put(0, 1);
    }
    let mut frame = w.bytes;
    frame.resize(AC3_FRAME_BYTES, 0);
    let five_eighths = (AC3_FRAME_BYTES / 2 / 8 * 5) * 2;
    let head = (0..=u16::MAX)
        .find(|&c| {
            let mut probe = frame[2..five_eighths].to_vec();
            probe[..2].copy_from_slice(&c.to_be_bytes());
            crc16(&probe) == 0
        })
        .unwrap_or(0);
    frame[2..4].copy_from_slice(&head.to_be_bytes());
    let n = frame.len();
    let tail = crc16(&frame[five_eighths..n - 2]);
    frame[n - 2..].copy_from_slice(&tail.to_be_bytes());
    frame
}

pub fn ac3_silence(frames: usize) -> Vec<Vec<u8>> {
    let frame = ac3_silent_frame();
    vec![frame; frames]
}
