use super::{AlphaDecoder, AlphaEncoder, Decoder, EncoderConfig, Rate};
use crate::media::{Error, Frame, Packet, PixelFormat, Plane, TimeBase};
use crate::mkv::Reader;
use crate::testref::{Digest, Refs};
use std::path::Path;

use super::tests::refs;

type Pairs = Vec<(Frame, Option<Plane>)>;

fn ours(path: &Path) -> Pairs {
    let r = Reader::open(path).unwrap();
    let t = r.video().unwrap();
    assert!(r.tracks[t].alpha(), "the track is not marked as alpha");
    let mut dec = AlphaDecoder::with_threads(3);
    let mut out = Vec::new();
    let mut extra = Vec::new();
    for i in 0..r.tracks[t].samples.len() {
        let p = r.packet(t, i).unwrap();
        let has = r.read_alpha(t, i, &mut extra).unwrap();
        out.extend(dec.decode(&p, has.then_some(extra.as_slice())).unwrap());
    }
    out.extend(dec.flush().unwrap());
    out
}

fn yuva_digest(name: &str, decoded: &Pairs) -> String {
    let mut d = Digest::new();
    for (i, (f, a)) in decoded.iter().enumerate() {
        d.frame(f);
        let a = a
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: frame {i} lost its alpha"));
        assert_eq!((a.width as u32, a.height as u32), (f.width, f.height));
        for y in 0..a.height {
            d.bytes(&a.data[y * a.stride..y * a.stride + a.width]);
        }
    }
    d.hex()
}

#[test]
fn libvpx_alpha_planes_decode_bit_exact() {
    let r = Refs::open(refs::AREA);
    for (name, w, h) in refs::ALPHA {
        let decoded = ours(&r.path(&format!("{name}.webm")));
        assert_eq!(decoded.len() as u64, refs::ALPHA_FRAMES, "{name}");
        let pts: Vec<i64> = decoded.iter().map(|(f, _)| f.pts).collect();
        assert!(pts.windows(2).all(|p| p[0] < p[1]), "{name}: {pts:?}");
        assert!(
            decoded.iter().all(|(f, _)| (f.width, f.height) == (w, h)),
            "{name}: a frame is not {w}x{h}"
        );
        r.assert_digest(&format!("{name}.yuva"), &yuva_digest(name, &decoded));
    }
}

#[test]
fn colour_without_alpha_comes_out_bare() {
    let store = Refs::open(refs::AREA);
    let path = store.path("alpha-96x64.webm");
    let r = Reader::open(&path).unwrap();
    let t = r.video().unwrap();
    let mut dec = AlphaDecoder::new();
    let mut out = Vec::new();
    for i in 0..r.tracks[t].samples.len() {
        out.extend(dec.decode(&r.packet(t, i).unwrap(), None).unwrap());
    }
    out.extend(dec.flush().unwrap());
    assert_eq!(out.len() as u64, refs::ALPHA_FRAMES);
    assert!(out.iter().all(|(_, a)| a.is_none()));
    dec.reset();
    let mut extra = Vec::new();
    assert!(r.read_alpha(t, 0, &mut extra).unwrap());
    let first = dec.decode(&r.packet(t, 0).unwrap(), Some(&extra)).unwrap();
    let rest = dec.flush().unwrap();
    let all: Vec<_> = first.into_iter().chain(rest).collect();
    assert_eq!(all.len(), 1);
    assert!(all[0].1.is_some());
}

fn config(w: u32, h: u32) -> EncoderConfig {
    EncoderConfig {
        width: w,
        height: h,
        fps: (10, 1),
        rate: Rate::Q(8),
        gop: 3,
        threads: 1,
        aq: false,
    }
}

fn picture(w: u32, h: u32, n: i64) -> (Frame, Plane) {
    let mut f = Frame::new(PixelFormat::Yuv420p8, w, h, n, TimeBase::new(1, 10));
    for (i, v) in f.planes[0].data.iter_mut().enumerate() {
        *v = ((i % w as usize) * 3 + n as usize * 5) as u8;
    }
    f.planes[1].data.fill(100);
    f.planes[2].data.fill(150);
    let a = Plane {
        data: (0..(w * h) as usize)
            .map(|i| {
                if (i % w as usize) < w as usize / 2 {
                    255
                } else {
                    (i / w as usize * 4) as u8
                }
            })
            .collect(),
        stride: w as usize,
        width: w as usize,
        height: h as usize,
    };
    (f, a)
}

