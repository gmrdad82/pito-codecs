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
    .union(Mode::LRA)
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
        (88_200, 2, 8820),
        (95_999, 1, 4000),
        (48_000, 4, 4800),
    ] {
        let tag = format!("{rate} Hz x{channels} blocks of {block}");
        let samples = signal(rate, channels, 7.3, rate + channels as u32);
        let mut theirs = EbuR128::new(channels as u32, rate, MODE).unwrap();
        let mut ours = LoudnessMeter::ebur128(rate, channels as u16)
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
            1e-6,
            &format!("{tag}: integrated"),
        );
        near(
            ours.range(),
            theirs.loudness_range().unwrap(),
            1e-6,
            &format!("{tag}: range"),
        );
        near(
            ours.threshold(),
            theirs.relative_threshold().unwrap(),
            1e-6,
            &format!("{tag}: threshold"),
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
                1e-6,
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
    let mut ours = LoudnessMeter::ebur128(rate, 1).unwrap().windows().unwrap();
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
    let mut ours = LoudnessMeter::ebur128(48_000, 2)
        .unwrap()
        .windows()
        .unwrap();
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
    assert!(LoudnessMeter::ebur128(0, 2).is_err());
    assert!(LoudnessMeter::ebur128(48_000, 0).is_err());
    assert!(LoudnessMeter::new(0, 2).is_err());
    assert!(LoudnessMeter::new(48_000, 0).is_err());
    let mut plain = LoudnessMeter::ebur128(48_000, 2).unwrap();
    assert!(plain.window(400).is_err());
    assert!(plain.add(&[0.0; 3]).is_err());
    let windowed = LoudnessMeter::ebur128(48_000, 2)
        .unwrap()
        .windows()
        .unwrap();
    assert!(windowed.window(0).is_err());
    assert!(
        LoudnessMeter::ebur128(5_000_000, 1)
            .unwrap()
            .windows()
            .is_err()
    );
    assert!(windowed.window(3001).is_err());
    assert!(windowed.sample_peak_of(2).is_none());
    plain.add(&[]).unwrap();
}

#[test]
fn whole_buffers_and_the_interleaved_slice_give_the_same_meter() {
    let samples = signal(48_000, 2, 3.0, 9);
    let mut a = LoudnessMeter::ebur128(48_000, 2).unwrap();
    let mut b = LoudnessMeter::ebur128(48_000, 2).unwrap();
    a.add(&samples).unwrap();
    b.process(&crate::media::AudioBuffer {
        samples: samples.clone(),
        rate: 48_000,
        channels: 2,
    })
    .unwrap();
    assert_eq!(a.summary(), b.summary());
    let tracked = {
        let mut c = LoudnessMeter::ebur128(48_000, 2)
            .unwrap()
            .windows()
            .unwrap();
        c.add(&samples).unwrap();
        c.summary()
    };
    assert_eq!(tracked, a.summary());
}

fn tone(rate: u32, hz: f64, amplitude: f64, phase_deg: f64, seconds: f64) -> Vec<f32> {
    let frames = (seconds * f64::from(rate)) as usize;
    let phase = phase_deg.to_radians();
    let mut out = Vec::with_capacity(frames * 2);
    for n in 0..frames {
        let v = (amplitude
            * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate) + phase).sin())
            as f32;
        out.extend([v, v]);
    }
    out
}

