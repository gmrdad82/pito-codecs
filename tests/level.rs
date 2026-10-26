#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9"
))]

#[path = "../src/testref.rs"]
mod testref;

use std::path::Path;

use std::f64::consts::PI;

use pito_codecs::filter::LoudnessMeter;
use pito_codecs::media::{
    AudioBuffer, AudioSpan, CALIBRATION_PASSES, Codec, Error, LevelMode, LevelOptions, LevelStyle,
    LevelTarget, Prefs, level, level_pcm, measure_loudness, meter_loudness, plan_level_styled, wav,
};
use pito_codecs::mp4::Reader;
use testref::{Clip, Sound};

fn bursts(i: u64, ch: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    let v = if ch == 0 {
        let burst = if t % 1.0 < 0.004 {
            0.9 * (2.0 * PI * 3000.0 * t).sin()
        } else {
            0.0
        };
        0.03 * (2.0 * PI * 440.0 * t).sin() + burst
    } else {
        0.03 * (2.0 * PI * 660.0 * t).sin()
    };
    v as f32
}

fn level_clip() -> Clip {
    Clip {
        gop: 25,
        sounds: vec![Sound::Fn(bursts)],
        kbps: 192,
        ..Clip::new((352, 240), (25, 1), 150)
    }
}

fn video(path: &Path) -> Vec<(Vec<u8>, i64, i64, bool)> {
    let r = Reader::open(path).unwrap();
    let v = r.video().unwrap();
    (0..r.tracks[v].samples.len())
        .map(|i| {
            let s = r.tracks[v].samples[i];
            (r.read(v, i).unwrap(), s.pts, s.dts, s.keyframe)
        })
        .collect()
}

#[test]
fn levelling_reaches_the_target_under_the_ceiling_and_copies_the_video() {
    let input = level_clip().make("level", "level-in.mp4");
    let output = input.with_file_name("level-out.mp4");
    let done = level(&input, &output, &LevelOptions::default(), |_| true).unwrap();
    assert_eq!(done.mode, LevelMode::Limited);
    let after = measure_loudness(&output, 0, &Prefs::default(), |_| true).unwrap();
    let target = LevelTarget::youtube();
    for a in [done.after, after] {
        assert!((a.integrated - target.integrated).abs() < 0.5, "{a:?}");
        assert!(a.true_peak <= target.true_peak, "{a:?}");
    }
    assert_eq!(video(&output), video(&input));
    let quiet = level_clip().video_only().make("level", "level-silent.mp4");
    let refused = level(
        &quiet,
        &output.with_file_name("level-none.mp4"),
        &LevelOptions::default(),
        |_| true,
    );
    assert!(matches!(refused, Err(Error::Unsupported(m)) if m == "no audio to level"));
}

fn film(seconds: usize) -> AudioBuffer {
    let rate = 48_000usize;
    let quiet = (seconds * 3 / 4) as f64;
    let mut b = AudioBuffer::new(rate as u32, 2);
    for n in 0..seconds * rate {
        let t = n as f64 / rate as f64;
        let body = if t < quiet {
            0.02 + 0.01 * (t * 0.7).sin()
        } else {
            0.02 * 6f64.powf((t - quiet) / (seconds as f64 - quiet))
        };
        let tone = ((2.0 * PI * 220.0 * t).sin() + 0.5 * (2.0 * PI * 331.0 * t).sin()) / 1.5;
        let hit = t >= quiet && t % 0.25 < 0.005;
        let burst = if hit {
            0.8 * (2.0 * PI * 200.0 * t).sin()
        } else {
            0.0
        };
        let l = body * tone + burst;
        let r = body * (2.0 * PI * 277.0 * t).sin() + burst;
        b.samples.extend([l as f32, r as f32]);
    }
    b
}

