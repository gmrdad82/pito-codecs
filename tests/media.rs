#![cfg(all(
    feature = "media",
    feature = "h264",
    feature = "hevc",
    feature = "aac",
    feature = "mp4",
    feature = "gpu",
    feature = "filter",
    feature = "mkv",
    feature = "vp9",
    feature = "opus"
))]

#[path = "../src/testref.rs"]
mod testref;

#[path = "../src/media/refs.rs"]
mod refs;

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use pito_codecs::media::{
    AudioBuffer, Bitstream, Canvas, Choice, Chroma, Codec, ExportOptions, Frame, IN_FLIGHT, Piece,
    PixelFormat, Prefs, Progress, Rate, Rgba, Seek, Stills, TimeBase, VideoFrame, audio_mix,
    audio_mix_stream, audio_pcm, audio_pcm_range, audio_stream, decode_video, decode_video_gpu,
    export, export_frames, export_with_audio, first_frame, frame_count, frames_at, frames_at_seek,
    keyframes, probe,
};
use pito_codecs::{h264, mkv, mp4};
use refs::AREA;
use testref::{AudioCodec, Clip, Digest, Picture, Refs, Sound, VideoCodec};

fn dir() -> PathBuf {
    testref::scratch(AREA)
}

fn fixture(name: &str) -> PathBuf {
    Refs::open(AREA).path(name)
}

fn sine_440(i: u64, _: u16, rate: u32) -> f32 {
    (0.125 * (std::f64::consts::TAU * 440.0 * i as f64 / f64::from(rate)).sin()) as f32
}

fn s16(rate: u32, frames: usize, left: Sound, right: Sound) -> Vec<i16> {
    let f: Vec<f32> = (0..frames as u64)
        .flat_map(|i| [left.at(i, 0, rate), right.at(i, 0, rate)])
        .collect();
    testref::s16_of(&f)
}

fn le(samples: &[i16]) -> Vec<u8> {
    samples.iter().flat_map(|s| s.to_le_bytes()).collect()
}

const CHUNK: usize = 2048;

enum Item {
    Copy(usize, usize, usize),
    Pcm(usize, usize),
}

fn pcm_items(pcm: &[Vec<i16>], rate: u32, items: &mut Vec<(f64, Item)>) {
    for (k, track) in pcm.iter().enumerate() {
        for start in (0..track.len() / 2).step_by(CHUNK) {
            items.push((start as f64 / f64::from(rate), Item::Pcm(k, start)));
        }
    }
}

fn chunk(track: &[i16], start: usize) -> (Vec<u8>, usize) {
    let end = (start + CHUNK).min(track.len() / 2);
    (le(&track[start * 2..end * 2]), end - start)
}

fn sowt(rate: u32) -> mp4::SampleEntry {
    let mut raw = vec![0u8; 6];
    raw.extend(1u16.to_be_bytes());
    raw.extend([0u8; 8]);
    raw.extend(2u16.to_be_bytes());
    raw.extend(16u16.to_be_bytes());
    raw.extend([0u8; 4]);
    raw.extend((rate << 16).to_be_bytes());
    mp4::SampleEntry {
        format: mp4::FourCc(*b"sowt"),
        codec: mp4::CodecConfig::Pcm(mp4::PcmConfig {
            bits: 16,
            float: false,
            big_endian: false,
            signed: true,
            channels: 2,
            sample_rate: f64::from(rate),
        }),
        video: None,
        audio: Some(mp4::AudioParams {
            channels: 2,
            sample_size: 16,
            sample_rate: f64::from(rate),
            ..Default::default()
        }),
        bitrate: None,
        raw,
    }
}

const PCM_RATE: u32 = 44_100;
const PCM_FRAMES: usize = 256;

#[derive(Clone, Copy)]
enum Layout {
    Mkv(&'static str),
    Sowt,
    Twos,
    In24,
    Fl32,
    Fl64,
    Lpcm,
    Ipcm,
    Fpcm,
}

#[derive(Clone, Copy)]
struct PcmCase {
    file: &'static str,
    layout: Layout,
    bits: u16,
    float: bool,
    big: bool,
}

const fn case(file: &'static str, layout: Layout, bits: u16, float: bool, big: bool) -> PcmCase {
    PcmCase {
        file,
        layout,
        bits,
        float,
        big,
    }
}

const PCM_CASES: [PcmCase; 14] = [
    case(
        "pcm-s24le.mkv",
        Layout::Mkv("A_PCM/INT/LIT"),
        24,
        false,
        false,
    ),
    case(
        "pcm-s32be.mkv",
        Layout::Mkv("A_PCM/INT/BIG"),
        32,
        false,
        true,
    ),
    case(
        "pcm-f32le.mkv",
        Layout::Mkv("A_PCM/FLOAT/IEEE"),
        32,
        true,
        false,
    ),
    case(
        "pcm-f64le.mkv",
        Layout::Mkv("A_PCM/FLOAT/IEEE"),
        64,
        true,
        false,
    ),
    case("pcm-u8.mkv", Layout::Mkv("A_PCM/INT/LIT"), 8, false, false),
    case("pcm-s16le.mov", Layout::Sowt, 16, false, false),
    case("pcm-s16be.mov", Layout::Twos, 16, false, true),
    case("pcm-s24le.mov", Layout::In24, 24, false, false),
    case("pcm-s32le.mov", Layout::Lpcm, 32, false, false),
    case("pcm-f32be.mov", Layout::Fl32, 32, true, true),
    case("pcm-f64le.mov", Layout::Fl64, 64, true, false),
    case("pcm-s16le.mp4", Layout::Ipcm, 16, false, false),
    case("pcm-s24be.mp4", Layout::Ipcm, 24, false, true),
    case("pcm-f32le.mp4", Layout::Fpcm, 32, true, false),
];

fn pcm_tone(i: u64, ch: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    if ch == 0 {
        (0.5 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
    } else {
        Sound::Noise { amp: 0.5, seed: 7 }.at(i, 0, rate)
    }
}

impl PcmCase {
    fn unsigned(&self) -> bool {
        self.bits == 8 && !self.float
    }

    fn stored(&self, x: f32) -> (Vec<u8>, f32) {
        let x = f64::from(x);
        if self.float {
            let (le, be, back) = if self.bits == 64 {
                (x.to_le_bytes().to_vec(), x.to_be_bytes().to_vec(), x as f32)
            } else {
                let v = x as f32;
                (v.to_le_bytes().to_vec(), v.to_be_bytes().to_vec(), v)
            };
            return (if self.big { be } else { le }, back);
        }
        if self.unsigned() {
            let v = ((x * 128.0).round() + 128.0).clamp(0.0, 255.0) as u8;
            return (vec![v], (f32::from(v) - 128.0) / 128.0);
        }
        let scale = (1i64 << (self.bits - 1)) as f64;
        let v = (x * scale).round().clamp(-scale, scale - 1.0) as i64;
        let back = v as f32 / scale as f32;
        let n = usize::from(self.bits / 8);
        let be = (v as i32).to_be_bytes();
        let bytes = be[4 - n..].to_vec();
        let bytes = if self.big {
            bytes
        } else {
            bytes.into_iter().rev().collect()
        };
        (bytes, back)
    }

    fn samples(&self) -> (Vec<u8>, Vec<f32>) {
        let source = testref::pcm(Sound::Fn(pcm_tone), PCM_RATE, 2, 0, PCM_FRAMES);
        let mut data = Vec::new();
        let mut want = Vec::new();
        for s in source {
            let (b, back) = self.stored(s);
            data.extend(b);
            want.push(back);
        }
        (data, want)
    }

    fn entry(&self) -> mp4::SampleEntry {
        let (format, version) = match self.layout {
            Layout::Sowt => (*b"sowt", 0),
            Layout::Twos => (*b"twos", 0),
            Layout::In24 => (*b"in24", 1),
            Layout::Fl32 => (*b"fl32", 1),
            Layout::Fl64 => (*b"fl64", 1),
            Layout::Lpcm => (*b"lpcm", 2),
            Layout::Ipcm => (*b"ipcm", 0),
            Layout::Fpcm => (*b"fpcm", 0),
            Layout::Mkv(_) => unreachable!("an mkv case has no sample entry"),
        };
        let bytes = u32::from(self.bits / 8) * 2;
        let mut raw = vec![0u8; 6];
        raw.extend(1u16.to_be_bytes());
        raw.extend((version as u16).to_be_bytes());
        raw.extend([0u8; 6]);
        if version == 2 {
            raw.extend(3u16.to_be_bytes());
            raw.extend(16u16.to_be_bytes());
            raw.extend((-2i16).to_be_bytes());
            raw.extend(0u16.to_be_bytes());
            raw.extend(0x0001_0000u32.to_be_bytes());
            raw.extend(72u32.to_be_bytes());
            raw.extend(f64::from(PCM_RATE).to_be_bytes());
            raw.extend(2u32.to_be_bytes());
            raw.extend(0x7F00_0000u32.to_be_bytes());
            raw.extend(u32::from(self.bits).to_be_bytes());
            let flags =
                u32::from(self.float) | u32::from(self.big) << 1 | u32::from(!self.float) << 2;
            raw.extend(flags.to_be_bytes());
            raw.extend(bytes.to_be_bytes());
            raw.extend(1u32.to_be_bytes());
        } else {
            raw.extend(2u16.to_be_bytes());
            raw.extend(self.bits.to_be_bytes());
            raw.extend([0u8; 4]);
            raw.extend((PCM_RATE << 16).to_be_bytes());
            if version == 1 {
                raw.extend(1u32.to_be_bytes());
                raw.extend((bytes / 2).to_be_bytes());
                raw.extend(bytes.to_be_bytes());
                raw.extend((bytes / 2).to_be_bytes());
            }
        }
        let leaf = |kind: &[u8; 4], body: &[u8]| {
            let mut b = (8 + body.len() as u32).to_be_bytes().to_vec();
            b.extend_from_slice(kind);
            b.extend_from_slice(body);
            b
        };
        match self.layout {
            Layout::In24 | Layout::Fl64 => {
                let wave = [
                    leaf(b"frma", &format),
                    leaf(b"enda", &u16::from(!self.big).to_be_bytes()),
                ]
                .concat();
                raw.extend(leaf(b"wave", &wave));
            }
            Layout::Ipcm | Layout::Fpcm => {
                let body = [0, 0, 0, 0, u8::from(!self.big), self.bits as u8];
                raw.extend(leaf(b"pcmC", &body));
            }
            _ => {}
        }
        mp4::SampleEntry {
            format: mp4::FourCc(format),
            codec: mp4::CodecConfig::Pcm(mp4::PcmConfig {
                bits: self.bits,
                float: self.float,
                big_endian: self.big,
                signed: !self.unsigned(),
                channels: 2,
                sample_rate: f64::from(PCM_RATE),
            }),
            video: None,
            audio: Some(mp4::AudioParams {
                channels: 2,
                sample_size: self.bits,
                sample_rate: f64::from(PCM_RATE),
                ..Default::default()
            }),
            bitrate: None,
            raw,
        }
    }

    fn write(&self) -> (PathBuf, Vec<f32>) {
        let (data, want) = self.samples();
        let bytes = match self.layout {
            Layout::Mkv(codec) => {
                let track = refs::MkvTrack {
                    codec,
                    rate: f64::from(PCM_RATE),
                    channels: 2,
                    bits: u64::from(self.bits),
                    ..refs::MkvTrack::default()
                };
                let block = refs::MkvBlock {
                    track: 0,
                    ms: 0,
                    key: true,
                    data,
                    discard_ns: 0,
                };
                let ms = PCM_FRAMES as f64 * 1000.0 / f64::from(PCM_RATE);
                refs::matroska("matroska", &[track], &[block], ms, refs::Sizes::Known)
            }
            _ => {
                let brand = if self.file.ends_with(".mov") {
                    mp4::Brand::Mov
                } else {
                    mp4::Brand::Mp4
                };
                let options = mp4::Options { brand, reserve: 0 };
                let mut w = mp4::Writer::new(Cursor::new(Vec::new()), options).unwrap();
                let t = w
                    .add_track(mp4::TrackConfig::new(self.entry(), PCM_RATE))
                    .unwrap();
                w.push_sample(t, &data, 0, 0, PCM_FRAMES as u32, true)
                    .unwrap();
                w.finish_faststart().unwrap().into_inner()
            }
        };
        let out = dir().join(self.file);
        testref::write_atomic(&out, &bytes).unwrap();
        (out, want)
    }
}

fn rebuild_mp4(
    srcs: &[&Path],
    name: &str,
    pcm: &[Vec<i16>],
    rate: u32,
    edit: impl Fn(&mp4::Reader, usize, usize, &mut mp4::TrackConfig),
) -> PathBuf {
    let readers: Vec<mp4::Reader> = srcs.iter().map(|s| mp4::Reader::open(s).unwrap()).collect();
    let mut w = mp4::Writer::new(Cursor::new(Vec::new()), mp4::Options::default()).unwrap();
    w.set_movie_timescale(readers[0].timescale);
    let mut items = Vec::new();
    let mut tracks = Vec::new();
    for (n, r) in readers.iter().enumerate() {
        let mut mine = Vec::new();
        for (i, t) in r.tracks.iter().enumerate() {
            let mut cfg = mp4::TrackConfig::from_track(t).unwrap();
            cfg.media_start = t
                .edits
                .iter()
                .find(|e| e.media_time >= 0)
                .map(|e| e.media_time);
            edit(r, n, i, &mut cfg);
            mine.push(w.add_track(cfg).unwrap());
            for (k, s) in t.samples.iter().enumerate() {
                items.push((t.seconds(s.dts), Item::Copy(n, i, k)));
            }
        }
        tracks.push(mine);
    }
    let lanes: Vec<usize> = pcm
        .iter()
        .map(|_| {
            w.add_track(mp4::TrackConfig::new(sowt(rate), rate))
                .unwrap()
        })
        .collect();
    pcm_items(pcm, rate, &mut items);
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, item) in items {
        match item {
            Item::Copy(n, i, k) => {
                let r = &readers[n];
                let s = r.tracks[i].samples[k];
                let data = r.read(i, k).unwrap();
                w.push_sample(tracks[n][i], &data, s.pts, s.dts, s.duration, s.keyframe)
                    .unwrap();
            }
            Item::Pcm(k, start) => {
                let (data, frames) = chunk(&pcm[k], start);
                let at = start as i64;
                w.push_sample(lanes[k], &data, at, at, frames as u32, true)
                    .unwrap();
            }
        }
    }
    let out = dir().join(name);
    let bytes = w.finish_faststart().unwrap().into_inner();
    testref::write_atomic(&out, &bytes).unwrap();
    out
}

fn rebuild_mkv(src: &Path, name: &str, pcm: &[Vec<i16>], rate: u32, late: [f64; 2]) -> PathBuf {
    let r = mkv::Reader::open(src).unwrap();
    let options = if name.ends_with(".webm") {
        mkv::Options::webm()
    } else {
        mkv::Options::default()
    };
    let mut w = mkv::Writer::new(Cursor::new(Vec::new()), options);
    let mut items = Vec::new();
    let mut tracks = Vec::new();
    for (i, t) in r.tracks.iter().enumerate() {
        tracks.push(w.add_track(mkv::TrackConfig::from_track(t)).unwrap());
        let video = t.kind == mkv::TrackKind::Video;
        let shift = if video { late[0] } else { late[1] };
        let fps = t.fps().max(1.0);
        for (k, s) in t.samples.iter().enumerate() {
            let at = if video {
                k as f64 / fps
            } else {
                t.seconds(s.pts)
            };
            items.push((at + shift, Item::Copy(0, i, k)));
        }
    }
    let lanes: Vec<usize> = pcm
        .iter()
        .map(|_| {
            let cfg = mkv::TrackConfig {
                sample_rate: rate,
                channels: 2,
                ..mkv::TrackConfig::new(
                    mkv::TrackKind::Audio,
                    "A_PCM/INT/LIT",
                    TimeBase::new(1, rate),
                )
            };
            w.add_track(cfg).unwrap()
        })
        .collect();
    pcm_items(pcm, rate, &mut items);
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, item) in items {
        match item {
            Item::Copy(_, i, k) => {
                let t = &r.tracks[i];
                let s = t.samples[k];
                let shift = if t.kind == mkv::TrackKind::Video {
                    late[0]
                } else {
                    late[1]
                };
                let data = r.read(i, k).unwrap();
                let duration = (s.duration > 0).then_some(s.duration);
                w.push_sample(
                    tracks[i],
                    &data,
                    s.pts + t.ticks(shift),
                    duration,
                    s.keyframe,
                )
                .unwrap();
            }
            Item::Pcm(k, start) => {
                let (data, frames) = chunk(&pcm[k], start);
                w.push_sample(lanes[k], &data, start as i64, Some(frames as u32), true)
                    .unwrap();
            }
        }
    }
    let out = dir().join(name);
    let bytes = w.finish().unwrap().into_inner();
    testref::write_atomic(&out, &bytes).unwrap();
    out
}

