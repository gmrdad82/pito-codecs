#[path = "bench/charts.rs"]
mod charts;
#[macro_use]
#[path = "support/fail.rs"]
mod fail;
#[path = "bench/docs.rs"]
mod docs;
#[path = "bench/reference.rs"]
mod reference;

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pito_codecs::aac;
use pito_codecs::filter::{Kernel, LoudnessMeter, Scaler};
use pito_codecs::gpu::{self, HwDecoder, HwEncoder};
use pito_codecs::h264;
use pito_codecs::hevc;
use pito_codecs::media::{
    AudioBuffer, Choice, Codec, ExportOptions, Frame, Packet, Piece, PixelFormat, Prefs, TimeBase,
    export, first_frame, frames_at,
};
use pito_codecs::mp4::{CodecConfig, Reader};
use reference::{Case, Reference};
use serde::{Deserialize, Serialize};

type Res<T> = Result<T, Box<dyn Error>>;

const CLIP_SECONDS: u32 = 20;
const ENCODE_FRAMES: usize = 240;
const ALL_SIZES: [(u32, u32); 2] = [(1920, 1080), (2560, 1440)];

const USAGE: &str = "usage: bench [--runs N] [--quick] [--only ID]... [--out DIR] [--images DIR] [--fixtures DIR] [--reference FILE] [--charts-only] [--no-charts]
       bench --check <new results.json> [baseline.json]
       bench --chart-docs <results.json> <dir> [reference.toml]
       bench --record-ffmpeg [--runs N] [--only ID]... [--out DIR] [--fixtures DIR] [--reference FILE]

The bench times pito-codecs and reads every ffmpeg column from the reference file
(bench/ffmpeg-reference.toml): it never runs ffmpeg. --record-ffmpeg is the one path
that runs ffmpeg: it times the ffmpeg on PATH on the bench's cases, makes the inputs,
and rewrites the reference file. Re-recording is the owner's call, made only on his word.";

static QUICK: AtomicBool = AtomicBool::new(false);
static FFMPEG: OnceLock<PathBuf> = OnceLock::new();

fn recording() -> bool {
    FFMPEG.get().is_some()
}

fn ours_runs(runs: usize) -> usize {
    if recording() { 0 } else { runs }
}

fn ffmpeg_runs(runs: usize) -> usize {
    if recording() { runs } else { 0 }
}

fn sizes() -> &'static [(u32, u32)] {
    if QUICK.load(Ordering::Relaxed) {
        &ALL_SIZES[..1]
    } else {
        &ALL_SIZES
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Row {
    id: String,
    group: String,
    label: String,
    unit: String,
    higher_is_better: bool,
    ours: Option<f64>,
    ffmpeg: Option<f64>,
    speedup: Option<f64>,
    ours_runs: Vec<f64>,
    ffmpeg_runs: Vec<f64>,
    route: String,
    note: String,
    details: BTreeMap<String, f64>,
    skipped: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Machine {
    cpu: String,
    threads: usize,
    gpu: String,
    driver: String,
    ffmpeg: String,
    os: String,
}

#[derive(Serialize, Deserialize)]
struct Results {
    date: String,
    command: String,
    commit: String,
    runs: usize,
    input: String,
    machine: Machine,
    load_average_start: String,
    load_average_end: String,
    rows: Vec<Row>,
}

#[derive(Default)]
struct Measured {
    ours: Vec<f64>,
    ffmpeg: Vec<f64>,
    route: String,
    note: String,
    details: BTreeMap<String, f64>,
}

fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let m = s.len() / 2;
    Some(if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    })
}

fn render_node() -> String {
    let mut nodes: Vec<PathBuf> = fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "/dev/dri/renderD128".into())
}

fn ffmpeg_run(args: &[&str], info: bool) -> Res<(f64, String)> {
    let exe = FFMPEG
        .get()
        .ok_or("ffmpeg runs only under bench --record-ffmpeg")?;
    let t = Instant::now();
    let out = Command::new(exe)
        .args([
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            if info { "info" } else { "error" },
        ])
        .args(args)
        .output()?;
    let secs = t.elapsed().as_secs_f64();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(format!("ffmpeg {}: {}", args.join(" "), stderr.trim()).into());
    }
    Ok((
        secs,
        format!("{}{stderr}", String::from_utf8_lossy(&out.stdout)),
    ))
}

fn ffmpeg_time(args: &[&str]) -> Res<f64> {
    Ok(ffmpeg_run(args, false)?.0)
}

fn path_str(p: &Path) -> String {
    p.display().to_string()
}

fn repeat(runs: usize, mut f: impl FnMut() -> Res<f64>) -> Res<Vec<f64>> {
    (0..runs).map(|_| f()).collect()
}

struct Assets {
    dir: PathBuf,
    speech: PathBuf,
    video: HashMap<(Codec, u32), PathBuf>,
    raw: BTreeMap<u32, PathBuf>,
    pcm: PathBuf,
}

fn speech_media(fixtures: &Path) -> Res<(PathBuf, PathBuf)> {
    let clip = fixtures.join("clips/resilience.mp4");
    let speech = fixtures.join("clips/solstice.mp4");
    if !clip.is_file() || !speech.is_file() {
        return Err(format!(
            "{} needs clips/resilience.mp4 and clips/solstice.mp4",
            fixtures.display()
        )
        .into());
    }
    Ok((clip, speech))
}

fn prepare(dir: &Path, fixtures: &Path) -> Res<Assets> {
    let (clip, speech) = speech_media(fixtures)?;
    fs::create_dir_all(dir)?;
    let mut video = HashMap::new();
    for &(w, h) in sizes() {
        for codec in [Codec::H264, Codec::Hevc] {
            let name = match codec {
                Codec::H264 => format!("h264-{h}.mp4"),
                _ => format!("hevc-{h}.mp4"),
            };
            let out = dir.join(&name);
            if !out.exists() {
                note!("bench: encoding {name}");
                let vf = format!("scale={w}:{h}:flags=lanczos");
                let secs = CLIP_SECONDS.to_string();
                let mut a = vec![
                    "-t",
                    &secs,
                    "-i",
                    clip.to_str().ok_or("path")?,
                    "-vf",
                    &vf,
                    "-pix_fmt",
                    "yuv420p",
                    "-g",
                    "120",
                ];
                if codec == Codec::H264 {
                    a.extend(["-c:v", "libx264", "-preset", "medium", "-crf", "20"]);
                } else {
                    a.extend([
                        "-c:v",
                        "libx265",
                        "-preset",
                        "fast",
                        "-crf",
                        "23",
                        "-tag:v",
                        "hvc1",
                        "-x265-params",
                        "log-level=error",
                    ]);
                }
                a.extend(["-c:a", "copy", "-movflags", "+faststart"]);
                let part = dir.join(format!("part-{name}"));
                a.push(part.to_str().ok_or("path")?);
                ffmpeg_time(&a)?;
                fs::rename(&part, &out)?;
            }
            video.insert((codec, h), out);
        }
    }
    let mut raw = BTreeMap::new();
    for &(w, h) in sizes() {
        let out = dir.join(format!("raw-{h}.yuv"));
        if !out.exists() {
            note!("bench: raw {w}x{h} frames");
            let src = &video[&(Codec::H264, h)];
            let part = dir.join(format!("part-raw-{h}.yuv"));
            ffmpeg_time(&[
                "-i",
                src.to_str().ok_or("path")?,
                "-frames:v",
                &ENCODE_FRAMES.to_string(),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "yuv420p",
                part.to_str().ok_or("path")?,
            ])?;
            fs::rename(&part, &out)?;
        }
        raw.insert(h, out);
    }
    let pcm = dir.join("audio.f32");
    if !pcm.exists() {
        note!("bench: speech PCM");
        let part = dir.join("part-audio.f32");
        ffmpeg_time(&[
            "-i",
            speech.to_str().ok_or("path")?,
            "-vn",
            "-f",
            "f32le",
            "-ac",
            "2",
            "-ar",
            "48000",
            part.to_str().ok_or("path")?,
        ])?;
        fs::rename(&part, &pcm)?;
    }
    Ok(Assets {
        dir: dir.to_path_buf(),
        speech,
        video,
        raw,
        pcm,
    })
}

