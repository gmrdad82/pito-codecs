#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};

use pito_codecs::media::TimeBase;
use pito_codecs::{mkv, mp4};

use crate::testref;

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn work(area: &str, name: &str) -> PathBuf {
    testref::root()
        .join("tmp/record_refs")
        .join(area)
        .join(name)
}

pub fn read(area: &str, name: &str) -> Result<Vec<u8>, String> {
    let path = work(area, name);
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Csp {
    I420,
    I422,
    I444,
}

impl Csp {
    fn shift(self) -> (u32, u32) {
        match self {
            Csp::I420 => (1, 1),
            Csp::I422 => (1, 0),
            Csp::I444 => (0, 0),
        }
    }

    pub fn y4m(self, bits: u8) -> String {
        let base = match self {
            Csp::I420 => "420",
            Csp::I422 => "422",
            Csp::I444 => "444",
        };
        match (self, bits) {
            (Csp::I420, 8) => "420jpeg".into(),
            (_, 8) => base.into(),
            _ => format!("{base}p{bits}"),
        }
    }

    pub fn x26x(self) -> &'static str {
        match self {
            Csp::I420 => "i420",
            Csp::I422 => "i422",
            Csp::I444 => "i444",
        }
    }
}

pub type Rgb = dyn Fn(u64, u32, u32) -> [u8; 3];

pub fn pattern(size: (u32, u32)) -> impl Fn(u64, u32, u32) -> [u8; 3] {
    move |n, x, y| testref::rgb_at(size.0, size.1, n, x, y)
}

pub fn picture(size: (u32, u32), csp: Csp, bits: u8, n: u64, rgb: &Rgb, out: &mut Vec<u8>) {
    let (w, h) = size;
    let (sx, sy) = csp.shift();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let mut put = |v: u32| {
        if bits > 8 {
            out.extend_from_slice(&((v << (bits - 8)) as u16).to_le_bytes());
        } else {
            out.push(v as u8);
        }
    };
    for y in 0..h {
        for x in 0..w {
            put(u32::from(testref::yuv_of(rgb(n, x, y))[0]));
        }
    }
    for plane in 1..3 {
        for cy in 0..ch {
            for cx in 0..cw {
                let mut sum = 0u32;
                let mut count = 0u32;
                for dy in 0..1 << sy {
                    for dx in 0..1 << sx {
                        let (x, y) = (((cx << sx) + dx).min(w - 1), ((cy << sy) + dy).min(h - 1));
                        sum += u32::from(testref::yuv_of(rgb(n, x, y))[plane]);
                        count += 1;
                    }
                }
                put((sum + count / 2) / count);
            }
        }
    }
}

pub fn raw_video(size: (u32, u32), csp: Csp, bits: u8, frames: u64, rgb: &Rgb) -> Vec<u8> {
    let mut out = Vec::new();
    for n in 0..frames {
        picture(size, csp, bits, n, rgb, &mut out);
    }
    out
}

pub fn y4m(
    size: (u32, u32),
    fps: (u32, u32),
    csp: Csp,
    bits: u8,
    frames: u64,
    rgb: &Rgb,
) -> Vec<u8> {
    let mut out = format!(
        "YUV4MPEG2 W{} H{} F{}:{} Ip A1:1 C{}\n",
        size.0,
        size.1,
        fps.0,
        fps.1,
        csp.y4m(bits)
    )
    .into_bytes();
    for n in 0..frames {
        out.extend_from_slice(b"FRAME\n");
        picture(size, csp, bits, n, rgb, &mut out);
    }
    out
}

pub fn nals(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut end = starts.get(k + 1).map_or(stream.len(), |&n| n - 3);
            while end > s && stream[end - 1] == 0 {
                end -= 1;
            }
            &stream[s..end]
        })
        .filter(|n| !n.is_empty())
        .collect()
}

pub struct Coded {
    pub config: Vec<u8>,
    pub samples: Vec<(Vec<u8>, bool)>,
}

