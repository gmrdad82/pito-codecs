use std::error::Error;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use pito_codecs::gpu::{self, Backend, HwDecoder, HwEncoder};
use pito_codecs::media::{
    self, Caps, Choice, Codec, Fallback, Packet, Prefs, Route, TimeBase, VideoFrame as Picture,
    pick_decode, pick_encode, probe,
};
use pito_codecs::mp4::{
    AvcConfig, CodecConfig, HevcConfig, Reader, SampleEntry, Track, TrackConfig, Writer,
};
use pito_codecs::{h264, hevc};

type Fail = Box<dyn Error + Send + Sync>;

#[path = "pcod/frames.rs"]
mod frames;
#[path = "pcod/json.rs"]
mod json;
#[path = "pcod/manifest.rs"]
mod manifest;
#[path = "pcod/probe_json.rs"]
mod probe_json;
#[path = "pcod/progress.rs"]
mod progress;
#[path = "pcod/quality.rs"]
mod quality;
#[path = "pcod/sha256.rs"]
mod sha256;
#[path = "pcod/stills.rs"]
mod stills;

#[path = "pcod/args.rs"]
mod args;
#[path = "pcod/convert.rs"]
mod convert;
#[path = "pcod/extract.rs"]
mod extract;
#[path = "pcod/pipeline.rs"]
mod pipeline;
#[path = "pcod/plan.rs"]
mod plan;
#[path = "pcod/profile.rs"]
mod profile;
#[path = "pcod/signals.rs"]
mod signals;
#[path = "pcod/size.rs"]
mod size;
#[path = "pcod/source.rs"]
mod source;