fn write_wav(path: &Path, b: &AudioBuffer) {
    let data = (b.samples.len() * 3) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(b.channels.to_le_bytes());
    out.extend(b.rate.to_le_bytes());
    out.extend((b.rate * u32::from(b.channels) * 3).to_le_bytes());
    out.extend((b.channels * 3).to_le_bytes());
    out.extend(24u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    for s in &b.samples {
        let v = (f64::from(*s) * 8_388_607.0).round() as i32;
        out.extend(&v.to_le_bytes()[..3]);
    }
    std::fs::write(path, out).unwrap();
}

#[test]
fn a_wav_measures_and_levels_like_its_samples() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("level-film.wav");
    write_wav(&path, &film(20));
    let samples = wav::decode(&std::fs::read(&path).unwrap()).unwrap();
    let mut meter = LoudnessMeter::new(samples.rate, samples.channels).unwrap();
    meter.process(&samples).unwrap();
    let want = meter.summary();
    let got = meter_loudness(&path, 0, &Prefs::default(), |_| true).unwrap();
    for (a, b) in [
        (got.integrated, want.integrated),
        (got.range, want.range),
        (got.true_peak, want.true_peak),
        (got.short_term_max, want.short_term_max),
        (got.momentary_max, want.momentary_max),
    ] {
        assert!((a - b).abs() < 0.01, "{got:?} against {want:?}");
    }
    let target = LevelTarget::new(-14.0);
    let span = AudioSpan::WHOLE;
    for linear in [false, true] {
        let style = LevelStyle {
            linear,
            encoded: None,
        };
        let plan = plan_level_styled(&path, 0, &Prefs::default(), &span, &target, style, |_| true)
            .unwrap();
        assert_eq!(plan.mode(), LevelMode::Limited);
    }
}

#[test]
fn linear_levelling_keeps_the_range_and_the_encoded_aac_keeps_the_ceiling() {
    let source = film(40);
    let target = LevelTarget::new(-14.0);
    let pressed = level_pcm(&source, &target, LevelStyle::default()).unwrap();
    let style = LevelStyle {
        linear: true,
        encoded: Some((Codec::Aac, 192)),
    };
    let kept = level_pcm(&source, &target, style).unwrap();
    assert!(
        (pressed.after.integrated - target.integrated).abs() < 0.2,
        "{:?}",
        pressed.after
    );
    assert!(
        kept.after.integrated < target.integrated - 1.0,
        "{:?}",
        kept.after
    );
    assert!(
        kept.after.range > pressed.after.range + 2.0,
        "linear {:?}, pressed {:?}",
        kept.after,
        pressed.after
    );
    let encoded = kept.encoded.unwrap();
    assert!(encoded.true_peak <= target.true_peak, "{encoded:?}");
}

