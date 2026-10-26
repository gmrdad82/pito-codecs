use std::path::Path;

use crate::testref::{self, Kind};
use crate::{Def, OURS, Take, sha, tool, words};

#[path = "../../src/aac/refs.rs"]
mod aac;
#[path = "../../src/ac3/refs.rs"]
pub mod ac3;
#[path = "../../src/alac/refs.rs"]
pub mod alac;
#[path = "../../src/flac/refs.rs"]
mod flac;
#[path = "../../src/mp3/refs.rs"]
mod mp3;
#[path = "../../src/opus/refs.rs"]
mod opus;

pub fn defs() -> Vec<Def> {
    [
        aac_defs(),
        ac3_defs(),
        mp3_defs(),
        flac_defs(),
        opus_defs(),
        alac_defs(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub type Bytes = Result<Vec<u8>, String>;
pub type Packets = Vec<(Vec<u8>, u32)>;

pub fn made(
    area: &'static str,
    file: String,
    kind: Kind,
    what: &str,
    f: impl Fn(&Path) -> Bytes + 'static,
) -> Def {
    Def {
        area,
        name: file.clone(),
        file,
        kind,
        make: Some(Box::new(move |out| {
            let dir = out.parent().ok_or("an output with no folder")?;
            testref::write_atomic(out, &f(dir)?)
        })),
        tool: OURS,
        args: words(what),
        take: Take::Keep,
        lib: None,
        settle: None,
    }
}

pub fn fixture(
    area: &'static str,
    file: String,
    what: &str,
    f: impl Fn(&Path) -> Bytes + 'static,
) -> Def {
    made(area, file, Kind::Fixture, what, f)
}

pub fn cut(
    area: &'static str,
    file: String,
    what: &str,
    f: impl Fn(&Path) -> Bytes + 'static,
) -> Def {
    made(area, file, Kind::Tolerance, what, f)
}

pub fn read(dir: &Path, name: &str) -> Bytes {
    std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))
}

pub fn f32s(dir: &Path, name: &str) -> Result<Vec<f32>, String> {
    Ok(testref::le_f32(&read(dir, name)?))
}

pub fn f32_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

pub fn ints(samples: &[f32], bits: u32) -> Vec<i32> {
    let k = (1i64 << (bits - 1)) as f64;
    samples
        .iter()
        .map(|&s| (s as f64 * k).round().clamp(-k, k - 1.0) as i32)
        .collect()
}

pub fn wav(samples: &[i32], rate: u32, channels: u16, bits: u32) -> Vec<u8> {
    let width = bits.div_ceil(8) as usize;
    let mut data = Vec::with_capacity(samples.len() * width);
    for &s in samples {
        data.extend_from_slice(&s.to_le_bytes()[..width]);
    }
    let block = channels as u32 * width as u32;
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * block).to_le_bytes());
    out.extend_from_slice(&(block as u16).to_le_bytes());
    out.extend_from_slice(&(bits as u16).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend(data);
    out
}

pub fn wav16(samples: &[f32], rate: u32, channels: u16) -> Vec<u8> {
    wav(&ints(samples, 16), rate, channels, 16)
}

pub fn lcg(seed: &mut u32) -> f32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*seed >> 8) as f32 / (1u32 << 24) as f32
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

pub mod mux {
    use super::Bytes;
    use pito_codecs::media::TimeBase;
    use pito_codecs::mp4::{AudioParams, Brand, CodecConfig, Edit, FourCc, SampleEntry, TrackKind};
    use std::io::Cursor;

    pub struct Track {
        pub format: [u8; 4],
        pub entry: Vec<u8>,
        pub mkv_id: &'static str,
        pub private: Vec<u8>,
        pub rate: u32,
        pub channels: u16,
        pub bits: u16,
        pub packets: super::Packets,
        pub priming: u32,
        pub length: u64,
        pub delay: f64,
        pub reserve: bool,
    }

    impl Track {
        pub fn new(rate: u32, channels: u16, packets: super::Packets) -> Track {
            let length = packets.iter().map(|p| u64::from(p.1)).sum();
            Track {
                format: *b"NONE",
                entry: Vec::new(),
                mkv_id: "",
                private: Vec::new(),
                rate,
                channels,
                bits: 16,
                packets,
                priming: 0,
                length,
                delay: 0.0,
                reserve: false,
            }
        }

