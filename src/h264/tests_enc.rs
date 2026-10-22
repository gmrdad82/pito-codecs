use super::tests::{assert_same, dir, ffmpeg_present, ffmpeg_yuv, psnr, raw, to_annexb};
use super::*;
use crate::media::Rate;

const CUT: i64 = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Scene {
    Flat,
    Gradient,
    Noise,
    Motion,
    Cut,
}

fn hash(x: usize, y: usize, i: i64, k: usize) -> u32 {
    let mut s = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (i as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ k as u64;
    s ^= s >> 29;
    s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    s ^= s >> 32;
    s as u32
}

fn sample(scene: Scene, x: usize, y: usize, pw: usize, ph: usize, i: i64, k: usize) -> u8 {
    let noise = |amp: u32| hash(x, y, i, k) % (2 * amp + 1);
    let v = match scene {
        Scene::Flat => [96, 120, 140][k],
        Scene::Gradient => (x * 200 / pw.max(1) + y * 50 / ph.max(1) + 2 * i as usize) as u32,
        Scene::Noise => 108 + noise(20),
        Scene::Motion => {
            let (mx, my) = (x as i64 + 3 * i, y as i64 + 2 * i);
            let checker = ((mx / 8 + my / 8) % 2) as u32;
            50 + checker * 90 + ((x + y) % 7) as u32 + noise(6)
        }
        Scene::Cut => {
            if i < CUT {
                return sample(Scene::Gradient, x, y, pw, ph, i, k);
            }
            255 - (x * 180 / pw.max(1)) as u32
        }
    };
    v.min(255) as u8
}

fn clip(scene: Scene, w: u32, h: u32, n: i64) -> Vec<Frame> {
    (0..n)
        .map(|i| {
            let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i, TimeBase::new(1, 30));
            for (k, p) in f.planes.iter_mut().enumerate() {
                for y in 0..p.height {
                    for x in 0..p.width {
                        p.data[y * p.stride + x] = sample(scene, x, y, p.width, p.height, i, k);
                    }
                }
            }
            f
        })
        .collect()
}

fn config(w: u32, h: u32, profile: Profile, rate: Rate) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        profile,
        rate,
        gop: 250,
        threads: 2,
        ..EncoderConfig::default()
    }
}

fn encode_all(cfg: &EncoderConfig, frames: &[Frame]) -> (Vec<u8>, Vec<Packet>) {
    let mut enc = Encoder::new(cfg.clone()).unwrap();
    let mut packets = Vec::new();
    for f in frames {
        packets.extend(enc.encode(f).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), frames.len(), "one packet per frame");
    assert!(packets[0].keyframe);
    (enc.avcc(), packets)
}

fn decoded_bit_exact(name: &str, avcc: &[u8], packets: &[Packet], w: u32, h: u32) -> Vec<u8> {
    let mut dec = Decoder::with_extradata(avcc).unwrap();
    let mut frames = Vec::new();
    for p in packets {
        frames.extend(dec.decode(p).unwrap());
    }
    frames.extend(dec.flush().unwrap());
    assert_eq!(frames.len(), packets.len(), "{name}: frames decoded");
    assert!(
        frames.iter().all(|f| (f.width, f.height) == (w, h)),
        "{name}: decoded size is not the cropped {w}x{h}"
    );
    let ours = raw(&frames);
    if !ffmpeg_present() {
        return ours;
    }
    let path = dir().join(format!("{name}.h264"));
    std::fs::write(&path, to_annexb(avcc, packets)).unwrap();
    let theirs = ffmpeg_yuv(&path);
    let size = (w * h + 2 * (w / 2) * (h / 2)) as usize;
    assert_eq!(theirs.len(), size * packets.len(), "{name}: ffmpeg frames");
    assert_same(&ours, &theirs, size, name);
    ours
}

fn kbps(packets: &[Packet], fps: f64) -> f64 {
    let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
    bytes as f64 * 8.0 * fps / packets.len() as f64 / 1000.0
}

#[test]
fn cavlc_decodes_bit_exact_at_every_preset_and_qp() {
    let (w, h) = (96, 64);
    for scene in [Scene::Flat, Scene::Noise, Scene::Motion] {
        let frames = clip(scene, w, h, 5);
        let source = raw(&frames);
        for preset in [Preset::Speed, Preset::Balanced, Preset::Quality] {
            for (qp, floor) in [(0u8, 40.0), (26, 26.0), (40, 18.0)] {
                let name = format!("cavlc_{scene:?}_{preset:?}_qp{qp}");
                let cfg = EncoderConfig {
                    preset,
                    gop: 4,
                    b_frames: 2,
                    ..config(w, h, Profile::Baseline, Rate::Qp(qp))
                };
                let (avcc, packets) = encode_all(&cfg, &frames);
                assert_eq!(avcc[1], 66, "{name}: Baseline profile_idc");
                assert!(
                    packets.iter().all(|p| p.dts == p.pts),
                    "{name}: Baseline reordered frames"
                );
                assert!(
                    packets.iter().filter(|p| p.keyframe).count() >= 2,
                    "{name}: an IDR at least every 4 frames"
                );
                let ours = decoded_bit_exact(&name, &avcc, &packets, w, h);
                let db = psnr(&ours, &source);
                assert!(db > floor, "{name}: PSNR {db:.2} dB under {floor}");
            }
        }
    }
}

