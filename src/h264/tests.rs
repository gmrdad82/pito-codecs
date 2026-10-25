use super::*;
use crate::testref::{Digest, Refs};
use std::time::Instant;

#[allow(dead_code)]
#[path = "refs.rs"]
pub(super) mod refs;

pub(super) use refs::raw;

pub(super) fn decode_annexb(data: &[u8], threads: usize) -> Vec<Frame> {
    let mut dec = Decoder::with_threads(threads);
    let mut frames = Vec::new();
    for (i, au) in access_units(data).into_iter().enumerate() {
        let packet = Packet {
            data: au.to_vec(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        };
        frames.extend(dec.decode(&packet).unwrap());
    }
    frames.extend(dec.flush().unwrap());
    frames
}

fn access_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    let mut pending_params = None;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let at = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            let t = data[i + 3] & 0x1f;
            let first_mb_zero = data.get(i + 4).is_some_and(|b| b & 0x80 != 0);
            match t {
                6..=9 if pending_params.is_none() => pending_params = Some(at),
                1 | 5 if first_mb_zero => {
                    starts.push(pending_params.take().unwrap_or(at));
                }
                1 | 5 => {}
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.push(data.len());
    starts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

pub(super) fn frames_digest(frames: &[Frame]) -> String {
    let mut d = Digest::new();
    for f in frames {
        d.frame(f);
    }
    d.hex()
}

#[test]
fn decode_is_bit_exact_with_ffmpeg() {
    let r = Refs::open(refs::AREA);
    for s in &refs::X264_STREAMS {
        let data = r.bytes(&format!("{}.h264", s.name));
        for threads in [1, 4] {
            let what = format!("{} with {threads} threads", s.name);
            let frames = decode_annexb(&data, threads);
            assert_eq!(frames.len() as u64, s.frames, "{what}: frame count");
            assert!(
                frames.iter().all(|f| (f.width, f.height) == s.size),
                "{what}: a frame is not {}x{}",
                s.size.0,
                s.size.1
            );
            r.assert_digest(&format!("{}.yuv", s.name), &frames_digest(&frames));
        }
    }
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(16)
}

pub(super) fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let se: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| (x as f64 - y as f64).powi(2))
        .sum();
    let mse = se / a.len() as f64;
    if mse == 0.0 {
        99.0
    } else {
        10.0 * (255.0 * 255.0 / mse).log10()
    }
}

#[test]
fn crf_18_encode_plays_in_ffmpeg_and_decodes_back() {
    let r = Refs::open(refs::AREA);
    let (w, h, _) = refs::CRF18;
    let frames = refs::crf18_source();
    let config = refs::crf18_config(0);
    let (avcc, packets) = refs::encode(config.clone(), &frames).unwrap();
    assert_eq!(packets.len(), frames.len());
    let (_, single) = refs::encode(refs::crf18_config(1), &frames).unwrap();
    assert!(
        packets
            .iter()
            .map(|p| &p.data)
            .eq(single.iter().map(|p| &p.data)),
        "frame threads change the bitstream"
    );
    assert!(packets[0].keyframe);
    assert!(
        packets.windows(2).all(|p| p[0].dts < p[1].dts),
        "dts strictly increasing"
    );
    assert!(
        packets.iter().all(|p| p.dts <= p.pts),
        "dts never after pts"
    );
    let mut pts: Vec<i64> = packets.iter().map(|p| p.pts).collect();
    pts.sort();
    assert_eq!(pts, (0..frames.len() as i64).collect::<Vec<_>>());

    let stream = refs::to_annexb(&avcc, &packets).unwrap();
    assert!(refs::crf18().unwrap() == stream, "crf18() is not this encode");
    r.assert_digest("crf18.h264", &crate::testref::sha256(&stream));
    let probed = String::from_utf8(r.bytes("crf18.probe")).unwrap();
    assert_eq!(probed.trim(), format!("h264,{w},{h},{}", frames.len()));

    let mut dec = Decoder::with_extradata(&avcc).unwrap();
    let mut ours = Vec::new();
    for p in &packets {
        ours.extend(dec.decode(p).unwrap());
    }
    ours.extend(dec.flush().unwrap());
    assert_eq!(ours.len(), frames.len());
    r.assert_digest("crf18.yuv", &frames_digest(&ours));
    let db = psnr(&raw(&ours), &raw(&frames));
    assert!(db > 40.0, "PSNR {db:.2} dB at CRF 18");
    assert_eq!(
        frames_digest(&decode_annexb(&stream, 4)),
        frames_digest(&ours),
        "the Annex B stream decodes like the avcC one"
    );
}