#[test]
fn our_encoder_pairs_alpha_that_decodes_back() {
    let (w, h) = (64u32, 48u32);
    let mut enc = AlphaEncoder::new(config(w, h)).unwrap();
    enc.set_color(1, true).unwrap();
    assert_eq!(enc.color(), (1, true));
    let mut packets = Vec::new();
    let mut sources = Vec::new();
    for n in 0..5 {
        let (f, a) = picture(w, h, n);
        if n % 2 == 0 {
            packets.extend(enc.encode(&f, &a).unwrap());
        } else {
            let dst = enc.alpha_frame();
            for y in 0..h as usize {
                dst.data[y * dst.stride..y * dst.stride + w as usize]
                    .copy_from_slice(&a.data[y * w as usize..(y + 1) * w as usize]);
            }
            packets.extend(enc.encode_filled(&f).unwrap());
        }
        sources.push(a);
    }
    packets.extend(enc.flush().unwrap());
    assert_eq!(packets.len(), 5);
    assert!(packets[0].0.keyframe);
    let mut dec = AlphaDecoder::with_threads(1);
    let mut plain = Decoder::with_threads(1);
    let mut got = Vec::new();
    let mut direct = Vec::new();
    for (p, a) in &packets {
        got.extend(dec.decode(p, Some(a.as_slice())).unwrap());
        let ap = Packet {
            data: a.clone(),
            pts: p.pts,
            dts: p.dts,
            keyframe: p.keyframe,
        };
        direct.extend(plain.decode(&ap).unwrap());
    }
    got.extend(dec.flush().unwrap());
    direct.extend(plain.flush().unwrap());
    assert_eq!(got.len(), 5);
    for (i, ((_, a), d)) in got.iter().zip(&direct).enumerate() {
        let a = a.as_ref().unwrap();
        assert_eq!(a.data, d.planes[0].data, "frame {i}");
        let err: u64 = a
            .data
            .iter()
            .zip(&sources[i].data)
            .map(|(x, y)| x.abs_diff(*y) as u64)
            .sum();
        assert!(
            err / a.data.len() as u64 <= 6,
            "frame {i}: mean error {}",
            err / a.data.len() as u64
        );
    }
}

#[test]
fn a_short_alpha_plane_is_refused() {
    let mut enc = AlphaEncoder::new(config(64, 48)).unwrap();
    let (f, a) = picture(64, 48, 0);
    for bad in [
        Plane {
            height: 47,
            ..a.clone()
        },
        Plane {
            width: 63,
            ..a.clone()
        },
        Plane {
            stride: 63,
            ..a.clone()
        },
        Plane {
            data: a.data[..64 * 47].to_vec(),
            ..a.clone()
        },
    ] {
        match enc.encode(&f, &bad) {
            Err(Error::Invalid(m)) => assert!(m.contains("alpha plane"), "{m}"),
            other => panic!("{other:?}"),
        }
    }
    assert!(enc.encode(&f, &a).is_ok());
}

#[test]
fn an_alpha_picture_of_another_size_is_refused() {
    let mut big = AlphaEncoder::new(config(64, 48)).unwrap();
    let mut small = AlphaEncoder::new(config(32, 32)).unwrap();
    let (f, a) = picture(64, 48, 0);
    let (fs, as_) = picture(32, 32, 0);
    let first = |enc: &mut AlphaEncoder, f: &Frame, a: &Plane| {
        let mut v = enc.encode(f, a).unwrap();
        v.extend(enc.flush().unwrap());
        v.remove(0)
    };
    let (p, _) = first(&mut big, &f, &a);
    let (_, wrong) = first(&mut small, &fs, &as_);
    let mut dec = AlphaDecoder::with_threads(2);
    let r = dec.decode(&p, Some(&wrong)).and_then(|mut v| {
        v.extend(dec.flush()?);
        Ok(v)
    });
    match r {
        Err(Error::Invalid(m)) => assert!(m.contains("alpha picture"), "{m}"),
        other => panic!("{:?}", other.map(|v| v.len())),
    }
}

type Decoded = Result<Vec<(Frame, Option<Plane>)>, Error>;
type Cut = fn(&[u8]) -> Vec<u8>;

fn paired(w: u32, h: u32, n: i64) -> Vec<(Packet, Vec<u8>)> {
    let mut enc = AlphaEncoder::new(config(w, h)).unwrap();
    let mut packets = Vec::new();
    for i in 0..n {
        let (f, a) = picture(w, h, i);
        packets.extend(enc.encode(&f, &a).unwrap());
    }
    packets.extend(enc.flush().unwrap());
    packets
}

fn run(
    packets: &[(Packet, Vec<u8>)],
    alpha: impl Fn(usize, &[u8]) -> Option<Vec<u8>>,
) -> Vec<Decoded> {
    run_on(2, packets, alpha)
}

fn run_on(
    threads: usize,
    packets: &[(Packet, Vec<u8>)],
    alpha: impl Fn(usize, &[u8]) -> Option<Vec<u8>>,
) -> Vec<Decoded> {
    let mut dec = AlphaDecoder::with_threads(threads);
    let mut out: Vec<_> = packets
        .iter()
        .enumerate()
        .map(|(i, (p, a))| dec.decode(p, alpha(i, a).as_deref()))
        .collect();
    out.push(dec.flush());
    out
}