#[derive(Parser)]
#[command(
    name = "pcod",
    version,
    about = "Convert, probe, decode and encode with pito-codecs",
    args_conflicts_with_subcommands = true,
    override_usage = args::USAGE,
    after_help = args::help()
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(value_name = "INPUT", help = "A video file, or a folder of PNG frames")]
    input: Option<PathBuf>,
    #[arg(
        value_name = "OUTPUT",
        help = "The file to write: mp4, mov, mkv, webm, gif, webp, or wav for the audio alone"
    )]
    output: Option<PathBuf>,
    #[command(flatten)]
    convert: args::ConvertArgs,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Convert a video or a folder of PNG frames (the default command)")]
    Convert {
        #[arg(value_name = "INPUT")]
        input: PathBuf,
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,
        #[command(flatten)]
        convert: args::ConvertArgs,
    },
    #[command(about = "Join clips in order into one file")]
    Join {
        #[arg(value_name = "INPUTS", num_args = 2.., required = true)]
        inputs: Vec<PathBuf>,
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,
        #[command(flatten)]
        convert: args::ConvertArgs,
    },
    #[command(about = "List the profiles and what each one does")]
    Profiles,
    #[command(about = "Show what is in a video file")]
    Probe {
        file: PathBuf,
        #[arg(long, help = "Print what pcod reads as JSON")]
        json: bool,
    },
    #[command(
        about = "Measure an audio track's loudness (EBU R128, ITU-R BS.1770-4)",
        long_about = "Measure an audio track's loudness (EBU R128, ITU-R BS.1770-4)\n\nReads the track at its own rate and channels and reports the integrated loudness (gated), the loudness range, the true peak (4x oversampled below 88.2 kHz), the sample peak and the short-term (3 s) and momentary (400 ms) maxima. Five- and six-channel tracks weigh the surrounds by 1.41 and leave out the LFE (the fourth channel). The file is a video or an audio file: MP4, MOV, M4A, MKV, MKA, WebM, WAV or MP3."
    )]
    Loudness {
        #[arg(help = "A video or audio file")]
        file: PathBuf,
        #[arg(long, default_value_t = 0, help = "The audio track, counted from 0")]
        track: usize,
        #[arg(long, help = "Print the measurements as JSON")]
        json: bool,
    },
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
    #[command(
        about = "Encode a folder of PNG frames and an optional WAV into an MP4",
        long_about = "Encode a folder of PNG frames and an optional WAV into an MP4\n\nThe frames are the folder's .png files in name order (000000.png, 000001.png, ...), all of one even size. The video is 8-bit 4:2:0 BT.709, the audio AAC at 48 kHz stereo, and the file is written with its index first (faststart). The audio is cut or padded with silence to the length of the video; without --audio the MP4 carries a silent stereo track."
    )]
    Frames {
        #[arg(help = "The folder of PNG frames")]
        dir: PathBuf,
        #[arg(help = "The MP4 to write")]
        output: PathBuf,
        #[arg(long, value_parser = rate_of, help = "Frame rate: an integer, a ratio such as 30000/1001, or a decimal")]
        fps: (u32, u32),
        #[arg(long, help = "A PCM WAV to encode as the audio track")]
        audio: Option<PathBuf>,
        #[arg(
            long,
            allow_hyphen_values = true,
            requires = "audio",
            value_parser = args::loudness_of,
            help = "Level the audio to LUFS (-70 to -5) or youtube, measured on the encoded AAC"
        )]
        loudness: Option<plan::Loudness>,
        #[arg(
            long,
            allow_hyphen_values = true,
            requires = "loudness",
            help = "The true-peak ceiling the encoded AAC keeps, in dBTP [default: -1]"
        )]
        true_peak: Option<f64>,
        #[arg(
            long,
            requires = "loudness",
            help = "Keep the dynamics: gain to the target, then only a true-peak limiter at the ceiling (may land under the target)"
        )]
        linear: bool,
        #[arg(
            long,
            value_parser = clap::value_parser!(u32).range(32..=512),
            help = "The AAC bitrate in kbps, 32 to 512 [default: 192, or 320 with --loudness]"
        )]
        audio_bitrate: Option<u32>,
        #[arg(
            long,
            value_enum,
            default_value = "h264",
            help = "The video codec; HEVC falls back to H.264 without a GPU HEVC encoder"
        )]
        codec: Kind,
        #[arg(
            long,
            value_enum,
            default_value = "gpu",
            help = "Encode on the GPU, falling back to the CPU, or on the CPU only"
        )]
        route: Where,
        #[arg(long, default_value_t = 18, help = "Quality, 0 to 51, lower is better")]
        crf: u8,
        #[arg(long, help = "PNG decode threads [default: one per core, at most 16]")]
        threads: Option<usize>,
    },
    #[command(
        about = "Write stills of a video as PNGs and/or one contact sheet",
        long_about = "Write stills of a video as PNGs and/or one contact sheet\n\nThe stills are N evenly spaced over the video (--count, 12 by default, or rows x cols when both are given for a sheet) or at the given times (--at). Each is scaled to --height keeping its shape. --out writes still-001.png, still-002.png, ... into a folder; --sheet writes one PNG of rows x cols stills on black with --pad pixels between and around them. --json prints each still's time, size and path.",
        group(ArgGroup::new("written").args(["out", "sheet"]).required(true).multiple(true))
    )]
    Stills {
        #[arg(help = "The video: MP4, MOV, MKV or WebM")]
        file: PathBuf,
        #[arg(
            long,
            short = 'n',
            conflicts_with = "at",
            help = "How many evenly spaced stills [default: 12]"
        )]
        count: Option<usize>,
        #[arg(
            long,
            value_delimiter = ',',
            value_parser = time_of,
            help = "Times to take, comma separated: seconds, m:ss or h:mm:ss"
        )]
        at: Vec<f64>,
        #[arg(long, default_value_t = 360, help = "Height of each still in pixels")]
        height: u32,
        #[arg(long, help = "A folder to write one PNG per still into")]
        out: Option<PathBuf>,
        #[arg(long, help = "A PNG to write the contact sheet to")]
        sheet: Option<PathBuf>,
        #[arg(long, help = "Rows of the contact sheet [default: to fit]")]
        rows: Option<usize>,
        #[arg(long, help = "Columns of the contact sheet [default: about square]")]
        cols: Option<usize>,
        #[arg(
            long,
            default_value_t = 8,
            help = "Pixels between and around the sheet's stills"
        )]
        pad: u32,
        #[arg(
            long,
            value_enum,
            default_value = "exact",
            help = "The exact frame at each time, or the keyframe at or before it (faster)"
        )]
        seek: SeekKind,
        #[arg(
            long,
            value_enum,
            default_value = "gpu",
            help = "Decode on the GPU, falling back to the CPU, or on the CPU only"
        )]
        route: Where,
        #[arg(long, help = "Print each still's time, size and path as JSON")]
        json: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SeekKind {
    Exact,
    Keyframe,
}

pub fn time_of(text: &str) -> Result<f64, String> {
    let bad = || format!("'{text}' is not a time such as 90, 1:30 or 1:02:03.5");
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return Err(bad());
    }
    let mut total = 0.0f64;
    for (i, part) in parts.iter().enumerate() {
        let v: f64 = part.parse().map_err(|_| bad())?;
        let last = i + 1 == parts.len();
        if !v.is_finite() || v < 0.0 || (!last && v.fract() != 0.0) || (i > 0 && v >= 60.0) {
            return Err(bad());
        }
        total = total * 60.0 + v;
    }
    Ok(total)
}

