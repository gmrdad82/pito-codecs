#![cfg(all(feature = "gpu", feature = "h264"))]

use pito_codecs::gpu::{self, HwDecoder, HwEncoder};
use pito_codecs::h264;
use pito_codecs::media::{Codec, Frame, Packet, PixelFormat, TimeBase};

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn picture(i: usize, w: u32, h: u32) -> Frame {
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, TimeBase::new(1, 30));
    for (p, plane) in f.planes.iter_mut().enumerate() {
        for (k, v) in plane.data.iter_mut().enumerate() {
            *v = ((k * (p + 1) + i * 7) % 251) as u8;
        }
    }
    f
}

fn stream() -> Vec<Packet> {
    let (w, h) = (128, 96);
    let mut enc = h264::Encoder::new(h264::EncoderConfig {
        width: w,
        height: h,
        gop: 4,
        b_frames: 0,
        threads: 1,
        bitstream: pito_codecs::media::Bitstream::AnnexB,
        ..h264::EncoderConfig::default()
    })
    .unwrap();
    let mut packets = Vec::new();
    for i in 0..8 {
        packets.extend(enc.encode(&picture(i, w, h)).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    packets
}

fn feed(codec: Codec, extradata: &[u8], packets: &[Packet]) {
    let mut dec = match HwDecoder::new(codec, extradata) {
        Ok(d) => d,
        Err(e) => {
            assert!(!e.to_string().is_empty());
            return;
        }
    };
    for p in packets {
        if dec.decode(p).is_err() {
            return;
        }
    }
    let _ = dec.flush();
}

#[test]
fn broken_streams_and_configs_never_panic_the_gpu_front() {
    let good = stream();
    feed(Codec::H264, &[], &good);
    let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
    for round in 0..24 {
        let mut packets = good.clone();
        if round % 3 == 0 {
            let cut = noise.below(packets.len());
            packets.truncate(cut + 1);
            let last = &mut packets[cut].data;
            let keep = noise.below(last.len());
            last.truncate(keep);
        } else {
            for _ in 0..1 + noise.below(6) {
                let k = noise.below(packets.len());
                let data = &mut packets[k].data;
                if data.is_empty() {
                    continue;
                }
                let at = noise.below(data.len().min(64));
                data[at] ^= 1 << noise.below(8);
            }
        }
        feed(Codec::H264, &[], &packets);
    }
    for codec in [Codec::H264, Codec::Hevc] {
        let junk: Vec<Packet> = (0..4)
            .map(|i| Packet {
                data: (0..64 + i * 37)
                    .map(|_| noise.next() as u8)
                    .chain([0, 0, 1])
                    .collect(),
                pts: i as i64,
                dts: i as i64,
                keyframe: i == 0,
            })
            .collect();
        feed(codec, &[], &junk);
        feed(codec, &[1, 0xff, 0, 0, 0xff, 0xe1, 0xff, 0xff], &junk);
        feed(codec, &[0, 0, 0, 1, 0x67, 0xff, 0xff, 0xff], &junk);
    }
}

#[test]
fn impossible_encoder_settings_are_refused() {
    let sizes = [(0, 0), (1, 1), (17, 9), (100_000, 100_000), (u32::MAX, 2)];
    let rates = [(30, 1), (0, 0), (30, 0), (0, 1), (u32::MAX, 1)];
    for codec in [Codec::H264, Codec::Hevc] {
        for &(width, height) in &sizes {
            for &fps in &rates {
                let config = gpu::EncoderConfig {
                    width,
                    height,
                    fps,
                    ..gpu::EncoderConfig::default()
                };
                let Ok(mut enc) = HwEncoder::new(codec, &config) else {
                    continue;
                };
                let wrong = picture(0, 64, 48);
                if (width, height) != (64, 48) {
                    assert!(enc.encode(&wrong).is_err());
                }
                let _ = enc.flush();
            }
        }
    }
}
