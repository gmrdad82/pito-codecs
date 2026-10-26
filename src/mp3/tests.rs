use super::*;
use crate::testref::{Refs, assert_within, diff};

#[path = "refs.rs"]
mod refs;

const LIMIT: f64 = 2e-5;

fn decode_stream(data: &[u8]) -> Vec<f32> {
    let mut dec = Decoder::new();
    let mut pcm = vec![0.0; MAX_SAMPLES_PER_FRAME];
    let mut out = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let f = dec.decode(rest, &mut pcm);
        if f.frame_bytes == 0 {
            break;
        }
        out.extend_from_slice(&pcm[..f.samples * f.channels as usize]);
        rest = &rest[f.frame_bytes..];
    }
    out
}

#[test]
fn decode_matches_mpg123() {
    let r = Refs::open(refs::AREA);
    for c in &refs::PLAIN {
        let file = refs::stream(c.name);
        let got = decode_stream(&r.bytes(&file));
        let got = refs::window::window(&got, c.channels as usize);
        let want = r.f32s(&refs::decoded(&file));
        assert_within(c.name, &diff(&got, &want), LIMIT, LIMIT);
    }
}

#[test]
fn a_file_is_trimmed_by_its_lame_tag_like_mpg123() {
    let r = Refs::open(refs::AREA);
    for c in &refs::GAPLESS {
        let file = refs::stream(c.name);
        let bytes = r.bytes(&file);
        assert!(bytes.starts_with(b"ID3"), "{}: no ID3v2 tag first", c.name);
        let got = decode_file(&bytes).unwrap();
        let ch = c.channels as usize;
        assert_eq!(got.rate, c.rate, "{}", c.name);
        assert_eq!(got.channels as usize, ch, "{}", c.name);
        assert_eq!(
            got.samples.len(),
            refs::frames(c) * ch,
            "{}: trimmed length",
            c.name
        );
        let got = refs::window::window(&got.samples, ch);
        let want = r.f32s(&refs::decoded(&file));
        assert_within(c.name, &diff(&got, &want), LIMIT, LIMIT);
    }
}

#[cfg(any(
    feature = "audio",
    all(
        feature = "h264",
        feature = "hevc",
        feature = "aac",
        feature = "mp4",
        feature = "gpu",
        feature = "filter",
        feature = "mkv",
        feature = "vp9"
    )
))]
#[test]
fn range_starts_on_the_right_sample_after_the_encoder_delay() {
    let r = Refs::open(refs::AREA);
    let (start, end) = refs::delay_samples();
    let (from, to) = refs::DELAY_RANGE;
    let want = r.f32s(&refs::decoded(&refs::stream(refs::DELAY.name)));
    for ext in refs::DELAY_EXTS {
        let file = refs::delay(ext);
        let path = r.path(&file);
        let prefs = crate::media::Prefs::default();
        let rate = refs::DELAY.rate;
        let got = crate::media::audio_pcm_range(&path, 0, from, to, rate, 2, &prefs).unwrap();
        assert_eq!(got.samples.len(), (end - start) * 2, "{ext}");
        let got = refs::window::body(&got.samples, 2);
        assert_within(ext, &diff(&got, &want), LIMIT, LIMIT);
    }
}

#[test]
fn the_window_tells_padding_left_after_the_trim() {
    let full: Vec<f32> = (0..22_050 * 2)
        .map(|i| ((i as f32 * 0.37).sin() + (i as f32 * 0.011).cos()) * 0.25)
        .collect();
    let want = refs::window::window(&full, 2);
    let mut padded = full.clone();
    padded.extend(std::iter::repeat_n(0.0, 350 * 2));
    let lost = refs::window::window(&full[..full.len() - 1152 * 2], 2);
    for (what, got) in [("padded", refs::window::window(&padded, 2)), ("lost", lost)] {
        assert!(!diff(&got, &want).within(LIMIT, LIMIT), "{what}");
    }
    let range = refs::window::window_range(&full, 2, 100, 200);
    assert_eq!(range, refs::window::body(&full[200..400], 2));
    assert_eq!(
        refs::window::window_range(&full, 2, 22_000, 99_999),
        refs::window::body(&full[44_000..], 2)
    );
}

#[test]
fn the_cases_are_named_once_and_cover_mono_and_stereo() {
    let mut names: Vec<&str> = refs::PLAIN
        .iter()
        .chain(&refs::GAPLESS)
        .map(|c| c.name)
        .chain([refs::DELAY.name])
        .collect();
    let all = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), all);
    for cases in [&refs::PLAIN[..], &refs::GAPLESS[..]] {
        assert!(cases.iter().any(|c| c.channels == 1));
        assert!(cases.iter().any(|c| c.channels == 2));
    }
    assert_eq!(refs::delay_samples(), (8820, 17_640));
    assert_eq!(refs::frames(&refs::DELAY), 22_050);
}
