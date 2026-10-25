use std::path::PathBuf;

use crate::opus::ogg::{OggOpus, Writer};
use crate::opus::{Mode, Packet};
use crate::testref::Refs;

#[path = "refs.rs"]
pub(crate) mod refs;

fn snr_db(reference: &[f32], ours: &[f32]) -> f64 {
    let (mut s, mut n) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(ours) {
        s += (*a as f64).powi(2);
        n += (*a as f64 - *b as f64).powi(2);
    }
    10.0 * (s / n.max(1e-20)).log10()
}

#[test]
fn decode_matches_libopus() {
    let r = Refs::open(refs::AREA);
    for (name, _, _, celt) in refs::LIBOPUS {
        let file = refs::stream(name);
        let file = OggOpus::parse(&r.bytes(&file)).unwrap();
        let modes: Vec<Mode> = (0..file.packets.len())
            .filter_map(|i| Packet::parse(file.packet(i)).ok().map(|p| p.toc.mode))
            .collect();
        assert_eq!(modes.iter().all(|m| *m == Mode::CeltOnly), celt, "{name}");
        let ours = file.decode().unwrap();
        let channels = file.head.channels;
        let mut ours = ours.samples;
        if channels == 6 {
            for f in ours.chunks_exact_mut(6) {
                let v = [f[0], f[2], f[1], f[5], f[3], f[4]];
                f.copy_from_slice(&v);
            }
        }
        let ours = refs::window::window(&ours, channels);
        let reference = r.f32s(&refs::decoded(&refs::stream(name)));
        assert_eq!(ours.len(), reference.len(), "{name}: length");
        let snr = snr_db(&reference, &ours);
        assert!(snr >= if celt { 50.0 } else { 10.0 }, "{name}: {snr:.1} dB");
    }
}

#[test]
fn ogg_rewrite_decodes_the_same() {
    let r = Refs::open(refs::AREA);
    let file = OggOpus::parse(&r.bytes(&refs::stream("music_32k_hybrid"))).unwrap();
    let mut w = Writer::new(Vec::new(), 7, &file.head, "pito-codecs").unwrap();
    let mut granule = 0i64;
    let end = file.end_granule().unwrap();
    for i in 0..file.packets.len() {
        granule += crate::opus::packet::packet_samples_48k(file.packet(i)).unwrap() as i64;
        w.push(file.packet(i), granule.min(end)).unwrap();
    }
    let again = OggOpus::parse(&w.finish().unwrap()).unwrap();
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    assert_eq!(again.samples(), file.samples());
    assert_eq!(again.packets.len(), file.packets.len());
    for i in 0..file.packets.len() {
        assert_eq!(again.packet(i), file.packet(i), "packet {i}");
    }
    let (a, b) = (again.decode().unwrap(), file.decode().unwrap());
    assert!(!a.samples.is_empty());
    assert!(a.samples == b.samples);
}


fn vectors() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/vectors/opus_newvectors");
    d.join("testvector01.bit").exists().then_some(d)
}

fn read_bit(path: &PathBuf) -> Vec<(Vec<u8>, u32)> {
    let data = std::fs::read(path).unwrap();
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= data.len() {
        let len = u32::from_be_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        let rng = u32::from_be_bytes(data[p + 4..p + 8].try_into().unwrap());
        p += 8;
        if p + len > data.len() {
            break;
        }
        out.push((data[p..p + len].to_vec(), rng));
        p += len;
    }
    out
}

const NBANDS: usize = 21;
const BANDS: [usize; NBANDS + 1] = [
    0, 2, 4, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 68, 80, 96, 120, 156, 200,
];
const WIN: usize = 480;
const STEP: usize = 120;
const NFREQ: usize = 240;
const CAL: f64 = 0.144;

