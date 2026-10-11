use crate::media::AudioBuffer;
use crate::opus::enc::{Encoder, EncoderConfig, encode_ogg};
use crate::opus::ogg::{OggOpus, crc32};
use crate::opus::{Decoder, OpusHead, Packet, SAMPLE_RATES, StreamDecoder};

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
        match self.below(6) {
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
                if let Some(b) = p.first_mut() {
                    *b = self.next() as u8;
                }
            }
            3 => {
                let n = self.below(1300);
                *p = (0..n).map(|_| self.next() as u8).collect();
            }
            4 => {
                for _ in 0..1 + self.below(8) {
                    if !p.is_empty() {
                        let i = self.below(p.len().min(8));
                        p[i] = self.next() as u8;
                    }
                }
            }
            _ => {
                let toc = (self.below(32) as u8) << 3 | (self.next() as u8 & 7);
                if let Some(b) = p.first_mut() {
                    *b = toc;
                }
            }
        }
    }
}

fn signal(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| {
            let t = (i / channels) as f32 / 48000.0;
            0.3 * (std::f32::consts::TAU * 330.0 * (1 + i % channels) as f32 * t).sin()
        })
        .collect()
}

fn packets(channels: u16, bitrate: u32) -> Vec<Vec<u8>> {
    let mut enc = Encoder::new(EncoderConfig::new(channels, bitrate)).unwrap();
    let mut a = AudioBuffer::new(48000, channels);
    a.samples = signal(9600, channels as usize);
    let mut out: Vec<Vec<u8>> = enc
        .encode(&a)
        .unwrap()
        .into_iter()
        .map(|p| p.data)
        .collect();
    out.extend(enc.flush().unwrap().into_iter().map(|p| p.data));
    out
}

fn self_delimited(p: &[u8]) -> Vec<u8> {
    let mut v = vec![p[0] & !3];
    let s = p.len() - 1;
    if s < 252 {
        v.push(s as u8);
    } else {
        let b0 = 252 + (s & 3);
        v.push(b0 as u8);
        v.push(((s - b0) >> 2) as u8);
    }
    v.extend_from_slice(&p[1..]);
    v
}

fn fix_crcs(ogg: &mut [u8]) {
    let mut pos = 0;
    while pos + 27 <= ogg.len() && &ogg[pos..pos + 4] == b"OggS" {
        let nseg = ogg[pos + 26] as usize;
        if pos + 27 + nseg > ogg.len() {
            return;
        }
        let body: usize = ogg[pos + 27..pos + 27 + nseg]
            .iter()
            .map(|&b| b as usize)
            .sum();
        let end = (pos + 27 + nseg + body).min(ogg.len());
        ogg[pos + 22..pos + 26].fill(0);
        let crc = crc32(&ogg[pos..end]);
        ogg[pos + 22..pos + 26].copy_from_slice(&crc.to_le_bytes());
        pos = end;
    }
}

fn decode_ogg(data: &[u8]) {
    if let Ok(o) = OggOpus::parse(data) {
        let _ = o.samples();
        let _ = o.decode();
    }
}

#[test]
fn malformed_opus_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut good = packets(1, 24_000);
    good.extend(packets(2, 128_000));
    for &rate in &SAMPLE_RATES {
        for ch in 1..=2 {
            let mut dec = StreamDecoder::new(rate, ch).unwrap();
            let mut out = Vec::new();
            for _ in 0..ROUNDS {
                let mut p = good[rng.below(good.len())].clone();
                rng.mutate(&mut p);
                out.clear();
                let _ = Packet::parse(&p);
                let _ = Packet::parse_self_delimited(&p);
                match rng.below(8) {
                    0 => {
                        let _ = dec.decode_interleaved(None, &mut out);
                    }
                    1 => {
                        let n = rate as usize / 400 * (1 + rng.below(24));
                        let _ = dec.decode_fec(&p, n, &mut out);
                    }
                    2 => dec.reset(),
                    _ => {
                        let _ = dec.decode_interleaved(Some(&p), &mut out);
                    }
                }
            }
        }
    }

    let mut a = AudioBuffer::new(48000, 2);
    a.samples = signal(4800, 2);
    let ogg = encode_ogg(EncoderConfig::new(2, 64_000), &a).unwrap();
    for cut in (0..ogg.len().min(300)).chain((300..ogg.len()).step_by(97)) {
        decode_ogg(&ogg[..cut]);
    }
    for _ in 0..ROUNDS {
        let mut m = ogg.clone();
        for _ in 0..1 + rng.below(4) {
            let i = if rng.below(2) == 0 {
                rng.below(m.len().min(200))
            } else {
                rng.below(m.len())
            };
            m[i] = rng.next() as u8;
        }
        fix_crcs(&mut m);
        decode_ogg(&m);
    }

    let mut head = b"OpusHead".to_vec();
    head.extend([
        1, 6, 0x38, 0x01, 0x80, 0xBB, 0, 0, 0, 0, 1, 4, 2, 0, 4, 1, 2, 3, 5,
    ]);
    let stereo = packets(2, 96_000);
    let mono = packets(1, 32_000);
    for _ in 0..ROUNDS {
        let mut h = head.clone();
        for _ in 0..1 + rng.below(3) {
            let i = 8 + rng.below(h.len() - 8);
            h[i] = match rng.below(3) {
                0 => rng.next() as u8,
                1 => rng.below(8) as u8,
                _ => 255,
            };
        }
        if rng.below(4) == 0 {
            let n = rng.below(h.len() + 1);
            h.truncate(n);
        }
        let _ = OpusHead::from_dops(&h);
        let Ok(parsed) = OpusHead::parse(&h) else {
            continue;
        };
        let rate = SAMPLE_RATES[rng.below(SAMPLE_RATES.len())];
        let Ok(mut dec) = Decoder::from_head(parsed.clone(), rate) else {
            continue;
        };
        dec.set_trim_pre_skip(true);
        let mut out = Vec::new();
        for k in 0..4 {
            let mut p = Vec::new();
            for s in 0..parsed.streams {
                let src = if s < parsed.coupled_streams {
                    &stereo[k % stereo.len()]
                } else {
                    &mono[k % mono.len()]
                };
                if s + 1 < parsed.streams {
                    p.extend(self_delimited(src));
                } else {
                    p.extend_from_slice(src);
                }
            }
            if rng.below(2) == 0 {
                rng.mutate(&mut p);
            }
            let _ = dec.decode_into(Some(&p), &mut out);
            if rng.below(4) == 0 {
                let _ = dec.decode_into(None, &mut out);
            }
        }
    }
}
