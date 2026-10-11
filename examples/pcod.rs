#[path = "support/fail.rs"]
mod fail;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use pito_codecs::gpu::{self, Backend, GpuFrame, HwDecoder, HwEncoder};
use pito_codecs::media::{Codec, Frame, Packet, Prefs, TimeBase, probe};
use pito_codecs::mp4::{
    AvcConfig, CodecConfig, HevcConfig, Reader, SampleEntry, Track, TrackConfig, Writer,
};
use pito_codecs::{h264, hevc};

type Fail = Box<dyn Error>;

#[derive(Parser)]
#[command(
    name = "pcod",
    version,
    about = "Probe, decode and encode with pito-codecs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Show what is in an MP4")]
    Probe { file: PathBuf },
    #[command(about = "Decode the whole video track and report the speed")]
    Decode {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "gpu")]
        route: Where,
        #[arg(long)]
        threads: Option<usize>,
    },
    #[command(about = "Decode, encode and mux an MP4, copying the audio")]
    Encode {
        input: PathBuf,
        output: PathBuf,
        #[arg(long, value_enum, default_value = "h264")]
        codec: Kind,
        #[arg(long, value_enum, default_value = "gpu")]
        route: Where,
        #[arg(long, default_value_t = 23)]
        crf: u8,
        #[arg(long)]
        threads: Option<usize>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Where {
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Kind {
    H264,
    Hevc,
}

impl Kind {
    fn codec(self) -> Codec {
        match self {
            Kind::H264 => Codec::H264,
            Kind::Hevc => Codec::Hevc,
        }
    }
}

fn main() -> ExitCode {
    let matches = Cli::command().arg_required_else_help(false).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    fail::run("pcod", || {
        let (file, result) = match cli.command {
            Command::Probe { file } => {
                let r = show(&file);
                (file, r)
            }
            Command::Decode {
                file,
                route,
                threads,
            } => {
                let r = decode(&file, route, threads);
                (file, r)
            }
            Command::Encode {
                input,
                output,
                codec,
                route,
                crf,
                threads,
            } => {
                let r = encode(&input, &output, codec, route, crf, threads);
                (input, r)
            }
        };
        result.map_err(|e| {
            if file.exists() {
                e
            } else {
                format!("{}: {e}", file.display()).into()
            }
        })
    })
}

fn show(file: &Path) -> Result<(), Fail> {
    let p = probe(file, &Prefs::default())?;
    let (w, h) = p.shown_size();
    println!("container  MP4");
    println!("duration   {:.1} s", p.duration);
    println!(
        "video      {} {} {w}x{h} {:.2} fps",
        p.video_codec_name,
        p.profile,
        p.fps()
    );
    for a in &p.audio {
        println!("audio      {} {} ch {} Hz", a.codec, a.channels, a.rate);
    }
    Ok(())
}

fn threads_of(wanted: Option<usize>) -> usize {
    wanted.filter(|n| *n > 0).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(16)
    })
}

fn length_prefixed_to_annexb(out: &mut Vec<u8>, data: &[u8], length: usize) {
    let mut pos = 0;
    while length > 0 && pos + length <= data.len() {
        let n = data[pos..pos + length]
            .iter()
            .fold(0usize, |a, &b| (a << 8) | b as usize);
        pos += length;
        let Some(nal) = data.get(pos..pos + n) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        pos += n;
    }
}

enum Picture {
    Cpu(Frame),
    Gpu(GpuFrame),
}

enum Decoder {
    H264 {
        dec: Box<h264::Decoder>,
        length: usize,
        params: Vec<u8>,
        buf: Vec<u8>,
    },
    Hevc(Box<hevc::Decoder>),
    Gpu(Box<HwDecoder>),
}