fn annexb_into(out: &mut Vec<u8>, data: &[u8], length: usize) {
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

#[derive(Clone, Copy, PartialEq)]
enum Dec {
    Cpu(usize),
    GpuRead,
    GpuSurface,
}

fn packet_of(r: &Reader, track: usize, index: usize, buf: &mut Vec<u8>) -> Res<Packet> {
    let t = r.tracks.get(track).ok_or("no such track")?;
    let s = *t.samples.get(index).ok_or("no such sample")?;
    r.read_into(track, index, buf)?;
    Ok(Packet {
        data: std::mem::take(buf),
        pts: s.pts.saturating_add(t.edit_offset),
        dts: s.dts.saturating_add(t.edit_offset),
        keyframe: s.keyframe,
    })
}

fn frame_count(path: &Path) -> Res<u64> {
    let r = Reader::open(path)?;
    let track = r.video().ok_or("no video track")?;
    Ok(r.tracks[track].samples.len() as u64)
}

fn decode_ours(path: &Path, mode: Dec) -> Res<(u64, f64)> {
    let t0 = Instant::now();
    let r = Reader::open(path)?;
    let track = r.video().ok_or("no video track")?;
    let t = &r.tracks[track];
    let tb = TimeBase::new(1, t.timescale.max(1));
    let count = t.samples.len();
    let mut buf = Vec::new();
    let mut frames = 0u64;
    match mode {
        Dec::Cpu(threads) => match t.config() {
            Some(CodecConfig::Avc(c)) => {
                let mut dec = h264::Decoder::with_threads(threads);
                dec.set_time_base(tb);
                let mut params = Vec::new();
                for nal in c.sps.iter().chain(&c.pps) {
                    params.extend_from_slice(&[0, 0, 0, 1]);
                    params.extend_from_slice(nal);
                }
                let length = c.length_size.clamp(1, 4) as usize;
                let mut annexb = Vec::new();
                for i in 0..count {
                    let p = packet_of(&r, track, i, &mut buf)?;
                    annexb.clear();
                    if p.keyframe {
                        annexb.extend_from_slice(&params);
                    }
                    annexb_into(&mut annexb, &p.data, length);
                    buf = p.data;
                    let packet = Packet {
                        data: std::mem::take(&mut annexb),
                        pts: p.pts,
                        dts: p.dts,
                        keyframe: p.keyframe,
                    };
                    let out = dec.decode(&packet)?;
                    annexb = packet.data;
                    frames += out.len() as u64;
                    for f in out {
                        dec.recycle(f);
                    }
                }
                frames += dec.flush()?.len() as u64;
            }
            Some(CodecConfig::Hevc(_)) => {
                let mut dec = hevc::Decoder::with_threads(threads);
                dec.configure_hvcc(&t.extradata().ok_or("no hvcC")?)?;
                dec.set_time_base(tb);
                for i in 0..count {
                    let p = packet_of(&r, track, i, &mut buf)?;
                    frames += dec.decode(&p)?.len() as u64;
                    buf = p.data;
                }
                frames += dec.flush()?.len() as u64;
            }
            _ => return Err("not H.264 or HEVC".into()),
        },
        Dec::GpuRead | Dec::GpuSurface => {
            let codec = t.codec().ok_or("unknown codec")?;
            let mut dec =
                HwDecoder::new(codec, &t.extradata().unwrap_or_default())?.with_time_base(tb);
            for i in 0..count {
                let p = packet_of(&r, track, i, &mut buf)?;
                frames += if mode == Dec::GpuRead {
                    dec.decode(&p)?.len()
                } else {
                    dec.decode_gpu(&p)?.len()
                } as u64;
                buf = p.data;
            }
            frames += if mode == Dec::GpuRead {
                dec.flush()?.len()
            } else {
                dec.flush_gpu()?.len()
            } as u64;
        }
    }
    Ok((frames, t0.elapsed().as_secs_f64()))
}

fn load_raw(path: &Path, w: u32, h: u32) -> Res<Vec<Frame>> {
    let bytes = fs::read(path)?;
    let (y, c) = (w as usize * h as usize, (w as usize / 2) * (h as usize / 2));
    let each = y + 2 * c;
    if each == 0 {
        return Err(format!("{}: a {w}x{h} frame is empty", path.display()).into());
    }
    let tb = TimeBase::new(1, 60);
    bytes
        .chunks_exact(each)
        .enumerate()
        .map(|(i, b)| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i as i64, tb);
            let parts = [&b[..y], &b[y..y + c], &b[y + c..]];
            for (plane, part) in f.planes.iter_mut().zip(parts) {
                if plane.data.len() != part.len() {
                    return Err(format!(
                        "{}: {w}x{h} planes don't match the frame size",
                        path.display()
                    )
                    .into());
                }
                plane.data.copy_from_slice(part);
            }
            Ok(f)
        })
        .collect()
}

fn encode_cpu(frames: &[Frame], w: u32, h: u32) -> Res<(f64, Vec<u8>)> {
    let t = Instant::now();
    let mut enc = h264::Encoder::new(h264::EncoderConfig {
        width: w,
        height: h,
        fps: (60, 1),
        rate: pito_codecs::media::Rate::Crf(23),
        gop: 250,
        b_frames: 2,
        bitstream: pito_codecs::media::Bitstream::AnnexB,
        ..h264::EncoderConfig::default()
    })?;
    let mut out = Vec::new();
    for f in frames {
        for p in enc.encode(f)? {
            out.extend_from_slice(&p.data);
        }
    }
    for p in enc.flush()? {
        out.extend_from_slice(&p.data);
    }
    Ok((t.elapsed().as_secs_f64(), out))
}

fn encode_gpu(frames: &[Frame], w: u32, h: u32, codec: Codec) -> Res<(f64, Vec<u8>, String)> {
    let t = Instant::now();
    let mut enc = HwEncoder::new(
        codec,
        &gpu::EncoderConfig {
            width: w,
            height: h,
            fps: (60, 1),
            profile: gpu::Profile::High,
            rate: pito_codecs::media::Rate::Qp(23),
            gop: 250,
            b_frames: 0,
            bitstream: pito_codecs::media::Bitstream::AnnexB,
            ..Default::default()
        },
    )?;
    let name = enc.name().to_string();
    let mut out = Vec::new();
    for f in frames {
        for p in enc.encode(f)? {
            out.extend_from_slice(&p.data);
        }
    }
    for p in enc.flush()? {
        out.extend_from_slice(&p.data);
    }
    Ok((t.elapsed().as_secs_f64(), out, name))
}

const BT709: &str = "setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709";

fn psnr(stream: &Path, format: &str, raw: &Path, w: u32, h: u32) -> Option<f64> {
    let size = format!("{w}x{h}");
    let (_, log) = ffmpeg_run(
        &[
            "-framerate",
            "60",
            "-f",
            format,
            "-i",
            stream.to_str()?,
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &size,
            "-framerate",
            "60",
            "-i",
            raw.to_str()?,
            "-lavfi",
            &format!(
                "[0:v]setpts=N/(60*TB),{BT709}[a];[1:v]setpts=N/(60*TB),{BT709}[b];[a][b]psnr"
            ),
            "-f",
            "null",
            "-",
        ],
        true,
    )
    .ok()?;
    let at = log.rfind("average:")? + "average:".len();
    log[at..].split_whitespace().next()?.parse().ok()
}

struct Bench {
    runs: usize,
    only: Vec<String>,
    rows: Vec<Row>,
    node: String,
    reference: Reference,
    missing: Vec<String>,
}

fn compare(reference: &Reference, row: &mut Row) -> bool {
    let Some(c) = reference.cases.get(&row.id).filter(|c| c.unit == row.unit) else {
        return false;
    };
    row.ffmpeg = Some(c.value);
    row.ffmpeg_runs = c.runs.clone();
    row.details.extend(c.details.clone());
    true
}

fn record(reference: &mut Reference, row: &Row) {
    let Some(value) = row.ffmpeg else {
        return;
    };
    let details = row
        .details
        .iter()
        .filter(|(k, _)| k.starts_with("ffmpeg_"))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    reference.cases.insert(
        row.id.clone(),
        Case {
            unit: row.unit.clone(),
            value,
            runs: row.ffmpeg_runs.clone(),
            details,
        },
    );
}

impl Bench {
    fn want(&self, ids: &[&str]) -> bool {
        self.only.is_empty()
            || ids.iter().any(|id| {
                self.only
                    .iter()
                    .any(|o| id.contains(o.as_str()) || o.starts_with(id))
            })
    }

    fn add(
        &mut self,
        id: &str,
        group: &str,
        label: &str,
        unit: &str,
        higher: bool,
        measured: Res<Measured>,
    ) {
        if !self.want(&[id]) {
            return;
        }
        let mut row = Row {
            id: id.into(),
            group: group.into(),
            label: label.into(),
            unit: unit.into(),
            higher_is_better: higher,
            ..Row::default()
        };
        match measured {
            Ok(m) => {
                row.ours = median(&m.ours);
                row.ffmpeg = median(&m.ffmpeg);
                row.ours_runs = m.ours;
                row.ffmpeg_runs = m.ffmpeg;
                row.route = m.route;
                row.note = m.note;
                row.details = m.details;
                if recording() {
                    record(&mut self.reference, &row);
                } else if !compare(&self.reference, &mut row) {
                    self.missing.push(row.id.clone());
                }
                row.speedup = row
                    .ours
                    .zip(row.ffmpeg)
                    .map(|(o, f)| if higher { o / f } else { f / o });
            }
            Err(e) => row.skipped = Some(e.to_string()),
        }
        let _ = say!(
            "{:<44} ours {:>10} ffmpeg {:>10} {}  {}",
            row.id,
            row.ours.map_or("-".into(), |v| format!("{v:.1}")),
            row.ffmpeg.map_or("-".into(), |v| format!("{v:.1}")),
            row.speedup.map_or("".into(), |s| format!("{s:.2}x")),
            row.skipped.as_deref().unwrap_or("")
        );
        self.rows.push(row);
    }

