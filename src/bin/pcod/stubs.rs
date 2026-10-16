pub mod sha256 {
    use std::io::Read;
    use std::path::Path;

    pub fn sha256_file(path: &Path) -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut sum = 0u64;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            for b in &buf[..n] {
                sum = sum
                    .wrapping_mul(1_099_511_628_211)
                    .wrapping_add(u64::from(*b));
            }
        }
        Ok(format!("{sum:064x}"))
    }
}

pub mod progress {
    pub struct Progress;

    impl Progress {
        pub fn new(_enabled: bool) -> Progress {
            Progress
        }

        pub fn step(&self, _name: &str, _index: usize, _total: usize, _frames: Option<u64>) {}

        pub fn frame(&self, _done: u64) {}

        pub fn clear(&self) {}
    }
}

pub mod quality {
    use pito_codecs::media::Rate;

    pub fn h264_rate(profile: &str, crf: Option<f32>, _tune_animation: bool) -> Rate {
        let base = if profile == "best" { 18.0 } else { 20.0 };
        Rate::Crf(crf.unwrap_or(base).round().clamp(1.0, 51.0) as u8)
    }

    pub fn vp9_rate(profile: &str, crf: Option<f32>) -> Rate {
        let base = if profile == "best" { 28.0 } else { 32.0 };
        Rate::Crf(crf.unwrap_or(base).round().clamp(0.0, 63.0) as u8)
    }
}

pub mod probe_json {
    use std::path::Path;

    use crate::Fail;

    pub fn probe_json(path: &Path) -> Result<String, Fail> {
        let p = pito_codecs::media::probe(path, &pito_codecs::media::Prefs::default())?;
        Ok(format!(
            "{{\"width\":{},\"height\":{},\"codec\":\"{}\"}}",
            p.width, p.height, p.video_codec_name
        ))
    }
}