pub fn hevc(stream: &[u8]) -> Result<Coded, String> {
    let config = pito_codecs::hevc::Hvcc::from_annexb(stream)
        .map_err(text)?
        .to_bytes();
    let mut samples = Vec::new();
    let mut cur = Vec::new();
    let mut key = false;
    let mut vcl = false;
    for nal in nals(stream) {
        let kind = (nal[0] >> 1) & 0x3f;
        let first = kind < 32 && nal.get(2).is_some_and(|b| b & 0x80 != 0);
        if vcl && ((32..=35).contains(&kind) || kind == 39 || first) {
            samples.push((std::mem::take(&mut cur), key));
            key = false;
            vcl = false;
        }
        if (32..=35).contains(&kind) {
            continue;
        }
        if kind < 32 {
            vcl = true;
            key |= (16..=21).contains(&kind);
        }
        cur.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        cur.extend_from_slice(nal);
    }
    if vcl {
        samples.push((cur, key));
    }
    Ok(Coded { config, samples })
}

pub fn x265_pocs(csv: &[u8]) -> Result<Vec<i64>, String> {
    let csv = String::from_utf8_lossy(csv);
    let mut lines = csv.lines();
    let head: Vec<String> = lines
        .next()
        .ok_or("an empty x265 csv")?
        .split(',')
        .map(|c| c.trim().to_string())
        .collect();
    let col = |name: &str| {
        head.iter()
            .position(|c| c == name)
            .ok_or_else(|| format!("the x265 csv has no {name} column"))
    };
    let (order, poc) = (col("Encode Order")?, col("POC")?);
    let mut rows = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split(',').map(str::trim).collect();
        let at = |i: usize| f.get(i).and_then(|v| v.parse::<i64>().ok());
        if let (Some(o), Some(p)) = (at(order), at(poc)) {
            rows.push((o, p));
        }
    }
    rows.sort_unstable();
    Ok(rows.into_iter().map(|(_, p)| p).collect())
}

fn delay(pocs: &[i64]) -> i64 {
    pocs.iter()
        .enumerate()
        .map(|(k, &p)| k as i64 - p)
        .max()
        .unwrap_or(0)
        .max(0)
}

pub struct Timed<'a> {
    pub coded: &'a Coded,
    pub pocs: &'a [i64],
    pub size: (u32, u32),
    pub fps: u32,
}

