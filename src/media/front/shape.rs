use std::collections::VecDeque;

use super::export::Job;
use super::video::{Source, Video};
use crate::filter::{
    ColorSpec, Fill, FpsConverter, Kernel, Matrix, Range, Rotation, Scaler, convert_color, pad,
    rotate,
};
use crate::gpu::Color;
use crate::media::{Error, Frame, PixelFormat, Result};

pub(super) const SAME_ASPECT: f64 = 0.01;

pub(super) trait Pull {
    fn pull(&mut self) -> Option<Result<Frame>>;
    fn give(&mut self, frame: Frame);
}

impl Pull for Video {
    fn pull(&mut self) -> Option<Result<Frame>> {
        self.next()
    }

    fn give(&mut self, frame: Frame) {
        self.recycle(frame);
    }
}

pub(super) fn color_of(src: &Source) -> ((u16, u16), Color) {
    let d = &src.demux;
    d.video()
        .ok()
        .and_then(|v| d.stream(v).ok()?.look)
        .unwrap_or(((1, 1), Color::UNKNOWN))
}

pub(super) fn spec(color: Color) -> ColorSpec {
    let matrix = match color.matrix {
        5 | 6 => Matrix::Bt601,
        9 | 10 => Matrix::Bt2020,
        _ => Matrix::Bt709,
    };
    let range = if color.full_range {
        Range::Full
    } else {
        Range::Limited
    };
    ColorSpec::new(matrix, range)
}

pub(super) fn display(src: &Source, sar: (u16, u16)) -> (u32, u32) {
    let (w, h) = (src.probe.width, src.probe.height);
    let w = if sar.0 > 0 && sar.1 > 0 && sar.0 != sar.1 {
        ((w as u64 * sar.0 as u64 + sar.1 as u64 / 2) / sar.1 as u64).max(1) as u32
    } else {
        w
    };
    if src.probe.rotation % 180 == 90 {
        (h, w)
    } else {
        (w, h)
    }
}

pub(super) fn fit(
    (source_width, source_height): (u32, u32),
    (width, height): (u32, u32),
) -> (u32, u32) {
    if source_width == 0 || source_height == 0 {
        return (width, height);
    }
    let (sw, sh) = (f64::from(source_width), f64::from(source_height));
    let (tw, th) = (f64::from(width), f64::from(height));
    let aspect = (sw / sh) / (tw / th);
    if (aspect - 1.0).abs() <= SAME_ASPECT {
        return (width, height);
    }
    let even = |value: f64| (((value / 2.0).round() as u32) * 2).max(2);
    if aspect > 1.0 {
        (width, even(tw * sh / sw).min(height))
    } else {
        (even(th * sw / sh).min(width), height)
    }
}

pub(super) fn reshapes(job: &Job, si: usize) -> bool {
    let src = &job.sources[si];
    let (sar, color) = color_of(src);
    let target = spec(color);
    (src.probe.width, src.probe.height) != job.size
        || !src.probe.rotation.is_multiple_of(360)
        || (sar.0 != sar.1 && sar.0 > 0 && sar.1 > 0)
        || src.probe.fps != job.fps
        || target != ColorSpec::BT709
}

pub(super) struct Shaped<'a> {
    video: &'a mut Video,
    fps: Option<FpsConverter>,
    ready: VecDeque<Frame>,
    flushed: bool,
    rotation: Option<Rotation>,
    source: (u32, u32),
    coded: (u32, u32),
    fitted: (u32, u32),
    size: (u32, u32),
    from: ColorSpec,
    scaler: Option<((u32, u32), PixelFormat, Scaler)>,
}

