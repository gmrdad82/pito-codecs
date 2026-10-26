use crate::{Def, OURS, sha, tool, words};

#[path = "../../src/media/refs.rs"]
mod refs;

use crate::containers::mux::{self, Csp, Timed};
use crate::testref::{Sound, pcm};
use pito_codecs::mp4;
use refs::{AREA, MkvBlock, MkvTrack, Sizes};

use crate::testref::{AudioCodec, Clip};

fn s16le(samples: &[f32]) -> Vec<u8> {
    crate::testref::s16_of(samples)
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect()
}

fn lineup_tone(i: u64, ch: u16, rate: u32) -> f32 {
    let hz = if ch == 0 { 440.0 } else { 1250.0 };
    let t = i as f64 / f64::from(rate);
    (0.125 * (std::f64::consts::TAU * hz * t).sin()) as f32
}

fn off_ms_tone(i: u64, _: u16, rate: u32) -> f32 {
    let t = i as f64 / f64::from(rate);
    (0.125 * (std::f64::consts::TAU * 440.0 * t).sin()) as f32
}

fn off_ms_source(rate: u32) -> Vec<f32> {
    pcm(
        Sound::Fn(off_ms_tone),
        rate,
        2,
        0,
        refs::off_ms_frames(rate),
    )
}

const SMPTE: [[u8; 3]; 7] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
];

fn bars_at(w: u32, h: u32, x: u32, y: u32) -> [u8; 3] {
    if y < h * 2 / 3 {
        return SMPTE[(x * 7 / w).min(6) as usize];
    }
    match x * 6 / w {
        0 => [0, 33, 76],
        1 => [255, 255, 255],
        2 => [50, 0, 106],
        _ => [16, 16, 16],
    }
}