fn spectra(x: &[f32], channels: usize, ch: usize) -> Vec<[f64; NFREQ]> {
    use crate::opus::celt::mdct::{Cpx, Fft};
    let n = x.len() / channels;
    let frames = if n >= WIN { (n - WIN) / STEP + 1 } else { 0 };
    let w: Vec<f32> = (0..WIN)
        .map(|i| {
            (0.5 - 0.5 * (2.0 * std::f64::consts::PI * (i as f64 + 0.5) / WIN as f64).cos()) as f32
        })
        .collect();
    let fft = Fft::new(WIN);
    let mut out = Vec::with_capacity(frames);
    let mut buf = vec![Cpx::default(); WIN];
    let mut scratch = Vec::new();
    for f in 0..frames {
        for i in 0..WIN {
            buf[i] = Cpx {
                re: x[(f * STEP + i) * channels + ch] * w[i],
                im: 0.0,
            };
        }
        fft.process(&mut buf, &mut scratch);
        let mut ps = [0f64; NFREQ];
        for (k, p) in ps.iter_mut().enumerate() {
            *p = (buf[k].re as f64).powi(2) + (buf[k].im as f64).powi(2);
        }
        out.push(ps);
    }
    out
}

fn quality(reference: &[f32], test: &[f32], channels: usize) -> f64 {
    let n = reference.len().min(test.len());
    let (reference, test) = (&reference[..n], &test[..n]);
    let mut total = 0f64;
    let mut count = 0usize;
    for ch in 0..channels {
        let xr = spectra(reference, channels, ch);
        let xt = spectra(test, channels, ch);
        let nf = xr.len();
        let mut mask = vec![[0f64; NBANDS]; nf];
        for f in 0..nf {
            for b in 0..NBANDS {
                let (lo, hi) = (BANDS[b], BANDS[b + 1]);
                mask[f][b] = xr[f][lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
            }
            for b in 1..NBANDS {
                mask[f][b] += 0.1 * mask[f][b - 1];
            }
            for b in (0..NBANDS - 1).rev() {
                mask[f][b] += 0.03 * mask[f][b + 1];
            }
        }
        for f in 1..nf {
            for b in 0..NBANDS {
                mask[f][b] += 0.5 * mask[f - 1][b];
            }
        }
        for f in (0..nf.saturating_sub(1)).rev() {
            for b in 0..NBANDS {
                mask[f][b] += 0.1 * mask[f + 1][b];
            }
        }
        for f in 0..nf {
            let mut ef = 0f64;
            for b in 0..NBANDS {
                let (lo, hi) = (BANDS[b], BANDS[b + 1]);
                let m = mask[f][b] + 1e-9 * (WIN * WIN) as f64;
                let eb: f64 = (lo..hi)
                    .map(|k| (xt[f][k].sqrt() - xr[f][k].sqrt()).powi(2))
                    .sum();
                ef += eb / ((hi - lo) as f64 * m);
            }
            total += (ef / NBANDS as f64).powi(2);
            count += 1;
        }
    }
    let err = (total / count.max(1) as f64).sqrt() / CAL;
    100.0 * (1.0 - 0.5 * (1.0 + err).ln() / 1.13f64.ln())
}

#[test]
fn rfc8251_vectors() {
    let Some(dir) = vectors() else {
        eprintln!("RFC 8251 test vectors not in tmp/vectors; skipping");
        return;
    };
    let mut failures = Vec::new();
    for v in 1..=12 {
        let name = format!("testvector{v:02}");
        let packets = read_bit(&dir.join(format!("{name}.bit")));
        let mut dec = crate::opus::StreamDecoder::new(48000, 2).unwrap();
        let mut out = Vec::new();
        let mut mismatch = None;
        for (i, (pkt, rng)) in packets.iter().enumerate() {
            let p = (!pkt.is_empty()).then_some(&pkt[..]);
            if dec.decode_interleaved(p, &mut out).is_err()
                || (p.is_some() && dec.final_range() != *rng)
            {
                mismatch.get_or_insert(i);
            }
        }
        let reference: Vec<f32> = std::fs::read(dir.join(format!("{name}.dec")))
            .unwrap()
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect();
        let q = quality(&reference, &out, 2);
        let snr = snr_db(&reference, &out);
        eprintln!("{name}: final range {mismatch:?}, quality {q:.1}, {snr:.1} dB");
        if mismatch.is_some() || q < 0.0 {
            failures.push(name);
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}