fn dense(seconds: usize) -> AudioBuffer {
    let rate = 48_000usize;
    let n = seconds * rate;
    let drop = seconds as f64 * 0.4;
    let mut out = vec![[0.0f64; 2]; n];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f64 / rate as f64;
        let chord = |shift: f64| {
            [(220.0, 0.0), (277.2, 1.0), (329.6, 2.0), (440.0, 3.0)]
                .iter()
                .map(|(f, p)| (2.0 * PI * f * (t - shift) + p).sin())
                .sum::<f64>()
                / 4.0
        };
        let swell = if t < drop {
            0.05 + 0.1 * t / drop
        } else {
            0.25
        };
        let sub = if t >= drop {
            0.3 * (2.0 * PI * 41.0 * t).sin()
        } else {
            0.0
        };
        *o = [
            chord(0.0) * swell + sub,
            chord(300.0 / rate as f64) * swell + sub,
        ];
    }
    let kick_len = rate * 35 / 100;
    let mut phase = 0.0;
    let kick: Vec<f64> = (0..kick_len)
        .map(|j| {
            let k = j as f64 / rate as f64;
            phase += 2.0 * PI * (45.0 + 110.0 * (-k * 30.0).exp()) / rate as f64;
            let click = if j < 48 {
                (1.0 - j as f64 / 48.0) * (j as f64 * 2.9).sin().signum()
            } else {
                0.0
            };
            phase.sin() * (-k * 9.0).exp() + click
        })
        .collect();
    let mut seed = 7u64;
    let mut noise = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    };
    let crash_len = rate * 3 / 2;
    let mut last = [0.0f64; 2];
    let crash: Vec<[f64; 2]> = (0..crash_len)
        .map(|j| {
            let e = (-(j as f64 / rate as f64) * 3.0).exp() * 0.6;
            let w = [noise(), noise()];
            let d = [w[0] - last[0], w[1] - last[1]];
            last = w;
            [d[0] * e, d[1] * e]
        })
        .collect();
    let mut add = |at: f64, gain: f64, sound: &dyn Fn(usize) -> [f64; 2], len: usize| {
        let i = (at * rate as f64) as usize;
        for j in 0..len.min(n.saturating_sub(i)) {
            let s = sound(j);
            out[i + j][0] += gain * s[0];
            out[i + j][1] += gain * s[1];
        }
    };
    let kicked = |j: usize| [kick[j], kick[j]];
    let mut at = drop;
    while at < seconds as f64 - 0.4 {
        add(at, 0.9, &kicked, kick_len);
        at += 0.5;
    }
    let mut at = drop;
    while at < seconds as f64 - 1.6 {
        add(at, 1.0, &|j| crash[j], crash_len);
        at += 2.0;
    }
    let mut at = 0.5;
    while at < drop {
        add(at, 0.3, &kicked, kick_len);
        at += 2.0;
    }
    let mut b = AudioBuffer::new(rate as u32, 2);
    b.samples = out
        .iter()
        .flat_map(|s| s.map(|v| v.clamp(-1.0, 1.0) as f32))
        .collect();
    b
}

#[test]
fn a_dense_mix_keeps_the_ceiling_after_the_aac_encode() {
    let source = dense(40);
    let target = LevelTarget::new(-14.0);
    for (linear, kbps) in [(false, 192), (true, 192), (false, 320), (true, 320)] {
        let style = LevelStyle {
            linear,
            encoded: Some((Codec::Aac, kbps)),
        };
        let done = level_pcm(&source, &target, style).unwrap();
        let encoded = done.encoded.unwrap();
        let floor = if kbps == 320 {
            target.true_peak - 0.15
        } else {
            f64::NEG_INFINITY
        };
        assert!(
            (floor..=target.true_peak).contains(&encoded.true_peak),
            "linear {linear} at {kbps} kbps: {encoded:?}, limiter at {:?}",
            done.ceiling_db
        );
    }
}

fn square_bursts(seconds: usize) -> AudioBuffer {
    let rate = 48_000usize;
    let mut b = AudioBuffer::new(rate as u32, 2);
    for n in 0..seconds * rate {
        let t = n as f64 / rate as f64;
        let bed = 0.08 * (2.0 * PI * 220.0 * t).sin();
        let burst = if t % 0.25 < 0.004 {
            0.9 * (2.0 * PI * 1000.0 * t).sin().signum()
        } else {
            0.0
        };
        let s = (bed + burst) as f32;
        b.samples.extend([s, s]);
    }
    b
}

