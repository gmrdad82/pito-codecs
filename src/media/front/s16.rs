use std::path::Path;

use super::probe::probe;
use super::route::Prefs;
use super::stream::{AudioSpan, decode_audio_span};
use crate::media::{Error, Result};

pub const LOWEST_RATE: u32 = 8_000;
pub const HIGHEST_RATE: u32 = 768_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct S16Span {
    pub stream: usize,
    pub from: f64,
    pub rate: u32,
    pub mono: bool,
}

pub fn to_s16(s: f32) -> i16 {
    (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16
}

pub fn decode_s16(
    path: &Path,
    span: &S16Span,
    prefs: &Prefs,
    mut each: impl FnMut(&[i16]) -> Result<()>,
) -> Result<u64> {
    if !(LOWEST_RATE..=HIGHEST_RATE).contains(&span.rate) {
        return Err(Error::Invalid(format!(
            "an output rate of {} Hz; use {LOWEST_RATE} to {HIGHEST_RATE}",
            span.rate
        )));
    }
    if !span.from.is_finite() || span.from < 0.0 {
        return Err(Error::Invalid(format!("a start at {} s", span.from)));
    }
    let p = probe(path, prefs).map_err(|e| e.in_file(path))?;
    let Some(t) = p.audio.iter().find(|t| t.stream == span.stream) else {
        let have: Vec<String> = p.audio.iter().map(|t| t.stream.to_string()).collect();
        let listed = match have.as_slice() {
            [] => "it has no audio".to_string(),
            [one] => format!("its audio stream is {one}"),
            many => format!("its audio streams are {}", many.join(", ")),
        };
        return Err(
            Error::Invalid(format!("no audio stream {}; {listed}", span.stream)).in_file(path),
        );
    };
    let named = |e: Error| e.in_stream(span.stream).in_file(path);
    t.decodable().map_err(named)?;
    let past = || {
        Error::Invalid(format!(
            "stream {}: the start at {} s is at or past the end of its audio",
            span.stream, span.from
        ))
        .in_file(path)
    };
    if p.duration.is_finite() && p.duration > 0.0 && span.from >= p.duration {
        return Err(past());
    }
    let channels = if span.mono { 1 } else { t.channels.max(1) };
    let audio = AudioSpan {
        from: span.from,
        to: f64::INFINITY,
        rate: span.rate,
        channels,
    };
    let mut scratch: Vec<i16> = Vec::new();
    let mut frames = 0u64;
    decode_audio_span(path, t.index, prefs, &audio, |b| {
        scratch.clear();
        scratch.extend(b.samples.iter().map(|&s| to_s16(s)));
        frames += b.frames() as u64;
        each(&scratch)
    })
    .map_err(named)?;
    if frames == 0 {
        return Err(if span.from > 0.0 {
            past()
        } else {
            silent(span.stream).in_file(path)
        });
    }
    Ok(frames)
}

fn silent(stream: usize) -> Error {
    Error::Invalid(format!("stream {stream}: it holds no audio samples"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_rounds_and_clips_like_a_wav_extract() {
        assert_eq!(to_s16(0.0), 0);
        assert_eq!(to_s16(1.0), 32767);
        assert_eq!(to_s16(-1.0), -32768);
        assert_eq!(to_s16(2.5), 32767);
        assert_eq!(to_s16(-3.0), -32768);
        assert_eq!(to_s16(0.5), 16384);
        assert_eq!(to_s16(-0.5), -16384);
        assert_eq!(to_s16(1.0 / 32768.0), 1);
        assert_eq!(to_s16(f32::NAN), 0);
    }

    fn refused(span: S16Span) -> String {
        decode_s16(Path::new("missing.mkv"), &span, &Prefs::default(), |_| {
            Ok(())
        })
        .unwrap_err()
        .to_string()
    }

    #[test]
    fn a_bad_rate_or_start_is_refused_before_the_file_is_read() {
        let span = S16Span {
            stream: 1,
            from: 0.0,
            rate: 16_000,
            mono: true,
        };
        let zero = refused(S16Span { rate: 0, ..span });
        assert!(zero.contains("output rate of 0 Hz"), "{zero}");
        let low = refused(S16Span {
            rate: LOWEST_RATE - 1,
            ..span
        });
        assert!(low.contains("7999 Hz"), "{low}");
        let high = refused(S16Span {
            rate: HIGHEST_RATE + 1,
            ..span
        });
        assert!(high.contains("768001 Hz"), "{high}");
        for from in [-1.0, f64::NAN, f64::INFINITY] {
            let said = refused(S16Span { from, ..span });
            assert!(said.contains("a start at"), "{from}: {said}");
        }
        let missing = refused(span);
        assert!(missing.contains("missing.mkv"), "{missing}");
    }

    #[test]
    fn an_empty_stream_says_it_holds_no_audio_rather_than_a_late_start() {
        let said = silent(2).to_string();
        assert!(said.contains("stream 2"), "{said}");
        assert!(said.contains("no audio samples"), "{said}");
        assert!(!said.contains("past the end"), "{said}");
    }
}
