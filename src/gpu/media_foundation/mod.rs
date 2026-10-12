mod device;
mod mft;
mod stream;

use std::sync::{Arc, OnceLock};

use windows::Win32::Graphics::Direct3D11::{
    D3D11_DECODER_PROFILE_H264_VLD_NOFGT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
    D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10,
};
use windows::core::GUID;

use crate::gpu::nal::{self, Framing};
use crate::gpu::{Backend, DecoderBackend, Direction};
use crate::media::{Codec, Error, Frame, Packet, Result};

use crate::gpu::biplanar::Geometry;
use device::{Gpu, Readback, SurfaceFormat, ensure_com};
use mft::{MfApi, Mft, Poll, Spec};
use stream::Plan;

const MAX_IN_FLIGHT: usize = 64;
const MAX_OUTPUTS_PER_CALL: usize = 4096;

fn dxva_profile(codec: Codec, format: SurfaceFormat) -> GUID {
    match (codec, format) {
        (Codec::Hevc, SurfaceFormat::P010) => D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10,
        (Codec::Hevc, _) => D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
        _ => D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
    }
}

fn label(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "HEVC",
        _ => "H.264",
    }
}

fn probe() -> Vec<(Codec, Direction)> {
    let Ok(api) = mft::api() else {
        return Vec::new();
    };
    let Ok(gpu) = Gpu::shared(api) else {
        return Vec::new();
    };
    [Codec::H264, Codec::Hevc]
        .into_iter()
        .filter(|&codec| {
            gpu.supports(
                dxva_profile(codec, SurfaceFormat::Nv12),
                SurfaceFormat::Nv12,
                (1280, 720),
            )
            .is_ok()
                && api.has_decoder(codec)
        })
        .map(|codec| (codec, Direction::Decode))
        .collect()
}

pub fn available() -> Vec<(Codec, Direction)> {
    static PROBE: OnceLock<Vec<(Codec, Direction)>> = OnceLock::new();
    PROBE.get_or_init(probe).clone()
}

pub fn decoder(codec: Codec, extradata: &[u8]) -> Result<Box<dyn DecoderBackend>> {
    Ok(Box::new(MfDecoder::new(codec, extradata)?))
}

struct Session {
    plan: Plan,
    geometry: Geometry,
    mft: Option<Mft>,
}

pub struct MfDecoder {
    codec: Codec,
    framing: Framing,
    sets: Vec<Vec<u8>>,
    api: &'static MfApi,
    gpu: Arc<Gpu>,
    session: Option<Session>,
    readback: Readback,
    input: Vec<u8>,
    in_flight: Vec<i64>,
    need_headers: bool,
    waiting: bool,
    skip_rasl: bool,
    name: String,
}

impl MfDecoder {
    pub fn new(codec: Codec, extradata: &[u8]) -> Result<Self> {
        if !matches!(codec, Codec::H264 | Codec::Hevc) {
            return Err(Error::Unsupported(format!("{codec:?} on Media Foundation")));
        }
        ensure_com().map_err(Error::Backend)?;
        let config = nal::parse_extradata(codec, extradata)?;
        let api = mft::api().map_err(Error::Backend)?;
        let gpu = Gpu::shared(api).map_err(Error::Backend)?;
        let mut d = Self {
            codec,
            framing: config.framing,
            sets: config.parameter_sets,
            api,
            gpu,
            session: None,
            readback: Readback::default(),
            input: Vec::new(),
            in_flight: Vec::new(),
            need_headers: true,
            waiting: true,
            skip_rasl: false,
            name: format!("Media Foundation {}", label(codec)),
        };
        if d.sets.iter().any(|n| stream::is_sps(codec, n)) {
            d.open()?;
        } else if !available().contains(&(codec, Direction::Decode)) {
            return Err(Error::Unsupported(format!(
                "no hardware {} decoder on Media Foundation ({})",
                label(codec),
                d.gpu.name()
            )));
        }
        Ok(d)
    }

    fn check(&self, plan: &Plan) -> Result<()> {
        self.gpu
            .supports(
                dxva_profile(self.codec, plan.format),
                plan.format,
                plan.coded,
            )
            .map_err(Error::Unsupported)
    }

    fn open(&mut self) -> Result<()> {
        let plan = stream::plan(self.codec, &self.sets)?;
        self.check(&plan)?;
        let mft = self.create(&plan)?;
        self.name = format!(
            "Media Foundation {} ({} on {})",
            label(self.codec),
            mft.name(),
            self.gpu.name()
        );
        self.session = Some(Session {
            geometry: Geometry {
                crop: plan.crop,
                bits: plan.format.bits(),
            },
            plan,
            mft: Some(mft),
        });
        Ok(())
    }

    fn create(&self, plan: &Plan) -> Result<Mft> {
        let (_, _, w, h) = plan.crop;
        let spec = Spec {
            codec: self.codec,
            size: (w, h),
            format: plan.format,
        };
        Mft::new(self.api, &self.gpu, &spec).map_err(Error::Unsupported)
    }