#[test]
fn a_delta_frame_without_its_alpha_heals_at_the_next_keyframe() {
    let packets = paired(64, 48, 6);
    assert_eq!(packets.len(), 6);
    assert!(packets[0].0.keyframe && packets[3].0.keyframe);
    assert!(!packets[1].0.keyframe && !packets[2].0.keyframe);
    let clean: Vec<_> = run(&packets, |_, a| Some(a.to_vec()))
        .into_iter()
        .flat_map(|r| r.unwrap())
        .collect();
    assert_eq!(clean.len(), 6);
    let gap = run(&packets, |i, a| (i != 1).then(|| a.to_vec()));
    let frames: Vec<_> = gap
        .into_iter()
        .flat_map(|r| r.expect("a lost alpha packet is not an error"))
        .collect();
    assert_eq!(frames.len(), 6);
    for pts in [1, 2] {
        let f = frames.iter().find(|(f, _)| f.pts == pts);
        assert!(
            f.is_some_and(|(_, a)| a.is_none()),
            "frame {pts} kept an alpha decoded against a lost reference"
        );
    }
    for (f, a) in frames.iter().filter(|(f, _)| f.pts >= 3) {
        let want = clean[f.pts as usize].1.as_ref().unwrap();
        let got = a
            .as_ref()
            .unwrap_or_else(|| panic!("frame {} lost its alpha", f.pts));
        assert_eq!(got.data, want.data, "frame {} after the keyframe", f.pts);
    }
    assert_eq!(frames.iter().filter(|(f, _)| f.pts >= 3).count(), 3);
}

#[test]
fn corrupt_and_empty_alpha_payloads_never_panic() {
    let packets = paired(64, 48, 4);
    let cuts: [Cut; 4] = [
        |a| a[..a.len() / 2].to_vec(),
        |a| a.iter().map(|b| b ^ 0x5A).collect(),
        |a| a[..1.min(a.len())].to_vec(),
        |_| Vec::new(),
    ];
    for (k, cut) in cuts.iter().enumerate() {
        for target in 0..packets.len() {
            for r in run(&packets, |i, a| {
                Some(if i == target { cut(a) } else { a.to_vec() })
            }) {
                let Ok(v) = r else { continue };
                for (f, a) in v {
                    if let Some(a) = a {
                        assert_eq!(
                            (a.width, a.height),
                            (f.width as usize, f.height as usize),
                            "cut {k} at {target}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_failed_alpha_packet_never_holds_back_colour_frames() {
    let packets = paired(64, 48, 6);
    let calls = |r: &[Decoded], what: &str| -> Vec<usize> {
        let mut at = vec![usize::MAX; packets.len()];
        for (call, r) in r.iter().enumerate() {
            let v = r
                .as_ref()
                .unwrap_or_else(|e| panic!("{what}: call {call} failed: {e}"));
            for (f, _) in v {
                at[f.pts as usize] = call;
            }
        }
        at
    };
    let clean = run_on(1, &packets, |_, a| Some(a.to_vec()));
    let clean_at = calls(&clean, "clean");
    assert_eq!(clean_at, (0..packets.len()).collect::<Vec<_>>(), "clean");
    let clean_frames: Vec<_> = clean.into_iter().flat_map(|r| r.unwrap()).collect();
    let cuts: [(&str, Cut, bool); 3] = [
        ("one byte", |a| a[..1].to_vec(), true),
        ("empty", |_| Vec::new(), true),
        ("flipped", |a| a.iter().map(|b| b ^ 0x5A).collect(), false),
    ];
    for (what, cut, lost) in cuts {
        let pick = |i: usize, a: &[u8]| Some(if i == 1 { cut(a) } else { a.to_vec() });
        let got = run_on(1, &packets, pick);
        assert_eq!(calls(&got, what), clean_at, "{what}: frames held back");
        let threaded = run_on(3, &packets, pick);
        calls(&threaded, what);
        let mut frames: Vec<_> = got.into_iter().flat_map(|r| r.unwrap()).collect();
        frames.extend(threaded.into_iter().flat_map(|r| r.unwrap()));
        assert_eq!(frames.len(), 2 * packets.len(), "{what}");
        for (f, a) in &frames {
            let want = clean_frames[f.pts as usize].1.as_ref().unwrap();
            match f.pts {
                1 | 2 if lost => assert!(a.is_none(), "{what}: frame {} kept an alpha", f.pts),
                1 | 2 => {}
                _ => {
                    let a = a
                        .as_ref()
                        .unwrap_or_else(|| panic!("{what}: frame {} lost its alpha", f.pts));
                    assert_eq!(a.data, want.data, "{what}: frame {}", f.pts);
                }
            }
        }
    }
}