fn with_pcm(video: Clip, name: &str, pcm: &[Vec<i16>], rate: u32) -> PathBuf {
    let src = video.video_only().make(AREA, &format!("video-{name}"));
    if name.ends_with(".mp4") {
        rebuild_mp4(&[&src], name, pcm, rate, |_, _, _, _| {})
    } else {
        rebuild_mkv(&src, name, pcm, rate, [0.0; 2])
    }
}

fn clip() -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        let shape = || Clip::new((1280, 720), (60, 1), 360);
        let game = Clip {
            b_frames: 2,
            sounds: vec![Sound::Sine {
                hz: 440.0,
                amp: 0.25,
            }],
            ..shape()
        }
        .make(AREA, "two-tracks-game.mp4");
        let mic = Clip {
            sounds: vec![Sound::Sine {
                hz: 880.0,
                amp: 0.25,
            }],
            rate: 44_100,
            channels: 1,
            ..shape()
        }
        .audio_only()
        .make(AREA, "two-tracks-mic.m4a");
        rebuild_mp4(
            &[&game, &mic],
            "two-tracks-bt709.mp4",
            &[],
            0,
            |r, n, i, cfg| {
                if r.audio().contains(&i) {
                    cfg.title = Some(["Game", "Mic"][n].into());
                }
            },
        )
    })
}

fn route_prefs(decode: Choice) -> Prefs {
    Prefs {
        decode,
        ..Prefs::default()
    }
}

fn frames_from(path: &Path, from: i64) -> impl Iterator<Item = Frame> {
    let cpu = route_prefs(Choice::Cpu);
    let p = probe(path, &cpu).unwrap();
    let at = (from - 1).max(0) as f64 * p.fps.1 as f64 / p.fps.0.max(1) as f64;
    decode_video(path, at, None, &cpu)
        .unwrap()
        .map(Result::unwrap)
        .skip_while(move |f| f.pts < from)
}

fn hash(f: &Frame) -> String {
    let mut d = Digest::new();
    d.frame(f);
    d.hex()
}

fn decodes_clean(path: &Path) -> bool {
    let cpu = route_prefs(Choice::Cpu);
    let (Ok(p), Ok(video), Ok(count)) = (
        probe(path, &cpu),
        decode_video(path, 0.0, None, &cpu),
        frame_count(path, &cpu),
    ) else {
        return false;
    };
    if let Ok(r) = mp4::Reader::open(path) {
        for t in &r.tracks {
            let mut pts: Vec<i64> = t.samples.iter().map(|s| s.pts).collect();
            pts.sort_unstable();
            let rising = t.samples.windows(2).all(|w| w[1].dts > w[0].dts);
            let timed = t.samples.iter().all(|s| s.duration > 0);
            if !rising || !timed || pts.windows(2).any(|w| w[0] == w[1]) {
                return false;
            }
        }
    }
    let mut n = 0u64;
    for f in video {
        if f.is_err() {
            return false;
        }
        n += 1;
    }
    n == count && (0..p.audio.len()).all(|t| audio_pcm(path, t, 48_000, false, &cpu).is_ok())
}

fn lit(img: &Rgba) -> bool {
    img.data.chunks_exact(4).any(|p| p[0] > 40 || p[1] > 40)
}

fn min_psnr(out: &Path, src: &Path, ranges: &[(i64, i64)]) -> f64 {
    let mut ours = frames_from(out, 0);
    let mut worst = f64::INFINITY;
    for &(a, b) in ranges {
        let mut n = 0;
        for theirs in frames_from(src, a).take((b - a) as usize) {
            let o = ours
                .next()
                .unwrap_or_else(|| panic!("{} ends inside [{a}, {b})", out.display()));
            worst = worst.min(testref::psnr(&packed(&o), &packed(&theirs)));
            n += 1;
        }
        assert_eq!(n, b - a, "{} has no frames [{a}, {b})", src.display());
    }
    assert!(
        ours.next().is_none(),
        "{} has more frames than its pieces",
        out.display()
    );
    worst
}

fn routes() -> [(&'static str, Prefs); 1] {
    [("ours", Prefs::default())]
}

fn span(o: u32, len: u32, to: u32) -> (u32, u32) {
    let a = o * len / to;
    (a, ((o + 1) * len / to).max(a + 1))
}

fn shrink(size: (u32, u32), like: &Rgba, rgb: impl Fn(u32, u32) -> [u8; 3]) -> Rgba {
    let mut out = Rgba::new(like.width, like.height);
    for oy in 0..like.height {
        let (y0, y1) = span(oy, size.1, like.height);
        for ox in 0..like.width {
            let (x0, x1) = span(ox, size.0, like.width);
            let mut sum = [0u32; 3];
            for y in y0..y1 {
                for x in x0..x1 {
                    for (s, c) in sum.iter_mut().zip(rgb(x, y)) {
                        *s += u32::from(c);
                    }
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let i = ((oy * like.width + ox) * 4) as usize;
            for (c, s) in sum.iter().enumerate() {
                out.data[i + c] = ((s + n / 2) / n) as u8;
            }
            out.data[i + 3] = 255;
        }
    }
    out
}

fn source_still(size: (u32, u32), n: u64, like: &Rgba) -> Rgba {
    shrink(size, like, |x, y| testref::rgb_at(size.0, size.1, n, x, y))
}

#[test]
fn our_route_probes_shows_and_hears() {
    let prefs = Prefs::default();
    let p = probe(clip(), &prefs).unwrap();
    assert_eq!((p.width, p.height, p.fps), (1280, 720, (60, 1)));
    assert_eq!(p.video_codec, Some(Codec::H264));
    let titles: Vec<_> = p.audio.iter().map(|a| a.title.clone()).collect();
    assert_eq!(titles, [Some("Game".into()), Some("Mic".into())]);
    let ours = first_frame(clip(), 240, &prefs).unwrap();
    assert_eq!(ours.height, 240);
    assert!((426..=428).contains(&ours.width), "{}", ours.width);
    let theirs = source_still((1280, 720), 0, &ours);
    let diff = mean_diff(&ours, &theirs);
    assert!(diff < 6.0, "{diff}");
    let strip = frames_at(clip(), &[0.5, 2.0, 4.0, 99.0], 120, &prefs).unwrap();
    assert!(strip.iter().all(|s| s.height == 120 && lit(s)));
    let pcm = audio_pcm(clip(), 0, 16_000, true, &prefs).unwrap();
    assert!(
        (pcm.frames() as i64 - 96_000).abs() < 1_600,
        "{}",
        pcm.frames()
    );
    let rms = (pcm.samples.iter().map(|s| s * s).sum::<f32>() / pcm.samples.len() as f32).sqrt();
    assert!(rms > 0.05, "{rms}");
    let frames: Vec<_> = decode_video(clip(), 3.0, None, &prefs)
        .unwrap()
        .take(3)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(frames[0].pts, 180);
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn ranged_audio_lines_up_with_the_whole_track_and_mixes_on_every_route() {
    for (name, prefs) in routes() {
        for (track, channels, limit) in [(0usize, 2u16, 1e-3f32), (1, 1, 5e-3)] {
            let whole =
                audio_pcm_range(clip(), track, 0.0, f64::INFINITY, 48_000, channels, &prefs)
                    .unwrap();
            let ch = channels as usize;
            let mut joined = Vec::new();
            for (a, b, frames) in [(1.0, 1.5, 24_000), (1.5, 2.25, 36_000), (3.5, 4.5, 48_000)] {
                let part = audio_pcm_range(clip(), track, a, b, 48_000, channels, &prefs).unwrap();
                assert_eq!(
                    (part.frames(), part.channels),
                    (frames, channels),
                    "{name} {track}"
                );
                joined.extend(part.samples);
            }
            let slice = [
                &whole.samples[48_000 * ch..108_000 * ch],
                &whole.samples[168_000 * ch..216_000 * ch],
            ]
            .concat();
            let d = worst(&joined, &slice);
            println!("{name} track {track}: worst sample difference {d}");
            assert!(d < limit, "{name} track {track}: {d}");
        }
        let tail = audio_pcm_range(clip(), 0, 5.5, 9.0, 48_000, 2, &prefs).unwrap();
        let whole = audio_pcm(clip(), 0, 48_000, false, &prefs).unwrap();
        assert_eq!(tail.frames(), whole.frames() - 264_000, "{name}");

        let game = audio_pcm_range(clip(), 0, 2.0, 3.0, 44_100, 2, &prefs).unwrap();
        let alone = audio_mix(clip(), &[1.0, 0.0], 2.0, 3.0, 44_100, &prefs).unwrap();
        assert_eq!(alone.samples, game.samples, "{name}");
        let both: AudioBuffer = audio_mix(clip(), &[0.5, 2.0], 2.0, 3.0, 44_100, &prefs).unwrap();
        assert_eq!(
            (both.frames(), both.channels, both.rate),
            (44_100, 2, 44_100)
        );
        let rms = |b: &[f32]| (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).sqrt();
        assert!(rms(&both.samples) > 2.0 * rms(&game.samples), "{name}");
    }
}

#[test]
fn a_three_piece_export_with_a_repeat_smart_cuts_to_the_frame_on_every_route() {
    for (name, prefs) in routes() {
        let out = dir().join(format!("smart-{name}.mp4"));
        let pieces = [(1.1, 3.4), (0.5, 1.5), (1.1, 3.4)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
            from,
            to,
        });
        let mut stages = 0;
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |_| {
                stages += 1;
                true
            },
        )
        .unwrap();
        assert!(done.smart && stages > 2, "{name}");
        assert_eq!(done.frames, 336, "{name}");
        assert!(done.copied >= 180, "{name}: {}", done.copied);
        assert!(decodes_clean(&out), "{name}");
        let psnr = min_psnr(&out, clip(), &[(66, 204), (30, 90), (66, 204)]);
        assert!(psnr > 40.0, "{name}: {psnr}");
        let audio = audio_pcm(&out, 0, 48_000, false, &Prefs::default())
            .unwrap()
            .frames() as f64
            / 48_000.0;
        assert!((audio - 5.6).abs() < 0.05, "{name}: {audio}");
        header_durations_match_the_samples(&out, name);
    }
}

fn children(data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let (head, size) = if size == 1 {
            (
                16,
                u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()) as usize,
            )
        } else {
            (8, size)
        };
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        out.push((kind, &data[at + head..at + size]));
        at += size;
    }
    out
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    children(data)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, d)| d)
}

fn be(d: &[u8], at: usize, wide: bool) -> u64 {
    if wide {
        u64::from_be_bytes(d[at..at + 8].try_into().unwrap())
    } else {
        u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as u64
    }
}

fn times(d: &[u8]) -> (u64, u64) {
    let v1 = d[0] == 1;
    let at = if v1 { 20 } else { 12 };
    (be(d, at, false), be(d, at + 4, v1))
}

fn header_durations_match_the_samples(path: &Path, name: &str) {
    let file = std::fs::read(path).unwrap();
    let moov = child(&file, b"moov").unwrap();
    let (movie_ts, movie) = times(child(moov, b"mvhd").unwrap());
    let mut longest = 0.0f64;
    for (kind, trak) in children(moov) {
        if &kind != b"trak" {
            continue;
        }
        let tkhd = child(trak, b"tkhd").unwrap();
        let track =
            be(tkhd, if tkhd[0] == 1 { 28 } else { 20 }, tkhd[0] == 1) as f64 / movie_ts as f64;
        let mdia = child(trak, b"mdia").unwrap();
        let (ts, media) = times(child(mdia, b"mdhd").unwrap());
        let video = &child(mdia, b"hdlr").unwrap()[8..12] == b"vide";
        let stbl = child(child(mdia, b"minf").unwrap(), b"stbl").unwrap();
        let stts = child(stbl, b"stts").unwrap();
        let runs: Vec<(u64, u64)> = (0..be(stts, 4, false) as usize)
            .map(|i| (be(stts, 8 + i * 8, false), be(stts, 12 + i * 8, false)))
            .collect();
        let frames: u64 = runs.iter().map(|r| r.0).sum();
        let ticks: u64 = runs.iter().map(|r| r.0 * r.1).sum();
        let step = runs.iter().max_by_key(|r| r.0).unwrap().1;
        let start = child(trak, b"edts")
            .and_then(|e| child(e, b"elst"))
            .map(|l| {
                let v1 = l[0] == 1;
                (be(l, 8, v1), be(l, if v1 { 16 } else { 12 }, v1))
            });
        let media_time = start.map_or(0, |s| s.1);
        let shown = if video {
            frames * step
        } else {
            ticks - media_time
        };
        let kind = if video { "video" } else { "audio" };
        let expected = if video { shown } else { shown + media_time };
        assert_eq!(media, expected, "{name}: mdhd of a {kind} track");
        assert_eq!(ticks, media, "{name}: stts of a {kind} track");
        let shown = shown as f64 / ts as f64;
        if let Some((segment, _)) = start {
            let segment = segment as f64 / movie_ts as f64;
            assert!(
                (segment - shown).abs() < 1e-3,
                "{name}: elst of a {kind} track {segment} against {shown}"
            );
        }
        assert!(
            (track - shown).abs() < 1e-3,
            "{name}: tkhd of a {kind} track {track} against {shown}"
        );
        longest = longest.max(track);
    }
    let movie = movie as f64 / movie_ts as f64;
    assert!(
        (movie - longest).abs() < 1e-3,
        "{name}: mvhd {movie} against {longest}"
    );
}

#[test]
fn an_export_ending_on_a_copied_gop_lasts_to_its_last_frame_in_the_headers() {
    for (name, prefs) in routes() {
        let out = dir().join(format!("headers-{name}.mp4"));
        let pieces = [(1.1, 2.0), (2.5, 4.0)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
            from,
            to,
        });
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(done.frames, 144, "{name}");
        header_durations_match_the_samples(&out, name);
    }
}

fn steady(told: &[f32], name: &str) {
    assert_eq!(told.last(), Some(&1.0), "{name}");
    let mut last = 0.0f32;
    for &d in told {
        assert!(d + 1e-6 >= last, "{name}: progress fell from {last} to {d}");
        assert!(
            d - last <= 0.06,
            "{name}: progress jumped from {last} to {d}"
        );
        last = d;
    }
}

fn picture(n: u64, size: (u32, u32), spent: Option<Frame>) -> Frame {
    let mut f = spent.unwrap_or_else(|| {
        Frame::new(
            PixelFormat::Yuv420p8,
            size.0,
            size.1,
            0,
            TimeBase::new(1, 30),
        )
    });
    let (w, h) = (f.width as usize, f.height as usize);
    let stride = f.planes[0].stride;
    for y in 0..h {
        for (x, px) in f.planes[0].data[y * stride..y * stride + w]
            .iter_mut()
            .enumerate()
        {
            *px = (16 + (x + y + n as usize * 3) % 200) as u8;
        }
    }
    for p in &mut f.planes[1..] {
        p.data.fill(128);
    }
    f
}

