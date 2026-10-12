mod device;
mod ffi;
mod session;

#[cfg(test)]
mod abi_tests;

use std::collections::VecDeque;
use std::sync::OnceLock;

use session::{Locked, Session, Setup};

use crate::gpu::nal;
use crate::gpu::{Backend, Direction, EncoderBackend, EncoderConfig, Profile};
use crate::media::{Bitstream, Rate};
use crate::media::{Codec, Error, Frame, Packet, PixelFormat, Plane, Result};

const DEPTH: usize = 6;

pub fn available() -> Vec<(Codec, Direction)> {
    static FOUND: OnceLock<Vec<(Codec, Direction)>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let Ok(session) = Session::open() else {
                return Vec::new();
            };
            [Codec::H264, Codec::Hevc]
                .into_iter()
                .filter(|&c| session.caps(c).is_ok_and(|caps| caps.max_size.0 > 0))
                .map(|c| (c, Direction::Encode))
                .collect()
        })
        .clone()
}

pub fn encoder(codec: Codec, config: &EncoderConfig) -> Result<Box<dyn EncoderBackend>> {
    Ok(Box::new(NvEncoder::new(codec, config)?))
}

struct NvEncoder {
    codec: Codec,
    session: Session,
    width: u32,
    height: u32,
    format: PixelFormat,
    fps: (u32, u32),
    b_frames: u8,
    bitstream: Bitstream,
    extradata: Vec<u8>,
    sets: Vec<Vec<u8>>,
    free: Vec<usize>,
    pending: VecDeque<usize>,
    ready: usize,
    display_pts: VecDeque<i64>,
    duration: i64,
    name: String,
}

fn hevc_level(width: u32, height: u32, fps: (u32, u32), kbps: u32) -> u32 {
    const LEVELS: [(u32, u64, u64, u64); 13] = [
        (30, 36_864, 552_960, 128),
        (60, 122_880, 3_686_400, 1_500),
        (63, 245_760, 7_372_800, 3_000),
        (90, 552_960, 16_588_800, 6_000),
        (93, 983_040, 33_177_600, 10_000),
        (120, 2_228_224, 66_846_720, 12_000),
        (123, 2_228_224, 133_693_440, 20_000),
        (150, 8_912_896, 267_386_880, 25_000),
        (153, 8_912_896, 534_773_760, 40_000),
        (156, 8_912_896, 1_069_547_520, 60_000),
        (180, 35_651_584, 1_069_547_520, 60_000),
        (183, 35_651_584, 2_139_095_040, 120_000),
        (186, 35_651_584, 4_278_190_080, 240_000),
    ];
    let ps = u64::from(width) * u64::from(height);
    let sr = (ps * u64::from(fps.0)).div_ceil(u64::from(fps.1.max(1)));
    let side = u64::from(width.max(height));
    LEVELS
        .iter()
        .find(|&&(_, max_ps, max_sr, max_br)| {
            ps <= max_ps && sr <= max_sr && u64::from(kbps) <= max_br && side * side <= max_ps * 8
        })
        .map_or(ffi::NV_ENC_LEVEL_AUTOSELECT, |&(code, ..)| code)
}

fn plane_fits(p: &Plane, width: usize, height: usize, bytes: usize) -> bool {
    let row = width * bytes;
    height == 0 || (p.stride >= row && p.data.len() >= p.stride * (height - 1) + row)
}

fn fill_nv12(dst: Locked<'_>, frame: &Frame) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (luma, chroma) = dst.data.split_at_mut(dst.pitch * h);
    let y = &frame.planes[0];
    for (d, s) in luma
        .chunks_mut(dst.pitch)
        .zip(y.data.chunks(y.stride))
        .take(h)
    {
        d[..w].copy_from_slice(&s[..w]);
    }
    let (u, v) = (&frame.planes[1], &frame.planes[2]);
    let cw = w / 2;
    let rows = chroma
        .chunks_mut(dst.pitch)
        .zip(u.data.chunks(u.stride))
        .zip(v.data.chunks(v.stride))
        .take(h / 2);
    for ((d, su), sv) in rows {
        let pairs = d[..cw * 2].as_chunks_mut::<2>().0;
        for ((p, &a), &b) in pairs.iter_mut().zip(&su[..cw]).zip(&sv[..cw]) {
            *p = [a, b];
        }
    }
}