fn bench_stream(w: u32, h: u32, frames: u64) -> Vec<u8> {
    let config = EncoderConfig {
        width: w,
        height: h,
        fps: (60, 1),
        rate: crate::media::Rate::Crf(20),
        ..EncoderConfig::default()
    };
    let (avcc, packets) = refs::encode(config, &refs::source(w, h, frames, 3)).unwrap();
    refs::to_annexb(&avcc, &packets).unwrap()
}

fn bench_decode(data: &[u8], label: &str) {
    let aus = access_units(data);
    let packets: Vec<Packet> = aus
        .iter()
        .enumerate()
        .map(|(i, a)| Packet {
            data: a.to_vec(),
            pts: i as i64,
            dts: i as i64,
            keyframe: false,
        })
        .collect();
    let threads: Vec<usize> = match std::env::var("BENCH_THREADS") {
        Ok(v) => v.split(',').map(|t| t.parse().unwrap()).collect(),
        Err(_) => vec![1, default_threads()],
    };
    for threads in threads {
        let mut dec = Decoder::with_threads(threads);
        let t = Instant::now();
        let mut n = 0;
        for p in &packets {
            n += dec.decode(p).unwrap().len();
        }
        n += dec.flush().unwrap().len();
        let s = t.elapsed().as_secs_f64();
        println!(
            "decode {label} threads {threads}: {n} frames in {s:.2} s, {:.1} fps",
            n as f64 / s
        );
    }
}

#[test]
#[ignore]
fn bench_decoder() {
    if let Ok(file) = std::env::var("BENCH_FILE") {
        bench_decode(&std::fs::read(&file).unwrap(), &file);
        return;
    }
    let only = std::env::var("BENCH_SIZE").unwrap_or_default();
    if only != "1440" {
        bench_decode(&bench_stream(1920, 1080, 120), "1080p");
    }
    if only != "1080" {
        bench_decode(&bench_stream(2560, 1440, 120), "1440p");
    }
}

#[test]
#[ignore]
fn bench_encoder() {
    let frames = refs::source(1920, 1080, 120, 3);
    for (preset, b) in [(Preset::Speed, 2), (Preset::Balanced, 2)] {
        let config = EncoderConfig {
            width: 1920,
            height: 1080,
            fps: (60, 1),
            preset,
            rate: crate::media::Rate::Crf(18),
            b_frames: b,
            ..EncoderConfig::default()
        };
        let mut enc = Encoder::new(config.clone()).unwrap();
        let t = Instant::now();
        let mut bytes = 0;
        for f in &frames {
            bytes += enc
                .encode(f)
                .unwrap()
                .iter()
                .map(|p| p.data.len())
                .sum::<usize>();
        }
        bytes += enc
            .flush()
            .unwrap()
            .iter()
            .map(|p| p.data.len())
            .sum::<usize>();
        let s = t.elapsed().as_secs_f64();
        println!(
            "encode 1080p CRF 18 {preset:?}: {} frames in {s:.2} s, {:.1} fps, {:.1} Mbit/s",
            frames.len(),
            frames.len() as f64 / s,
            bytes as f64 * 8.0 * 60.0 / frames.len() as f64 / 1e6
        );
    }
}

#[test]
fn corrupt_streams_never_panic() {
    let r = Refs::open(refs::AREA);
    let streams: Vec<Vec<u8>> = refs::FUZZ
        .iter()
        .map(|name| r.bytes(&format!("{name}.h264")))
        .collect();
    let me = std::thread::current().id();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let t = std::thread::current();
        if t.id() == me || t.name().is_some_and(|n| n.starts_with("h264-")) {
            if let Ok(mut l) = log.lock() {
                l.push(info.to_string());
            }
        } else {
            prev(info);
        }
    }));
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as usize % n.max(1)
    };
    for data in &streams {
        for round in 0..300 {
            let mut d = data.clone();
            match round % 4 {
                0 => {
                    for _ in 0..1 + next(8) {
                        let i = 4 + next(d.len() - 4);
                        d[i] ^= 1 << next(8);
                    }
                }
                1 => d.truncate(4 + next(d.len() - 4)),
                2 => {
                    let i = 4 + next(d.len() - 4);
                    let n = next(64).min(d.len() - i);
                    d[i..i + n].fill(next(256) as u8);
                }
                _ => {
                    let i = 4 + next(d.len() - 4);
                    let n = next(400).min(d.len() - i);
                    d.drain(i..i + n);
                }
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut dec = Decoder::with_threads(1 + round % 2 * 3);
                for (i, au) in access_units(&d).into_iter().enumerate() {
                    let _ = dec.decode(&Packet {
                        data: au.to_vec(),
                        pts: i as i64,
                        dts: i as i64,
                        keyframe: false,
                    });
                }
                let _ = dec.flush();
            }));
        }
    }
    drop(std::panic::take_hook());
    let seen = seen.lock().unwrap();
    let mut kinds: Vec<&str> = seen.iter().map(|s| s.as_str()).collect();
    kinds.sort();
    kinds.dedup();
    assert!(
        seen.is_empty(),
        "{} panics:\n{}",
        seen.len(),
        kinds.join("\n")
    );
}

