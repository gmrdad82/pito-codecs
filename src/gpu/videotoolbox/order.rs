use std::sync::Arc;

use crate::gpu::syntax::bits::unescape_rbsp;
use crate::gpu::syntax::h264::params as h264p;
use crate::gpu::syntax::h264::slice as h264s;
use crate::gpu::syntax::hevc::dpb::PocState as HevcPoc;
use crate::gpu::syntax::hevc::params as hevcp;
use crate::gpu::syntax::hevc::slice as hevcs;
use crate::media::{Codec, Error, Result};

const HEADER_BYTES: usize = 512;

pub type Key = (u32, i32);

#[derive(Default)]
pub struct Order {
    epoch: u32,
    first: bool,
    after_eos: bool,
    avc_sps: Vec<Option<h264p::Sps>>,
    avc_pps: Vec<Option<h264p::Pps>>,
    avc_poc: h264s::PocState,
    hevc_sps: Vec<Option<Arc<hevcp::Sps>>>,
    hevc_pps: Vec<Option<Arc<hevcp::Pps>>>,
    hevc_poc: HevcPoc,
}

fn invalid(e: impl std::fmt::Display) -> Error {
    Error::Invalid(e.to_string())
}

fn store<T>(list: &mut Vec<Option<T>>, id: usize, v: T) -> Result<()> {
    if id >= 64 {
        return Err(invalid(format!("parameter set id {id} out of range")));
    }
    if list.len() <= id {
        list.resize_with(id + 1, || None);
    }
    list[id] = Some(v);
    Ok(())
}

fn prefixes(payload: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    let short = payload.len() > HEADER_BYTES;
    [true, false]
        .into_iter()
        .filter(move |&cut| cut || short)
        .map(|cut| {
            unescape_rbsp(if cut {
                &payload[..payload.len().min(HEADER_BYTES)]
            } else {
                payload
            })
        })
}

impl Order {
    pub fn new() -> Self {
        Self {
            first: true,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch.wrapping_add(1),
            ..Self::new()
        };
    }

    pub fn key(&mut self, codec: Codec, nals: &[&[u8]]) -> Result<Option<Key>> {
        let mut key = None;
        for n in nals {
            let found = match codec {
                Codec::Hevc => self.hevc(n)?,
                _ => self.avc(n)?,
            };
            if key.is_none() {
                key = found;
            }
        }
        Ok(key)
    }

    fn avc(&mut self, n: &[u8]) -> Result<Option<Key>> {
        let Some(&b) = n.first() else {
            return Ok(None);
        };
        let hdr = h264s::NalHeader::parse(b).map_err(invalid)?;
        let rest = n.get(1..).unwrap_or_default();
        match hdr.nal_unit_type {
            h264s::nal_type::SPS => {
                let sps = h264p::Sps::parse(&unescape_rbsp(rest)).map_err(invalid)?;
                let id = sps.id as usize;
                store(&mut self.avc_sps, id, sps)?;
                Ok(None)
            }
            h264s::nal_type::PPS => {
                let pps =
                    h264p::Pps::parse(&unescape_rbsp(rest), &self.avc_sps).map_err(invalid)?;
                let id = pps.id as usize;
                store(&mut self.avc_pps, id, pps)?;
                Ok(None)
            }
            h264s::nal_type::SLICE | h264s::nal_type::IDR => {
                if rest.first().is_none_or(|b| b & 0x80 == 0) {
                    return Ok(None);
                }
                let (ppss, spss) = (&self.avc_pps, &self.avc_sps);
                let mut parsed = None;
                for rbsp in prefixes(rest) {
                    parsed = Some(h264s::SliceHeader::parse(&rbsp, hdr, |id| {
                        let missing = |what: String| {
                            crate::gpu::syntax::error::Error::MissingParameterSet(what)
                        };
                        let pps = ppss
                            .get(id as usize)
                            .and_then(Option::as_ref)
                            .ok_or_else(|| missing(format!("PPS {id}")))?;
                        let sps = spss
                            .get(pps.sps_id as usize)
                            .and_then(Option::as_ref)
                            .ok_or_else(|| missing(format!("SPS {}", pps.sps_id)))?;
                        Ok((pps, sps))
                    }));
                    if parsed.as_ref().is_some_and(|p| p.is_ok()) {
                        break;
                    }
                }
                let (sh, _, sps) = parsed
                    .ok_or_else(|| invalid("empty slice"))?
                    .map_err(invalid)?;
                let poc = self.avc_poc.compute(&sh, sps);
                self.avc_poc.update(&sh, &poc);
                self.first = false;
                if sh.idr {
                    self.epoch = self.epoch.wrapping_add(1);
                }
                if sh.has_mmco5() {
                    self.epoch = self.epoch.wrapping_add(1);
                    return Ok(Some((self.epoch, 0)));
                }
                Ok(Some((self.epoch, poc.frame())))
            }
            _ => Ok(None),
        }
    }

    fn hevc(&mut self, n: &[u8]) -> Result<Option<Key>> {
        let hdr = hevcs::NalHeader::parse(n).map_err(invalid)?;
        if hdr.layer_id != 0 {
            return Ok(None);
        }
        let rest = n.get(2..).unwrap_or_default();
        match hdr.nal_type {
            33 => {
                let sps = hevcp::Sps::parse(&unescape_rbsp(rest)).map_err(invalid)?;
                let id = sps.id as usize;
                store(&mut self.hevc_sps, id, Arc::new(sps))?;
                Ok(None)
            }
            34 => {
                let pps = hevcp::Pps::parse(&unescape_rbsp(rest)).map_err(invalid)?;
                let id = pps.id as usize;
                store(&mut self.hevc_pps, id, Arc::new(pps))?;
                Ok(None)
            }
            36 => {
                self.after_eos = true;
                Ok(None)
            }
            t if t < 32 => {
                if rest.first().is_none_or(|b| b & 0x80 == 0) {
                    return Ok(None);
                }
                let (ppss, spss) = (&self.hevc_pps, &self.hevc_sps);
                let mut parsed = None;
                for rbsp in prefixes(rest) {
                    parsed = Some(hevcs::SliceHeader::parse(
                        &rbsp,
                        hdr,
                        |id| {
                            let missing = |what: String| {
                                crate::gpu::syntax::error::Error::MissingParameterSet(what)
                            };
                            let pps = ppss
                                .get(id as usize)
                                .and_then(Option::as_ref)
                                .ok_or_else(|| missing(format!("PPS {id}")))?;
                            let sps = spss
                                .get(pps.sps_id as usize)
                                .and_then(Option::as_ref)
                                .ok_or_else(|| missing(format!("SPS {}", pps.sps_id)))?;
                            Ok((pps.clone(), sps.clone()))
                        },
                        None,
                    ));
                    if parsed.as_ref().is_some_and(|p| p.is_ok()) {
                        break;
                    }
                }
                let (sh, _, sps) = parsed
                    .ok_or_else(|| invalid("empty slice"))?
                    .map_err(invalid)?;
                let no_rasl =
                    hdr.is_irap() && (hdr.is_idr() || hdr.is_bla() || self.first || self.after_eos);
                if no_rasl {
                    self.epoch = self.epoch.wrapping_add(1);
                }
                let poc = self.hevc_poc.compute(&sh, sps.max_poc_lsb(), no_rasl);
                self.hevc_poc.update(&sh, poc);
                self.first = false;
                self.after_eos = false;
                Ok(Some((self.epoch, poc)))
            }
            _ => Ok(None),
        }
    }
}