    fn decode(&mut self, a: &Assets) {
        for codec in [Codec::H264, Codec::Hevc] {
            for &(_, h) in sizes() {
                let path = a.video[&(codec, h)].clone();
                let pstr = path_str(&path);
                let all = std::thread::available_parallelism()
                    .map_or(4, |n| n.get())
                    .min(16);
                let modes = [
                    ("cpu-1t", "CPU, 1 thread", Dec::Cpu(1)),
                    ("cpu-all", "CPU, all threads", Dec::Cpu(all)),
                    ("gpu-readback", "VA-API, frames read back", Dec::GpuRead),
                    ("gpu-surface", "VA-API, surface only", Dec::GpuSurface),
                ];
                for (key, text, mode) in modes {
                    let id = format!(
                        "decode.{}.{h}.{key}",
                        codec.to_string().to_lowercase().replace('.', "")
                    );
                    if !self.want(&[&id]) {
                        continue;
                    }
                    let label = format!("{codec} {h}P DECODE, {text}");
                    let runs = self.runs;
                    let node = self.node.clone();
                    let m = (|| -> Res<Measured> {
                        let mut m = Measured {
                            route: if matches!(mode, Dec::Cpu(_)) {
                                "cpu"
                            } else {
                                "gpu"
                            }
                            .into(),
                            ..Measured::default()
                        };
                        let mut n = frame_count(&path)?;
                        if !recording() {
                            decode_ours(&path, mode)?;
                            for _ in 0..runs {
                                let (count, s) = decode_ours(&path, mode)?;
                                n = count;
                                m.ours.push(count as f64 / s);
                            }
                        }
                        let threads = match mode {
                            Dec::Cpu(1) => vec!["-threads", "1"],
                            Dec::Cpu(_) => vec!["-threads", "0"],
                            Dec::GpuRead => vec!["-hwaccel", "vaapi", "-hwaccel_device", &node],
                            Dec::GpuSurface => vec![
                                "-hwaccel",
                                "vaapi",
                                "-hwaccel_device",
                                &node,
                                "-hwaccel_output_format",
                                "vaapi",
                            ],
                        };
                        let mut args = threads;
                        args.extend([
                            "-i",
                            &pstr,
                            "-an",
                            "-fps_mode",
                            "passthrough",
                            "-f",
                            "null",
                            "-",
                        ]);
                        if recording() {
                            ffmpeg_time(&args)?;
                            m.ffmpeg = repeat(runs, || Ok(n as f64 / ffmpeg_time(&args)?))?;
                        }
                        m.details.insert("frames".into(), n as f64);
                        Ok(m)
                    })();
                    self.add(&id, "decode", &label, "fps", true, m);
                }
            }
        }
    }

    fn encode(&mut self, a: &Assets) {
        for &(w, h) in sizes() {
            let id_prefix = format!("encode.{h}");
            if !self.want(&[&id_prefix]) {
                continue;
            }
            let Some(raw) = a.raw.get(&h).cloned() else {
                continue;
            };
            let frames = match load_raw(&raw, w, h) {
                Ok(f) => f,
                Err(e) => {
                    note!("bench: {e}");
                    continue;
                }
            };
            let n = frames.len() as f64;
            let size = format!("{w}x{h}");
            let rawstr = path_str(&raw);
            let runs = self.runs;
            let node = self.node.clone();
            let dir = a.dir.clone();
            let ffmpeg_args = |codec: &[&str], format: &str, out: &str, hw: bool| -> Vec<String> {
                let mut v: Vec<String> = Vec::new();
                if hw {
                    v.extend(["-vaapi_device".into(), node.clone()]);
                }
                v.extend(
                    [
                        "-f",
                        "rawvideo",
                        "-pix_fmt",
                        "yuv420p",
                        "-s",
                        &size,
                        "-framerate",
                        "60",
                        "-i",
                        &rawstr,
                    ]
                    .map(String::from),
                );
                if hw {
                    v.extend(["-vf".into(), "format=nv12,hwupload".into()]);
                }
                v.extend(codec.iter().map(|s| s.to_string()));
                v.extend(["-f".into(), format.into(), out.into()]);
                v
            };
            let run_ff = |args: &[String]| -> Res<f64> {
                let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                ffmpeg_time(&refs)
            };
            let with_quality = |m: &mut Measured, ours: &[u8], theirs: &Path, fmt: &str| {
                if !recording() {
                    m.details.insert("ours_bytes".into(), ours.len() as f64);
                    return;
                }
                if let Some(p) = psnr(theirs, fmt, &raw, w, h) {
                    m.details.insert("ffmpeg_psnr_db".into(), p);
                }
                m.details.insert(
                    "ffmpeg_bytes".into(),
                    fs::metadata(theirs).map_or(0.0, |x| x.len() as f64),
                );
            };

            for (key, preset) in [("medium", "medium"), ("veryfast", "veryfast")] {
                let id = format!("encode.{h}.h264.cpu.{key}");
                if !self.want(&[&id]) {
                    continue;
                }
                let m = (|| -> Res<Measured> {
                    let mut m = Measured {
                        route: "cpu".into(),
                        note: format!(
                            "ours: Balanced preset CRF 23, 2 B-frames, GOP 250; libx264 {preset} CRF 23, 2 B-frames, GOP 250; {ENCODE_FRAMES} frames"
                        ),
                        ..Measured::default()
                    };
                    let mut last = Vec::new();
                    for _ in 0..ours_runs(runs) {
                        let (s, out) = encode_cpu(&frames, w, h)?;
                        m.ours.push(n / s);
                        last = out;
                    }
                    let out = dir.join(format!("ffmpeg-{h}-{key}.h264"));
                    let args = ffmpeg_args(
                        &[
                            "-c:v", "libx264", "-preset", preset, "-crf", "23", "-bf", "2", "-g",
                            "250",
                        ],
                        "h264",
                        &path_str(&out),
                        false,
                    );
                    for _ in 0..ffmpeg_runs(runs) {
                        m.ffmpeg.push(n / run_ff(&args)?);
                    }
                    with_quality(&mut m, &last, &out, "h264");
                    Ok(m)
                })();
                self.add(
                    &id,
                    "encode",
                    &format!(
                        "H.264 {h}P ENCODE, CPU vs LIBX264 {}",
                        preset.to_uppercase()
                    ),
                    "fps",
                    true,
                    m,
                );
            }

            for (codec, name, fmt, encoder) in [
                (Codec::H264, "h264", "h264", "h264_vaapi"),
                (Codec::Hevc, "hevc", "hevc", "hevc_vaapi"),
            ] {
                let id = format!("encode.{h}.{name}.gpu");
                if !self.want(&[&id]) {
                    continue;
                }
                let m = (|| -> Res<Measured> {
                    let mut m = Measured {
                        route: "gpu".into(),
                        note: format!(
                            "ours: VA-API constant QP 23, no B-frames, GOP 250; ffmpeg {encoder} -qp 23, same; {ENCODE_FRAMES} frames from system memory"
                        ),
                        ..Measured::default()
                    };
                    let mut last = Vec::new();
                    if !recording() {
                        let (_, _, driver) = encode_gpu(&frames[..8], w, h, codec)?;
                        m.note.push_str(&format!("; backend {driver}"));
                    }
                    for _ in 0..ours_runs(runs) {
                        let (s, out, _) = encode_gpu(&frames, w, h, codec)?;
                        m.ours.push(n / s);
                        last = out;
                    }
                    let out = dir.join(format!("ffmpeg-{h}-gpu.{name}"));
                    let args = ffmpeg_args(
                        &["-c:v", encoder, "-qp", "23", "-bf", "0", "-g", "250"],
                        fmt,
                        &path_str(&out),
                        true,
                    );
                    for _ in 0..ffmpeg_runs(runs) {
                        m.ffmpeg.push(n / run_ff(&args)?);
                    }
                    with_quality(&mut m, &last, &out, fmt);
                    Ok(m)
                })();
                self.add(
                    &id,
                    "encode",
                    &format!("{} {h}P ENCODE, VA-API", codec.to_string().to_uppercase()),
                    "fps",
                    true,
                    m,
                );
            }
        }
    }

    fn transcode(&mut self, a: &Assets) {
        for codec in [Codec::H264, Codec::Hevc] {
            let (_, h) = *sizes().last().unwrap_or(&ALL_SIZES[0]);
            let name = codec.to_string().to_lowercase().replace('.', "");
            let id = format!("transcode.{name}.{h}.gpu");
            if !self.want(&[&id]) {
                continue;
            }
            let path = a.video[&(codec, h)].clone();
            let pstr = path_str(&path);
            let out = path_str(&a.dir.join(format!(
                "ffmpeg-transcode-{name}.{}",
                if codec == Codec::H264 { "h264" } else { "hevc" }
            )));
            let runs = self.runs;
            let node = self.node.clone();
            let m = (|| -> Res<Measured> {
                let mut m = Measured {
                    route: "gpu".into(),
                    note: "decode on the GPU, encode on the GPU, frames stay on the GPU; constant QP 23, no B-frames".into(),
                    ..Measured::default()
                };
                let one = || -> Res<(u64, f64)> {
                    let t0 = Instant::now();
                    let r = Reader::open(&path)?;
                    let track = r.video().ok_or("no video")?;
                    let t = &r.tracks[track];
                    let mut dec = HwDecoder::new(codec, &t.extradata().unwrap_or_default())?
                        .with_time_base(TimeBase::new(1, t.timescale.max(1)));
                    let mut enc = HwEncoder::new(
                        codec,
                        &gpu::EncoderConfig {
                            width: t.width,
                            height: t.height,
                            fps: (60, 1),
                            profile: gpu::Profile::High,
                            rate: pito_codecs::media::Rate::Qp(23),
                            gop: 250,
                            b_frames: 0,
                            bitstream: pito_codecs::media::Bitstream::AnnexB,
                            ..Default::default()
                        },
                    )?;
                    let mut buf = Vec::new();
                    let mut frames = 0u64;
                    for i in 0..t.samples.len() {
                        let p = packet_of(&r, track, i, &mut buf)?;
                        for f in dec.decode_gpu(&p)? {
                            enc.encode_gpu(&f)?;
                            frames += 1;
                        }
                        buf = p.data;
                    }
                    for f in dec.flush_gpu()? {
                        enc.encode_gpu(&f)?;
                        frames += 1;
                    }
                    enc.flush()?;
                    Ok((frames, t0.elapsed().as_secs_f64()))
                };
                let mut n = frame_count(&path)?;
                if !recording() {
                    one()?;
                    for _ in 0..runs {
                        let (c, s) = one()?;
                        n = c;
                        m.ours.push(c as f64 / s);
                    }
                }
                let encoder = if codec == Codec::H264 {
                    "h264_vaapi"
                } else {
                    "hevc_vaapi"
                };
                let args = [
                    "-hwaccel",
                    "vaapi",
                    "-hwaccel_device",
                    &node,
                    "-hwaccel_output_format",
                    "vaapi",
                    "-i",
                    &pstr,
                    "-an",
                    "-c:v",
                    encoder,
                    "-qp",
                    "23",
                    "-bf",
                    "0",
                    "-g",
                    "250",
                    "-f",
                    if codec == Codec::H264 { "h264" } else { "hevc" },
                    &out,
                ];
                for _ in 0..ffmpeg_runs(runs) {
                    m.ffmpeg.push(n as f64 / ffmpeg_time(&args)?);
                }
                Ok(m)
            })();
            self.add(
                &id,
                "encode",
                &format!("{codec} {h}P GPU TRANSCODE, SURFACES"),
                "fps",
                true,
                m,
            );
        }
    }