#[test]
fn caller_made_frames_export_at_their_size_with_sample_exact_audio() {
    let size = (360, 640);
    let fps = (30_000, 1_001);
    let count = 299u64;
    let total = 478_878usize;
    let out = dir().join("frames-360x640.mp4");
    let mut asked = Vec::new();
    let mut told = Vec::new();
    let mut at = 0usize;
    let canvas = Canvas {
        width: size.0,
        height: size.1,
        fps,
        frames: Some(count),
    };
    let done = export_frames(
        &out,
        canvas,
        ExportOptions::default(),
        |n, spent| Ok((n < count).then(|| VideoFrame::Cpu(picture(n, size, spent)))),
        |upto, frames| {
            asked.push((upto, frames));
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = (at..at + frames).flat_map(|k| [tone(k), tone(k)]).collect();
            at += frames;
            Ok(buf)
        },
        |p: &Progress| {
            told.push(p.done);
            true
        },
    )
    .unwrap();
    assert_eq!(done.frames, count);
    steady(&told, "frames");
    assert_eq!(asked.last().map(|a| a.0), Some(count));
    assert_eq!(asked.iter().map(|a| a.1).sum::<usize>(), total);
    assert!(asked.windows(2).all(|w| w[0].0 < w[1].0));
    let back = probe(&out, &Prefs::default()).unwrap();
    assert_eq!((back.width, back.height), size);
    assert_eq!(frame_count(&out, &Prefs::default()).unwrap(), count);
    assert!(decodes_clean(&out));
    let pcm = audio_pcm(&out, 0, 48_000, false, &Prefs::default())
        .unwrap()
        .samples;
    assert_eq!(pcm.len(), total * 2);
    let want: Vec<f32> = (0..total).flat_map(|k| [tone(k), tone(k)]).collect();
    let span = 96_000 * 2..100_096 * 2;
    assert!(worst(&pcm[span.clone()], &want[span]) < 0.01);
    header_durations_match_the_samples(&out, "frames");
    let wrong = export_frames(
        &out,
        canvas,
        ExportOptions::default(),
        |n, _| {
            let s = if n == 7 { (352, 240) } else { size };
            Ok((n < 20).then(|| VideoFrame::Cpu(picture(n, s, None))))
        },
        |_, frames| {
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = vec![0.0; frames * 2];
            Ok(buf)
        },
        |_| true,
    )
    .unwrap_err();
    assert!(wrong.to_string().contains("frame 7"), "{wrong}");
}

fn tone(k: usize) -> f32 {
    (k as f64 * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.5
}

#[test]
fn a_premixed_export_keeps_its_tone_whole_and_sample_exact_on_every_route() {
    let pieces = [(1.1, 3.4), (0.5, 1.5), (4.0, 4.52)].map(|(from, to)| Piece {
        path: clip().to_path_buf(),
        from,
        to,
    });
    let total = 229 * 800;
    for (name, prefs) in routes() {
        let out = dir().join(format!("premixed-{name}.mp4"));
        let mut asked = Vec::new();
        let mut at = 0usize;
        let done = export_with_audio(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |piece, frames| {
                asked.push((piece, frames));
                let mut buf = AudioBuffer::new(48_000, 2);
                buf.samples = (at..at + frames).flat_map(|k| [tone(k), tone(k)]).collect();
                at += frames;
                Ok(buf)
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(done.frames, 229, "{name}");
        assert_eq!(asked, [(0, 110_400), (1, 48_000), (2, 24_800)], "{name}");
        assert!(decodes_clean(&out), "{name}");
        let ours =
            audio_pcm_range(&out, 0, 0.0, f64::INFINITY, 48_000, 2, &Prefs::default()).unwrap();
        assert_eq!(ours.frames(), total, "{name}");
        let pcm = ours.samples;
        let want: Vec<f32> = (0..total).flat_map(|k| [tone(k), tone(k)]).collect();
        for cut in [110_400usize, 158_400] {
            let span = (cut - 2_048) * 2..(cut + 2_048) * 2;
            let d = worst(&pcm[span.clone()], &want[span]);
            println!("{name}: worst sample difference around {cut}: {d}");
            assert!(d < 0.01, "{name} at {cut}: {d}");
        }
    }
    let bad = |frames: usize| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = vec![0.0; frames * 2];
        buf
    };
    let out = dir().join("premixed-bad.mp4");
    let short = export_with_audio(
        &out,
        &pieces,
        ExportOptions::default(),
        |piece, frames| Ok(bad(if piece == 1 { frames - 1 } else { frames })),
        |_| true,
    )
    .unwrap_err();
    assert!(short.to_string().contains("piece 1"), "{short}");
    let nan = export_with_audio(
        &out,
        &pieces,
        ExportOptions::default(),
        |piece, frames| {
            let mut buf = bad(frames);
            if piece == 2 {
                buf.samples[7] = f32::NAN;
            }
            Ok(buf)
        },
        |_| true,
    )
    .unwrap_err();
    assert!(nan.to_string().contains("piece 2"), "{nan}");
}

#[test]
fn an_export_with_an_overlay_re_encodes_and_paints_every_frame_on_every_route() {
    for (name, prefs) in routes() {
        let out = dir().join(format!("overlay-{name}.mp4"));
        let pieces = [(0.0, 1.0), (4.0, 4.5)].map(|(from, to)| Piece {
            path: clip().to_path_buf(),
            from,
            to,
        });
        let red = Arc::new({
            let mut l = Rgba::new(1280, 720);
            for px in l.data.chunks_exact_mut(4).take(1280 * 100) {
                px.copy_from_slice(&[255, 0, 0, 255]);
            }
            l
        });
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                video: Codec::Hevc,
                prefs,
                overlay: Some(Box::new(move |_, _, _| Some(red.clone()))),
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert!(!done.smart && done.encoded == 90, "{name}");
        let codec = probe(&out, &Prefs::default()).unwrap().video_codec;
        if codec == Some(Codec::H264) {
            assert!(
                done.reasons.iter().any(|r| r.contains("instead")),
                "{name}: {:?}",
                done.reasons
            );
        } else {
            assert_eq!(codec, Some(Codec::Hevc), "{name}");
        }
        let top = first_frame(&out, 720, &route_prefs(Choice::Cpu)).unwrap();
        assert_eq!((top.width, top.height), (1280, 720), "{name}");
        let px = &top.data[(50 * 1280 + 640) * 4..(50 * 1280 + 640) * 4 + 3];
        assert!(px[0] > 220 && px[1] < 40 && px[2] < 40, "{name}: {px:?}");
        assert!(decodes_clean(&out), "{name}");
        assert!(min_psnr(&out, clip(), &[(0, 60), (240, 270)]) > 0.0);
    }
}

#[test]
fn adoption_through_every_available_route() {
    let sample = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/sample/adoption.mp4");
    if !sample.exists() {
        eprintln!("no tmp/sample/adoption.mp4: skipped");
        return;
    }
    for (name, prefs) in routes() {
        let p = probe(&sample, &prefs).unwrap();
        assert_eq!((p.width, p.height, p.fps), (2560, 1440, (60, 1)));
        assert_eq!(p.audio[0].title.as_deref(), Some("Gameplay"));
        assert!(lit(&first_frame(&sample, 240, &prefs).unwrap()));
        let out = dir().join(format!("adoption-{name}.mp4"));
        let pieces = [(4.0, 4.5), (8.0, 8.4), (4.0, 4.5)].map(|(from, to)| Piece {
            path: sample.clone(),
            from,
            to,
        });
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(done.frames, 30 + 24 + 30, "{name}");
        assert!(decodes_clean(&out), "{name}");
        let psnr = min_psnr(&out, &sample, &[(240, 270), (480, 504), (240, 270)]);
        assert!(psnr > 40.0, "{name}: {psnr}");
    }
}

fn luma_diff(a: &Frame, b: &Frame) -> f64 {
    let (pa, pb) = (&a.planes[0], &b.planes[0]);
    let mut sum = 0u64;
    for y in 0..pa.height {
        let ra = &pa.data[y * pa.stride..y * pa.stride + pa.width];
        let rb = &pb.data[y * pb.stride..y * pb.stride + pb.width];
        sum += ra
            .iter()
            .zip(rb)
            .map(|(x, y)| x.abs_diff(*y) as u64)
            .sum::<u64>();
    }
    sum as f64 / (pa.width * pa.height) as f64
}

#[test]
fn gpu_playback_keeps_frames_on_the_gpu_and_falls_back_to_the_cpu() {
    let gpu = pito_codecs::gpu::available()
        .iter()
        .any(|&(c, d, _)| c == Codec::H264 && d == pito_codecs::gpu::Direction::Decode);
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    for (prefs, on_gpu) in [(Prefs::default(), gpu), (cpu.clone(), false)] {
        let mut video = decode_video_gpu(clip(), 3.0, Some((640, 360)), &prefs).unwrap();
        assert_eq!(video.on_gpu(), on_gpu, "{:?}", video.reasons);
        assert_eq!((video.color.matrix, video.color.full_range), (1, false));
        let mut held = Vec::new();
        let mut n = 0;
        while let Some(f) = video.next() {
            let f = f.unwrap();
            assert_eq!(
                (f.on_gpu(), f.size(), f.pts()),
                (on_gpu && cfg!(target_os = "linux"), (640, 360), 180 + n)
            );
            #[cfg(target_os = "linux")]
            if let VideoFrame::Gpu(g) = &f {
                let d = g.dmabuf(false).unwrap();
                assert_eq!((d.fourcc, d.crop), (0x3231_564e, (0, 0, 640, 360)));
            }
            n += 1;
            held.push(f);
            if held.len() == IN_FLIGHT {
                for f in held.drain(..) {
                    video.recycle(f);
                }
            }
        }
        assert_eq!(n, 180);
    }
    if !gpu {
        return;
    }
    let ours = decode_video_gpu(clip(), 1.0, None, &Prefs::default())
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        ours.on_gpu(),
        cfg!(target_os = "linux"),
        "a native-size frame left the GPU"
    );
    let ours = match ours {
        VideoFrame::Gpu(g) => g.download().unwrap(),
        VideoFrame::Cpu(f) => f,
    };
    let cpu = decode_video(clip(), 1.0, None, &cpu)
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        (ours.width, ours.height, ours.pts),
        (cpu.width, cpu.height, cpu.pts)
    );
    let diff = luma_diff(&ours, &cpu);
    assert!(diff < 1.0, "{diff}");
}

fn matroska(name: &str, video: VideoCodec) -> PathBuf {
    Clip {
        video: Some(video),
        b_frames: if video == VideoCodec::H264 { 2 } else { 0 },
        ..Clip::new((640, 360), (30, 1), 90)
    }
    .video_only()
    .make(AREA, name)
}

#[test]
fn matroska_and_webm_play_through_the_media_front() {
    let files = [
        (matroska("h264.mkv", VideoCodec::H264), Codec::H264),
        (matroska("vp9.webm", VideoCodec::Vp9), Codec::Vp9),
    ];
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    for (path, codec) in files {
        let p = probe(&path, &cpu).unwrap();
        assert_eq!(
            (p.video_codec, p.width, p.height, p.fps),
            (Some(codec), 640, 360, (30, 1))
        );
        let gpu = pito_codecs::gpu::available().iter().any(|&(c, d, b)| {
            c == codec
                && d == pito_codecs::gpu::Direction::Decode
                && b == pito_codecs::gpu::Backend::VaApi
        });
        let mut firsts = Vec::new();
        for (prefs, on_gpu) in [(Prefs::default(), gpu), (cpu.clone(), false)] {
            let video = decode_video_gpu(&path, 1.0, None, &prefs).unwrap();
            let mut n = 0;
            for f in video {
                let f = f.unwrap();
                assert_eq!(
                    (f.on_gpu(), f.size(), f.pts()),
                    (on_gpu, (640, 360), 30 + n),
                    "{codec}"
                );
                if n == 0 {
                    firsts.push(match f {
                        VideoFrame::Gpu(g) => g.download().unwrap(),
                        VideoFrame::Cpu(c) => c,
                    });
                }
                n += 1;
            }
            assert_eq!(n, 60, "{codec}");
        }
        let diff = luma_diff(&firsts[0], &firsts[1]);
        assert!(diff < 1.0, "{codec}: {diff}");
    }
}

#[test]
fn pcm_tracks_decode_bit_exact_in_every_layout() {
    let prefs = Prefs::default();
    let known: Vec<i16> = (0..72_000i32)
        .flat_map(|i| {
            let l = ((i as f64 * 0.0577).sin() * 30_000.0) as i16;
            let r = match i % 4800 {
                0 => i16::MIN,
                1 => i16::MAX,
                _ => (i.wrapping_mul(7919) % 65_536 - 32_768) as i16,
            };
            [l, r]
        })
        .collect();
    let obs = with_pcm(
        Clip::new((320, 240), (30, 1), 45),
        "h264-pcm-s16le.mkv",
        std::slice::from_ref(&known),
        48_000,
    );
    let p = probe(&obs, &prefs).unwrap();
    assert_eq!(p.video_codec, Some(Codec::H264));
    assert_eq!(
        (p.audio[0].codec.as_str(), p.audio[0].native()),
        ("pcm", true)
    );
    assert!(p.unsupported().is_empty(), "{:?}", p.unsupported());
    let pcm = audio_pcm(&obs, 0, 48_000, false, &prefs).unwrap();
    let want: Vec<f32> = known.iter().map(|&s| s as f32 / 32768.0).collect();
    assert_eq!(pcm.samples.len(), want.len());
    assert!(pcm.samples == want, "s16le in MKV is not bit-exact");
    let mid = audio_pcm_range(&obs, 0, 0.5, 1.0, 48_000, 2, &prefs).unwrap();
    assert!(
        mid.samples == want[48_000..96_000],
        "a PCM range is not bit-exact"
    );
    for c in PCM_CASES {
        let (path, want) = c.write();
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(p.audio.len(), 1, "{}", c.file);
        assert!(p.audio[0].native(), "{}: {}", c.file, p.audio[0].codec);
        let ours = audio_pcm(&path, 0, PCM_RATE, false, &prefs).unwrap();
        assert_eq!(ours.samples.len(), PCM_FRAMES * 2, "{}", c.file);
        assert!(ours.samples == want, "{} is not bit-exact", c.file);
    }
}

fn with_ac3() -> PathBuf {
    let src = Clip {
        sounds: vec![Sound::Sine {
            hz: 880.0,
            amp: 0.25,
        }],
        kbps: 32,
        ..Clip::new((64, 64), (10, 1), 10)
    }
    .make(AREA, "ac3-base.mkv");
    let r = mkv::Reader::open(&src).unwrap();
    let mut w = mkv::Writer::new(Cursor::new(Vec::new()), mkv::Options::default());
    let (v, a) = (r.video().unwrap(), r.audio()[0]);
    let tv = w
        .add_track(mkv::TrackConfig::from_track(&r.tracks[v]))
        .unwrap();
    let t3 = w
        .add_track(mkv::TrackConfig {
            sample_rate: 48_000,
            channels: 2,
            ..mkv::TrackConfig::new(mkv::TrackKind::Audio, "A_AC3", TimeBase::new(1, 48_000))
        })
        .unwrap();
    let ta = w
        .add_track(mkv::TrackConfig::from_track(&r.tracks[a]))
        .unwrap();
    let frames = refs::ac3_silence(48_000usize.div_ceil(refs::AC3_FRAME));
    let mut items: Vec<(f64, usize, usize)> = Vec::new();
    for (lane, track) in [(0, v), (2, a)] {
        let t = &r.tracks[track];
        for (k, s) in t.samples.iter().enumerate() {
            items.push((t.seconds(s.pts), lane, k));
        }
    }
    for k in 0..frames.len() {
        items.push(((k * refs::AC3_FRAME) as f64 / 48_000.0, 1, k));
    }
    items.sort_by(|x, y| x.0.total_cmp(&y.0));
    for (_, lane, k) in items {
        if lane == 1 {
            let at = (k * refs::AC3_FRAME) as i64;
            w.push_sample(t3, &frames[k], at, Some(refs::AC3_FRAME as u32), true)
                .unwrap();
            continue;
        }
        let (track, to) = if lane == 0 { (v, tv) } else { (a, ta) };
        let s = r.tracks[track].samples[k];
        let data = r.read(track, k).unwrap();
        let duration = (s.duration > 0).then_some(s.duration);
        w.push_sample(to, &data, s.pts, duration, s.keyframe)
            .unwrap();
    }
    let out = dir().join("h264-ac3-aac.mkv");
    testref::write_atomic(&out, &w.finish().unwrap().into_inner()).unwrap();
    out
}

#[test]
fn an_ac3_track_probes_and_decodes_natively() {
    let prefs = Prefs::default();
    let path = with_ac3();
    let p = probe(&path, &prefs).unwrap();
    let names: Vec<_> = p
        .audio
        .iter()
        .map(|a| (a.codec.as_str(), a.native()))
        .collect();
    assert_eq!(names, [("ac3", true), ("aac", true)]);
    assert!(p.video_native());
    assert!(p.unsupported().is_empty());
    let ac3 = audio_pcm(&path, 0, 48_000, false, &prefs).unwrap();
    assert!(
        (ac3.frames() as i64 - 48_000).abs() < 1_600,
        "{}",
        ac3.frames()
    );
    assert!(
        ac3.samples.iter().all(|&s| s == 0.0),
        "silent AC-3 decoded loud"
    );
    let aac = audio_pcm(&path, 1, 48_000, false, &prefs).unwrap();
    assert!(
        (aac.frames() as i64 - 48_000).abs() < 1_200,
        "{}",
        aac.frames()
    );
}