        fn starts(&self) -> Vec<(i64, u32)> {
            let mut at = 0i64;
            let end = i64::from(self.priming) + self.length as i64;
            let n = self.packets.len();
            self.packets
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let d = if k + 1 == n {
                        u32::try_from((end - at).max(1)).unwrap_or(p.1)
                    } else {
                        p.1
                    };
                    let s = (at, d);
                    at += i64::from(p.1);
                    s
                })
                .collect()
        }
    }

    pub fn sound_entry(
        channels: u16,
        bits: u16,
        rate: u32,
        boxes: &[(&[u8; 4], Vec<u8>)],
    ) -> Vec<u8> {
        let mut e = vec![0u8; 6];
        e.extend_from_slice(&1u16.to_be_bytes());
        e.extend_from_slice(&[0; 8]);
        e.extend_from_slice(&channels.to_be_bytes());
        e.extend_from_slice(&bits.to_be_bytes());
        e.extend_from_slice(&[0; 4]);
        let fixed = if rate < 65536 { rate << 16 } else { 0 };
        e.extend_from_slice(&fixed.to_be_bytes());
        for (kind, body) in boxes {
            e.extend(boxed(kind, body));
        }
        e
    }

    pub fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    pub fn esds(oti: u8, asc: &[u8], bitrate: u32) -> Vec<u8> {
        let mut dc = vec![oti, 0x15, 0, 0, 0];
        dc.extend_from_slice(&bitrate.to_be_bytes());
        dc.extend_from_slice(&bitrate.to_be_bytes());
        if !asc.is_empty() {
            dc.extend(descriptor(5, asc));
        }
        let mut es = vec![0, 1, 0];
        es.extend(descriptor(4, &dc));
        es.extend(descriptor(6, &[2]));
        let mut b = vec![0u8; 4];
        b.extend(descriptor(3, &es));
        b
    }

    fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
        let n = body.len() as u32;
        let mut d = vec![
            tag,
            0x80 | (n >> 21) as u8 & 0x7f,
            0x80 | (n >> 14) as u8 & 0x7f,
            0x80 | (n >> 7) as u8 & 0x7f,
            n as u8 & 0x7f,
        ];
        d.extend_from_slice(body);
        d
    }

    fn text<E: std::fmt::Display>(e: E) -> String {
        e.to_string()
    }

    fn order(tracks: &[Track]) -> Vec<(usize, usize, i64, u32)> {
        let mut all: Vec<(f64, usize, usize, i64, u32)> = Vec::new();
        for (t, track) in tracks.iter().enumerate() {
            for (k, (at, d)) in track.starts().into_iter().enumerate() {
                let s =
                    track.delay + (at - i64::from(track.priming)) as f64 / f64::from(track.rate);
                all.push((s, t, k, at, d));
            }
        }
        all.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        all.into_iter()
            .map(|(_, t, k, at, d)| (t, k, at, d))
            .collect()
    }

    pub fn mp4(tracks: &[Track], brand: Brand) -> Bytes {
        let opts = pito_codecs::mp4::Options {
            brand,
            ..Default::default()
        };
        let mut w = pito_codecs::mp4::Writer::new(Cursor::new(Vec::new()), opts).map_err(text)?;
        let movie = tracks.first().map_or(1000, |t| t.rate);
        w.set_movie_timescale(movie);
        let mut ids = Vec::new();
        for t in tracks {
            let entry = SampleEntry {
                format: FourCc(t.format),
                codec: CodecConfig::Other(FourCc(t.format)),
                video: None,
                audio: Some(AudioParams {
                    channels: u32::from(t.channels),
                    sample_size: t.bits,
                    sample_rate: f64::from(t.rate),
                    ..Default::default()
                }),
                bitrate: None,
                raw: t.entry.clone(),
            };
            let mut cfg = pito_codecs::mp4::TrackConfig::new(entry, t.rate);
            let scale = |n: u64, from: u32| n * u64::from(movie) / u64::from(from.max(1));
            if t.delay > 0.0 {
                cfg.edits.push(Edit {
                    segment_duration: (t.delay * f64::from(movie)).round() as u64,
                    media_time: -1,
                    media_rate: 0x10000,
                });
            }
            if t.delay > 0.0 || t.priming > 0 {
                cfg.edits.push(Edit {
                    segment_duration: scale(t.length, t.rate),
                    media_time: i64::from(t.priming),
                    media_rate: 0x10000,
                });
            }
            ids.push(w.add_track(cfg).map_err(text)?);
        }
        for (t, k, at, d) in order(tracks) {
            let data = &tracks[t].packets[k].0;
            w.push_sample(ids[t], data, at, at, d, true).map_err(text)?;
        }
        Ok(w.finish().map_err(text)?.into_inner())
    }

    pub fn mkv(tracks: &[Track], webm: bool) -> Bytes {
        let opts = if webm {
            pito_codecs::mkv::Options::webm()
        } else {
            pito_codecs::mkv::Options::default()
        };
        let mut w = pito_codecs::mkv::Writer::new(Cursor::new(Vec::new()), opts);
        let mut ids = Vec::new();
        for t in tracks {
            let cfg = pito_codecs::mkv::TrackConfig {
                codec_private: t.private.clone(),
                sample_rate: t.rate,
                channels: u32::from(t.channels),
                codec_delay: u64::from(t.priming) * 1_000_000_000 / u64::from(t.rate.max(1)),
                seek_pre_roll: if t.reserve { 0x00ff_ffff_ffff_ffff } else { 0 },
                ..pito_codecs::mkv::TrackConfig::new(
                    TrackKind::Audio,
                    t.mkv_id,
                    TimeBase::new(1, t.rate.max(1)),
                )
            };
            ids.push(w.add_track(cfg).map_err(text)?);
        }
        for (t, k, at, d) in order(tracks) {
            let track = &tracks[t];
            let offset = (track.delay * f64::from(track.rate)).round() as i64;
            let pts = at - i64::from(track.priming) + offset;
            w.push_sample(ids[t], &track.packets[k].0, pts, Some(d), true)
                .map_err(text)?;
        }
        Ok(w.finish().map_err(text)?.into_inner())
    }

    pub fn by_ext(ext: &str, tracks: &[Track]) -> Bytes {
        match ext {
            "mp4" | "m4a" | "3gp" => mp4(tracks, Brand::Mp4),
            "mov" => mp4(tracks, Brand::Mov),
            "mkv" | "mka" => mkv(tracks, false),
            "webm" => mkv(tracks, true),
            _ => Err(format!("no muxer for .{ext}")),
        }
    }
}