    fn audio(&mut self, a: &Assets) {
        let speech = a.speech.clone();
        let sstr = path_str(&speech);
        let pcm = a.pcm.clone();
        let pstr = path_str(&pcm);
        let runs = self.runs;
        let dir = a.dir.clone();
        let seconds = fs::metadata(&pcm).map_or(0.0, |m| m.len() as f64 / (48000.0 * 8.0));
        let id = "audio.aac.decode";
        if self.want(&[id]) {
            let m = (|| -> Res<Measured> {
                let one = || -> Res<f64> {
                    let t0 = Instant::now();
                    let r = Reader::open(&speech)?;
                    let i = *r.audio().first().ok_or("no audio")?;
                    let mut dec = aac::Decoder::new(&r.tracks[i].extradata().ok_or("no config")?)?;
                    let mut buf = Vec::new();
                    for k in 0..r.tracks[i].samples.len() {
                        r.read_into(i, k, &mut buf)?;
                        dec.decode_frame(&buf)?;
                    }
                    Ok(t0.elapsed().as_secs_f64())
                };
                let mut m = Measured {
                    route: "cpu".into(),
                    note: format!("{seconds:.0} s of 48 kHz stereo AAC-LC"),
                    ..Measured::default()
                };
                if !recording() {
                    one()?;
                    m.ours = repeat(runs, || Ok(seconds / one()?))?;
                }
                let args = ["-i", &sstr, "-vn", "-f", "null", "-"];
                if recording() {
                    ffmpeg_time(&args)?;
                    m.ffmpeg = repeat(runs, || Ok(seconds / ffmpeg_time(&args)?))?;
                }
                Ok(m)
            })();
            self.add(
                id,
                "audio",
                "AAC DECODE, 48 KHZ STEREO",
                "x realtime",
                true,
                m,
            );
        }
        let id = "audio.aac.encode";
        if self.want(&[id]) {
            let m = (|| -> Res<Measured> {
                let bytes = fs::read(&pcm)?;
                let samples: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                let one = || -> Res<(f64, usize)> {
                    let t0 = Instant::now();
                    let mut enc = aac::Encoder::new(aac::EncoderConfig {
                        rate: 48000,
                        channels: 2,
                        bitrate: 128000,
                    })?;
                    let mut size = 0usize;
                    for chunk in samples.chunks(48000 * 2) {
                        let buf = AudioBuffer {
                            samples: chunk.to_vec(),
                            rate: 48000,
                            channels: 2,
                        };
                        size += enc
                            .encode(&buf)?
                            .iter()
                            .map(|p| p.data.len())
                            .sum::<usize>();
                    }
                    size += enc.flush().iter().map(|p| p.data.len()).sum::<usize>();
                    Ok((t0.elapsed().as_secs_f64(), size))
                };
                let mut m = Measured {
                    route: "cpu".into(),
                    note: format!(
                        "{seconds:.0} s of 48 kHz stereo at 128 kbps; ffmpeg's native aac encoder"
                    ),
                    ..Measured::default()
                };
                if !recording() {
                    let (_, size) = one()?;
                    m.ours = repeat(runs, || Ok(seconds / one()?.0))?;
                    m.details.insert("ours_bytes".into(), size as f64);
                }
                let out = path_str(&dir.join("ffmpeg.aac"));
                let args = [
                    "-f", "f32le", "-ar", "48000", "-ac", "2", "-i", &pstr, "-c:a", "aac", "-b:a",
                    "128k", "-f", "adts", &out,
                ];
                if recording() {
                    ffmpeg_time(&args)?;
                    m.ffmpeg = repeat(runs, || Ok(seconds / ffmpeg_time(&args)?))?;
                    m.details.insert(
                        "ffmpeg_bytes".into(),
                        fs::metadata(&out).map_or(0.0, |x| x.len() as f64),
                    );
                }
                Ok(m)
            })();
            self.add(
                id,
                "audio",
                "AAC ENCODE, 128 KBPS STEREO",
                "x realtime",
                true,
                m,
            );
        }
        let id = "audio.r128";
        if self.want(&[id]) {
            let m = (|| -> Res<Measured> {
                let bytes = fs::read(&pcm)?;
                let samples: Vec<f32> = bytes
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect();
                let one = || -> Res<(f64, f64)> {
                    let t0 = Instant::now();
                    let mut meter = LoudnessMeter::new(48000, 2)?;
                    for chunk in samples.chunks(48000 * 2) {
                        meter.process(&AudioBuffer {
                            samples: chunk.to_vec(),
                            rate: 48000,
                            channels: 2,
                        })?;
                    }
                    Ok((t0.elapsed().as_secs_f64(), meter.summary().integrated))
                };
                let mut m = Measured {
                    route: "cpu".into(),
                    note: format!(
                        "{seconds:.0} s of 48 kHz stereo, integrated loudness, range and true peak"
                    ),
                    ..Measured::default()
                };
                if !recording() {
                    let (_, lufs) = one()?;
                    m.ours = repeat(runs, || Ok(seconds / one()?.0))?;
                    m.details.insert("ours_lufs".into(), lufs);
                }
                let args = [
                    "-f",
                    "f32le",
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                    "-i",
                    &pstr,
                    "-af",
                    "ebur128=peak=true",
                    "-f",
                    "null",
                    "-",
                ];
                if !recording() {
                    return Ok(m);
                }
                let (_, log) = ffmpeg_run(&args, true)?;
                if let Some(at) = log.rfind("I:")
                    && let Some(v) = log[at + 2..]
                        .split_whitespace()
                        .next()
                        .and_then(|v| v.parse().ok())
                {
                    m.details.insert("ffmpeg_lufs".into(), v);
                }
                m.ffmpeg = repeat(runs, || Ok(seconds / ffmpeg_run(&args, true)?.0))?;
                Ok(m)
            })();
            self.add(
                id,
                "audio",
                "EBU R128 LOUDNESS, 48 KHZ STEREO",
                "x realtime",
                true,
                m,
            );
        }
    }

    fn first_frame(&mut self, a: &Assets) {
        let (_, h) = *sizes().last().unwrap_or(&ALL_SIZES[0]);
        for codec in [Codec::H264, Codec::Hevc] {
            for (key, text, choice) in [("cpu", "CPU", Choice::Cpu), ("gpu", "GPU", Choice::Gpu)] {
                let name = codec.to_string().to_lowercase().replace('.', "");
                let id = format!("first-frame.{name}.{h}.{key}");
                if !self.want(&[&id]) {
                    continue;
                }
                let path = a.video[&(codec, h)].clone();
                let node = self.node.clone();
                let dir = a.dir.join("ttff");
                let runs = if QUICK.load(Ordering::Relaxed) {
                    3
                } else {
                    self.runs.max(5)
                };
                let m = (|| -> Res<Measured> {
                    fs::create_dir_all(&dir)?;
                    let prefs = Prefs {
                        decode: choice.clone(),
                        ..Prefs::default()
                    };
                    let mut m = Measured {
                        route: key.into(),
                        note: "a cold file each time: opened, parsed, first frame decoded, scaled to 240 px; ffmpeg includes starting the process".into(),
                        ..Measured::default()
                    };
                    for i in 0..ours_runs(runs) {
                        let copy = dir.join(format!("{name}-{key}-{i}.mp4"));
                        let _ = fs::remove_file(&copy);
                        fs::hard_link(&path, &copy)?;
                        let t0 = Instant::now();
                        first_frame(&copy, 240, &prefs)?;
                        m.ours.push(t0.elapsed().as_secs_f64() * 1e3);
                    }
                    if let Some(&first) = m.ours.first() {
                        m.details.insert("ours_first_call_ms".into(), first);
                    }
                    let pstr = path_str(&path);
                    let mut args = vec![];
                    if key == "gpu" {
                        args.extend(["-hwaccel", "vaapi", "-hwaccel_device", &node]);
                    }
                    args.extend([
                        "-i",
                        &pstr,
                        "-frames:v",
                        "1",
                        "-vf",
                        "scale=-2:240",
                        "-f",
                        "null",
                        "-",
                    ]);
                    m.ffmpeg = repeat(ffmpeg_runs(runs), || Ok(ffmpeg_time(&args)? * 1e3))?;
                    Ok(m)
                })();
                self.add(
                    &id,
                    "pipeline",
                    &format!("{codec} {h}P TIME TO FIRST FRAME, {text}"),
                    "ms",
                    false,
                    m,
                );
            }
        }
    }

