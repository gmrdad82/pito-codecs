use super::*;
use crate::media::{AudioBuffer, Frame, PixelFormat, Rgba, TimeBase};
use crate::testref::Refs;
use std::time::Instant;

#[allow(dead_code)]
#[path = "refs.rs"]
mod refs;

fn frames_of(what: &str, bytes: &[u8], format: PixelFormat, w: u32, h: u32) -> Vec<Frame> {
    let one = Frame::new(format, w, h, 0, TimeBase::new(1, 30));
    let size: usize = one.planes.iter().map(|p| p.data.len()).sum();
    assert_eq!(
        bytes.len(),
        size * refs::FRAMES as usize,
        "{what}: {} bytes, not {} frames of {w}x{h} {format:?}",
        bytes.len(),
        refs::FRAMES
    );
    bytes
        .chunks_exact(size)
        .map(|chunk| {
            let mut f = one.clone();
            let mut at = 0;
            for p in f.planes.iter_mut() {
                let n = p.data.len();
                p.data.copy_from_slice(&chunk[at..at + n]);
                at += n;
            }
            f
        })
        .collect()
}

fn yuv_ref(r: &Refs, name: &str, format: PixelFormat, w: u32, h: u32) -> Vec<Frame> {
    frames_of(name, &r.bytes(name), format, w, h)
}

fn packed_ref(r: &Refs, name: &str, (w, h): (u32, u32)) -> Vec<Vec<u8>> {
    let bytes = r.bytes(name);
    let size = (w * h * 4) as usize;
    assert_eq!(
        bytes.len(),
        size * refs::FRAMES as usize,
        "{name}: {} bytes, not {} frames of {w}x{h} packed",
        bytes.len(),
        refs::FRAMES
    );
    bytes.chunks_exact(size).map(<[u8]>::to_vec).collect()
}

fn psnr(a: &[u8], b: &[u8], wide: bool) -> f64 {
    let (mse, peak) = if wide {
        let v = |d: &[u8]| -> Vec<f64> {
            d.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]) as f64)
                .collect()
        };
        let (x, y) = (v(a), v(b));
        let m = x.iter().zip(&y).map(|(p, q)| (p - q).powi(2)).sum::<f64>() / x.len() as f64;
        (m, 1023.0)
    } else {
        let m = a
            .iter()
            .zip(b)
            .map(|(&p, &q)| (p as f64 - q as f64).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        (m, 255.0)
    };
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (peak * peak / mse).log10()
    }
}

fn frame_psnr(a: &Frame, b: &Frame) -> [f64; 3] {
    let wide = a.format == PixelFormat::Yuv420p10;
    [0, 1, 2].map(|i| psnr(&a.planes[i].data, &b.planes[i].data, wide))
}

fn summary_of(samples: &[f32], rate: u32) -> Loudness {
    let channels = refs::CHANNELS;
    let mut meter = LoudnessMeter::new(rate, channels).unwrap();
    for chunk in samples.chunks(usize::from(channels) * 1999) {
        let part = AudioBuffer {
            samples: chunk.to_vec(),
            rate,
            channels,
        };
        meter.process(&part).unwrap();
    }
    meter.summary()
}

#[test]
fn r128_matches_ffmpeg_ebur128() {
    let r = Refs::open(refs::AREA);
    for (label, rate, seconds, sound) in refs::loudness() {
        let name = refs::loudness_ref(label);
        let log = String::from_utf8(r.bytes(&name)).unwrap();
        let field = |key: &str| {
            refs::meter_field(&log, key)
                .unwrap_or_else(|| panic!("{name}: no lavfi.r128.{key} in {log}"))
        };
        let (i, lra) = (field("I"), field("LRA"));
        let tp = 20.0 * field("true_peak").max(1e-12).log10();
        let ours = summary_of(&refs::loudness_pcm(sound, rate, seconds), rate);
        println!(
            "{label}: I {:.2}/{i} LRA {:.2}/{lra} TP {:.2}/{tp:.2}",
            ours.integrated, ours.range, ours.true_peak
        );
        assert!(
            (ours.integrated - i).abs() <= 0.1,
            "{label}: I {} vs {i}",
            ours.integrated
        );
        assert!(
            (ours.true_peak - tp).abs() <= 0.2,
            "{label}: TP {} vs {tp}",
            ours.true_peak
        );
        assert!(
            (ours.range - lra).abs() <= 0.5,
            "{label}: LRA {} vs {lra}",
            ours.range
        );
    }
}

#[test]
fn the_loudness_inputs_are_the_ones_recorded() {
    for (label, rate, seconds, sound) in refs::loudness() {
        let a = refs::loudness_pcm(sound, rate, seconds);
        let b = refs::loudness_pcm(sound, rate, seconds);
        assert_eq!(a, b, "{label} is not deterministic");
        assert_eq!(
            a.len(),
            refs::loudness_frames(rate, seconds) * usize::from(refs::CHANNELS),
            "{label}"
        );
        let peak = a.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.01 && peak <= 1.0, "{label}: peak {peak}");
        assert_eq!(
            refs::loudness_bytes(sound, rate, seconds).len(),
            a.len() * 4,
            "{label}"
        );
    }
}

#[test]
fn a_meter_log_gives_the_last_value_of_each_field() {
    let log = "frame:0    pts:0       pts_time:0\nlavfi.r128.I=-70.000\nlavfi.r128.LRA=0.000\n\
               frame:1    pts:4800    pts_time:0.1\nlavfi.r128.I=-23.015\nlavfi.r128.LRA=4.250\n\
               lavfi.r128.true_peaks_ch0=0.251\nlavfi.r128.true_peak=0.251\n";
    assert_eq!(refs::meter_field(log, "I"), Some(-23.015));
    assert_eq!(refs::meter_field(log, "LRA"), Some(4.25));
    assert_eq!(refs::meter_field(log, "true_peak"), Some(0.251));
    assert_eq!(refs::meter_field(log, "M"), None);
    assert_eq!(refs::meter_field("lavfi.r128.I=loud", "I"), None);
}

#[test]
fn a_damaged_filter_reference_is_refused_before_use() {
    let r = Refs::open(refs::AREA);
    let name = refs::loudness_ref("tone");
    let dir = crate::testref::scratch("filter-damaged");
    let file = &r.entry(&name).file;
    let mut bytes = r.bytes(&name);
    bytes.push(b'\n');
    std::fs::write(dir.join(file), &bytes).unwrap();
    std::fs::copy(
        r.dir.join(crate::testref::MANIFEST),
        dir.join(crate::testref::MANIFEST),
    )
    .unwrap();
    let damaged = Refs::at(dir, refs::AREA);
    let caught = std::panic::catch_unwind(|| damaged.bytes(&name));
    assert!(caught.is_err(), "a changed {name} was read");
}

fn kernel_of(name: &str) -> Kernel {
    match name {
        "bilinear" => Kernel::Bilinear,
        "bicubic" => Kernel::Bicubic,
        _ => Kernel::Lanczos,
    }
}

fn spec_of(matrix: &str, range: &str) -> ColorSpec {
    let m = match matrix {
        "bt601" => Matrix::Bt601,
        "bt2020" => Matrix::Bt2020,
        _ => Matrix::Bt709,
    };
    let r = if range == "pc" {
        Range::Full
    } else {
        Range::Limited
    };
    ColorSpec::new(m, r)
}

fn depth(deep: bool) -> PixelFormat {
    if deep {
        PixelFormat::Yuv420p10
    } else {
        PixelFormat::Yuv420p8
    }
}