fn aac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (channels, _, kbps) in aac::STREAMS {
        let kbps = kbps.to_string();
        defs.push(
            Def::fixture(
                aac::AREA,
                aac::stream(channels),
                "faac",
                tool(
                    "faac",
                    &["-v0", "--pns", "0", "-b", &kbps, "-o", "{out}", "{src}"],
                ),
            )
            .from(move || {
                let pcm = aac::signal(aac::RATE, channels, aac::SECONDS);
                Ok(wav16(&pcm, aac::RATE, channels))
            }),
        );
        let full = format!("{}.faad", aac::stream(channels));
        let input = format!("{{ref:{}}}", aac::stream(channels));
        defs.push(Def::digest(
            aac::AREA,
            full.clone(),
            "faad",
            tool("faad", &["-q", "-f", "2", "-b", "4", "-o", "{out}", &input]),
            sha,
        ));
        defs.push(cut(
            aac::AREA,
            aac::decoded(channels),
            &format!("src/aac/refs.rs window of {full}"),
            move |dir| {
                let pcm = f32s(dir, &full)?;
                Ok(f32_bytes(&aac::window::window(&pcm, channels as usize)))
            },
        ));
    }
    defs
}

fn mp3_signal(rate: u32, channels: u16, frames: usize) -> Vec<f32> {
    use std::f64::consts::TAU;
    let mut seeds = [0x1234_5678u32, 0x9abc_def0];
    let mut out = Vec::with_capacity(frames * channels as usize);
    for i in 0..frames {
        let t = i as f64 / f64::from(rate);
        let left = 0.25 * (TAU * (300.0 + 200.0 * t) * t).sin()
            + 0.2 * (TAU * 3100.0 * t).sin() * f64::from(u8::from(t % 0.37 < 0.03));
        let right = 0.25 * (TAU * 523.0 * t).sin()
            + 0.15 * (TAU * 7000.0 * t).sin() * f64::from(u8::from(t % 0.29 < 0.02));
        let noise = |s: &mut u32| 0.1 * (f64::from(lcg(s)) - 0.5);
        let l = left + noise(&mut seeds[0]);
        let r = right + noise(&mut seeds[1]);
        if channels == 1 {
            out.push(l as f32);
        } else {
            out.extend([l as f32, r as f32]);
        }
    }
    out
}

fn lame(c: &'static mp3::Case, tag: bool) -> Def {
    let mut args = vec!["--quiet"];
    if tag {
        args.extend(["--add-id3v2", "--tt", "pito-codecs"]);
    } else {
        args.push("-t");
    }
    args.extend(c.lame);
    args.extend(["{src}", "{out}"]);
    let wav = move || {
        let pcm = mp3_signal(c.rate, c.channels, mp3::frames(c));
        Ok(wav16(&pcm, c.rate, c.channels))
    };
    Def::fixture(mp3::AREA, mp3::stream(c.name), "lame", tool("lame", &args)).from(wav)
}

fn mpg123(c: &'static mp3::Case, gapless: bool, window: fn(&[f32], usize) -> Vec<f32>) -> [Def; 2] {
    let file = mp3::stream(c.name);
    let full = format!("{file}.mpg123");
    let input = format!("{{ref:{file}}}");
    let mut args = vec!["-q", "-e", "f32"];
    if !gapless {
        args.push("--no-gapless");
    }
    args.extend(["-O", "{out}", &input]);
    let ch = c.channels as usize;
    let what = format!("src/mp3/refs.rs window of {full}");
    [
        Def::digest(
            mp3::AREA,
            full.clone(),
            "mpg123",
            tool("mpg123", &args),
            sha,
        ),
        cut(mp3::AREA, mp3::decoded(&file), &what, move |dir| {
            Ok(f32_bytes(&window(&f32s(dir, &full)?, ch)))
        }),
    ]
}

pub fn mp3_frames(data: &[u8]) -> Result<(Packets, u32), String> {
    let mut dec = pito_codecs::mp3::Decoder::new();
    let mut pcm = vec![0.0; pito_codecs::mp3::MAX_SAMPLES_PER_FRAME];
    let mut rest = data;
    let mut frames = Vec::new();
    let mut priming = None;
    while !rest.is_empty() {
        let f = dec.decode(rest, &mut pcm);
        if f.frame_bytes == 0 {
            break;
        }
        let frame = &rest[f.frame_offset..f.frame_bytes];
        match pito_codecs::mp3::vbr_tag(frame) {
            Some(tag) if priming.is_none() && frames.is_empty() => {
                priming = Some(tag.delay as u32);
            }
            _ => frames.push((frame.to_vec(), f.samples as u32)),
        }
        rest = &rest[f.frame_bytes..];
    }
    Ok((
        frames,
        priming.ok_or("no LAME tag to read the encoder delay from")?,
    ))
}