#[test]
fn aac_and_opus_in_matroska_line_up_with_faad_and_libopus_to_the_sample() {
    let prefs = Prefs::default();
    let r = Refs::open(AREA);
    let rate = refs::LINEUP_RATE;
    let from = refs::LINEUP_AT[1] as f64 / f64::from(rate);
    for name in refs::LINEUPS {
        let path = r.path(name);
        let ours = audio_pcm(&path, 0, rate, false, &prefs).unwrap().samples;
        let range = audio_pcm_range(&path, 0, from, 0.9, rate, 2, &prefs).unwrap();
        for (k, at) in refs::LINEUP_AT.into_iter().enumerate() {
            let theirs = r.f32s(&refs::window(name, &format!("at{k}")));
            assert_eq!(theirs.len(), refs::WINDOW * 2, "{name} at {at}");
            let whole = worst(&ours[at * 2..at * 2 + theirs.len()], &theirs);
            assert!(whole < 0.01, "{name} at {at}: {whole}");
        }
        let theirs = r.f32s(&refs::window(name, "at1"));
        let part = worst(&range.samples[..theirs.len()], &theirs);
        assert!(part < 0.01, "{name}: {part}");
    }
}

#[test]
fn matroska_audio_off_the_millisecond_drops_the_last_blocks_discard_padding() {
    let prefs = Prefs::default();
    let r = Refs::open(AREA);
    for o in refs::OFF_MS {
        let (name, rate, limit) = (o.file, o.rate, o.limit);
        let path = r.path(name);
        let frames = refs::off_ms_frames(rate);
        let ours = audio_pcm(&path, 0, rate, false, &prefs).unwrap().samples;
        assert_eq!(ours.len(), frames * 2, "{name}");
        for (at, start) in [("head", refs::OFF_MS_HEAD), ("tail", frames - refs::WINDOW)] {
            let theirs = if o.silent {
                vec![0.0; refs::WINDOW * 2]
            } else {
                r.f32s(&refs::window(name, at))
            };
            assert_eq!(theirs.len(), refs::WINDOW * 2, "{name} {at}");
            let w = worst(&ours[start * 2..(start + refs::WINDOW) * 2], &theirs);
            assert!(w < limit, "{name} {at}: {w}");
        }
        let at = |t: f64| (t * rate as f64).round() as usize * 2;
        for (from, to) in [(0.5, f64::INFINITY), (0.9, 1.5)] {
            let part = audio_pcm_range(&path, 0, from, to, rate, 2, &prefs).unwrap();
            assert_eq!(part.samples.len(), ours.len() - at(from), "{name} {from}");
            let w = worst(&part.samples[4_000..], &ours[at(from) + 4_000..]);
            assert!(w < limit, "{name} {from}: {w}");
        }
        let streamed: usize = audio_stream(&path, 0, 0.5, rate, 2, &prefs)
            .unwrap()
            .map(|b| b.unwrap().samples.len())
            .sum();
        assert_eq!(streamed, ours.len() - at(0.5), "{name} stream");
    }
    let aac = Clip {
        kbps: 32,
        ..Clip::new((64, 64), (10_000, 1), 10_123)
    }
    .audio_only()
    .make(AREA, "off-ms-aac.mka");
    let m = mkv::Reader::open(&aac).unwrap();
    let t = &m.tracks[0];
    assert!(t.samples.iter().all(|s| s.discard == 0));
    let stamped = (t.seconds(t.end()) * 48_000.0).round() as usize;
    let ours = audio_pcm(&aac, 0, 48_000, true, &prefs).unwrap();
    assert_eq!(ours.frames(), stamped);
    assert!(ours.frames() < t.samples.len() * 1024);
}

const RECORDING_FRAMES: usize = 180 * 48_000;

fn recording_s16() -> Vec<i16> {
    s16(
        48_000,
        RECORDING_FRAMES,
        Sound::Fn(sine_440),
        Sound::Noise { amp: 0.3, seed: 7 },
    )
}

fn recording() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        with_pcm(
            Clip {
                gop: 60,
                ..Clip::new((352, 240), (30, 1), 5400)
            },
            "recording-h264-pcm.mkv",
            &[recording_s16()],
            48_000,
        )
    })
}

fn recording_pcm() -> &'static [f32] {
    static PCM: OnceLock<Vec<f32>> = OnceLock::new();
    PCM.get_or_init(|| {
        recording_s16()
            .into_iter()
            .map(|s| s as f32 / 32768.0)
            .collect()
    })
}

fn vint(d: &[u8], at: usize) -> (u64, usize) {
    let len = d[at].leading_zeros() as usize + 1;
    let mut v = u64::from(d[at]) & (0xff >> len);
    for b in &d[at + 1..at + len] {
        v = v << 8 | u64::from(*b);
    }
    let unknown = v == (1u64 << (7 * len)) - 1;
    (if unknown { u64::MAX } else { v }, len)
}

fn element(d: &[u8], at: usize) -> (u32, usize, usize) {
    let id_len = d[at].leading_zeros() as usize + 1;
    let id = d[at..at + id_len]
        .iter()
        .fold(0u32, |a, b| a << 8 | u32::from(*b));
    let (size, size_len) = vint(d, at + id_len);
    let body = at + id_len + size_len;
    let end = usize::try_from(size).map_or(d.len(), |s| (body + s).min(d.len()));
    (id, body, end)
}

fn void(d: &mut [u8], at: usize, end: usize) {
    d[at] = 0xec;
    d[at + 1] = 0x01;
    let n = (end - at - 9) as u64;
    d[at + 2..at + 9].copy_from_slice(&n.to_be_bytes()[1..]);
}

const CUES: u32 = 0x1c53_bb6b;

fn without_cues(src: &Path, name: &str) -> PathBuf {
    let mut d = std::fs::read(src).unwrap();
    let (_, _, header) = element(&d, 0);
    let (_, mut at, _) = element(&d, header);
    let mut voided = 0;
    while at < d.len() {
        let (id, body, end) = element(&d, at);
        if id == CUES {
            void(&mut d, at, end);
            voided += 1;
        } else if id == 0x114d_9b74 {
            let mut seek = body;
            while seek < end {
                let (_, _, next) = element(&d, seek);
                if d[seek..next].windows(4).any(|w| w == CUES.to_be_bytes()) {
                    void(&mut d, seek, next);
                }
                seek = next;
            }
        }
        at = end;
    }
    assert!(voided > 0, "{} has no Cues to drop", src.display());
    let out = dir().join(name);
    testref::write_atomic(&out, &d).unwrap();
    out
}

fn mean_diff(a: &Rgba, b: &Rgba) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| x.abs_diff(*y) as u64)
        .sum::<u64>() as f64
        / a.data.len() as f64
}

fn streamed(path: &Path, from: f64, rate: u32, channels: u16) -> Vec<f32> {
    let mut all = Vec::new();
    let stream = audio_stream(path, 0, from, rate, channels, &Prefs::default()).unwrap();
    let blocks: Vec<AudioBuffer> = stream.collect::<Result<_, _>>().unwrap();
    let (last, full) = blocks.split_last().unwrap();
    assert!(full.iter().all(|b| b.frames() == rate as usize));
    assert!(last.frames() <= rate as usize && last.frames() > 0);
    for b in &blocks {
        all.extend_from_slice(&b.samples);
    }
    all
}

#[test]
fn an_mkv_still_late_in_a_recording_comes_from_the_cues_on_every_route() {
    let path = recording();
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let mut routes = Vec::new();
    for prefs in [cpu.clone(), Prefs::default()] {
        let mut ours = frames_at(path, &[170.5, 10.0, 171.9, 172.0, 999.0], 120, &prefs).unwrap();
        assert_eq!(ours.len(), 5);
        assert!(ours.iter().all(|s| s.height == 120 && lit(s)));
        let theirs = source_still((352, 240), 5100, &ours[0]);
        let d = mean_diff(&ours[0], &theirs);
        assert!(d < 6.0, "{d}");
        assert!(ours[0].data == ours[2].data);
        assert!(ours[0].data != ours[1].data && ours[0].data != ours[3].data);
        routes.push(ours.swap_remove(0));
    }
    assert!(mean_diff(&routes[0], &routes[1]) < 1.0);
    let first = first_frame(path, 240, &cpu).unwrap();
    let theirs = source_still((352, 240), 0, &first);
    let d = mean_diff(&first, &theirs);
    assert!(d < 6.0, "{d}");
}

#[test]
fn mkv_pcm_streams_bit_exact_against_the_whole_decode() {
    let path = recording();
    let prefs = Prefs::default();
    let theirs = recording_pcm();
    let whole = audio_pcm(path, 0, 48_000, false, &prefs).unwrap();
    assert!(whole.samples == theirs, "the whole decode is not bit-exact");
    let ours = streamed(path, 0.0, 48_000, 2);
    assert!(ours == theirs, "the stream from 0 is not bit-exact");
    let late = streamed(path, 150.25, 48_000, 2);
    assert!(
        late == theirs[150 * 96_000 + 24_000..],
        "a late stream is off"
    );
    let part = audio_pcm_range(path, 0, 100.0, 130.0, 48_000, 2, &prefs).unwrap();
    assert!(part.samples == theirs[9_600_000..12_480_000]);
    let mono = audio_pcm(path, 0, 16_000, true, &prefs).unwrap();
    assert!(streamed(path, 0.0, 16_000, 1) == mono.samples);
}

#[test]
fn a_cueless_or_cut_mkv_still_streams_and_shows_or_fails_cleanly() {
    let prefs = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let path = recording();
    let bare = without_cues(path, "recording-no-cues.mkv");
    let want = frames_at(path, &[120.0], 120, &prefs).unwrap();
    let got = frames_at(&bare, &[120.0], 120, &prefs).unwrap();
    assert!(mean_diff(&want[0], &got[0]) < 1.0);
    let theirs = recording_pcm();
    assert!(streamed(&bare, 120.0, 48_000, 2) == theirs[120 * 96_000..]);
    let bytes = std::fs::read(path).unwrap();
    let cut = dir().join("recording-cut.mkv");
    std::fs::write(&cut, &bytes[..bytes.len() * 3 / 5]).unwrap();
    let early = frames_at(&cut, &[20.0], 120, &prefs).unwrap();
    assert!(lit(&early[0]));
    let _ = frames_at(&cut, &[170.0], 120, &prefs);
    let _ = first_frame(&cut, 120, &prefs);
    if let Ok(stream) = audio_stream(&cut, 0, 170.0, 48_000, 2, &prefs) {
        for b in stream {
            if b.is_err() {
                break;
            }
        }
    }
    let head: Vec<f32> = audio_stream(&cut, 0, 0.0, 48_000, 2, &prefs)
        .unwrap()
        .take(30)
        .flat_map(|b| b.unwrap().samples)
        .collect();
    assert!(head == theirs[..30 * 96_000]);
}

#[test]
fn a_range_past_the_end_returns_only_what_the_track_has() {
    let path = recording();
    let theirs = recording_pcm();
    let prefs = Prefs::default();
    let tail = audio_pcm_range(path, 0, 178.0, 190.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(tail.frames(), 96_000);
    assert!(tail.samples == theirs[178 * 96_000..]);
    let past = audio_pcm_range(path, 0, 200.0, 210.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(past.frames(), 0);
    let whole = audio_pcm(clip(), 0, 48_000, false, &prefs).unwrap();
    let aac = audio_pcm_range(clip(), 0, 4.0, 16.0, 48_000, 2, &prefs).unwrap();
    assert_eq!(aac.frames(), whole.frames() - 4 * 48_000);
}

const FADE: u64 = 90;

fn fade_picture(n: u64, f: &mut Frame) {
    testref::draw_yuv(n, 0, f);
    let k = n.min(FADE) as i32;
    for (p, mid) in f.planes.iter_mut().zip([16, 128, 128]) {
        for v in p.data.iter_mut() {
            *v = (mid + (i32::from(*v) - mid) * k / FADE as i32) as u8;
        }
    }
}

#[test]
fn an_exact_still_inside_the_first_gop_of_a_fade_is_not_the_keyframe() {
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let size = (352, 240);
    for name in ["fade-in.mp4", "fade-in.mkv"] {
        let path = Clip {
            gop: 120,
            b_frames: 2,
            picture: Picture::Fn(fade_picture),
            ..Clip::new(size, (30, 1), 300)
        }
        .video_only()
        .make(AREA, name);
        for prefs in [cpu.clone(), Prefs::default()] {
            let key = frames_at(&path, &[1.5], 120, &prefs).unwrap();
            assert!(!lit(&key[0]), "{name}: the keyframe at 0 is not black");
            let ours =
                frames_at_seek(&path, &[1.5, 0.5, 1.5, 6.0], 120, Seek::Exact, &prefs).unwrap();
            assert!(lit(&ours[0]) && lit(&ours[3]), "{name}");
            assert!(ours[0].data == ours[2].data && ours[0].data != ours[1].data);
            let theirs = shrink(size, &ours[0], |x, y| {
                testref::rgb_at(size.0, size.1, 45, x, y).map(|c| (u64::from(c) * 45 / FADE) as u8)
            });
            let d = mean_diff(&ours[0], &theirs);
            assert!(d < 6.0, "{name}: {d}");
        }
    }
}

const COUNTED_WIDTH: usize = 426;
const THIRD: usize = COUNTED_WIDTH / 3;

fn counted_picture(n: u64, f: &mut Frame) {
    let digits = [n % 16, n / 16 % 16, n / 256 % 16];
    let stride = f.planes[0].stride;
    for y in 0..f.height as usize {
        for x in 0..f.width as usize {
            f.planes[0].data[y * stride + x] = (22 + 13 * digits[(x / THIRD).min(2)]) as u8;
        }
    }
    for p in &mut f.planes[1..] {
        p.data.fill(128);
    }
}

fn counted() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let tone = || Sound::Fn(sine_440);
        with_pcm(
            Clip {
                gop: 60,
                b_frames: 2,
                qp: 10,
                picture: Picture::Fn(counted_picture),
                ..Clip::new((COUNTED_WIDTH as u32, 240), (30, 1), 1800)
            },
            "counted-426x240-h264-pcm.mkv",
            &[s16(48_000, 60 * 48_000, tone(), tone())],
            48_000,
        )
    })
}

fn frame_numbers(path: &Path) -> Vec<i64> {
    frames_from(path, 0)
        .map(|f| {
            let p = &f.planes[0];
            let digit = |third: usize| {
                let cols = third * THIRD + 10..(third + 1) * THIRD - 10;
                let width = cols.len();
                let sum: u64 = (0..p.height)
                    .flat_map(|y| p.data[y * p.stride..][cols.clone()].iter())
                    .map(|&v| u64::from(v))
                    .sum();
                let mean = sum as f64 / (width * p.height) as f64;
                ((mean - 22.0) / 13.0).round().clamp(0.0, 15.0) as i64
            };
            digit(0) + 16 * digit(1) + 256 * digit(2)
        })
        .collect()
}