fn fill_p010(dst: Locked<'_>, frame: &Frame) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (luma, chroma) = dst.data.split_at_mut(dst.pitch * h);
    let shift = |s: [u8; 2]| ((u16::from_le_bytes(s) & 0x3ff) << 6).to_le_bytes();
    let y = &frame.planes[0];
    for (d, s) in luma
        .chunks_mut(dst.pitch)
        .zip(y.data.chunks(y.stride))
        .take(h)
    {
        let out = d[..w * 2].as_chunks_mut::<2>().0;
        let src = s[..w * 2].as_chunks::<2>().0;
        for (o, &i) in out.iter_mut().zip(src) {
            *o = shift(i);
        }
    }
    let (u, v) = (&frame.planes[1], &frame.planes[2]);
    let cw = w / 2;
    let rows = chroma
        .chunks_mut(dst.pitch)
        .zip(u.data.chunks(u.stride))
        .zip(v.data.chunks(v.stride))
        .take(h / 2);
    for ((d, su), sv) in rows {
        let out = d[..cw * 4].as_chunks_mut::<4>().0;
        let a = su[..cw * 2].as_chunks::<2>().0;
        let b = sv[..cw * 2].as_chunks::<2>().0;
        for ((o, &cb), &cr) in out.iter_mut().zip(a).zip(b) {
            let ([b0, b1], [r0, r1]) = (shift(cb), shift(cr));
            *o = [b0, b1, r0, r1];
        }
    }
}

fn is_delimiter(codec: Codec, nal: &[u8]) -> bool {
    let t = nal::nal_type(codec, nal);
    match codec {
        Codec::Hevc => (35..=37).contains(&t),
        _ => (9..=11).contains(&t),
    }
}

fn shape(
    codec: Codec,
    bitstream: Bitstream,
    sets: &[Vec<u8>],
    bytes: &[u8],
    idr: bool,
) -> Result<(Vec<u8>, bool)> {
    let mut keep = Vec::new();
    let mut key = idr;
    for n in nal::annexb(bytes) {
        if nal::is_parameter_set(codec, n) || is_delimiter(codec, n) {
            continue;
        }
        key |= nal::is_keyframe(codec, n);
        keep.push(n);
    }
    if keep.is_empty() {
        return Err(Error::Backend("NVENC wrote no picture data".into()));
    }
    let data = match bitstream {
        Bitstream::LengthPrefixed => nal::length_prefix(&keep),
        Bitstream::AnnexB => {
            let extra = if key {
                sets.iter().map(|s| s.len() + 4).sum()
            } else {
                0
            };
            let mut out = Vec::with_capacity(bytes.len() + extra);
            let heads = if key { sets } else { &[] };
            nal::put_start_codes(heads.iter().map(Vec::as_slice).chain(keep), &mut out);
            out
        }
    };
    Ok((data, key))
}

fn parameter_sets(codec: Codec, bytes: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>)> {
    let mut found: [Vec<Vec<u8>>; 3] = Default::default();
    for n in nal::annexb(bytes) {
        if let Some(i) = nal::set_slot(codec, n) {
            found[i].push(n.to_vec());
        }
    }
    if found[1].is_empty() || found[2].is_empty() {
        return Err(Error::Backend("NVENC returned no parameter sets".into()));
    }
    let record = nal::record(codec, &found)?;
    Ok((record, found.into_iter().flatten().collect()))
}

