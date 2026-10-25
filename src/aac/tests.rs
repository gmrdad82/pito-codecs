use super::*;
use crate::testref::{Refs, assert_within, diff};

#[path = "refs.rs"]
mod refs;

fn signal(rate: u32, channels: u16, seconds: f32) -> AudioBuffer {
    let mut a = AudioBuffer::new(rate, channels);
    a.samples = refs::signal(rate, channels, seconds);
    a
}

fn decode_adts(data: &[u8]) -> AudioBuffer {
    let frames = split_adts(data).unwrap();
    let mut dec = Decoder::from_adts(&frames[0].0).unwrap();
    let mut out = AudioBuffer::new(dec.rate(), dec.channels());
    for (_, au) in frames {
        out.samples.extend(dec.decode_frame(au).unwrap().samples);
    }
    out
}

#[test]
fn decode_matches_ffmpeg_in_wave_channel_order() {
    let r = Refs::open(refs::AREA);
    for (channels, layout, _) in refs::STREAMS {
        let ours = decode_adts(&r.bytes(&refs::stream(channels)));
        assert_eq!(ours.channels, channels, "{layout}");
        let reference = r.f32s(&refs::decoded(channels));
        let got = refs::window::window(&ours.samples, channels as usize);
        assert_within(layout, &diff(&got, &reference), 1e-3, 1e-3);
    }
}

fn goertzel(x: &[f32], rate: u32, f: f32) -> f32 {
    let w = std::f32::consts::TAU * f / rate as f32;
    let k = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for &v in x {
        let s0 = v + k * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - k * s1 * s2).sqrt() / x.len() as f32 * 2.0
}

fn channel(a: &AudioBuffer, c: usize) -> Vec<f32> {
    a.samples
        .chunks_exact(a.channels as usize)
        .map(|f| f[c])
        .collect()
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

#[test]
fn encode_round_trip_keeps_loudness_and_spectrum() {
    let input = signal(48_000, 2, 3.0);
    let mut enc = Encoder::new(EncoderConfig {
        rate: 48_000,
        channels: 2,
        bitrate: 160_000,
    })
    .unwrap();
    let mut packets = enc.encode(&input).unwrap();
    packets.extend(enc.flush());
    let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
    let kbps = bytes as f64 * 8.0 / 3.0 / 1000.0;
    assert!((140.0..180.0).contains(&kbps), "{kbps} kbps");
    let mut dec = Decoder::new(&enc.extradata()).unwrap();
    let mut out = AudioBuffer::new(48_000, 2);
    for p in &packets {
        out.samples.extend(dec.decode(p).unwrap().samples);
    }
    let skip = enc.priming() as usize * 2;
    out.samples.drain(..skip);
    out.samples.truncate(input.samples.len());
    assert_eq!(out.samples.len(), input.samples.len());
    for c in 0..2 {
        let (a, b) = (channel(&input, c), channel(&out, c));
        let (a, b) = (&a[4800..a.len() - 4800], &b[4800..b.len() - 4800]);
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
        assert!((db(rms(a)) - db(rms(b))).abs() < 0.5, "loudness ch{c}");
        for f in [220.0 * (c as f32 + 1.0), 1000.0, 5000.0] {
            let (ga, gb) = (goertzel(a, 48_000, f), goertzel(b, 48_000, f));
            assert!(
                (db(ga) - db(gb)).abs() < 0.5,
                "{f} Hz ch{c}: {} vs {}",
                db(ga),
                db(gb)
            );
        }
        let err: Vec<f32> = a.iter().zip(b).map(|(x, y)| x - y).collect();
        let snr = db(rms(a)) - db(rms(&err));
        assert!(snr > 15.0, "snr ch{c} {snr} dB");
    }
}

#[test]
fn speech_resample_keeps_the_band_and_drops_aliases() {
    let tone = |f: f32| {
        let mut a = AudioBuffer::new(48_000, 2);
        for i in 0..48_000 {
            let s = 0.5 * (std::f32::consts::TAU * f * i as f32 / 48_000.0).sin();
            a.samples.extend([s, s]);
        }
        a
    };
    let low = mono(&resample(&tone(1000.0), 16_000));
    assert_eq!(
        (low.rate, low.channels, low.samples.len()),
        (16_000, 1, 16_000)
    );
    let g = goertzel(&low.samples[800..15_200], 16_000, 1000.0);
    assert!((db(g) - db(0.5)).abs() < 0.1, "{}", db(g));
    let high = mono(&resample(&tone(10_000.0), 16_000));
    let alias = goertzel(&high.samples[800..15_200], 16_000, 6000.0);
    assert!(db(alias) - db(0.5) < -60.0, "{}", db(alias));
    let mut r = Resampler::new(48_000, 44_100, 2);
    let input = tone(1000.0);
    let mut streamed = AudioBuffer::new(44_100, 2);
    for part in input.samples.chunks(2 * 999) {
        let mut b = AudioBuffer::new(48_000, 2);
        b.samples.extend_from_slice(part);
        streamed.samples.extend(r.process(&b).samples);
    }
    streamed.samples.extend(r.flush().samples);
    assert_eq!(streamed.samples, resample(&input, 44_100).samples);
}