fn speech(seconds: usize) -> AudioBuffer {
    let rate = 48_000usize;
    let mut b = AudioBuffer::new(rate as u32, 2);
    let mut seed = 11u64;
    for n in 0..seconds * rate {
        let t = n as f64 / rate as f64;
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let hiss = ((seed >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
        let syllable = (2.0 * PI * 4.0 * t).sin().max(0.0).powi(2);
        let pause = if t % 3.0 < 2.2 { 1.0 } else { 0.0 };
        let pitch = 140.0 + 15.0 * (2.0 * PI * 0.3 * t).sin();
        let voice: f64 = (1..=8)
            .map(|h| (2.0 * PI * pitch * h as f64 * t).sin() / h as f64)
            .sum();
        let plosive = if t % 1.7 < 0.003 { 0.25 * hiss } else { 0.0 };
        let s = (0.012 * voice * syllable * pause + 0.002 * hiss + plosive) as f32;
        b.samples.extend([s, s * 0.9]);
    }
    b
}

fn lands_on_target(name: &str, source: &AudioBuffer, target: &LevelTarget) {
    let done = level_pcm(source, target, LevelStyle::default()).unwrap();
    assert_eq!(done.mode, LevelMode::Limited, "{name}: {:?}", done.after);
    let c = done
        .calibration
        .unwrap_or_else(|| panic!("{name}: levelled without calibrating"));
    let passes = c.passes;
    eprintln!(
        "{name}: {:.2} LUFS, true peak {:.2} dBTP, {passes} passes",
        done.after.integrated, done.after.true_peak
    );
    assert!(
        (done.after.integrated - target.integrated).abs() <= 0.1,
        "{name}: {:?} {:?}",
        done.after,
        done.calibration
    );
    assert!(
        done.after.true_peak <= target.true_peak,
        "{name}: {:?}",
        done.after
    );
    assert!(c.reached, "{name}: {c:?}");
    assert!(
        (1..=CALIBRATION_PASSES).contains(&c.passes),
        "{name}: {c:?}"
    );
    assert!(
        (c.landed - done.after.integrated).abs() < 1e-9,
        "{name}: {c:?}"
    );
    assert_eq!(c.ceiling_db, done.ceiling_db.unwrap(), "{name}");
    assert_eq!(c.shortfall(target), None);
}

#[test]
fn calibrated_levelling_lands_within_a_tenth_of_a_lu_on_limited_signals() {
    for target in [LevelTarget::youtube(), LevelTarget::new(-14.0)] {
        let bursts = square_bursts(20);
        let looser = LevelTarget {
            integrated: target.integrated,
            true_peak: target.true_peak + 1.0,
        };
        let pressed = level_pcm(&bursts, &looser, LevelStyle::default()).unwrap();
        assert!(
            (pressed.after.integrated - target.integrated).abs() <= 0.1,
            "{:?}",
            pressed.after
        );
        assert_eq!(pressed.mode, LevelMode::Limited);
        assert!(pressed.calibration.is_some());
        lands_on_target("square bursts", &bursts, &target);
        lands_on_target("dense mix", &dense(30), &target);
        let quiet = speech(20);
        let mut meter = LoudnessMeter::new(quiet.rate, quiet.channels).unwrap();
        meter.process(&quiet).unwrap();
        assert!(meter.integrated() < -30.0, "{}", meter.integrated());
        lands_on_target("quiet speech", &quiet, &target);
        lands_on_target("already at the target", &pressed.buffer, &target);
    }
}

#[test]
fn a_target_the_ceiling_cannot_reach_says_how_short_it_landed() {
    let rate = 48_000usize;
    let mut hum = AudioBuffer::new(rate as u32, 2);
    for n in 0..10 * rate {
        let s = (0.1 * (2.0 * PI * 100.0 * n as f64 / rate as f64).sin()) as f32;
        hum.samples.extend([s, s]);
    }
    let target = LevelTarget {
        integrated: -5.0,
        true_peak: -9.0,
    };
    let done = level_pcm(&hum, &target, LevelStyle::default()).unwrap();
    let c = done.calibration.unwrap();
    assert!(!c.reached, "{c:?}");
    assert!(c.passes <= CALIBRATION_PASSES, "{c:?}");
    assert!(done.after.true_peak <= target.true_peak, "{:?}", done.after);
    assert!(done.after.integrated < target.integrated - 0.1);
    let line = c.shortfall(&target).unwrap();
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains("-5 LUFS"), "{line}");
    let linear = LevelStyle {
        linear: true,
        encoded: None,
    };
    assert_eq!(level_pcm(&hum, &target, linear).unwrap().calibration, None);
}