fn rate_of(text: &str) -> Result<(u32, u32), String> {
    let bad = || format!("'{text}' is not a frame rate such as 60, 30000/1001 or 29.97");
    let (num, den) = if let Some((n, d)) = text.split_once('/') {
        (
            n.trim().parse::<u32>().map_err(|_| bad())?,
            d.trim().parse::<u32>().map_err(|_| bad())?,
        )
    } else if let Some((whole, frac)) = text.split_once('.') {
        if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let den = 10u32.pow(frac.len() as u32);
        let whole = whole.parse::<u32>().map_err(|_| bad())?;
        let frac = frac.parse::<u32>().map_err(|_| bad())?;
        (
            whole
                .checked_mul(den)
                .and_then(|w| w.checked_add(frac))
                .ok_or_else(bad)?,
            den,
        )
    } else {
        (text.trim().parse::<u32>().map_err(|_| bad())?, 1)
    };
    if num == 0 || den == 0 || num / den > 1000 {
        return Err(bad());
    }
    let g = gcd(num, den);
    Ok((num / g, den / g))
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Where {
    Cpu,
    Gpu,
}

impl Where {
    fn prefs(self) -> Prefs {
        let choice = match self {
            Where::Cpu => Choice::Cpu,
            Where::Gpu => Choice::Gpu,
        };
        Prefs {
            decode: choice.clone(),
            encode: choice,
            ..Prefs::default()
        }
    }
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
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };
    panic::set_hook(Box::new(|_| {}));
    let command = match cli.command {
        None => {
            return convert::run(args::bare(cli.input, cli.output, cli.convert));
        }
        Some(Command::Convert {
            input,
            output,
            convert,
        }) => {
            return convert::run(args::options(
                Some("convert"),
                input,
                Vec::new(),
                output,
                convert,
            ));
        }
        Some(Command::Join {
            mut inputs,
            output,
            convert,
        }) => {
            let first = inputs.remove(0);
            return convert::run(args::options(Some("join"), first, inputs, output, convert));
        }
        Some(Command::Profiles) => return convert::print(&convert::profiles()),
        Some(Command::Probe { file, json: true }) => {
            return match probe_json::probe_json(&file) {
                Ok(text) => convert::print(&text),
                Err(e) => convert::refuse(&e.to_string()),
            };
        }
        Some(c) => c,
    };
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| run(command)));
    let text = match outcome {
        Ok(Ok(())) => return ExitCode::SUCCESS,
        Ok(Err(e)) => e.to_string(),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown cause".into());
            format!("internal error: {what}")
        }
    };
    let line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    let line = if line.is_empty() {
        "failed with no message".to_string()
    } else {
        line
    };
    let _ = writeln!(io::stderr(), "pcod: {line}");
    ExitCode::FAILURE
}

fn run(command: Command) -> Result<(), Fail> {
    let mut out = io::stdout().lock();
    let result = match command {
        Command::Probe { file, .. } => show(&mut out, &file),
        Command::Loudness { file, track, json } => loudness(&mut out, &file, track, json),
        Command::Convert { .. } | Command::Join { .. } | Command::Profiles => Ok(()),
        Command::Decode {
            file,
            route,
            threads,
        } => decode(&mut out, &file, route, threads),
        Command::Encode {
            input,
            output,
            codec,
            route,
            crf,
            threads,
        } => encode(&mut out, &input, &output, codec, route, crf, threads),
        Command::Frames {
            dir,
            output,
            fps,
            audio,
            loudness,
            true_peak,
            linear,
            audio_bitrate,
            codec,
            route,
            crf,
            threads,
        } => frames::run(
            &mut out,
            &frames::Job {
                dir,
                output,
                fps,
                audio,
                audio_kbps: audio_bitrate.unwrap_or(if loudness.is_some() {
                    plan::AAC_KBPS
                } else {
                    frames::AAC_KBPS
                }),
                loudness: args::loudness_with(loudness, true_peak, linear),
                codec: codec.codec(),
                prefs: route.prefs(),
                crf,
                threads: threads_of(threads),
            },
        ),
        Command::Stills {
            file,
            count,
            at,
            height,
            out: dir,
            sheet,
            rows,
            cols,
            pad,
            seek,
            route,
            json,
        } => stills::run(
            &mut out,
            &stills::Job {
                file,
                count,
                at,
                height,
                out: dir,
                sheet,
                rows,
                cols,
                pad,
                seek: match seek {
                    SeekKind::Exact => media::Seek::Exact,
                    SeekKind::Keyframe => media::Seek::Keyframe,
                },
                prefs: route.prefs(),
                json,
            },
        ),
    };
    result?;
    out.flush()?;
    Ok(())
}

