use std::collections::BTreeMap;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use pito_codecs::filter::{ColorSpec, Order, Packed, downmix, packed_to_yuv, resample};
use pito_codecs::media::{
    self, AudioBuffer, Canvas, Codec, Error, ExportOptions, Frame, PixelFormat, Prefs, Route,
    TimeBase, VideoFrame, export_frames, wav,
};
use pito_codecs::png;

use super::Fail;
use super::convert::{ceiling_held, land, target_of};
use super::plan::{Loudness, partial};

pub const AAC_KBPS: u32 = 192;
const RATE: u32 = 48_000;
const POOL_BYTES: u64 = 1 << 30;
const SLACK_BYTES: u64 = 1 << 30;
const ARENA_BYTES: u64 = 64 << 20;

pub struct Job {
    pub dir: PathBuf,
    pub output: PathBuf,
    pub fps: (u32, u32),
    pub audio: Option<PathBuf>,
    pub loudness: Option<Loudness>,
    pub audio_kbps: u32,
    pub codec: Codec,
    pub prefs: Prefs,
    pub crf: u8,
    pub threads: usize,
    pub overwrite: bool,
}

fn listed(dir: &Path) -> Result<Vec<PathBuf>, Fail> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry?.path();
        let png = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if png && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(format!("{}: no .png files", dir.display()).into());
    }
    let numbers: Option<Vec<u64>> = files
        .iter()
        .map(|p| {
            let stem = p.file_stem()?.to_str()?;
            let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
            stem.get(stem.len() - digits..)?.parse().ok()
        })
        .collect();
    if let Some(numbers) = numbers {
        for (k, pair) in numbers.windows(2).enumerate() {
            if pair[1] != pair[0] + 1 {
                return Err(format!(
                    "{}: the frames jump from {} to {}",
                    dir.display(),
                    name(&files[k]),
                    name(&files[k + 1])
                )
                .into());
            }
        }
    }
    Ok(files)
}

fn address_limit() -> Option<u64> {
    let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
    let line = limits
        .lines()
        .find(|l| l.starts_with("Max address space"))?;
    line.trim_start_matches("Max address space")
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn sound(path: &Path) -> Result<AudioBuffer, Fail> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let data = std::fs::read(path).map_err(|e| at(&e))?;
    let mut buf = wav::decode(&data).map_err(|e| at(&e))?;
    buf = match buf.channels {
        1 => AudioBuffer {
            samples: buf.samples.iter().flat_map(|s| [*s, *s]).collect(),
            rate: buf.rate,
            channels: 2,
        },
        2 => buf,
        _ => downmix(&buf, 2).map_err(|e| at(&e))?,
    };
    if buf.rate != RATE {
        buf = resample(&buf, RATE).map_err(|e| at(&e))?;
    }
    Ok(buf)
}

struct Queue {
    next: usize,
    wanted: usize,
    ready: BTreeMap<usize, media::Result<Frame>>,
    spare: Vec<Frame>,
    stop: bool,
}

struct Pool {
    files: Vec<PathBuf>,
    size: (u32, u32),
    depth: usize,
    queue: Mutex<Queue>,
    moved: Condvar,
}

impl Pool {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn work(&self) {
        let mut decoder = png::Decoder::new();
        let mut rgba = media::Rgba::new(0, 0);
        let mut bytes = Vec::new();
        loop {
            let (index, spare) = {
                let mut q = self.lock();
                while !q.stop && q.next < self.files.len() && q.next >= q.wanted + self.depth {
                    q = self.moved.wait(q).unwrap_or_else(|e| e.into_inner());
                }
                if q.stop || q.next >= self.files.len() {
                    return;
                }
                q.next += 1;
                (q.next - 1, q.spare.pop())
            };
            let made = self.frame(index, spare, &mut decoder, &mut rgba, &mut bytes);
            self.lock().ready.insert(index, made);
            self.moved.notify_all();
        }
    }