fn mp3_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for c in &mp3::PLAIN {
        defs.push(lame(c, false));
        defs.extend(mpg123(c, false, mp3::window::window));
    }
    for c in &mp3::GAPLESS {
        defs.push(lame(c, true));
        defs.extend(mpg123(c, true, mp3::window::window));
    }
    let d = &mp3::DELAY;
    let mut stream = lame(d, true);
    stream.file = String::new();
    stream.kind = Kind::BitExact;
    stream.take = Take::Digest(sha);
    defs.push(stream);
    defs.extend(mpg123(d, true, |pcm, ch| {
        let (start, end) = mp3::delay_samples();
        mp3::window::window_range(pcm, ch, start, end)
    }));
    for ext in mp3::DELAY_EXTS {
        let what = format!(
            "our {ext} writer: the frames of {} after its LAME tag",
            mp3::stream(d.name)
        );
        defs.push(fixture(mp3::AREA, mp3::delay(ext), &what, move |dir| {
            let (packets, priming) = mp3_frames(&read(dir, &mp3::stream(d.name))?)?;
            let mut t = mux::Track::new(d.rate, d.channels, packets);
            t.format = *b"mp4a";
            t.entry = mux::sound_entry(
                d.channels,
                16,
                d.rate,
                &[(b"esds", mux::esds(0x6b, &[], 160_000))],
            );
            t.mkv_id = "A_MPEG/L3";
            t.priming = priming;
            t.length = mp3::frames(d) as u64;
            mux::by_ext(ext, &[t])
        }));
    }
    defs
}

pub fn flac_frames(data: &[u8]) -> Result<(Vec<u8>, Packets), String> {
    let info = pito_codecs::flac::config(data).map_err(|e| e.to_string())?;
    if !data.starts_with(b"fLaC") {
        return Err("no fLaC marker".into());
    }
    let mut at = 4;
    loop {
        let head = data.get(at..at + 4).ok_or("metadata cut short")?;
        at += 4 + (u32::from_be_bytes([0, head[1], head[2], head[3]]) as usize);
        if head[0] & 0x80 != 0 {
            break;
        }
    }
    let mut streaminfo = b"fLaC\x80\x00\x00\x22".to_vec();
    streaminfo.extend_from_slice(data.get(8..42).ok_or("no STREAMINFO")?);
    let block = u64::from(info.max_block);
    let mut left = info.samples;
    let mut frames = Vec::new();
    while at < data.len() {
        let end = (at + 2..=data.len())
            .filter(|&e| e == data.len() || (data[e] == 0xff && data[e + 1] & 0xfe == 0xf8))
            .find(|&e| crc16(&data[at..e - 2]) == u16::from_be_bytes([data[e - 2], data[e - 1]]))
            .ok_or("a FLAC frame with no CRC-16 end")?;
        let n = block.min(left);
        frames.push((data[at..end].to_vec(), n as u32));
        left -= n;
        at = end;
    }
    Ok((streaminfo, frames))
}

fn tracked_flac(name: &str) -> Bytes {
    let path = testref::root()
        .join("tests/fixtures/flac")
        .join(format!("{name}.flac"));
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn flac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for (name, exts) in flac::REMUXED {
        for ext in exts {
            let what = format!("our {ext} writer: the frames of tests/fixtures/flac/{name}.flac");
            defs.push(fixture(
                flac::AREA,
                flac::remuxed(name, ext),
                &what,
                move |_| {
                    let data = tracked_flac(name)?;
                    let info = pito_codecs::flac::config(&data).map_err(|e| e.to_string())?;
                    let (head, packets) = flac_frames(&data)?;
                    let channels = info.channels as u16;
                    let mut t = mux::Track::new(info.rate, channels, packets);
                    t.bits = info.bits as u16;
                    t.format = *b"fLaC";
                    let mut dfla = vec![0u8; 4];
                    dfla.extend_from_slice(&head[4..]);
                    t.entry = mux::sound_entry(channels, t.bits, info.rate, &[(b"dfLa", dfla)]);
                    t.mkv_id = "A_FLAC";
                    t.private = head;
                    mux::by_ext(ext, &[t])
                },
            ));
        }
    }
    defs
}

#[cfg(target_os = "linux")]
pub fn lib_path(name: &str) -> Result<String, String> {
    match &crate::pins()?.pin(name)?.probe {
        testref::Probe::Library { path, .. } => Ok(path.clone()),
        testref::Probe::Command { .. } => Err(format!("{name} is pinned as a command")),
    }
}

#[cfg(target_os = "linux")]
mod libopus {
    use super::Packets;
    use libloading::{Library, Symbol};
    use std::ffi::{c_int, c_void};

    type Ctl = unsafe extern "C" fn(*mut c_void, c_int, ...) -> c_int;

    const SET_BITRATE: c_int = 4002;
    const SET_VBR: c_int = 4006;
    const GET_LOOKAHEAD: c_int = 4027;
    const SET_FORCE_MODE: c_int = 11002;
    const MODE_HYBRID: c_int = 1001;
    const VOIP: c_int = 2048;
    const AUDIO: c_int = 2049;
    const MAX_FRAME: usize = 5760;

    pub struct Opus {
        lib: Library,
    }

    pub struct Stream {
        pub packets: Vec<Vec<u8>>,
        pub pre_skip: u16,
        pub streams: usize,
        pub coupled: usize,
        pub mapping: Vec<u8>,
    }