fn hot(rate: u32, seconds: f64) -> Vec<f32> {
    let frames = (seconds * f64::from(rate)) as usize;
    let mut out = Vec::with_capacity(frames * 2);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let pi2 = 2.0 * std::f64::consts::PI;
    for n in 0..frames {
        let t = n as f64 / f64::from(rate);
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let noise = (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
        let kick = (-((t * 2.0).fract() * 18.0)).exp() * (pi2 * 55.0 * t).sin();
        let lead = (pi2 * 11_997.0 * t).sin() * 0.6 + (pi2 * 7_001.0 * t).sin() * 0.5;
        let l = 1.3 * kick + lead + 0.2 * noise;
        let r = 1.1 * kick + lead - 0.2 * noise;
        out.push(l.clamp(-1.0, 1.0) as f32);
        out.push(r.clamp(-1.0, 1.0) as f32);
    }
    out
}

fn true_peaks(rate: u32, channels: u32, samples: &[f32]) -> (Vec<f64>, Vec<f64>) {
    let mut theirs = EbuR128::new(channels, rate, Mode::TRUE_PEAK).unwrap();
    theirs.add_frames_f32(samples).unwrap();
    let mut ours = LoudnessMeter::ebur128(rate, channels as u16).unwrap();
    ours.add(samples).unwrap();
    (
        (0..channels)
            .map(|c| theirs.true_peak(c).unwrap())
            .collect(),
        (0..channels as usize)
            .map(|c| ours.true_peak_of(c).unwrap())
            .collect(),
    )
}

#[test]
fn the_true_peaks_pfxs_peak_tests_ask_for_read_as_ebur128_reads_them() {
    let rate = 48_000;
    let mut cases = vec![
        (
            "quarter rate at 0 degrees",
            tone(rate, 12_000.0, 0.5, 0.0, 1.0),
        ),
        (
            "quarter rate at 45 degrees",
            tone(rate, 12_000.0, 0.5, 45.0, 1.0),
        ),
        (
            "quarter rate at 60 degrees",
            tone(rate, 12_000.0, 0.5, 60.0, 1.0),
        ),
        (
            "quarter rate at 67.5 degrees",
            tone(rate, 12_000.0, 0.5, 67.5, 1.0),
        ),
        (
            "quarter rate with full scale samples",
            tone(rate, 12_000.0, 2f64.sqrt(), 45.0, 1.0),
        ),
        ("1 kHz", tone(rate, 1_000.0, 1.0, 30.0, 1.0)),
        ("17 kHz", tone(rate, 17_000.0, 1.0, 0.0, 1.0)),
        ("9.6 kHz", tone(rate, 9_600.0, 1.0, 18.0, 1.0)),
        ("a hot mix", hot(rate, 3.0)),
    ];
    let mut click = vec![0.0f32; 9_600];
    click[4_800] = 1.0;
    click[4_801] = 1.0;
    cases.push(("a full-scale click", click));
    for (name, signal) in cases {
        let (theirs, ours) = true_peaks(rate, 2, &signal);
        for (c, (a, b)) in ours.iter().zip(&theirs).enumerate() {
            assert!(
                (a - b).abs() <= 1e-6,
                "{name}, channel {c}: {a} against {b}"
            );
        }
    }
}

#[test]
fn the_true_peak_follows_ebur128_at_every_rate_boundary_and_block_size() {
    for rate in [
        8_000u32, 22_050, 44_100, 48_000, 88_200, 95_999, 96_000, 176_400, 191_999, 192_000,
        384_000,
    ] {
        let signal = tone(rate, f64::from(rate) / 4.0 - 7.0, 0.8, 33.0, 0.3);
        let (theirs, ours) = true_peaks(rate, 2, &signal);
        assert!(
            (ours[0] - theirs[0]).abs() <= 1e-6,
            "{rate} Hz: {} against {}",
            ours[0],
            theirs[0]
        );
        let mut chunked = LoudnessMeter::ebur128(rate, 2).unwrap();
        for piece in signal.chunks(2 * 97) {
            chunked.add(piece).unwrap();
        }
        assert_eq!(chunked.true_peak_of(0), {
            let mut whole = LoudnessMeter::ebur128(rate, 2).unwrap();
            whole.add(&signal).unwrap();
            whole.true_peak_of(0)
        });
    }
}

#[test]
fn the_chime_reading_matches_ebur128_in_tenth_of_a_second_blocks_with_a_second_of_padding() {
    let rate = 48_000u32;
    let mut chime = Vec::new();
    for (i, level) in [0.02, 0.15, 0.3, 0.12, 0.05, 0.01].into_iter().enumerate() {
        chime.extend(tone(
            rate,
            440.0 * (i + 1) as f64,
            level,
            11.0 * i as f64,
            0.55,
        ));
    }
    chime.extend(std::iter::repeat_n(0.0, rate as usize * 2));
    let mut theirs = EbuR128::new(2, rate, Mode::S | Mode::TRUE_PEAK).unwrap();
    let mut ours = LoudnessMeter::ebur128(rate, 2).unwrap();
    let (mut short_theirs, mut short_ours) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for block in chime.chunks(rate as usize / 10 * 2) {
        theirs.add_frames_f32(block).unwrap();
        ours.add(block).unwrap();
        short_theirs = short_theirs.max(theirs.loudness_shortterm().unwrap());
        short_ours = short_ours.max(ours.short_term());
    }
    near(short_ours, short_theirs, 1e-6, "short-term maximum");
    near(
        ours.summary().short_term_max,
        short_theirs,
        1e-6,
        "summary maximum",
    );
    for c in 0..2 {
        near(
            ours.true_peak_of(c).unwrap(),
            theirs.true_peak(c as u32).unwrap(),
            1e-6,
            "chime true peak",
        );
    }
}

#[test]
fn the_range_equals_ebur128s_on_programmes_with_quiet_and_loud_passages() {
    let rate = 48_000u32;
    for (seed, seconds) in [(1u32, 12.0f64), (2, 31.0), (3, 63.5)] {
        let mut samples = Vec::new();
        for (i, level) in [0.4, 0.02, 0.2, 0.001, 0.6, 0.05, 0.3]
            .into_iter()
            .enumerate()
        {
            let part = signal(rate, 2, seconds / 7.0, seed * 10 + i as u32);
            samples.extend(part.iter().map(|v| v * level as f32));
        }
        let mut theirs = EbuR128::new(2, rate, MODE).unwrap();
        let mut ours = LoudnessMeter::ebur128(rate, 2).unwrap();
        for chunk in samples.chunks(2 * 2048) {
            theirs.add_frames_f32(chunk).unwrap();
            ours.add(chunk).unwrap();
        }
        near(
            ours.range(),
            theirs.loudness_range().unwrap(),
            1e-6,
            "range",
        );
        near(
            ours.integrated(),
            theirs.loudness_global().unwrap(),
            1e-6,
            "integrated",
        );
        assert!(
            ours.range() > 1.0,
            "the programme has a range: {}",
            ours.range()
        );
    }
    let steady = tone(rate, 997.0, 0.25, 0.0, 12.0);
    let mut theirs = EbuR128::new(2, rate, MODE).unwrap();
    let mut ours = LoudnessMeter::ebur128(rate, 2).unwrap();
    theirs.add_frames_f32(&steady).unwrap();
    ours.add(&steady).unwrap();
    near(
        ours.range(),
        theirs.loudness_range().unwrap(),
        1e-6,
        "steady range",
    );
    assert!(ours.range() < 0.01);
}

#[test]
fn a_clip_under_three_seconds_has_no_range_in_either() {
    let samples = signal(48_000, 1, 2.9, 4);
    let mut theirs = EbuR128::new(1, 48_000, MODE).unwrap();
    let mut ours = LoudnessMeter::ebur128(48_000, 1).unwrap();
    theirs.add_frames_f32(&samples).unwrap();
    ours.add(&samples).unwrap();
    assert_eq!(theirs.loudness_range().unwrap(), 0.0);
    assert_eq!(ours.range(), 0.0);
    near(
        ours.threshold(),
        theirs.relative_threshold().unwrap(),
        1e-6,
        "threshold",
    );
}

#[test]
fn the_default_weights_are_ebur128s_up_to_six_channels_and_eight_follow_set_weights() {
    for channels in [1usize, 2, 3, 4, 5, 6] {
        let samples = signal(48_000, channels, 4.0, channels as u32);
        let mut theirs = EbuR128::new(channels as u32, 48_000, MODE).unwrap();
        let mut ours = LoudnessMeter::ebur128(48_000, channels as u16).unwrap();
        theirs.add_frames_f32(&samples).unwrap();
        ours.add(&samples).unwrap();
        near(
            ours.integrated(),
            theirs.loudness_global().unwrap(),
            1e-6,
            "integrated",
        );
    }
    let samples = signal(48_000, 8, 4.0, 8);
    let mut theirs = EbuR128::new(8, 48_000, MODE).unwrap();
    let mut ours = LoudnessMeter::ebur128(48_000, 8).unwrap();
    ours.set_weights(&[1.0, 1.0, 1.0, 0.0, 1.41, 1.41, 0.0, 0.0]);
    theirs.add_frames_f32(&samples).unwrap();
    ours.add(&samples).unwrap();
    near(
        ours.integrated(),
        theirs.loudness_global().unwrap(),
        1e-6,
        "eight channels",
    );
}

#[test]
fn a_voice_clip_is_levelled_from_the_readings_inbox0_asks_for() {
    let rate = 48_000u32;
    let samples: Vec<f32> = signal(rate, 1, 5.0, 77);
    let mut theirs = EbuR128::new(1, rate, Mode::I | Mode::SAMPLE_PEAK | Mode::M).unwrap();
    let mut ours = LoudnessMeter::ebur128(rate, 1).unwrap().windows().unwrap();
    theirs.add_frames_f32(&samples).unwrap();
    ours.add(&samples).unwrap();
    assert_eq!(
        ours.sample_peak_of(0).unwrap(),
        theirs.sample_peak(0).unwrap()
    );
    near(
        ours.integrated(),
        theirs.loudness_global().unwrap(),
        1e-6,
        "integrated",
    );
    near(
        ours.momentary_now().unwrap(),
        theirs.loudness_momentary().unwrap(),
        1e-6,
        "momentary",
    );
}

#[test]
fn hostile_samples_and_odd_rates_never_panic_in_any_reading() {
    let mut state = 0x9e37_79b9u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    for rate in [
        1u32, 7, 99, 8_000, 44_100, 95_999, 96_000, 191_999, 192_000, 400_000,
    ] {
        for channels in [1u16, 2, 6, 9] {
            let frames = (rate as usize * 4).min(40_000);
            let raw: Vec<f32> = (0..frames * channels as usize)
                .map(|i| match i % 11 {
                    0 => f32::from_bits(next()),
                    1 => f32::NAN,
                    2 => f32::INFINITY,
                    3 => f32::MAX,
                    4 => f32::MIN_POSITIVE / 3.0,
                    _ => (next() as f32 / u32::MAX as f32) * 2.0 - 1.0,
                })
                .collect();
            let Ok(meter) = LoudnessMeter::ebur128(rate, channels) else {
                continue;
            };
            let mut meter = meter.windows().unwrap();
            for piece in raw.chunks(channels as usize * 1013) {
                meter.add(piece).unwrap();
            }
            let _ = (meter.integrated(), meter.threshold(), meter.range());
            let _ = (meter.true_peak(), meter.sample_peak(), meter.summary());
            let _ = (meter.momentary_now(), meter.short_term_now());
            for c in 0..channels as usize {
                let _ = (meter.true_peak_of(c), meter.sample_peak_of(c));
            }
        }
    }
}

#[test]
fn the_default_meter_keeps_its_own_true_peak_and_range_and_agrees_on_the_rest() {
    let rate = 48_000u32;
    let mix = hot(rate, 6.0);
    let mut theirs = EbuR128::new(2, rate, MODE).unwrap();
    let mut reference = LoudnessMeter::ebur128(rate, 2).unwrap();
    let mut own = LoudnessMeter::new(rate, 2).unwrap();
    theirs.add_frames_f32(&mix).unwrap();
    reference.add(&mix).unwrap();
    own.add(&mix).unwrap();
    near(
        own.integrated(),
        theirs.loudness_global().unwrap(),
        1e-6,
        "integrated",
    );
    assert_eq!(own.sample_peak(), reference.sample_peak());
    let (a, b) = (own.true_peak(), reference.true_peak());
    assert!(a != b && (a - b).abs() < 1.0, "true peaks {a} and {b}");
    let noise: Vec<f32> = (0..44_100u32 * 2 * 10)
        .scan(3u32, |s, _| {
            *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            Some((f64::from(*s >> 8) / f64::from(1u32 << 24) * 0.6 - 0.3) as f32)
        })
        .collect();
    let mut own = LoudnessMeter::new(44_100, 2).unwrap();
    let mut reference = LoudnessMeter::ebur128(44_100, 2).unwrap();
    own.add(&noise).unwrap();
    reference.add(&noise).unwrap();
    assert!(
        own.true_peak() > reference.true_peak() + 0.3,
        "white noise: {} against {}",
        own.true_peak(),
        reference.true_peak()
    );
    let quiet = signal(rate, 2, 20.0, 5);
    let mut own = LoudnessMeter::new(rate, 2).unwrap();
    let mut reference = LoudnessMeter::ebur128(rate, 2).unwrap();
    own.add(&quiet).unwrap();
    reference.add(&quiet).unwrap();
    assert!(own.range() > 1.0 && reference.range() > 1.0);
    assert_ne!(own.range(), reference.range());
}