    fn filmstrip(&mut self, a: &Assets) {
        for codec in [Codec::H264, Codec::Hevc] {
            for &(_, h) in sizes() {
                for (key, text, choice) in [
                    ("cpu", "CPU", Choice::Cpu),
                    ("auto", "GPU FIRST", Choice::Auto),
                ] {
                    let name = codec.to_string().to_lowercase().replace('.', "");
                    let id = format!("filmstrip.{name}.{h}.{key}");
                    if !self.want(&[&id]) {
                        continue;
                    }
                    let path = a.video[&(codec, h)].clone();
                    let pstr = path_str(&path);
                    let runs = self.runs;
                    let m = (|| -> Res<Measured> {
                        let prefs = Prefs {
                            decode: choice.clone(),
                            ..Prefs::default()
                        };
                        let times: Vec<f64> = (0..12)
                            .map(|k| CLIP_SECONDS as f64 * (k as f64 + 0.5) / 12.0)
                            .collect();
                        let mut m = Measured {
                            route: key.into(),
                            note: "12 stills at 240 px, snapped to keyframes; ffmpeg runs 12 processes (-noaccurate_seek), 8 at a time".into(),
                            ..Measured::default()
                        };
                        if !recording() {
                            frames_at(&path, &times, 240, &prefs)?;
                            m.ours = repeat(runs, || {
                                let t0 = Instant::now();
                                frames_at(&path, &times, 240, &prefs)?;
                                Ok(12.0 / t0.elapsed().as_secs_f64())
                            })?;
                            return Ok(m);
                        }
                        let strs: Vec<String> = times.iter().map(|t| format!("{t:.3}")).collect();
                        let one = |t: &str| -> Res<f64> {
                            ffmpeg_time(&[
                                "-ss",
                                t,
                                "-noaccurate_seek",
                                "-i",
                                &pstr,
                                "-frames:v",
                                "1",
                                "-vf",
                                "scale=-2:240",
                                "-f",
                                "null",
                                "-",
                            ])
                        };
                        let parallel = || -> Res<f64> {
                            let t0 = Instant::now();
                            let next = std::sync::atomic::AtomicUsize::new(0);
                            let failed = std::sync::Mutex::new(None::<String>);
                            std::thread::scope(|s| {
                                for _ in 0..8 {
                                    s.spawn(|| {
                                        loop {
                                            let i = next
                                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                            let Some(t) = strs.get(i) else { break };
                                            if let Err(e) = one(t)
                                                && let Ok(mut slot) = failed.lock()
                                            {
                                                *slot = Some(e.to_string());
                                            }
                                        }
                                    });
                                }
                            });
                            if let Some(e) = failed.into_inner().ok().flatten() {
                                return Err(e.into());
                            }
                            Ok(12.0 / t0.elapsed().as_secs_f64())
                        };
                        parallel()?;
                        m.ffmpeg = repeat(runs, parallel)?;
                        let t0 = Instant::now();
                        for t in &strs {
                            one(t)?;
                        }
                        m.details.insert(
                            "ffmpeg_sequential".into(),
                            12.0 / t0.elapsed().as_secs_f64(),
                        );
                        Ok(m)
                    })();
                    self.add(
                        &id,
                        "pipeline",
                        &format!("{codec} {h}P FILMSTRIP STILLS, {text}"),
                        "stills/s",
                        true,
                        m,
                    );
                }
            }
        }
    }

    fn smart_cut(&mut self, a: &Assets) {
        for &(_, h) in sizes() {
            let ids = [
                format!("smartcut.{h}.copy"),
                format!("smartcut.{h}.reencode"),
            ];
            if !self.want(&[&ids[0], &ids[1]]) {
                continue;
            }
            let path = a.video[&(Codec::H264, h)].clone();
            let pstr = path_str(&path);
            let dir = a.dir.join("smartcut");
            let runs = self.runs;
            let spans: [(f64, f64); 3] = [(2.3, 7.6), (11.1, 13.9), (14.2, 19.1)];
            let seconds: f64 = spans.iter().map(|(a, b)| b - a).sum();
            let ours = (|| -> Res<(Vec<f64>, f64, u64, u64)> {
                if recording() {
                    return Ok((Vec::new(), 0.0, 0, 0));
                }
                fs::create_dir_all(&dir)?;
                let out = dir.join("ours.mp4");
                let pieces: Vec<Piece> = spans
                    .iter()
                    .map(|(from, to)| Piece {
                        path: path.clone(),
                        from: *from,
                        to: *to,
                    })
                    .collect();
                let one = || -> Res<(f64, u64, u64)> {
                    let t0 = Instant::now();
                    let done = export(&out, &pieces, ExportOptions::default(), |_| true)?;
                    Ok((t0.elapsed().as_secs_f64(), done.copied, done.encoded))
                };
                let (_, copied, encoded) = one()?;
                let times = repeat(runs, || Ok(seconds / one()?.0))?;
                Ok((times, fs::metadata(&out)?.len() as f64, copied, encoded))
            })();
            let theirs = |reencode: bool| -> Res<(Vec<f64>, f64)> {
                fs::create_dir_all(&dir)?;
                let list = dir.join("list.txt");
                let out = dir.join(if reencode {
                    "ffmpeg-re.mp4"
                } else {
                    "ffmpeg-copy.mp4"
                });
                let one = || -> Res<f64> {
                    let mut total = 0.0;
                    let mut entries = String::new();
                    for (i, (from, to)) in spans.iter().enumerate() {
                        let piece = dir.join(format!("piece-{i}.mp4"));
                        let (f, d) = (format!("{from:.3}"), format!("{:.3}", to - from));
                        let mut args = vec!["-ss", &f, "-i", &pstr, "-t", &d];
                        if reencode {
                            args.extend([
                                "-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-c:a",
                                "aac", "-b:a", "192k",
                            ]);
                        } else {
                            args.extend(["-c", "copy", "-avoid_negative_ts", "make_zero"]);
                        }
                        let ps = path_str(&piece);
                        args.push(&ps);
                        total += ffmpeg_time(&args)?;
                        entries.push_str(&format!(
                            "file '{}'\n",
                            piece.file_name().and_then(|n| n.to_str()).unwrap_or("")
                        ));
                    }
                    fs::write(&list, entries)?;
                    total += ffmpeg_time(&[
                        "-f",
                        "concat",
                        "-safe",
                        "0",
                        "-i",
                        &path_str(&list),
                        "-c",
                        "copy",
                        &path_str(&out),
                    ])?;
                    Ok(total)
                };
                one()?;
                let times = repeat(runs, || Ok(seconds / one()?))?;
                Ok((times, fs::metadata(&out)?.len() as f64))
            };
            for (i, reencode) in [false, true].into_iter().enumerate() {
                let m = (|| -> Res<Measured> {
                    let (o, o_size, copied, encoded) =
                        ours.as_ref().map_err(|e| e.to_string())?.clone();
                    let (f, f_size) = if recording() {
                        theirs(reencode)?
                    } else {
                        (Vec::new(), 0.0)
                    };
                    let mut m = Measured {
                        route: "auto".into(),
                        note: if reencode {
                            "3 cuts, 10.1 s kept; ffmpeg re-encodes every frame with libx264 veryfast CRF 18 (frame-accurate)".into()
                        } else {
                            "3 cuts, 10.1 s kept; ffmpeg stream-copies (snaps to keyframes, not frame-accurate)".into()
                        },
                        ours: o,
                        ffmpeg: f,
                        ..Measured::default()
                    };
                    if recording() {
                        m.details.insert("ffmpeg_bytes".into(), f_size);
                    } else {
                        m.details.insert("ours_bytes".into(), o_size);
                        m.details.insert("ours_frames_copied".into(), copied as f64);
                        m.details
                            .insert("ours_frames_encoded".into(), encoded as f64);
                    }
                    Ok(m)
                })();
                self.add(
                    &ids[i],
                    "pipeline",
                    &format!(
                        "SMART-CUT EXPORT {h}P VS FFMPEG {}",
                        if reencode { "RE-ENCODE" } else { "STREAM COPY" }
                    ),
                    "x realtime",
                    true,
                    m,
                );
            }
        }
    }

