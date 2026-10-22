use std::collections::VecDeque;

use crate::media::{Error, Frame, Packet, PixelFormat, Plane, Result, TimeBase};
use crate::vp9::{Decoder, Encoder, EncoderConfig};

pub struct AlphaEncoder {
    color: Encoder,
    alpha: Encoder,
    plane: Frame,
}

impl AlphaEncoder {
    pub fn new(cfg: EncoderConfig) -> Result<AlphaEncoder> {
        let color = Encoder::new(cfg.clone())?;
        let alpha = Encoder::new(cfg.clone())?;
        let mut plane = Frame::new(
            PixelFormat::Yuv420p8,
            cfg.width,
            cfg.height,
            0,
            TimeBase::new(1, 1),
        );
        plane.planes[1].data.fill(128);
        plane.planes[2].data.fill(128);
        Ok(AlphaEncoder {
            color,
            alpha,
            plane,
        })
    }

    pub fn set_color(&mut self, color_space: u8, full_range: bool) -> Result<()> {
        self.color.set_color(color_space, full_range)
    }

    pub fn color(&self) -> (u8, bool) {
        self.color.color()
    }

    pub fn alpha_frame(&mut self) -> &mut Plane {
        &mut self.plane.planes[0]
    }

    pub fn encode(&mut self, frame: &Frame, alpha: &Plane) -> Result<Vec<(Packet, Vec<u8>)>> {
        let (w, h) = (self.plane.width as usize, self.plane.height as usize);
        if alpha.width < w
            || alpha.height < h
            || alpha.stride < w
            || alpha.data.len() < alpha.stride * (h - 1) + w
        {
            return Err(Error::Invalid(format!(
                "vp9 alpha: a {}x{} alpha plane for a {w}x{h} stream",
                alpha.width, alpha.height
            )));
        }
        let dst = &mut self.plane.planes[0];
        for y in 0..h {
            dst.data[y * dst.stride..y * dst.stride + w]
                .copy_from_slice(&alpha.data[y * alpha.stride..y * alpha.stride + w]);
        }
        self.encode_filled(frame)
    }

    pub fn encode_filled(&mut self, frame: &Frame) -> Result<Vec<(Packet, Vec<u8>)>> {
        self.plane.pts = frame.pts;
        self.plane.time_base = frame.time_base;
        let (color, alpha, plane) = (&mut self.color, &mut self.alpha, &self.plane);
        let (c, a) = std::thread::scope(|s| {
            let a = s.spawn(|| alpha.encode(plane));
            let c = color.encode(frame);
            let a = a.join().unwrap_or_else(|_| {
                Err(Error::Backend("vp9 alpha: the alpha encoder failed".into()))
            });
            (c, a)
        });
        pair(c?, a?)
    }

    pub fn flush(&mut self) -> Result<Vec<(Packet, Vec<u8>)>> {
        pair(self.color.flush()?, self.alpha.flush()?)
    }
}

fn pair(color: Vec<Packet>, alpha: Vec<Packet>) -> Result<Vec<(Packet, Vec<u8>)>> {
    if color.len() != alpha.len() {
        return Err(Error::Backend(format!(
            "vp9 alpha: {} colour packets against {} alpha packets",
            color.len(),
            alpha.len()
        )));
    }
    color
        .into_iter()
        .zip(alpha)
        .map(|(c, a)| {
            if c.keyframe != a.keyframe {
                return Err(Error::Backend(
                    "vp9 alpha: the colour and alpha keyframes differ".into(),
                ));
            }
            Ok((c, a.data))
        })
        .collect()
}

pub struct AlphaDecoder {
    color: Decoder,
    alpha: Decoder,
    colors: VecDeque<Frame>,
    alphas: VecDeque<Frame>,
    missing: Vec<i64>,
    broken: bool,
}

impl Default for AlphaDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl AlphaDecoder {
    pub fn new() -> Self {
        Self::with_threads(crate::media::default_threads())
    }

