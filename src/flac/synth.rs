pub struct Fixture {
    pub name: &'static str,
    pub seed: u32,
    pub rate: u32,
    pub channels: usize,
    pub bits: u32,
    pub shift: u32,
    pub frames: usize,
    pub block: u64,
    pub flac: &'static [&'static str],
}

pub const FIXTURES: [Fixture; 7] = [
    Fixture {
        name: "s16_stereo_44k",
        seed: 1,
        rate: 44100,
        channels: 2,
        bits: 16,
        shift: 0,
        frames: 6000,
        block: 4096,
        flac: &["-5"],
    },
    Fixture {
        name: "s24_mono_96k_exhaustive",
        seed: 2,
        rate: 96000,
        channels: 1,
        bits: 24,
        shift: 0,
        frames: 4800,
        block: 4096,
        flac: &["-8", "-e", "-p"],
    },
    Fixture {
        name: "s16_block1001_lpc32",
        seed: 3,
        rate: 44100,
        channels: 2,
        bits: 16,
        shift: 0,
        frames: 6500,
        block: 1001,
        flac: &["--lax", "-b", "1001", "-l", "32"],
    },
    Fixture {
        name: "s24_wasted_bits",
        seed: 4,
        rate: 48000,
        channels: 1,
        bits: 24,
        shift: 8,
        frames: 4800,
        block: 4096,
        flac: &["-5"],
    },
    Fixture {
        name: "u8_mono_8k",
        seed: 5,
        rate: 8000,
        channels: 1,
        bits: 8,
        shift: 0,
        frames: 2000,
        block: 4096,
        flac: &["-5"],
    },
    Fixture {
        name: "s16_six_channels",
        seed: 6,
        rate: 48000,
        channels: 6,
        bits: 16,
        shift: 0,
        frames: 1200,
        block: 4096,
        flac: &["-5"],
    },
    Fixture {
        name: "s16_block577_independent",
        seed: 7,
        rate: 11025,
        channels: 2,
        bits: 16,
        shift: 0,
        frames: 3000,
        block: 577,
        flac: &["--lax", "-b", "577", "--no-mid-side"],
    },
];

impl Fixture {
    pub fn pcm(&self) -> Vec<i32> {
        pcm(
            self.seed,
            self.frames,
            self.channels,
            self.bits - self.shift,
        )
        .into_iter()
        .map(|v| v << self.shift)
        .collect()
    }
}

pub fn pcm(seed: u32, frames: usize, channels: usize, bits: u32) -> Vec<i32> {
    let top = (1i64 << (bits - 1)) - 1;
    let low = -(1i64 << (bits - 1));
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    let mut noise = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as i64
    };
    let quiet = 1;
    let mut osc: Vec<[i64; 4]> = (0..channels)
        .map(|_| [top * 11 / 20, 0, top * 3 / 10, 0])
        .collect();
    let burst = frames * 5 / 8;
    let silence = burst + 256.min(frames / 8);
    let constant = frames * 6 / 8;
    let tail = frames * 7 / 8;
    let mut out = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for (c, o) in osc.iter_mut().enumerate() {
            let v = if i < burst || i >= tail {
                let up = if i >= tail { 2 } else { 1 };
                let (e1, e2) = (up * (900 + 97 * c as i64), up * (5100 + 311 * c as i64));
                o[0] -= (o[1] * e1) >> 16;
                o[1] += (o[0] * e1) >> 16;
                o[2] -= (o[3] * e2) >> 16;
                o[3] += (o[2] * e2) >> 16;
                o[1] + o[3] + noise() % (2 * quiet + 1) - quiet
            } else if i < silence {
                noise().rem_euclid(top - low + 1) + low
            } else if i < constant {
                0
            } else {
                top / 3 - 100 * c as i64
            };
            out.push(v.clamp(low, top) as i32);
        }
    }
    out
}

pub fn wav(pcm: &[i32], rate: u32, channels: usize, bits: u32) -> Vec<u8> {
    let width = bits.div_ceil(8) as usize;
    let data = pcm.len() * width;
    let mask: u32 = match channels {
        1 => 0x4,
        2 => 0x3,
        6 => 0x3f,
        n => (1u32 << n) - 1,
    };
    let mut w = Vec::with_capacity(68 + data);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&((60 + data) as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&40u32.to_le_bytes());
    w.extend_from_slice(&0xfffeu16.to_le_bytes());
    w.extend_from_slice(&(channels as u16).to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * (channels * width) as u32).to_le_bytes());
    w.extend_from_slice(&((channels * width) as u16).to_le_bytes());
    w.extend_from_slice(&((width * 8) as u16).to_le_bytes());
    w.extend_from_slice(&22u16.to_le_bytes());
    w.extend_from_slice(&(bits as u16).to_le_bytes());
    w.extend_from_slice(&mask.to_le_bytes());
    w.extend_from_slice(&[
        1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
    ]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(data as u32).to_le_bytes());
    for &s in pcm {
        if width == 1 {
            w.push((s + 128) as u8);
        } else {
            w.extend_from_slice(&s.to_le_bytes()[..width]);
        }
    }
    w
}

pub fn fnv(pcm: &[i32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for s in pcm {
        for b in s.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}