#[test]
fn three_hundred_short_cuts_join_without_drift_on_every_route() {
    let mut seed = 0x2545_f491_u64;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let mut want: Vec<i64> = Vec::new();
    let pieces: Vec<Piece> = (0..300)
        .map(|k| {
            let len = if k % 9 == 4 {
                61 + next(80) as i64
            } else {
                2 + next(19) as i64
            };
            let a = next((1800 - len) as u64) as i64;
            want.extend(a..a + len);
            Piece {
                path: counted().to_path_buf(),
                from: a as f64 / 30.0,
                to: (a + len) as f64 / 30.0,
            }
        })
        .collect();
    let total = want.len() as u64;
    for (name, prefs) in routes() {
        let out = dir().join(format!("drift-{name}.mp4"));
        let mut told = Vec::new();
        let done = export(
            &out,
            &pieces,
            ExportOptions {
                prefs,
                ..ExportOptions::default()
            },
            |p| {
                told.push(p.done);
                true
            },
        )
        .unwrap();
        assert!(done.smart, "{name}: {:?}", done.reasons);
        steady(&told, name);
        assert_eq!(done.frames, total, "{name}");
        assert!(done.copied > 0 && done.encoded > 0, "{name}: {done:?}");
        let got = frame_numbers(&out);
        assert_eq!(got.len(), want.len(), "{name}");
        let wrong = got.iter().zip(&want).filter(|(g, w)| g != w).count();
        let at = got.iter().zip(&want).position(|(g, w)| g != w).unwrap_or(0);
        let near = at.saturating_sub(3)..(at + 4).min(want.len());
        assert_eq!(
            wrong,
            0,
            "{name}: {wrong} frames are not the source frame they should be, first at {at}: {:?} for {:?}",
            &got[near.clone()],
            &want[near]
        );
        assert_eq!(got.last(), want.last(), "{name}");
        let r = mp4::Reader::open(&out).unwrap();
        let t = &r.tracks[r.video().unwrap()];
        let ticks: u64 = t.samples.iter().map(|s| u64::from(s.duration)).sum();
        let video = t.seconds(ticks as i64);
        assert!(
            (video - total as f64 / 30.0).abs() < 1e-3,
            "{name}: {video} s of video for {total} frames"
        );
        let pcm = audio_pcm(&out, 0, 48_000, false, &Prefs::default())
            .unwrap()
            .samples;
        assert_eq!(pcm.len() as u64, total * 1600 * 2, "{name}");
        let rms = (pcm.iter().map(|&s| s as f64 * s as f64).sum::<f64>() / pcm.len() as f64).sqrt();
        let tone = 0.125 / std::f64::consts::SQRT_2;
        assert!((rms - tone).abs() < 0.004, "{name}: {rms} against {tone}");
    }
}

fn dropped_frames() -> PathBuf {
    let (w, h) = (160, 96);
    let tb = TimeBase::new(1, 30);
    let mut e = h264::Encoder::new(h264::EncoderConfig {
        width: w,
        height: h,
        fps: (30, 1),
        rate: Rate::Qp(24),
        gop: 30,
        b_frames: 0,
        threads: 1,
        bitstream: Bitstream::LengthPrefixed,
        ..Default::default()
    })
    .unwrap();
    let mut m = mkv::Writer::new(Cursor::new(Vec::new()), mkv::Options::default());
    let t = m
        .add_track(mkv::TrackConfig::h264(e.avcc(), w, h, tb))
        .unwrap();
    for n in (0..240u64).filter(|n| n % 5 != 2) {
        let mut f = testref::yuv_frame(w, h, n, 0);
        f.time_base = tb;
        for p in e.encode(&f).unwrap() {
            m.push(t, &p).unwrap();
        }
    }
    for p in e.flush().unwrap() {
        m.push(t, &p).unwrap();
    }
    let out = dir().join("dropped-frames-h264.mkv");
    testref::write_atomic(&out, &m.finish().unwrap().into_inner()).unwrap();
    let back = mkv::Reader::open(&out).unwrap();
    let mut pts: Vec<i64> = back.tracks[0].samples.iter().map(|s| s.pts).collect();
    pts.sort_unstable();
    let steps: Vec<i64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().min() < steps.iter().max(),
        "the dropped frames left no gap in the timestamps"
    );
    out
}

#[test]
fn an_mkv_probe_reads_the_head_and_cues_and_matches_the_full_index() {
    let vfr = dropped_frames();
    let prefs = Prefs::default();
    for (name, src) in [
        ("counted", counted()),
        ("recording", recording()),
        ("vfr", vfr.as_path()),
    ] {
        let head = probe(src, &prefs).unwrap();
        let bare = without_cues(
            src,
            &format!("cueless-{}", src.file_name().unwrap().to_str().unwrap()),
        );
        let full = probe(&bare, &prefs).unwrap();
        let count = frame_count(src, &prefs).unwrap();
        assert_eq!(frame_count(&bare, &prefs).unwrap(), count, "{name}");
        assert!(
            full.frames.is_none() || full.frames == Some(count),
            "{name}"
        );
        assert_eq!(
            keyframes(src, &prefs).unwrap(),
            keyframes(&bare, &prefs).unwrap(),
            "{name}"
        );
        assert!(
            head.frames.is_none() || head.frames == Some(count),
            "{name}"
        );
        assert!(
            (head.duration - full.duration).abs() < 1e-6,
            "{name}: {} against {}",
            head.duration,
            full.duration
        );
        assert_eq!(
            (
                head.fps,
                head.width,
                head.height,
                &head.profile,
                head.audio.len()
            ),
            (
                full.fps,
                full.width,
                full.height,
                &full.profile,
                full.audio.len()
            ),
            "{name}"
        );
    }
    assert_eq!(frame_count(counted(), &prefs).unwrap(), 1800);
    let hollow = dir().join("counted-hollow.mkv");
    let mut bytes = std::fs::read(counted()).unwrap();
    let n = bytes.len();
    bytes[n / 4..n * 3 / 4].fill(0);
    std::fs::write(&hollow, &bytes).unwrap();
    let got = probe(&hollow, &prefs).unwrap();
    assert!(got.frames.is_none(), "the probe read the clusters");
    assert_eq!((got.duration, got.fps), (60.0, (30, 1)));
    let bare = without_cues(counted(), "cueless-counted-hollow-src.mkv");
    let hollow = dir().join("cueless-counted-hollow.mkv");
    let mut bytes = std::fs::read(&bare).unwrap();
    let n = bytes.len();
    bytes[n / 4..n * 3 / 4].fill(0);
    std::fs::write(&hollow, &bytes).unwrap();
    let got = probe(&hollow, &prefs).unwrap();
    assert!(got.frames.is_none(), "the Cue-less probe read the clusters");
    assert_eq!((got.duration, got.fps), (60.0, (30, 1)));
}

fn two_tracks() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let tone = || Sound::Fn(sine_440);
        let noise = || Sound::Noise { amp: 0.3, seed: 3 };
        with_pcm(
            Clip {
                gop: 60,
                ..Clip::new((352, 240), (30, 1), 1200)
            },
            "two-pcm-tracks.mkv",
            &[
                s16(48_000, 40 * 48_000, tone(), tone()),
                s16(48_000, 37 * 48_000 + 24_000, noise(), noise()),
            ],
            48_000,
        )
    })
}

#[test]
fn a_mix_stream_equals_the_whole_mix_with_and_without_cues() {
    let prefs = Prefs::default();
    let path = two_tracks();
    let bare = without_cues(path, "cueless-two-pcm-tracks.mkv");
    let gains = [0.5, 1.25];
    for file in [path, bare.as_path()] {
        for from in [0.0, 12.3] {
            let want = audio_mix(file, &gains, from, f64::INFINITY, 48_000, &prefs).unwrap();
            let got: Vec<f32> = audio_mix_stream(file, &gains, from, 48_000, &prefs)
                .unwrap()
                .flat_map(|b| b.unwrap().samples)
                .collect();
            assert_eq!(
                got.len(),
                want.samples.len(),
                "{} from {from}",
                file.display()
            );
            assert!(got == want.samples, "{} from {from}", file.display());
        }
    }
}

fn quiet_440(i: u64, _: u16, rate: u32) -> f32 {
    0.4 * sine_440(i, 0, rate)
}

fn quiet_660(i: u64, _: u16, rate: u32) -> f32 {
    (0.05 * (std::f64::consts::TAU * 660.0 * i as f64 / f64::from(rate)).sin()) as f32
}

fn black(_: u64, f: &mut Frame) {
    f.planes[0].data.fill(16);
    for p in &mut f.planes[1..] {
        p.data.fill(128);
    }
}

fn two_tones(name: &str, audio: Option<AudioCodec>) -> PathBuf {
    let clip = Clip {
        gop: 20,
        picture: Picture::Fn(black),
        sounds: vec![Sound::Fn(quiet_440), Sound::Fn(quiet_660)],
        ..Clip::new((64, 64), (10, 1), 200)
    };
    match audio {
        Some(codec) => Clip {
            audio: Some(codec),
            ..clip
        }
        .make(AREA, name),
        None => {
            let pcm = [Sound::Fn(quiet_440), Sound::Fn(quiet_660)]
                .map(|s| s16(48_000, 20 * 48_000, s, s));
            with_pcm(clip, name, &pcm, 48_000)
        }
    }
}

fn off(got: &[f32], want: &[f32], limit: f32) -> Option<usize> {
    if got.len() != want.len() {
        return Some(got.len().min(want.len()));
    }
    got.iter()
        .zip(want)
        .position(|(a, b)| (a - b).abs() > limit)
}

#[test]
fn every_audio_range_and_stream_start_cuts_the_whole_decode() {
    let prefs = Prefs::default();
    let pcm = two_tones("ranged-pcm.mkv", None);
    let files = [
        (pcm.clone(), 0.0),
        (without_cues(&pcm, "ranged-pcm-no-cues.mkv"), 0.0),
        (two_tones("ranged-pcm.mp4", None), 0.0),
        (two_tones("ranged-aac.mkv", Some(AudioCodec::Aac)), 1e-5),
        (two_tones("ranged-aac.mp4", Some(AudioCodec::Aac)), 1e-5),
        (two_tones("ranged-opus.mkv", Some(AudioCodec::Opus)), 2e-3),
    ];
    let mut ranges: Vec<(usize, usize)> = vec![
        (9_600, 33_600),
        (29_600, 33_600),
        (40_960, 44_960),
        (41_460, 45_460),
        (307_201, 311_297),
        (0, 48_000),
        (48_000, 144_000),
        (144_000, 336_000),
        (336_000, 720_000),
        (900_000, 960_000),
    ];
    ranges.extend((0..8).map(|k| (9_600 + 4_000 * k, 13_600 + 4_000 * k)));
    ranges.extend((0..60).map(|k| (20_000 + 337 * k, 22_000 + 337 * k)));
    for (file, limit) in &files {
        let name = file.file_name().unwrap().to_string_lossy();
        let whole: Vec<Vec<f32>> = (0..2)
            .map(|t| audio_pcm(file, t, 48_000, false, &prefs).unwrap().samples)
            .collect();
        for (t, all) in whole.iter().enumerate() {
            assert!(all.len() >= 960_000 * 2, "{name} track {t}");
            for &(a, b) in &ranges {
                let (from, to) = (a as f64 / 48_000.0, b as f64 / 48_000.0);
                let got = audio_pcm_range(file, t, from, to, 48_000, 2, &prefs).unwrap();
                let at = off(&got.samples, &all[a * 2..b * 2], *limit);
                assert!(at.is_none(), "{name} track {t} [{a}, {b}) at {at:?}");
            }
        }
        for &(a, b) in ranges.iter().take(10) {
            let from = a as f64 / 48_000.0;
            for (t, all) in whole.iter().enumerate() {
                let got: Vec<f32> = audio_stream(file, t, from, 48_000, 2, &prefs)
                    .unwrap()
                    .flat_map(|b| b.unwrap().samples)
                    .take((b - a) * 2)
                    .collect();
                let at = off(&got, &all[a * 2..b * 2], *limit);
                assert!(at.is_none(), "{name} stream {t} from {a} at {at:?}");
            }
            let mixed: Vec<f32> = audio_mix_stream(file, &[0.5, 2.0], from, 48_000, &prefs)
                .unwrap()
                .flat_map(|b| b.unwrap().samples)
                .take((b - a) * 2)
                .collect();
            let want: Vec<f32> = whole[0][a * 2..b * 2]
                .iter()
                .zip(&whole[1][a * 2..b * 2])
                .map(|(x, y)| 0.0 + x * 0.5 + y * 2.0)
                .collect();
            let at = off(&mixed, &want, limit * 3.0);
            assert!(at.is_none(), "{name} mix from {a} at {at:?}");
        }
    }
}

fn frame_hashes(path: &Path) -> Vec<String> {
    frames_from(path, 0).map(|f| hash(&f)).collect()
}

fn luma(path: &Path) -> Vec<Vec<u8>> {
    frames_from(path, 0)
        .map(|f| {
            let p = &f.planes[0];
            (0..p.height)
                .flat_map(|y| p.data[y * p.stride..y * p.stride + p.width].to_vec())
                .collect()
        })
        .collect()
}

fn luma_psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    10.0 * (255.0f64.powi(2) * a.len() as f64 / se.max(1e-9)).log10()
}

#[test]
fn a_vp9_cut_copies_its_middles_bit_exact_into_vp09_and_encodes_only_its_heads() {
    let src = fixture(refs::VP9_ALTREF);
    let webm = mkv::Reader::open(&src).unwrap();
    let superframes = (0..webm.tracks[0].samples.len())
        .filter(|&k| {
            webm.read(0, k)
                .unwrap()
                .last()
                .is_some_and(|b| b & 0xe0 == 0xc0)
        })
        .count();
    assert!(
        superframes > 0,
        "{}: libvpx placed no alt-ref superframe",
        src.display()
    );
    let out = dir().join("vp9-cut.mp4");
    let pieces = [(0.4, 1.2), (1.5, 1.8), (0.1, 0.5)].map(|(from, to)| Piece {
        path: src.clone(),
        from,
        to,
    });
    let silent = |_: usize, frames: usize| {
        let mut buf = AudioBuffer::new(48_000, 2);
        buf.samples = vec![0.0; frames * 2];
        Ok(buf)
    };
    let opts = || ExportOptions {
        video: Codec::Vp9,
        ..ExportOptions::default()
    };
    let done = export_with_audio(&out, &pieces, opts(), silent, |_| true).unwrap();
    assert!(done.smart, "{:?}", done.reasons);
    assert_eq!(done.video.codec, Codec::Vp9);
    assert!(done.video.reasons.is_empty(), "{:?}", done.video.reasons);
    assert_eq!((done.frames, done.copied, done.encoded), (45, 25, 20));
    let r = mp4::Reader::open(&out).unwrap();
    let t = &r.tracks[r.video().unwrap()];
    assert_eq!(t.entry().unwrap().format, mp4::FourCc(*b"vp09"));
    assert!(decodes_clean(&out));
    let mut times: Vec<f64> = (0..t.samples.len())
        .filter_map(|i| t.presentation(i))
        .map(|p| t.seconds(p))
        .collect();
    times.sort_by(f64::total_cmp);
    assert_eq!(times.len(), 45);
    for (k, t) in times.iter().enumerate() {
        assert!((t - k as f64 / 30.0).abs() < 1e-3, "frame {k} at {t}");
    }
    let (ours, theirs) = (frame_hashes(&out), frame_hashes(&src));
    for (at, from, n) in [(8, 20, 16), (29, 50, 4), (40, 10, 5)] {
        assert_eq!(
            ours[at..at + n],
            theirs[from..from + n],
            "copied from {from}"
        );
    }
    let (ours, theirs) = (luma(&out), luma(&src));
    for (at, from, n) in [(0, 12, 8), (24, 45, 5), (33, 3, 7)] {
        for k in 0..n {
            let psnr = luma_psnr(&ours[at + k], &theirs[from + k]);
            assert!(psnr > 35.0, "edge frame {} at {psnr}", at + k);
        }
    }
    assert_eq!(
        probe(&out, &Prefs::default()).unwrap().video_codec,
        Some(Codec::Vp9)
    );
    let again = dir().join("vp9-recut.mp4");
    let piece = [Piece {
        path: out.clone(),
        from: 0.0,
        to: 0.8,
    }];
    let done = export_with_audio(&again, &piece, opts(), silent, |_| true).unwrap();
    assert!(done.smart && done.copied == 24, "{:?}", done.reasons);
    assert!(decodes_clean(&again));
}

const LATE: [f64; 2] = [2999.747, 2999.997];