    fn scale(&mut self, a: &Assets) {
        let cases = [(1440u32, 1080u32), (1440, 720), (1080, 720), (1080, 1440)];
        let cases = cases
            .into_iter()
            .filter(|(from, _)| sizes().iter().any(|s| s.1 == *from));
        for (from, to) in cases {
            let id = format!("scale.{from}to{to}");
            if !self.want(&[&id]) {
                continue;
            }
            let (sw, sh) = ALL_SIZES
                .iter()
                .find(|s| s.1 == from)
                .copied()
                .unwrap_or((1920, 1080));
            let (dw, dh) = (to * 16 / 9, to);
            let Some(raw) = a.raw.get(&from).cloned() else {
                continue;
            };
            let rawstr = path_str(&raw);
            let runs = self.runs;
            let m = (|| -> Res<Measured> {
                let frames = load_raw(&raw, sw, sh)?;
                let scaler = Scaler::new(sw, sh, dw, dh, PixelFormat::Yuv420p8, Kernel::Bicubic)?;
                let mut dst = Frame::new(PixelFormat::Yuv420p8, dw, dh, 0, TimeBase::new(1, 60));
                let one = |dst: &mut Frame| -> Res<f64> {
                    let t0 = Instant::now();
                    for f in &frames {
                        scaler.run(f, dst)?;
                    }
                    Ok(frames.len() as f64 / t0.elapsed().as_secs_f64())
                };
                if !recording() {
                    one(&mut dst)?;
                }
                let mut m = Measured {
                    route: "cpu".into(),
                    note: format!(
                        "{} frames, bicubic, 8-bit 4:2:0, all threads on both sides",
                        frames.len()
                    ),
                    ..Measured::default()
                };
                m.ours = repeat(ours_runs(runs), || one(&mut dst))?;
                let size = format!("{sw}x{sh}");
                let vf = format!("scale={dw}:{dh}:flags=bicubic");
                let n = frames.len() as f64;
                let args = [
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "yuv420p",
                    "-s",
                    &size,
                    "-framerate",
                    "60",
                    "-i",
                    &rawstr,
                    "-vf",
                    &vf,
                    "-f",
                    "null",
                    "-",
                ];
                if recording() {
                    ffmpeg_time(&args)?;
                    m.ffmpeg = repeat(runs, || Ok(n / ffmpeg_time(&args)?))?;
                }
                Ok(m)
            })();
            self.add(
                &id,
                "pipeline",
                &format!("SCALE {from}P TO {to}P, BICUBIC"),
                "fps",
                true,
                m,
            );
        }
    }
}

