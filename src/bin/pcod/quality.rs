use pito_codecs::h264::Preset;
use pito_codecs::media::Rate;

pub const FAST_QP: u8 = 25;

const H264_BALANCED: [(f32, f32); 4] = [(18.0, 14.0), (20.0, 16.0), (23.0, 20.0), (28.0, 25.0)];
const H264_BEST: [(f32, f32); 4] = [(18.0, 16.0), (20.0, 18.0), (23.0, 21.0), (28.0, 26.0)];
const H264_ANIMATION: [(f32, f32); 4] = [(18.0, 11.0), (20.0, 13.0), (23.0, 17.0), (28.0, 23.0)];
const VP9_BALANCED: [(f32, f32); 3] = [(28.0, 14.0), (32.0, 18.0), (40.0, 28.0)];
const VP9_BEST: [(f32, f32); 3] = [(28.0, 12.0), (32.0, 16.0), (40.0, 26.0)];

const H264_MAX: f32 = 51.0;
const VP9_MIN: f32 = 2.0;
const VP9_MAX: f32 = 63.0;

fn best(profile: &str) -> bool {
    profile.eq_ignore_ascii_case("best")
}

pub fn h264_preset(profile: &str) -> Preset {
    if best(profile) {
        Preset::Quality
    } else {
        Preset::Balanced
    }
}

pub fn h264_rate(profile: &str, crf: Option<f32>, tune_animation: bool) -> Rate {
    let (table, default) = match (best(profile), tune_animation) {
        (true, true) => (&H264_ANIMATION, 18.0),
        (false, true) => (&H264_ANIMATION, 20.0),
        (true, false) => (&H264_BEST, 18.0),
        (false, false) => (&H264_BALANCED, 20.0),
    };
    Rate::Crf(map(table, crf.unwrap_or(default), default, (0.0, H264_MAX)))
}

pub fn vp9_rate(profile: &str, crf: Option<f32>) -> Rate {
    let (table, default) = if best(profile) {
        (&VP9_BEST, 28.0)
    } else {
        (&VP9_BALANCED, 32.0)
    };
    Rate::Crf(map(
        table,
        crf.unwrap_or(default),
        default,
        (VP9_MIN, VP9_MAX),
    ))
}

pub fn fast_qp(qp: Option<u8>) -> Rate {
    Rate::Qp(qp.unwrap_or(FAST_QP).min(51))
}

fn map(table: &[(f32, f32)], x: f32, default: f32, (lo, hi): (f32, f32)) -> u8 {
    let x = if x.is_finite() { x } else { default };
    let (Some(&first), Some(&last)) = (table.first(), table.last()) else {
        return lo as u8;
    };
    let y = if x <= first.0 {
        first.1 + (x - first.0)
    } else if x >= last.0 {
        last.1 + (x - last.0)
    } else {
        table.windows(2).find(|w| x <= w[1].0).map_or(last.1, |w| {
            w[0].1 + (x - w[0].0) * (w[1].1 - w[0].1) / (w[1].0 - w[0].0)
        })
    };
    y.round().clamp(lo, hi) as u8
}
