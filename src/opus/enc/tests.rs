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

pub fn ogg(cfg: EncoderConfig, pcm: &[f32]) -> Vec<u8> {
    let mut buf = AudioBuffer::new(48000, cfg.channels);
    buf.samples = pcm.to_vec();
    encode_ogg(cfg, &buf).unwrap()
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
        let file = ogg(EncoderConfig::new(channels, bitrate), &pcm);
        let ours = crate::opus::ogg::decode(&file).unwrap();
        assert_eq!(ours.samples.len(), pcm.len(), "our trimmed length");
        assert!(snr_db(&pcm, &ours.samples) > 12.0);
        let path = tmp_dir().join(format!("enc_{c}_{bitrate}.opus"));
        std::fs::write(&path, file).unwrap();
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

fn read_f32(path: &std::path::Path) -> Option<Vec<f32>> {
    let b = std::fs::read(path).ok()?;
    Some(
        b.chunks_exact(4)
            .map(|v| f32::from_le_bytes([v[0], v[1], v[2], v[3]]))
            .collect(),
    )
}

fn seg_snr(a: &[f32], b: &[f32], c: usize) -> f32 {
    let w = 960 * c;
    let mut acc = 0f64;
    let mut n = 0;
    for (x, y) in a.chunks(w).zip(b.chunks(w)) {
        let s: f64 = x.iter().map(|v| (*v as f64).powi(2)).sum();
        if s < 1e-6 * w as f64 {
            continue;
        }
        let e: f64 = x
            .iter()
            .zip(y)
            .map(|(p, q)| (*p as f64 - *q as f64).powi(2))
            .sum();
        acc += (10.0 * (s / e.max(1e-20)).log10()).clamp(-10.0, 60.0);
        n += 1;
    }
    (acc / n.max(1) as f64) as f32
}

#[test]
#[ignore]
fn measure_against_libopus() {
    let dir = tmp_dir();
    for name in ["resilience", "solstice", "music"] {
        let src = dir.join(format!("{name}.f32"));
        let Some(pcm) = read_f32(&src) else {
            continue;
        };
        let c = 2usize;
        let secs = pcm.len() as f64 / (48000.0 * c as f64);
        for bitrate in [64_000u32, 96_000, 128_000] {
            let mut line = format!("{name} {} kbps:", bitrate / 1000);
            for vbr in [false, true] {
                let mut cfg = EncoderConfig::new(c as u16, bitrate);
                cfg.vbr = vbr;
                let t = std::time::Instant::now();
                let file = ogg(cfg, &pcm);
                let ours = t.elapsed().as_secs_f64();
                let kbps = file.len() as f64 * 8.0 / secs / 1000.0;
                let path = dir.join(format!("{name}_{bitrate}_{vbr}.opus"));
                std::fs::write(&path, file).unwrap();
                let dec = ffmpeg_decode(&path, c).unwrap();
                line += &format!(
                    " ours {} {:.0}x {:.1}k snr {:.2} seg {:.2} |",
                    if vbr { "vbr" } else { "cbr" },
                    secs / ours,
                    kbps,
                    snr_db(&pcm, &dec),
                    seg_snr(&pcm, &dec, c)
                );
            }
            for vbr in ["off", "on"] {
                let lib = dir.join(format!("{name}_{bitrate}_libopus_{vbr}.opus"));
                let t = std::time::Instant::now();
                let st = std::process::Command::new("ffmpeg")
                    .args([
                        "-v", "error", "-y", "-threads", "1", "-f", "f32le", "-ar", "48000", "-ac",
                        "2", "-i",
                    ])
                    .arg(&src)
                    .args(["-c:a", "libopus", "-b:a", &bitrate.to_string(), "-vbr", vbr])
                    .arg(&lib)
                    .status()
                    .unwrap();
                let theirs = t.elapsed().as_secs_f64();
                assert!(st.success());
                let kbps = std::fs::metadata(&lib).unwrap().len() as f64 * 8.0 / secs / 1000.0;
                let ldec = ffmpeg_decode(&lib, c).unwrap();
                let n = pcm.len().min(ldec.len());
                line += &format!(
                    " libopus {vbr} {:.0}x {:.1}k snr {:.2} seg {:.2} |",
                    secs / theirs,
                    kbps,
                    snr_db(&pcm[..n], &ldec[..n]),
                    seg_snr(&pcm[..n], &ldec[..n], c)
                );
            }
            eprintln!("{line}");
        }
    }
}

#[test]
fn parallel_chunks_match_streaming_bytes() {
    let c = 2usize;
    let pcm = signal(11.0, c);
    let cfg = EncoderConfig::new(c as u16, 96_000);
    let mut buf = AudioBuffer::new(48000, 2);
    let mut seq = Encoder::new(cfg).unwrap();
    let mut a = Vec::new();
    for chunk in pcm.chunks(1000 * c) {
        buf.samples = chunk.to_vec();
        a.extend(seq.encode(&buf).unwrap());
    }
    a.extend(seq.flush().unwrap());
    let mut par = Encoder::new(cfg).unwrap();
    buf.samples = pcm.clone();
    let mut b = par.encode(&buf).unwrap();
    b.extend(par.flush().unwrap());
    assert_eq!(a.len(), b.len());
    for (k, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "packet {k}");
    }
    let mut dec = StreamDecoder::new(48000, c).unwrap();
    let mut out = Vec::new();
    for p in &b {
        dec.decode_interleaved(Some(&p.data), &mut out).unwrap();
    }
    let skip = par.pre_skip() as usize * c;
    let out = &out[skip..skip + pcm.len()];
    let at = 256 * FRAME * c;
    let w = 4 * FRAME * c;
    let around = snr_db(&pcm[at - w..at + w], &out[at - w..at + w]);
    let whole = snr_db(&pcm, out);
    assert!(around > whole - 3.0, "chunk boundary {around} vs {whole}");
}

