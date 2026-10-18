use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Instant;

use pito_codecs::filter::LoudnessMeter;
use pito_codecs::media::{self, AudioBuffer, AudioSpan, Prefs};

use super::Fail;
use super::convert::{Loud, Options, hash, land, level_progress, stats, target_of};
use super::manifest;
use super::pipeline::Stopped;
use super::plan::{Loudness, partial, same_file};
use super::progress::Progress;
use super::signals;

pub const RATES: [u32; 2] = [44_100, 48_000];
const HEADER: usize = 44;
const WRITE_BLOCK: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wav {
    pub from: f64,
    pub to: Option<f64>,
    pub mono: bool,
    pub rate: Option<u32>,
}

pub fn is_wav(output: &Path) -> bool {
    output
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

fn header(rate: u32, channels: u16, data: u32) -> [u8; HEADER] {
    let align = channels * 2;
    let mut h = [0u8; HEADER];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(data + 36).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * u32::from(align)).to_le_bytes());
    h[32..34].copy_from_slice(&align.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

struct Sink {
    file: BufWriter<File>,
    bytes: u64,
    scratch: Vec<u8>,
    meter: Option<LoudnessMeter>,
}

impl Sink {
    fn push(&mut self, buf: &AudioBuffer) -> Result<(), Fail> {
        if let Some(m) = self.meter.as_mut() {
            m.process(buf)?;
        }
        self.scratch.clear();
        for &s in &buf.samples {
            let v = (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            self.scratch.extend_from_slice(&v.to_le_bytes());
        }
        self.bytes += self.scratch.len() as u64;
        if self.bytes > u64::from(u32::MAX) - 36 {
            return Err("the WAV would pass 4 GiB; cut it shorter with --from and --to".into());
        }
        self.file.write_all(&self.scratch)?;
        Ok(())
    }
}

fn cancelled(e: media::Error) -> Fail {
    match e {
        media::Error::Cancelled => Box::new(Stopped),
        e => e.to_string().into(),
    }
}

pub fn run(
    options: &Options,
    wav: &Wav,
    partial_path: &mut Option<PathBuf>,
) -> Result<String, Fail> {
    let started = Instant::now();
    let request = &options.request;
    let input = &request.input;
    let output = &request.output;
    if same_file(input, output) {
        return Err("the output is the input; write to another file".into());
    }
    if !request.overwrite && output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            output.display()
        )
        .into());
    }
    let prefs = Prefs::default();
    let probe = media::probe(input, &prefs).map_err(|e| format!("{}: {e}", input.display()))?;
    let sound = probe
        .audio
        .first()
        .ok_or_else(|| format!("{}: the source has no audio", input.display()))?;
    let length = probe.duration;
    if length.is_finite() && length > 0.0 && wav.from >= length {
        return Err(format!(
            "--from {} is at or past the end of the audio ({length:.3} s)",
            wav.from
        )
        .into());
    }
    let rate = wav.rate.unwrap_or(if RATES.contains(&sound.rate) {
        sound.rate
    } else {
        48_000
    });
    let channels = if wav.mono { 1 } else { 2 };
    let span = AudioSpan {
        from: wav.from,
        to: wav.to.unwrap_or(f64::INFINITY),
        rate,
        channels,
    };
    let source_hash = if options.json {
        hash(input)?
    } else {
        String::new()
    };
    let part = partial(output)?;
    *partial_path = Some(part.clone());
    let progress = Progress::new(options.progress);
    let written = write(
        input,
        &probe,
        &prefs,
        &span,
        request.loudness,
        &part,
        &progress,
    );
    progress.clear();
    let (frames, loud) = written?;
    if frames == 0 {
        return Err("there is no audio in that range".into());
    }
    land(&part, output, request.overwrite)?;
    *partial_path = None;
    if !options.json {
        let mut lines = vec![output.display().to_string()];
        if let Some(l) = &loud {
            lines.push(l.lines());
        }
        return Ok(lines.join("\n"));
    }
    let seconds = started.elapsed().as_secs_f64();
    let pcm = "pcm_s16le";
    let document = manifest::Manifest {
        sources: vec![manifest::Source {
            path: input.display().to_string(),
            sha256: source_hash,
            loudness: loud.as_ref().map(|l| stats(&l.before)),
        }],
        params: manifest::Params {
            profile: "wav".into(),
            encoder: manifest::Encoder {
                encoder: pcm.into(),
                codec: None,
                settings: Vec::new(),
            },
            quality: None,
            width: 0,
            height: 0,
            fps: None,
            tune: None,
            loudness: loud.as_ref().map(Loud::target),
            audio: Some(manifest::Audio {
                encoder: pcm.into(),
                codec: Some("pcm".into()),
                kbps: None,
                layout: Some(if wav.mono { "mono" } else { "stereo" }.into()),
                sample_rate: Some(rate),
            }),
        },
        device: "CPU".into(),
        seconds: (seconds * 100.0).round() / 100.0,
        files: vec![manifest::Output {
            file: output.display().to_string(),
            sha256: hash(output)?,
            bytes: std::fs::metadata(output).map(|m| m.len()).unwrap_or(0),
            loudness: loud.as_ref().map(|l| stats(&l.after)),
        }],
        join: None,
    };
    Ok(document.to_json())
}