    fn frame(
        &self,
        index: usize,
        spare: Option<Frame>,
        decoder: &mut png::Decoder,
        rgba: &mut media::Rgba,
        bytes: &mut Vec<u8>,
    ) -> media::Result<Frame> {
        let path = &self.files[index];
        let at = |e: &dyn std::fmt::Display| Error::Invalid(format!("{}: {e}", path.display()));
        bytes.clear();
        std::fs::File::open(path)
            .and_then(|mut f| f.read_to_end(bytes))
            .map_err(|e| at(&e))?;
        decoder.decode_into(bytes, rgba).map_err(|e| at(&e))?;
        if (rgba.width, rgba.height) != self.size {
            return Err(at(&format_args!(
                "the frame is {}x{}, the first is {}x{}",
                rgba.width, rgba.height, self.size.0, self.size.1
            )));
        }
        let mut frame = spare.unwrap_or_else(|| {
            Frame::new(
                PixelFormat::Yuv420p8,
                self.size.0,
                self.size.1,
                0,
                TimeBase::new(1, 1),
            )
        });
        let source = Packed::new(rgba.width, rgba.height, Order::Rgbx, &rgba.data);
        packed_to_yuv(source, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut frame)
            .map_err(|e| at(&e))?;
        Ok(frame)
    }