#[test]
fn extremes_stay_in_sync() {
    let mut loud = signal(0.5, 2);
    for (i, v) in loud.iter_mut().enumerate() {
        *v *= 8.0;
        if i % 997 == 0 {
            *v = f32::NAN;
        }
        if i % 1499 == 0 {
            *v = f32::INFINITY;
        }
    }
    let mut quiet = signal(0.5, 2);
    for v in quiet.iter_mut() {
        *v *= 1e-6;
    }
    let mut gaps = signal(0.5, 2);
    for v in gaps[9600..30000].iter_mut() {
        *v = 0.0;
    }
    for pcm in [&loud, &quiet, &gaps] {
        for (channels, bitrate, vbr) in [
            (1u16, 6_000u32, false),
            (2, 6_000, true),
            (1, 510_000, false),
            (2, 510_000, true),
            (2, 24_000, true),
        ] {
            let c = channels as usize;
            let src: Vec<f32> = if c == 1 {
                pcm.chunks(2).map(|p| p[0]).collect()
            } else {
                pcm.to_vec()
            };
            let mut cfg = EncoderConfig::new(channels, bitrate);
            cfg.vbr = vbr;
            let mut enc = Encoder::new(cfg).unwrap();
            let mut dec = StreamDecoder::new(48000, c).unwrap();
            let mut buf = AudioBuffer::new(48000, channels);
            let mut out = Vec::new();
            for chunk in src.chunks(960 * c) {
                buf.samples = chunk.to_vec();
                for p in enc.encode(&buf).unwrap() {
                    assert!(p.data.len() <= 1276);
                    dec.decode_interleaved(Some(&p.data), &mut out).unwrap();
                    assert_eq!(dec.final_range(), enc.final_range(), "c{c} {bitrate}");
                }
            }
            assert!(out.iter().all(|v| v.is_finite()));
        }
    }
}

#[test]
#[ignore]
fn speed() {
    let Some(pcm) = read_f32(&tmp_dir().join("resilience.f32")) else {
        return;
    };
    let secs = pcm.len() as f64 / 96000.0;
    let mut buf = AudioBuffer::new(48000, 2);
    for bitrate in [64_000u32, 128_000, 256_000] {
        let mut best = f64::MAX;
        for _ in 0..3 {
            let mut enc = Encoder::new(EncoderConfig::new(2, bitrate)).unwrap();
            let t = std::time::Instant::now();
            for chunk in pcm.chunks(48000 * 2) {
                buf.samples.clear();
                buf.samples.extend_from_slice(chunk);
                std::hint::black_box(enc.encode(&buf).unwrap());
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        eprintln!(
            "single thread {} kbps: {:.0}x real time",
            bitrate / 1000,
            secs / best
        );
    }
}
