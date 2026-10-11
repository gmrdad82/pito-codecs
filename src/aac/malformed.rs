use crate::aac::{
    AdtsHeader, AudioSpecificConfig, Decoder, Encoder, EncoderConfig, Resampler, split_adts,
};
use crate::media::AudioBuffer;

const ROUNDS: usize = 40;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn mutate(&mut self, p: &mut Vec<u8>) {
        match self.below(4) {
            0 => {
                let n = self.below(p.len() + 1);
                p.truncate(n);
            }
            1 => {
                for _ in 0..1 + self.below(4) {
                    if !p.is_empty() {
                        let i = self.below(p.len());
                        p[i] ^= 1 << self.below(8);
                    }
                }
            }
            2 => {
                for _ in 0..1 + self.below(4) {
                    if !p.is_empty() {
                        let i = self.below(p.len().min(12));
                        p[i] = self.next() as u8;
                    }
                }
            }
            _ => {
                let n = self.below(600);
                *p = (0..n).map(|_| self.next() as u8).collect();
            }
        }
    }
}

fn encoded(rate: u32, channels: u16) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut enc = Encoder::new(EncoderConfig {
        rate,
        channels,
        bitrate: 0,
    })
    .unwrap();
    let mut a = AudioBuffer::new(rate, channels);
    let frames = rate as usize / 8;
    a.samples = (0..frames * channels as usize)
        .map(|i| {
            let t = (i / channels as usize) as f32 / rate as f32;
            let f = 200.0 * (1 + i % channels as usize) as f32;
            0.3 * (std::f32::consts::TAU * f * t).sin() * if i % 997 < 40 { 3.0 } else { 1.0 }
        })
        .collect();
    let mut packets: Vec<Vec<u8>> = enc
        .encode(&a)
        .unwrap()
        .into_iter()
        .map(|p| p.data)
        .collect();
    packets.extend(enc.flush().into_iter().map(|p| p.data));
    (enc.extradata(), packets)
}

#[test]
fn malformed_aac_never_panics() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for (rate, channels) in [(48000, 2), (44100, 1), (8000, 2), (96000, 1), (48000, 7)] {
        let (asc, packets) = encoded(rate, channels);
        let mut dec = Decoder::new(&asc).unwrap();
        let p = &packets[packets.len() / 2];
        for cut in (0..p.len().min(160)).chain((160..p.len()).step_by(41)) {
            let _ = dec.decode_frame(&p[..cut]);
        }
        for _ in 0..ROUNDS {
            let mut p = packets[rng.below(packets.len())].clone();
            rng.mutate(&mut p);
            let _ = dec.decode_frame(&p);
        }
        for _ in 0..ROUNDS {
            let mut c = asc.clone();
            rng.mutate(&mut c);
            let _ = AudioSpecificConfig::parse(&c);
            if let Ok(mut d) = Decoder::new(&c) {
                for p in packets.iter().take(2) {
                    let _ = d.decode_frame(p);
                }
            }
        }
        let parsed = AudioSpecificConfig::parse(&asc).unwrap();
        if parsed.channel_config == 0 {
            continue;
        }
        let mut adts = Vec::new();
        for p in &packets {
            adts.extend(AdtsHeader::write(
                2,
                parsed.sf_index,
                parsed.channel_config,
                p.len(),
                0x7FF,
            ));
            adts.extend_from_slice(p);
        }
        for _ in 0..ROUNDS {
            let mut m = adts.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(m.len().min(64));
                m[i] = rng.next() as u8;
            }
            let n = rng.below(m.len() + 1);
            m.truncate(n);
            if let Ok(frames) = split_adts(&m) {
                for (h, payload) in frames.into_iter().take(3) {
                    if let Ok(mut d) = Decoder::from_adts(&h) {
                        let _ = d.decode_frame(payload);
                    }
                }
            }
        }
    }
    let mut r = Resampler::new(u32::MAX - 4, 48000, 2);
    let mut a = AudioBuffer::new(u32::MAX - 4, 2);
    a.samples = vec![0.25; 64];
    let _ = r.process(&a);
    let _ = r.flush();
}