    fn sym<'a, T>(lib: &'a Library, name: &str) -> Result<Symbol<'a, T>, String> {
        unsafe { lib.get::<T>(name.as_bytes()) }.map_err(|e| format!("libopus has no {name}: {e}"))
    }

    fn check(what: &str, code: c_int) -> Result<(), String> {
        if code < 0 {
            return Err(format!("libopus {what} failed with {code}"));
        }
        Ok(())
    }

    impl Opus {
        pub fn open() -> Result<Opus, String> {
            crate::lib_version("libopus")?;
            let path = super::lib_path("libopus")?;
            let lib = unsafe { Library::new(&path) }.map_err(|e| format!("{path}: {e}"))?;
            Ok(Opus { lib })
        }

        pub fn encode(
            &self,
            pcm: &[f32],
            channels: u16,
            bitrate: i32,
            cbr: bool,
            voip: bool,
            hybrid: bool,
        ) -> Result<Stream, String> {
            let ch = usize::from(channels);
            let app = if voip { VOIP } else { AUDIO };
            let mut err: c_int = 0;
            let (mut streams, mut coupled) = (1 as c_int, (ch == 2) as c_int);
            let mut mapping = vec![0u8; ch];
            let surround = ch > 2;
            let enc = if surround {
                let create: Symbol<
                    unsafe extern "C" fn(
                        i32,
                        c_int,
                        c_int,
                        *mut c_int,
                        *mut c_int,
                        *mut u8,
                        c_int,
                        *mut c_int,
                    ) -> *mut c_void,
                > = sym(&self.lib, "opus_multistream_surround_encoder_create")?;
                unsafe {
                    create(
                        48_000,
                        ch as c_int,
                        1,
                        &mut streams,
                        &mut coupled,
                        mapping.as_mut_ptr(),
                        app,
                        &mut err,
                    )
                }
            } else {
                let create: Symbol<
                    unsafe extern "C" fn(i32, c_int, c_int, *mut c_int) -> *mut c_void,
                > = sym(&self.lib, "opus_encoder_create")?;
                unsafe { create(48_000, ch as c_int, app, &mut err) }
            };
            check("encoder create", err)?;
            if enc.is_null() {
                return Err("libopus gave no encoder".into());
            }
            let prefix = if surround {
                "opus_multistream_"
            } else {
                "opus_"
            };
            let result = (|| {
                let ctl: Symbol<Ctl> = sym(&self.lib, &format!("{prefix}encoder_ctl"))?;
                check("bitrate", unsafe { ctl(enc, SET_BITRATE, bitrate) })?;
                check("vbr", unsafe { ctl(enc, SET_VBR, c_int::from(!cbr)) })?;
                if hybrid {
                    check("mode", unsafe { ctl(enc, SET_FORCE_MODE, MODE_HYBRID) })?;
                }
                let mut lookahead: c_int = 0;
                check("lookahead", unsafe {
                    ctl(enc, GET_LOOKAHEAD, &mut lookahead as *mut c_int)
                })?;
                let encode: Symbol<
                    unsafe extern "C" fn(*mut c_void, *const f32, c_int, *mut u8, i32) -> i32,
                > = sym(&self.lib, &format!("{prefix}encode_float"))?;
                let frames = (pcm.len() / ch + lookahead as usize).div_ceil(960);
                let mut padded = pcm.to_vec();
                padded.resize(frames * 960 * ch, 0.0);
                let mut out = vec![0u8; 4000 * ch];
                let mut packets = Vec::with_capacity(frames);
                for f in padded.chunks_exact(960 * ch) {
                    let n =
                        unsafe { encode(enc, f.as_ptr(), 960, out.as_mut_ptr(), out.len() as i32) };
                    check("encode", n)?;
                    packets.push(out[..n as usize].to_vec());
                }
                Ok(Stream {
                    packets,
                    pre_skip: lookahead as u16,
                    streams: streams as usize,
                    coupled: coupled as usize,
                    mapping: if surround {
                        mapping.clone()
                    } else {
                        Vec::new()
                    },
                })
            })();
            let destroy: Symbol<unsafe extern "C" fn(*mut c_void)> =
                sym(&self.lib, &format!("{prefix}encoder_destroy"))?;
            unsafe { destroy(enc) };
            result
        }

        pub fn decode(
            &self,
            head: &pito_codecs::opus::OpusHead,
            packets: &Packets,
        ) -> Result<Vec<f32>, String> {
            let ch = head.channels;
            let multi = head.mapping_family != 0;
            let mut err: c_int = 0;
            let dec = if multi {
                let create: Symbol<
                    unsafe extern "C" fn(
                        i32,
                        c_int,
                        c_int,
                        c_int,
                        *const u8,
                        *mut c_int,
                    ) -> *mut c_void,
                > = sym(&self.lib, "opus_multistream_decoder_create")?;
                unsafe {
                    create(
                        48_000,
                        ch as c_int,
                        head.streams as c_int,
                        head.coupled_streams as c_int,
                        head.mapping.as_ptr(),
                        &mut err,
                    )
                }
            } else {
                let create: Symbol<unsafe extern "C" fn(i32, c_int, *mut c_int) -> *mut c_void> =
                    sym(&self.lib, "opus_decoder_create")?;
                unsafe { create(48_000, ch as c_int, &mut err) }
            };
            check("decoder create", err)?;
            if dec.is_null() {
                return Err("libopus gave no decoder".into());
            }
            let prefix = if multi { "opus_multistream_" } else { "opus_" };
            let result = (|| {
                let decode: Symbol<
                    unsafe extern "C" fn(
                        *mut c_void,
                        *const u8,
                        i32,
                        *mut f32,
                        c_int,
                        c_int,
                    ) -> c_int,
                > = sym(&self.lib, &format!("{prefix}decode_float"))?;
                let mut pcm = vec![0f32; MAX_FRAME * ch];
                let mut out = Vec::new();
                for (p, _) in packets {
                    let n = unsafe {
                        decode(
                            dec,
                            p.as_ptr(),
                            p.len() as i32,
                            pcm.as_mut_ptr(),
                            MAX_FRAME as c_int,
                            0,
                        )
                    };
                    check("decode", n)?;
                    out.extend_from_slice(&pcm[..n as usize * ch]);
                }
                Ok(out)
            })();
            let destroy: Symbol<unsafe extern "C" fn(*mut c_void)> =
                sym(&self.lib, &format!("{prefix}decoder_destroy"))?;
            unsafe { destroy(dec) };
            result
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod libopus {
    pub struct Opus;
    pub struct Stream {
        pub packets: Vec<Vec<u8>>,
        pub pre_skip: u16,
        pub streams: usize,
        pub coupled: usize,
        pub mapping: Vec<u8>,
    }
    impl Opus {
        pub fn open() -> Result<Opus, String> {
            Err("libopus loads only on Linux".into())
        }
        pub fn encode(
            &self,
            _: &[f32],
            _: u16,
            _: i32,
            _: bool,
            _: bool,
            _: bool,
        ) -> Result<Stream, String> {
            Err("libopus loads only on Linux".into())
        }
        pub fn decode(
            &self,
            _: &pito_codecs::opus::OpusHead,
            _: &super::Packets,
        ) -> Result<Vec<f32>, String> {
            Err("libopus loads only on Linux".into())
        }
    }
}

fn opus_music(channels: u16) -> Vec<f32> {
    use std::f64::consts::TAU;
    let n = (48_000.0 * opus::SECONDS) as usize;
    let mut seeds = [0x0bad_cafeu32, 0x5eed_1234];
    let mut out = Vec::with_capacity(n * usize::from(channels));
    for i in 0..n {
        let t = i as f64 / 48_000.0;
        let burst = f64::from(u8::from(t % 0.5 > 0.45));
        let l = 0.3 * (TAU * 220.0 * t).sin()
            + 0.2 * (TAU * 277.2 * t + (TAU * 5.0 * t).sin()).sin()
            + 0.15 * (TAU * 329.6 * t).sin() * (TAU * 0.5 * t).sin()
            + 0.05 * (TAU * 3520.0 * t).sin()
            + 0.08 * (f64::from(lcg(&mut seeds[0])) - 0.5) * burst;
        let r = 0.25 * (TAU * 330.0 * t).sin()
            + 0.2 * (TAU * 440.0 * t + 0.3 * (TAU * 3.0 * t).sin()).sin()
            + 0.05 * (TAU * 7040.0 * t).sin()
            + 0.05 * (f64::from(lcg(&mut seeds[1])) - 0.5);
        let frame = match channels {
            1 => vec![l],
            2 => vec![l, r],
            _ => vec![l, 0.5 * l + 0.5 * r, r, 0.8 * l, 0.8 * r, 0.1 * l],
        };
        out.extend(frame.into_iter().map(|v| v as f32));
    }
    out
}

fn opus_speech() -> Vec<f32> {
    use std::f64::consts::TAU;
    let n = (48_000.0 * opus::SECONDS) as usize;
    let mut seed = 0x0051_eec4_u32;
    (0..n)
        .map(|i| {
            let t = i as f64 / 48_000.0;
            let wobble = (TAU * 3.0 * t).sin();
            let voice = 0.4 * (TAU * (120.0 + 20.0 * wobble) * t).sin()
                + 0.2 * (TAU * (240.0 + 40.0 * wobble) * t).sin()
                + 0.1 * (TAU * 900.0 * t).sin()
                + 0.05 * (f64::from(lcg(&mut seed)) - 0.5);
            let gate = f64::from(u8::from((TAU * 1.3 * t).sin() > -0.6));
            (voice * (0.6 + 0.4 * (TAU * 4.0 * t).sin()) * gate) as f32
        })
        .collect()
}

fn ogg_opus(head: &pito_codecs::opus::OpusHead, packets: &[Vec<u8>], length: u64) -> Bytes {
    let text = |e: pito_codecs::media::Error| e.to_string();
    let mut w = pito_codecs::opus::ogg::Writer::new(Vec::new(), 0x7069_746f, head, "libopus")
        .map_err(text)?;
    let end = i64::from(head.pre_skip) + length as i64;
    for (k, p) in packets.iter().enumerate() {
        w.push(p, (((k + 1) * 960) as i64).min(end)).map_err(text)?;
    }
    w.finish().map_err(text)
}

fn libopus_decoded(file: String, channels: usize) -> Def {
    let what =
        format!("libopus decode of {file}, pre-skip and end trimmed, src/opus/refs.rs window");
    let mut d = cut(opus::AREA, opus::decoded(&file), &what, move |dir| {
        let ogg = pito_codecs::opus::ogg::OggOpus::parse(&read(dir, &file)?)
            .map_err(|e| e.to_string())?;
        let packets: Packets = (0..ogg.packets.len())
            .map(|i| (ogg.packet(i).to_vec(), 0))
            .collect();
        let pcm = libopus::Opus::open()?.decode(&ogg.head, &packets)?;
        let skip = usize::from(ogg.head.pre_skip) * channels;
        let keep = ogg.samples() as usize * channels;
        let pcm = pcm
            .get(skip..skip + keep)
            .ok_or("libopus gave fewer samples than the granule")?;
        Ok(f32_bytes(&opus::window::window(pcm, channels)))
    });
    d.lib = Some("libopus");
    d
}

fn opus_defs() -> Vec<Def> {
    let area = opus::AREA;
    let mut defs = Vec::new();
    for c in &opus::LIBOPUS {
        let what = format!(
            "libopus {} bit/s, {}, {}{}, 20 ms frames, our Ogg writer",
            c.bitrate,
            if c.cbr { "CBR" } else { "VBR" },
            if c.voip { "VoIP" } else { "audio" },
            if c.music && !c.celt {
                ", hybrid forced"
            } else {
                ""
            }
        );
        let mut d = fixture(area, opus::stream(c.name), &what, move |_| {
            let pcm = if c.music {
                opus_music(c.channels)
            } else {
                opus_speech()
            };
            let s = libopus::Opus::open()?.encode(
                &pcm,
                c.channels,
                c.bitrate,
                c.cbr,
                c.voip,
                c.music && !c.celt,
            )?;
            let head = pito_codecs::opus::OpusHead {
                channels: usize::from(c.channels),
                pre_skip: s.pre_skip,
                input_sample_rate: 48_000,
                output_gain: 0,
                mapping_family: u8::from(c.channels > 2),
                streams: s.streams,
                coupled_streams: s.coupled,
                mapping: s.mapping,
            };
            ogg_opus(
                &head,
                &s.packets,
                (pcm.len() / usize::from(c.channels)) as u64,
            )
        });
        d.lib = Some("libopus");
        defs.push(d);
        defs.push(libopus_decoded(
            opus::stream(c.name),
            usize::from(c.channels),
        ));
    }
    for (channels, bitrate) in opus::OURS {
        let file = opus::ours(channels, bitrate);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/opus/refs.rs encode_ours({channels}, {bitrate})"),
            move || opus::encode_ours(channels, bitrate),
        ));
        defs.push(libopus_decoded(file, usize::from(channels)));
    }
    for (channels, bitrate) in opus::SILK {
        let file = opus::silk(channels, bitrate);
        defs.push(Def::ours(
            area,
            file.clone(),
            &format!("src/opus/refs.rs encode_silk({channels}, {bitrate})"),
            move || opus::encode_silk(channels, bitrate),
        ));
        defs.push(libopus_decoded(file, usize::from(channels)));
    }
    defs
}