impl NvEncoder {
    fn new(codec: Codec, config: &EncoderConfig) -> Result<Self> {
        let (width, height) = (config.width, config.height);
        let fps = config.fps;
        if width == 0 || height == 0 || fps.0 == 0 || fps.1 == 0 {
            return Err(Error::Invalid(
                "encoder size and frame rate must be set".into(),
            ));
        }
        if width % 2 != 0 || height % 2 != 0 {
            return Err(Error::Unsupported(format!(
                "NVENC takes even sizes, not {width}x{height}"
            )));
        }
        let (profile, ten_bit) = match (codec, config.profile) {
            (Codec::H264, Profile::Baseline) => (ffi::NV_ENC_H264_PROFILE_BASELINE_GUID, false),
            (Codec::H264, Profile::Main) => (ffi::NV_ENC_H264_PROFILE_MAIN_GUID, false),
            (Codec::H264, Profile::High) => (ffi::NV_ENC_H264_PROFILE_HIGH_GUID, false),
            (Codec::Hevc, Profile::Main10) => (ffi::NV_ENC_HEVC_PROFILE_MAIN10_GUID, true),
            (Codec::Hevc, _) => (ffi::NV_ENC_HEVC_PROFILE_MAIN_GUID, false),
            (c, p) => return Err(Error::Unsupported(format!("{c:?} {p:?} on NVENC"))),
        };
        let mut session = Session::open().map_err(Error::Unsupported)?;
        let caps = session.caps(codec).map_err(Error::Unsupported)?;
        if ten_bit && !caps.ten_bit {
            return Err(Error::Unsupported(
                "this GPU's NVENC has no HEVC Main10".into(),
            ));
        }
        let (min, max) = (caps.min_size, caps.max_size);
        if width < min.0 || height < min.1 || width > max.0 || height > max.1 {
            return Err(Error::Unsupported(format!(
                "NVENC takes {}x{} to {}x{} {codec:?} pictures, not {width}x{height}",
                min.0, min.1, max.0, max.1
            )));
        }
        let gop = if config.gop == 0 {
            (fps.0 / fps.1).max(1) * 2
        } else {
            config.gop
        };
        let b_frames = if config.profile == Profile::Baseline {
            0
        } else {
            u32::from(config.b_frames)
                .min(caps.max_b_frames)
                .min(gop.saturating_sub(2))
        };
        let level = match (codec, config.rate) {
            (Codec::Hevc, Rate::Bitrate(kbps)) => hevc_level(width, height, fps, kbps),
            (Codec::Hevc, Rate::Vbr { kbps, max_kbps }) => {
                hevc_level(width, height, fps, kbps.max(max_kbps))
            }
            _ => ffi::NV_ENC_LEVEL_AUTOSELECT,
        };
        session
            .initialize(&Setup {
                codec,
                size: (width, height),
                fps,
                profile,
                ten_bit,
                rate: config.rate,
                gop,
                b_frames,
                level,
                slots: DEPTH + b_frames as usize + 1,
            })
            .map_err(Error::Unsupported)?;
        let params = session.sequence_params().map_err(Error::Backend)?;
        let (extradata, sets) = parameter_sets(codec, &params)?;
        let slots = session.slots();
        Ok(Self {
            codec,
            session,
            width,
            height,
            format: if ten_bit {
                PixelFormat::Yuv420p10
            } else {
                PixelFormat::Yuv420p8
            },
            fps,
            b_frames: b_frames as u8,
            bitstream: config.bitstream,
            extradata,
            sets,
            free: (0..slots).rev().collect(),
            pending: VecDeque::with_capacity(slots),
            ready: 0,
            display_pts: VecDeque::with_capacity(slots),
            duration: 0,
            name: format!("NVENC {codec} encoder"),
        })
    }

    fn check(&self, frame: &Frame) -> Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let bytes = self.format.bytes_per_sample();
        let fits = frame.format == self.format
            && frame.width == self.width
            && frame.height == self.height
            && plane_fits(&frame.planes[0], w, h, bytes)
            && frame.planes[1..]
                .iter()
                .all(|p| plane_fits(p, w / 2, h / 2, bytes));
        if fits {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "the encoder takes {}x{} {:?} frames, not {}x{} {:?}",
                self.width, self.height, self.format, frame.width, frame.height, frame.format
            )))
        }
    }

    fn submit(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        self.check(frame)?;
        if self.duration == 0 {
            let tb = frame.time_base;
            let num = i128::from(tb.den) * i128::from(self.fps.1);
            let den = i128::from(tb.num) * i128::from(self.fps.0);
            self.duration = if den > 0 {
                ((num + den / 2) / den).max(1) as i64
            } else {
                1
            };
        }
        let mut out = Vec::new();
        if self.free.is_empty() {
            if self.ready == 0 {
                return Err(Error::Backend("NVENC holds every buffer".into()));
            }
            out.push(self.read_oldest()?);
        }
        let slot = self
            .free
            .pop()
            .ok_or_else(|| Error::Backend("no free NVENC buffer".into()))?;
        let ten_bit = self.format == PixelFormat::Yuv420p10;
        let done = self.session.submit(slot, frame.pts, |locked| {
            if ten_bit {
                fill_p010(locked, frame)
            } else {
                fill_nv12(locked, frame)
            }
        });
        let done = match done {
            Ok(d) => d,
            Err(e) => {
                self.free.push(slot);
                return Err(Error::Backend(e));
            }
        };
        self.pending.push_back(slot);
        self.display_pts.push_back(frame.pts);
        if done {
            self.ready = self.pending.len();
        }
        while self.ready > DEPTH {
            out.push(self.read_oldest()?);
        }
        Ok(out)
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        if self.pending.len() > self.ready {
            self.session.end_of_stream().map_err(Error::Backend)?;
            self.ready = self.pending.len();
        }
        let mut out = Vec::with_capacity(self.ready);
        while self.ready > 0 {
            out.push(self.read_oldest()?);
        }
        Ok(out)
    }

    fn read_oldest(&mut self) -> Result<Packet> {
        let slot = self
            .pending
            .pop_front()
            .ok_or_else(|| Error::Backend("no picture in flight".into()))?;
        self.ready = self.ready.saturating_sub(1);
        let (codec, bitstream, sets) = (self.codec, self.bitstream, &self.sets);
        let read = self.session.read(slot, |bytes, pts, idr| {
            shape(codec, bitstream, sets, bytes, idr).map(|(data, key)| (data, pts, key))
        });
        self.free.push(slot);
        let (data, pts, keyframe) = read.map_err(Error::Backend)??;
        let base = self.display_pts.pop_front().unwrap_or(pts);
        let delay = if self.b_frames > 0 { self.duration } else { 0 };
        Ok(Packet {
            data,
            pts,
            dts: base - delay,
            keyframe,
        })
    }
}