impl Timed<'_> {
    fn poc(&self, k: usize) -> i64 {
        self.pocs.get(k).copied().unwrap_or(k as i64)
    }

    fn mp4(&self, entry: mp4::SampleEntry, brand: mp4::Brand) -> Result<Vec<u8>, String> {
        let d = delay(self.pocs);
        let mut cfg = mp4::TrackConfig::new(entry, self.fps);
        cfg.media_start = (d > 0).then_some(d);
        let options = mp4::Options { brand, reserve: 0 };
        let mut w = mp4::Writer::new(Cursor::new(Vec::new()), options).map_err(text)?;
        let t = w.add_track(cfg).map_err(text)?;
        for (k, (data, key)) in self.coded.samples.iter().enumerate() {
            w.push_sample(t, data, self.poc(k) + d, k as i64, 1, *key)
                .map_err(text)?;
        }
        Ok(w.finish_faststart().map_err(text)?.into_inner())
    }

    fn mkv(&self, cfg: mkv::TrackConfig, webm: bool) -> Result<Vec<u8>, String> {
        let options = if webm {
            mkv::Options::webm()
        } else {
            mkv::Options::default()
        };
        let mut w = mkv::Writer::new(Cursor::new(Vec::new()), options);
        let t = w.add_track(cfg).map_err(text)?;
        for (k, (data, key)) in self.coded.samples.iter().enumerate() {
            w.push_sample(t, data, self.poc(k), Some(1), *key)
                .map_err(text)?;
        }
        Ok(w.finish().map_err(text)?.into_inner())
    }

    fn tb(&self) -> TimeBase {
        TimeBase::new(1, self.fps)
    }

    pub fn h264_mp4(&self) -> Result<Vec<u8>, String> {
        let config = mp4::AvcConfig::parse(&self.coded.config).map_err(text)?;
        let entry = mp4::SampleEntry::h264(config, self.size.0 as u16, self.size.1 as u16);
        self.mp4(entry, mp4::Brand::Mp4)
    }

    pub fn hevc_mp4(&self, brand: mp4::Brand) -> Result<Vec<u8>, String> {
        let config = mp4::HevcConfig::parse(&self.coded.config).map_err(text)?;
        let entry = mp4::SampleEntry::hevc(config, self.size.0 as u16, self.size.1 as u16);
        self.mp4(entry, brand)
    }

    pub fn hevc_mkv(&self) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        let cfg = mkv::TrackConfig::hevc(self.coded.config.clone(), w, h, self.tb());
        self.mkv(cfg, false)
    }

    pub fn av1_mp4(&self) -> Result<Vec<u8>, String> {
        let config = mp4::Av1Config::parse(&self.coded.config).map_err(text)?;
        let entry = mp4::SampleEntry::av1(config, self.size.0 as u16, self.size.1 as u16);
        self.mp4(entry, mp4::Brand::Mp4)
    }

    pub fn av1_mkv(&self, webm: bool) -> Result<Vec<u8>, String> {
        let (w, h) = self.size;
        let cfg = mkv::TrackConfig::av1(self.coded.config.clone(), w, h, self.tb());
        self.mkv(cfg, webm)
    }

    pub fn vp9_mp4(&self, config: mp4::VpcConfig) -> Result<Vec<u8>, String> {
        let entry = mp4::SampleEntry::vp9(config, self.size.0 as u16, self.size.1 as u16);
        self.mp4(entry, mp4::Brand::Mp4)
    }
}

pub fn ivf(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    if data.get(..4) != Some(b"DKIF") {
        return Err("not an IVF file".into());
    }
    let head = data
        .get(6..8)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
        .ok_or("an IVF header cut short")?;
    let mut at = head;
    let mut frames = Vec::new();
    while at + 12 <= data.len() {
        let size =
            u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let body = data
            .get(at + 12..at + 12 + size)
            .ok_or("an IVF frame cut short")?;
        frames.push(body.to_vec());
        at += 12 + size;
    }
    Ok(frames)
}