#[test]
fn bitrate_modes_land_within_thirty_percent_of_target() {
    let (w, h, fps) = (96, 64, 30.0);
    let frames = clip(Scene::Motion, w, h, 30);
    for profile in [Profile::Baseline, Profile::High] {
        for (rate, target) in [
            (Rate::Bitrate(120), 120.0),
            (
                Rate::Vbr {
                    kbps: 160,
                    max_kbps: 320,
                },
                160.0,
            ),
        ] {
            let name = format!("rate_{profile:?}_{target}");
            let (avcc, packets) = encode_all(&config(w, h, profile, rate), &frames);
            let got = kbps(&packets, fps);
            assert!(
                (got / target - 1.0).abs() <= 0.3,
                "{name}: {got:.0} kbps for a {target} kbps target"
            );
            decoded_bit_exact(&name, &avcc, &packets, w, h);
        }
    }
}

#[test]
fn a_scene_cut_starts_a_new_idr() {
    let (w, h) = (160, 128);
    let frames = clip(Scene::Cut, w, h, CUT + 6);
    let cfg = EncoderConfig {
        gop: 60,
        ..config(w, h, Profile::High, Rate::Bitrate(600))
    };
    let (avcc, packets) = encode_all(&cfg, &frames);
    let keys: Vec<i64> = packets
        .iter()
        .filter(|p| p.keyframe)
        .map(|p| p.pts)
        .collect();
    assert_eq!(keys, [0, CUT], "keyframes");
    decoded_bit_exact("scene_cut", &avcc, &packets, w, h);
}

#[test]
fn crf_shrinks_with_the_factor_and_a_flat_clip_stays_clean() {
    let (w, h) = (96, 64);
    let motion = clip(Scene::Motion, w, h, 10);
    let flat = clip(Scene::Flat, w, h, 10);
    for profile in [Profile::Baseline, Profile::High] {
        let mut last = usize::MAX;
        for crf in [18u8, 28, 38] {
            let name = format!("crf_{profile:?}_{crf}");
            let (avcc, packets) = encode_all(&config(w, h, profile, Rate::Crf(crf)), &motion);
            let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
            assert!(bytes < last, "{name}: {bytes} bytes, not below {last}");
            last = bytes;
            decoded_bit_exact(&name, &avcc, &packets, w, h);
        }
        for (rate, tag) in [(Rate::Crf(23), "crf"), (Rate::Bitrate(300), "cbr")] {
            let name = format!("flat_{profile:?}_{tag}");
            let (avcc, packets) = encode_all(&config(w, h, profile, rate), &flat);
            assert!(kbps(&packets, 30.0) <= 300.0 * 1.3, "{name}: over budget");
            let ours = decoded_bit_exact(&name, &avcc, &packets, w, h);
            let db = psnr(&ours, &raw(&flat));
            assert!(db > 40.0, "{name}: PSNR {db:.2} dB on a flat clip");
        }
    }
}