    fn take(&self, index: usize, spent: Option<Frame>) -> media::Result<Frame> {
        let mut q = self.lock();
        if let Some(f) = spent {
            q.spare.push(f);
        }
        q.wanted = index;
        self.moved.notify_all();
        loop {
            if let Some(made) = q.ready.remove(&index) {
                q.wanted = index + 1;
                self.moved.notify_all();
                return made;
            }
            q = self.moved.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn halt(&self) {
        self.lock().stop = true;
        self.moved.notify_all();
    }
}

fn shown(s: &media::LoudnessStats) -> String {
    format!(
        "{:.2} LUFS, LRA {:.2} LU, true peak {:.2} dBTP",
        s.integrated, s.range, s.true_peak
    )
}

pub fn run(out: &mut impl Write, job: &Job) -> Result<(), Fail> {
    if let Some(l) = &job.loudness {
        l.check()?;
    }
    if !job.overwrite && job.output.exists() {
        return Err(format!(
            "{} already exists; use --force to replace it",
            job.output.display()
        )
        .into());
    }
    let files = listed(&job.dir)?;
    let first = std::fs::read(&files[0]).map_err(|e| format!("{}: {e}", files[0].display()))?;
    let info = png::info(&first).map_err(|e| format!("{}: {e}", files[0].display()))?;
    let size = (info.width, info.height);
    if !size.0.is_multiple_of(2) || !size.1.is_multiple_of(2) {
        return Err(format!(
            "{}: the frames are {}x{}; 4:2:0 video needs an even width and height",
            job.dir.display(),
            size.0,
            size.1
        )
        .into());
    }
    let pixels = size.0 as u64 * size.1 as u64;
    let per_thread = pixels * 8 + 2 * first.len() as u64 + pixels * 3;
    let threads = job.threads.min((POOL_BYTES / per_thread) as usize).max(1);
    let audio_bytes = match &job.audio {
        Some(path) => std::fs::metadata(path).map_or(0, |m| m.len()) * 4,
        None => 0,
    };
    let need = threads as u64 * per_thread
        + pito_codecs::h264::Encoder::memory(size.0, size.1, 0)
        + audio_bytes
        + SLACK_BYTES
        + ARENA_BYTES * std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    if let Some(limit) = address_limit()
        && need > limit
    {
        return Err(format!(
            "{}: {}x{} frames need about {:.1} GB of memory, over this process's {:.1} GB address-space limit",
            job.dir.display(),
            size.0,
            size.1,
            need as f64 / 1e9,
            limit as f64 / 1e9
        )
        .into());
    }
    let count = files.len();
    let (num, den) = job.fps;
    let seconds = count as f64 * den as f64 / num as f64;
    let shown_fps = num as f64 / den as f64;
    writeln!(
        out,
        "frames  {count} PNG {}x{} at {} fps ({seconds:.2} s), {} decode threads",
        size.0,
        size.1,
        trim(shown_fps),
        threads
    )?;
    let pcm = match &job.audio {
        Some(path) => {
            let mut buf = sound(path)?;
            writeln!(
                out,
                "audio   {}: {:.2} s, AAC 48 kHz stereo at {} kbps",
                name(path),
                buf.frames() as f64 / RATE as f64,
                job.audio_kbps
            )?;
            if let Some(l) = &job.loudness {
                let style = media::LevelStyle {
                    linear: l.linear,
                    encoded: Some((Codec::Aac, job.audio_kbps)),
                };
                let done = media::level_pcm(&buf, &target_of(l), style)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                writeln!(out, "before  {}", shown(&done.before))?;
                let how = match (done.mode, l.linear) {
                    (media::LevelMode::Linear, _) => "linear gain".to_string(),
                    (_, true) => format!(
                        "linear gain, peaks limited at {:.2} dBTP",
                        done.ceiling_db.unwrap_or(l.true_peak)
                    ),
                    (_, false) => format!(
                        "gain and limiter at {:.2} dBTP",
                        done.ceiling_db.unwrap_or(l.true_peak)
                    ),
                };
                writeln!(
                    out,
                    "level   {:+.2} dB, {how} (target {} LUFS, ceiling {} dBTP)",
                    done.gain_db, l.integrated, l.true_peak
                )?;
                if let Some(s) = done.calibration.and_then(|c| c.shortfall(&target_of(l))) {
                    writeln!(out, "short   {s}")?;
                }
                if let Some(e) = &done.encoded {
                    writeln!(out, "check   {} (a trial AAC)", shown(e))?;
                }
                buf = done.buffer;
            }
            Some(buf)
        }
        None => {
            writeln!(out, "audio   none, a silent AAC track")?;
            None
        }
    };
    out.flush()?;

    let pool = Pool {
        files,
        size,
        depth: threads * 2,
        queue: Mutex::new(Queue {
            next: 0,
            wanted: 0,
            ready: BTreeMap::new(),
            spare: Vec::new(),
            stop: false,
        }),
        moved: Condvar::new(),
    };
    let canvas = Canvas {
        width: size.0,
        height: size.1,
        fps: job.fps,
        frames: Some(count as u64),
    };
    let opts = ExportOptions {
        video: job.codec,
        crf: job.crf,
        audio_kbps: job.audio_kbps,
        prefs: job.prefs.clone(),
        ..ExportOptions::default()
    };
    let live = std::io::stderr().is_terminal();
    let start = Instant::now();
    let mut played = 0usize;
    let audio = move |_upto: u64, n: usize| -> media::Result<AudioBuffer> {
        let mut samples = vec![0.0f32; n * 2];
        if let Some(buf) = &pcm {
            let from = (played * 2).min(buf.samples.len());
            let to = ((played + n) * 2).min(buf.samples.len());
            samples[..to - from].copy_from_slice(&buf.samples[from..to]);
        }
        played += n;
        Ok(AudioBuffer {
            samples,
            rate: RATE,
            channels: 2,
        })
    };
    let part = Partial(partial(&job.output)?);
    let made = std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| pool.work());
        }
        let result = export_frames(
            &part.0,
            canvas,
            opts,
            |n, spent| {
                let index = n as usize;
                if index >= count {
                    return Ok(None);
                }
                if live && index.is_multiple_of(15) {
                    eprint!("\rpcod: frame {index} of {count}");
                }
                pool.take(index, spent).map(|f| Some(VideoFrame::Cpu(f)))
            },
            audio,
            |_| true,
        );
        pool.halt();
        result
    });
    if live {
        eprint!("\r\x1b[K");
    }
    let done = made.map_err(|e| format!("{}: {e}", job.output.display()))?;
    let took = start.elapsed().as_secs_f64().max(1e-9);
    if !done.reasons.is_empty() {
        writeln!(out, "note    {}", done.reasons.join("; "))?;
    }
    let route = match done.video.route {
        Route::Gpu => "GPU",
        Route::Cpu => "CPU",
    };
    writeln!(out, "encode  {} {route}", done.video.codec)?;
    writeln!(
        out,
        "encoded {} frames in {took:.2} s: {:.0} fps",
        done.frames,
        done.frames as f64 / took
    )?;
    land_held(out, &part.0, job, |p| {
        media::measure_loudness(p, 0, &Prefs::default(), |_| true)
    })
}

fn land_held(
    out: &mut impl Write,
    part: &Path,
    job: &Job,
    measure: impl FnOnce(&Path) -> media::Result<media::LoudnessStats>,
) -> Result<(), Fail> {
    if let Some(l) = &job.loudness {
        let after = measure(part).map_err(|e| format!("{}: {e}", job.output.display()))?;
        writeln!(out, "after   {} (the MP4's AAC)", shown(&after))?;
        ceiling_held(after.true_peak, l.true_peak)
            .map_err(|e| format!("{}: {e}", job.output.display()))?;
    }
    let size = std::fs::metadata(part)?.len() as f64 / 1e6;
    land(part, &job.output, job.overwrite)?;
    writeln!(out, "wrote   {} ({size:.1} MB)", job.output.display())?;
    Ok(())
}

struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn trim(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dir: &Path, overwrite: bool) -> Job {
        Job {
            dir: dir.to_path_buf(),
            output: dir.join("out.mp4"),
            fps: (24, 1),
            audio: None,
            loudness: Some(Loudness {
                integrated: -16.0,
                true_peak: -1.0,
                linear: false,
            }),
            audio_kbps: 320,
            codec: Codec::H264,
            prefs: Prefs::default(),
            crf: 23,
            threads: 1,
            overwrite,
        }
    }

    fn peak(true_peak: f64) -> media::LoudnessStats {
        media::LoudnessStats {
            integrated: -16.0,
            range: 0.0,
            true_peak,
            threshold: -70.0,
        }
    }

    fn fresh(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/unit-frames-land")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_held_ceiling_measures_the_partial_then_lands_it() {
        let dir = fresh("held");
        let part = dir.join("part.mp4");
        std::fs::write(&part, b"new").unwrap();
        let mut said = Vec::new();
        let mut measured = None;
        land_held(&mut said, &part, &job(&dir, false), |p| {
            measured = Some(p.to_path_buf());
            Ok(peak(-1.004))
        })
        .unwrap();
        assert_eq!(measured.as_deref(), Some(part.as_path()));
        assert_eq!(std::fs::read(dir.join("out.mp4")).unwrap(), b"new");
        assert!(!part.exists());
        let said = String::from_utf8(said).unwrap();
        assert!(
            said.starts_with(
                "after   -16.00 LUFS, LRA 0.00 LU, true peak -1.00 dBTP (the MP4's AAC)\nwrote   "
            ),
            "{said}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missed_ceiling_writes_nothing() {
        let dir = fresh("missed");
        let part = dir.join("part.mp4");
        std::fs::write(&part, b"new").unwrap();
        let e = land_held(&mut Vec::new(), &part, &job(&dir, false), |_| {
            Ok(peak(-0.5))
        })
        .unwrap_err()
        .to_string();
        assert!(
            e.ends_with("out.mp4: the encoded audio peaks at -0.50 dBTP, over the -1 dBTP ceiling, so nothing was written; ask for a lower --true-peak or a higher --audio-bitrate"),
            "{e}"
        );
        assert!(!e.contains('\n'));
        assert!(!dir.join("out.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missed_ceiling_with_force_keeps_the_old_output() {
        let dir = fresh("forced");
        let part = dir.join("part.mp4");
        std::fs::write(&part, b"new").unwrap();
        std::fs::write(dir.join("out.mp4"), b"old render").unwrap();
        let e = land_held(&mut Vec::new(), &part, &job(&dir, true), |_| Ok(peak(0.2)));
        assert!(
            e.unwrap_err()
                .to_string()
                .contains("over the -1 dBTP ceiling")
        );
        assert_eq!(std::fs::read(dir.join("out.mp4")).unwrap(), b"old render");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_measure_is_refused_and_lands_nothing() {
        let dir = fresh("unmeasured");
        let part = dir.join("part.mp4");
        std::fs::write(&part, b"new").unwrap();
        let e = land_held(&mut Vec::new(), &part, &job(&dir, true), |_| {
            Err(Error::Unsupported("no audio track".into()))
        })
        .unwrap_err()
        .to_string();
        assert!(
            e.starts_with(&dir.join("out.mp4").display().to_string()),
            "{e}"
        );
        assert!(!dir.join("out.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_loudness_nothing_is_measured() {
        let dir = fresh("plain");
        let part = dir.join("part.mp4");
        std::fs::write(&part, b"new").unwrap();
        let mut plain = job(&dir, false);
        plain.loudness = None;
        let mut said = Vec::new();
        land_held(&mut said, &part, &plain, |_| panic!("measured")).unwrap();
        assert!(String::from_utf8(said).unwrap().starts_with("wrote   "));
        assert_eq!(std::fs::read(dir.join("out.mp4")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