fn av1_stream(bits: u8) -> String {
    format!("av1-{bits}.ivf")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chroma {
    C420,
    C422,
    C444,
}

#[derive(Clone, Copy, Debug)]
enum Variant {
    X264(Chroma, u8, &'static str),
    X264Interlaced,
    X265(Chroma, u8),
    Vpx(Chroma, u8),
}

const VARIANTS: [(&str, Variant); 13] = [
    (
        "h264-yuv422p10le.mp4",
        Variant::X264(Chroma::C422, 10, "high422"),
    ),
    (
        "h264-yuv422p10le.mkv",
        Variant::X264(Chroma::C422, 10, "high422"),
    ),
    (
        "h264-yuv420p10le.mp4",
        Variant::X264(Chroma::C420, 10, "high10"),
    ),
    (
        "h264-yuv420p10le.mkv",
        Variant::X264(Chroma::C420, 10, "high10"),
    ),
    ("h264-interlaced.mp4", Variant::X264Interlaced),
    ("hevc-10bit-422.mp4", Variant::X265(Chroma::C422, 10)),
    ("hevc-10bit-422.mkv", Variant::X265(Chroma::C422, 10)),
    ("hevc-main10.mp4", Variant::X265(Chroma::C420, 10)),
    ("hevc-main10.mkv", Variant::X265(Chroma::C420, 10)),
    ("vp9-444.webm", Variant::Vpx(Chroma::C444, 8)),
    ("vp9-444.mp4", Variant::Vpx(Chroma::C444, 8)),
    ("vp9-10bit.webm", Variant::Vpx(Chroma::C420, 10)),
    ("vp9-10bit.mp4", Variant::Vpx(Chroma::C420, 10)),
];

fn variant_stream(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    format!("{stem}.stream")
}

fn mp3_source() -> Vec<f32> {
    pcm(
        Sound::Fn(off_ms_tone),
        refs::MP3_RATE,
        2,
        0,
        refs::MP3_RATE as usize,
    )
}

fn audio_blocks(
    packets: &[Vec<u8>],
    frame: usize,
    rate: u32,
    shift: usize,
    keep: usize,
) -> Vec<MkvBlock> {
    let total = packets.len() * frame;
    let left = total.saturating_sub(shift + keep);
    packets
        .iter()
        .enumerate()
        .map(|(k, data)| {
            let at = (k * frame) as i64 - shift as i64;
            let last = k + 1 == packets.len();
            MkvBlock {
                track: 0,
                ms: (at.max(0) as f64 * 1000.0 / f64::from(rate)).round() as i64,
                key: true,
                data: data.clone(),
                discard_ns: if last {
                    (left as f64 * 1e9 / f64::from(rate)).round() as i64
                } else {
                    0
                },
            }
        })
        .collect()
}

fn text<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn ours(file: &str, what: &str, make: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
    Def::fixture(AREA, file.into(), OURS, words(what)).from(make)
}

fn window(name: String, what: &str, make: impl Fn() -> Result<Vec<u8>, String> + 'static) -> Def {
    Def::tolerance(AREA, name, OURS, words(what)).from(make)
}

fn csp(c: Chroma) -> Csp {
    match c {
        Chroma::C420 => Csp::I420,
        Chroma::C422 => Csp::I422,
        Chroma::C444 => Csp::I444,
    }
}

fn lineup_clip(codec: AudioCodec) -> Clip {
    Clip {
        audio: Some(codec),
        sounds: vec![Sound::Fn(lineup_tone)],
        rate: refs::LINEUP_RATE,
        channels: 2,
        kbps: refs::LINEUP_KBPS,
        ..Clip::new((64, 64), (1, 1), 1)
    }
    .audio_only()
}

fn delay_frames(ns: u64, rate: f64) -> usize {
    (ns as f64 * rate / 1e9).round() as usize
}

fn opus_decoded(file: &'static str) -> Result<Vec<f32>, String> {
    let t = mux::first_audio(&mux::work(AREA, file))?;
    let all = mux::libopus_decode(2, &t.packets)?;
    let skip = delay_frames(t.codec_delay, 48_000.0) * 2;
    let drop = delay_frames(t.discard.max(0) as u64, 48_000.0) * 2;
    let end = all.len().saturating_sub(drop);
    Ok(all.get(skip..end).unwrap_or_default().to_vec())
}

fn faad_decoded(file: &str) -> Result<Vec<f32>, String> {
    let t = mux::first_audio(&mux::work(AREA, file))?;
    let all = crate::testref::le_f32(&mux::read(AREA, &refs::decoded(file))?);
    let skip = delay_frames(t.codec_delay, t.rate) * 2;
    Ok(all.get(skip..).unwrap_or_default().to_vec())
}

fn lineups(defs: &mut Vec<Def>) {
    defs.push(ours(
        refs::LINEUP_AAC,
        "testref::Clip, our AAC at 96k, 440 and 1250 Hz",
        || mux::clip(&lineup_clip(AudioCodec::Aac), AREA, refs::LINEUP_AAC),
    ));
    defs.push(
        Def::digest(
            AREA,
            refs::decoded(refs::LINEUP_AAC),
            "faad",
            tool(
                "faad",
                &["-q", "-f", "2", "-b", "4", "-o", "{out}", "{src}"],
            ),
            sha,
        )
        .from(|| {
            let t = mux::first_audio(&mux::work(AREA, refs::LINEUP_AAC))?;
            Ok(mux::adts(&t.private, &t.packets))
        }),
    );
    defs.push(ours(
        refs::LINEUP_OPUS,
        "testref::Clip, our Opus at 96k, 440 and 1250 Hz",
        || mux::clip(&lineup_clip(AudioCodec::Opus), AREA, refs::LINEUP_OPUS),
    ));
    for (k, at) in refs::LINEUP_AT.into_iter().enumerate() {
        let what = format!(
            "faad's decode of {{ref:{}}} after its CodecDelay, {} frames from {at}",
            refs::decoded(refs::LINEUP_AAC),
            refs::WINDOW
        );
        defs.push(window(
            refs::window(refs::LINEUP_AAC, &format!("at{k}")),
            &what,
            move || mux::cut(&faad_decoded(refs::LINEUP_AAC)?, 2, at, refs::WINDOW),
        ));
        let what = format!(
            "libopus's decode of {{ref:{}}} after its pre-skip, {} frames from {at}",
            refs::LINEUP_OPUS,
            refs::WINDOW
        );
        defs.push(
            window(
                refs::window(refs::LINEUP_OPUS, &format!("at{k}")),
                &what,
                move || mux::cut(&opus_decoded(refs::LINEUP_OPUS)?, 2, at, refs::WINDOW),
            )
            .library("libopus"),
        );
    }
}

fn opus_off_ms(webm: bool) -> Result<Vec<u8>, String> {
    use pito_codecs::media::AudioBuffer;
    use pito_codecs::opus::enc::{Encoder, EncoderConfig};
    let mut enc = Encoder::new(EncoderConfig::new(2, refs::OFF_MS_KBPS * 1000)).map_err(text)?;
    let source = off_ms_source(48_000);
    let buf = AudioBuffer {
        samples: source,
        rate: 48_000,
        channels: 2,
    };
    let mut packets: Vec<Vec<u8>> = enc
        .encode(&buf)
        .map_err(text)?
        .into_iter()
        .map(|p| p.data)
        .collect();
    packets.extend(enc.flush().map_err(text)?.into_iter().map(|p| p.data));
    let skip = usize::from(enc.pre_skip());
    let frame = enc.frame_samples();
    let blocks = audio_blocks(&packets, frame, 48_000, skip, refs::off_ms_frames(48_000));
    let track = MkvTrack {
        codec: "A_OPUS",
        private: enc.codec_private(),
        rate: 48_000.0,
        channels: 2,
        delay_ns: (skip as f64 * 1e9 / 48_000.0).round() as u64,
        preroll_ns: 80_000_000,
        ..MkvTrack::default()
    };
    Ok(off_ms_file(
        if webm { "webm" } else { "matroska" },
        track,
        blocks,
        48_000,
    ))
}

fn off_ms_file(doctype: &str, track: MkvTrack, blocks: Vec<MkvBlock>, rate: u32) -> Vec<u8> {
    let ms = refs::off_ms_frames(rate) as f64 * 1000.0 / f64::from(rate);
    refs::matroska(doctype, &[track], &blocks, ms, Sizes::Known)
}

fn ac3_off_ms() -> Result<Vec<u8>, String> {
    let frames = refs::off_ms_frames(48_000).div_ceil(refs::AC3_FRAME);
    let packets = refs::ac3_silence(frames);
    let blocks = audio_blocks(
        &packets,
        refs::AC3_FRAME,
        48_000,
        0,
        refs::off_ms_frames(48_000),
    );
    let track = MkvTrack {
        codec: "A_AC3",
        rate: 48_000.0,
        channels: 2,
        ..MkvTrack::default()
    };
    Ok(off_ms_file("matroska", track, blocks, 48_000))
}

fn mp3_frames(data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    const KBPS: [u32; 15] = [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ];
    let mut at = 0;
    let mut out = Vec::new();
    while at + 4 <= data.len() {
        let h = &data[at..at + 4];
        if h[0] != 0xFF || h[1] & 0xFE != 0xFA {
            return Err(format!("no MPEG-1 layer III header at byte {at}"));
        }
        let kbps = KBPS
            .get(usize::from(h[2] >> 4))
            .copied()
            .filter(|&k| k > 0)
            .ok_or("a free or bad MP3 bitrate")?;
        if h[2] >> 2 & 3 != 0 {
            return Err("an MP3 frame not at 44.1 kHz".into());
        }
        let len = (144_000 * kbps / 44_100 + u32::from(h[2] >> 1 & 1)) as usize;
        out.push(
            data.get(at..at + len)
                .ok_or("an MP3 frame cut short")?
                .to_vec(),
        );
        at += len;
    }
    Ok(out)
}

fn mp3_off_ms() -> Result<Vec<u8>, String> {
    let rate = 44_100;
    let keep = refs::off_ms_frames(rate);
    let mut packets = mp3_frames(&mux::read(AREA, refs::OFF_MS_MP3)?)?;
    packets.truncate(keep.div_ceil(1152));
    let blocks = audio_blocks(&packets, 1152, rate, 0, keep);
    let track = MkvTrack {
        codec: "A_MPEG/L3",
        rate: f64::from(rate),
        channels: 2,
        ..MkvTrack::default()
    };
    Ok(off_ms_file("matroska", track, blocks, rate))
}

fn lame(kbps: &str) -> Vec<String> {
    tool(
        "lame",
        &[
            "-r",
            "-s",
            "44.1",
            "--resample",
            "44.1",
            "--bitwidth",
            "16",
            "--little-endian",
            "-m",
            "j",
            "--cbr",
            "-b",
            kbps,
            "-t",
            "--quiet",
            "{src}",
            "{out}",
        ],
    )
}

fn off_ms(defs: &mut Vec<Def>) {
    defs.push(ours(
        "off-ms-opus.webm",
        "our Opus at 32k of a 1.0123 s sine, the last block's DiscardPadding to its end",
        || opus_off_ms(true),
    ));
    defs.push(ours(
        "off-ms-opus.mkv",
        "our Opus at 32k of a 1.0123 s sine, the last block's DiscardPadding to its end",
        || opus_off_ms(false),
    ));
    defs.push(ours(
        "off-ms-ac3.mkv",
        "silent AC-3 frames from src/media/refs.rs, the last block's DiscardPadding to 1.0123 s",
        ac3_off_ms,
    ));
    defs.push(
        Def::digest(AREA, refs::OFF_MS_MP3.into(), "lame", lame("32"), sha)
            .from(|| Ok(s16le(&off_ms_source(44_100)))),
    );
    defs.push(ours(
        "off-ms-mp3.mkv",
        "{ref:off-ms-mp3.mp3}'s frames up to 1.0123 s, the last block's DiscardPadding to its end",
        mp3_off_ms,
    ));
    defs.push(Def::digest(
        AREA,
        refs::decoded(refs::OFF_MS_MP3),
        "mpg123",
        tool(
            "mpg123",
            &[
                "-q",
                "--no-gapless",
                "-e",
                "f32",
                "-O",
                "{out}",
                &format!("{{ref:{}}}", refs::OFF_MS_MP3),
            ],
        ),
        sha,
    ));
    for o in refs::OFF_MS.iter().filter(|o| !o.silent) {
        let (file, rate) = (o.file, o.rate);
        let frames = refs::off_ms_frames(rate);
        for (at, from) in [("head", refs::OFF_MS_HEAD), ("tail", frames - refs::WINDOW)] {
            let name = refs::window(file, at);
            let def = if file.ends_with(".mkv") && file.contains("mp3") {
                let what = format!(
                    "mpg123's decode of {{ref:{}}}, {} frames from {from}",
                    refs::decoded(refs::OFF_MS_MP3),
                    refs::WINDOW
                );
                window(name, &what, move || {
                    let all =
                        crate::testref::le_f32(&mux::read(AREA, &refs::decoded(refs::OFF_MS_MP3))?);
                    mux::cut(&all, 2, from, refs::WINDOW)
                })
            } else {
                let what = format!(
                    "libopus's decode of {{ref:{file}}} after its pre-skip and discard, {} frames from {from}",
                    refs::WINDOW
                );
                window(name, &what, move || {
                    mux::cut(&opus_decoded(file)?, 2, from, refs::WINDOW)
                })
                .library("libopus")
            };
            defs.push(def);
        }
    }
}

fn vp9_altref() -> Def {
    let size = refs::VP9_ALTREF_SIZE;
    Def::fixture(
        AREA,
        refs::VP9_ALTREF.into(),
        "vpxenc",
        tool(
            "vpxenc",
            &[
                "--codec=vp9",
                "--disable-warning-prompt",
                "--debug",
                "--good",
                "--cpu-used=4",
                "--threads=1",
                "--auto-alt-ref=1",
                "--lag-in-frames=16",
                "--end-usage=vbr",
                "--target-bitrate=60",
                "--kf-min-dist=10",
                "--kf-max-dist=10",
                "--webm",
                "--quiet",
                "-o",
                "{out}",
                "{src}",
            ],
        ),
    )
    .from(move || {
        Ok(mux::y4m(
            size,
            (30, 1),
            Csp::I420,
            8,
            refs::VP9_ALTREF_FRAMES,
            &mux::pattern(size),
        ))
    })
}

fn x265(extra: &[&str], size: (u32, u32), fps: u32, csp: Csp, bits: u8) -> Vec<String> {
    let res = format!("{}x{}", size.0, size.1);
    let fps = fps.to_string();
    let depth = bits.to_string();
    let mut args = vec![
        "--input",
        "{src}",
        "--input-res",
        &res,
        "--fps",
        &fps,
        "--input-csp",
        csp.x26x(),
        "--input-depth",
        &depth,
        "--output-depth",
        &depth,
        "--frame-threads",
        "1",
        "--no-wpp",
        "--pools",
        "none",
        "--log-level",
        "error",
        "--no-info",
        "--no-progress",
    ];
    args.extend_from_slice(extra);
    tool("x265", &args)
}

fn pyramid(defs: &mut Vec<Def>) {
    let size = refs::PYRAMID_SIZE;
    let csv = format!("{}.csv", refs::PYRAMID);
    let stream = format!("{}.hevc", refs::PYRAMID);
    let frames = refs::PYRAMID_FRAMES.to_string();
    defs.push(
        Def::digest(
            AREA,
            refs::pyramid_recon(),
            "x265",
            x265(
                &[
                    "--frames",
                    &frames,
                    "--preset",
                    "ultrafast",
                    "--keyint",
                    "60",
                    "--min-keyint",
                    "60",
                    "--no-scenecut",
                    "--bframes",
                    "4",
                    "--b-pyramid",
                    "--crf",
                    "35",
                    "--csv",
                    &csv,
                    "--csv-log-level",
                    "1",
                    "--recon",
                    "{out}",
                    "--output",
                    &stream,
                ],
                size,
                30,
                Csp::I420,
                8,
            ),
            sha,
        )
        .from(move || {
            Ok(mux::raw_video(
                size,
                Csp::I420,
                8,
                refs::PYRAMID_FRAMES as u64,
                &mux::pattern(size),
            ))
        }),
    );
    defs.push(ours(
        refs::PYRAMID,
        "x265's stream and csv POCs from the pyramid-hevc.mp4.yuv run, in our MP4 writer as hvc1",
        move || {
            let coded = mux::hevc(&mux::read(AREA, &stream)?)?;
            let pocs = mux::x265_pocs(&mux::read(AREA, &csv)?)?;
            Timed {
                coded: &coded,
                pocs: &pocs,
                size,
                fps: 30,
            }
            .hevc_mp4(mp4::Brand::Mp4)
        },
    ));
}

struct Svt {
    name: String,
    bits: u8,
    size: (u32, u32),
    fps: u32,
    frames: u64,
}

fn svt(s: Svt, extra: &[&str]) -> Def {
    let n = s.frames.to_string();
    let fps = s.fps.to_string();
    let mut args = vec![
        "-i",
        "{src}",
        "-b",
        "{out}",
        "--preset",
        "12",
        "--crf",
        "55",
        "--keyint",
        &fps,
        "--lp",
        "1",
        "-n",
        &n,
        "--progress",
        "0",
    ];
    args.extend_from_slice(extra);
    let Svt {
        name,
        bits,
        size,
        fps,
        frames,
    } = s;
    Def::digest(AREA, name, "SvtAv1EncApp", tool("SvtAv1EncApp", &args), sha).from(move || {
        Ok(mux::y4m(
            size,
            (fps, 1),
            Csp::I420,
            bits,
            frames,
            &mux::pattern(size),
        ))
    })
}

fn av1_timed(stream: &str) -> Result<(mux::Coded, Vec<i64>), String> {
    let coded = mux::av1(&mux::ivf(&mux::read(AREA, stream)?)?)?;
    let pocs = (0..coded.samples.len() as i64).collect();
    Ok((coded, pocs))
}

fn av1s(defs: &mut Vec<Def>) {
    let size = refs::AV1_SIZE;
    let frames = refs::AV1_FRAMES as u64;
    for bits in [8, 10] {
        defs.push(svt(
            Svt {
                name: av1_stream(bits),
                bits,
                size,
                fps: 10,
                frames,
            },
            &[],
        ));
    }
    for (file, bits) in refs::AV1S {
        let what = format!("{{ref:{}}} in our writer", av1_stream(bits));
        defs.push(ours(file, &what, move || {
            let (coded, pocs) = av1_timed(&av1_stream(bits))?;
            let t = Timed {
                coded: &coded,
                pocs: &pocs,
                size,
                fps: 10,
            };
            match file.rsplit_once('.').map(|(_, e)| e) {
                Some("mp4") => t.av1_mp4(),
                Some("webm") => t.av1_mkv(true),
                _ => t.av1_mkv(false),
            }
        }));
    }
}

fn tagged(defs: &mut Vec<Def>) {
    let size = refs::TAGGED_SIZE;
    let frames = refs::TAGGED_FRAMES;
    let svt = svt(
        Svt {
            name: format!("{}.ivf", refs::AV1_709),
            bits: 8,
            size,
            fps: 30,
            frames,
        },
        &[
            "--color-primaries",
            "1",
            "--transfer-characteristics",
            "1",
            "--matrix-coefficients",
            "1",
        ],
    );
    defs.push(svt);
    defs.push(ours(
        refs::AV1_709,
        "{ref:av1-709.mp4.ivf}, BT.709 in its sequence header, in our MP4 writer",
        move || {
            let (coded, pocs) = av1_timed(&format!("{}.ivf", refs::AV1_709))?;
            Timed {
                coded: &coded,
                pocs: &pocs,
                size,
                fps: 30,
            }
            .av1_mp4()
        },
    ));
    let webm = format!("{}.webm", refs::VP9_709);
    defs.push(
        Def::digest(
            AREA,
            webm.clone(),
            "vpxenc",
            tool(
                "vpxenc",
                &[
                    "--codec=vp9",
                    "--disable-warning-prompt",
                    "--debug",
                    "--rt",
                    "--cpu-used=8",
                    "--threads=1",
                    "--end-usage=cbr",
                    "--target-bitrate=100",
                    "--kf-max-dist=30",
                    "--color-space=bt709",
                    "--webm",
                    "--quiet",
                    "-o",
                    "{out}",
                    "{src}",
                ],
            ),
            sha,
        )
        .from(move || {
            Ok(mux::y4m(
                size,
                (30, 1),
                Csp::I420,
                8,
                frames,
                &mux::pattern(size),
            ))
        }),
    );
    defs.push(ours(
        refs::VP9_709,
        "{ref:vp9-709.mp4.webm}'s frames in our MP4 writer, vpcC BT.709",
        move || vp9_mp4(&webm, (0, 8, 1), 2),
    ));
}

fn vp9_mp4(webm: &str, format: (u8, u8, u8), space: u8) -> Result<Vec<u8>, String> {
    let mux::Webm { coded, size, pocs } = mux::webm_samples(&mux::work(AREA, webm))?;
    let fps = 30;
    let config = mp4::VpcConfig::new(format, (space, false), size, (fps, 1));
    Timed {
        coded: &coded,
        pocs: &pocs,
        size,
        fps,
    }
    .vp9_mp4(config)
}

fn with_range(src: &[u8], full: bool) -> Result<Vec<u8>, String> {
    let path = mux::work(AREA, "range-probe.webm");
    std::fs::write(&path, src).map_err(text)?;
    let r = pito_codecs::mkv::Reader::open(&path).map_err(text)?;
    let at = r.tracks[0].samples.first().ok_or("no VP9 frame")?.offset as usize;
    let mut bytes = src.to_vec();
    if bytes.get(at + 1..at + 4) != Some(&[0x49, 0x83, 0x42]) {
        return Err(format!("no VP9 keyframe sync code at {at}"));
    }
    if full {
        bytes[at + 4] |= 0x10;
    } else {
        bytes[at + 4] &= !0x10;
    }
    Ok(bytes)
}

fn vp9_keys(defs: &mut Vec<Def>) {
    for ((w, h), space, _, range, _) in refs::VP9_KEYS {
        let file = refs::vp9_key(h, space, range);
        if range == "pc" {
            let tv = refs::vp9_key(h, space, "tv");
            let what = format!("{{ref:{tv}}} with its keyframe header's colour range bit set");
            defs.push(ours(&file, &what, move || {
                with_range(&mux::read(AREA, &tv)?, true)
            }));
            continue;
        }
        let color = format!("--color-space={space}");
        defs.push(
            Def::fixture(
                AREA,
                file,
                "vpxenc",
                tool(
                    "vpxenc",
                    &[
                        "--codec=vp9",
                        "--disable-warning-prompt",
                        "--debug",
                        "--rt",
                        "--cpu-used=8",
                        "--threads=1",
                        "--end-usage=q",
                        "--cq-level=40",
                        "--limit=1",
                        &color,
                        "--webm",
                        "--quiet",
                        "-o",
                        "{out}",
                        "{src}",
                    ],
                ),
            )
            .from(move || {
                Ok(mux::y4m(
                    (w, h),
                    (10, 1),
                    Csp::I420,
                    8,
                    1,
                    &move |_, x, y| bars_at(w, h, x, y),
                ))
            }),
        );
    }
}

fn variant_source(c: Csp, bits: u8) -> Vec<u8> {
    let size = refs::VARIANT_SIZE;
    mux::y4m(
        size,
        (refs::VARIANT_FPS, 1),
        c,
        bits,
        refs::VARIANT_FRAMES,
        &mux::pattern(size),
    )
}

fn x264(file: &str, extra: &[&str], c: Csp, bits: u8) -> Vec<Def> {
    use crate::containers::remux;
    let mut args = vec!["--crf", "30"];
    args.extend_from_slice(extra);
    let make = move || Ok(variant_source(c, bits));
    if file.ends_with(".mkv") {
        vec![remux::x264(AREA, file, &args).from(make)]
    } else {
        remux::x264_mp4(AREA, file, refs::VARIANT_FPS, &args, make)
    }
}

fn vpx(file: &str, c: Csp, bits: u8) -> Def {
    let profile = match (c, bits) {
        (Csp::I420, 8) => "--profile=0",
        (_, 8) => "--profile=1",
        (Csp::I420, _) => "--profile=2",
        _ => "--profile=3",
    };
    let depth = format!("--bit-depth={bits}");
    let input = format!("--input-bit-depth={bits}");
    let args = [
        "--codec=vp9",
        "--disable-warning-prompt",
        "--debug",
        "--rt",
        "--cpu-used=8",
        "--threads=1",
        profile,
        &depth,
        &input,
        "--end-usage=q",
        "--cq-level=40",
        "--webm",
        "--quiet",
        "-o",
        "{out}",
        "{src}",
    ];
    Def::fixture(AREA, file.into(), "vpxenc", tool("vpxenc", &args))
        .from(move || Ok(variant_source(c, bits)))
}

fn variants(defs: &mut Vec<Def>) {
    let size = refs::VARIANT_SIZE;
    let frames = refs::VARIANT_FRAMES.to_string();
    let mut made = Vec::new();
    for (file, v) in VARIANTS {
        match v {
            Variant::X264(c, bits, profile) => {
                let depth = bits.to_string();
                let out = csp(c).x26x();
                defs.extend(x264(
                    file,
                    &[
                        "--output-depth",
                        &depth,
                        "--output-csp",
                        out,
                        "--profile",
                        profile,
                    ],
                    csp(c),
                    bits,
                ));
            }
            Variant::X264Interlaced => defs.extend(x264(file, &["--tff"], Csp::I420, 8)),
            Variant::X265(c, bits) => {
                let stream = variant_stream(file);
                if !made.contains(&stream) {
                    made.push(stream.clone());
                    let profile = match c {
                        Chroma::C422 => "main422-10",
                        _ => "main10",
                    };
                    defs.push(
                        Def::digest(
                            AREA,
                            stream.clone(),
                            "x265",
                            x265(
                                &[
                                    "--frames",
                                    &frames,
                                    "--preset",
                                    "ultrafast",
                                    "--profile",
                                    profile,
                                    "--crf",
                                    "35",
                                    "--output",
                                    "{out}",
                                ],
                                size,
                                refs::VARIANT_FPS,
                                csp(c),
                                bits,
                            ),
                            sha,
                        )
                        .from(move || {
                            Ok(mux::raw_video(
                                size,
                                csp(c),
                                bits,
                                refs::VARIANT_FRAMES,
                                &mux::pattern(size),
                            ))
                        }),
                    );
                }
                let what = format!("{{ref:{stream}}} in our writer");
                defs.push(ours(file, &what, move || {
                    let coded = mux::hevc(&mux::read(AREA, &stream)?)?;
                    let t = Timed {
                        coded: &coded,
                        pocs: &[],
                        size,
                        fps: refs::VARIANT_FPS,
                    };
                    if file.ends_with(".mkv") {
                        t.hevc_mkv()
                    } else {
                        t.hevc_mp4(mp4::Brand::Mp4)
                    }
                }));
            }
            Variant::Vpx(c, bits) => {
                if file.ends_with(".webm") {
                    defs.push(vpx(file, csp(c), bits));
                    continue;
                }
                let webm = file.replace(".mp4", ".webm");
                let what = format!("{{ref:{webm}}}'s frames in our MP4 writer");
                let profile = match (c, bits) {
                    (Chroma::C420, 8) => 0,
                    (_, 8) => 1,
                    (Chroma::C420, _) => 2,
                    _ => 3,
                };
                let chroma = if c == Chroma::C444 { 3 } else { 1 };
                defs.push(ours(file, &what, move || {
                    vp9_mp4_at(&webm, (profile, bits, chroma), refs::VARIANT_FPS)
                }));
            }
        }
    }
}

fn vp9_mp4_at(webm: &str, format: (u8, u8, u8), fps: u32) -> Result<Vec<u8>, String> {
    let mux::Webm { coded, size, .. } = mux::webm_samples(&mux::work(AREA, webm))?;
    let pocs: Vec<i64> = (0..coded.samples.len() as i64).collect();
    let config = mp4::VpcConfig::new(format, (0, false), size, (fps, 1));
    Timed {
        coded: &coded,
        pocs: &pocs,
        size,
        fps,
    }
    .vp9_mp4(config)
}

fn profile2() -> Def {
    let size = refs::VP9_PROFILE2_SIZE;
    Def::fixture(
        AREA,
        refs::VP9_PROFILE2.into(),
        "vpxenc",
        tool(
            "vpxenc",
            &[
                "--codec=vp9",
                "--disable-warning-prompt",
                "--debug",
                "--good",
                "--cpu-used=8",
                "--threads=1",
                "--profile=2",
                "--bit-depth=10",
                "--input-bit-depth=10",
                "--kf-min-dist=10",
                "--kf-max-dist=10",
                "--auto-alt-ref=0",
                "--lag-in-frames=0",
                "--end-usage=q",
                "--cq-level=55",
                "--webm",
                "--quiet",
                "-o",
                "{out}",
                "{src}",
            ],
        ),
    )
    .from(move || {
        Ok(mux::y4m(
            size,
            (30, 1),
            Csp::I420,
            10,
            refs::VP9_PROFILE2_FRAMES,
            &mux::pattern(size),
        ))
    })
}

pub fn defs() -> Vec<Def> {
    let mut defs = Vec::new();
    lineups(&mut defs);
    off_ms(&mut defs);
    defs.push(vp9_altref());
    pyramid(&mut defs);
    tagged(&mut defs);
    vp9_keys(&mut defs);
    av1s(&mut defs);
    variants(&mut defs);
    defs.push(profile2());
    defs.push(
        Def::fixture(AREA, refs::MP3.into(), "lame", lame("64")).from(|| Ok(s16le(&mp3_source()))),
    );
    defs
}
