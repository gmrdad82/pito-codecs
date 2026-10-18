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

use std::path::{Path, PathBuf};
use std::process::Command;

use std::f64::consts::PI;

use pito_codecs::filter::LoudnessMeter;
use pito_codecs::media::{
    AudioBuffer, AudioSpan, Codec, Error, LevelMode, LevelOptions, LevelStyle, LevelTarget, Prefs,
    level, level_pcm, measure_loudness, meter_loudness, plan_level_styled, wav,
};
use pito_codecs::mp4::Reader;

fn make(name: &str, args: &[&str]) -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/test-media");
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y"])
        .args(args)
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
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
    let picture = ["-f", "lavfi", "-i", "testsrc2=s=352x240:r=25:d=6"];
    let sound = "aevalsrc=0.03*sin(2*PI*440*t)+if(lt(mod(t\\,1)\\,0.004)\\,0.9*sin(2*PI*3000*t)\\,0)|0.03*sin(2*PI*660*t):s=48000:d=6";
    let encode = [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-g",
        "25",
        "-pix_fmt",
        "yuv420p",
    ];
    let mut args = picture.to_vec();
    args.extend(["-f", "lavfi", "-i", sound]);
    args.extend(encode);
    args.extend(["-c:a", "aac", "-b:a", "192k", "-shortest"]);
    let Some(input) = make("level-in.mp4", &args) else {
        return;
    };
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
    let mut silent = picture.to_vec();
    silent.extend(encode);
    let quiet = make("level-silent.mp4", &silent).unwrap();
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
