use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use super::export::{ExportOptions, Exported, MAX_FPS, Progress, Stage, WRITTEN};
use super::mux::{
    RATE, Shape, Sink, Spill, VEnc, aac_encoder, check_at, encoder_for, joining, seal,
};
use super::picture::Blender;
use super::route::{Caps, Choice, Picked, Prefs, pick_encode};
use super::video::VideoFrame;
use crate::media::{AudioBuffer, Codec, Error, Frame, PixelFormat, Result, Route, TimeBase};

#[cfg(target_os = "linux")]
pub type Readback = Box<dyn FnOnce() -> Result<VideoFrame>>;

#[cfg(target_os = "linux")]
pub struct DmaFrame {
    pub image: crate::gpu::DmaImage,
    pub readback: Option<Readback>,
}

pub enum FrameSource {
    Video(VideoFrame),
    #[cfg(target_os = "linux")]
    Dma(DmaFrame),
}

const AUDIO_STOPPED: &str = "the audio stopped";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub frames: Option<u64>,
}

fn samples_at(frames: u64, fps: (u32, u32)) -> u64 {
    let num = frames as u128 * RATE as u128 * fps.1 as u128;
    let den = (fps.0 as u128).max(1);
    u64::try_from((2 * num + den) / (2 * den)).unwrap_or(u64::MAX)
}

fn native_prefs(prefs: &Prefs) -> Prefs {
    Prefs {
        decode: Choice::Auto,
        encode: prefs.encode.clone(),
        hevc_encode_fallback: prefs.hevc_encode_fallback.clone(),
        pad_audio: prefs.pad_audio,
    }
}

fn open_encoder(shape: Shape, video: &mut Picked, crf: u8) -> Result<VEnc> {
    match encoder_for(shape, video, crf, false, 0, 0) {
        Ok(e) => Ok(e),
        Err(e) if video.route == Route::Gpu => {
            video.reasons.push(format!(
                "the GPU encoder failed ({e}); encoding H.264 on the CPU instead"
            ));
            video.route = Route::Cpu;
            video.codec = Codec::H264;
            encoder_for(shape, video, crf, false, 0, 0)
        }
        Err(e) => Err(e),
    }
}

fn checked(n: u64, frame: &Frame, size: (u32, u32)) -> Result<()> {
    if (frame.width, frame.height) != size {
        return Err(Error::Invalid(format!(
            "frame {n} is {}x{}, the export is {}x{}",
            frame.width, frame.height, size.0, size.1
        )));
    }
    if frame.format != PixelFormat::Yuv420p8 {
        return Err(Error::Invalid(format!(
            "frame {n} is {:?}, the export takes 8-bit 4:2:0",
            frame.format
        )));
    }
    crate::filter::check_frame(frame).map_err(|e| Error::Invalid(format!("frame {n}: {e}")))
}

pub fn export_frames(
    out: &Path,
    canvas: Canvas,
    opts: ExportOptions,
    mut frames: impl FnMut(u64, Option<Frame>) -> Result<Option<VideoFrame>>,
    audio: impl FnMut(u64, usize) -> Result<AudioBuffer> + Send,
    progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    export_sources(
        out,
        canvas,
        opts,
        |n, spent| Ok(frames(n, spent)?.map(FrameSource::Video)),
        audio,
        progress,
    )
}

#[cfg(target_os = "linux")]
pub fn export_dmabufs(
    out: &Path,
    canvas: Canvas,
    opts: ExportOptions,
    frames: impl FnMut(u64, Option<Frame>) -> Result<Option<FrameSource>>,
    audio: impl FnMut(u64, usize) -> Result<AudioBuffer> + Send,
    progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    export_sources(out, canvas, opts, frames, audio, progress)
}