#[test]
fn the_synthetic_sources_are_what_the_recorder_feeds() {
    for deep in [false, true] {
        for size in [refs::SCALE_SIZE, refs::COLOUR_SIZE] {
            let bytes = refs::yuv_bytes(size, deep);
            let frames = frames_of("source", &bytes, depth(deep), size.0, size.1);
            for (n, f) in frames.iter().enumerate() {
                assert_eq!(*f, refs::yuv(size, n as u64, deep), "{size:?} {deep}");
            }
            assert_ne!(frames[0], frames[1], "{size:?} {deep}: the frames match");
        }
    }
    let ten = refs::yuv10(refs::SCALE_SIZE, 1);
    let eight = refs::yuv8(refs::SCALE_SIZE, 1);
    for (p, q) in ten.planes.iter().zip(&eight.planes) {
        for (c, v) in p.data.chunks_exact(2).zip(&q.data) {
            let s = u16::from_le_bytes([c[0], c[1]]);
            assert!(s < 1024 && s >> 2 == u16::from(*v), "{s} from {v}");
        }
    }
    let (w, h) = refs::COLOUR_SIZE;
    let rgba = refs::rgba_bytes(refs::COLOUR_SIZE);
    let bgra = refs::bgra_bytes(refs::COLOUR_SIZE);
    assert_eq!(rgba.len(), (w * h * 4) as usize * refs::FRAMES as usize);
    for (a, b) in rgba.chunks_exact(4).zip(bgra.chunks_exact(4)) {
        assert_eq!([a[2], a[1], a[0], a[3]], *b);
    }
}

#[test]
fn a_short_reference_is_refused_by_name() {
    let caught = std::panic::catch_unwind(|| {
        frames_of("short.yuv", &[0; 100], PixelFormat::Yuv420p8, 48, 36)
    });
    assert!(caught.is_err());
    let one = frames_of(
        "two.yuv",
        &refs::yuv_bytes(refs::SCALE_SIZE, false),
        PixelFormat::Yuv420p8,
        refs::SCALE_SIZE.0,
        refs::SCALE_SIZE.1,
    );
    assert_eq!(one.len() as u64, refs::FRAMES);
}

#[test]
fn scale_matches_zscale() {
    let r = Refs::open(refs::AREA);
    for (name, w, h, deep) in refs::SCALES {
        let pix = refs::pix(deep);
        let theirs = yuv_ref(&r, &refs::scale_ref(name, w, h, deep), depth(deep), w, h);
        for (n, t) in theirs.iter().enumerate() {
            let input = refs::yuv(refs::SCALE_SIZE, n as u64, deep);
            let ours = scale(&input, w, h, kernel_of(name)).unwrap();
            let p = frame_psnr(&ours, t);
            println!(
                "{name} {w}x{h} {pix} frame {n}: PSNR {:.1} {:.1} {:.1}",
                p[0], p[1], p[2]
            );
            assert!(
                p.iter().all(|&v| v > 60.0),
                "{name} {w}x{h} {pix} frame {n}: {p:?}"
            );
        }
    }
}

#[test]
fn colour_matches_ffmpeg() {
    let r = Refs::open(refs::AREA);
    let size = refs::COLOUR_SIZE;
    let (w, h) = size;
    for (matrix, range) in refs::COLOURS {
        let spec = spec_of(matrix, range);
        let theirs = packed_ref(&r, &refs::rgba_ref(matrix, range), size);
        let back = yuv_ref(
            &r,
            &refs::yuv_ref(matrix, range),
            PixelFormat::Yuv420p8,
            w,
            h,
        );
        for n in 0..refs::FRAMES {
            let frame = refs::yuv8(size, n);
            let mut rgba = Rgba::new(0, 0);
            yuv_to_rgba(&frame, spec, &mut rgba).unwrap();
            let p = psnr(&rgba.data, &theirs[n as usize], false);
            println!("yuv to rgba {matrix} {range} frame {n}: PSNR {p:.1}");
            assert!(p > 70.0, "yuv to rgba {matrix} {range} frame {n}: {p}");

            let image = Rgba {
                width: w,
                height: h,
                data: refs::rgba(size, n),
            };
            let mut ours = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
            rgba_to_yuv(
                &image,
                spec.with_siting(Siting::Centre),
                PixelFormat::Yuv420p8,
                &mut ours,
            )
            .unwrap();
            let p = frame_psnr(&ours, &back[n as usize]);
            println!(
                "rgba to yuv {matrix} {range} frame {n}: PSNR {:.1} {:.1} {:.1}",
                p[0], p[1], p[2]
            );
            assert!(
                p.iter().all(|&v| v > 48.0),
                "rgba to yuv {matrix} {range} frame {n}: {p:?}"
            );
        }
    }
    let theirs = yuv_ref(&r, refs::CONVERT_10BIT, PixelFormat::Yuv420p10, w, h);
    for (n, t) in theirs.iter().enumerate() {
        let frame = refs::yuv8(size, n as u64);
        let mut ours = Frame::new(PixelFormat::Yuv420p10, 0, 0, 0, frame.time_base);
        convert_color(
            &frame,
            ColorSpec::BT601,
            ColorSpec::new(Matrix::Bt709, Range::Full),
            PixelFormat::Yuv420p10,
            &mut ours,
        )
        .unwrap();
        let p = frame_psnr(&ours, t);
        println!(
            "bt601 limited 8-bit to bt709 full 10-bit frame {n}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(p.iter().all(|&v| v > 75.0), "convert frame {n}: {p:?}");
    }
}

fn pattern(format: PixelFormat, width: u32, height: u32, seed: u32) -> Frame {
    let mut f = Frame::new(format, width, height, 0, TimeBase::new(1, 30));
    let mut s = seed | 1;
    let top = (1u32 << format.bit_depth()) - 1;
    for p in f.planes.iter_mut() {
        if format == PixelFormat::Yuv420p8 {
            for v in p.data.iter_mut() {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                *v = s as u8;
            }
        } else {
            for c in p.data.chunks_exact_mut(2) {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                c.copy_from_slice(&((s % (top + 1)) as u16).to_le_bytes());
            }
        }
    }
    f
}

#[test]
fn simd_kernels_match_plain() {
    use super::packed::{from_packed, to_packed};
    use super::plane::{Simd, simd};
    let fast = simd();
    let (w, h) = (77u32, 13u32);
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        let frame = pattern(format, w, h, 7);
        for siting in [Siting::Left, Siting::Centre] {
            let spec = ColorSpec::BT709.with_siting(siting);
            for order in [Order::Rgba, Order::Bgra, Order::Rgbx, Order::Bgrx] {
                for chroma in [Chroma::Nearest, Chroma::Bilinear] {
                    let mut a = vec![0u8; (w * h * 4) as usize];
                    let mut b = a.clone();
                    to_packed(
                        &frame,
                        spec,
                        chroma,
                        PackedMut::new(w, h, order, &mut a),
                        Simd::Plain,
                    )
                    .unwrap();
                    to_packed(
                        &frame,
                        spec,
                        chroma,
                        PackedMut::new(w, h, order, &mut b),
                        fast,
                    )
                    .unwrap();
                    assert_eq!(a, b, "{format:?} {siting:?} {order:?} {chroma:?} to packed");
                }
                let [luma, _, _] = pattern(PixelFormat::Yuv420p8, w * 2, h * 2, 9).planes;
                let bytes = luma.data;
                let image = Packed::new(w, h, order, &bytes[..(w * h * 4) as usize]);
                let mut a = Frame::new(format, 0, 0, 0, frame.time_base);
                let mut b = a.clone();
                from_packed(image, spec, format, &mut a, Simd::Plain).unwrap();
                from_packed(image, spec, format, &mut b, fast).unwrap();
                assert_eq!(a, b, "{format:?} {siting:?} {order:?} from packed");
            }
        }
        for (dw, dh) in [(40, 29), (160, 50)] {
            for kernel in [Kernel::Bilinear, Kernel::Lanczos] {
                let s = Scaler::new(w, h, dw, dh, format, kernel).unwrap();
                let mut a = Frame::new(format, 0, 0, 0, frame.time_base);
                let mut b = a.clone();
                s.clone()
                    .with_simd(Simd::Plain)
                    .run(&frame, &mut a)
                    .unwrap();
                s.run(&frame, &mut b).unwrap();
                assert_eq!(a, b, "{format:?} scale {kernel:?} to {dw}x{dh}");
            }
        }
    }
}

