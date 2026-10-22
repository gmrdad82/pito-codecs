use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use pito_codecs::filter::spectrum::{self, Options};
use pito_codecs::media::{self, AudioBuffer, AudioSpan, Prefs, probe};
use pito_codecs::png;

use super::convert::land;
use super::json::Json;
use super::plan::{partial, same_file};
use super::{Fail, about};

pub struct Job {
    pub file: PathBuf,
    pub output: PathBuf,
    pub track: usize,
    pub options: Options,
    pub overwrite: bool,
    pub json: bool,
}

fn ms(seconds: f64) -> f64 {
    (seconds * 10_000.0).round() / 10.0
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    let started = Instant::now();
    let file = &job.file;
    let named_png = job
        .output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    if !named_png {
        return Err(format!(
            "{}: a spectrogram is a PNG; name the output .png",
            job.output.display()
        )
        .into());
    }
    if same_file(file, &job.output) {
        return Err("the output is the input; write to another file".into());
    }
    if !job.overwrite && job.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            job.output.display()
        )
        .into());
    }
    let prefs = Prefs::default();
    let p = probe(file, &prefs).map_err(|e| about(file, e))?;
    let t = p.audio.get(job.track).ok_or_else(|| {
        about(
            file,
            match p.audio.len() {
                0 => "the file has no audio".to_string(),
                n => format!("there is no audio track {}; the file has {n}", job.track),
            },
        )
    })?;
    let rate = if t.rate == 0 { 48_000 } else { t.rate };
    let span = AudioSpan {
        from: 0.0,
        to: f64::INFINITY,
        rate,
        channels: 1,
    };
    let mut audio = AudioBuffer::new(rate, 1);
    media::decode_audio_span(file, job.track, &prefs, &span, |b| {
        audio.samples.extend_from_slice(&b.samples);
        Ok(())
    })
    .map_err(|e| about(file, e))?;
    let decoded = started.elapsed().as_secs_f64();
    let s = spectrum::spectrogram(&audio, &job.options).map_err(|e| about(file, e))?;
    let drawn = started.elapsed().as_secs_f64();
    let data = png::encode(&s.image).map_err(|e| about(&job.output, e))?;
    let part = partial(&job.output)?;
    let landed = std::fs::write(&part, &data)
        .map_err(|e| about(&job.output, e))
        .and_then(|()| land(&part, &job.output, job.overwrite));
    if landed.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    landed?;
    let written = started.elapsed().as_secs_f64();
    if !job.json {
        writeln!(out, "{}", job.output.display())?;
        return Ok(());
    }
    let doc = Json::Obj(vec![
        ("file", file.display().to_string().into()),
        ("track", job.track.into()),
        ("output", job.output.display().to_string().into()),
        ("width", s.image.width.into()),
        ("height", s.image.height.into()),
        ("seconds", s.seconds.into()),
        ("sample_rate", rate.into()),
        ("window", s.window.into()),
        ("max_hz", (job.options.max_hz.round() as i64).into()),
        ("floor_db", (job.options.floor_db.round() as i64).into()),
        ("bytes", data.len().into()),
        ("decode_ms", ms(decoded).into()),
        ("draw_ms", ms(drawn - decoded).into()),
        ("png_ms", ms(written - drawn).into()),
    ]);
    writeln!(out, "{}", doc.text())?;
    Ok(())
}