#[test]
fn tiny_and_odd_sizes_encode_without_crashing() {
    for (w, h) in [(2, 2), (18, 34), (34, 18), (66, 2), (2, 66)] {
        for threads in [1, 4] {
            let mut enc = Encoder::new(EncoderConfig {
                width: w,
                height: h,
                gop: 4,
                b_frames: 2,
                threads,
                ..EncoderConfig::default()
            })
            .unwrap();
            let mut n = 0;
            for i in 0..7i64 {
                let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, i, TimeBase::new(1, 30));
                for (k, p) in f.planes.iter_mut().enumerate() {
                    for (j, b) in p.data.iter_mut().enumerate() {
                        *b = ((j as i64 * 7 + i * 13 + k as i64 * 50) % 251) as u8;
                    }
                }
                n += enc.encode(&f).unwrap().len();
            }
            n += enc.flush().unwrap().len();
            assert_eq!(n, 7, "{w}x{h} with {threads} threads");
        }
    }
    for (w, h) in [(3, 2), (2, 3), (0, 16), (16, 0), (16386, 16)] {
        assert!(
            Encoder::new(EncoderConfig {
                width: w,
                height: h,
                ..EncoderConfig::default()
            })
            .is_err()
        );
    }
}

fn crop_sps(sps: &[u8], crop: (u32, u32, u32, u32)) -> Vec<u8> {
    use crate::syntax::bits::{BitReader, BitWriter, escape_rbsp, unescape_rbsp};
    let rbsp = unescape_rbsp(&sps[1..]);
    let end = rbsp
        .iter()
        .rposition(|&b| b != 0)
        .map(|i| i * 8 + 7 - rbsp[i].trailing_zeros() as usize)
        .unwrap();
    let mut r = BitReader::new(&rbsp);
    let mut w = BitWriter::new();
    assert_eq!(rbsp[0], 66);
    w.write_bits(r.read_bits(24).unwrap(), 24);
    for _ in 0..5 {
        w.write_ue(r.read_ue().unwrap());
    }
    w.write_bit(r.read_flag().unwrap());
    w.write_ue(r.read_ue().unwrap());
    w.write_ue(r.read_ue().unwrap());
    assert!(r.read_flag().unwrap());
    w.write_bit(true);
    w.write_bit(r.read_flag().unwrap());
    if r.read_flag().unwrap() {
        for _ in 0..4 {
            r.read_ue().unwrap();
        }
    }
    w.write_bit(true);
    for v in [crop.0, crop.1, crop.2, crop.3] {
        w.write_ue(v);
    }
    while r.position() < end {
        w.write_bit(r.read_bit().unwrap());
    }
    w.rbsp_trailing();
    let mut out = vec![sps[0]];
    out.extend(escape_rbsp(&w.finish()));
    out
}

pub(crate) fn far_cropped_stream() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let (w, h) = (64, 64);
    let mut enc = Encoder::new(EncoderConfig {
        width: w,
        height: h,
        profile: Profile::Baseline,
        gop: 1,
        b_frames: 0,
        threads: 1,
        bitstream: media::Bitstream::AnnexB,
        ..EncoderConfig::default()
    })
    .unwrap();
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, 0, TimeBase::new(1, 30));
    for p in f.planes.iter_mut() {
        p.data.fill(128);
    }
    let mut packets = enc.encode(&f).unwrap();
    packets.extend(enc.flush().unwrap());
    let sps = crop_sps(&enc.sps(), (0, 0, 0, 0));
    let crafted = crop_sps(&enc.sps(), (2_147_483_649, 2_147_483_647, 0, 0));
    let pps = enc.pps();
    let build = |sps: &[u8]| {
        let mut out = Vec::new();
        for nal in [sps, &pps[..]] {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        for p in &packets {
            for nal in crate::syntax::bits::annexb_nals(&p.data) {
                if !matches!(nal.first().map(|b| b & 0x1f), Some(7 | 8)) {
                    out.extend_from_slice(&[0, 0, 0, 1]);
                    out.extend_from_slice(nal);
                }
            }
        }
        out
    };
    (build(&sps), build(&crafted), crafted, pps)
}

#[test]
fn crop_offsets_past_the_picture_are_refused() {
    let (plain, crafted, _, _) = far_cropped_stream();
    for threads in [1, 4] {
        let mut dec = Decoder::with_threads(threads);
        let mut n = dec
            .decode(&Packet {
                data: plain.clone(),
                pts: 0,
                dts: 0,
                keyframe: true,
            })
            .unwrap()
            .len();
        n += dec.flush().unwrap().len();
        assert_eq!(n, 1);
        let mut dec = Decoder::with_threads(threads);
        let got = dec.decode(&Packet {
            data: crafted.clone(),
            pts: 0,
            dts: 0,
            keyframe: true,
        });
        assert!(got.is_err(), "{threads} threads");
        assert!(dec.flush().unwrap_or_default().is_empty());
    }
}