impl<'a> Shaped<'a> {
    pub(super) fn new(job: &Job, si: usize, video: &'a mut Video) -> Result<Self> {
        let src = &job.sources[si];
        let (sar, color) = color_of(src);
        let fitted = fit(display(src, sar), job.size);
        let rotation = match src.probe.rotation % 360 {
            90 => Some(Rotation::R90),
            180 => Some(Rotation::R180),
            270 => Some(Rotation::R270),
            _ => None,
        };
        let coded = if src.probe.rotation % 180 == 90 {
            (fitted.1, fitted.0)
        } else {
            fitted
        };
        let fps = if src.probe.fps == job.fps {
            None
        } else {
            Some(FpsConverter::new(job.fps.0, job.fps.1)?)
        };
        Ok(Shaped {
            video,
            fps,
            ready: VecDeque::new(),
            flushed: false,
            rotation,
            source: (src.probe.width, src.probe.height),
            coded,
            fitted,
            size: job.size,
            from: spec(color),
            scaler: None,
        })
    }

    fn next_raw(&mut self) -> Option<Result<Frame>> {
        loop {
            if let Some(f) = self.ready.pop_front() {
                return Some(Ok(f));
            }
            let Some(fps) = self.fps.as_mut() else {
                return self.video.pull();
            };
            match self.video.pull() {
                Some(Ok(f)) => self.ready.extend(fps.push(f)),
                Some(Err(e)) => return Some(Err(e)),
                None if self.flushed => return None,
                None => {
                    self.flushed = true;
                    self.ready.extend(fps.flush());
                }
            }
        }
    }

    fn swap(&mut self, frame: &mut Frame, out: Frame) {
        let old = std::mem::replace(frame, out);
        self.give(old);
    }

    fn shape(&mut self, frame: Frame) -> Result<Frame> {
        let mut frame = frame;
        if (frame.width, frame.height) != self.coded {
            let fits = self
                .scaler
                .as_ref()
                .is_some_and(|(s, f, _)| *s == (frame.width, frame.height) && *f == frame.format);
            if !fits {
                let s = Scaler::new(
                    frame.width,
                    frame.height,
                    self.coded.0,
                    self.coded.1,
                    frame.format,
                    Kernel::Lanczos,
                )?;
                self.scaler = Some(((frame.width, frame.height), frame.format, s));
            }
            let (_, _, s) = self
                .scaler
                .as_ref()
                .ok_or_else(|| Error::Backend("no scaler".to_string()))?;
            let mut out = Frame::new(
                frame.format,
                self.coded.0,
                self.coded.1,
                frame.pts,
                frame.time_base,
            );
            s.run(&frame, &mut out)?;
            self.swap(&mut frame, out);
        }
        if let Some(r) = self.rotation {
            let mut out = Frame::new(
                frame.format,
                self.fitted.0,
                self.fitted.1,
                frame.pts,
                frame.time_base,
            );
            rotate(&frame, r, &mut out)?;
            self.swap(&mut frame, out);
        }
        if self.from != ColorSpec::BT709 {
            let mut out = Frame::new(
                frame.format,
                frame.width,
                frame.height,
                frame.pts,
                frame.time_base,
            );
            convert_color(&frame, self.from, ColorSpec::BT709, frame.format, &mut out)?;
            self.swap(&mut frame, out);
        }
        if self.fitted != self.size {
            let x = ((self.size.0 - self.fitted.0) / 2) & !1;
            let y = ((self.size.1 - self.fitted.1) / 2) & !1;
            let mut out = Frame::new(
                frame.format,
                self.size.0,
                self.size.1,
                frame.pts,
                frame.time_base,
            );
            let fill = Fill::black(Range::Limited, frame.format);
            pad(&frame, self.size.0, self.size.1, x, y, fill, &mut out)?;
            self.swap(&mut frame, out);
        }
        Ok(frame)
    }
}

impl Pull for Shaped<'_> {
    fn pull(&mut self) -> Option<Result<Frame>> {
        match self.next_raw()? {
            Ok(f) => Some(self.shape(f)),
            Err(e) => Some(Err(e)),
        }
    }

    fn give(&mut self, frame: Frame) {
        if (frame.width, frame.height) == self.source {
            self.video.give(frame);
        }
    }
}