fn loudness(out: &mut impl Write, file: &Path, track: usize, json: bool) -> Result<(), Fail> {
    let prefs = Prefs::default();
    let p = probe(file, &prefs).map_err(|e| about(file, e))?;
    let t = p.audio.get(track).ok_or_else(|| {
        about(
            file,
            match p.audio.len() {
                0 => "the file has no audio".to_string(),
                n => format!("there is no audio track {track}; the file has {n}"),
            },
        )
    })?;
    let m = media::meter_loudness(file, track, &prefs, |_| true).map_err(|e| about(file, e))?;
    if json {
        let doc = json::Json::Obj(vec![
            ("file", file.display().to_string().into()),
            ("track", track.into()),
            ("codec", t.codec.as_str().into()),
            ("channels", t.channels.into()),
            ("sample_rate", t.rate.into()),
            ("seconds", p.duration.into()),
            ("integrated", m.integrated.into()),
            ("range", m.range.into()),
            ("true_peak", m.true_peak.into()),
            ("sample_peak", m.sample_peak.into()),
            ("short_term_max", m.short_term_max.into()),
            ("momentary_max", m.momentary_max.into()),
        ]);
        writeln!(out, "{}", doc.text())?;
        return Ok(());
    }
    let n = |v: f64| {
        if v.is_finite() {
            format!("{v:7.2}")
        } else {
            "   -inf".to_string()
        }
    };
    writeln!(
        out,
        "audio           track {track}, {} {} ch {} Hz, {:.2} s",
        t.codec, t.channels, t.rate, p.duration
    )?;
    writeln!(out, "integrated      {} LUFS", n(m.integrated))?;
    writeln!(out, "range           {} LU", n(m.range))?;
    writeln!(out, "true peak       {} dBTP", n(m.true_peak))?;
    writeln!(out, "sample peak     {} dBFS", n(m.sample_peak))?;
    writeln!(out, "short-term max  {} LUFS", n(m.short_term_max))?;
    writeln!(out, "momentary max   {} LUFS", n(m.momentary_max))?;
    Ok(())
}

fn about(file: &Path, e: impl std::fmt::Display) -> Fail {
    let shown = file.display().to_string();
    let text = e.to_string();
    if text.contains(&shown) {
        text.into()
    } else {
        format!("{shown}: {text}").into()
    }
}

fn show(out: &mut impl Write, file: &Path) -> Result<(), Fail> {
    let p = probe(file, &Prefs::default()).map_err(|e| about(file, e))?;
    let (w, h) = p.shown_size();
    writeln!(out, "container  MP4")?;
    writeln!(out, "duration   {:.1} s", p.duration)?;
    if p.start > 0.0 {
        writeln!(out, "start      {:.3} s", p.start)?;
    }
    writeln!(
        out,
        "video      {} {} {w}x{h} {:.2} fps",
        p.video_codec_name,
        p.profile,
        p.fps()
    )?;
    if p.alpha {
        writeln!(out, "alpha      yes")?;
    }
    for a in &p.audio {
        writeln!(
            out,
            "audio      {} {} ch {} Hz",
            a.codec, a.channels, a.rate
        )?;
    }
    Ok(())
}