#[test]
fn extreme_rate_settings_are_clamped_or_refused() {
    let (w, h) = (64, 48);
    for (rate, why) in [
        (Rate::Bitrate(0), "bitrate must be non-zero"),
        (
            Rate::Vbr {
                kbps: 0,
                max_kbps: 100,
            },
            "bitrate must be non-zero",
        ),
        (
            Rate::Vbr {
                kbps: 100,
                max_kbps: 0,
            },
            "maximum bitrate must be non-zero",
        ),
    ] {
        match Encoder::new(config(w, h, Profile::High, rate)) {
            Ok(_) => panic!("{rate:?} accepted"),
            Err(e) => assert!(e.to_string().contains(why), "{rate:?}: {e}"),
        }
    }
    let n = 4;
    let frames = clip(Scene::Motion, w, h, n);
    let encode = |rate: Rate, profile: Profile, threads: usize| {
        let name = format!("extreme_{profile:?}_{rate:?}_t{threads}")
            .replace([' ', '{', '}', ':', ','], "");
        let cfg = EncoderConfig {
            threads,
            ..config(w, h, profile, rate)
        };
        let (avcc, packets) = encode_all(&cfg, &frames);
        decoded_bit_exact(&name, &avcc, &packets, w, h);
        (avcc, packets)
    };
    let bytes = |rate: Rate, profile: Profile| encode(rate, profile, 2).1;
    let size = |p: &[Packet]| p.iter().map(|p| p.data.len()).sum::<usize>();
    for profile in [Profile::Baseline, Profile::High] {
        let floor = size(&bytes(Rate::Qp(51), profile));
        let starved = size(&bytes(Rate::Bitrate(1), profile));
        assert!(
            starved <= floor * 2,
            "1 kbps: {starved} bytes, qp 51: {floor}"
        );
        let crf = size(&bytes(Rate::Crf(23), profile));
        let flood = size(&bytes(Rate::Bitrate(u32::MAX), profile));
        assert!(flood > crf, "u32::MAX kbps: {flood} bytes, crf 23: {crf}");
        let capped = size(&bytes(
            Rate::Vbr {
                kbps: 5000,
                max_kbps: 10,
            },
            profile,
        ));
        let at_max = size(&bytes(
            Rate::Vbr {
                kbps: 10,
                max_kbps: 10,
            },
            profile,
        ));
        assert_eq!(capped, at_max, "vbr 5000 over a 10 kbps max is not vbr 10");
        let band = 10 * 1000 / 8 * n as usize / 30 * 13 / 10;
        assert!(
            capped <= (floor * 2).max(band),
            "vbr capped at 10 kbps: {capped} bytes, qp 51: {floor}"
        );
        let data = |p: Vec<Packet>| p.into_iter().map(|p| p.data).collect::<Vec<_>>();
        let once = |rate: Rate| data(encode(rate, profile, 1).1);
        assert_eq!(
            once(Rate::Crf(255)),
            once(Rate::Crf(51)),
            "crf 255 is not crf 51"
        );
        assert_eq!(
            once(Rate::Qp(255)),
            once(Rate::Qp(51)),
            "qp 255 is not qp 51"
        );
    }
}

#[test]
fn rate_controlled_encodes_repeat_byte_for_byte() {
    let (w, h) = (96, 64);
    let frames = clip(Scene::Motion, w, h, 16);
    for profile in [Profile::High] {
        for rate in [
            Rate::Bitrate(300),
            Rate::Vbr {
                kbps: 200,
                max_kbps: 400,
            },
        ] {
            for threads in [1, 4] {
                let cfg = EncoderConfig {
                    threads,
                    ..config(w, h, profile, rate)
                };
                let data = || {
                    encode_all(&cfg, &frames)
                        .1
                        .into_iter()
                        .map(|p| p.data)
                        .collect::<Vec<_>>()
                };
                let first = data();
                for run in 1..2 {
                    assert!(
                        data() == first,
                        "{profile:?} {rate:?} at {threads} threads: run {run} differs"
                    );
                }
            }
        }
    }
}

