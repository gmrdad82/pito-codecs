use std::path::PathBuf;
use std::process::Command;

use crate::opus::ogg::{OggOpus, Writer};
use crate::opus::{Mode, Packet};

fn dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tmp/test-opus");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn libopus() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libopus"))
}

fn run(args: &[&str]) {
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

fn libopus_decode(path: &PathBuf) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-c:a",
            "libopus",
            "-i",
        ])
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn snr_db(reference: &[f32], ours: &[f32]) -> f64 {
    let (mut s, mut n) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(ours) {
        s += (*a as f64).powi(2);
        n += (*a as f64 - *b as f64).powi(2);
    }
    10.0 * (s / n.max(1e-20)).log10()
}

const MUSIC: &str = "aevalsrc=0.3*sin(2*PI*220*t)+0.2*sin(2*PI*277.2*t+sin(2*PI*5*t))+0.15*sin(2*PI*329.6*t)*sin(2*PI*0.5*t)+0.05*sin(2*PI*3520*t)+0.08*(random(0)-0.5)*gt(mod(t\\,0.5)\\,0.45)|0.25*sin(2*PI*330*t)+0.2*sin(2*PI*440*t+0.3*sin(2*PI*3*t))+0.05*sin(2*PI*7040*t)+0.05*(random(1)-0.5):s=48000:d=3";
const SPEECH: &str = "aevalsrc=(0.4*sin(2*PI*(120+20*sin(2*PI*3*t))*t)+0.2*sin(2*PI*(240+40*sin(2*PI*3*t))*t)+0.1*sin(2*PI*900*t)+0.05*(random(0)-0.5))*(0.6+0.4*sin(2*PI*4*t))*gt(sin(2*PI*1.3*t)\\,-0.6):s=48000:d=3";

#[test]
fn decode_matches_libopus() {
    if !libopus() {
        eprintln!("ffmpeg with libopus not found; skipping");
        return;
    }
    let cases: [(&str, &str, &[&str], bool); 4] = [
        ("music_160k", MUSIC, &["-b:a", "160k", "-vbr", "off"], true),
        (
            "speech_16k_voip",
            SPEECH,
            &["-ac", "1", "-b:a", "16k", "-application", "voip"],
            false,
        ),
        ("music_32k_hybrid", MUSIC, &["-b:a", "32k"], false),
        (
            "music_51_256k",
            MUSIC,
            &[
                "-af",
                "pan=5.1|c0=c0|c1=c1|c2=0.5*c0+0.5*c1|c3=0.1*c0|c4=0.8*c0|c5=0.8*c1",
                "-b:a",
                "256k",
                "-mapping_family",
                "1",
            ],
            true,
        ),
    ];
    for (name, source, args, celt) in cases {
        let path = dir().join(format!("{name}.opus"));
        let mut all = vec!["-f", "lavfi", "-i", source, "-c:a", "libopus"];
        all.extend_from_slice(args);
        let p = path.to_str().unwrap();
        all.push(p);
        run(&all);
        let file = OggOpus::parse(&std::fs::read(&path).unwrap()).unwrap();
        let modes: Vec<Mode> = (0..file.packets.len())
            .filter_map(|i| Packet::parse(file.packet(i)).ok().map(|p| p.toc.mode))
            .collect();
        assert_eq!(modes.iter().all(|m| *m == Mode::CeltOnly), celt, "{name}");
        let ours = file.decode().unwrap();
        let reference = libopus_decode(&path);
        let mut ours = ours.samples;
        if file.head.channels == 6 {
            for f in ours.chunks_exact_mut(6) {
                let v = [f[0], f[2], f[1], f[5], f[3], f[4]];
                f.copy_from_slice(&v);
            }
        }
        assert_eq!(ours.len(), reference.len(), "{name}: length");
        let snr = snr_db(&reference, &ours);
        eprintln!("{name}: {snr:.1} dB against libopus");
        assert!(snr >= if celt { 50.0 } else { 10.0 }, "{name}: {snr:.1} dB");
    }
}

#[test]
fn ogg_rewrite_decodes_the_same() {
    if !libopus() {
        return;
    }
    let src = dir().join("rewrap_src.opus");
    run(&[
        "-f",
        "lavfi",
        "-i",
        MUSIC,
        "-c:a",
        "libopus",
        "-b:a",
        "96k",
        src.to_str().unwrap(),
    ]);
    let file = OggOpus::parse(&std::fs::read(&src).unwrap()).unwrap();
    let mut w = Writer::new(Vec::new(), 7, &file.head, "pito-codecs").unwrap();
    let mut granule = 0i64;
    let end = file.end_granule().unwrap();
    for i in 0..file.packets.len() {
        granule += crate::opus::packet::packet_samples_48k(file.packet(i)).unwrap() as i64;
        w.push(file.packet(i), granule.min(end)).unwrap();
    }
    let dst = dir().join("rewrap_dst.opus");
    std::fs::write(&dst, w.finish().unwrap()).unwrap();
    let again = OggOpus::parse(&std::fs::read(&dst).unwrap()).unwrap();
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    assert_eq!(again.samples(), file.samples());
    assert_eq!(libopus_decode(&dst), libopus_decode(&src));
}

#[test]
#[ignore]
fn decode_speed() {
    let src = dir().join("speed.opus");
    for (name, args) in [
        ("celt 128k stereo", &["-b:a", "128k"][..]),
        (
            "silk 16k mono",
            &["-ac", "1", "-b:a", "16k", "-application", "voip"][..],
        ),
    ] {
        let mut all = vec![
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.3*sin(2*PI*220*t)+0.2*sin(2*PI*277.2*t+sin(2*PI*5*t))+0.05*(random(0)-0.5)|0.25*sin(2*PI*330*t)+0.05*(random(1)-0.5):s=48000:d=120",
            "-c:a",
            "libopus",
        ];
        all.extend_from_slice(args);
        all.push(src.to_str().unwrap());
        run(&all);
        let data = std::fs::read(&src).unwrap();
        let mut ours = f64::MAX;
        let mut out = None;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            out = Some(crate::opus::ogg::decode(&data).unwrap());
            ours = ours.min(t.elapsed().as_secs_f64());
        }
        let out = out.unwrap();
        let mut theirs = f64::MAX;
        for _ in 0..3 {
            let o = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-benchmark",
                    "-threads",
                    "1",
                    "-c:a",
                    "libopus",
                    "-i",
                ])
                .arg(&src)
                .args(["-f", "null", "-"])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&o.stderr);
            let rtime = text
                .split("rtime=")
                .nth(1)
                .and_then(|r| r.split('s').next())
                .and_then(|r| r.parse::<f64>().ok())
                .unwrap();
            theirs = theirs.min(rtime);
        }
        let secs = out.frames() as f64 / 48000.0;
        eprintln!(
            "{name}: ours {:.0}x real time, ffmpeg libopus {:.0}x",
            secs / ours,
            secs / theirs
        );
    }
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