fn bursts(i: u64, _: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    if t % 1.0 < 0.1 {
        (0.5 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
    } else {
        0.0
    }
}

fn late_base(ext: &str) -> PathBuf {
    Clip {
        gop: 48,
        b_frames: 2,
        sounds: vec![Sound::Fn(bursts)],
        channels: 1,
        ..Clip::new((352, 240), (24_000, 1_001), 959)
    }
    .make(AREA, &format!("late-base.{ext}"))
}

fn late_start(ext: &str) -> PathBuf {
    let base = late_base(ext);
    let name = format!("late-start.{ext}");
    if ext == "mkv" {
        return rebuild_mkv(&base, &name, &[], 0, LATE);
    }
    rebuild_mp4(&[&base], &name, &[], 0, |r, _, i, cfg| {
        let t = &r.tracks[i];
        let by = if t.kind == mp4::TrackKind::Video {
            LATE[0]
        } else {
            LATE[1]
        };
        let movie = f64::from(r.timescale);
        let media = t
            .edits
            .iter()
            .find(|e| e.media_time >= 0)
            .copied()
            .unwrap_or(mp4::Edit {
                segment_duration: (t.duration as f64 * movie / f64::from(t.timescale)).round()
                    as u64,
                media_time: 0,
                media_rate: 0x10000,
            });
        cfg.edits = vec![
            mp4::Edit {
                segment_duration: (by * movie).round() as u64,
                media_time: -1,
                media_rate: 0x10000,
            },
            media,
        ];
    })
}

fn onset(samples: &[f32], rate: u32) -> f64 {
    samples.iter().position(|s| s.abs() > 0.05).unwrap() as f64 / rate as f64
}

#[test]
fn a_file_that_starts_late_counts_every_time_from_its_start() {
    let prefs = Prefs::default();
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let gpu = pito_codecs::gpu::available()
        .iter()
        .any(|&(c, d, _)| c == Codec::H264 && d == pito_codecs::gpu::Direction::Decode);
    for ext in ["mkv", "mp4"] {
        let path = late_start(ext);
        let p = probe(&path, &prefs).unwrap();
        assert!((p.start - 2999.747).abs() < 0.002, "{ext}: {}", p.start);
        assert!((p.duration - 40.0).abs() < 0.2, "{ext}: {}", p.duration);
        assert!((p.video_start.unwrap() - 2999.747).abs() < 0.002, "{ext}");
        let audio_start = p.audio[0].start.unwrap();
        assert!((audio_start - 2999.99).abs() < 0.03, "{ext}: {audio_start}");

        let fps = p.fps.0 as f64 / p.fps.1 as f64;
        let k = (30.0 * fps - 1e-9).ceil() as i64;
        let at = k as f64 / fps;
        let theirs = decode_video(&path, 0.0, None, &cpu)
            .unwrap()
            .map(Result::unwrap)
            .find(|f| f.pts == k)
            .unwrap();
        let frame = decode_video(&path, at, None, &cpu)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(frame.pts, (at * fps).round() as i64, "{ext}");
        assert!(
            packed(&frame) == packed(&theirs),
            "{ext}: the frame at {at:.4} s differs"
        );
        let mut on_gpu = decode_video_gpu(&path, at, None, &prefs).unwrap();
        assert_eq!(on_gpu.on_gpu(), gpu, "{ext}: {:?}", on_gpu.reasons);
        let g = on_gpu.next().unwrap().unwrap();
        assert_eq!(g.pts(), frame.pts, "{ext}");
        if gpu {
            let g = match g {
                VideoFrame::Gpu(g) => g.download().unwrap(),
                VideoFrame::Cpu(f) => f,
            };
            let diff = luma_diff(&g, &frame);
            assert!(diff < 1.0, "{ext}: {diff}");
        }
        let still = frames_at_seek(&path, &[at], 240, Seek::Exact, &cpu).unwrap();
        let theirs_still = source_still((352, 240), k as u64, &still[0]);
        let d = mean_diff(&still[0], &theirs_still);
        assert!(d < 6.0, "{ext}: {d}");

        let ours = audio_pcm_range(&path, 0, 30.0, 35.0, 48_000, 1, &prefs).unwrap();
        let lead = LATE[1] - LATE[0];
        let theirs = audio_pcm_range(
            &late_base(ext),
            0,
            30.0 - lead,
            35.0 - lead,
            48_000,
            1,
            &prefs,
        )
        .unwrap()
        .samples;
        assert_eq!(ours.frames(), 240_000, "{ext}");
        let (lag, w) = (-24i64..=24)
            .map(|lag| {
                let skip = lag.unsigned_abs() as usize;
                let (a, b) = if lag < 0 {
                    (&ours.samples[skip..], &theirs[..])
                } else {
                    (&ours.samples[..], &theirs[skip..])
                };
                let n = a.len().min(b.len()) - 48;
                (lag, worst(&a[..n], &b[..n]))
            })
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap();
        assert!(w < 0.01, "{ext}: {w} at a lag of {lag} samples");
        let head = audio_pcm_range(&path, 0, 0.0, 1.0, 48_000, 1, &prefs).unwrap();
        let o = onset(&head.samples, 48_000);
        assert!((o - 0.25).abs() < 0.01, "{ext}: {o}");

        let out = dir().join(format!("late-cut-{ext}.mp4"));
        let done = export(
            &out,
            &[Piece {
                path: path.clone(),
                from: 10.0,
                to: 20.0,
            }],
            ExportOptions {
                prefs: prefs.clone(),
                ..ExportOptions::default()
            },
            |_| true,
        )
        .unwrap();
        assert!(done.smart && done.copied > 0, "{ext}");
        assert!(decodes_clean(&out), "{ext}");
        let first = (10.0 * fps).round() as i64;
        let psnr = min_psnr(&out, &path, &[(first, first + done.frames as i64)]);
        assert!(psnr > 40.0, "{ext}: {psnr}");
        let r = mp4::Reader::open(&out).unwrap();
        for (s, t) in [("video", r.video().unwrap()), ("audio", r.audio()[0])] {
            let t = &r.tracks[t];
            let start = (0..t.samples.len())
                .filter_map(|i| t.presentation(i))
                .min()
                .map_or(0.0, |p| t.seconds(p));
            let length = t.duration_seconds();
            assert!(start.abs() < 0.05, "{ext} {s}: {start}");
            assert!((length - 10.0).abs() < 0.06, "{ext} {s}: {length}");
        }
        let cut = audio_pcm(&out, 0, 48_000, true, &prefs).unwrap();
        let o = onset(&cut.samples, 48_000);
        assert!(
            (o - 0.25).abs() < 0.02,
            "{ext}: the cut's audio starts at {o}"
        );
    }
}

fn packed(f: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    for p in &f.planes {
        for y in 0..p.height {
            out.extend_from_slice(&p.data[y * p.stride..y * p.stride + p.width]);
        }
    }
    out
}

#[test]
fn a_start_inside_a_gop_skips_unused_pictures_and_stays_bit_exact() {
    let cpu = Prefs {
        decode: Choice::Cpu,
        ..Prefs::default()
    };
    let hevc = fixture(refs::PYRAMID);
    let mut whole = Digest::new();
    let mut frames = 0;
    for f in frames_from(&hevc, 0) {
        whole.frame(&f);
        frames += 1;
    }
    assert_eq!(frames, refs::PYRAMID_FRAMES);
    Refs::open(AREA).assert_digest(&refs::pyramid_recon(), &whole.hex());
    for (path, k) in [
        (clip(), 47),
        (clip(), 22),
        (hevc.as_path(), 37),
        (hevc.as_path(), 22),
    ] {
        let p = probe(path, &cpu).unwrap();
        let fps = p.fps.0 as f64 / p.fps.1 as f64;
        let at = k as f64 / fps;
        let theirs: Vec<String> = frames_from(path, 0).take(k + 8).map(|f| hash(&f)).collect();
        let ours: Vec<Frame> = decode_video(path, at, None, &cpu)
            .unwrap()
            .take(6)
            .map(Result::unwrap)
            .collect();
        let first = usize::try_from(ours[0].pts).unwrap();
        assert!(first.abs_diff(k) <= 1, "{k}: {first}");
        for (n, f) in ours.iter().enumerate() {
            assert!(
                hash(f) == theirs[first + n],
                "{}: frame {} differs",
                path.display(),
                first + n
            );
        }
        let mut on_gpu = decode_video_gpu(path, at, None, &Prefs::default()).unwrap();
        if on_gpu.on_gpu() {
            for f in &ours {
                let g = match on_gpu.next().unwrap().unwrap() {
                    VideoFrame::Gpu(g) => g.download().unwrap(),
                    VideoFrame::Cpu(f) => f,
                };
                assert_eq!(g.pts, f.pts, "{}", path.display());
                let diff = luma_diff(&g, f);
                assert!(diff < 1.0, "{}: {diff}", path.display());
            }
        }
    }
}

#[test]
fn a_stills_handle_hands_each_index_once_and_matches_single_calls() {
    let prefs = Prefs::default();
    let hevc = fixture(refs::PYRAMID);
    let times = [0.5, 1.2, 0.5, 1.37, 0.0];
    for path in [clip(), hevc.as_path()] {
        let stills = Stills::open(path, 120, &prefs).unwrap();
        for seek in [Seek::Keyframe, Seek::Exact] {
            let want = frames_at_seek(path, &times, 120, seek, &prefs).unwrap();
            for round in 0..4 {
                match round {
                    2 => stills.keep(1),
                    3 => stills.trim(),
                    _ => {}
                }
                let mut got: Vec<Vec<Rgba>> = vec![Vec::new(); times.len()];
                stills
                    .frames(&times, seek, |i, r| got[i].push(r.unwrap()))
                    .unwrap();
                for (i, g) in got.iter().enumerate() {
                    assert_eq!(g.len(), 1, "{} {seek:?} {i}", path.display());
                    assert!(g[0] == want[i], "{} {seek:?} {i}", path.display());
                }
            }
        }
    }
}

const TIMES: [f64; 6] = [0.1, 0.25, 0.4, 0.6, 0.75, 0.9];

#[test]
fn av1_stills_on_the_gpu_match_the_cpu_in_a_tagged_matrix() {
    let path = fixture(refs::AV1_709);
    let cpu = Stills::open(&path, 240, &route_prefs(Choice::Cpu)).unwrap();
    gpu_stills_match(&path, &cpu, &TIMES);
}

fn still_psnr(a: &Rgba, b: &Rgba) -> f64 {
    let se: f64 = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum();
    10.0 * (255.0f64.powi(2) / (se / a.data.len() as f64).max(1e-9)).log10()
}

fn gpu_stills_match(path: &Path, cpu: &Stills, times: &[f64]) -> bool {
    let Ok(gpu) = Stills::open(path, 240, &route_prefs(Choice::Gpu)) else {
        return false;
    };
    for seek in [Seek::Exact, Seek::Keyframe] {
        for round in 0..2 {
            let mut want: Vec<Option<Rgba>> = vec![None; times.len()];
            cpu.frames(times, seek, |i, r| want[i] = Some(r.unwrap()))
                .unwrap();
            let mut got: Vec<Option<Result<Rgba, String>>> = vec![None; times.len()];
            if gpu
                .frames(times, seek, |i, r| {
                    got[i] = Some(r.map_err(|e| e.to_string()))
                })
                .is_err()
            {
                return false;
            }
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let (Some(Ok(g)), Some(w)) = (g, w) else {
                    return false;
                };
                assert_eq!((g.width, g.height), (w.width, w.height));
                let p = still_psnr(g, w);
                assert!(
                    p > 50.0,
                    "{} {seek:?} round {round} still {i}: {p:.1} dB",
                    path.display()
                );
            }
        }
    }
    true
}

fn vp9_mkv_of(src: &Path, name: &str) -> PathBuf {
    let r = mp4::Reader::open(src).unwrap();
    let v = r.video().unwrap();
    let t = &r.tracks[v];
    let mut w = mkv::Writer::new(Cursor::new(Vec::new()), mkv::Options::default());
    let tb = TimeBase::new(1, t.timescale);
    let track = w
        .add_track(mkv::TrackConfig::vp9(t.width, t.height, tb))
        .unwrap();
    for (i, s) in t.samples.iter().enumerate() {
        let data = r.read(v, i).unwrap();
        let pts = t.presentation(i).unwrap();
        w.push_sample(track, &data, pts, Some(s.duration), s.keyframe)
            .unwrap();
    }
    let out = dir().join(name);
    testref::write_atomic(&out, &w.finish().unwrap().into_inner()).unwrap();
    out
}

#[test]
fn vp9_stills_follow_the_vpcc_matrix_on_the_gpu_and_the_cpu() {
    let mp4 = fixture(refs::VP9_709);
    let mkv = vp9_mkv_of(&mp4, "vp9-709.mkv");
    let cpu = Stills::open(&mp4, 240, &route_prefs(Choice::Cpu)).unwrap();
    let by_header = Stills::open(&mkv, 240, &route_prefs(Choice::Cpu)).unwrap();
    for seek in [Seek::Exact, Seek::Keyframe] {
        let mut want: Vec<Option<Rgba>> = vec![None; TIMES.len()];
        by_header
            .frames(&TIMES, seek, |i, r| want[i] = Some(r.unwrap()))
            .unwrap();
        cpu.frames(&TIMES, seek, |i, r| {
            let (g, w) = (r.unwrap(), want[i].as_ref().unwrap());
            let p = still_psnr(&g, w);
            assert!(p > 50.0, "{seek:?} CPU MP4 still {i} vs MKV: {p:.1} dB");
        })
        .unwrap();
    }
    if gpu_stills_match(&mp4, &cpu, &TIMES) {
        assert!(gpu_stills_match(&mkv, &cpu, &TIMES));
    }
}

fn matrix_still(frame: &Frame, matrix: &str, range: &str, like: &Rgba) -> Rgba {
    let (kr, kb) = if matrix == "bt709" {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    };
    let full = range == "pc";
    let at = |plane: usize, x: u32, y: u32| {
        let p = &frame.planes[plane];
        f64::from(p.data[y as usize * p.stride + x as usize])
    };
    shrink((frame.width, frame.height), like, |x, y| {
        let (l, u, v) = (at(0, x, y), at(1, x / 2, y / 2), at(2, x / 2, y / 2));
        let (l, cb, cr) = if full {
            (l / 255.0, (u - 128.0) / 255.0, (v - 128.0) / 255.0)
        } else {
            ((l - 16.0) / 219.0, (u - 128.0) / 224.0, (v - 128.0) / 224.0)
        };
        let r = l + 2.0 * (1.0 - kr) * cr;
        let b = l + 2.0 * (1.0 - kb) * cb;
        let g = (l - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b].map(|c| (c * 255.0).round().clamp(0.0, 255.0) as u8)
    })
}

fn with_header_space(src: &Path, space: u8, out: &Path) {
    let r = mkv::Reader::open(src).unwrap();
    let at = r.tracks[0].samples[0].offset as usize;
    let mut bytes = std::fs::read(src).unwrap();
    assert_eq!(
        bytes[at + 1..at + 4],
        [0x49, 0x83, 0x42],
        "{}: no VP9 keyframe sync code at {at}",
        src.display()
    );
    bytes[at + 4] = (bytes[at + 4] & 0x1f) | (space << 5);
    testref::write_atomic(out, &bytes).unwrap();
}

fn rewrapped(src: &Path, size: (u32, u32), vpcc: Option<u8>, full: bool, out: &Path) {
    use pito_codecs::{mkv, mp4};
    let r = mkv::Reader::open(src).unwrap();
    let samples = r.tracks[0].samples.clone();
    let mut buf = Vec::new();
    let tb = TimeBase::new(1, 1000);
    match vpcc {
        None => {
            let mut w = mkv::Writer::create(out).unwrap();
            let t = w
                .add_track(mkv::TrackConfig::vp9(size.0, size.1, tb))
                .unwrap();
            for (i, s) in samples.iter().enumerate() {
                r.read_into(0, i, &mut buf).unwrap();
                w.push_sample(t, &buf, i as i64 * 100, Some(100), s.keyframe)
                    .unwrap();
            }
            w.finish().unwrap();
        }
        Some(space) => {
            let config = mp4::VpcConfig::new((0, 8, 1), (space, full), size, (10, 1));
            let entry = mp4::SampleEntry::vp9(config, size.0 as u16, size.1 as u16);
            let mut w = mp4::Writer::create(out).unwrap();
            let t = w.add_track(mp4::TrackConfig::new(entry, 1000)).unwrap();
            for (i, s) in samples.iter().enumerate() {
                r.read_into(0, i, &mut buf).unwrap();
                let pts = i as i64 * 100;
                w.push_sample(t, &buf, pts, pts, 100, s.keyframe).unwrap();
            }
            w.finish().unwrap();
        }
    }
}