    pub fn with_threads(threads: usize) -> Self {
        let alpha = threads.div_ceil(2).max(1);
        AlphaDecoder {
            color: Decoder::with_threads(threads.max(1)),
            alpha: Decoder::with_threads(alpha),
            colors: VecDeque::new(),
            alphas: VecDeque::new(),
            missing: Vec::new(),
            broken: false,
        }
    }

    pub fn set_time_base(&mut self, time_base: TimeBase) {
        self.color.set_time_base(time_base);
        self.alpha.set_time_base(time_base);
    }

    pub fn reset(&mut self) {
        self.color.reset();
        self.alpha.reset();
        self.colors.clear();
        self.alphas.clear();
        self.missing.clear();
        self.broken = false;
    }

    pub fn decode(
        &mut self,
        packet: &Packet,
        alpha: Option<&[u8]>,
    ) -> Result<Vec<(Frame, Option<Plane>)>> {
        let alpha = alpha.filter(|a| !a.is_empty() && (packet.keyframe || !self.broken));
        if alpha.is_none() && !self.broken {
            self.settle();
        }
        self.broken = alpha.is_none();
        let (color, other) = (&mut self.color, &mut self.alpha);
        let (c, a) = match alpha {
            Some(data) => {
                let ap = Packet {
                    data: data.to_vec(),
                    pts: packet.pts,
                    dts: packet.dts,
                    keyframe: packet.keyframe,
                };
                std::thread::scope(|s| {
                    let a = s.spawn(move || other.decode(&ap));
                    let c = color.decode(packet);
                    let a = a.join().unwrap_or_else(|_| {
                        Err(Error::Backend("vp9 alpha: the alpha decoder failed".into()))
                    });
                    (c, Some(a))
                })
            }
            None => {
                self.missing.push(packet.pts);
                (color.decode(packet), None)
            }
        };
        if matches!(a, Some(Err(_))) {
            self.broken = true;
            self.settle();
        }
        self.colors.extend(c?);
        if let Some(Ok(a)) = a {
            self.alphas.extend(a);
        }
        self.matched(false)
    }

    pub fn flush(&mut self) -> Result<Vec<(Frame, Option<Plane>)>> {
        let c = self.color.flush();
        let a = self.alpha.flush();
        self.colors.extend(c?);
        if let Ok(a) = a {
            self.alphas.extend(a);
        }
        let out = self.matched(true);
        self.missing.clear();
        self.alphas.clear();
        out
    }

    fn settle(&mut self) {
        if let Ok(a) = self.alpha.flush() {
            self.alphas.extend(a);
        }
        self.alpha.reset();
    }

    fn matched(&mut self, last: bool) -> Result<Vec<(Frame, Option<Plane>)>> {
        let mut out = Vec::new();
        while let Some(pts) = self.colors.front().map(|f| f.pts) {
            if let Some(i) = self.missing.iter().position(|&m| m == pts) {
                self.missing.swap_remove(i);
                if let Some(f) = self.colors.pop_front() {
                    out.push((f, None));
                }
                continue;
            }
            while self.alphas.front().is_some_and(|a| a.pts < pts) {
                self.alphas.pop_front();
            }
            let plane = match self.alphas.front().map(|a| a.pts) {
                Some(p) if p == pts => self.alphas.pop_front().map(|a| a.planes),
                Some(_) => None,
                None if last || self.broken => None,
                None => break,
            };
            let Some(f) = self.colors.pop_front() else {
                break;
            };
            let plane = match plane {
                Some([y, _, _]) => {
                    if (y.width, y.height) != (f.width as usize, f.height as usize) {
                        return Err(Error::Invalid(format!(
                            "vp9 alpha: a {}x{} alpha picture for a {}x{} frame",
                            y.width, y.height, f.width, f.height
                        )));
                    }
                    if y.stride != y.width {
                        return Err(Error::Unsupported(
                            "vp9 alpha: a high bit depth alpha stream".into(),
                        ));
                    }
                    Some(y)
                }
                None => None,
            };
            out.push((f, plane));
        }
        Ok(out)
    }
}