impl Decoder {
    fn cpu(track: &Track, threads: usize) -> Result<Self, Fail> {
        let time_base = TimeBase::new(1, track.timescale.max(1));
        match track.config() {
            Some(CodecConfig::Avc(c)) => {
                let mut dec = h264::Decoder::with_threads(threads);
                dec.set_time_base(time_base);
                let mut params = Vec::new();
                for nal in c.sps.iter().chain(&c.pps) {
                    params.extend_from_slice(&[0, 0, 0, 1]);
                    params.extend_from_slice(nal);
                }
                Ok(Decoder::H264 {
                    dec: Box::new(dec),
                    length: c.length_size.clamp(1, 4) as usize,
                    params,
                    buf: Vec::new(),
                })
            }
            Some(CodecConfig::Hevc(_)) => {
                let mut dec = hevc::Decoder::with_threads(threads);
                let hvcc = track.extradata().ok_or("the HEVC track has no hvcC")?;
                dec.configure_hvcc(&hvcc)?;
                dec.set_time_base(time_base);
                Ok(Decoder::Hevc(Box::new(dec)))
            }
            _ => Err("the video track is neither H.264 nor HEVC".into()),
        }
    }

    fn gpu(track: &Track) -> Result<Self, Fail> {
        let codec = track.codec().ok_or("unknown video codec")?;
        let extradata = track.extradata().unwrap_or_default();
        let dec = HwDecoder::new(codec, &extradata)?
            .with_time_base(TimeBase::new(1, track.timescale.max(1)));
        Ok(Decoder::Gpu(Box::new(dec)))
    }

    fn decode(&mut self, p: &Packet) -> Result<Vec<Picture>, Fail> {
        let frames = match self {
            Decoder::H264 {
                dec,
                length,
                params,
                buf,
            } => {
                buf.clear();
                if p.keyframe {
                    buf.extend_from_slice(params);
                }
                length_prefixed_to_annexb(buf, &p.data, *length);
                let packet = Packet {
                    data: std::mem::take(buf),
                    pts: p.pts,
                    dts: p.dts,
                    keyframe: p.keyframe,
                };
                let out = dec.decode(&packet);
                *buf = packet.data;
                out?
            }
            Decoder::Hevc(dec) => dec.decode(p)?,
            Decoder::Gpu(dec) => {
                return Ok(dec.decode_gpu(p)?.into_iter().map(Picture::Gpu).collect());
            }
        };
        Ok(frames.into_iter().map(Picture::Cpu).collect())
    }

    fn flush(&mut self) -> Result<Vec<Picture>, Fail> {
        let frames = match self {
            Decoder::H264 { dec, .. } => dec.flush()?,
            Decoder::Hevc(dec) => dec.flush()?,
            Decoder::Gpu(dec) => {
                return Ok(dec.flush_gpu()?.into_iter().map(Picture::Gpu).collect());
            }
        };
        Ok(frames.into_iter().map(Picture::Cpu).collect())
    }
}

fn driver_of(backend: Backend) -> String {
    let name = match backend {
        Backend::VaApi => "VA-API",
        Backend::Nvenc => "NVENC",
        Backend::VideoToolbox => "VideoToolbox",
        Backend::MediaFoundation => "Media Foundation",
    };
    match gpu::driver(backend) {
        Ok(d) => {
            let short = d
                .split_once('(')
                .and_then(|(_, rest)| rest.split(',').next())
                .unwrap_or(&d);
            format!("{name}, {short}")
        }
        Err(_) => name.to_string(),
    }
}

struct Opened {
    reader: Reader,
    track: usize,
    decoder: Decoder,
    label: String,
    note: Option<String>,
}

fn open(file: &Path, want: Where, threads: usize) -> Result<Opened, Fail> {
    let reader = Reader::open(file)?;
    let track = reader.video().ok_or("no video track")?;
    let t = &reader.tracks[track];
    let mut note = None;
    if want == Where::Gpu {
        match Decoder::gpu(t) {
            Ok(decoder) => {
                let label = match &decoder {
                    Decoder::Gpu(d) => format!("GPU ({})", driver_of(d.backend())),
                    _ => "GPU".to_string(),
                };
                return Ok(Opened {
                    reader,
                    track,
                    decoder,
                    label,
                    note,
                });
            }
            Err(e) => note = Some(format!("no GPU decoder ({e}), decoding on the CPU")),
        }
    }
    let decoder = Decoder::cpu(t, threads)?;
    Ok(Opened {
        reader,
        track,
        decoder,
        label: format!("CPU ({threads} threads)"),
        note,
    })
}