#[test]
fn vp9_tagged_only_in_its_keyframe_converts_with_the_header_matrix_and_range() {
    let vp9_gpu = pito_codecs::gpu::available()
        .iter()
        .any(|&(c, d, _)| c == Codec::Vp9 && d == pito_codecs::gpu::Direction::Decode);
    eprintln!("VP9 GPU decode available: {vp9_gpu}");
    for ((w, h), space, matrix, range, default) in refs::VP9_KEYS {
        let tagged = fixture(&refs::vp9_key(h, space, range));
        let name = format!("vp9-key-{h}-{space}-{range}");
        let full = range == "pc";
        let bare_mkv = dir().join(format!("{name}-bare.mkv"));
        let open_mp4 = dir().join(format!("{name}-open.mp4"));
        rewrapped(&tagged, (w, h), None, full, &bare_mkv);
        rewrapped(&tagged, (w, h), Some(0), false, &open_mp4);
        let decoded = frames_from(&tagged, 0).next().unwrap();
        let tags_win = dir().join(format!("{name}-tags-win.mkv"));
        let header = if space == "bt709" { 1 } else { 2 };
        with_header_space(&tagged, header, &tags_win);
        for path in [&bare_mkv, &open_mp4, &tags_win] {
            for decode in [Choice::Cpu, Choice::Gpu] {
                let ours = first_frame(path, 240, &route_prefs(decode.clone())).unwrap();
                let right = matrix_still(&decoded, matrix, range, &ours);
                let wrong = matrix_still(&decoded, default, "tv", &ours);
                let (good, bad) = (mean_diff(&ours, &right), mean_diff(&ours, &wrong));
                assert!(
                    good < 6.0 && bad > good + 0.5,
                    "{} {decode:?}: {good:.2} against {matrix}/{range}, {bad:.2} against {default}/tv",
                    path.display()
                );
            }
        }
        if !full {
            let other = if space == "bt709" { 1 } else { 2 };
            let wins = dir().join(format!("{name}-wins.mp4"));
            rewrapped(&tagged, (w, h), Some(other), true, &wins);
            let ours = first_frame(&wins, 240, &route_prefs(Choice::Cpu)).unwrap();
            let right = matrix_still(&decoded, matrix, range, &ours);
            let wrong = matrix_still(&decoded, default, "tv", &ours);
            let container = matrix_still(&decoded, default, "pc", &ours);
            let good = mean_diff(&ours, &container);
            for (against, lost) in [("header", &right), ("size default", &wrong)] {
                let bad = mean_diff(&ours, lost);
                assert!(
                    good < 6.0 && bad > good + 0.5,
                    "{name}: the full-range {default} vpcC lost to the {against}: {good:.2} vs {bad:.2}"
                );
            }
        }
    }
}

#[test]
fn av1_in_mp4_mkv_and_webm_probes_and_decodes_on_the_gpu_or_refuses_cleanly() {
    let prefs = Prefs::default();
    for (name, bits) in refs::AV1S {
        let path = fixture(name);
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(
            (
                p.video_codec,
                p.video_codec_name.as_str(),
                p.width,
                p.height,
                p.bit_depth,
                p.profile.as_str()
            ),
            (Some(Codec::Av1), "av1", 352, 240, bits, "Main"),
            "{name}"
        );
        assert!((p.duration - 1.0).abs() < 0.05, "{name}: {}", p.duration);
        if !p.video_native() {
            assert_eq!(p.unsupported(), ["av1"], "{name}");
            let e = first_frame(&path, 180, &prefs).unwrap_err().to_string();
            assert!(e.contains("no GPU AV1 decoder"), "{name}: {e}");
            continue;
        }
        let still = first_frame(&path, 180, &prefs).unwrap();
        assert_eq!((still.width, still.height), (264, 180), "{name}");
        let stills = frames_at_seek(&path, &[0.2, 0.7], 180, Seek::Exact, &prefs).unwrap();
        assert_eq!(stills.len(), 2, "{name}");
        let video = decode_video_gpu(&path, 0.0, None, &prefs).unwrap();
        if !video.on_gpu() {
            continue;
        }
        let mut n = 0;
        for f in video {
            assert_eq!(f.unwrap().size(), (352, 240), "{name}");
            n += 1;
        }
        assert_eq!(n, refs::AV1_FRAMES, "{name}");
    }
}

#[test]
fn a_10_bit_4_2_2_h264_is_refused_at_probe_with_its_variant_on_every_container() {
    let prefs = Prefs::default();
    for ext in ["mp4", "mkv"] {
        let path = fixture(&format!("h264-yuv422p10le.{ext}"));
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(
            (p.video_codec, p.bit_depth, p.chroma, p.interlaced),
            (Some(Codec::H264), 10, Some(Chroma::Yuv422), false),
            "{ext}"
        );
        assert_eq!(p.video_variant(), "10-bit 4:2:2 H.264", "{ext}");
        assert!(!p.video_native(), "{ext}");
        assert_eq!(p.unsupported(), ["10-bit 4:2:2 H.264"], "{ext}");
        assert_eq!(
            first_frame(&path, 120, &prefs).unwrap_err().to_string(),
            "unsupported: 10-bit 4:2:2 H.264 video is not supported",
            "{ext}"
        );
    }
}

#[test]
fn an_interlaced_h264_is_refused_at_probe_and_a_plain_one_is_native() {
    let prefs = Prefs::default();
    let interlaced = fixture("h264-interlaced.mp4");
    let p = probe(&interlaced, &prefs).unwrap();
    assert!(p.interlaced);
    assert_eq!(p.video_variant(), "interlaced 8-bit 4:2:0 H.264");
    assert!(!p.video_native());
    assert_eq!(p.unsupported(), ["interlaced 8-bit 4:2:0 H.264"]);
    let plain = Clip::new((352, 240), (30, 1), 30)
        .video_only()
        .make(AREA, "h264-plain-high.mkv");
    let p = probe(&plain, &prefs).unwrap();
    assert_eq!(
        (p.bit_depth, p.chroma, p.interlaced),
        (8, Some(Chroma::Yuv420), false)
    );
    assert_eq!(p.video_variant(), "8-bit 4:2:0 H.264");
    assert!(p.video_native());
    assert!(p.unsupported().is_empty());
}

#[test]
fn a_10_bit_4_2_2_hevc_is_refused_at_probe_and_main_10_is_native() {
    let prefs = Prefs::default();
    for ext in ["mp4", "mkv"] {
        let path = fixture(&format!("hevc-10bit-422.{ext}"));
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(
            (p.video_codec, p.bit_depth, p.chroma, p.interlaced),
            (Some(Codec::Hevc), 10, Some(Chroma::Yuv422), false),
            "{ext}"
        );
        let variant = p.video_variant();
        assert!(variant.starts_with("10-bit 4:2:2 HEVC"), "{ext}: {variant}");
        assert!(!p.video_native(), "{ext}");
        assert_eq!(p.unsupported(), [variant], "{ext}");
        let path = fixture(&format!("hevc-main10.{ext}"));
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(p.video_variant(), "10-bit 4:2:0 HEVC", "{ext}");
        assert!(p.video_native(), "{ext}");
        assert!(p.unsupported().is_empty(), "{ext}");
    }
}

#[test]
fn vp9_profiles_read_from_the_webm_keyframe_and_the_mp4_vpcc_alike() {
    let prefs = Prefs::default();
    for ext in ["webm", "mp4"] {
        let path = fixture(&format!("vp9-444.{ext}"));
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(p.video_variant(), "8-bit 4:4:4 VP9", "{ext}");
        assert!(!p.video_native(), "{ext}");
        assert_eq!(p.unsupported(), ["8-bit 4:4:4 VP9"], "{ext}");
        let path = fixture(&format!("vp9-10bit.{ext}"));
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(p.video_variant(), "10-bit 4:2:0 VP9", "{ext}");
        assert!(p.video_native(), "{ext}");
    }
}

fn said<T, E: std::fmt::Display>(r: Result<T, E>) -> String {
    match r {
        Ok(_) => panic!("opened what should be refused"),
        Err(e) => e.to_string(),
    }
}

fn refused_in_one_line<T, E: std::fmt::Display>(r: Result<T, E>, naming: &str) -> String {
    let e = said(r);
    assert_eq!(e.lines().count(), 1, "{e}");
    assert!(e.contains(naming), "{naming}: {e}");
    assert!(
        !e.replace(env!("CARGO_MANIFEST_DIR"), "")
            .to_lowercase()
            .contains("ffmpeg"),
        "{e}"
    );
    e
}

fn every_open_says(path: &Path, want: &str) {
    let prefs = Prefs::default();
    let name = path.display();
    assert_eq!(said(decode_video(path, 0.0, None, &prefs)), want, "{name}");
    assert_eq!(
        said(decode_video_gpu(path, 0.0, None, &prefs)),
        want,
        "{name}"
    );
    assert_eq!(said(first_frame(path, 120, &prefs)), want, "{name}");
    assert_eq!(said(frames_at(path, &[0.5], 120, &prefs)), want, "{name}");
    assert_eq!(said(Stills::open(path, 120, &prefs)), want, "{name}");
    let piece = Piece {
        path: path.to_path_buf(),
        from: 0.1,
        to: 0.6,
    };
    let out = dir().join(format!(
        "{}.refused.mp4",
        path.file_name().unwrap().to_string_lossy()
    ));
    assert_eq!(
        said(export(&out, &[piece], ExportOptions::default(), |_| true)),
        want,
        "{name}"
    );
    assert!(!out.exists(), "{name}");
}

fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = (8 + body.len() as u32).to_be_bytes().to_vec();
    b.extend_from_slice(kind);
    b.extend_from_slice(body);
    b
}

fn foreign_video(name: &str) -> PathBuf {
    let size = (64u16, 64u16);
    let frames: Vec<Vec<u8>> = (0..10u8)
        .map(|n| {
            let mut d = vec![0, 0, 1, 0xb6, n];
            d.resize(64, n);
            d
        })
        .collect();
    let bytes = if name.ends_with(".mkv") {
        let codec = if name.starts_with("mpeg4") {
            "V_MPEG4/ISO/ASP"
        } else if name.starts_with("mpeg2") {
            "V_MPEG2"
        } else {
            "V_MJPEG"
        };
        let track = refs::MkvTrack {
            video: true,
            codec,
            size: (u32::from(size.0), u32::from(size.1)),
            ..refs::MkvTrack::default()
        };
        let blocks: Vec<refs::MkvBlock> = frames
            .iter()
            .enumerate()
            .map(|(k, data)| refs::MkvBlock {
                track: 0,
                ms: k as i64 * 100,
                key: true,
                data: data.clone(),
                discard_ns: 0,
            })
            .collect();
        refs::matroska("matroska", &[track], &blocks, 1000.0, refs::Sizes::Known)
    } else {
        let (format, oti) = if name.starts_with("mjpeg") {
            (*b"jpeg", None)
        } else if name.starts_with("mpeg4") {
            (*b"mp4v", Some(0x20u8))
        } else {
            (*b"mp4v", Some(0x61))
        };
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
            raw.extend(mp4_box(b"esds", &[&[0, 0, 0, 0][..], &es].concat()));
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
        let brand = if name.ends_with(".mov") {
            mp4::Brand::Mov
        } else {
            mp4::Brand::Mp4
        };
        let options = mp4::Options { brand, reserve: 0 };
        let mut w = mp4::Writer::new(Cursor::new(Vec::new()), options).unwrap();
        let t = w.add_track(mp4::TrackConfig::new(entry, 10)).unwrap();
        for (k, data) in frames.iter().enumerate() {
            w.push_sample(t, data, k as i64, k as i64, 1, true).unwrap();
        }
        w.finish_faststart().unwrap().into_inner()
    };
    let out = dir().join(name);
    testref::write_atomic(&out, &bytes).unwrap();
    out
}

fn ts_packet(stamped: bool, k: usize) -> Vec<u8> {
    let mut p = if stamped {
        (k as u32 * 1800).to_be_bytes().to_vec()
    } else {
        Vec::new()
    };
    if k == 0 {
        p.extend([
            0x47, 0x40, 0x00, 0x10, 0x00, 0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00,
        ]);
        p.extend([0x00, 0x00, 0x01, 0xF0, 0x00, 0x2A, 0xB1, 0x04, 0xB2]);
    } else {
        p.extend([0x47, 0x1F, 0xFF, 0x10]);
    }
    p.resize(if stamped { 192 } else { 188 }, 0xFF);
    p
}

fn foreign_container(name: &str) -> PathBuf {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    let mut b: Vec<u8> = match ext {
        "avi" => {
            let mut b = b"RIFF".to_vec();
            b.extend(8184u32.to_le_bytes());
            b.extend_from_slice(b"AVI LIST");
            b.extend(68u32.to_le_bytes());
            b.extend_from_slice(b"hdrlavih");
            b.extend(56u32.to_le_bytes());
            b.extend(33_333u32.to_le_bytes());
            b
        }
        "ts" | "m2ts" => (0..40).flat_map(|k| ts_packet(ext == "m2ts", k)).collect(),
        "flv" => b"FLV\x01\x01\x00\x00\x00\x09\x00\x00\x00\x00".to_vec(),
        "mpg" => {
            let mut b = vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 0x01, 0x89, 0xC3, 0xF8];
            b.extend([0, 0, 1, 0xBB, 0, 12, 0x80, 0xC4, 0xE1, 0x04, 0xE1, 0xFF]);
            b.extend([0xE0, 0xE0, 0xE8, 0xBD, 0xE0, 0x20]);
            b
        }
        _ => {
            let mut b = vec![
                0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62,
                0xce, 0x6c,
            ];
            b.extend(30u64.to_le_bytes());
            b.extend(0u32.to_le_bytes());
            b.extend([1, 2]);
            b
        }
    };
    if b.len() < 8192 {
        b.resize(8192, 0);
    }
    let out = dir().join(name);
    testref::write_atomic(&out, &b).unwrap();
    out
}

#[test]
fn a_video_codec_no_decoder_reads_is_refused_at_open_by_its_plain_name() {
    let prefs = Prefs::default();
    for (name, plain) in [
        ("mpeg4-part2.mp4", "MPEG-4 Part 2"),
        ("mpeg4-part2.mkv", "MPEG-4 Part 2"),
        ("mpeg2.mp4", "MPEG-2"),
        ("mpeg2.mkv", "MPEG-2"),
        ("mjpeg.mov", "MJPEG"),
        ("mjpeg.mkv", "MJPEG"),
    ] {
        let path = foreign_video(name);
        let p = probe(&path, &prefs).unwrap();
        assert_eq!(p.video_codec, None, "{name}");
        assert!(!p.video_native(), "{name}");
        assert_eq!(p.unsupported(), [plain], "{name}");
        assert!(keyframes(&path, &prefs).is_ok(), "{name}");
        let want = format!("unsupported: {plain} video is not supported");
        refused_in_one_line(first_frame(&path, 120, &prefs), plain);
        every_open_says(&path, &want);
    }
}

#[test]
fn a_container_no_reader_takes_is_refused_by_name_at_probe_and_every_open() {
    let prefs = Prefs::default();
    for (name, container) in [
        ("mpeg4.avi", "AVI"),
        ("mpeg2.ts", "MPEG-TS"),
        ("mpeg2.m2ts", "MPEG-TS"),
        ("flv1.flv", "FLV"),
        ("mpeg2.mpg", "MPEG-PS"),
        ("wmv2.wmv", "ASF"),
    ] {
        let path = foreign_container(name);
        let e = refused_in_one_line(probe(&path, &prefs), container);
        assert!(e.starts_with("unsupported: "), "{name}: {e}");
        assert!(e.ends_with(" files are not supported"), "{name}: {e}");
        assert_eq!(said(keyframes(&path, &prefs)), e, "{name}");
        assert_eq!(said(frame_count(&path, &prefs)), e, "{name}");
        every_open_says(&path, &e);
    }
}

#[test]
fn a_file_that_is_no_media_container_is_refused_plainly() {
    let path = dir().join("not-media.txt");
    std::fs::write(
        &path,
        "these words are not a movie, and no reader should try them",
    )
    .unwrap();
    let prefs = Prefs::default();
    let want = "unsupported: this file is not an MP4, MOV, MKV or WebM file";
    assert_eq!(said(probe(&path, &prefs)), want);
    assert_eq!(said(keyframes(&path, &prefs)), want);
    every_open_says(&path, want);
}

#[test]
fn an_mp4_whose_first_box_is_442_bytes_is_no_mpeg_ps() {
    let size = (352, 240);
    let src = dir().join(format!("{}.free-442-source.mp4", std::process::id()));
    export_frames(
        &src,
        Canvas {
            width: size.0,
            height: size.1,
            fps: (30, 1),
            frames: Some(30),
        },
        ExportOptions::default(),
        |n, spent| Ok((n < 30).then(|| VideoFrame::Cpu(picture(n, size, spent)))),
        |_, frames| {
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = vec![0.0; frames * 2];
            Ok(buf)
        },
        |_| true,
    )
    .unwrap();
    let mut bytes = vec![0, 0, 1, 0xba];
    bytes.extend_from_slice(b"free");
    bytes.resize(442, 0);
    bytes.extend_from_slice(&std::fs::read(&src).unwrap());
    let _ = std::fs::remove_file(&src);
    let path = dir().join("free-442-first.mp4");
    std::fs::write(&path, &bytes).unwrap();
    let prefs = Prefs::default();
    let p = probe(&path, &prefs).unwrap();
    assert_eq!(p.video_codec, Some(Codec::H264));
    assert_eq!((p.width, p.height), size);
    assert!(p.unsupported().is_empty(), "{:?}", p.unsupported());
    assert_eq!(frame_count(&path, &prefs).unwrap(), 30);
}