fn shell(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn machine(ffmpeg: &str) -> Machine {
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_default();
    let vainfo = Command::new("vainfo")
        .output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default();
    let driver = vainfo
        .lines()
        .find_map(|l| l.split("Driver version:").nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let gpu = driver
        .split(" for ")
        .nth(1)
        .map(|s| s.split(" (").next().unwrap_or(s).to_string())
        .unwrap_or_default();
    Machine {
        cpu,
        threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        gpu,
        driver,
        ffmpeg: ffmpeg.to_string(),
        os: shell("uname", &["-sr"]),
    }
}

fn fmt_value(v: f64, unit: &str) -> String {
    match unit {
        "ms" if v < 100.0 => format!("{v:.1} MS"),
        "ms" => format!("{v:.0} MS"),
        "x realtime" if v < 100.0 => format!("{v:.1}×"),
        "x realtime" => format!("{v:.0}×"),
        "stills/s" => format!("{v:.0} STILLS/S"),
        _ => format!("{v:.0} FPS"),
    }
}

fn kind(id: &str) -> (usize, &'static str) {
    match id.split('.').next().unwrap_or("") {
        "decode" if id.contains(".cpu") => (0, "DECODE, CPU"),
        "decode" => (1, "DECODE, GPU (VA-API)"),
        "encode" | "transcode" => (2, "ENCODE AND GPU TRANSCODE"),
        "audio" => (3, "AUDIO"),
        "first-frame" | "filmstrip" => (4, "FIRST FRAME AND FILMSTRIP"),
        "smartcut" => (5, "SMART-CUT EXPORT"),
        _ => (6, "SCALE"),
    }
}

fn mark(id: &str) -> &'static str {
    if id.starts_with("encode.") && id.contains(".cpu.") {
        " *"
    } else if id.starts_with("smartcut.") {
        " †"
    } else {
        ""
    }
}

const X264_NOTE: &str = "* NOT LIKE FOR LIKE: LIBX264 RUNS HAND-WRITTEN ASSEMBLY, WHILE OUR CPU H.264 ENCODER IS RUST WITH NO HAND-WRITTEN SIMD.";
const CUT_NOTE: &str = "† NOT LIKE FOR LIKE: OUR EXPORT IS FRAME-ACCURATE; IT COPIES THE MIDDLE OF EACH PIECE AND RE-ENCODES THE FRAMES BETWEEN EACH CUT AND ITS NEAREST KEYFRAME (540 OF 780 FRAMES AT 1080P HERE). FFMPEG'S STREAM COPY ENCODES NOTHING AND CUTS ON KEYFRAMES, SO IT IS NOT FRAME-ACCURATE. FFMPEG'S RE-ENCODE IS FRAME-ACCURATE AND RE-ENCODES EVERY FRAME WITH LIBX264 VERYFAST, CRF 18.";

fn strip(label: &str, word: &str) -> String {
    if word.is_empty() {
        label.to_string()
    } else {
        label.replacen(word, "", 1)
    }
}

struct Spec {
    file: &'static str,
    title: &'static str,
    unit: &'static str,
    pick: fn(&str) -> bool,
    group: fn(&str) -> (usize, &'static str),
    strip: &'static str,
    notes: &'static [&'static str],
}

fn charts(rows: &[Row], images: &Path, machine: &Machine, date: &str) -> Res<()> {
    let sub = format!("{} + {}, {}", machine.cpu, machine.gpu, date);
    let none = |_: &str| (0, "");
    let specs = [
        Spec {
            file: "bench-decode.png",
            title: "DECODE SPEED",
            unit: "FRAMES PER SECOND, HIGHER IS BETTER",
            pick: |id| id.starts_with("decode."),
            group: |id| {
                if id.contains(".cpu") {
                    (0, "CPU")
                } else {
                    (1, "GPU (VA-API)")
                }
            },
            strip: " DECODE",
            notes: &[
                "SURFACE ONLY: FRAMES STAY ON THE GPU. READ BACK: EVERY FRAME IS COPIED TO MEMORY. FFMPEG'S TIMES INCLUDE STARTING ITS PROCESS.",
            ],
        },
        Spec {
            file: "bench-encode.png",
            title: "ENCODE SPEED",
            unit: "FRAMES PER SECOND, HIGHER IS BETTER",
            pick: |id| id.starts_with("encode.") || id.starts_with("transcode."),
            group: |id| {
                if id.starts_with("transcode.") {
                    (2, "GPU TRANSCODE: DECODE AND ENCODE ON THE GPU")
                } else if id.contains(".cpu.") {
                    (0, "CPU, CRF 23 (FFMPEG: LIBX264)")
                } else {
                    (1, "GPU (VA-API), QP 23")
                }
            },
            strip: " ENCODE",
            notes: &[
                X264_NOTE,
                "BOTH SIDES ENCODE THE SAME 240 RAW FRAMES; OURS HOLDS THEM IN MEMORY, FFMPEG READS THEM FROM A FILE.",
            ],
        },
        Spec {
            file: "bench-first-frame.png",
            title: "TIME TO FIRST FRAME",
            unit: "MILLISECONDS, LOWER IS BETTER",
            pick: |id| id.starts_with("first-frame."),
            group: none,
            strip: " TIME TO FIRST FRAME",
            notes: &[
                "A COLD FILE OPENED AND ITS FIRST FRAME DECODED. OURS RUNS IN THE APP'S PROCESS; FFMPEG'S TIME INCLUDES STARTING ITS PROCESS, AS AN APP THAT CALLS FFMPEG PAYS IT.",
            ],
        },
        Spec {
            file: "bench-filmstrip.png",
            title: "FILMSTRIP SPEED",
            unit: "STILLS PER SECOND, HIGHER IS BETTER",
            pick: |id| id.starts_with("filmstrip."),
            group: none,
            strip: " FILMSTRIP STILLS",
            notes: &[
                "12 STILLS AT 240 PX, SNAPPED TO KEYFRAMES. OURS RUNS IN ONE PROCESS; FFMPEG RUNS 12 PROCESSES, 8 AT A TIME.",
            ],
        },
        Spec {
            file: "bench-export.png",
            title: "SMART-CUT EXPORT",
            unit: "× REAL TIME, HIGHER IS BETTER",
            pick: |id| id.starts_with("smartcut."),
            group: none,
            strip: "SMART-CUT EXPORT ",
            notes: &[
                "3 CUTS KEEP 10.1 S OF A 20 S CLIP. × REAL TIME IS SECONDS KEPT PER SECOND OF WORK.",
                CUT_NOTE,
            ],
        },
        Spec {
            file: "bench-scale.png",
            title: "SCALE SPEED",
            unit: "FRAMES PER SECOND, HIGHER IS BETTER",
            pick: |id| id.starts_with("scale."),
            group: none,
            strip: "SCALE ",
            notes: &[
                "BICUBIC, 8-BIT 4:2:0, ALL THREADS ON BOTH SIDES. OURS SCALES FRAMES IN MEMORY; FFMPEG READS THEM FROM A FILE.",
            ],
        },
        Spec {
            file: "bench-audio.png",
            title: "AUDIO SPEED",
            unit: "× REAL TIME, HIGHER IS BETTER",
            pick: |id| id.starts_with("audio."),
            group: none,
            strip: "",
            notes: &["× REAL TIME IS SECONDS OF AUDIO PER SECOND OF WORK."],
        },
    ];
    let done: Vec<&Row> = rows
        .iter()
        .filter(|r| r.ours.is_some() && r.ffmpeg.is_some() && r.speedup.is_some())
        .collect();
    for spec in &specs {
        let mut picked: Vec<&Row> = done
            .iter()
            .copied()
            .filter(|r| (spec.pick)(&r.id))
            .collect();
        picked.sort_by_key(|r| (spec.group)(&r.id).0);
        let entries: Vec<charts::Pair> = picked
            .iter()
            .map(|r| charts::Pair {
                group: (spec.group)(&r.id).1.into(),
                label: format!("{}{}", strip(&r.label, spec.strip), mark(&r.id)),
                ours: r.ours.unwrap_or(0.0),
                ffmpeg: r.ffmpeg.unwrap_or(0.0),
                ours_text: fmt_value(r.ours.unwrap_or(0.0), &r.unit),
                ffmpeg_text: fmt_value(r.ffmpeg.unwrap_or(0.0), &r.unit),
                speedup: r.speedup.unwrap_or(1.0),
            })
            .collect();
        if !entries.is_empty() {
            let notes: Vec<String> = spec.notes.iter().map(|n| n.to_string()).collect();
            charts::paired(
                &images.join(spec.file),
                spec.title,
                spec.unit,
                &sub,
                &notes,
                &entries,
            )?;
        }
    }
    let mut all = done;
    all.sort_by_key(|r| kind(&r.id).0);
    let ratios: Vec<charts::Ratio> = all
        .iter()
        .map(|r| charts::Ratio {
            group: kind(&r.id).1.into(),
            label: format!("{}{}", r.label, mark(&r.id)),
            speedup: r.speedup.unwrap_or(1.0),
        })
        .collect();
    if !ratios.is_empty() {
        let notes = [
            "THE SAME JOB ON BOTH SIDES, AS MANY TIMES FASTER OR SLOWER THAN FFMPEG. THE MEASURED VALUES ARE IN THE CHARTS PER UNIT.".to_string(),
            X264_NOTE.to_string(),
            "† NOT LIKE FOR LIKE: OUR EXPORT IS FRAME-ACCURATE AND RE-ENCODES THE FRAMES AROUND EACH CUT. FFMPEG'S STREAM COPY ENCODES NOTHING AND CUTS ON KEYFRAMES; ITS RE-ENCODE RE-ENCODES EVERY FRAME.".to_string(),
        ];
        charts::ratios(
            &images.join("bench-speedup.png"),
            "PITO-CODECS VS FFMPEG",
            "TIMES FASTER OR SLOWER, LOG SCALE",
            &sub,
            &notes,
            &ratios,
        )?;
    }
    Ok(())
}

fn main() -> ExitCode {
    fail::run("bench", run)
}

fn run() -> Res<()> {
    let given: Vec<String> = std::env::args().skip(1).collect();
    match given.first().map(String::as_str) {
        Some("--check") if (2..=3).contains(&given.len()) => {
            let baseline = given
                .get(2)
                .map_or(".dev/bench/baseline.json", String::as_str);
            return docs::check(Path::new(&given[1]), Path::new(baseline));
        }
        Some("--chart-docs") if (3..=4).contains(&given.len()) => {
            let reference = Reference::read(Path::new(
                given.get(3).map_or(reference::PATH, String::as_str),
            ))?;
            return docs::charts(Path::new(&given[1]), Path::new(&given[2]), &reference);
        }
        Some("--check" | "--chart-docs") => {
            return Err("usage: bench --check <new results.json> [baseline.json] | bench --chart-docs <results.json> <dir> [reference.toml]".into());
        }
        Some("--help" | "-h") => {
            say!("{USAGE}")?;
            return Ok(());
        }
        _ => {}
    }
    let mut args = given.into_iter();
    let mut runs = 3usize;
    let mut quick = false;
    let mut only = Vec::new();
    let mut out = PathBuf::from("tmp/bench");
    let mut images = PathBuf::from("docs/images");
    let mut fixtures = PathBuf::from("tmp/bench/fixtures");
    let mut charts_only = false;
    let mut no_charts = false;
    let mut record_ffmpeg = false;
    let mut reference_path = PathBuf::from(reference::PATH);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--runs" => runs = args.next().ok_or("--runs N")?.parse()?,
            "--quick" => {
                quick = true;
                runs = 1;
            }
            "--only" => only.push(args.next().ok_or("--only ID")?),
            "--out" => out = args.next().ok_or("--out DIR")?.into(),
            "--images" => images = args.next().ok_or("--images DIR")?.into(),
            "--fixtures" => fixtures = args.next().ok_or("--fixtures DIR")?.into(),
            "--charts-only" => charts_only = true,
            "--no-charts" => no_charts = true,
            "--record-ffmpeg" => record_ffmpeg = true,
            "--reference" => reference_path = args.next().ok_or("--reference FILE")?.into(),
            _ => return Err(format!("unknown {a}; bench --help gives the usage").into()),
        }
    }
    QUICK.store(quick, Ordering::Relaxed);
    let results_path = out.join(if quick {
        "results-quick.json"
    } else {
        "results.json"
    });
    if charts_only {
        let r: Results = serde_json::from_slice(&fs::read(&results_path)?)?;
        charts(&r.rows, &images, &r.machine, &r.date)?;
        return Ok(());
    }
    let reference = if record_ffmpeg {
        let _ = FFMPEG.set(ffmpeg_on(std::env::var_os("PATH").as_deref())?);
        let mut r = if only.is_empty() {
            Reference::default()
        } else {
            Reference::read(&reference_path).unwrap_or_default()
        };
        let (_, version) = ffmpeg_run(&["-version"], false)?;
        let line = |p: &str| {
            version
                .lines()
                .find(|l| l.starts_with(p))
                .map(str::to_string)
        };
        r.ffmpeg = line("ffmpeg version ").ok_or("ffmpeg -version gave no version line")?;
        r.configuration = line("configuration:").unwrap_or_default();
        r
    } else {
        let r = Reference::read(&reference_path)?;
        r.check_inputs(&out)?;
        r
    };
    let load_start = fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let assets = prepare(&out, &fixtures)?;
    let mut b = Bench {
        runs: runs.max(1),
        only,
        rows: Vec::new(),
        node: render_node(),
        reference,
        missing: Vec::new(),
    };
    b.decode(&assets);
    b.encode(&assets);
    b.transcode(&assets);
    b.audio(&assets);
    b.first_frame(&assets);
    b.filmstrip(&assets);
    b.smart_cut(&assets);
    b.scale(&assets);
    let date = shell("date", &["-u", "+%Y-%m-%d"]);
    let machine = machine(&b.reference.ffmpeg);
    if record_ffmpeg {
        return write_reference(b.reference, machine, date, b.runs, &assets, &reference_path);
    }
    if let Some(line) = missing_line(&b.missing, &reference_path) {
        note!("bench: {line}");
    }
    let results = Results {
        date: date.clone(),
        command: format!(
            "cargo run --release --example bench -- {}",
            std::env::args().skip(1).collect::<Vec<_>>().join(" ")
        )
        .trim()
        .to_string(),
        commit: shell("git", &["rev-parse", "--short", "HEAD"]),
        runs: b.runs,
        input: format!(
            "{CLIP_SECONDS} s of the public-domain NASA 'A Message of Resilience from the International Space Station' clip, 720p60 scaled to 1080p and 1440p and re-encoded with libx264 (CRF 20) and libx265 (CRF 23); audio from the NASA solstice interview"
        ),
        machine,
        load_average_start: load_start.trim().to_string(),
        load_average_end: fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .trim()
            .to_string(),
        rows: b.rows,
    };
    fs::create_dir_all(&out)?;
    let merged = if results.rows.is_empty() || (!b.only.is_empty() && results_path.exists()) {
        merge(&results_path, results)?
    } else {
        results
    };
    fs::write(&results_path, serde_json::to_vec_pretty(&merged)?)?;
    say!("wrote {}", results_path.display())?;
    if !no_charts && !quick && b.only.is_empty() {
        charts(&merged.rows, &images, &merged.machine, &merged.date)?;
        say!("wrote charts to {}", images.display())?;
    }
    Ok(())
}

fn ffmpeg_on(path: Option<&std::ffi::OsStr>) -> Res<PathBuf> {
    Ok(reference::find_on_path(path, "ffmpeg").ok_or("--record-ffmpeg needs ffmpeg on PATH")?)
}

fn missing_line(missing: &[String], path: &Path) -> Option<String> {
    (!missing.is_empty()).then(|| {
        format!(
            "no ffmpeg reference for {} cases in {}, not measured: {}",
            missing.len(),
            path.display(),
            missing.join(", ")
        )
    })
}

fn write_reference(
    mut r: Reference,
    m: Machine,
    date: String,
    runs: usize,
    a: &Assets,
    path: &Path,
) -> Res<()> {
    r.date = date;
    r.runs = runs;
    r.cpu = m.cpu;
    r.threads = m.threads;
    r.gpu = m.gpu;
    r.driver = m.driver;
    r.os = m.os;
    let files = a.video.values().chain(a.raw.values()).chain([&a.pcm]);
    for f in files {
        let name = f.file_name().and_then(|n| n.to_str()).ok_or("input name")?;
        r.inputs.insert(name.to_string(), fs::metadata(f)?.len());
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, r.render())?;
    say!(
        "wrote {} cases to {} with {}",
        r.cases.len(),
        path.display(),
        r.ffmpeg
    )?;
    Ok(())
}

