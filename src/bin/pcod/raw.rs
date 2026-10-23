use std::fs::File;
use std::io::{self, BufWriter, ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use pito_codecs::media::{self, Prefs, S16Span};

use super::Fail;
use super::convert::{Options, land};
use super::extract::Wav;
use super::pipeline::Stopped;
use super::plan::{partial, same_file};

const WRITE_BLOCK: usize = 1 << 16;

pub fn is_raw(output: &Path) -> bool {
    output.as_os_str() == "-"
        || output
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("s16"))
}

fn hung_up(e: &Fail) -> bool {
    e.downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::BrokenPipe)
}

pub fn span_of(input: &Path, wav: &Wav) -> Result<S16Span, Fail> {
    let p = media::probe(input, &Prefs::default()).map_err(|e| e.in_file(input).to_string())?;
    let first = p
        .audio
        .first()
        .ok_or_else(|| format!("{}: the source has no audio", input.display()))?;
    let stream = wav.track.unwrap_or(first.stream);
    let rate = wav.rate.unwrap_or_else(|| {
        p.audio
            .iter()
            .find(|t| t.stream == stream)
            .map_or(0, |t| t.rate)
    });
    Ok(S16Span {
        stream,
        from: wav.from,
        rate: if rate == 0 { 48_000 } else { rate },
        mono: wav.mono,
    })
}

pub fn run(
    options: &Options,
    wav: &Wav,
    partial_path: &mut Option<PathBuf>,
    stop: &AtomicBool,
) -> Result<String, Fail> {
    let request = &options.request;
    let (input, output) = (&request.input, &request.output);
    if output.as_os_str() == "-" {
        not_a_terminal(io::stdout().is_terminal())?;
    }
    let span = span_of(input, wav)?;
    if output.as_os_str() == "-" {
        let mut out = BufWriter::with_capacity(WRITE_BLOCK, io::stdout().lock());
        let written = write(input, &span, &mut out, stop).and_then(|_| Ok(out.flush()?));
        return match written {
            Err(e) if hung_up(&e) => Ok(String::new()),
            Err(e) => Err(e),
            Ok(()) => Ok(String::new()),
        };
    }
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
    let part = partial(output)?;
    *partial_path = Some(part.clone());
    let file = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut out = BufWriter::with_capacity(WRITE_BLOCK, file);
    write(input, &span, &mut out, stop)?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    land(&part, output, request.overwrite)?;
    *partial_path = None;
    Ok(output.display().to_string())
}

fn not_a_terminal(terminal: bool) -> Result<(), Fail> {
    if terminal {
        return Err("stdout is a terminal; redirect it or name a .s16 file".into());
    }
    Ok(())
}

