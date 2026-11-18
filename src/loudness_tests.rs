use crate::loudness::LoudnessMeter;
use ebur128::{EbuR128, Mode};

fn signal(rate: u32, channels: usize, seconds: f64, seed: u32) -> Vec<f32> {
    let frames = (f64::from(rate) * seconds) as usize;
    let mut s = seed;
    let mut out = Vec::with_capacity(frames * channels);
    for t in 0..frames {
        let x = t as f64 / f64::from(rate);
        let level = if x < seconds * 0.4 {
            0.5
        } else if x < seconds * 0.6 {
            0.02
        } else {
            0.25
        };
        for c in 0..channels {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (f64::from(s >> 8) / f64::from(1u32 << 24) - 0.5) * 0.2;
            let tone = (x * 2.0 * std::f64::consts::PI * (220.0 + 110.0 * c as f64)).sin();
            let burst = if t % 7919 < 3 { 0.9 } else { 0.0 };
            out.push(((tone * level + noise * level + burst) as f32).clamp(-1.0, 1.0));
        }
    }
    out
}

const MODE: Mode = Mode::I
    .union(Mode::M)
    .union(Mode::S)
    .union(Mode::SAMPLE_PEAK)
    .union(Mode::TRUE_PEAK);

fn near(a: f64, b: f64, tolerance: f64, what: &str) {
    if a.is_infinite() || b.is_infinite() {
        assert_eq!(a, b, "{what}");
    } else {
        assert!((a - b).abs() <= tolerance, "{what}: {a} against {b}");
    }
}

#[test]
fn every_reading_matches_ebur128_across_rates_channels_and_block_sizes() {
    for (rate, channels, block) in [
        (48_000u32, 1usize, 4800usize),
        (44_100, 2, 4410),
        (48_000, 2, 1000),
        (96_000, 2, 9600),
        (192_000, 2, 19_200),
        (44_100, 3, 441),
        (48_000, 5, 4800),
        (48_000, 6, 4800),
        (22_050, 2, 777),
    ] {
        let tag = format!("{rate} Hz x{channels} blocks of {block}");
        let samples = signal(rate, channels, 7.3, rate + channels as u32);
        let mut theirs = EbuR128::new(channels as u32, rate, MODE).unwrap();
        let mut ours = LoudnessMeter::new(rate, channels as u16)
            .unwrap()
            .windows()
            .unwrap();
        for chunk in samples.chunks(block * channels) {
            theirs.add_frames_f32(chunk).unwrap();
            ours.add(chunk).unwrap();
            near(
                ours.momentary_now().unwrap(),
                theirs.loudness_momentary().unwrap(),
                1e-6,
                &format!("{tag}: momentary"),
            );
            near(
                ours.short_term_now().unwrap(),
                theirs.loudness_shortterm().unwrap(),
                1e-6,
                &format!("{tag}: short-term"),
            );
            for ms in [1u32, 50, 400, 1234, 3000] {
                if ms <= 3000 {
                    near(
                        ours.window(ms).unwrap(),
                        theirs.loudness_window(ms).unwrap(),
                        1e-6,
                        &format!("{tag}: window {ms}"),
                    );
                }
            }
        }
        near(
            ours.integrated(),
            theirs.loudness_global().unwrap(),
            0.02,
            &format!("{tag}: integrated"),
        );
        for c in 0..channels {
            let sp = theirs.sample_peak(c as u32).unwrap();
            let tp = theirs.true_peak(c as u32).unwrap();
            near(
                ours.sample_peak_of(c).unwrap(),
                sp,
                1e-7,
                &format!("{tag}: sample peak {c}"),
            );
            near(
                ours.true_peak_of(c).unwrap(),
                tp,
                3e-2 * tp,
                &format!("{tag}: true peak {c}"),
            );
        }
    }
}

#[test]
fn a_clip_shorter_than_a_gating_block_reads_through_the_window_the_way_a_short_voice_clip_is_measured()
 {
    let rate = 48_000;
    let samples = signal(rate, 1, 0.31, 5);
    let mut theirs = EbuR128::new(1, rate, Mode::I | Mode::M | Mode::SAMPLE_PEAK).unwrap();
    let mut ours = LoudnessMeter::new(rate, 1).unwrap().windows().unwrap();
    theirs.add_frames_f32(&samples).unwrap();
    ours.add(&samples).unwrap();
    assert_eq!(theirs.loudness_global().unwrap(), f64::NEG_INFINITY);
    assert_eq!(ours.integrated(), f64::NEG_INFINITY);
    let ms = (samples.len() as f64 / f64::from(rate) * 1000.0) as u32;
    near(
        ours.window(ms.clamp(1, 400)).unwrap(),
        theirs.loudness_window(ms.clamp(1, 400)).unwrap(),
        1e-6,
        "whole-clip window",
    );
    near(
        ours.sample_peak_of(0).unwrap(),
        theirs.sample_peak(0).unwrap(),
        1e-9,
        "sample peak",
    );
}

#[test]
fn silence_reads_as_minus_infinity_and_a_zero_peak_in_both() {
    let samples = vec![0.0f32; 48_000 * 2 * 2];
    let mut theirs = EbuR128::new(2, 48_000, MODE).unwrap();
    let mut ours = LoudnessMeter::new(48_000, 2).unwrap().windows().unwrap();
    theirs.add_frames_f32(&samples).unwrap();
    ours.add(&samples).unwrap();
    assert_eq!(ours.integrated(), theirs.loudness_global().unwrap());
    assert_eq!(
        ours.momentary_now().unwrap(),
        theirs.loudness_momentary().unwrap()
    );
    assert_eq!(
        ours.short_term_now().unwrap(),
        theirs.loudness_shortterm().unwrap()
    );
    assert_eq!(ours.true_peak_of(1).unwrap(), 0.0);
    assert_eq!(theirs.true_peak(1).unwrap(), 0.0);
}

#[test]
fn what_a_meter_refuses_it_refuses_in_a_line() {
    assert!(LoudnessMeter::new(0, 2).is_err());
    assert!(LoudnessMeter::new(48_000, 0).is_err());
    let mut plain = LoudnessMeter::new(48_000, 2).unwrap();
    assert!(plain.window(400).is_err());
    assert!(plain.add(&[0.0; 3]).is_err());
    let windowed = LoudnessMeter::new(48_000, 2).unwrap().windows().unwrap();
    assert!(windowed.window(0).is_err());
    assert!(LoudnessMeter::new(5_000_000, 1).unwrap().windows().is_err());
    assert!(windowed.window(3001).is_err());
    assert!(windowed.sample_peak_of(2).is_none());
    plain.add(&[]).unwrap();
}

#[test]
fn whole_buffers_and_the_interleaved_slice_give_the_same_meter() {
    let samples = signal(48_000, 2, 3.0, 9);
    let mut a = LoudnessMeter::new(48_000, 2).unwrap();
    let mut b = LoudnessMeter::new(48_000, 2).unwrap();
    a.add(&samples).unwrap();
    b.process(&crate::media::AudioBuffer {
        samples: samples.clone(),
        rate: 48_000,
        channels: 2,
    })
    .unwrap();
    assert_eq!(a.summary(), b.summary());
    let tracked = {
        let mut c = LoudnessMeter::new(48_000, 2).unwrap().windows().unwrap();
        c.add(&samples).unwrap();
        c.summary()
    };
    assert_eq!(tracked, a.summary());
}