#[test]
fn orders_and_siting_match_ffmpeg() {
    let r = Refs::open(refs::AREA);
    let size = refs::COLOUR_SIZE;
    let (w, h) = size;
    let mut ours = vec![0u8; (w * h * 4) as usize];
    let bgra = packed_ref(&r, refs::BGRA, size);
    let back = yuv_ref(&r, refs::BGR0_TO_YUV, PixelFormat::Yuv420p8, w, h);
    let zscaled: Vec<Vec<Vec<u8>>> = refs::SITINGS
        .iter()
        .map(|s| packed_ref(&r, &refs::zscale_rgba_ref(s), size))
        .collect();
    let left = yuv_ref(&r, refs::RGBA_TO_YUV_LEFT, PixelFormat::Yuv420p8, w, h);
    for n in 0..refs::FRAMES {
        let at = n as usize;
        let frame = refs::yuv8(size, n);
        yuv_to_packed(
            &frame,
            ColorSpec::BT709,
            Chroma::Nearest,
            PackedMut::new(w, h, Order::Bgra, &mut ours),
        )
        .unwrap();
        let p = psnr(&ours, &bgra[at], false);
        println!("yuv to bgra frame {n}: PSNR {p:.1}");
        assert!(p > 70.0, "yuv to bgra frame {n}: {p}");

        let image = refs::bgra(size, n);
        let mut got = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
        packed_to_yuv(
            Packed::new(w, h, Order::Bgrx, &image),
            ColorSpec::BT709.with_siting(Siting::Centre),
            PixelFormat::Yuv420p8,
            &mut got,
        )
        .unwrap();
        let p = frame_psnr(&got, &back[at]);
        println!(
            "bgrx to yuv, centre, frame {n}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(p.iter().all(|&v| v > 75.0), "bgrx to yuv frame {n}: {p:?}");

        for (siting, (name, theirs)) in [Siting::Left, Siting::Centre]
            .into_iter()
            .zip(refs::SITINGS.iter().zip(&zscaled))
        {
            yuv_to_packed(
                &frame,
                ColorSpec::BT709.with_siting(siting),
                Chroma::Bilinear,
                PackedMut::new(w, h, Order::Rgba, &mut ours),
            )
            .unwrap();
            let p = psnr(&ours, &theirs[at], false);
            println!("yuv to rgba, bilinear chroma, {name}, frame {n}: PSNR {p:.1}");
            assert!(p > 80.0, "bilinear chroma {name} frame {n}: {p}");
        }

        let image = refs::rgba(size, n);
        packed_to_yuv(
            Packed::new(w, h, Order::Rgba, &image),
            ColorSpec::BT709,
            PixelFormat::Yuv420p8,
            &mut got,
        )
        .unwrap();
        let p = frame_psnr(&got, &left[at]);
        println!(
            "rgba to yuv, left, frame {n}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(p.iter().all(|&v| v > 53.0), "rgba to yuv left frame {n}: {p:?}");
    }

    let (sw, sh) = refs::SCALE_SIZE;
    let expect = yuv_ref(&r, refs::SCALE_CENTRE, PixelFormat::Yuv420p8, 48, 36);
    let scaler = Scaler::new(sw, sh, 48, 36, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .siting(Siting::Centre);
    for (n, t) in expect.iter().enumerate() {
        let frame = refs::yuv8(refs::SCALE_SIZE, n as u64);
        let mut scaled = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, frame.time_base);
        scaler.run(&frame, &mut scaled).unwrap();
        let p = frame_psnr(&scaled, t);
        println!(
            "scale lanczos, centre siting, frame {n}: PSNR {:.1} {:.1} {:.1}",
            p[0], p[1], p[2]
        );
        assert!(p.iter().all(|&v| v > 60.0), "scale centre frame {n}: {p:?}");
    }
}

fn tone(freq: f64, rate: u32, seconds: f64, amp: f64) -> Vec<f32> {
    (0..(seconds * rate as f64) as usize)
        .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32)
        .collect()
}

fn mono(samples: Vec<f32>, rate: u32) -> AudioBuffer {
    AudioBuffer {
        samples,
        rate,
        channels: 1,
    }
}

fn rms_db(x: &[f32]) -> f64 {
    let e = x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64;
    10.0 * e.log10()
}

#[test]
fn resample_round_trip_keeps_the_band_and_drops_aliases() {
    let rate = 48000;
    let mut x = vec![0.0f32; rate as usize];
    for f in [100.0, 1000.0, 5000.0, 12000.0, 16000.0] {
        for (o, v) in x.iter_mut().zip(tone(f, rate, 1.0, 0.15)) {
            *o += v;
        }
    }
    let input = mono(x.clone(), rate);
    let down = resample(&input, 44100).unwrap();
    assert_eq!(down.samples.len(), 44100);
    let back = resample(&down, rate).unwrap();
    assert_eq!(back.samples.len(), x.len());
    let edge = 2000;
    let err: Vec<f32> = x[edge..x.len() - edge]
        .iter()
        .zip(&back.samples[edge..x.len() - edge])
        .map(|(a, b)| a - b)
        .collect();
    let snr = rms_db(&x[edge..x.len() - edge]) - rms_db(&err);
    assert!(snr > 60.0, "round trip SNR {snr:.1} dB");

    let alias = resample(&mono(tone(23000.0, rate, 1.0, 0.5), rate), 44100).unwrap();
    let level = rms_db(&alias.samples[edge..alias.samples.len() - edge])
        - rms_db(&tone(23000.0, rate, 1.0, 0.5));
    assert!(level < -70.0, "23 kHz into 44.1 kHz leaks {level:.1} dB");

    let speech = resample(&mono(tone(10000.0, rate, 1.0, 0.5), rate), 16000).unwrap();
    assert_eq!(speech.samples.len(), 16000);
    let level = rms_db(&speech.samples[500..15500]) - rms_db(&tone(10000.0, rate, 1.0, 0.5));
    assert!(level < -70.0, "10 kHz into 16 kHz leaks {level:.1} dB");

    let odd = resample(&mono(tone(1000.0, rate, 1.0, 0.5), rate), 47999).unwrap();
    let want = tone(1000.0, 47999, 1.0, 0.5);
    let err: Vec<f32> = odd.samples[edge..40000]
        .iter()
        .zip(&want[edge..40000])
        .map(|(a, b)| a - b)
        .collect();
    let snr = rms_db(&want[edge..40000]) - rms_db(&err);
    assert!(snr > 60.0, "48000 to 47999 SNR {snr:.1} dB");

    for (from, to, ch) in [(44100, 48000, 2u16), (48000, 16000, 1), (48000, 47999, 2)] {
        let input = AudioBuffer {
            samples: (0..from as usize * ch as usize / 3)
                .map(|i| ((i * 7919) % 2003) as f32 / 1001.0 - 1.0)
                .collect(),
            rate: from,
            channels: ch,
        };
        let mut rs = Resampler::new(from, to, ch).unwrap();
        let mut streamed = AudioBuffer::new(to, ch);
        let mut part = AudioBuffer::new(to, ch);
        for c in input.samples.chunks(999 * ch as usize) {
            let piece = AudioBuffer {
                samples: c.to_vec(),
                rate: from,
                channels: ch,
            };
            rs.process(&piece, &mut part).unwrap();
            streamed.samples.extend_from_slice(&part.samples);
        }
        rs.flush(&mut part);
        streamed.samples.extend_from_slice(&part.samples);
        assert_eq!(
            resample(&input, to).unwrap().samples,
            streamed.samples,
            "{from} to {to}"
        );
    }
}

#[test]
fn normalise_hits_target_under_the_ceiling() {
    let rate = 48000;
    let mut x = tone(220.0, rate, 20.0, 0.1);
    let burst = tone(1000.0, rate, 0.01, 0.8);
    for at in (0..x.len() - burst.len()).step_by(2 * rate as usize) {
        for (o, b) in x[at..].iter_mut().zip(&burst) {
            *o += b;
        }
    }
    let stereo: Vec<f32> = x.iter().flat_map(|&v| [v, v]).collect();
    let mut buf = AudioBuffer {
        samples: stereo,
        rate,
        channels: 2,
    };
    let frames = buf.frames();
    let before = normalize(&mut buf, -14.0, -1.0).unwrap();
    assert!(before.true_peak - before.integrated + -14.0 > 0.0);
    assert_eq!(buf.frames(), frames);
    let mut meter = LoudnessMeter::new(rate, 2).unwrap();
    meter.process(&buf).unwrap();
    let after = meter.summary();
    assert!(after.true_peak <= -0.95, "true peak {}", after.true_peak);
    assert!(
        (after.integrated + 14.0).abs() < 0.2,
        "integrated {}",
        after.integrated
    );
}

#[test]
fn fps_change_drops_and_repeats_by_pts() {
    let tiny = |pts: i64, tb: TimeBase| Frame::new(PixelFormat::Yuv420p8, 2, 2, pts, tb);
    for (from, to, count, want) in [
        ((30, 1), (24, 1), 90, 72),
        ((24, 1), (60, 1), 48, 120),
        ((30000, 1001), (30, 1), 300, 300),
    ] {
        let mut conv = FpsConverter::new(to.0, to.1).unwrap();
        let tb = TimeBase::new(from.1 * 1000, from.0 * 1000);
        let mut out = Vec::new();
        for i in 0..count {
            out.extend(conv.push(tiny(i, tb)));
        }
        out.extend(conv.flush());
        let pts: Vec<i64> = out.iter().map(|f| f.pts).collect();
        assert!(
            (pts.len() as i64 - want).abs() <= 1,
            "{from:?} to {to:?}: {} frames",
            pts.len()
        );
        assert!(
            pts.windows(2).all(|w| w[1] == w[0] + 1),
            "{from:?} to {to:?}: gaps"
        );
        assert_eq!(out[0].time_base, conv.time_base());
    }
}

#[test]
fn rotation_matches_a_plain_transpose_at_odd_sizes() {
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        let (w, h) = (150usize, 86usize);
        let mut f = Frame::new(format, w as u32, h as u32, 0, TimeBase::new(1, 30));
        for p in f.planes.iter_mut() {
            for (i, v) in p.data.iter_mut().enumerate() {
                *v = (i * 31 + i / 7) as u8;
            }
        }
        let b = format.bytes_per_sample();
        for rotation in [Rotation::R90, Rotation::R270] {
            let mut r = Frame::new(format, 0, 0, 0, f.time_base);
            rotate(&f, rotation, &mut r).unwrap();
            for i in 0..3 {
                let (pw, ph) = if i == 0 { (w, h) } else { (w / 2, h / 2) };
                let (s, d) = (&f.planes[i], &r.planes[i]);
                for y in 0..pw {
                    for x in 0..ph {
                        let (sy, sx) = match rotation {
                            Rotation::R90 => (ph - 1 - x, y),
                            _ => (x, pw - 1 - y),
                        };
                        let at = |p: &crate::media::Plane, row: usize, col: usize| {
                            p.data[row * p.stride + col * b..row * p.stride + col * b + b].to_vec()
                        };
                        assert_eq!(
                            at(d, y, x),
                            at(s, sy, sx),
                            "{format:?} {rotation:?} plane {i} at {x},{y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn geometry_round_trips() {
    let mut f = Frame::new(PixelFormat::Yuv420p8, 6, 4, 7, TimeBase::new(1, 30));
    for p in f.planes.iter_mut() {
        for (i, v) in p.data.iter_mut().enumerate() {
            *v = (i * 7 + 3) as u8;
        }
    }
    let mut r = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, f.time_base);
    let mut back = r.clone();
    rotate(&f, Rotation::R90, &mut r).unwrap();
    assert_eq!((r.width, r.height), (4, 6));
    assert_eq!(r.planes[0].data[3], f.planes[0].data[0]);
    assert_eq!(r.planes[0].data[0], f.planes[0].data[18]);
    rotate(&r, Rotation::R270, &mut back).unwrap();
    assert_eq!(back, f);
    rotate(&f, Rotation::R180, &mut r).unwrap();
    rotate(&r, Rotation::R180, &mut back).unwrap();
    assert_eq!(back, f);
    let mut flipped = f.clone();
    flip_horizontal(&mut flipped).unwrap();
    flip_vertical(&mut flipped).unwrap();
    assert_eq!(flipped, r);

    let black = Fill::black(Range::Limited, PixelFormat::Yuv420p8);
    pad(&f, 10, 8, 2, 2, black, &mut r).unwrap();
    assert_eq!(r.planes[0].data[0], 16);
    assert_eq!(r.planes[1].data[0], 128);
    crop(&r, 2, 2, 6, 4, &mut back).unwrap();
    assert_eq!(back, f);

    let mut canvas = Frame::new(PixelFormat::Yuv420p8, 8, 8, 0, f.time_base);
    pad(&f, 8, 8, 0, 0, black, &mut canvas).unwrap();
    let mut mark = Rgba::new(4, 4);
    for p in mark.data.chunks_exact_mut(4) {
        p.copy_from_slice(&[255, 255, 255, 255]);
    }
    mark.data[3] = 0;
    let overlay = Overlay::new(&mark, ColorSpec::BT709, PixelFormat::Yuv420p8).unwrap();
    let before = canvas.clone();
    overlay.draw(&mut canvas, 4, 4).unwrap();
    let y = &canvas.planes[0].data;
    assert_eq!(y[4 * 8 + 4], before.planes[0].data[4 * 8 + 4]);
    assert_eq!(y[4 * 8 + 5], 235);
    assert_eq!(y[7 * 8 + 7], 235);
    assert_eq!(&y[..32], &before.planes[0].data[..32]);
    overlay.draw(&mut canvas, -10, -10).unwrap();
    overlay.draw(&mut canvas, 7, 7).unwrap();
}

fn rate(label: &str, n: usize, f: impl FnMut()) -> f64 {
    let mut f = f;
    f();
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    let per = t.elapsed().as_secs_f64() / n as f64;
    println!("{label}: {:.1} fps ({:.2} ms)", 1.0 / per, per * 1000.0);
    1.0 / per
}

#[test]
#[ignore]
fn bench() {
    let src = crate::testref::yuv_frame(2560, 1440, 0, 0);
    let threads = rayon::current_num_threads();
    for kernel in [Kernel::Bilinear, Kernel::Bicubic, Kernel::Lanczos] {
        let s = Scaler::new(2560, 1440, 1920, 1080, PixelFormat::Yuv420p8, kernel).unwrap();
        let mut out = Frame::new(PixelFormat::Yuv420p8, 1920, 1080, 0, src.time_base);
        rate(
            &format!("scale 1440p to 1080p 8-bit {kernel:?}, {threads} threads"),
            200,
            || s.run(&src, &mut out).unwrap(),
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        pool.install(|| {
            rate(
                &format!("scale 1440p to 1080p 8-bit {kernel:?}, 1 thread"),
                30,
                || s.run(&src, &mut out).unwrap(),
            );
        });
    }
    let mut deep = Frame::new(PixelFormat::Yuv420p10, 0, 0, 0, src.time_base);
    convert_color(
        &src,
        ColorSpec::BT709,
        ColorSpec::BT709,
        PixelFormat::Yuv420p10,
        &mut deep,
    )
    .unwrap();
    let s = Scaler::new(
        2560,
        1440,
        1920,
        1080,
        PixelFormat::Yuv420p10,
        Kernel::Lanczos,
    )
    .unwrap();
    let mut out = Frame::new(PixelFormat::Yuv420p10, 1920, 1080, 0, src.time_base);
    rate("scale 1440p to 1080p 10-bit Lanczos", 200, || {
        s.run(&deep, &mut out).unwrap()
    });
    let s = Scaler::new(
        1920,
        1080,
        3840,
        2160,
        PixelFormat::Yuv420p8,
        Kernel::Bicubic,
    )
    .unwrap();
    let small = scale(&src, 1920, 1080, Kernel::Bicubic).unwrap();
    let mut big = Frame::new(PixelFormat::Yuv420p8, 3840, 2160, 0, src.time_base);
    rate("scale 1080p to 2160p 8-bit Bicubic", 100, || {
        s.run(&small, &mut big).unwrap()
    });

    let mut bgra = vec![0u8; 2560 * 1440 * 4];
    rate("yuv to bgra 1440p 8-bit", 300, || {
        yuv_to_packed(
            &src,
            ColorSpec::BT709,
            Chroma::Nearest,
            PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
        )
        .unwrap()
    });
    rate("yuv to bgra 1440p 8-bit, bilinear chroma", 300, || {
        yuv_to_packed(
            &src,
            ColorSpec::BT709,
            Chroma::Bilinear,
            PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
        )
        .unwrap()
    });
    let mut out_yuv = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
    rate("bgrx to yuv 1440p 8-bit", 300, || {
        packed_to_yuv(
            Packed::new(2560, 1440, Order::Bgrx, &bgra),
            ColorSpec::BT709,
            PixelFormat::Yuv420p8,
            &mut out_yuv,
        )
        .unwrap()
    });
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.install(|| {
        rate("yuv to bgra 1440p 8-bit, 1 thread", 60, || {
            yuv_to_packed(
                &src,
                ColorSpec::BT709,
                Chroma::Nearest,
                PackedMut::new(2560, 1440, Order::Bgra, &mut bgra),
            )
            .unwrap()
        });
        rate("bgrx to yuv 1440p 8-bit, 1 thread", 60, || {
            packed_to_yuv(
                Packed::new(2560, 1440, Order::Bgrx, &bgra),
                ColorSpec::BT709,
                PixelFormat::Yuv420p8,
                &mut out_yuv,
            )
            .unwrap()
        });
    });
    let mut rgba = Rgba::new(0, 0);
    rate("yuv to rgba 1440p 8-bit", 300, || {
        yuv_to_rgba(&src, ColorSpec::BT709, &mut rgba).unwrap()
    });
    let mut yuv = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, src.time_base);
    rate("rgba to yuv 1440p 8-bit", 300, || {
        rgba_to_yuv(&rgba, ColorSpec::BT709, PixelFormat::Yuv420p8, &mut yuv).unwrap()
    });
    rate("bt601 to bt709 1440p 8-bit", 300, || {
        convert_color(
            &src,
            ColorSpec::BT601,
            ColorSpec::BT709,
            PixelFormat::Yuv420p8,
            &mut yuv,
        )
        .unwrap()
    });
    rate("rotate 90 1440p 8-bit", 300, || {
        rotate(&src, Rotation::R90, &mut yuv).unwrap()
    });
    let mut caption = Rgba::new(1600, 160);
    for (i, p) in caption.data.chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(&[255, 255, 255, if (i / 7) % 3 == 0 { 255 } else { 0 }]);
    }
    let overlay = Overlay::new(&caption, ColorSpec::BT709, PixelFormat::Yuv420p8).unwrap();
    let mut target = src.clone();
    rate("overlay 1600x160 caption on 1440p", 300, || {
        overlay.draw(&mut target, 480, 1200).unwrap()
    });

    let seconds = 60.0;
    let r = 48000;
    let noise: Vec<f32> = {
        let mut s = 0x1234_5678u32;
        (0..(seconds * r as f64) as usize * 2)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    };
    let stereo = AudioBuffer {
        samples: noise.clone(),
        rate: r,
        channels: 2,
    };
    let blocks: Vec<AudioBuffer> = stereo
        .samples
        .chunks(2 * 1024)
        .map(|c| AudioBuffer {
            samples: c.to_vec(),
            rate: r,
            channels: 2,
        })
        .collect();
    let t = Instant::now();
    let mut meter = LoudnessMeter::new(r, 2).unwrap();
    for b in &blocks {
        meter.process(b).unwrap();
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "r128 meter 48 kHz stereo, 1024-frame blocks: {:.0}x real time",
        seconds / s
    );
    let t = Instant::now();
    let mut meter = LoudnessMeter::new(r, 2).unwrap();
    meter.process(&stereo).unwrap();
    let s = t.elapsed().as_secs_f64();
    println!(
        "r128 meter 48 kHz stereo, one block: {:.0}x real time",
        seconds / s
    );

    for (from, to, ch) in [(44100, 48000, 2u16), (48000, 44100, 2), (48000, 16000, 1)] {
        let input = AudioBuffer {
            samples: noise[..(seconds * from as f64) as usize * ch as usize].to_vec(),
            rate: from,
            channels: ch,
        };
        let mut rs = Resampler::new(from, to, ch).unwrap();
        let mut out = AudioBuffer::new(to, ch);
        let t = Instant::now();
        for c in input.samples.chunks(1024 * ch as usize) {
            let part = AudioBuffer {
                samples: c.to_vec(),
                rate: from,
                channels: ch,
            };
            rs.process(&part, &mut out).unwrap();
        }
        let s = t.elapsed().as_secs_f64();
        println!(
            "resample {from} to {to} Hz, {ch} channels: {:.0}x real time",
            seconds / s
        );
    }
    let mut lim = Limiter::new(r, 2, -1.0).unwrap();
    let mut out = AudioBuffer::new(r, 2);
    let t = Instant::now();
    for b in &blocks {
        lim.process(b, &mut out).unwrap();
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "true-peak limiter 48 kHz stereo: {:.0}x real time",
        seconds / s
    );
}

#[test]
fn hostile_input_returns_errors_instead_of_panicking() {
    let tb = TimeBase::new(1, 30);
    let mut frames = Vec::new();
    for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
        for (w, h) in [(0, 0), (1, 1), (3, 5), (17, 1), (1, 33)] {
            frames.push(Frame::new(format, w, h, 0, tb));
        }
        let mut short = Frame::new(format, 64, 64, 0, tb);
        short.planes[1].data.truncate(10);
        frames.push(short);
        let mut lying = Frame::new(format, 64, 64, 0, tb);
        lying.width = u32::MAX;
        lying.height = u32::MAX;
        frames.push(lying);
        let mut wide = Frame::new(format, 8, 8, 0, tb);
        wide.planes[0].stride = usize::MAX / 2;
        frames.push(wide);
    }
    let mut out = Frame::new(PixelFormat::Yuv420p8, 0, 0, 0, tb);
    let fill = Fill::black(Range::Limited, PixelFormat::Yuv420p8);
    for f in &frames {
        for r in [Rotation::R90, Rotation::R180, Rotation::R270] {
            let _ = rotate(f, r, &mut out);
        }
        let _ = convert_color(
            f,
            ColorSpec::BT601,
            ColorSpec::BT709,
            PixelFormat::Yuv420p10,
            &mut out,
        );
        let mut rgba = Rgba::new(0, 0);
        let _ = yuv_to_rgba(f, ColorSpec::BT709, &mut rgba);
        for k in [Kernel::Bilinear, Kernel::Lanczos] {
            let _ = scale(f, 7, 3, k);
            let _ = scale(f, u32::MAX, u32::MAX, k);
        }
        let _ = crop(f, 1, 1, 2, 2, &mut out);
        let _ = pad(f, u32::MAX, u32::MAX, 0, 0, fill, &mut out);
        let _ = pad(f, 70, 70, 3, 3, fill, &mut out);
        let mut g = f.clone();
        let _ = flip_horizontal(&mut g);
        let _ = flip_vertical(&mut g);
        let mut packed = vec![0u8; 16];
        let _ = yuv_to_packed(
            f,
            ColorSpec::BT709,
            Chroma::Bilinear,
            PackedMut::new(2, 2, Order::Bgra, &mut packed),
        );
        let _ = packed_to_yuv(
            Packed::new(u32::MAX, u32::MAX, Order::Bgrx, &packed),
            ColorSpec::BT709,
            f.format,
            &mut out,
        );
        let _ = rgba_to_yuv(
            &Rgba {
                width: u32::MAX,
                height: 2,
                data: vec![0; 8],
            },
            ColorSpec::BT709,
            f.format,
            &mut out,
        );
        let _ = Overlay::new(
            &Rgba {
                width: u32::MAX,
                height: u32::MAX,
                data: vec![0; 8],
            },
            ColorSpec::BT709,
            f.format,
        );
        if let Ok(o) = Overlay::new(&Rgba::new(5, 3), ColorSpec::BT709, f.format) {
            let mut g = f.clone();
            let _ = o.draw(&mut g, -3, i32::MAX);
            let _ = o.draw(&mut g, i32::MIN, 1);
        }
    }
    let ragged = AudioBuffer {
        samples: vec![0.5; 7],
        rate: 48000,
        channels: 2,
    };
    let empty = AudioBuffer::new(44100, 1);
    let odd = AudioBuffer {
        samples: vec![f32::NAN, f32::INFINITY, -1.0],
        rate: 1,
        channels: 3,
    };
    for a in [&ragged, &empty, &odd] {
        for rate in [0, 1, 8000, 48000, 384000, u32::MAX] {
            let _ = resample(a, rate);
            let _ = Resampler::new(a.rate, rate, a.channels);
        }
        for ch in [0, 1, 2, 9] {
            let _ = downmix(a, ch);
        }
        if let Ok(mut m) = LoudnessMeter::new(a.rate.max(1), a.channels.max(1)) {
            let _ = m.process(a);
            let _ = m.summary();
        }
        let mut b = a.clone();
        let _ = normalize(&mut b, -16.0, -1.0);
        gain(&mut b, f32::NAN);
        if let Ok(mut l) = Limiter::new(a.rate, a.channels, -1.0) {
            let mut o = AudioBuffer::new(a.rate, a.channels);
            let _ = l.process(a, &mut o);
            l.flush(&mut o);
        }
    }
    let _ = FpsConverter::new(0, 0);
    if let Ok(mut c) = FpsConverter::new(30, 1) {
        for pts in [i64::MAX, i64::MIN, 0, -1] {
            let _ = c.push(Frame::new(
                PixelFormat::Yuv420p8,
                2,
                2,
                pts,
                TimeBase::new(1, 0),
            ));
        }
        let _ = c.flush();
    }
}

mod overlay_reference {
    use super::super::plane::{Sample, check, samples_mut};
    use super::*;

    struct Layer {
        width: usize,
        height: usize,
        alpha: Vec<u16>,
        value: Vec<u32>,
        spans: Vec<(usize, usize)>,
    }

    impl Layer {
        fn new(width: usize, height: usize, alpha: Vec<u16>, value: Vec<u32>) -> Self {
            let spans = (0..height)
                .map(|y| {
                    let row = &alpha[y * width..(y + 1) * width];
                    match row.iter().position(|&a| a > 0) {
                        Some(first) => {
                            let last = row.iter().rposition(|&a| a > 0).unwrap_or(first);
                            (first, last + 1)
                        }
                        None => (0, 0),
                    }
                })
                .collect();
            Layer {
                width,
                height,
                alpha,
                value,
                spans,
            }
        }

        fn draw<S: Sample>(
            &self,
            dst: &mut [S],
            stride: usize,
            dw: usize,
            dh: usize,
            x: i64,
            y: i64,
        ) {
            for r in 0..self.height {
                let ty = y + r as i64;
                if ty < 0 || ty >= dh as i64 {
                    continue;
                }
                let (a, b) = self.spans[r];
                let from = (a as i64).max(-x) as usize;
                let to = (b as i64).min(dw as i64 - x);
                if to <= from as i64 {
                    continue;
                }
                let to = to as usize;
                let row = &mut dst[ty as usize * stride..];
                let base = r * self.width;
                for c in from..to {
                    let al = self.alpha[base + c] as u32;
                    if al == 0 {
                        continue;
                    }
                    let o = &mut row[(x + c as i64) as usize];
                    let v = (o.get() as u32 * (256 - al) + self.value[base + c] + 128) >> 8;
                    *o = S::put(v as i32);
                }
            }
        }
    }

    struct Reference {
        format: PixelFormat,
        luma: Layer,
        u: Layer,
        v: Layer,
    }

    impl Reference {
        fn new(image: &Rgba, spec: ColorSpec, format: PixelFormat) -> Self {
            let w = image.width as usize;
            let h = image.height as usize;
            let l = spec.levels(format.bit_depth());
            let m = spec.matrix.forward();
            let top = ((1u32 << format.bit_depth()) - 1) as f64;
            let mut ya = vec![0u16; w * h];
            let mut yv = vec![0u32; w * h];
            let mut planes = vec![[0.0f64; 3]; w * h];
            for (i, p) in image.data.chunks_exact(4).take(w * h).enumerate() {
                let c = [
                    p[0] as f64 / 255.0,
                    p[1] as f64 / 255.0,
                    p[2] as f64 / 255.0,
                ];
                let dot = |k: usize| m[k][0] * c[0] + m[k][1] * c[1] + m[k][2] * c[2];
                let a = (p[3] as f64 * 256.0 / 255.0).round();
                let y = (l.y_off + l.y_range * dot(0)).clamp(0.0, top);
                ya[i] = a as u16;
                yv[i] = (y * a).round() as u32;
                planes[i] = [
                    a,
                    a * (l.c_off + l.c_range * dot(1)),
                    a * (l.c_off + l.c_range * dot(2)),
                ];
            }
            let cw = w.div_ceil(2);
            let ch = h.div_ceil(2);
            let mut ca = vec![0u16; cw * ch];
            let mut cu = vec![0u32; cw * ch];
            let mut cv = vec![0u32; cw * ch];
            for cy in 0..ch {
                for cx in 0..cw {
                    let mut s = [0.0f64; 3];
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (px, py) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                        let q = planes[py * w + px];
                        s.iter_mut().zip(q).for_each(|(a, b)| *a += b);
                    }
                    let i = cy * cw + cx;
                    ca[i] = (s[0] / 4.0).round() as u16;
                    cu[i] = (s[1] / 4.0).round().clamp(0.0, top * 256.0) as u32;
                    cv[i] = (s[2] / 4.0).round().clamp(0.0, top * 256.0) as u32;
                }
            }
            Reference {
                format,
                luma: Layer::new(w, h, ya, yv),
                u: Layer::new(cw, ch, ca.clone(), cu),
                v: Layer::new(cw, ch, ca, cv),
            }
        }

        fn draw(&self, frame: &mut Frame, x: i32, y: i32) {
            check(frame).unwrap();
            let (fw, fh) = (frame.width as usize, frame.height as usize);
            let (cw, ch) = (fw.div_ceil(2), fh.div_ceil(2));
            let (x, y) = (x as i64, y as i64);
            let (cx, cy) = (x.div_euclid(2), y.div_euclid(2));
            let jobs = [
                (&self.luma, fw, fh, x, y),
                (&self.u, cw, ch, cx, cy),
                (&self.v, cw, ch, cx, cy),
            ];
            for (plane, (layer, w, h, px, py)) in frame.planes.iter_mut().zip(jobs) {
                match self.format {
                    PixelFormat::Yuv420p8 => {
                        let stride = plane.stride;
                        layer.draw(samples_mut::<u8>(plane).unwrap(), stride, w, h, px, py);
                    }
                    PixelFormat::Yuv420p10 => {
                        let stride = plane.stride / 2;
                        layer.draw(samples_mut::<u16>(plane).unwrap(), stride, w, h, px, py);
                    }
                }
            }
        }
    }

    struct Noise(u64);

    impl Noise {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    fn layers(w: u32, h: u32, noise: &mut Noise) -> Vec<Rgba> {
        let mut out = Vec::new();
        let mut make = |f: &mut dyn FnMut(u32, u32, &mut [u8])| {
            let mut img = Rgba::new(w, h);
            for (i, px) in img.data.chunks_exact_mut(4).enumerate() {
                f(i as u32 % w, i as u32 / w, px);
            }
            out.push(img);
        };
        make(&mut |_, _, _| {});
        make(&mut |x, y, px| {
            px.copy_from_slice(&[(x * 37) as u8, (y * 91) as u8, (x ^ y) as u8, 255])
        });
        make(&mut |_, _, px| {
            let r = noise.next();
            px.copy_from_slice(&r.to_le_bytes());
            if r.is_multiple_of(3) {
                px[3] = 0;
            }
        });
        make(&mut |_, _, px| {
            if noise.next().is_multiple_of(23) {
                px.copy_from_slice(&noise.next().to_le_bytes());
                px[3] = px[3].max(1);
            }
        });
        let colours = [[255, 255, 255], [255, 220, 0], [0, 0, 0], [30, 140, 255]];
        make(&mut |x, y, px| {
            let band = y * 3 >= h && y * 4 < h * 3 + 1 && x * 8 >= w && x * 8 < w * 7;
            if band {
                let c = colours[((x / 3 + y) % 4) as usize];
                let edge = (x * 13 + y * 7) % 9;
                let a = if edge < 3 {
                    0
                } else {
                    [40, 128, 255, 255, 255, 200][edge as usize - 3]
                };
                px.copy_from_slice(&[c[0], c[1], c[2], a]);
                if edge == 3 {
                    px[0] = px[0] / 2 + 10;
                }
            }
        });
        make(&mut |x, y, px| {
            let c = colours[((x / 4 + y / 4) % 4) as usize];
            let a = [0, 255, 128, 1][((x / 6 + y / 2) % 4) as usize];
            px.copy_from_slice(&[c[0], c[1], c[2], a]);
        });
        if w > 1 && h > 1 {
            make(&mut |x, y, px| {
                if x == w - 1 && y == h - 1 {
                    px.copy_from_slice(&[200, 30, 90, 1]);
                }
            });
            make(&mut |x, y, px| {
                if (x == 1 && y == 1) || (x == w / 2 && y == h - 2) {
                    px.copy_from_slice(&[10, 250, 90, 2]);
                }
            });
        }
        out
    }

    fn canvas(format: PixelFormat, w: u32, h: u32, noise: &mut Noise) -> Frame {
        let mut f = Frame::new(format, w, h, 0, TimeBase::new(1, 30));
        for plane in &mut f.planes {
            match format {
                PixelFormat::Yuv420p8 => {
                    plane.data.iter_mut().for_each(|s| *s = noise.next() as u8)
                }
                PixelFormat::Yuv420p10 => samples_mut::<u16>(plane)
                    .unwrap()
                    .iter_mut()
                    .for_each(|s| *s = (noise.next() % 1024) as u16),
            }
        }
        f
    }

    #[test]
    fn a_cropped_overlay_blends_what_the_full_size_one_blended() {
        let mut noise = Noise(7);
        let specs = [
            ColorSpec::BT709,
            ColorSpec::new(Matrix::Bt601, Range::Full),
            ColorSpec::new(Matrix::Bt2020, Range::Limited),
        ];
        let sizes = [
            (1, 1),
            (2, 2),
            (1, 6),
            (7, 5),
            (33, 17),
            (64, 36),
            (101, 3),
            (40, 41),
        ];
        for format in [PixelFormat::Yuv420p8, PixelFormat::Yuv420p10] {
            for (k, &(w, h)) in sizes.iter().enumerate() {
                let spec = specs[k % specs.len()];
                for layer in layers(w, h, &mut noise) {
                    let ours = Overlay::new(&layer, spec, format).unwrap();
                    let reference = Reference::new(&layer, spec, format);
                    for (fw, fh) in [(48u32, 30u32), (9, 7)] {
                        let (lw, lh) = (w as i32, h as i32);
                        let (fw_, fh_) = (fw as i32, fh as i32);
                        for (x, y) in [
                            (0, 0),
                            (3, 2),
                            (-3, -2),
                            (-1, 5),
                            (5, -1),
                            (fw_ - 2, fh_ - 1),
                            (fw_ - lw / 2, fh_ - lh / 2 - 1),
                            (-lw - 1, 0),
                            (0, -lh - 2),
                            (fw_ + 3, fh_ + 3),
                        ] {
                            let mut a = canvas(format, fw, fh, &mut noise);
                            let mut b = a.clone();
                            ours.draw(&mut a, x, y).unwrap();
                            reference.draw(&mut b, x, y);
                            assert_eq!(a, b, "{format:?} {w}x{h} on {fw}x{fh} at {x},{y}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn linear_light_resize_keeps_light_and_alpha_edges() {
    let mut board = Rgba::new(64, 64);
    for (i, px) in board.data.chunks_exact_mut(4).enumerate() {
        let v = if (i % 64 + i / 64) % 2 == 0 { 255 } else { 0 };
        px.copy_from_slice(&[v, v, v, 255]);
    }
    let s = Scaler::new(64, 64, 16, 16, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear);
    let mut out = Rgba::new(1, 1);
    s.convert(Image::Rgba(&board), ImageMut::Rgba(&mut out))
        .unwrap();
    assert_eq!((out.width, out.height), (16, 16));
    assert!(
        out.data
            .chunks_exact(4)
            .all(|p| (180..=200).contains(&p[0]) && p[3] == 255)
    );

    let mut edge = Rgba::new(64, 32);
    for (i, px) in edge.data.chunks_exact_mut(4).enumerate() {
        if i % 64 < 32 {
            px.copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let s = Scaler::new(64, 32, 21, 11, PixelFormat::Yuv420p8, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear);
    s.convert(Image::Rgba(&edge), ImageMut::Rgba(&mut out))
        .unwrap();
    let seen: Vec<_> = out.data.chunks_exact(4).filter(|p| p[3] > 8).collect();
    assert!(seen.iter().any(|p| p[3] < 250));
    assert!(seen.iter().all(|p| p[0] >= 250 && p[1] <= 4 && p[2] <= 4));

    let mut flat = Frame::new(PixelFormat::Yuv420p10, 64, 48, 0, TimeBase::new(1, 25));
    for (p, v) in flat.planes.iter_mut().zip([400u16, 450, 560]) {
        p.data
            .chunks_exact_mut(2)
            .for_each(|c| c.copy_from_slice(&v.to_ne_bytes()));
    }
    let s = Scaler::new(64, 48, 40, 30, PixelFormat::Yuv420p10, Kernel::Lanczos)
        .unwrap()
        .light(Light::Linear)
        .output(PixelFormat::Yuv420p8);
    let mut small = Frame::new(PixelFormat::Yuv420p8, 2, 2, 0, TimeBase::new(1, 25));
    s.run(&flat, &mut small).unwrap();
    for (p, want) in small.planes.iter().zip([100u8, 112, 140]) {
        assert!(p.data.iter().all(|&v| v.abs_diff(want) <= 1), "{want}");
    }
}

fn sine_buffer(hz: f64, seconds: f64, rate: u32) -> AudioBuffer {
    let n = (seconds * f64::from(rate)) as usize;
    let mut b = AudioBuffer::new(rate, 2);
    for i in 0..n {
        let v = (0.5 * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32;
        b.samples.extend_from_slice(&[v, v]);
    }
    b
}

fn has_text(img: &Rgba, a: spectrum::Area) -> bool {
    (a.y..a.y + a.height).any(|y| {
        (a.x..a.x + a.width).any(|x| {
            let at = (y as usize * img.width as usize + x as usize) * 4;
            img.data[at..at + 3] == spectrum::TEXT
        })
    })
}

#[test]
fn spectrogram_size_and_axis_labels() {
    let opts = spectrum::Options::default();
    let s = spectrogram(&sine_buffer(1000.0, 3.0, 48_000), &opts).unwrap();
    assert_eq!((s.image.width, s.image.height), (1200, 360));
    assert_eq!(s.image.data.len(), 1200 * 360 * 4);
    let (plot, bar) = (s.plot, s.bar);
    assert!(plot.x > 0 && plot.y > 0 && plot.width >= 1000 && plot.height >= 280);
    assert!(bar.x > plot.x + plot.width && bar.x + bar.width < 1200);
    assert_eq!(bar.y, plot.y);
    assert_eq!(bar.height, plot.height);
    for l in &s.labels {
        let a = l.area;
        assert!(a.x >= 0 && a.y >= 0, "{l:?}");
        assert!(a.x + a.width <= 1200 && a.y + a.height <= 360, "{l:?}");
        assert!(has_text(&s.image, a), "{l:?} is not drawn");
    }
    let of = |axis| {
        s.labels
            .iter()
            .filter(|l| l.axis == axis && l.value.is_some())
            .collect::<Vec<_>>()
    };
    let title = |axis| {
        s.labels
            .iter()
            .find(|l| l.axis == axis && l.value.is_none())
            .unwrap()
    };
    let freq = of(spectrum::Axis::Frequency);
    let texts: Vec<&str> = freq.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["0", "2", "4", "6", "8", "10", "12"]);
    assert_eq!(freq[0].at, plot.y + plot.height - 1);
    assert_eq!(freq.last().unwrap().at, plot.y);
    for l in &freq {
        assert_eq!(l.area.x + l.area.width, plot.x - 9);
        assert!(l.area.y <= l.at && l.at < l.area.y + l.area.height);
        let v = l.value.unwrap();
        let expect = plot.y + ((1.0 - v / 12_000.0) * f64::from(plot.height)).round() as i32;
        assert_eq!(l.at, expect.min(plot.y + plot.height - 1));
    }
    assert_eq!(title(spectrum::Axis::Frequency).text, "kHz");
    assert!(title(spectrum::Axis::Frequency).area.y + 14 <= plot.y);
    let time = of(spectrum::Axis::Time);
    assert_eq!(time[0].text, "0.0s");
    assert_eq!(time[0].at, plot.x);
    assert!(time.len() >= 4);
    for w in time.windows(2) {
        assert!(w[0].at < w[1].at && w[0].area.x + w[0].area.width < w[1].area.x);
    }
    for l in &time {
        assert_eq!(l.area.y, plot.y + plot.height + 9);
        let t = l.value.unwrap();
        let expect = plot.x + (t / 3.0 * f64::from(plot.width)).round() as i32;
        assert_eq!(l.at, expect.min(plot.x + plot.width - 1));
    }
    let level = of(spectrum::Axis::Level);
    assert_eq!(level[0].text, "0");
    assert_eq!(level[0].at, bar.y);
    assert_eq!(level.last().unwrap().text, "-80");
    for l in &level {
        assert_eq!(l.area.x, bar.x + bar.width + 9);
    }
    assert_eq!(title(spectrum::Axis::Level).text, "dB");
}

#[test]
fn spectrogram_puts_a_tone_on_its_row() {
    let opts = spectrum::Options::default();
    for (hz, rate) in [(1000.0, 48_000), (3000.0, 16_000), (6000.0, 44_100)] {
        let s = spectrogram(&sine_buffer(hz, 1.0, rate), &opts).unwrap();
        let x = s.plot.x + s.plot.width / 2;
        let row = (s.plot.y..s.plot.y + s.plot.height)
            .max_by_key(|&y| {
                let at = (y as usize * 1200 + x as usize) * 4;
                s.image.data[at..at + 3]
                    .iter()
                    .map(|&v| u32::from(v))
                    .sum::<u32>()
            })
            .unwrap();
        let expect = s.plot.y + ((1.0 - hz / 12_000.0) * f64::from(s.plot.height)) as i32;
        assert!(
            (row - expect).abs() <= 1,
            "{hz} Hz at {rate}: row {row}, expected {expect}"
        );
    }
}

#[test]
fn spectrogram_refuses_bad_input() {
    let opts = spectrum::Options::default();
    assert!(spectrogram(&AudioBuffer::new(48_000, 1), &opts).is_err());
    let small = spectrum::Options { width: 100, ..opts };
    assert!(spectrogram(&sine_buffer(1000.0, 0.1, 48_000), &small).is_err());
    let floor = spectrum::Options {
        floor_db: 3.0,
        ..opts
    };
    assert!(spectrogram(&sine_buffer(1000.0, 0.1, 48_000), &floor).is_err());
}

fn sine_tone(hz: f64, amplitude: f64, seconds: f64, rate: u32) -> AudioBuffer {
    let n = (seconds * f64::from(rate)) as usize;
    let mut b = AudioBuffer::new(rate, 1);
    b.samples.extend((0..n).map(|i| {
        (amplitude * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(rate)).sin()) as f32
    }));
    b
}

fn pixel(s: &spectrum::Spectrogram, x: i32, y: i32) -> [u8; 3] {
    let at = (y as usize * s.image.width as usize + x as usize) * 4;
    [0, 1, 2].map(|c| s.image.data[at + c])
}

fn brightest(s: &spectrum::Spectrogram, x: i32) -> (i32, [u8; 3]) {
    (s.plot.y..s.plot.y + s.plot.height)
        .map(|y| (y, pixel(s, x, y)))
        .max_by_key(|(_, p)| p.iter().map(|&v| u32::from(v)).sum::<u32>())
        .unwrap()
}

#[test]
fn spectrogram_levels_hold_over_one_and_many_windows() {
    let hz = 1500.0;
    let cases = [
        (1.5, 1200, 360, 1),
        (30.0, 1200, 360, 2),
        (30.0, 320, 160, 9),
    ];
    for (seconds, width, height, windows) in cases {
        let opts = spectrum::Options {
            width,
            height,
            ..spectrum::Options::default()
        };
        let plot_width = width as f64 - 152.0;
        let span = seconds * 48_000.0 / plot_width;
        assert_eq!((span / 1024.0).ceil() as usize, windows);
        for (amplitude, db) in [(1.0, 0.0), (0.1, -20.0)] {
            let s = spectrogram(&sine_tone(hz, amplitude, seconds, 48_000), &opts).unwrap();
            assert_eq!(s.window, 2048);
            let expect_row =
                s.plot.y + ((1.0 - hz / 12_000.0) * f64::from(s.plot.height)).round() as i32;
            let want = spectrum::color(((db + 90.0) / 90.0) as f32);
            for x in [
                s.plot.x + s.plot.width / 3,
                s.plot.x + s.plot.width / 2,
                s.plot.x + s.plot.width * 2 / 3,
            ] {
                let (row, got) = brightest(&s, x);
                assert!(
                    (row - expect_row).abs() <= 1,
                    "{seconds} s at {width}x{height}, {db} dB: row {row}, expected {expect_row}"
                );
                assert!(
                    got.iter().zip(want).all(|(&g, w)| g.abs_diff(w) <= 2),
                    "{seconds} s at {width}x{height}, {db} dB: {got:?}, expected {want:?}"
                );
                let far = s.plot.y + s.plot.height / 8;
                assert!(
                    pixel(&s, x, far).iter().map(|&v| u32::from(v)).sum::<u32>() < 120,
                    "{seconds} s at {width}x{height}: energy at 10.5 kHz"
                );
            }
        }
    }
}

#[test]
fn spectrogram_leaves_rows_above_nyquist_blank() {
    let s = spectrogram(
        &sine_tone(1000.0, 0.5, 2.0, 16_000),
        &spectrum::Options::default(),
    )
    .unwrap();
    let x = s.plot.x + s.plot.width / 2;
    let row = |hz: f64| s.plot.y + ((1.0 - hz / 12_000.0) * f64::from(s.plot.height)) as i32;
    for hz in [8_200.0, 10_000.0, 11_900.0] {
        assert_eq!(pixel(&s, x, row(hz)), spectrum::BACKGROUND, "{hz} Hz");
    }
    for hz in [500.0, 4_000.0, 7_800.0] {
        assert_ne!(pixel(&s, x, row(hz)), spectrum::BACKGROUND, "{hz} Hz");
    }
}

#[test]
fn text_at_the_edges_is_clipped_never_wrapped_onto_other_rows() {
    let painted = |img: &Rgba| -> Vec<(u32, u32)> {
        let mut at = Vec::new();
        for y in 0..img.height {
            for x in 0..img.width {
                let i = (y as usize * img.width as usize + x as usize) * 4;
                if img.data[i..i + 3] != [0, 0, 0] {
                    at.push((x, y));
                }
            }
        }
        at
    };
    let ink = [255, 255, 255];
    let mut left = Rgba::new(20, 30);
    spectrum::draw_text(&mut left, -3, 4, "8", ink);
    let px = painted(&left);
    assert!(px.iter().any(|&(x, _)| x == 0), "{px:?}");
    assert!(
        px.iter().all(|&(x, y)| x < 8 && (4..18).contains(&y)),
        "{px:?}"
    );
    let mut right = Rgba::new(20, 30);
    spectrum::draw_text(&mut right, 14, 4, "8", ink);
    let px = painted(&right);
    assert!(px.iter().any(|&(x, _)| x == 19), "{px:?}");
    assert!(
        px.iter().all(|&(x, y)| x >= 14 && (4..18).contains(&y)),
        "{px:?}"
    );
    let mut low = Rgba::new(20, 10);
    spectrum::draw_text(&mut low, 2, 6, "8", ink);
    assert!(painted(&low).iter().all(|&(_, y)| y >= 6));
    let mut above = Rgba::new(20, 10);
    spectrum::draw_text(&mut above, 2, -10, "8", ink);
    assert!(painted(&above).iter().all(|&(_, y)| y < 4));
}