fn a52dec_slots(c: &ac3::Case) -> Vec<Vec<(usize, f32)>> {
    let k = std::f32::consts::FRAC_1_SQRT_2;
    if c.channels() <= 2 && !c.lfe {
        return (0..c.channels() as usize).map(|i| vec![(i, 1.0)]).collect();
    }
    let mut out = vec![vec![(0, 1.0)], vec![(1, 1.0)]];
    if matches!(c.acmod, 3 | 5 | 7) {
        out.push(vec![(2, 1.0)]);
    }
    if c.lfe {
        out.push(vec![(3, 1.0)]);
    }
    match c.acmod {
        4 | 5 => out.push(vec![(4, k), (5, k)]),
        6 | 7 => out.extend([vec![(4, 1.0)], vec![(5, 1.0)]]),
        _ => {}
    }
    out
}

fn wav_pcm(wav: &[u8]) -> Result<(usize, Vec<f32>), String> {
    let channels = wav.get(22..24).ok_or("a WAV with no fmt chunk")?;
    let channels = u16::from_le_bytes([channels[0], channels[1]]) as usize;
    let at = wav
        .windows(4)
        .position(|w| w == b"data")
        .ok_or("a WAV with no data chunk")?
        + 8;
    let pcm = wav[at..]
        .chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect();
    Ok((channels, pcm))
}

