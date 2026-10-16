pub mod quality {
    use pito_codecs::h264::Preset;
    use pito_codecs::media::Rate;

    pub fn h264_preset(profile: &str) -> Preset {
        if profile == "best" {
            Preset::Quality
        } else {
            Preset::Balanced
        }
    }

    pub fn h264_rate(profile: &str, crf: Option<f32>, _tune_animation: bool) -> Rate {
        let base = if profile == "best" { 18.0 } else { 20.0 };
        Rate::Crf(crf.unwrap_or(base).round().clamp(1.0, 51.0) as u8)
    }

    pub fn vp9_rate(profile: &str, crf: Option<f32>) -> Rate {
        let base = if profile == "best" { 28.0 } else { 32.0 };
        Rate::Crf(crf.unwrap_or(base).round().clamp(0.0, 63.0) as u8)
    }

    pub fn fast_qp(qp: Option<u8>) -> Rate {
        Rate::Qp(qp.unwrap_or(25).min(51))
    }
}