impl EncoderBackend for NvEncoder {
    fn backend(&self) -> Backend {
        Backend::Nvenc
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn b_frames(&self) -> u8 {
        self.b_frames
    }

    fn encode(&mut self, frame: &Frame) -> Result<Vec<Packet>> {
        self.submit(frame)
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.drain()
    }

    fn extradata(&self) -> Option<&[u8]> {
        Some(&self.extradata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::TimeBase;

    fn frame(format: PixelFormat) -> Frame {
        let mut f = Frame::new(format, 4, 2, 0, TimeBase::new(1, 90000));
        let bytes = format.bytes_per_sample();
        for (i, p) in f.planes.iter_mut().enumerate() {
            for (k, s) in p.data.chunks_mut(bytes).enumerate() {
                let v = (i * 100 + k) as u16 | if bytes == 2 { 0xfc00 } else { 0 };
                s.copy_from_slice(&v.to_le_bytes()[..bytes]);
            }
        }
        f
    }

    #[test]
    fn nv12_interleaves_chroma_after_the_luma_rows() {
        let (pitch, h) = (8, 2);
        let mut buf = vec![0xaa; pitch * h * 3 / 2];
        fill_nv12(
            Locked {
                data: &mut buf,
                pitch,
            },
            &frame(PixelFormat::Yuv420p8),
        );
        assert_eq!(&buf[..4], &[0, 1, 2, 3]);
        assert_eq!(&buf[4..8], &[0xaa; 4]);
        assert_eq!(&buf[pitch..pitch + 4], &[4, 5, 6, 7]);
        assert_eq!(&buf[pitch * h..pitch * h + 4], &[100, 200, 101, 201]);
    }

    #[test]
    fn p010_puts_the_ten_bits_high_and_masks_the_rest() {
        let (pitch, h) = (12, 2);
        let mut buf = vec![0xaa; pitch * h * 3 / 2];
        fill_p010(
            Locked {
                data: &mut buf,
                pitch,
            },
            &frame(PixelFormat::Yuv420p10),
        );
        let at = |i: usize| u16::from_le_bytes([buf[i], buf[i + 1]]);
        assert_eq!(
            (0..4).map(|x| at(x * 2)).collect::<Vec<_>>(),
            [0, 64, 128, 192]
        );
        assert_eq!(&buf[8..12], &[0xaa; 4]);
        assert_eq!(at(pitch), 4 << 6);
        let c = pitch * h;
        assert_eq!(
            [at(c), at(c + 2), at(c + 4)],
            [100 << 6, 200 << 6, 101 << 6]
        );
    }

    #[test]
    fn hevc_bitrates_pick_a_main_tier_level() {
        assert_eq!(hevc_level(1920, 1080, (30, 1), 8_000), 120);
        assert_eq!(hevc_level(1920, 1080, (30, 1), 15_000), 123);
        assert_eq!(hevc_level(3840, 2160, (60, 1), 50_000), 156);
    }

    #[test]
    fn without_an_nvidia_driver_nvenc_says_why() {
        if Session::open().is_ok() {
            return;
        }
        assert!(available().is_empty());
        let e = encoder(Codec::H264, &EncoderConfig::default())
            .err()
            .unwrap();
        assert!(matches!(e, Error::Unsupported(_)), "{e}");
    }
}