fn pump(
    opened: &mut Opened,
    mut sink: impl FnMut(Picture) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let count = opened.reader.tracks[opened.track].samples.len();
    for i in 0..count {
        let packet = opened.reader.packet(opened.track, i)?;
        for pic in opened.decoder.decode(&packet)? {
            sink(pic)?;
        }
    }
    for pic in opened.decoder.flush()? {
        sink(pic)?;
    }
    Ok(())
}

fn decode(file: &Path, want: Where, threads: Option<usize>) -> Result<(), Fail> {
    let mut opened = open(file, want, threads_of(threads))?;
    if let Some(note) = &opened.note {
        println!("{note}");
    }
    println!("route   {}", opened.label);
    let start = Instant::now();
    let mut frames = 0u64;
    pump(&mut opened, |_| {
        frames += 1;
        Ok(())
    })?;
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "decoded {frames} frames in {seconds:.2} s: {:.0} fps",
        frames as f64 / seconds
    );
    Ok(())
}

enum Encoder {
    Cpu(Box<h264::Encoder>),
    Gpu(Box<HwEncoder>),
}

impl Encoder {
    fn extradata(&self) -> Option<Vec<u8>> {
        match self {
            Encoder::Cpu(e) => Some(e.avcc()),
            Encoder::Gpu(e) => e.extradata().map(<[u8]>::to_vec),
        }
    }

    fn flush(&mut self) -> Result<Vec<Packet>, Fail> {
        Ok(match self {
            Encoder::Cpu(e) => e.flush()?,
            Encoder::Gpu(e) => e.flush()?,
        })
    }
}