fn wav(name: &str) -> PathBuf {
    let path = dir().join(name);
    let rate = 48_000u32;
    let data = vec![0u8; rate as usize * 4 / 2];
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    std::fs::write(&path, b).unwrap();
    path
}

fn foreign_audio() -> &'static Path {
    static FILE: OnceLock<PathBuf> = OnceLock::new();
    FILE.get_or_init(|| {
        let out = dir().join("aac-dts-truehd.mkv");
        if out.exists() {
            return out;
        }
        let id = std::process::id();
        let src = dir().join(format!("{id}.foreign-audio-source.mp4"));
        let size = (352, 240);
        let canvas = Canvas {
            width: size.0,
            height: size.1,
            fps: (30, 1),
            frames: Some(30),
        };
        export_frames(
            &src,
            canvas,
            ExportOptions::default(),
            |n, spent| Ok((n < 30).then(|| VideoFrame::Cpu(picture(n, size, spent)))),
            |_, frames| {
                let mut buf = AudioBuffer::new(48_000, 2);
                buf.samples = (0..frames).flat_map(|k| [tone(k), tone(k)]).collect();
                Ok(buf)
            },
            |_| true,
        )
        .unwrap();
        let r = pito_codecs::mp4::Reader::open(&src).unwrap();
        let v = r.video().unwrap();
        let a = r.audio()[0];
        let part = dir().join(format!("{id}.aac-dts-truehd.mkv"));
        let mut w = pito_codecs::mkv::Writer::create(&part).unwrap();
        let tv = w
            .add_track(pito_codecs::mkv::TrackConfig::from_mp4(&r.tracks[v]).unwrap())
            .unwrap();
        let ta = w
            .add_track(pito_codecs::mkv::TrackConfig::from_mp4(&r.tracks[a]).unwrap())
            .unwrap();
        let foreign = |codec: &str| pito_codecs::mkv::TrackConfig {
            sample_rate: 48_000,
            channels: 2,
            ..pito_codecs::mkv::TrackConfig::new(
                pito_codecs::mkv::TrackKind::Audio,
                codec,
                TimeBase::new(1, 48_000),
            )
        };
        let tf = [
            w.add_track(foreign("A_DTS")).unwrap(),
            w.add_track(foreign("A_TRUEHD")).unwrap(),
        ];
        let mut blocks: Vec<(f64, usize, Vec<u8>, i64, bool)> = Vec::new();
        for (track, t) in [(tv, v), (ta, a)] {
            let at = &r.tracks[t];
            for (i, s) in at.samples.iter().enumerate() {
                let seconds = s.dts as f64 / at.timescale.max(1) as f64;
                blocks.push((seconds, track, r.read(t, i).unwrap(), s.pts, s.keyframe));
            }
        }
        for track in tf {
            for k in 0..48i64 {
                let seconds = (k * 1024) as f64 / 48_000.0;
                blocks.push((seconds, track, vec![0x55; 64], k * 1024, true));
            }
        }
        blocks.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (_, track, data, pts, key) in blocks {
            w.push_sample(track, &data, pts, None, key).unwrap();
        }
        w.finish().unwrap();
        std::fs::rename(&part, &out).unwrap();
        let _ = std::fs::remove_file(&src);
        out
    })
}

#[test]
fn an_audio_track_no_decoder_reads_is_named_on_every_path_and_a_muted_one_is_skipped() {
    let path = foreign_audio();
    let prefs = Prefs::default();
    let p = probe(path, &prefs).unwrap();
    assert!(p.video_native());
    assert_eq!(p.audio.len(), 3);
    assert_eq!(p.audio[0].codec, "aac");
    assert_eq!(p.unsupported(), ["DTS", "TrueHD"]);
    let whole = audio_pcm(path, 0, 48_000, false, &prefs).unwrap();
    assert!(!whole.samples.is_empty());
    for (track, name) in [(1, "DTS"), (2, "TrueHD")] {
        assert_eq!(
            said(audio_pcm(path, track, 48_000, false, &prefs)),
            format!("unsupported: {name} audio is not supported")
        );
        refused_in_one_line(audio_pcm(path, track, 48_000, false, &prefs), name);
        refused_in_one_line(
            audio_pcm_range(path, track, 0.2, 0.6, 48_000, 2, &prefs),
            name,
        );
        refused_in_one_line(audio_stream(path, track, 0.2, 48_000, 2, &prefs), name);
    }
    let either = |e: String| {
        assert_eq!(e.lines().count(), 1, "{e}");
        assert!(e.contains("DTS") || e.contains("TrueHD"), "{e}");
        assert!(
            !e.replace(env!("CARGO_MANIFEST_DIR"), "")
                .to_lowercase()
                .contains("ffmpeg"),
            "{e}"
        );
    };
    either(said(audio_mix(path, &[], 0.0, 0.5, 48_000, &prefs)));
    either(said(audio_mix_stream(path, &[], 0.0, 48_000, &prefs)));
    either(said(audio_mix(
        path,
        &[1.0, 0.5, 0.0],
        0.0,
        0.5,
        48_000,
        &prefs,
    )));
    let muted = [1.0, 0.0, 0.0];
    let mixed = audio_mix(path, &muted, 0.0, 0.5, 48_000, &prefs).unwrap();
    assert_eq!(mixed.samples.len(), 24_000 * 2);
    assert!(audio_mix_stream(path, &muted, 0.0, 48_000, &prefs).is_ok());
    let piece = || Piece {
        path: path.to_path_buf(),
        from: 0.0,
        to: 0.5,
    };
    let out = dir().join("foreign-audio-export.mp4");
    let _ = std::fs::remove_file(&out);
    either(said(export(
        &out,
        &[piece()],
        ExportOptions::default(),
        |_| true,
    )));
    let opts = ExportOptions {
        audio_gains: muted.to_vec(),
        ..ExportOptions::default()
    };
    let done = export(&out, &[piece()], opts, |_| true).unwrap();
    assert!(done.frames > 0);
    let back = probe(&out, &prefs).unwrap();
    assert_eq!(back.audio.len(), 1);
    assert_eq!(back.audio[0].codec, "aac");
    assert!(back.unsupported().is_empty(), "{:?}", back.unsupported());
}

#[test]
fn export_refuses_a_gained_track_no_decoder_reads_before_any_work() {
    let path = foreign_audio();
    let long: Vec<Piece> = (0..20)
        .map(|_| Piece {
            path: path.to_path_buf(),
            from: 0.0,
            to: 1.0,
        })
        .collect();
    let out = dir().join("foreign-audio-long.mp4");
    let _ = std::fs::remove_file(&out);
    let parts = out.with_extension("parts");
    let _ = std::fs::remove_dir_all(&parts);
    let mut told = 0;
    let opts = ExportOptions {
        audio_gains: vec![1.0, 0.5, 0.0],
        ..ExportOptions::default()
    };
    assert_eq!(
        said(export(&out, &long, opts, |_| {
            told += 1;
            true
        })),
        "unsupported: DTS audio is not supported"
    );
    assert_eq!(told, 0);
    assert!(!out.exists());
    assert!(!parts.exists());

    let twin = dir().join("aac-dts-truehd-twin.mkv");
    std::fs::copy(path, &twin).unwrap();
    let both: Vec<Piece> = (0..10)
        .flat_map(|_| [path, twin.as_path()])
        .map(|p| Piece {
            path: p.to_path_buf(),
            from: 0.0,
            to: 1.0,
        })
        .collect();
    let opts = ExportOptions {
        audio_gains: vec![1.0, 0.0, 0.7],
        ..ExportOptions::default()
    };
    assert_eq!(
        said(export(&out, &both, opts, |_| {
            told += 1;
            true
        })),
        format!(
            "unsupported: {}: TrueHD audio is not supported",
            path.display()
        )
    );
    assert_eq!(told, 0);
    assert!(!out.exists());
    assert!(!parts.exists());

    let premixed = dir().join("foreign-audio-premixed.mp4");
    let _ = std::fs::remove_file(&premixed);
    let done = export_with_audio(
        &premixed,
        &long[..2],
        ExportOptions::default(),
        |_, frames| {
            let mut buf = AudioBuffer::new(48_000, 2);
            buf.samples = (0..frames).flat_map(|k| [tone(k), tone(k)]).collect();
            Ok(buf)
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(done.frames, 60);
    let back = probe(&premixed, &Prefs::default()).unwrap();
    assert_eq!(back.audio.len(), 1);
    assert!(back.unsupported().is_empty(), "{:?}", back.unsupported());
}

#[test]
fn a_file_with_no_video_says_so_on_every_video_path() {
    let path = wav("audio-only.wav");
    let prefs = Prefs::default();
    let p = probe(&path, &prefs).unwrap();
    assert_eq!(p.video_codec, None);
    assert!(p.unsupported().is_empty(), "{:?}", p.unsupported());
    assert_eq!(
        audio_pcm(&path, 0, 48_000, false, &prefs).unwrap().frames(),
        24_000
    );
    let mono = audio_pcm(&path, 0, 48_000, true, &prefs).unwrap();
    assert_eq!((mono.channels, mono.frames()), (1, 24_000));
    let span = audio_pcm_range(&path, 0, 0.1, 0.3, 48_000, 2, &prefs).unwrap();
    assert_eq!(span.frames(), 9_600);
    assert!(
        audio_pcm_range(&path, 0, 0.2, 0.2, 48_000, 2, &prefs)
            .unwrap()
            .samples
            .is_empty()
    );
    assert_eq!(
        audio_mix(&path, &[], 0.0, 0.25, 48_000, &prefs)
            .unwrap()
            .frames(),
        12_000
    );
    every_open_says(&path, "invalid data: no video track");
}

#[test]
fn a_10_bit_or_4_2_2_h264_is_refused_when_opened_not_at_its_first_frame() {
    for ext in ["mp4", "mkv"] {
        for (pix, variant) in [
            ("yuv422p10le", "10-bit 4:2:2 H.264"),
            ("yuv420p10le", "10-bit 4:2:0 H.264"),
        ] {
            let path = fixture(&format!("h264-{pix}.{ext}"));
            every_open_says(
                &path,
                &format!("unsupported: {variant} video is not supported"),
            );
        }
    }
}

#[test]
fn a_10_bit_4_2_2_h264_exports_when_every_part_is_copied_and_is_refused_by_file_when_one_needs_a_decode()
 {
    let mp4 = fixture("h264-yuv422p10le.mp4");
    let mkv = fixture("h264-yuv422p10le.mkv");
    let prefs = Prefs::default();
    let whole = probe(&mp4, &prefs).unwrap().duration;
    let out = dir().join("h264-422-10-copied.mp4");
    let piece = Piece {
        path: mp4.clone(),
        from: 0.0,
        to: whole,
    };
    let done = export(&out, &[piece], ExportOptions::default(), |_| true).unwrap();
    assert!(done.smart);
    assert_eq!(done.encoded, 0);
    assert!(done.copied > 0);
    let p = probe(&out, &prefs).unwrap();
    assert_eq!(p.video_variant(), "10-bit 4:2:2 H.264");
    assert!(!p.video_native());

    let out = dir().join("h264-422-10-two-files.refused.mp4");
    let pieces = [
        Piece {
            path: mp4.clone(),
            from: 0.1,
            to: 0.6,
        },
        Piece {
            path: mkv.clone(),
            from: 0.1,
            to: 0.6,
        },
    ];
    let e = said(export(&out, &pieces, ExportOptions::default(), |_| true));
    assert_eq!(
        e,
        format!(
            "unsupported: {}: 10-bit 4:2:2 H.264 video is not supported",
            mp4.display()
        )
    );
    assert!(!out.exists());

    let out = dir().join("h264-422-10-one-file.refused.mp4");
    let piece = Piece {
        path: mp4,
        from: 0.1,
        to: 0.6,
    };
    assert_eq!(
        said(export(&out, &[piece], ExportOptions::default(), |_| true)),
        "unsupported: 10-bit 4:2:2 H.264 video is not supported"
    );
    assert!(!out.exists());
}

#[test]
fn a_vp9_our_encoder_cannot_splice_is_re_encoded_whole_even_on_gop_bounds() {
    let src = fixture(refs::VP9_PROFILE2);
    let prefs = Prefs::default();
    let p = probe(&src, &prefs).unwrap();
    assert_eq!((p.video_codec, p.bit_depth), (Some(Codec::Vp9), 10));
    assert!(p.video_native(), "{}", p.video_variant());
    let opts = || ExportOptions {
        video: Codec::Vp9,
        ..ExportOptions::default()
    };
    for (name, from, to, frames) in [("gops", 0.0, 20.0 / 30.0, 20), ("cut", 0.1, 0.6, 15)] {
        let out = dir().join(format!("vp9-profile2-{name}.webm"));
        let piece = Piece {
            path: src.clone(),
            from,
            to,
        };
        let done = export(&out, &[piece], opts(), |_| true).unwrap();
        assert!(!done.smart, "{name}: {:?}", done.reasons);
        assert_eq!((done.copied, done.encoded), (0, frames), "{name}");
        assert!(
            done.reasons.iter().any(|r| r
                == "the source VP9 is Profile 2 at 10 bits, our VP9 encoder writes profile 0 at 8 bits, so the whole export is re-encoded"),
            "{name}: {:?}",
            done.reasons
        );
        let back = probe(&out, &prefs).unwrap();
        assert_eq!(
            (back.video_codec, back.bit_depth, back.profile.as_str()),
            (Some(Codec::Vp9), 8, "Profile 0"),
            "{name}"
        );
    }
}

#[test]
fn a_4_4_4_vp9_with_alpha_is_refused_when_its_stills_open() {
    let src = fixture("vp9-444.webm");
    let r = pito_codecs::mkv::Reader::open(&src).unwrap();
    let v = r.video().unwrap();
    let path = dir().join("vp9-444-alpha.webm");
    let mut w =
        pito_codecs::mkv::Writer::create_with(&path, pito_codecs::mkv::Options::webm()).unwrap();
    let mut cfg = pito_codecs::mkv::TrackConfig::from_track(&r.tracks[v]);
    cfg.alpha = true;
    let t = w.add_track(cfg).unwrap();
    for (i, s) in r.tracks[v].samples.iter().enumerate() {
        let data = r.read(v, i).unwrap();
        w.push_block(t, &data, s.pts, None, s.keyframe, Some(&data))
            .unwrap();
    }
    w.finish().unwrap();
    let prefs = Prefs::default();
    let p = probe(&path, &prefs).unwrap();
    assert_eq!(p.video_variant(), "8-bit 4:4:4 VP9");
    let want = "unsupported: 8-bit 4:4:4 VP9 video is not supported";
    assert_eq!(said(Stills::open(&path, 120, &prefs)), want);
    assert_eq!(said(first_frame(&path, 120, &prefs)), want);
}

#[test]
fn a_bare_mp3_decodes_on_every_audio_path_with_no_ffmpeg_route() {
    let path = fixture(refs::MP3);
    let prefs = Prefs::default();
    let p = probe(&path, &prefs).unwrap();
    assert_eq!(p.video_codec, None);
    assert_eq!(p.audio.len(), 1);
    let whole = audio_pcm(&path, 0, 48_000, false, &prefs).unwrap();
    assert!(
        whole.frames().abs_diff(48_000) < 2_400,
        "{}",
        whole.frames()
    );
    let peak = whole.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.05, "{peak}");
    let span = audio_pcm_range(&path, 0, 0.25, 0.75, 48_000, 1, &prefs).unwrap();
    assert_eq!((span.channels, span.frames()), (1, 24_000));
    let mixed = audio_mix(&path, &[], 0.0, 0.5, 48_000, &prefs).unwrap();
    assert_eq!(mixed.frames(), 24_000);
    assert!(audio_pcm(&path, 1, 48_000, false, &prefs).is_err());
}