fn write(
    input: &Path,
    probe: &media::Probe,
    prefs: &Prefs,
    span: &AudioSpan,
    target: Option<Loudness>,
    part: &Path,
    progress: &Progress,
) -> Result<(u64, Option<Loud>), Fail> {
    let track = probe.audio.first().map_or(0, |a| a.index);
    let plan = match target {
        Some(t) => Some(
            media::plan_level_styled(
                input,
                track,
                prefs,
                span,
                &target_of(&t),
                media::LevelStyle {
                    linear: t.linear,
                    encoded: None,
                },
                level_progress(progress),
            )
            .map_err(cancelled)?,
        ),
        None => None,
    };
    let mut leveler = plan
        .as_ref()
        .map(|p| p.leveler(span.rate, span.channels))
        .transpose()?;
    let file = File::create(part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut sink = Sink {
        file: BufWriter::with_capacity(WRITE_BLOCK, file),
        bytes: 0,
        scratch: Vec::new(),
        meter: plan
            .is_some()
            .then(|| LoudnessMeter::new(span.rate, span.channels))
            .transpose()?,
    };
    sink.file.write_all(&header(span.rate, span.channels, 0))?;
    let total = Some(probe.duration.min(span.to) - span.from)
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * f64::from(span.rate)).round() as u64);
    progress.step("extract", 1, 1, total);
    let mut frames = 0u64;
    let mut failed: Option<Fail> = None;
    let decoded = media::decode_audio_span(input, track, prefs, span, |mut b| {
        if signals::STOP.load(Ordering::Relaxed) {
            return Err(media::Error::Cancelled);
        }
        if let Some(l) = leveler.as_mut() {
            l.process(&mut b)?;
        }
        frames += b.frames() as u64;
        progress.frame(frames);
        sink.push(&b).map_err(|e| {
            failed = Some(e);
            media::Error::Cancelled
        })
    });
    if let Some(e) = failed {
        return Err(e);
    }
    decoded.map_err(cancelled)?;
    if let Some(l) = leveler.as_mut() {
        let mut tail = AudioBuffer::new(span.rate, span.channels);
        l.flush(&mut tail)?;
        frames += tail.frames() as u64;
        sink.push(&tail)?;
    }
    let data = u32::try_from(sink.bytes).map_err(|_| "the WAV would pass 4 GiB")?;
    let mut file = sink.file.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(span.rate, span.channels, data))?;
    file.sync_all()?;
    let loud = match (plan, target, sink.meter) {
        (Some(plan), Some(target), Some(meter)) => Some(Loud {
            target,
            before: plan.before,
            after: media::LoudnessStats {
                integrated: meter.integrated(),
                range: meter.range(),
                true_peak: meter.true_peak(),
                threshold: meter.threshold(),
            },
            mode: plan.mode(),
        }),
        _ => None,
    };
    Ok((frames, loud))
}
