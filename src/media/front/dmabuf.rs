use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use super::frames::{Canvas, DmaFrame, FrameSource, export_dmabufs};
use super::still::{Seek, frames_at_seek};
use super::{ExportOptions, Prefs, Progress};
use crate::filter::{ColorSpec, rgba_to_yuv};
use crate::gpu::{Backend, Direction, DmaImage, DmaReady, available};
use crate::media::{
    AudioBuffer, Codec, Error, Frame, PixelFormat, Result, Rgba, Route, TimeBase, VideoFrame,
};

const SIZE: (u32, u32) = (640, 360);
const FRAMES: u64 = 24;

fn picture(i: u64) -> Rgba {
    let mut p = Rgba::new(SIZE.0, SIZE.1);
    for (n, px) in p.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (n % SIZE.0 as usize, n / SIZE.0 as usize);
        let moved = (x + i as usize * 7) % 256;
        let bar = if (x / 64 + y / 45 + i as usize).is_multiple_of(2) {
            200
        } else {
            60
        };
        px.copy_from_slice(&[moved as u8, (y * 255 / 359) as u8, bar, 255]);
    }
    p
}

fn yuv(p: &Rgba) -> Frame {
    let mut f = Frame::new(
        PixelFormat::Yuv420p8,
        p.width,
        p.height,
        0,
        TimeBase::new(1, 90000),
    );
    rgba_to_yuv(p, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut f).unwrap();
    f
}

fn on_the_gpu() -> bool {
    available()
        .iter()
        .any(|&(c, d, b)| c == Codec::H264 && d == Direction::Encode && b == Backend::VaApi)
}