fn merge(path: &Path, mut fresh: Results) -> Res<Results> {
    let Ok(bytes) = fs::read(path) else {
        return Ok(fresh);
    };
    let Ok(old) = serde_json::from_slice::<Results>(&bytes) else {
        return Ok(fresh);
    };
    let mut rows = old.rows;
    for r in std::mem::take(&mut fresh.rows) {
        match rows.iter_mut().find(|o| o.id == r.id) {
            Some(slot) => *slot = r,
            None => rows.push(r),
        }
    }
    fresh.rows = rows;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tmp/bench-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("clips")).unwrap();
        Scratch(dir)
    }

    fn refusal(dir: &Path) -> String {
        format!(
            "{} needs clips/resilience.mp4 and clips/solstice.mp4",
            dir.display()
        )
    }

    #[test]
    fn missing_speech_clip_names_folder_and_both_files() {
        let s = scratch("missing-speech");
        fs::write(s.0.join("clips/resilience.mp4"), b"x").unwrap();
        let err = speech_media(&s.0).unwrap_err().to_string();
        assert_eq!(err, refusal(&s.0));
        assert!(!err.contains('\n'));
    }

    #[test]
    fn missing_resilience_clip_names_folder_and_both_files() {
        let s = scratch("missing-clip");
        fs::write(s.0.join("clips/solstice.mp4"), b"x").unwrap();
        let err = speech_media(&s.0).unwrap_err().to_string();
        assert_eq!(err, refusal(&s.0));
    }

    #[test]
    fn empty_or_absent_fixtures_folder_is_refused_by_name() {
        let s = scratch("empty");
        assert_eq!(speech_media(&s.0).unwrap_err().to_string(), refusal(&s.0));
        let absent = s.0.join("absent");
        assert_eq!(
            speech_media(&absent).unwrap_err().to_string(),
            refusal(&absent)
        );
    }

    #[test]
    fn present_media_gives_both_paths() {
        let s = scratch("present");
        fs::write(s.0.join("clips/resilience.mp4"), b"x").unwrap();
        fs::write(s.0.join("clips/solstice.mp4"), b"x").unwrap();
        let (clip, speech) = speech_media(&s.0).unwrap();
        assert_eq!(clip, s.0.join("clips/resilience.mp4"));
        assert_eq!(speech, s.0.join("clips/solstice.mp4"));
    }

    fn bench_with(reference: Reference) -> Bench {
        Bench {
            runs: 3,
            only: Vec::new(),
            rows: Vec::new(),
            node: String::new(),
            reference,
            missing: Vec::new(),
        }
    }

    fn ours(values: &[f64]) -> Res<Measured> {
        Ok(Measured {
            ours: values.to_vec(),
            route: "cpu".into(),
            ..Measured::default()
        })
    }

    #[test]
    fn every_reference_case_gives_a_comparison_row_without_running_ffmpeg() {
        let r = reference::tests::sample();
        let mut b = bench_with(r.clone());
        b.add(
            "decode.h264.1080.cpu-1t",
            "decode",
            "H.264 1080P DECODE",
            "fps",
            true,
            ours(&[800.0, 825.0, 850.0]),
        );
        b.add(
            "first-frame.h264.1440.cpu",
            "pipeline",
            "FIRST FRAME",
            "ms",
            false,
            ours(&[4.0, 4.26, 5.0]),
        );
        assert!(b.missing.is_empty());
        assert_eq!(b.rows.len(), r.cases.len());
        let decode = &b.rows[0];
        assert_eq!(decode.ffmpeg, Some(412.5));
        assert_eq!(decode.ffmpeg_runs, vec![410.0, 412.5, 415.25]);
        assert_eq!(decode.speedup, Some(2.0));
        assert_eq!(decode.details.get("ffmpeg_psnr_db"), Some(&41.2));
        let first = &b.rows[1];
        assert_eq!(first.ffmpeg, Some(85.2));
        assert!((first.speedup.unwrap() - 20.0).abs() < 1e-9);
        assert!(FFMPEG.get().is_none());
    }

    #[test]
    fn a_case_with_no_reference_is_reported_missing_not_measured() {
        let mut b = bench_with(reference::tests::sample());
        b.add(
            "scale.1440to1080",
            "pipeline",
            "SCALE",
            "fps",
            true,
            ours(&[900.0]),
        );
        b.add(
            "decode.h264.1080.cpu-1t",
            "decode",
            "H.264 1080P DECODE",
            "ms",
            true,
            ours(&[1.0]),
        );
        assert_eq!(b.missing, ["scale.1440to1080", "decode.h264.1080.cpu-1t"]);
        for row in &b.rows {
            assert_eq!(row.ffmpeg, None);
            assert!(row.ffmpeg_runs.is_empty());
            assert_eq!(row.speedup, None);
            assert!(row.ours.is_some());
        }
        let line = missing_line(&b.missing, Path::new("bench/ffmpeg-reference.toml")).unwrap();
        assert_eq!(
            line,
            "no ffmpeg reference for 2 cases in bench/ffmpeg-reference.toml, not measured: scale.1440to1080, decode.h264.1080.cpu-1t"
        );
        assert_eq!(missing_line(&[], Path::new("x")), None);
    }

    #[test]
    fn a_skipped_case_is_neither_compared_nor_missing() {
        let mut b = bench_with(reference::tests::sample());
        b.add(
            "decode.h264.1080.cpu-1t",
            "decode",
            "H.264 1080P DECODE",
            "fps",
            true,
            Err("no VA-API device".into()),
        );
        assert!(b.missing.is_empty());
        assert_eq!(b.rows[0].skipped.as_deref(), Some("no VA-API device"));
        assert_eq!(b.rows[0].ffmpeg, None);
    }

    #[test]
    fn recording_keeps_only_ffmpeg_details() {
        let mut r = Reference::default();
        let row = Row {
            id: "encode.1080.h264.gpu".into(),
            unit: "fps".into(),
            ffmpeg: Some(300.0),
            ffmpeg_runs: vec![290.0, 300.0, 310.0],
            details: BTreeMap::from([
                ("ffmpeg_bytes".into(), 1000.0),
                ("ours_bytes".into(), 900.0),
            ]),
            ..Row::default()
        };
        record(&mut r, &row);
        record(
            &mut r,
            &Row {
                id: "skipped".into(),
                ..Row::default()
            },
        );
        assert_eq!(r.cases.len(), 1);
        let c = &r.cases["encode.1080.h264.gpu"];
        assert_eq!(c.value, 300.0);
        assert_eq!(c.unit, "fps");
        assert_eq!(c.runs, vec![290.0, 300.0, 310.0]);
        assert_eq!(c.details, BTreeMap::from([("ffmpeg_bytes".into(), 1000.0)]));
    }

    #[test]
    fn ffmpeg_refuses_to_run_outside_record_ffmpeg() {
        assert!(FFMPEG.get().is_none());
        let err = ffmpeg_run(&["-version"], false).unwrap_err().to_string();
        assert_eq!(err, "ffmpeg runs only under bench --record-ffmpeg");
        assert!(ffmpeg_time(&["-i", "x"]).is_err());
        assert!(psnr(Path::new("a"), "h264", Path::new("b"), 352, 240).is_none());
    }

    #[test]
    fn only_the_gated_ffmpeg_run_spawns_a_process() {
        let src = include_str!("bench.rs");
        let spawn = concat!("Command::", "new(");
        let targets: Vec<&str> = src
            .match_indices(spawn)
            .filter_map(|(i, _)| src[i + spawn.len()..].split(')').next())
            .collect();
        assert_eq!(targets, ["exe", "cmd", "\"vainfo\""]);
        let gate = &src[src.find("fn ffmpeg_run(").unwrap()..];
        let gate = &gate[..gate.find(&format!("{spawn}exe)")).unwrap()];
        assert!(gate.contains("let exe = FFMPEG\n        .get()"));
        let shells: Vec<&str> = src
            .match_indices("shell(\"")
            .filter_map(|(i, _)| src[i + 7..].split('"').next())
            .collect();
        assert_eq!(shells, ["uname", "date", "git"]);
        let sets = src.matches(concat!("FFMPEG", ".set(")).count();
        assert_eq!(sets, 1);
        let set_at = src.find(concat!("FFMPEG", ".set(")).unwrap();
        let before = &src[..set_at];
        assert!(
            before[before.rfind("let reference = if ").unwrap()..].contains("if record_ffmpeg {")
        );
    }

    #[test]
    fn record_ffmpeg_without_ffmpeg_on_path_fails_in_one_line() {
        let s = scratch("no-ffmpeg");
        let err = ffmpeg_on(Some(s.0.as_os_str())).unwrap_err().to_string();
        assert_eq!(err, "--record-ffmpeg needs ffmpeg on PATH");
        assert_eq!(
            ffmpeg_on(None).unwrap_err().to_string(),
            "--record-ffmpeg needs ffmpeg on PATH"
        );
        fs::write(s.0.join("ffmpeg"), b"").unwrap();
        assert_eq!(
            ffmpeg_on(Some(s.0.as_os_str())).unwrap(),
            s.0.join("ffmpeg")
        );
    }

    #[test]
    fn chart_docs_name_the_reference_version_and_date() {
        let (sub, ff) = docs::ffmpeg_names(&reference::tests::sample());
        assert_eq!(ff, "FFMPEG 9.0.1");
        assert!(sub.ends_with("FFMPEG 9.0.1 RECORDED 2026-10-23"), "{sub}");
    }
}
