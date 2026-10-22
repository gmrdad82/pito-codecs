use std::collections::HashMap;
use std::sync::Arc;

use super::demux::Demux;
use super::export::{Job, Overlay, Stage, Tell, convert_depth};
use super::mux::VEnc;
use super::native;
use super::picture::{Blender, resize, scale};
use super::video::IN_FLIGHT;
use crate::gpu::{GpuFrame, GpuScaler, HwDecoder};
use crate::media::{Error, Frame, Packet, Result, Rgba, Route, TimeBase};

const HELD: usize = 8;

pub(super) struct Resident {
    pub(super) on: bool,
    cpu: bool,
    blender: Blender,
    fitted: Option<(Arc<Rgba>, Arc<Rgba>)>,
    pub(super) reasons: Vec<String>,
    decoders: HashMap<usize, native::Decoder>,
    scaler: Option<((u32, u32), bool, GpuScaler)>,
}

impl Resident {
    pub(super) fn new(enc: &VEnc) -> Self {
        Resident {
            on: enc.composes(),
            cpu: false,
            blender: Blender::new(),
            fitted: None,
            reasons: Vec::new(),
            decoders: HashMap::new(),
            scaler: None,
        }
    }

    pub(super) fn renew(&mut self, enc: &VEnc) {
        self.on = enc.composes();
        self.cpu = false;
    }

    fn frames(&mut self, job: &Job, si: usize, demux: &Demux, from: i64) -> Result<native::Frames> {
        let dec = match self.decoders.remove(&si) {
            Some(mut dec) => {
                if let native::Decoder::Gpu(d) = &mut dec {
                    d.reset();
                }
                dec
            }
            None => {
                let st = demux.stream(demux.video()?)?;
                let codec = st
                    .codec
                    .ok_or_else(|| Error::Unsupported("an unknown video codec".to_string()))?;
                let d = HwDecoder::on(job.device.as_ref(), codec, &st.extradata, HELD)?;
                native::Decoder::Gpu(Box::new(d.with_time_base(st.time_base)))
            }
        };
        native::Frames::with_decoder(demux.clone(), job.seconds(from), dec)
    }

    fn scaler(&mut self, job: &Job, ten: bool) -> Result<&GpuScaler> {
        let fits = self
            .scaler
            .as_ref()
            .is_some_and(|(size, t, _)| *size == job.size && *t == ten);
        if !fits {
            self.scaler = None;
            let s = GpuScaler::on(job.device.as_ref(), job.size.0, job.size.1, ten, IN_FLIGHT)?;
            self.scaler = Some((job.size, ten, s));
        }
        self.scaler
            .as_ref()
            .map(|(_, _, s)| s)
            .ok_or_else(|| Error::Backend("no GPU scaler".to_string()))
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
        out: &mut dyn FnMut(&VEnc, Vec<Packet>) -> Result<()>,
        overlay: &mut Option<Overlay>,
        progress: Tell,
    ) -> Result<bool> {
        if frames <= 0 {
            return Ok(true);
        }
        let Some(mut frames_in) = self.open(job, si, from) else {
            return Ok(false);
        };
        let done = self.run(
            job,
            si,
            &mut frames_in,
            frames,
            at,
            enc,
            out,
            overlay,
            progress,
        )?;
        self.close(si, frames_in);
        Ok(done)
    }

    pub(super) fn open(&mut self, job: &Job, si: usize, from: i64) -> Option<native::Frames> {
        let d = job.sources[si].demux.clone();
        let opened = self.frames(job, si, &d, from);
        match opened {
            Ok(f) if f.route() == Route::Gpu => Some(f),
            Ok(_) => {
                self.on = false;
                self.note("the video doesn't decode on the GPU here, so its frames are drawn and encoded from system memory".to_string());
                None
            }
            Err(e) => {
                self.on = false;
                self.note(format!(
                    "the GPU decoder failed ({e}); frames are drawn and encoded from system memory"
                ));
                None
            }
        }
    }

    pub(super) fn close(&mut self, si: usize, frames_in: native::Frames) {
        self.decoders.insert(si, frames_in.into_decoder());
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run(
        &mut self,
        job: &mut Job,
        si: usize,
        frames_in: &mut native::Frames,
        frames: i64,
        at: i64,
        enc: &mut VEnc,
        out: &mut dyn FnMut(&VEnc, Vec<Packet>) -> Result<()>,
        overlay: &mut Option<Overlay>,
        progress: Tell,
    ) -> Result<bool> {
        let src = &job.sources[si];
        let scaled = (src.probe.width, src.probe.height) != job.size;
        let ten = src.probe.bit_depth > 8;
        if scaled {
            match self.scaler(job, ten) {
                Ok(_) => {}
                Err(e) => {
                    self.on = false;
                    self.note(format!(
                        "no GPU scaler ({e}); frames are drawn and encoded from system memory"
                    ));
                    return Ok(false);
                }
            }
        }
        let scaler = if scaled { self.scaler.take() } else { None };
        self.note(if overlay.is_some() {
            "frames stay on the GPU from decode to encode, overlay included, through VA-API"
                .to_string()
        } else {
            "frames stay on the GPU from decode to encode through VA-API".to_string()
        });
        let time_base = TimeBase::new(job.fps.1, job.fps.0);
        let mut last: Option<Shown> = None;
        let mut n = 0i64;
        while n < frames {
            let next = match frames_in.next_gpu()? {
                Some(g) if g.on_gpu() => Some(Shown::Gpu(match &scaler {
                    Some((_, _, s)) => s.scale(&g)?,
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
            out(enc, packets)?;
            n += 1;
            job.done += 1;
            if n % 15 == 0 {
                job.tell(progress, Stage::Encoding)?;
            }
        }
        drop(last);
        if scaler.is_some() {
            self.scaler = scaler;
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