fn threads_of(wanted: Option<usize>) -> usize {
    wanted.filter(|n| *n > 0).unwrap_or_else(media::threads)
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
                let params = c.parameter_sets_annexb();
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

    fn gpu(track: &Track, codec: Codec) -> Result<HwDecoder, Fail> {
        let extradata = track.extradata().unwrap_or_default();
        Ok(HwDecoder::new(codec, &extradata)?
            .with_time_base(TimeBase::new(1, track.timescale.max(1))))
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
                media::length_prefixed_to_annexb(buf, &p.data, *length);
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

    fn recycle(&mut self, pic: Picture) {
        if let (Decoder::H264 { dec, .. }, Picture::Cpu(f)) = (self, pic) {
            dec.recycle(f);
        }
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
    notes: Vec<String>,
}

fn open(file: &Path, want: Where, threads: usize) -> Result<Opened, Fail> {
    let prefs = want.prefs();
    let reader = Reader::open(file).map_err(|e| about(file, e))?;
    let track = reader
        .video()
        .ok_or_else(|| about(file, "no video track"))?;
    let t = &reader.tracks[track];
    let codec = t
        .codec()
        .ok_or_else(|| about(file, "an unknown video codec"))?;
    let picked =
        pick_decode(codec, &prefs, &Caps::for_decode(&prefs)).map_err(|e| about(file, e))?;
    let mut notes = picked.reasons;
    if picked.route == Route::Gpu {
        match Decoder::gpu(t, codec) {
            Ok(dec) => {
                let label = format!("GPU ({})", driver_of(dec.backend()));
                return Ok(Opened {
                    reader,
                    track,
                    decoder: Decoder::Gpu(Box::new(dec)),
                    label,
                    notes,
                });
            }
            Err(e) => notes.push(format!("the GPU decoder failed ({e}); decoding on the CPU")),
        }
    }
    let decoder = Decoder::cpu(t, threads).map_err(|e| about(file, e))?;
    Ok(Opened {
        reader,
        track,
        decoder,
        label: format!("CPU ({threads} threads)"),
        notes,
    })
}

fn pump(
    opened: &mut Opened,
    mut sink: impl FnMut(Picture) -> Result<Option<Picture>, Fail>,
) -> Result<(), Fail> {
    let count = opened.reader.tracks[opened.track].samples.len();
    let mut drain = |opened: &mut Opened, pics: Vec<Picture>| -> Result<(), Fail> {
        for pic in pics {
            if let Some(back) = sink(pic)? {
                opened.decoder.recycle(back);
            }
        }
        Ok(())
    };
    for i in 0..count {
        let packet = opened.reader.packet(opened.track, i)?;
        let pics = opened.decoder.decode(&packet)?;
        drain(opened, pics)?;
    }
    let pics = opened.decoder.flush()?;
    drain(opened, pics)
}

fn notes(out: &mut impl Write, notes: &[String]) -> Result<(), Fail> {
    if !notes.is_empty() {
        writeln!(out, "note    {}", notes.join("; "))?;
    }
    Ok(())
}

fn decode(
    out: &mut impl Write,
    file: &Path,
    want: Where,
    threads: Option<usize>,
) -> Result<(), Fail> {
    let mut opened = open(file, want, threads_of(threads))?;
    notes(out, &opened.notes)?;
    writeln!(out, "route   {}", opened.label)?;
    let start = Instant::now();
    let mut frames = 0u64;
    pump(&mut opened, |pic| {
        frames += 1;
        Ok(Some(pic))
    })?;
    let seconds = start.elapsed().as_secs_f64().max(1e-9);
    writeln!(
        out,
        "decoded {frames} frames in {seconds:.2} s: {:.0} fps",
        frames as f64 / seconds
    )?;
    Ok(())
}

enum Encoder {
    Cpu(Box<h264::Encoder>),
    Gpu(Box<HwEncoder>),
}

impl Encoder {
    fn cpu(
        width: u32,
        height: u32,
        fps: (u32, u32),
        crf: u8,
        threads: usize,
    ) -> Result<Self, Fail> {
        let e = h264::Encoder::new(h264::EncoderConfig {
            width,
            height,
            fps,
            profile: h264::Profile::High,
            preset: h264::Preset::Balanced,
            rate: media::Rate::Crf(crf),
            gop: gop_of(fps),
            b_frames: 2,
            threads,
            bitstream: media::Bitstream::LengthPrefixed,
        })?;
        Ok(Encoder::Cpu(Box::new(e)))
    }

    fn gpu(codec: Codec, width: u32, height: u32, fps: (u32, u32), crf: u8) -> Result<Self, Fail> {
        let config = gpu::EncoderConfig {
            width,
            height,
            fps,
            profile: if codec == Codec::Hevc {
                gpu::Profile::Main
            } else {
                gpu::Profile::High
            },
            rate: media::Rate::Crf(crf),
            gop: gop_of(fps),
            b_frames: 0,
            bitstream: media::Bitstream::LengthPrefixed,
            ..Default::default()
        };
        Ok(Encoder::Gpu(Box::new(HwEncoder::new(codec, &config)?)))
    }

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

fn gop_of(fps: (u32, u32)) -> u32 {
    (fps.0 / fps.1.max(1)).clamp(1, 120) * 2
}

fn frame_index(pts: i64, first: i64, time_base: TimeBase, fps: (u32, u32)) -> i64 {
    let ticks = (pts as i128 - first as i128) * fps.0 as i128 * time_base.num as i128;
    let per = (fps.1.max(1) as i128 * time_base.den.max(1) as i128).max(1);
    ((ticks + per / 2).div_euclid(per)).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn encode(
    out: &mut impl Write,
    input: &Path,
    output: &Path,
    kind: Kind,
    want: Where,
    crf: u8,
    threads: Option<usize>,
) -> Result<(), Fail> {
    let threads = threads_of(threads);
    let prefs = want.prefs();
    let info = probe(input, &prefs).map_err(|e| about(input, e))?;
    let fps = info.fps;
    if fps.0 == 0 || fps.1 == 0 {
        return Err(about(input, "the video track has no frame rate"));
    }
    let (width, height) = (info.width & !1, info.height & !1);
    if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(about(
            input,
            format!(
                "the video is {}x{}, which can't be encoded",
                info.width, info.height
            ),
        ));
    }
    let mut picked = pick_encode(kind.codec(), &prefs, &Caps::detect(&prefs))?;
    let mut opened = open(input, want, threads)?;
    notes(out, &opened.notes)?;
    writeln!(out, "decode  {}", opened.label)?;
    let mut encoder = None;
    if picked.route == Route::Gpu {
        let mut tries = vec![picked.codec];
        if picked.codec == Codec::Hevc && prefs.hevc_encode_fallback.contains(&Fallback::H264Gpu) {
            tries.push(Codec::H264);
        }
        for codec in tries {
            match Encoder::gpu(codec, width, height, fps, crf) {
                Ok(e) => {
                    picked.codec = codec;
                    encoder = Some(e);
                    break;
                }
                Err(e) => picked
                    .reasons
                    .push(format!("the GPU {codec} encoder failed ({e})")),
            }
        }
    }
    let mut encoder = match encoder {
        Some(e) => e,
        None => {
            if picked.route == Route::Gpu {
                picked.reasons.push("encoding H.264 on the CPU".to_string());
            }
            picked.codec = Codec::H264;
            Encoder::cpu(width, height, fps, crf, threads)?
        }
    };
    notes(out, &picked.reasons)?;
    let codec = picked.codec;
    match &encoder {
        Encoder::Gpu(e) => writeln!(out, "encode  {codec} GPU ({})", driver_of(e.backend()))?,
        Encoder::Cpu(_) => writeln!(out, "encode  {codec} CPU ({threads} threads)")?,
    }

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
    pump(&mut opened, |pic| {
        frames += 1;
        let (pts, base) = match &pic {
            Picture::Cpu(f) => (f.pts, f.time_base),
            Picture::Gpu(g) => (g.pts(), g.time_base()),
        };
        let origin = *first.get_or_insert(pts);
        let index = frame_index(pts, origin, base, fps);
        let back = match (&mut encoder, pic) {
            (Encoder::Gpu(e), Picture::Gpu(g)) if g.on_gpu() => {
                in_ticks = true;
                packets.extend(e.encode_gpu(&g)?);
                None
            }
            (enc, pic) => {
                let mut frame = match pic {
                    Picture::Cpu(f) => f,
                    Picture::Gpu(g) => g.download()?,
                };
                frame.pts = index;
                frame.time_base = frame_base;
                packets.extend(match enc {
                    Encoder::Cpu(e) => e.encode(&frame)?,
                    Encoder::Gpu(e) => e.encode(&frame)?,
                });
                Some(Picture::Cpu(frame))
            }
        };
        Ok(back)
    })?;
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

    let mut writer = Writer::create(output).map_err(|e| about(output, e))?;
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
    for p in packets {
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
                pts: p.pts.saturating_mul(step),
                dts,
                ..p
            },
        )?;
    }
    for (source, target) in audio {
        for i in 0..opened.reader.tracks[source].samples.len() {
            writer.push(target, &opened.reader.packet(source, i)?)?;
        }
    }
    writer.finish_faststart()?;
    let seconds = start.elapsed().as_secs_f64().max(1e-9);
    writeln!(
        out,
        "encoded {frames} frames in {seconds:.2} s: {:.0} fps",
        frames as f64 / seconds
    )?;
    writeln!(
        out,
        "wrote   {} ({:.1} MB)",
        output.display(),
        std::fs::metadata(output)?.len() as f64 / 1e6
    )?;
    Ok(())
}
