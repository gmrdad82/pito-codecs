use std::sync::Arc;

use super::export::{Job, Overlay, Stage, Tell, convert_depth};
use super::mux::{Sink, VEnc};
use super::native;
use super::picture::{Blender, resize, scale};
use super::video::IN_FLIGHT;
use crate::gpu::{GpuFrame, GpuScaler};
use crate::media::{Error, Frame, Packet, Result, Rgba, Route, TimeBase};

pub(super) struct Resident {
    pub(super) on: bool,
    cpu: bool,
    blender: Blender,
    fitted: Option<(Arc<Rgba>, Arc<Rgba>)>,
    pub(super) reasons: Vec<String>,
}

impl Resident {
    pub(super) fn new(enc: &VEnc) -> Self {
        Resident {
            on: enc.composes(),
            cpu: false,
            blender: Blender::new(),
            fitted: None,
            reasons: Vec::new(),
        }
    }

    fn note(&mut self, why: String) {
        if !self.reasons.contains(&why) {
            self.reasons.push(why);
        }
    }

    fn fit(&mut self, layer: Arc<Rgba>, size: (u32, u32)) -> Arc<Rgba> {
        if (layer.width, layer.height) == size {
            return layer;
        }
        if let Some((held, fitted)) = &self.fitted
            && Arc::ptr_eq(held, &layer)
            && (fitted.width, fitted.height) == size
        {
            return fitted.clone();
        }
        let fitted = Arc::new(scale(&layer, size.0, size.1));
        self.fitted = Some((layer, fitted.clone()));
        fitted
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn piece(
        &mut self,
        job: &mut Job,
        si: usize,
        (from, frames): (i64, i64),
        at: i64,
        enc: &mut VEnc,
        out: &mut Sink,
        overlay: &mut Option<Overlay>,
        progress: Tell,
    ) -> Result<bool> {
        if frames <= 0 {
            return Ok(true);
        }
        let src = &job.sources[si];
        let opened = src
            .demux()
            .and_then(|d| native::Frames::open(d.clone(), job.seconds(from), Route::Gpu));
        let mut frames_in = match opened {
            Ok(f) if f.route() == Route::Gpu => f,
            Ok(_) => {
                self.on = false;
                self.note("the video doesn't decode on the GPU here, so its frames are drawn and encoded from system memory".to_string());
                return Ok(false);
            }
            Err(e) => {
                self.on = false;
                self.note(format!(
                    "the GPU decoder failed ({e}); frames are drawn and encoded from system memory"
                ));
                return Ok(false);
            }
        };
        let scaler = if (src.probe.width, src.probe.height) != job.size {
            match GpuScaler::with_frames(job.size.0, job.size.1, src.probe.bit_depth > 8, IN_FLIGHT)
            {
                Ok(s) => Some(s),
                Err(e) => {
                    self.on = false;
                    self.note(format!(
                        "no GPU scaler ({e}); frames are drawn and encoded from system memory"
                    ));
                    return Ok(false);
                }
            }
        } else {
            None
        };
        self.note(
            "frames stay on the GPU: decoded, overlaid and encoded there through VA-API"
                .to_string(),
        );
        let time_base = TimeBase::new(job.fps.1, job.fps.0);
        let mut last: Option<Shown> = None;
        let mut n = 0i64;
        while n < frames {
            let next = match frames_in.next_gpu()? {
                Some(g) if g.on_gpu() => Some(Shown::Gpu(match &scaler {
                    Some(s) => s.scale(&g)?,
                    None => g,
                })),
                Some(g) => g.into_cpu().map(Shown::Cpu),
                None => None,
            };
            if let Some(f) = next {
                last = Some(f);
            }
            let Some(frame) = last.as_mut() else {
                return Err(Error::Invalid("a piece with no frames".to_string()));
            };
            let pts = at + n;
            let layer = overlay
                .as_mut()
                .and_then(|o| o(job.seconds(pts), job.size.0, job.size.1))
                .map(|l| self.fit(l, job.size));
            let packets = match frame {
                Shown::Gpu(g) => {
                    g.retime(pts, time_base);
                    match layer {
                        None => enc.encode_gpu(g)?,
                        Some(l) if !self.cpu => match enc.encode_gpu_over(g, &l) {
                            Ok(p) => p,
                            Err(e) => {
                                self.cpu = true;
                                self.note(format!("the GPU couldn't draw the overlay ({e}); it is drawn on the CPU"));
                                self.on_cpu(job, &g.download()?, pts, Some(&l), enc)?
                            }
                        },
                        Some(l) => self.on_cpu(job, &g.download()?, pts, Some(&l), enc)?,
                    }
                }
                Shown::Cpu(f) => self.on_cpu(job, f, pts, layer.as_ref(), enc)?,
            };
            out.feed(enc, packets)?;
            n += 1;
            job.done += 1;
            if n % 15 == 0 {
                job.tell(progress, Stage::Encoding)?;
            }
        }
        Ok(true)
    }

    fn on_cpu(
        &mut self,
        job: &Job,
        frame: &Frame,
        pts: i64,
        layer: Option<&Arc<Rgba>>,
        enc: &mut VEnc,
    ) -> Result<Vec<Packet>> {
        let mut f = if frame.format != job.format {
            convert_depth(frame, job.format)
        } else {
            frame.clone()
        };
        if (f.width, f.height) != job.size {
            f = resize(&f, job.size.0, job.size.1);
        }
        f.pts = pts;
        f.time_base = TimeBase::new(job.fps.1, job.fps.0);
        if let Some(l) = layer {
            self.blender.draw(&mut f, l);
        }
        enc.encode(&f)
    }
}

enum Shown {
    Gpu(GpuFrame),
    Cpu(Frame),
}
