use super::*;
use crate::opus::StreamDecoder;
use crate::opus::tests::refs;
use crate::testref::{self, Refs};

pub use refs::signal;

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

fn tmp_dir() -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/opusenc");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn rms(v: &[f32]) -> f32 {
    (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
}

fn libopus_heard(r: &Refs, file: &str, bytes: &[u8], pcm: &[f32], c: usize) -> (f32, f32) {
    r.assert_digest(file, &testref::sha256(bytes));
    let dec = r.f32s(&refs::decoded(file));
    let pcm = refs::window::window(pcm, c);
    assert_eq!(dec.len(), pcm.len(), "{file}: trimmed length");
    let level = 20.0 * (rms(&dec) / rms(&pcm)).log10();
    (snr_db(&pcm, &dec), level)
}

#[test]
fn libopus_decodes_our_ogg_with_matching_level() {
    let r = Refs::open(refs::AREA);
    for (channels, bitrate) in refs::OURS {
        let c = channels as usize;
        let pcm = signal(refs::SECONDS, c);
        let file = ogg(EncoderConfig::new(channels, bitrate), &pcm);
        assert_eq!(file, refs::encode_ours(channels, bitrate).unwrap());
        let ours = crate::opus::ogg::decode(&file).unwrap();
        assert_eq!(ours.samples.len(), pcm.len(), "our trimmed length");
        assert!(snr_db(&pcm, &ours.samples) > 12.0);
        let (snr, level) = libopus_heard(&r, &refs::ours(channels, bitrate), &file, &pcm, c);
        assert!(snr > 12.0, "c{c} {bitrate}: snr {snr}");
        assert!(level.abs() < 0.5, "c{c} {bitrate}: level {level}");
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
            if c == 2 {
                buf.samples = vec![0.0; 3];
                assert!(enc.encode(&buf).is_err());
            }
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

#[test]
fn voip_speech_goes_through_silk_and_lines_up_in_libopus() {
    let r = Refs::open(refs::AREA);
    for (channels, bitrate) in refs::SILK {
        let c = channels as usize;
        let pcm = refs::speech(c);
        let mut cfg = EncoderConfig::new(channels, bitrate);
        cfg.application = Application::Voip;
        let file = ogg(cfg, &pcm);
        assert_eq!(file, refs::encode_silk(channels, bitrate).unwrap());
        let ours = crate::opus::ogg::decode(&file).unwrap();
        assert_eq!(ours.samples.len(), pcm.len());
        let (snr, _) = libopus_heard(&r, &refs::silk(channels, bitrate), &file, &pcm, c);
        assert!(snr > 4.0, "c{c} {bitrate}: snr {snr}");
    }
}