fn a52dec(c: &'static ac3::Case) -> [Def; 2] {
    let full = format!("{}.a52dec.wav", c.name);
    let input = c.plain();
    let args = ["-r", "-o", "wav6"];
    let mut run = Def::digest(
        ac3::AREA,
        full.clone(),
        "a52dec",
        tool(
            "a52dec",
            &[&args[..], &[&format!("{{ref:{input}}}")]].concat(),
        ),
        sha,
    );
    run.make = Some(Box::new(move |src| {
        let dir = src.parent().ok_or("a source with no folder")?;
        let o = std::process::Command::new("a52dec")
            .args(args)
            .arg(dir.join(&input))
            .stderr(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("a52dec did not start: {e}"))?;
        if !o.status.success() {
            return Err(format!("a52dec failed on {input}"));
        }
        let out = src.with_file_name(format!("{}.a52dec.wav", c.name));
        testref::write_atomic(&out, &o.stdout)
    }));
    let what = format!(
        "a52dec's standard output for {full}, in our channel slots, src/ac3/refs.rs window"
    );
    let window = cut(ac3::AREA, c.decoded(), &what, move |dir| {
        let (wch, pcm) = wav_pcm(&read(dir, &full)?)?;
        let slots = a52dec_slots(c);
        if slots.len() != wch {
            return Err(format!(
                "{}: a52dec wrote {wch} channels, {} expected",
                c.name,
                slots.len()
            ));
        }
        let n = if c.channels() <= 2 && !c.lfe { wch } else { 6 };
        let frames = pcm.len() / wch;
        let mut out = vec![0f32; frames * n];
        for i in 0..frames {
            for (a, to) in slots.iter().enumerate() {
                for &(slot, g) in to {
                    out[i * n + slot] += pcm[i * wch + a] * g;
                }
            }
        }
        Ok(f32_bytes(&ac3::window::window(&out, n)))
    });
    [run, window]
}

