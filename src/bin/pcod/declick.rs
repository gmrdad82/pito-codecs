use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use pito_codecs::filter::{Click, declick as repair};
use pito_codecs::media::{self, AudioSpan, Prefs, wav};

use super::Fail;
use super::manifest::Declicked;
use super::pipeline::Stopped;
use super::plan::Declick;
use super::progress::Progress;
use super::signals;

pub struct Report {
    pub job: Declick,
    pub rate: u32,
    pub clicks: Vec<Click>,
}

impl Report {
    pub fn lines(&self) -> String {
        let n = self.clicks.len();
        let mut out = format!(
            "declick: {n} {} repaired ({})",
            if n == 1 { "click" } else { "clicks" },
            self.job.strength.name()
        );
        if self.job.list {
            for c in &self.clicks {
                let t = c.seconds(self.rate);
                out.push_str(&format!(
                    "\n  {}:{:06.3}  {:.1} dBFS  {:.1} dB above its surroundings",
                    (t / 60.0).floor() as u64,
                    t % 60.0,
                    c.level_db,
                    c.above_db
                ));
            }
        }
        out
    }

    pub fn manifest(&self) -> Declicked {
        Declicked {
            strength: self.job.strength.name().into(),
            repaired: self.clicks.len(),
            clicks: self.job.list.then(|| {
                self.clicks
                    .iter()
                    .map(|c| (c.seconds(self.rate), c.level_db, c.above_db))
                    .collect()
            }),
        }
    }
}

pub struct Prepared {
    pub path: PathBuf,
    pub report: Report,
}

impl Drop for Prepared {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn prepare(
    input: &Path,
    probe: &media::Probe,
    job: Declick,
    output: &Path,
    progress: &Progress,
) -> Result<Prepared, Fail> {
    let track = match job.track {
        Some(stream) => probe
            .audio
            .iter()
            .find(|t| t.stream == stream)
            .ok_or_else(|| {
                let have: Vec<String> = probe.audio.iter().map(|t| t.stream.to_string()).collect();
                format!(
                    "{}: no audio stream {stream}; the audio streams are {}",
                    input.display(),
                    have.join(", ")
                )
            })?,
        None => probe
            .audio
            .first()
            .ok_or_else(|| format!("{}: the source has no audio to de-click", input.display()))?,
    };
    if track.rate == 0 || track.channels == 0 {
        return Err(format!(
            "{}: the audio track has no rate or channels",
            input.display()
        )
        .into());
    }
    let channels = track.channels.clamp(1, 2);
    let span = AudioSpan {
        from: 0.0,
        to: f64::INFINITY,
        rate: track.rate,
        channels,
    };
    progress.step("declick", 1, 1, None);
    let mut samples: Vec<f32> = Vec::new();
    media::decode_audio_span(input, track.index, &Prefs::default(), &span, |b| {
        if signals::STOP.load(Ordering::Relaxed) {
            return Err(media::Error::Cancelled);
        }
        samples.extend_from_slice(&b.samples);
        Ok(())
    })
    .map_err(|e| match e {
        media::Error::Cancelled => Box::new(Stopped) as Fail,
        e => e.to_string().into(),
    })?;
    let clicks = repair(
        &mut samples,
        usize::from(channels),
        track.rate,
        job.strength,
    );
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "output needs a UTF-8 file name".to_string())?;
    let path = output.with_file_name(format!(".{name}.pcod-{}.declick.wav", std::process::id()));
    let prepared = Prepared {
        path: path.clone(),
        report: Report {
            job,
            rate: track.rate,
            clicks,
        },
    };
    let mut out = wav::Writer::create(&path, track.rate, channels, wav::Format::Float32)
        .map_err(|e| e.to_string())?;
    out.write(&samples).map_err(|e| e.to_string())?;
    out.finish().map_err(|e| e.to_string())?;
    Ok(prepared)
}
