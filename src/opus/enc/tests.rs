use super::*;
use crate::opus::StreamDecoder;

pub fn signal(seconds: f32, channels: usize) -> Vec<f32> {
    let n = (48000.0 * seconds) as usize;
    let mut seed = 0x1234_5678u32;
    let mut out = Vec::with_capacity(n * channels);
    for i in 0..n {
        let t = i as f32 / 48000.0;
        let env = if (i / 12000) % 4 == 3 { 0.2 } else { 1.0 };
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

pub fn snr_db(reference: &[f32], decoded: &[f32]) -> f32 {
    let (mut s, mut e) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(decoded) {
        s += (*a as f64).powi(2);
        e += (*a as f64 - *b as f64).powi(2);
    }
    (10.0 * (s / e.max(1e-20)).log10()) as f32
}

pub fn encode_all(cfg: EncoderConfig, pcm: &[f32]) -> (Encoder, Vec<Packet>) {
    let mut enc = Encoder::new(cfg).unwrap();
    let mut buf = AudioBuffer::new(48000, cfg.channels);
    buf.samples = pcm.to_vec();
    let mut packets = enc.encode(&buf).unwrap();
    packets.extend(enc.flush().unwrap());
    (enc, packets)
}

#[test]
fn own_decoder_tracks_the_range_coder_and_reconstructs() {
    for (channels, bitrate) in [(1u16, 64_000u32), (2, 128_000), (2, 48_000), (1, 32_000)] {
        let c = channels as usize;
        let pcm = signal(1.0, c);
        let mut enc = Encoder::new(EncoderConfig::new(channels, bitrate)).unwrap();
        let mut dec = StreamDecoder::new(48000, c).unwrap();
        let mut buf = AudioBuffer::new(48000, channels);
        let mut out = Vec::new();
        let mut packets = Vec::new();
        for chunk in pcm.chunks(4410 * c) {
            buf.samples = chunk.to_vec();
            packets.extend(enc.encode(&buf).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        let mut enc2 = Encoder::new(EncoderConfig::new(channels, bitrate)).unwrap();
        buf.samples = pcm.clone();
        let mut packets2 = enc2.encode(&buf).unwrap();
        packets2.extend(enc2.flush().unwrap());
        assert_eq!(packets, packets2);
        let mut ranges = Vec::new();
        {
            let mut e3 = Encoder::new(EncoderConfig::new(channels, bitrate)).unwrap();
            buf.samples = pcm.clone();
            for chunk in pcm.chunks(960 * c) {
                buf.samples = chunk.to_vec();
                for _ in e3.encode(&buf).unwrap() {
                    ranges.push(e3.final_range());
                }
            }
        }
        for (k, p) in packets.iter().enumerate() {
            dec.decode_interleaved(Some(&p.data), &mut out).unwrap();
            if k < ranges.len() {
                assert_eq!(dec.final_range(), ranges[k], "packet {k} c{c} {bitrate}");
            }
        }
        let skip = enc.pre_skip() as usize * c;
        let snr = snr_db(&pcm, &out[skip..skip + pcm.len()]);
        eprintln!("c{c} {bitrate} snr {snr:.2} dB, {} packets", packets.len());
        assert!(snr > 10.0, "snr {snr}");
    }
}

fn crc(data: &[u8]) -> u32 {
    let mut c = 0u32;
    for &b in data {
        c ^= (b as u32) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04c1_1db7
            } else {
                c << 1
            };
        }
    }
    c
}

fn page(out: &mut Vec<u8>, flags: u8, granule: i64, seq: u32, packets: &[&[u8]]) {
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let mut lacing = Vec::new();
    for p in packets {
        let mut n = p.len();
        while n >= 255 {
            lacing.push(255u8);
            n -= 255;
        }
        lacing.push(n as u8);
    }
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    for p in packets {
        out.extend_from_slice(p);
    }
    let c = crc(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&c.to_le_bytes());
}

pub fn ogg(enc: &Encoder, packets: &[Packet], samples: usize) -> Vec<u8> {
    let mut out = Vec::new();
    page(&mut out, 2, 0, 0, &[&enc.codec_private()]);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&5u32.to_le_bytes());
    tags.extend_from_slice(b"pito ");
    tags.extend_from_slice(&0u32.to_le_bytes());
    page(&mut out, 0, 0, 1, &[&tags]);
    let end = (samples + enc.pre_skip() as usize) as i64;
    for (k, p) in packets.iter().enumerate() {
        let last = k + 1 == packets.len();
        let g = if last {
            end
        } else {
            (k as i64 + 1) * FRAME as i64
        };
        page(
            &mut out,
            if last { 4 } else { 0 },
            g,
            k as u32 + 2,
            &[&p.data],
        );
    }
    out
}

pub fn ffmpeg_decode(path: &std::path::Path, channels: usize) -> Option<Vec<f32>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-c:a", "libopus", "-i"])
        .arg(path)
        .args([
            "-f",
            "f32le",
            "-ac",
            &channels.to_string(),
            "-ar",
            "48000",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        panic!("ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    }
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

fn tmp_dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/opusenc");
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn libopus_decodes_our_ogg_with_matching_level() {
    for (channels, bitrate) in [(2u16, 128_000u32), (1, 64_000), (2, 64_000)] {
        let c = channels as usize;
        let pcm = signal(2.0, c);
        let (enc, packets) = encode_all(EncoderConfig::new(channels, bitrate), &pcm);
        let path = tmp_dir().join(format!("enc_{c}_{bitrate}.opus"));
        std::fs::write(&path, ogg(&enc, &packets, pcm.len() / c)).unwrap();
        let Some(dec) = ffmpeg_decode(&path, c) else {
            return;
        };
        assert_eq!(dec.len(), pcm.len(), "trimmed length");
        let snr = snr_db(&pcm, &dec);
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let level = 20.0 * (rms(&dec) / rms(&pcm)).log10();
        eprintln!("libopus c{c} {bitrate}: snr {snr:.2} dB, level {level:+.2} dB");
        assert!(snr > 12.0, "snr {snr}");
        assert!(level.abs() < 0.5, "level {level}");
    }
}