#[test]
fn sps_vui_carries_the_given_colour_tags() {
    use crate::syntax::bits::unescape_rbsp;
    use crate::syntax::h264::params::Sps;
    let vui = |color: Option<crate::media::color::Color>| {
        let mut config = EncoderConfig {
            width: 64,
            height: 48,
            ..EncoderConfig::default()
        };
        if let Some(c) = color {
            config.color = c;
        }
        let sps = Encoder::new(config).unwrap().sps();
        Sps::parse(&unescape_rbsp(&sps[1..])).unwrap().vui.unwrap()
    };
    let tags = |v: &crate::syntax::h264::params::Vui| {
        (
            v.colour_primaries,
            v.transfer_characteristics,
            v.matrix_coefficients,
            v.full_range,
        )
    };
    assert_eq!(tags(&vui(None)), (1, 1, 1, false));
    for c in [
        crate::media::color::Color {
            primaries: 1,
            transfer: 1,
            matrix: 6,
            full_range: false,
        },
        crate::media::color::Color {
            primaries: 9,
            transfer: 16,
            matrix: 9,
            full_range: true,
        },
    ] {
        let v = vui(Some(c));
        assert_eq!(tags(&v), (c.primaries, c.transfer, c.matrix, c.full_range));
        assert_eq!(v.video_format, 5);
        assert!(v.timing.is_some() && v.bitstream_restriction.is_some());
    }
}

#[test]
fn encoded_frames_carry_the_colour_tags_back_out() {
    let r = Refs::open(refs::AREA);
    for (n, (color, names)) in refs::COLOURS.into_iter().enumerate() {
        let (avcc, packets) = refs::encode(refs::colour_config(n), &refs::colour_frames()).unwrap();
        assert_eq!(packets.len(), 6);
        let stream = refs::to_annexb(&avcc, &packets).unwrap();
        assert!(refs::colour(n).unwrap() == stream, "colour({n}) is not this encode");
        let starts: Vec<usize> = stream
            .windows(3)
            .enumerate()
            .filter(|(_, w)| *w == [0, 0, 1])
            .map(|(i, _)| i + 3)
            .collect();
        let sps = starts
            .iter()
            .copied()
            .find(|&at| stream[at] & 0x1f == 7)
            .unwrap();
        let end = starts
            .iter()
            .copied()
            .find(|&at| at > sps)
            .map_or(stream.len(), |at| at - 3);
        let vui = crate::syntax::h264::params::Sps::parse(&crate::syntax::bits::unescape_rbsp(
            &stream[sps + 1..end],
        ))
        .unwrap()
        .vui
        .unwrap();
        assert_eq!(
            (
                vui.colour_primaries,
                vui.transfer_characteristics,
                vui.matrix_coefficients,
                vui.full_range
            ),
            (
                color.primaries,
                color.transfer,
                color.matrix,
                color.full_range
            )
        );
        let mut dec = Decoder::with_extradata(&avcc).unwrap();
        let mut shown = 0;
        for p in &packets {
            shown += dec.decode(p).unwrap().len();
        }
        shown += dec.flush().unwrap().len();
        assert_eq!(shown, 6);
        assert_eq!(
            dec.color(),
            Some(ColorInfo {
                full_range: color.full_range,
                primaries: color.primaries,
                transfer: color.transfer,
                matrix: color.matrix,
            })
        );
        r.assert_digest(
            &format!("colour-{n}.h264"),
            &crate::testref::sha256(&stream),
        );
        let text = String::from_utf8(r.bytes(&format!("colour-{n}.probe"))).unwrap();
        let tag = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or("")
                .to_string()
        };
        assert_eq!(
            refs::COLOUR_KEYS.map(tag),
            names.map(String::from),
            "{text}"
        );
    }
}

#[test]
fn the_x264_sources_are_what_the_recorder_feeds_x264() {
    for s in &refs::X264_STREAMS {
        let (w, h) = (s.size.0 as usize, s.size.1 as usize);
        let bytes = refs::x264_source(s.name).unwrap();
        let frame = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
        assert_eq!(bytes.len(), frame * s.frames as usize, "{}", s.name);
        assert_eq!(refs::x264(s.name).unwrap().name, s.name);
        assert!(refs::FUZZ.iter().all(|n| refs::x264(n).is_ok()));
    }
    assert!(refs::x264("nothing").is_err());
    assert!(refs::x264_source("nothing").is_err());
    assert!(refs::to_annexb(&[1, 0x64, 0, 0x1f, 0xff, 0xe1], &[]).is_err());
}