fn export_sources(
    out: &Path,
    canvas: Canvas,
    mut opts: ExportOptions,
    mut frames: impl FnMut(u64, Option<Frame>) -> Result<Option<FrameSource>>,
    mut audio: impl FnMut(u64, usize) -> Result<AudioBuffer> + Send,
    mut progress: impl FnMut(&Progress) -> bool,
) -> Result<Exported> {
    let size = (canvas.width, canvas.height);
    let fps = canvas.fps;
    if size.0 == 0 || size.1 == 0 || !size.0.is_multiple_of(2) || !size.1.is_multiple_of(2) {
        return Err(Error::Invalid(format!(
            "a picture of {}x{}; the width and height must be even",
            size.0, size.1
        )));
    }
    if fps.0 == 0 || fps.1 == 0 || fps.0 / fps.1 > MAX_FPS {
        return Err(Error::Invalid(format!(
            "a frame rate of {}/{}",
            fps.0, fps.1
        )));
    }
    if !matches!(opts.video, Codec::H264 | Codec::Hevc) {
        return Err(Error::Unsupported(format!(
            "frames export to H.264 or HEVC, not {}",
            opts.video
        )));
    }
    let mut reasons = Vec::new();
    let prefs = native_prefs(&opts.prefs);
    let caps = Caps::detect(&prefs);
    let mut video = pick_encode(opts.video, &prefs, &caps)?;
    let shape = (size, fps, PixelFormat::Yuv420p8);
    let mut enc = open_encoder(shape, &mut video, opts.crf)?;
    for r in &video.reasons {
        if !reasons.contains(r) {
            reasons.push(r.clone());
        }
    }
    let work = std::path::absolute(out.with_extension("parts"))?;
    if work.exists() {
        std::fs::remove_dir_all(&work).map_err(|e| Error::at(&work, e))?;
    }
    std::fs::create_dir_all(&work).map_err(|e| Error::at(&work, e))?;
    let part = work.join("joined.mp4");
    let route = video.route;
    let result = (|| -> Result<u64> {
        let mut aac = aac_encoder(opts.audio_kbps)?;
        let timescale = fps.0.saturating_mul(64);
        let fd = fps.1 as i64 * 64;
        let mut sink = Sink::new(video.codec, size, 0, (timescale, fd), &part);
        let stop = AtomicBool::new(false);
        let batch = u64::from((fps.0 / fps.1).max(1));
        let (tx, rx) = mpsc::sync_channel::<u64>(4);
        let (count, sound) = std::thread::scope(|s| -> Result<_> {
            let (aac, stop, work) = (&mut aac, &stop, &work);
            let sound = s.spawn(move || -> Result<(Spill, u64)> {
                crate::threads::enter();
                let mut packets = Spill::create(work, "premixed.aac")?;
                let mut first = 0u64;
                for upto in rx {
                    if stop.load(Ordering::Relaxed) {
                        return Err(Error::Cancelled);
                    }
                    let end = samples_at(upto, fps);
                    let n = usize::try_from(end.saturating_sub(first))
                        .map_err(|_| Error::Invalid("too much audio for one call".into()))?;
                    let buf = audio(upto, n)?;
                    check_at(&format_args!("the audio to frame {upto}"), "it", n, &buf)?;
                    packets.extend(aac.encode(&buf)?)?;
                    first = end;
                }
                if stop.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                packets.extend(aac.flush())?;
                Ok((packets, first))
            });
            let mut pictures = || -> Result<u64> {
                let time_base = TimeBase::new(fps.1, fps.0);
                let mut blender = Blender::new();
                let mut spent: Option<Frame> = None;
                #[cfg(target_os = "linux")]
                let mut dma_off: Option<String> = None;
                #[cfg(target_os = "linux")]
                let mut overlay_noted = false;
                let mut n = 0u64;
                let tell = |n: u64, progress: &mut dyn FnMut(&Progress) -> bool| {
                    let done = match canvas.frames {
                        Some(t) if t > 0 => (n as f32 / t as f32).min(1.0) * WRITTEN,
                        _ => 0.0,
                    };
                    let keep = progress(&Progress {
                        stage: Stage::Encoding,
                        done,
                        encoder: route,
                    });
                    if keep { Ok(()) } else { Err(Error::Cancelled) }
                };
                tell(0, &mut progress)?;
                while canvas.frames.is_none_or(|t| n < t)
                    && let Some(frame) = frames(n, spent.take())?
                {
                    let seconds = n as f64 * fps.1 as f64 / fps.0 as f64;
                    let layer = match opts.overlay.as_mut() {
                        Some(o) => o(seconds, size.0, size.1),
                        None => None,
                    };
                    let pts = i64::try_from(n).unwrap_or(i64::MAX);
                    let frame = match frame {
                        FrameSource::Video(v) => Ok(v),
                        #[cfg(target_os = "linux")]
                        FrameSource::Dma(d) => {
                            if (d.image.width, d.image.height) != size {
                                return Err(Error::Invalid(format!(
                                    "frame {n} is {}x{}, the export is {}x{}",
                                    d.image.width, d.image.height, size.0, size.1
                                )));
                            }
                            if dma_off.is_none()
                                && let Err(why) = enc.takes_dmabuf()
                            {
                                dma_off = Some(why);
                            }
                            if layer.is_some() && !overlay_noted {
                                overlay_noted = true;
                                let r = "an overlay is drawn on the CPU, so dmabuf frames are read back to it"
                                    .to_string();
                                if !reasons.contains(&r) {
                                    reasons.push(r);
                                }
                            }
                            let direct = if dma_off.is_none() && layer.is_none() {
                                match enc.encode_dmabuf(&d.image, pts, time_base) {
                                    Ok(p) => Some(p),
                                    Err(Error::Unsupported(e)) => {
                                        dma_off = Some(e);
                                        None
                                    }
                                    Err(e) => return Err(e),
                                }
                            } else {
                                None
                            };
                            if let Some(why) = &dma_off {
                                let r = format!(
                                    "dmabuf frames are read back to the CPU instead: {why}"
                                );
                                if !reasons.contains(&r) {
                                    reasons.push(r);
                                }
                            }
                            match direct {
                                Some(p) => Err(p),
                                None => match d.readback {
                                    Some(read) => Ok(read()?),
                                    None => {
                                        return Err(Error::Unsupported(format!(
                                            "frame {n} is a dmabuf the encoder could not take ({}), and the frame has no readback",
                                            dma_off
                                                .as_deref()
                                                .unwrap_or("an overlay is drawn on the CPU")
                                        )));
                                    }
                                },
                            }
                        }
                    };
                    let packets = match frame {
                        Err(packets) => packets,
                        Ok(VideoFrame::Gpu(mut g)) if layer.is_none() && enc.on_gpu() => {
                            if g.size() != size {
                                return Err(Error::Invalid(format!(
                                    "frame {n} is {}x{}, the export is {}x{}",
                                    g.size().0,
                                    g.size().1,
                                    size.0,
                                    size.1
                                )));
                            }
                            g.retime(pts, time_base);
                            enc.encode_gpu(&g)?
                        }
                        Ok(other) => {
                            let mut f = match other {
                                VideoFrame::Cpu(f) => f,
                                VideoFrame::Gpu(g) => g.download()?,
                            };
                            checked(n, &f, size)?;
                            f.pts = pts;
                            f.time_base = time_base;
                            if let Some(layer) = &layer {
                                blender.draw(&mut f, layer);
                            }
                            let p = enc.encode(&f)?;
                            spent = Some(f);
                            p
                        }
                    };
                    sink.feed(&enc, packets)?;
                    n += 1;
                    if n.is_multiple_of(batch) && tx.send(n).is_err() {
                        return Err(Error::Backend(AUDIO_STOPPED.to_string()));
                    }
                    if n.is_multiple_of(15) {
                        tell(n, &mut progress)?;
                    }
                }
                if n == 0 {
                    return Err(Error::Invalid("no frames to export".to_string()));
                }
                if !n.is_multiple_of(batch) && tx.send(n).is_err() {
                    return Err(Error::Backend(AUDIO_STOPPED.to_string()));
                }
                let packets = enc.flush()?;
                sink.feed(&enc, packets)?;
                Ok(n)
            };
            let made = pictures();
            if made.is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            drop(tx);
            let sound = sound
                .join()
                .unwrap_or_else(|_| Err(Error::Backend("the audio failed".to_string())));
            match (made, sound) {
                (Ok(n), Ok(sound)) => Ok((n, sound)),
                (Err(Error::Backend(m)), Err(e)) if m == AUDIO_STOPPED => Err(e),
                (Err(e), _) | (Ok(_), Err(e)) => Err(e),
            }
        })?;
        let mux = sink.done()?;
        if !progress(&joining(0.0, route)) {
            return Err(Error::Cancelled);
        }
        let (packets, total) = sound;
        seal(
            mux,
            &aac,
            opts.audio_kbps,
            (packets, Some(total)),
            (&part, out),
            &mut |f| progress(&joining(f, route)),
        )?;
        Ok(count)
    })();
    let _ = std::fs::remove_dir_all(&work);
    let count = result?;
    if !progress(&Progress {
        stage: Stage::Joining,
        done: 1.0,
        encoder: route,
    }) {
        return Err(Error::Cancelled);
    }
    Ok(Exported {
        path: out.to_path_buf(),
        frames: count,
        copied: 0,
        encoded: count,
        smart: false,
        video,
        reasons,
        sessions: Default::default(),
    })
}