fn options_sei(packet: &Packet) -> String {
    let text = String::from_utf8_lossy(&packet.data);
    let start = text
        .find("options: ")
        .expect("no options SEI in the first packet");
    text[start..]
        .split(|c: char| c.is_control())
        .next()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn clamped_rates_are_what_the_stream_reports() {
    let (w, h) = (128, 96);
    let frames = clip(Scene::Gradient, w, h, 1);
    for (profile, top) in [(Profile::Baseline, 800_000), (Profile::High, 1_000_000)] {
        for (rate, want) in [
            (Rate::Bitrate(u32::MAX), format!("rc=cbr bitrate={top} ")),
            (
                Rate::Vbr {
                    kbps: 100,
                    max_kbps: u32::MAX,
                },
                format!("rc=vbr bitrate=100 vbv_maxrate={top} "),
            ),
            (
                Rate::Vbr {
                    kbps: u32::MAX,
                    max_kbps: u32::MAX,
                },
                format!("rc=vbr bitrate={top} vbv_maxrate={top} "),
            ),
            (
                Rate::Vbr {
                    kbps: 5000,
                    max_kbps: 10,
                },
                "rc=vbr bitrate=10 vbv_maxrate=10 ".to_string(),
            ),
            (Rate::Bitrate(400), "rc=cbr bitrate=400 ".to_string()),
        ] {
            let (_, packets) = encode_all(&config(w, h, profile, rate), &frames);
            let sei = options_sei(&packets[0]);
            assert!(sei.contains(&want), "{profile:?} {rate:?}: {sei}");
        }
    }
}

fn padded(f: &Frame, extra: usize) -> Frame {
    let mut out = f.clone();
    for (p, src) in out.planes.iter_mut().zip(&f.planes) {
        let stride = src.width + extra;
        p.stride = stride;
        p.data = vec![0xA5; stride * src.height];
        for y in 0..src.height {
            p.data[y * stride..y * stride + src.width]
                .copy_from_slice(&src.data[y * src.stride..y * src.stride + src.width]);
        }
    }
    out
}

#[test]
fn padded_strides_encode_like_packed_frames() {
    for (w, h) in [(64, 64), (50, 38)] {
        let frames = clip(Scene::Motion, w, h, 4);
        for profile in [Profile::Baseline, Profile::High] {
            let cfg = EncoderConfig {
                threads: 1,
                ..config(w, h, profile, Rate::Crf(23))
            };
            let (avcc, packed) = encode_all(&cfg, &frames);
            for extra in [8, 16, 32] {
                let wide: Vec<_> = frames.iter().map(|f| padded(f, extra)).collect();
                assert_eq!(wide[0].planes[0].stride, w as usize + extra);
                assert_eq!(wide[0].planes[1].stride, wide[0].planes[2].stride);
                let (wide_avcc, strided) = encode_all(&cfg, &wide);
                let name = format!("padded_{w}x{h}_{profile:?}_{extra}");
                assert_eq!(wide_avcc, avcc, "{name}: avcc");
                assert!(
                    strided.iter().zip(&packed).all(|(a, b)| a.data == b.data),
                    "{name}: a padded stride changed the stream"
                );
                decoded_bit_exact(&name, &wide_avcc, &strided, w, h);
            }
        }
    }
}

#[test]
fn odd_and_tiny_sizes_crop_and_decode_bit_exact() {
    for (w, h) in [
        (2, 2),
        (4, 6),
        (16, 16),
        (18, 30),
        (30, 18),
        (34, 2),
        (50, 38),
        (62, 46),
        (64, 64),
        (100, 58),
    ] {
        let frames = clip(Scene::Motion, w, h, 5);
        for profile in [Profile::Baseline, Profile::High] {
            let name = format!("odd_{w}x{h}_{profile:?}");
            let cfg = EncoderConfig {
                gop: 3,
                ..config(w, h, profile, Rate::Crf(23))
            };
            let (avcc, packets) = encode_all(&cfg, &frames);
            decoded_bit_exact(&name, &avcc, &packets, w, h);
        }
    }
}

#[test]
fn a_single_frame_encodes_to_one_keyframe() {
    let (w, h) = (48, 34);
    let frames = clip(Scene::Gradient, w, h, 1);
    for profile in [Profile::Baseline, Profile::Main, Profile::High] {
        for b_frames in [0, 2] {
            for rate in [Rate::Crf(23), Rate::Bitrate(300), Rate::Qp(30)] {
                let name = format!("single_{profile:?}_b{b_frames}_{}", size_tag(rate));
                let cfg = EncoderConfig {
                    b_frames,
                    ..config(w, h, profile, rate)
                };
                let (avcc, packets) = encode_all(&cfg, &frames);
                assert_eq!((packets[0].pts, packets[0].dts), (0, 0), "{name}");
                decoded_bit_exact(&name, &avcc, &packets, w, h);
            }
        }
    }
}

fn size_tag(rate: Rate) -> &'static str {
    match rate {
        Rate::Crf(_) => "crf",
        Rate::Bitrate(_) => "cbr",
        Rate::Vbr { .. } => "vbr",
        Rate::Qp(_) => "qp",
    }
}

#[test]
fn frames_that_do_not_fit_are_refused_and_the_stream_goes_on() {
    let (w, h) = (64, 64);
    let mut enc = Encoder::new(config(w, h, Profile::Baseline, Rate::Crf(23))).unwrap();
    let good = clip(Scene::Gradient, w, h, 1).remove(0);
    let mut bad = vec![
        clip(Scene::Gradient, w, 48, 1).remove(0),
        clip(Scene::Gradient, 66, h, 1).remove(0),
    ];
    let mut deep = good.clone();
    deep.format = PixelFormat::Yuv420p10;
    bad.push(deep);
    let mut short = good.clone();
    short.planes[0].data.pop();
    bad.push(short);
    let mut short_chroma = good.clone();
    short_chroma.planes[2].data.truncate(10);
    bad.push(short_chroma);
    let mut narrow = good.clone();
    narrow.planes[0].stride = 32;
    bad.push(narrow);
    let mut strides = good.clone();
    strides.planes[1].stride = 40;
    strides.planes[1].data.resize(40 * 32, 0);
    bad.push(strides);
    for (i, f) in bad.iter().enumerate() {
        assert!(enc.encode(f).is_err(), "bad frame {i} accepted");
    }
    let mut packets = enc.encode(&good).unwrap();
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), 1);
    assert!(packets[0].keyframe);
    decoded_bit_exact("refused_then_good", &enc.avcc(), &packets, w, h);
}
