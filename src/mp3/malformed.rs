use super::{Decoder, MAX_SAMPLES_PER_FRAME};

const ROUNDS: usize = 400;

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
}

const SECOND: [u8; 10] = [0xFB, 0xFA, 0xF3, 0xF2, 0xE3, 0xFD, 0xFC, 0xFF, 0xF5, 0xF7];

fn frame(rng: &mut Rng) -> Vec<u8> {
    let b1 = SECOND[rng.below(SECOND.len())];
    let bitrate = rng.below(15) as u8;
    let b2 = (bitrate << 4) | ((rng.below(3) as u8) << 2) | (rng.below(2) as u8) << 1;
    let b3 = rng.next() as u8;
    let n = 4 + rng.below(1500);
    let mut f: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
    f[..4].copy_from_slice(&[0xFF, b1, b2, b3]);
    f
}

#[test]
fn malformed_frames_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut pcm = vec![0.0f32; MAX_SAMPLES_PER_FRAME];
    let mut out = Vec::new();
    for _ in 0..ROUNDS {
        let mut dec = Decoder::with_format(44100, 2);
        let frames: Vec<Vec<u8>> = (0..1 + rng.below(8)).map(|_| frame(&mut rng)).collect();
        for f in &frames {
            let _ = dec.decode_packet(f, &mut out);
            let cut = rng.below(f.len() + 1);
            let _ = dec.decode_packet(&f[..cut], &mut out);
        }
        let mut stream: Vec<u8> = frames.concat();
        for _ in 0..rng.below(16) {
            if !stream.is_empty() {
                let i = rng.below(stream.len());
                stream[i] ^= 1 << rng.below(8);
            }
        }
        let mut dec = Decoder::new();
        let mut rest = &stream[..];
        while !rest.is_empty() {
            let f = dec.decode(rest, &mut pcm);
            if f.frame_bytes == 0 {
                break;
            }
            rest = &rest[f.frame_bytes.min(rest.len())..];
        }
    }
    let mut dec = Decoder::new();
    assert!(dec.decode_packet(&[], &mut out).is_err());
    assert!(dec.decode_packet(&[0, 1, 2, 3, 4, 5], &mut out).is_err());
}