    fn update(&mut self, in_band: &[&[u8]]) -> Result<()> {
        self.sets = stream::merge_sets(self.codec, &self.sets, in_band);
        if !in_band.iter().any(|n| stream::is_sps(self.codec, n)) {
            return Ok(());
        }
        let Some(current) = self.session.as_ref().map(|s| s.plan) else {
            return self.open();
        };
        let plan = stream::plan(self.codec, &self.sets)?;
        if plan == current {
            return Ok(());
        }
        if plan.format != current.format {
            return Err(Error::Unsupported(format!(
                "{} bit depth changes mid-stream on Media Foundation",
                label(self.codec)
            )));
        }
        self.check(&plan)?;
        if let Some(session) = self.session.as_mut() {
            session.geometry.crop = plan.crop;
            session.plan = plan;
        }
        Ok(())
    }

    fn take_pts(&mut self, t: i64) -> i64 {
        let pts = if self.in_flight.contains(&t) {
            t
        } else {
            self.in_flight.iter().copied().min().unwrap_or(t)
        };
        self.in_flight.retain(|&p| p > pts);
        pts
    }

    fn collect(&mut self, out: &mut Vec<Frame>) -> std::result::Result<(), String> {
        for _ in 0..MAX_OUTPUTS_PER_CALL {
            let Some(session) = self.session.as_ref() else {
                return Ok(());
            };
            let Some(mft) = session.mft.as_ref() else {
                return Ok(());
            };
            match mft.poll()? {
                Poll::NeedInput => return Ok(()),
                Poll::FormatChanged => {}
                Poll::Frame(sample, t) => {
                    let (geometry, format) = (session.geometry, session.plan.format);
                    let mut frame = self.readback.read(&self.gpu, &sample, format, |b| {
                        crate::gpu::biplanar::to_frame(b, &geometry)
                    })?;
                    frame.pts = self.take_pts(t);
                    out.push(frame);
                }
            }
        }
        Err("the decoder keeps producing output".into())
    }

    fn feed(&mut self, pts: i64) -> std::result::Result<Vec<Frame>, String> {
        let mut out = Vec::new();
        for _ in 0..2 {
            let mft = self
                .session
                .as_ref()
                .and_then(|s| s.mft.as_ref())
                .ok_or("no decoder")?;
            if mft.input(self.api, &self.input, pts)? {
                self.in_flight.push(pts);
                self.need_headers = false;
                self.collect(&mut out)?;
                if self.in_flight.len() > MAX_IN_FLIGHT {
                    return Err(format!(
                        "the decoder holds {} pictures back",
                        self.in_flight.len()
                    ));
                }
                return Ok(out);
            }
            self.collect(&mut out)?;
        }
        Err("the decoder does not accept input".into())
    }

    fn restart(&mut self) {
        if let Some(session) = self.session.as_mut()
            && session.mft.as_ref().is_some_and(|m| m.flush().is_err())
        {
            session.mft = None;
        }
        self.in_flight.clear();
        self.need_headers = true;
        self.waiting = true;
        self.skip_rasl = false;
    }
}

impl DecoderBackend for MfDecoder {
    fn backend(&self) -> Backend {
        Backend::MediaFoundation
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn decode(&mut self, packet: &Packet) -> Result<Vec<Frame>> {
        ensure_com().map_err(Error::Backend)?;
        let codec = self.codec;
        let nals = self.framing.split(&packet.data)?;
        let in_band: Vec<&[u8]> = nals
            .iter()
            .copied()
            .filter(|n| nal::is_parameter_set(codec, n))
            .collect();
        if !in_band.is_empty() {
            self.update(&in_band)?;
        }
        if self.session.is_none() {
            return Ok(Vec::new());
        }
        let types: Vec<u8> = nals.iter().map(|n| nal::nal_type(codec, n)).collect();
        let starting = self.waiting;
        if starting {
            if !nals.iter().any(|n| nal::is_keyframe(codec, n)) {
                return Ok(Vec::new());
            }
            self.waiting = false;
        }
        if codec == Codec::Hevc {
            let bla = types.iter().any(|t| (16..=18).contains(t));
            if types.iter().any(|t| (16..=21).contains(t)) {
                self.skip_rasl = bla || (starting && types.contains(&21));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                return Ok(Vec::new());
            }
        }
        let headers = (self.need_headers && in_band.is_empty()).then_some(self.sets.as_slice());
        stream::annexb(&nals, headers, &mut self.input);
        if let Some(session) = self.session.as_ref()
            && session.mft.is_none()
        {
            let mft = self.create(&session.plan)?;
            if let Some(session) = self.session.as_mut() {
                session.mft = Some(mft);
            }
        }
        self.feed(packet.pts).map_err(|e| {
            self.gpu.note_failure();
            Error::Backend(e)
        })
    }

    fn flush(&mut self) -> Result<Vec<Frame>> {
        ensure_com().map_err(Error::Backend)?;
        let mut out = Vec::new();
        if let Some(mft) = self.session.as_ref().and_then(|s| s.mft.as_ref()) {
            mft.drain().map_err(Error::Backend)?;
            self.collect(&mut out).map_err(|e| {
                self.gpu.note_failure();
                Error::Backend(e)
            })?;
        }
        self.restart();
        Ok(out)
    }

    fn reset(&mut self) {
        if ensure_com().is_ok() {
            self.restart();
        }
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        let _ = ensure_com();
    }
}
