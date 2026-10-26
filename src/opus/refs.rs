#![allow(dead_code)]

pub mod window {
    pub const RUN: usize = 32;
    pub const EVERY: usize = 50;

    pub fn body(samples: &[f32], channels: usize) -> Vec<f32> {
        samples
            .chunks(RUN * channels.max(1))
            .step_by(EVERY)
            .flatten()
            .copied()
            .collect()
    }

    pub fn tail(samples: &[f32], channels: usize) -> &[f32] {
        &samples[samples.len().saturating_sub(RUN * channels.max(1))..]
    }

    pub fn window(samples: &[f32], channels: usize) -> Vec<f32> {
        let mut out = body(samples, channels);
        out.extend_from_slice(tail(samples, channels));
        out
    }
}

use pito_codecs::media::AudioBuffer;
use pito_codecs::opus::enc::{Application, EncoderConfig, encode_ogg};

pub const AREA: &str = "opus";
pub const SECONDS: f32 = 0.3;
pub const SPEECH_SECONDS: f32 = 0.6;

pub struct Libopus {
    pub name: &'static str,
    pub music: bool,
    pub channels: u16,
    pub bitrate: i32,
    pub cbr: bool,
    pub voip: bool,
    pub celt: bool,
}

const fn libopus(
    name: &'static str,
    music: bool,
    channels: u16,
    bitrate: i32,
    cbr: bool,
    voip: bool,
    celt: bool,
) -> Libopus {
    Libopus {
        name,
        music,
        channels,
        bitrate,
        cbr,
        voip,
        celt,
    }
}

pub const LIBOPUS: [Libopus; 4] = [
    libopus("music_160k", true, 2, 160_000, true, false, true),
    libopus("speech_16k_voip", false, 1, 16_000, false, true, false),
    libopus("music_32k_hybrid", true, 2, 32_000, false, false, false),
    libopus("music_51_256k", true, 6, 256_000, false, false, true),
];

pub const OURS: [(u16, u32); 3] = [(2, 128_000), (1, 64_000), (2, 64_000)];
pub const SILK: [(u16, u32); 2] = [(1, 16_000), (2, 24_000)];

pub fn stream(name: &str) -> String {
    format!("{name}.opus")
}

pub fn decoded(file: &str) -> String {
    format!("{file}.f32")
}

pub fn ours(channels: u16, bitrate: u32) -> String {
    format!("enc_{channels}_{bitrate}.opus")
}

pub fn silk(channels: u16, bitrate: u32) -> String {
    format!("silk_{channels}_{bitrate}.opus")
}

pub fn signal(seconds: f32, channels: usize) -> Vec<f32> {
    let n = (48000.0 * seconds) as usize;
    let mut seed = 0x1234_5678u32;
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f32 / 48000.0;
        let env = if (i / 3000) % 4 == 3 { 0.2 } else { 1.0 };
        for ch in 0..channels {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 9) as f32 / (1 << 23) as f32 - 0.5) * 0.05;
            let f = if ch == 0 { 440.0 } else { 660.0 };
            let v = 0.3 * (2.0 * std::f32::consts::PI * f * t).sin()
                + 0.1 * (2.0 * std::f32::consts::PI * 3150.0 * t).sin()
                + noise;
            out.push(v * env);
        }
    }
    out
}

pub fn speech(channels: usize) -> Vec<f32> {
    speechlike(SPEECH_SECONDS, 48000)
        .iter()
        .flat_map(|&v| std::iter::repeat_n(v, channels))
        .collect()
}

pub fn encode(cfg: EncoderConfig, pcm: &[f32]) -> Result<Vec<u8>, String> {
    let mut buf = AudioBuffer::new(48000, cfg.channels);
    buf.samples = pcm.to_vec();
    encode_ogg(cfg, &buf).map_err(|e| e.to_string())
}

pub fn encode_ours(channels: u16, bitrate: u32) -> Result<Vec<u8>, String> {
    let pcm = signal(SECONDS, channels as usize);
    encode(EncoderConfig::new(channels, bitrate), &pcm)
}

pub fn encode_silk(channels: u16, bitrate: u32) -> Result<Vec<u8>, String> {
    let mut cfg = EncoderConfig::new(channels, bitrate);
    cfg.application = Application::Voip;
    encode(cfg, &speech(channels as usize))
}

pub fn speechlike(seconds: f32, rate: u32) -> Vec<f32> {
    let n = (seconds * rate as f32) as usize;
    let fs = rate as f32;
    let mut out = vec![0f32; n];
    let mut phase = 0f32;
    let mut noise = 12345u32;
    let formants = [
        (700f32, 130f32),
        (1220.0, 70.0),
        (2600.0, 160.0),
        (3500.0, 250.0),
    ];
    let mut res = [[0f32; 2]; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / fs;
        let syl = (t * 3.1).fract();
        let voiced = syl < 0.62;
        let env = if syl < 0.7 {
            (std::f32::consts::PI * syl / 0.7).sin().powf(0.6)
        } else if syl < 0.82 {
            0.25
        } else {
            0.0
        };
        let f0 = 110.0 + 45.0 * (t * 1.7).sin() + 20.0 * (t * 5.3).sin();
        phase += f0 / fs;
        let mut src = 0f32;
        if phase >= 1.0 {
            phase -= 1.0;
            if voiced {
                src = 1.0;
            }
        }
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let nz = (noise >> 8) as f32 / 16_777_216.0 - 0.5;
        if !voiced {
            src = nz * 0.6;
        } else {
            src += nz * 0.02;
        }
        let shift = 1.0 + 0.15 * (t * 2.3).sin();
        let mut y = 0f32;
        for (k, &(f, bw)) in formants.iter().enumerate() {
            let r = (-std::f32::consts::PI * bw / fs).exp();
            let c = 2.0 * r * (2.0 * std::f32::consts::PI * f * shift / fs).cos();
            let v = src + c * res[k][0] - r * r * res[k][1];
            res[k][1] = res[k][0];
            res[k][0] = v;
            y += v * (1.0 - r) * if k == 0 { 1.0 } else { 0.6 };
        }
        *o = y * env * 0.5;
    }
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    for v in out.iter_mut() {
        *v *= 0.5 / peak;
    }
    out
}