fn write(
    input: &Path,
    span: &S16Span,
    out: &mut impl Write,
    stop: &AtomicBool,
) -> Result<u64, Fail> {
    let mut bytes = Vec::new();
    let mut failed: Option<io::Error> = None;
    let decoded = media::decode_s16(input, span, &Prefs::default(), |samples| {
        if stop.load(Ordering::Relaxed) {
            return Err(media::Error::Cancelled);
        }
        bytes.clear();
        bytes.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        out.write_all(&bytes).map_err(|e| {
            failed = Some(e);
            media::Error::Cancelled
        })
    });
    if let Some(e) = failed {
        return Err(Box::new(e));
    }
    decoded.map_err(|e| -> Fail {
        match e {
            media::Error::Cancelled => Box::new(Stopped),
            e => e.to_string().into(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dash_or_an_s16_extension_is_raw_and_nothing_else_is() {
        for raw in ["-", "out.s16", "a/b.S16"] {
            assert!(is_raw(Path::new(raw)), "{raw}");
        }
        for other in ["out.wav", "out.s16le", "s16", "-.wav", "--"] {
            assert!(!is_raw(Path::new(other)), "{other}");
        }
    }

    #[test]
    fn raw_pcm_to_a_terminal_is_refused_and_to_a_pipe_is_not() {
        let said = not_a_terminal(true).unwrap_err().to_string();
        assert!(said.contains("stdout is a terminal"), "{said}");
        assert!(said.contains(".s16"), "{said}");
        assert!(not_a_terminal(false).is_ok());
    }

    #[test]
    fn only_a_broken_pipe_counts_as_the_reader_hanging_up() {
        let pipe: Fail = Box::new(io::Error::from(ErrorKind::BrokenPipe));
        assert!(hung_up(&pipe));
        let full: Fail = Box::new(io::Error::from(ErrorKind::StorageFull));
        assert!(!hung_up(&full));
        let text: Fail = "broken pipe".into();
        assert!(!hung_up(&text));
    }

    fn tone_wav(name: &str, frames: u32) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/raw-unit")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let data = frames * 4;
        let mut b = Vec::with_capacity(44 + data as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&[1, 0, 2, 0]);
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&192_000u32.to_le_bytes());
        b.extend_from_slice(&[4, 0, 16, 0]);
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data.to_le_bytes());
        for n in 0..frames {
            let v = ((n % 96) as i16 - 48) * 300;
            b.extend_from_slice(&v.to_le_bytes());
            b.extend_from_slice(&(-v).to_le_bytes());
        }
        let path = d.join("tone.wav");
        std::fs::write(&path, b).unwrap();
        path
    }

    fn whole(input: &Path) -> S16Span {
        let p = media::probe(input, &Prefs::default()).unwrap();
        S16Span {
            stream: p.audio[0].stream,
            from: 0.0,
            rate: 48_000,
            mono: false,
        }
    }

    struct Refusing(ErrorKind);

    impl Write for Refusing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn the_samples_come_out_as_little_endian_pairs_until_the_end() {
        let input = tone_wav("all", 4800);
        let mut out = Vec::new();
        let frames = write(&input, &whole(&input), &mut out, &AtomicBool::new(false)).unwrap();
        assert_eq!(frames, 4800);
        assert_eq!(out.len(), 4800 * 4);
        assert_eq!(&out[..4], &[0xc0, 0xc7, 0x40, 0x38]);
    }

    #[test]
    fn a_failed_write_comes_back_as_that_io_error() {
        let input = tone_wav("refused", 4800);
        for kind in [ErrorKind::StorageFull, ErrorKind::BrokenPipe] {
            let e = write(
                &input,
                &whole(&input),
                &mut Refusing(kind),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            let io = e.downcast_ref::<io::Error>().expect("an io error");
            assert_eq!(io.kind(), kind);
            assert_eq!(hung_up(&e), kind == ErrorKind::BrokenPipe);
        }
    }

    #[test]
    fn a_stop_ends_the_stream_before_any_sample_is_written() {
        let input = tone_wav("stopped", 4800);
        let mut out = Vec::new();
        let e = write(&input, &whole(&input), &mut out, &AtomicBool::new(true)).unwrap_err();
        assert!(e.downcast_ref::<Stopped>().is_some(), "{e}");
        assert!(out.is_empty());
    }

    #[test]
    fn a_stopped_s16_file_names_its_partial_for_removal_and_never_lands() {
        let input = tone_wav("stopped-file", 48_000);
        let output = input.with_file_name("out.s16");
        let options = Options {
            request: super::super::plan::Request::new(&input, &output),
            json: false,
            progress: false,
            wav: None,
            add: None,
            range: None,
            still: None,
            crop: None,
        };
        let wav = Wav {
            from: 0.0,
            to: None,
            mono: true,
            rate: Some(16_000),
            opus: None,
            track: None,
            raw: true,
        };
        let mut part = None;
        let e = run(&options, &wav, &mut part, &AtomicBool::new(true)).unwrap_err();
        assert!(e.downcast_ref::<Stopped>().is_some(), "{e}");
        let part = part.expect("the partial is named so the caller removes it");
        assert!(part.is_file());
        assert!(!output.exists());
        let mut fine = None;
        let landed = run(&options, &wav, &mut fine, &AtomicBool::new(false)).unwrap();
        assert_eq!(landed, output.display().to_string());
        assert!(fine.is_none());
        let samples = std::fs::metadata(&output).unwrap().len() / 2;
        assert!((15_900..=16_100).contains(&samples), "{samples}");
    }
}