fn frame_index(pts: i64, first: i64, time_base: TimeBase, fps: (u32, u32)) -> i64 {
    let ticks = (pts as i128 - first as i128) * fps.0 as i128 * time_base.num as i128;
    let per = (fps.1.max(1) as i128 * time_base.den.max(1) as i128).max(1);
    ((ticks + per / 2).div_euclid(per)).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn encode(
    input: &Path,
    output: &Path,
    kind: Kind,
    want: Where,
    crf: u8,
    threads: Option<usize>,
) -> Result<(), Fail> {
    let threads = threads_of(threads);
    let info = probe(input, &Prefs::default())?;
    let fps = info.fps;
    if fps.0 == 0 || fps.1 == 0 {
        return Err("the video track has no frame rate".into());
    }
    let (width, height) = (info.width & !1, info.height & !1);
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(format!(
            "the video is {}x{}, which can't be encoded",
            info.width, info.height
        )
        .into());
    }
    let mut opened = open(input, want, threads)?;
    if let Some(note) = &opened.note {
        println!("{note}");
    }
    println!("decode  {}", opened.label);
    let gop = (fps.0 / fps.1).clamp(1, 120) * 2;
    let mut codec = kind.codec();
    let mut gpu_encoder = None;
    if want == Where::Gpu {
        let mut tries = vec![codec];
        if codec == Codec::Hevc {
            tries.push(Codec::H264);
        }
        for c in tries {
            let config = gpu::EncoderConfig {
                width,
                height,
                fps,
                profile: if c == Codec::Hevc {
                    gpu::Profile::Main
                } else {
                    gpu::Profile::High
                },
                rate: gpu::Rate::Crf(crf),
                gop,
                b_frames: 0,
                bitstream: gpu::Bitstream::LengthPrefixed,
            };
            match HwEncoder::new(c, &config) {
                Ok(e) => {
                    codec = c;
                    gpu_encoder = Some(e);
                    break;
                }
                Err(e) => println!("no GPU {c} encoder ({e})"),
            }
        }
    }
    let mut encoder = match gpu_encoder {
        Some(e) => {
            println!("encode  {codec} GPU ({})", driver_of(e.backend()));
            Encoder::Gpu(Box::new(e))
        }
        None => {
            if codec != Codec::H264 {
                println!("{codec} encodes on the GPU only, encoding H.264 on the CPU");
            }
            codec = Codec::H264;
            let e = h264::Encoder::new(h264::EncoderConfig {
                width,
                height,
                fps,
                profile: h264::Profile::High,
                preset: h264::Preset::Balanced,
                rate: h264::Rate::Crf(crf),
                gop,
                b_frames: 2,
                threads,
                bitstream: h264::Bitstream::LengthPrefixed,
            })?;
            println!("encode  {codec} CPU ({threads} threads)");
            Encoder::Cpu(Box::new(e))
        }
    };

    let source_scale = opened.reader.tracks[opened.track].timescale.max(1);
    let source_base = TimeBase::new(1, source_scale);
    let frame_base = TimeBase::new(fps.1, fps.0);
    let step = fps.1 as i64 * 64;
    let timescale = fps
        .0
        .checked_mul(64)
        .ok_or("the frame rate is out of range")?;
    let rotation = opened.reader.tracks[opened.track].matrix;

    let start = Instant::now();
    let mut first: Option<i64> = None;
    let mut frames = 0u64;
    let mut in_ticks = false;
    let mut packets: Vec<Packet> = Vec::new();
    {
        let encoder = &mut encoder;
        let packets = &mut packets;
        let in_ticks = &mut in_ticks;
        pump(&mut opened, |pic| {
            frames += 1;
            let (pts, base) = match &pic {
                Picture::Cpu(f) => (f.pts, f.time_base),
                Picture::Gpu(g) => (g.pts(), g.time_base()),
            };
            let origin = *first.get_or_insert(pts);
            let index = frame_index(pts, origin, base, fps);
            let out = match (&mut *encoder, pic) {
                (Encoder::Gpu(e), Picture::Gpu(g)) => {
                    *in_ticks = true;
                    e.encode_gpu(&g)?
                }
                (enc, pic) => {
                    let mut frame = match pic {
                        Picture::Cpu(f) => f,
                        Picture::Gpu(g) => g.download()?,
                    };
                    frame.pts = index;
                    frame.time_base = frame_base;
                    match enc {
                        Encoder::Cpu(e) => e.encode(&frame)?,
                        Encoder::Gpu(e) => e.encode(&frame)?,
                    }
                }
            };
            packets.extend(out);
            Ok(())
        })?;
    }
    packets.extend(encoder.flush()?);
    if in_ticks {
        let origin = first.unwrap_or(0);
        for p in &mut packets {
            p.pts = frame_index(p.pts, origin, source_base, fps);
            p.dts = frame_index(p.dts, origin, source_base, fps);
        }
    }
    let extradata = encoder
        .extradata()
        .ok_or("the encoder gave no parameter sets")?;
    let entry = match codec {
        Codec::Hevc => {
            SampleEntry::hevc(HevcConfig::parse(&extradata)?, width as u16, height as u16)
        }
        _ => SampleEntry::h264(AvcConfig::parse(&extradata)?, width as u16, height as u16),
    };

    let mut writer = Writer::create(output)?;
    let mut cfg = TrackConfig::new(entry, timescale);
    cfg.matrix = rotation;
    let video = writer.add_track(cfg)?;
    let audio: Vec<(usize, usize)> = opened
        .reader
        .audio()
        .into_iter()
        .map(|source| {
            let cfg = TrackConfig::from_track(&opened.reader.tracks[source])?;
            Ok::<_, Fail>((source, writer.add_track(cfg)?))
        })
        .collect::<Result<_, _>>()?;
    let mut last: Option<i64> = None;
    for p in &packets {
        let mut dts = p.dts.saturating_mul(step);
        if let Some(l) = last
            && dts <= l
        {
            dts = l.saturating_add(1);
        }
        last = Some(dts);
        writer.push(
            video,
            &Packet {
                data: p.data.clone(),
                pts: p.pts.saturating_mul(step),
                dts,
                keyframe: p.keyframe,
            },
        )?;
    }
    for (source, target) in audio {
        for i in 0..opened.reader.tracks[source].samples.len() {
            writer.push(target, &opened.reader.packet(source, i)?)?;
        }
    }
    writer.finish_faststart()?;
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "encoded {frames} frames in {seconds:.2} s: {:.0} fps",
        frames as f64 / seconds
    );
    println!(
        "wrote   {} ({:.1} MB)",
        output.display(),
        std::fs::metadata(output)?.len() as f64 / 1e6
    );
    Ok(())
}