fn out(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/dmabuf");
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

struct Run {
    reads: u64,
    exported: super::Exported,
    path: PathBuf,
}

fn run(
    name: &str,
    opts: ExportOptions,
    shape: impl Fn(&mut DmaImage),
    readable: bool,
) -> Result<Run> {
    let path = out(name);
    let reads = Rc::new(Cell::new(0u64));
    let counted = reads.clone();
    let canvas = Canvas {
        width: SIZE.0,
        height: SIZE.1,
        fps: (30, 1),
        frames: Some(FRAMES),
    };
    let exported = export_dmabufs(
        &path,
        canvas,
        opts,
        |n, _| {
            if n >= FRAMES {
                return Ok(None);
            }
            let p = picture(n);
            let buf = crate::gpu::rgba_dmabuf(&p).map_err(Error::Backend)?;
            let mut image =
                DmaImage::from_export(&buf, DmaReady::Finished).map_err(Error::Backend)?;
            shape(&mut image);
            let counted = counted.clone();
            let readback: Option<super::Readback> = readable.then(|| {
                Box::new(move || {
                    counted.set(counted.get() + 1);
                    Ok(VideoFrame::Cpu(yuv(&p)))
                }) as super::Readback
            });
            Ok(Some(FrameSource::Dma(DmaFrame { image, readback })))
        },
        |_, n| {
            let mut b = AudioBuffer::new(48_000, 2);
            b.samples = vec![0.0; n * 2];
            Ok(b)
        },
        |_: &Progress| true,
    )?;
    Ok(Run {
        reads: reads.get(),
        exported,
        path,
    })
}

fn worst_mean_error(path: &std::path::Path) -> f64 {
    let times: Vec<f64> = [0u64, 5, 11, 17, 23]
        .iter()
        .map(|n| *n as f64 / 30.0)
        .collect();
    let got = frames_at_seek(path, &times, SIZE.1, Seek::Exact, &Prefs::default()).unwrap();
    assert_eq!(got.len(), times.len());
    [0u64, 5, 11, 17, 23]
        .iter()
        .zip(&got)
        .map(|(n, g)| {
            let want = picture(*n);
            let total: u64 = g
                .data
                .chunks_exact(4)
                .zip(want.data.chunks_exact(4))
                .flat_map(|(a, b)| (0..3).map(move |k| u64::from(a[k].abs_diff(b[k]))))
                .sum();
            total as f64 / (SIZE.0 as f64 * SIZE.1 as f64 * 3.0)
        })
        .fold(0.0, f64::max)
}

fn said(r: &Run) -> String {
    r.exported.reasons.join(" | ")
}

#[test]
fn dmabuf_frames_encode_in_place_with_no_readback_and_decode_to_the_source() {
    if !on_the_gpu() {
        return;
    }
    let r = run("direct.mp4", ExportOptions::default(), |_| {}, true).unwrap();
    assert_eq!(r.exported.frames, FRAMES);
    assert_eq!(r.exported.video.route, Route::Gpu);
    assert_eq!(r.reads, 0, "{}", said(&r));
    assert!(!said(&r).contains("read back"), "{}", said(&r));
    let err = worst_mean_error(&r.path);
    assert!(err < 3.0, "mean error per channel {err:.2} of 255");
}

#[test]
fn a_buffer_on_another_gpu_falls_back_to_the_cpu_frames_and_says_why() {
    if !on_the_gpu() {
        return;
    }
    let r = run(
        "elsewhere.mp4",
        ExportOptions::default(),
        |i| i.device = Some(u64::MAX),
        true,
    )
    .unwrap();
    assert_eq!(r.exported.frames, FRAMES);
    assert_eq!(r.reads, FRAMES);
    let why = said(&r);
    assert!(why.contains("another GPU"), "{why}");
    assert!(why.contains("read back to the CPU"), "{why}");
    assert_eq!(r.exported.reasons.len(), 1, "{why}");
    let err = worst_mean_error(&r.path);
    assert!(err < 3.0, "mean error per channel {err:.2} of 255");
}

#[test]
fn a_format_the_encoder_does_not_take_falls_back_naming_it() {
    if !on_the_gpu() {
        return;
    }
    let r = run(
        "format.mp4",
        ExportOptions::default(),
        |i| i.fourcc = u32::from_le_bytes(*b"P010"),
        true,
    )
    .unwrap();
    assert_eq!(r.reads, FRAMES);
    assert!(said(&r).contains("P010"), "{}", said(&r));
}

#[test]
fn a_description_the_encoder_refuses_before_the_driver_falls_back_and_says_why() {
    if !on_the_gpu() {
        return;
    }
    let r = run(
        "pitch.mp4",
        ExportOptions::default(),
        |i| i.planes[0].pitch = 64,
        true,
    )
    .unwrap();
    assert_eq!(r.reads, FRAMES);
    assert!(said(&r).contains("pitch"), "{}", said(&r));
    assert!(said(&r).contains("read back to the CPU"), "{}", said(&r));
    assert!(worst_mean_error(&r.path) < 3.0);
}

#[test]
fn a_refused_dmabuf_with_no_readback_is_an_unsupported_error_not_a_guess() {
    if !on_the_gpu() {
        return;
    }
    let e = run(
        "bare.mp4",
        ExportOptions::default(),
        |i| i.device = Some(u64::MAX),
        false,
    )
    .err()
    .unwrap();
    assert!(matches!(e, Error::Unsupported(_)), "{e}");
    assert!(e.to_string().contains("no readback"), "{e}");
    assert!(e.to_string().contains("another GPU"), "{e}");
}

#[test]
fn a_cpu_encode_reads_dmabufs_back_with_its_reason() {
    if !on_the_gpu() {
        return;
    }
    let cpu = ExportOptions {
        prefs: Prefs {
            encode: super::route::Choice::Cpu,
            ..Prefs::default()
        },
        ..ExportOptions::default()
    };
    let r = run("cpu.mp4", cpu, |_| {}, true).unwrap();
    assert_eq!(r.exported.video.route, Route::Cpu);
    assert_eq!(r.reads, FRAMES);
    assert!(said(&r).contains("encoded on the CPU"), "{}", said(&r));
}

#[test]
fn an_overlay_reads_dmabufs_back_to_draw_on_the_cpu_and_says_so() {
    if !on_the_gpu() {
        return;
    }
    let mut mark = Rgba::new(SIZE.0, SIZE.1);
    for px in mark.data.chunks_exact_mut(4).take(SIZE.0 as usize * 40) {
        px.copy_from_slice(&[255, 255, 255, 255]);
    }
    let mark = Arc::new(mark);
    let layered = ExportOptions {
        overlay: Some(Box::new(move |_, _, _| Some(mark.clone()))),
        ..ExportOptions::default()
    };
    let r = run("overlay.mp4", layered, |_| {}, true).unwrap();
    assert_eq!(r.reads, FRAMES);
    assert!(
        said(&r).contains("an overlay is drawn on the CPU"),
        "{}",
        said(&r)
    );
}

#[test]
fn a_dmabuf_of_the_wrong_size_is_refused_by_frame() {
    if !on_the_gpu() {
        return;
    }
    let e = run(
        "size.mp4",
        ExportOptions::default(),
        |i| {
            i.width = 352;
            i.height = 240;
        },
        true,
    )
    .err()
    .unwrap();
    assert!(e.to_string().contains("frame 0"), "{e}");
}

type Round = (Codec, [Vec<f64>; 3], [Vec<f64>; 3]);

fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|l| l.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let after = stat.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (ticks(11) + ticks(12)) / 100.0
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore]
fn speed() {
    use crate::gpu::{DRM_FORMAT_XBGR8888, EncoderConfig, HwDecoder, HwEncoder, Profile};
    use crate::media::{Bitstream, Rate};
    use std::time::Instant;

    let size = (1080u32, 1920u32);
    let count = 600u64;
    let mut source = Rgba::new(size.0, size.1);
    for (n, px) in source.data.chunks_exact_mut(4).enumerate() {
        let (x, y) = (n % size.0 as usize, n / size.0 as usize);
        px.copy_from_slice(&[
            (x * 255 / 1079) as u8,
            (y * 255 / 1919) as u8,
            ((x / 90 + y / 120) % 2 * 200 + 30) as u8,
            255,
        ]);
    }
    let buf = crate::gpu::rgba_dmabuf(&source).unwrap();
    let mut rgb = DmaImage::from_export(&buf, DmaReady::Finished).unwrap();
    rgb.fourcc = DRM_FORMAT_XBGR8888;
    let config = EncoderConfig {
        width: size.0,
        height: size.1,
        fps: (60, 1),
        profile: Profile::High,
        rate: Rate::Crf(18),
        gop: 120,
        b_frames: 0,
        bitstream: Bitstream::LengthPrefixed,
        ..Default::default()
    };
    let mut enc = HwEncoder::new(Codec::H264, &config).unwrap();
    let packets = enc
        .encode_dmabuf(&rgb, 0, TimeBase::new(1, 60))
        .unwrap()
        .into_iter()
        .chain(enc.flush().unwrap())
        .collect::<Vec<_>>();
    let mut dec = HwDecoder::new(Codec::H264, enc.extradata().unwrap()).unwrap();
    let mut gpu = Vec::new();
    for p in &packets {
        gpu.extend(dec.decode_gpu(p).unwrap());
    }
    gpu.extend(dec.flush_gpu().unwrap());
    let nv12 = gpu.pop().unwrap();
    assert!(nv12.on_gpu());
    let nv12_buf = nv12.dmabuf(false).unwrap();
    let mut rounds: Vec<Round> = Vec::new();
    for codec in [Codec::H264, Codec::Hevc] {
        let ok = available()
            .iter()
            .any(|&(c, d, _)| c == codec && d == Direction::Encode);
        if !ok {
            eprintln!("{codec:?}: no GPU encoder here");
            continue;
        }
        let mut times: [Vec<f64>; 3] = Default::default();
        let mut cpus: [Vec<f64>; 3] = Default::default();
        for round in 0..5 {
            for (k, name) in ["readback", "nv12 dmabuf", "rgb dmabuf"].iter().enumerate() {
                let path = out(&format!("speed-{k}.mp4"));
                let canvas = Canvas {
                    width: size.0,
                    height: size.1,
                    fps: (60, 1),
                    frames: Some(count),
                };
                let opts = ExportOptions {
                    video: codec,
                    ..ExportOptions::default()
                };
                let audio = |_: u64, n: usize| {
                    let mut b = AudioBuffer::new(48_000, 2);
                    b.samples = vec![0.0; n * 2];
                    Ok(b)
                };
                let start = Instant::now();
                let cpu = cpu_seconds();
                let done = if k == 0 {
                    super::export_frames(
                        &path,
                        canvas,
                        opts,
                        |n, _| Ok((n < count).then(|| VideoFrame::Cpu(nv12.download().unwrap()))),
                        audio,
                        |_: &Progress| true,
                    )
                } else {
                    let nv12_buf = &nv12_buf;
                    let rgb_buf = &buf;
                    export_dmabufs(
                        &path,
                        canvas,
                        opts,
                        |n, _| {
                            if n >= count {
                                return Ok(None);
                            }
                            let image = if k == 1 {
                                DmaImage::from_export(nv12_buf, DmaReady::Finished)
                            } else {
                                DmaImage::from_export(rgb_buf, DmaReady::Finished).map(|mut i| {
                                    i.fourcc = DRM_FORMAT_XBGR8888;
                                    i
                                })
                            }
                            .map_err(Error::Backend)?;
                            Ok(Some(FrameSource::Dma(DmaFrame {
                                image,
                                readback: None,
                            })))
                        },
                        audio,
                        |_: &Progress| true,
                    )
                }
                .unwrap();
                let secs = start.elapsed().as_secs_f64();
                assert_eq!(done.frames, count);
                assert!(
                    done.reasons.iter().all(|r| !r.contains("read back")),
                    "{:?}",
                    done.reasons
                );
                let busy = cpu_seconds() - cpu;
                eprintln!(
                    "{codec:?} round {round} {name}: {secs:.3} s wall, {busy:.2} s cpu, load {}",
                    load()
                );
                times[k].push(secs);
                cpus[k].push(busy);
            }
        }
        rounds.push((codec, times, cpus));
    }
    for (codec, mut t, mut c) in rounds {
        let m: Vec<f64> = t.iter_mut().map(|v| median(v)).collect();
        let u: Vec<f64> = c.iter_mut().map(|v| median(v)).collect();
        eprintln!(
            "{codec:?} {count} frames {}x{} 60 fps, medians of 5, wall / cpu seconds: readback {:.3} / {:.2}, nv12 dmabuf {:.3} / {:.2}, rgb dmabuf {:.3} / {:.2}; load {}",
            size.0,
            size.1,
            m[0],
            u[0],
            m[1],
            u[1],
            m[2],
            u[2],
            load()
        );
    }
}
