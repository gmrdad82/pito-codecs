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

use pito_codecs::media::{
    Error, LevelMode, LevelOptions, LevelTarget, Prefs, level, measure_loudness,
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