fn leb128(data: &[u8]) -> Option<(usize, usize)> {
    let mut v = 0usize;
    for (i, b) in data.iter().take(8).enumerate() {
        v |= usize::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

fn obus(unit: &[u8]) -> Result<Vec<(u8, &[u8])>, String> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < unit.len() {
        let head = unit[at];
        let kind = (head >> 3) & 0xf;
        let ext = usize::from(head >> 2 & 1);
        if head >> 1 & 1 == 0 {
            return Err("an AV1 OBU without its size field".into());
        }
        let (size, len) = leb128(&unit[at + 1 + ext..]).ok_or("a bad OBU size")?;
        let end = at + 1 + ext + len + size;
        let obu = unit.get(at..end).ok_or("an OBU past the end of its unit")?;
        out.push((kind, obu));
        at = end;
    }
    Ok(out)
}

pub fn av1(units: &[Vec<u8>]) -> Result<Coded, String> {
    use pito_codecs::av1::parser::{ObuAction, ObuType, Parser};
    let mut config = None;
    let mut samples = Vec::new();
    for unit in units {
        let mut data = Vec::new();
        let mut key = false;
        for (kind, obu) in obus(unit)? {
            if kind == 2 {
                continue;
            }
            if kind == 1 {
                key = true;
                if config.is_none() {
                    let mut p = Parser::default();
                    let ObuAction::Process(o) = p.read_obu(obu).map_err(text)? else {
                        return Err("the sequence header was dropped".into());
                    };
                    if o.header.obu_type != ObuType::SequenceHeader {
                        return Err("no sequence header".into());
                    }
                    let s = p.parse_sequence_header_obu(&o).map_err(text)?.clone();
                    let c = &s.color_config;
                    let op = &s.operating_points[0];
                    let mut av1c = vec![
                        0x81,
                        (s.seq_profile << 5) | (op.seq_level_idx & 0x1f),
                        (u8::from(op.seq_tier != 0) << 7)
                            | (u8::from(c.high_bitdepth) << 6)
                            | (u8::from(c.twelve_bit) << 5)
                            | (u8::from(c.mono_chrome) << 4)
                            | (u8::from(c.subsampling_x) << 3)
                            | (u8::from(c.subsampling_y) << 2)
                            | (c.chroma_sample_position & 3),
                        0,
                    ];
                    av1c.extend_from_slice(obu);
                    config = Some(av1c);
                }
            }
            data.extend_from_slice(obu);
        }
        samples.push((data, key));
    }
    Ok(Coded {
        config: config.ok_or("an AV1 stream with no sequence header")?,
        samples,
    })
}

pub struct Webm {
    pub coded: Coded,
    pub size: (u32, u32),
    pub pocs: Vec<i64>,
}

pub fn webm_samples(path: &Path) -> Result<Webm, String> {
    let r = mkv::Reader::open(path).map_err(text)?;
    let v = r.video().ok_or("no video track")?;
    let t = &r.tracks[v];
    let fps = t.fps().round().max(1.0);
    let mut samples = Vec::new();
    let mut pocs = Vec::new();
    for (k, s) in t.samples.iter().enumerate() {
        samples.push((r.read(v, k).map_err(text)?, s.keyframe));
        pocs.push((t.seconds(s.pts) * fps).round() as i64);
    }
    let coded = Coded {
        config: Vec::new(),
        samples,
    };
    Ok(Webm {
        coded,
        size: (t.width, t.height),
        pocs,
    })
}

pub fn x264_mp4(mkv_path: &Path, fps: u32) -> Result<Vec<u8>, String> {
    let r = mkv::Reader::open(mkv_path).map_err(text)?;
    let v = r.video().ok_or("no video track")?;
    let t = &r.tracks[v];
    let mut samples = Vec::new();
    let mut pocs = Vec::new();
    for (k, s) in t.samples.iter().enumerate() {
        samples.push((r.read(v, k).map_err(text)?, s.keyframe));
        pocs.push((t.seconds(s.pts) * f64::from(fps)).round() as i64);
    }
    let first = pocs.iter().copied().min().unwrap_or(0);
    let pocs: Vec<i64> = pocs.into_iter().map(|p| p - first).collect();
    let coded = Coded {
        config: t.codec_private.clone(),
        samples,
    };
    Timed {
        coded: &coded,
        pocs: &pocs,
        size: (t.width, t.height),
        fps,
    }
    .h264_mp4()
}

pub fn adts(asc: &[u8], packets: &[Vec<u8>]) -> Vec<u8> {
    let object = u32::from(asc[0] >> 3).saturating_sub(1);
    let index = u32::from(asc[0] & 7) << 1 | u32::from(asc[1] >> 7);
    let channels = u32::from(asc[1] >> 3 & 0xf);
    let mut out = Vec::new();
    for p in packets {
        let len = p.len() as u32 + 7;
        out.extend_from_slice(&[
            0xFF,
            0xF1,
            (object << 6 | index << 2 | channels >> 2) as u8,
            ((channels & 3) << 6 | len >> 11) as u8,
            (len >> 3 & 0xff) as u8,
            ((len & 7) << 5 | 0x1f) as u8,
            0xFC,
        ]);
        out.extend_from_slice(p);
    }
    out
}

pub struct Track {
    pub codec_delay: u64,
    pub private: Vec<u8>,
    pub packets: Vec<Vec<u8>>,
    pub discard: i64,
    pub rate: f64,
}

pub fn first_audio(path: &Path) -> Result<Track, String> {
    let r = mkv::Reader::open(path).map_err(text)?;
    let a = *r.audio().first().ok_or("no audio track")?;
    let t = &r.tracks[a];
    let packets = (0..t.samples.len())
        .map(|k| r.read(a, k).map_err(text))
        .collect::<Result<_, _>>()?;
    Ok(Track {
        codec_delay: t.codec_delay,
        private: t.codec_private.clone(),
        packets,
        discard: t.samples.last().map_or(0, |s| s.discard),
        rate: t.audio.as_ref().map_or(48_000.0, |a| a.sampling_frequency),
    })
}

pub fn clip(c: &testref::Clip, area: &str, name: &str) -> Result<Vec<u8>, String> {
    let path = work(area, &format!("clip-{name}"));
    c.write(&path)?;
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn cut(
    samples: &[f32],
    channels: usize,
    from: usize,
    frames: usize,
) -> Result<Vec<u8>, String> {
    let part = samples
        .get(from * channels..(from + frames) * channels)
        .ok_or_else(|| {
            format!(
                "a window of {frames} frames at {from} past the {} frames decoded",
                samples.len() / channels
            )
        })?;
    Ok(part.iter().flat_map(|s| s.to_le_bytes()).collect())
}

pub fn libopus_decode(channels: usize, packets: &[Vec<u8>]) -> Result<Vec<f32>, String> {
    use std::ffi::c_int;
    let pin = crate::pins()?.pin("libopus")?;
    let testref::Probe::Library { path, .. } = &pin.probe else {
        return Err("libopus is pinned as a command".into());
    };
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| format!("{path}: {e}"))?;
    type Create = unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut u8;
    type Decode = unsafe extern "C" fn(*mut u8, *const u8, i32, *mut f32, c_int, c_int) -> c_int;
    type Destroy = unsafe extern "C" fn(*mut u8);
    let missing = |e: libloading::Error| format!("{path}: {e}");
    let create = unsafe { lib.get::<Create>(b"opus_decoder_create") }.map_err(missing)?;
    let decode = unsafe { lib.get::<Decode>(b"opus_decode_float") }.map_err(missing)?;
    let destroy = unsafe { lib.get::<Destroy>(b"opus_decoder_destroy") }.map_err(missing)?;
    let mut err: c_int = 0;
    let dec = unsafe { create(48_000, channels as c_int, &mut err) };
    if dec.is_null() || err != 0 {
        return Err(format!("opus_decoder_create failed with {err}"));
    }
    let mut out = Vec::new();
    let mut buf = vec![0f32; 5760 * channels];
    let mut failed = None;
    for p in packets {
        let n = unsafe { decode(dec, p.as_ptr(), p.len() as i32, buf.as_mut_ptr(), 5760, 0) };
        if n < 0 {
            failed = Some(n);
            break;
        }
        out.extend_from_slice(&buf[..n as usize * channels]);
    }
    unsafe { destroy(dec) };
    match failed {
        Some(n) => Err(format!("opus_decode_float failed with {n}")),
        None => Ok(out),
    }
}

pub fn mp4v(size: (u16, u16), oti: Option<u8>, fps: u32, frames: usize) -> Result<Vec<u8>, String> {
    let format = if oti.is_some() { *b"mp4v" } else { *b"jpeg" };
    let mut raw = vec![0u8; 6];
    raw.extend(1u16.to_be_bytes());
    raw.extend([0u8; 16]);
    raw.extend(size.0.to_be_bytes());
    raw.extend(size.1.to_be_bytes());
    raw.extend(0x0048_0000u32.to_be_bytes());
    raw.extend(0x0048_0000u32.to_be_bytes());
    raw.extend(0u32.to_be_bytes());
    raw.extend(1u16.to_be_bytes());
    raw.extend([0u8; 32]);
    raw.extend(0x18u16.to_be_bytes());
    raw.extend((-1i16).to_be_bytes());
    if let Some(oti) = oti {
        let config = [4, 13, oti, 0x11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let es = [&[3, 21, 0, 1, 0][..], &config, &[6, 1, 2]].concat();
        let body = [&[0, 0, 0, 0][..], &es].concat();
        raw.extend((8 + body.len() as u32).to_be_bytes());
        raw.extend_from_slice(b"esds");
        raw.extend(body);
    }
    let entry = mp4::SampleEntry {
        format: mp4::FourCc(format),
        codec: mp4::CodecConfig::Other(mp4::FourCc(format)),
        video: Some(mp4::VideoParams {
            width: size.0,
            height: size.1,
            pixel_aspect: None,
        }),
        audio: None,
        bitrate: None,
        raw,
    };
    let mut w = mp4::Writer::new(Cursor::new(Vec::new()), mp4::Options::default()).map_err(text)?;
    let t = w
        .add_track(mp4::TrackConfig::new(entry, fps))
        .map_err(text)?;
    for k in 0..frames {
        let mut data = vec![0, 0, 1, 0xb6, k as u8];
        data.resize(64, k as u8);
        w.push_sample(t, &data, k as i64, k as i64, 1, true)
            .map_err(text)?;
    }
    Ok(w.finish_faststart().map_err(text)?.into_inner())
}

fn vint(data: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
    let first = *data.first()?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || data.len() < len {
        return None;
    }
    let mut v = if keep_marker {
        u64::from(first)
    } else {
        u64::from(first & (0xFF >> len))
    };
    for b in &data[1..len] {
        v = v << 8 | u64::from(*b);
    }
    Some((v, len))
}

pub fn unseekable(src: &[u8]) -> Result<Vec<u8>, String> {
    const SEGMENT: u64 = 0x1853_8067;
    const CLUSTER: u64 = 0x1F43_B675;
    const SEEK_HEAD: u64 = 0x114D_9B74;
    const CUES: u64 = 0x1C53_BB6B;
    let mut out = src.to_vec();
    let unknown = |out: &mut [u8], at: usize, len: usize| {
        out[at] = 0xFF >> (len - 1);
        for b in &mut out[at + 1..at + len] {
            *b = 0xFF;
        }
    };
    let mut at = 0;
    while at < src.len() {
        let (id, idl) = vint(&src[at..], true).ok_or("a bad EBML id")?;
        let (size, sl) = vint(&src[at + idl..], false).ok_or("a bad EBML size")?;
        if id != SEGMENT {
            at += idl + sl + size as usize;
            continue;
        }
        unknown(&mut out, at + idl, sl);
        let end = (at + idl + sl + size as usize).min(src.len());
        let mut c = at + idl + sl;
        while c < end {
            let (id, idl) = vint(&src[c..], true).ok_or("a bad EBML id")?;
            let (size, sl) = vint(&src[c + idl..], false).ok_or("a bad EBML size")?;
            let total = idl + sl + size as usize;
            match id {
                CLUSTER => unknown(&mut out, c + idl, sl),
                SEEK_HEAD | CUES if total >= 9 => {
                    out[c] = 0xEC;
                    out[c + 1] = 0x01;
                    out[c + 2..c + 9].copy_from_slice(&((total - 9) as u64).to_be_bytes()[1..]);
                }
                _ => {}
            }
            c += total;
        }
        at = end;
    }
    Ok(out)
}

pub struct BitWriter {
    pub bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn new() -> BitWriter {
        BitWriter {
            bytes: Vec::new(),
            bits: 0,
        }
    }

    pub fn put(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if v >> i & 1 == 1 {
                let at = self.bits / 8;
                self.bytes[at] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

pub fn alac_m4a(samples: &[i16], rate: u32) -> Result<Vec<u8>, String> {
    const FRAME: usize = 4096;
    let mut cookie = Vec::new();
    cookie.extend((FRAME as u32).to_be_bytes());
    cookie.extend([0, 16, 40, 10, 14, 2]);
    cookie.extend(255u16.to_be_bytes());
    cookie.extend(0u32.to_be_bytes());
    cookie.extend(0u32.to_be_bytes());
    cookie.extend(rate.to_be_bytes());
    let mut raw = vec![0u8; 6];
    raw.extend(1u16.to_be_bytes());
    raw.extend([0u8; 8]);
    raw.extend(2u16.to_be_bytes());
    raw.extend(16u16.to_be_bytes());
    raw.extend([0u8; 4]);
    raw.extend((rate << 16).to_be_bytes());
    raw.extend((12 + cookie.len() as u32).to_be_bytes());
    raw.extend_from_slice(b"alac");
    raw.extend([0u8; 4]);
    raw.extend_from_slice(&cookie);
    let entry = mp4::SampleEntry {
        format: mp4::FourCc(*b"alac"),
        codec: mp4::CodecConfig::Other(mp4::FourCc(*b"alac")),
        video: None,
        audio: Some(mp4::AudioParams {
            channels: 2,
            sample_size: 16,
            sample_rate: f64::from(rate),
            ..Default::default()
        }),
        bitrate: None,
        raw,
    };
    let mut w = mp4::Writer::new(Cursor::new(Vec::new()), mp4::Options::default()).map_err(text)?;
    let t = w
        .add_track(mp4::TrackConfig::new(entry, rate))
        .map_err(text)?;
    for (k, frame) in samples.chunks(FRAME * 2).enumerate() {
        let n = frame.len() / 2;
        let mut b = BitWriter::new();
        b.put(1, 3);
        b.put(0, 4);
        b.put(0, 12);
        b.put(if n < FRAME { 0b1001 } else { 0b0001 }, 4);
        if n < FRAME {
            b.put(n as u32, 32);
        }
        for s in frame {
            b.put(u32::from(*s as u16), 16);
        }
        b.put(7, 3);
        let at = (k * FRAME) as i64;
        w.push_sample(t, &b.bytes, at, at, n as u32, true)
            .map_err(text)?;
    }
    Ok(w.finish_faststart().map_err(text)?.into_inner())
}

fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub fn ogg(packets: &[(Vec<u8>, i64)], serial: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pages: Vec<Vec<usize>> = vec![vec![0], (1..3.min(packets.len())).collect()];
    let mut cur: Vec<usize> = Vec::new();
    let mut bytes = 0;
    for (k, (packet, _)) in packets.iter().enumerate().skip(3) {
        let len = packet.len();
        if !cur.is_empty() && (bytes + len > 4096 || cur.len() > 200) {
            pages.push(std::mem::take(&mut cur));
            bytes = 0;
        }
        cur.push(k);
        bytes += len;
    }
    if !cur.is_empty() {
        pages.push(cur);
    }
    let last = pages.len() - 1;
    for (seq, page) in pages.iter().enumerate() {
        let mut lacing = Vec::new();
        let mut body = Vec::new();
        for &k in page {
            let p = &packets[k].0;
            lacing.extend(std::iter::repeat_n(255u8, p.len() / 255));
            lacing.push((p.len() % 255) as u8);
            body.extend_from_slice(p);
        }
        let granule = page.last().map_or(0, |&k| packets[k].1);
        let kind = match seq {
            0 => 2,
            s if s == last => 4,
            _ => 0,
        };
        let mut head = b"OggS".to_vec();
        head.push(0);
        head.push(kind);
        head.extend(granule.to_le_bytes());
        head.extend(serial.to_le_bytes());
        head.extend((seq as u32).to_le_bytes());
        head.extend([0u8; 4]);
        head.push(lacing.len() as u8);
        head.extend(lacing);
        let start = out.len();
        out.extend(head);
        out.extend(body);
        let crc = ogg_crc(&out[start..]);
        out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
    }
    out
}

pub fn xiph_private(headers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![(headers.len() - 1) as u8];
    for h in &headers[..headers.len() - 1] {
        out.extend(std::iter::repeat_n(255u8, h.len() / 255));
        out.push((h.len() % 255) as u8);
    }
    for h in headers {
        out.extend_from_slice(h);
    }
    out
}