fn ac3_frames(data: &[u8], size: usize) -> Packets {
    data.chunks(size).map(|f| (f.to_vec(), 1536)).collect()
}

pub fn aac_track(pcm: &[f32], rate: u32, channels: u16, kbps: u32) -> Result<mux::Track, String> {
    let text = |e: pito_codecs::media::Error| e.to_string();
    let cfg = pito_codecs::aac::EncoderConfig {
        rate,
        channels,
        bitrate: kbps * 1000,
    };
    let mut enc = pito_codecs::aac::Encoder::new(cfg).map_err(text)?;
    let mut buf = pito_codecs::media::AudioBuffer::new(rate, channels);
    buf.samples = pcm.to_vec();
    let mut packets = enc.encode(&buf).map_err(text)?;
    packets.extend(enc.flush());
    let asc = enc.extradata();
    let timed: Packets = packets.into_iter().map(|p| (p.data, 1024)).collect();
    let mut t = mux::Track::new(rate, channels, timed);
    t.priming = enc.priming();
    t.length = (pcm.len() / usize::from(channels.max(1))) as u64;
    t.format = *b"mp4a";
    t.entry = mux::sound_entry(
        channels,
        16,
        rate,
        &[(b"esds", mux::esds(0x40, &asc, kbps * 1000))],
    );
    t.mkv_id = "A_AAC";
    t.private = asc;
    Ok(t)
}

fn second_track() -> Result<mux::Track, String> {
    let pcm: Vec<f32> = (0..9600)
        .flat_map(|i| {
            let v = 0.25 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
            [v, v]
        })
        .collect();
    aac_track(&pcm, 48_000, 2, 64)
}

fn ac3_defs() -> Vec<Def> {
    let area = ac3::AREA;
    let mut defs = Vec::new();
    for c in ac3::cases() {
        let what = format!(
            "src/ac3/refs.rs writer, {} kbps, acmod {}, LFE {}, coupling {}, short blocks {}, dither flags set",
            c.kbps, c.acmod, c.lfe, c.coupling, c.short
        );
        defs.push(fixture(area, c.stream(), &what, move |_| {
            Ok(ac3::stream(c, true)?.0)
        }));
        defs.push(Def::ours(
            area,
            c.plain(),
            &format!(
                "src/ac3/refs.rs plain({}): the stream with its dither flags cleared",
                c.name
            ),
            move || ac3::plain(c),
        ));
        defs.extend(a52dec(c));
    }
    for name in ac3::BOXED {
        let c = ac3::named(name);
        for ext in ac3::BOX_EXTS {
            let what = format!(
                "our {ext} writer: {} and a second AAC track from our encoder",
                c.plain()
            );
            defs.push(fixture(area, ac3::boxed(name, ext), &what, move |dir| {
                let (code, size) = ac3::frame_bytes(c.rate, c.kbps).ok_or("no frame size")?;
                let plain = read(dir, &c.plain())?;
                let mut t = mux::Track::new(c.rate, c.channels(), ac3_frames(&plain, size));
                t.format = *b"ac-3";
                let dac3 = (u32::from(c.fscod()) << 22)
                    | (8 << 17)
                    | (u32::from(c.acmod) << 11)
                    | (u32::from(c.lfe) << 10)
                    | (u32::from(code >> 1) << 5);
                let dac3 = dac3.to_be_bytes()[1..].to_vec();
                t.entry = mux::sound_entry(c.channels(), 16, c.rate, &[(b"dac3", dac3)]);
                t.mkv_id = "A_AC3";
                mux::by_ext(ext, &[t, second_track()?])
            }));
        }
    }
    defs
}

fn alac_defs() -> Vec<Def> {
    let mut defs = Vec::new();
    for f in &alac::FIXTURES {
        let what = format!("src/alac/refs.rs writer, {:?}, {:?}", f.opts, f.entry);
        defs.push(fixture(alac::AREA, f.name.to_string(), &what, move |_| {
            let packets = alac::encode(&f.source(), f.channels, f.depth, &f.opts);
            let cookie = alac::cookie(f.channels, f.depth, f.rate, alac::max_frame(&packets));
            let n = f.frames;
            let timed: Packets = packets
                .into_iter()
                .enumerate()
                .map(|(k, p)| (p, (n - k * alac::FRAME).min(alac::FRAME) as u32))
                .collect();
            let mut t = mux::Track::new(f.rate, f.channels as u16, timed);
            t.bits = f.depth as u16;
            t.format = *b"alac";
            t.entry = alac::sample_entry(f.entry, f.channels, f.depth, f.rate, &cookie);
            t.mkv_id = "A_ALAC";
            t.private = cookie;
            let ext = f.name.rsplit('.').next().unwrap_or_default();
            mux::by_ext(ext, &[t])
        }));
    }
    defs
}
