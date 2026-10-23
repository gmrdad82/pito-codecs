use super::SilkEncoder;
use crate::opus::StreamDecoder;
use crate::opus::enc::Bandwidth;
use crate::opus::range::RangeEncoder;

pub(crate) fn speechlike(seconds: f32, rate: u32) -> Vec<f32> {
    let n = (seconds * rate as f32) as usize;
    let fs = rate as f32;
    let mut out = vec![0f32; n];
    let mut phase = 0f32;
    let mut noise = 12345u32;
    let formants = [
        (700f32, 130f32),
        (1220.0, 70.0),
        (2600.0, 160.0),
        (3500.0, 250.0),
    ];
    let mut res = [[0f32; 2]; 4];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / fs;
        let syl = (t * 3.1).fract();
        let voiced = syl < 0.62;
        let env = if syl < 0.7 {
            (std::f32::consts::PI * syl / 0.7).sin().powf(0.6)
        } else if syl < 0.82 {
            0.25
        } else {
            0.0
        };
        let f0 = 110.0 + 45.0 * (t * 1.7).sin() + 20.0 * (t * 5.3).sin();
        phase += f0 / fs;
        let mut src = 0f32;
        if phase >= 1.0 {
            phase -= 1.0;
            if voiced {
                src = 1.0;
            }
        }
        noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let nz = (noise >> 8) as f32 / 16_777_216.0 - 0.5;
        if !voiced {
            src = nz * 0.6;
        } else {
            src += nz * 0.02;
        }
        let shift = 1.0 + 0.15 * (t * 2.3).sin();
        let mut y = 0f32;
        for (k, &(f, bw)) in formants.iter().enumerate() {
            let r = (-std::f32::consts::PI * bw / fs).exp();
            let c = 2.0 * r * (2.0 * std::f32::consts::PI * f * shift / fs).cos();
            let v = src + c * res[k][0] - r * r * res[k][1];
            res[k][1] = res[k][0];
            res[k][0] = v;
            y += v * (1.0 - r) * if k == 0 { 1.0 } else { 0.6 };
        }
        *o = y * env * 0.5;
    }
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    for v in out.iter_mut() {
        *v *= 0.5 / peak;
    }
    out
}

pub(crate) fn encode_packets(
    pcm: &[f32],
    channels: usize,
    bw: Bandwidth,
    frame_ms: usize,
    bitrate: u32,
) -> Vec<Vec<u8>> {
    let mut enc = SilkEncoder::new(channels);
    let n = 48 * frame_ms;
    let config = match bw {
        Bandwidth::Narrowband => 0,
        Bandwidth::Mediumband => 4,
        _ => 8,
    } + match frame_ms {
        10 => 0,
        20 => 1,
        40 => 2,
        _ => 3,
    };
    let toc = (config << 3) as u8 | if channels == 2 { 4 } else { 0 };
    let bits = bitrate * frame_ms as u32 / 1000 - 8;
    let mut rc = RangeEncoder::new(1275);
    let mut out = Vec::new();
    for chunk in pcm.chunks_exact(n * channels) {
        rc.reset(1275);
        enc.encode(&mut rc, chunk, bw, bits);
        let nbytes = ((rc.tell() + 7) >> 3) as usize;
        let buf = rc.finish();
        let mut p = Vec::with_capacity(nbytes + 1);
        p.push(toc);
        p.extend_from_slice(&buf[..nbytes]);
        out.push(p);
    }
    out
}

pub(crate) fn decode_ours(packets: &[Vec<u8>], channels: usize) -> Vec<f32> {
    let mut dec = StreamDecoder::new(48000, channels).unwrap();
    let mut out = Vec::new();
    for p in packets {
        dec.decode_interleaved(Some(p), &mut out).unwrap();
    }
    out
}

pub(crate) fn snr_aligned(reference: &[f32], decoded: &[f32], max_lag: usize) -> (f32, usize) {
    let n = reference
        .len()
        .min(decoded.len())
        .saturating_sub(max_lag + 1);
    let mut best = (f32::MIN, 0);
    for lag in 0..max_lag {
        let mut sig = 0f64;
        let mut err = 0f64;
        for i in 0..n {
            let r = reference[i] as f64;
            let d = decoded[i + lag] as f64;
            sig += r * r;
            err += (r - d) * (r - d);
        }
        let snr = (10.0 * (sig / err.max(1e-12)).log10()) as f32;
        if snr > best.0 {
            best = (snr, lag);
        }
    }
    best
}

#[test]
fn speech_round_trips_through_our_decoder() {
    let pcm = speechlike(2.0, 48000);
    for (bitrate, min_snr) in [(16000, 6.0), (32000, 9.0)] {
        let packets = encode_packets(&pcm, 1, Bandwidth::Wideband, 20, bitrate);
        let bytes: usize = packets.iter().map(|p| p.len()).sum();
        let kbps = bytes as f32 * 8.0 / 2.0 / 1000.0;
        let dec = decode_ours(&packets, 1);
        let (snr, lag) = snr_aligned(&pcm, &dec, 400);
        eprintln!("{bitrate}: {kbps:.1} kbps, snr {snr:.2} dB at lag {lag}");
        assert!(snr > min_snr, "{bitrate}: snr {snr}");
        assert!(kbps < bitrate as f32 / 1000.0 * 1.25, "{kbps} kbps");
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn ogg_page(out: &mut Vec<u8>, flags: u8, granule: u64, seq: u32, packet: &[u8]) {
    let mut page = b"OggS".to_vec();
    page.push(0);
    page.push(flags);
    page.extend(granule.to_le_bytes());
    page.extend(0x5049_544fu32.to_le_bytes());
    page.extend(seq.to_le_bytes());
    page.extend([0; 4]);
    let mut lacing = Vec::new();
    let mut n = packet.len();
    while n >= 255 {
        lacing.push(255u8);
        n -= 255;
    }
    lacing.push(n as u8);
    page.push(lacing.len() as u8);
    page.extend(&lacing);
    page.extend(packet);
    let c = crc32(&page);
    page[22..26].copy_from_slice(&c.to_le_bytes());
    out.extend(page);
}

pub(crate) fn ogg_opus(packets: &[Vec<u8>], channels: usize, frame_ms: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut head = b"OpusHead".to_vec();
    head.extend([1, channels as u8, 0, 0]);
    head.extend(48000u32.to_le_bytes());
    head.extend([0, 0, 0]);
    ogg_page(&mut out, 2, 0, 0, &head);
    let mut tags = b"OpusTags".to_vec();
    tags.extend(4u32.to_le_bytes());
    tags.extend(b"pito");
    tags.extend(0u32.to_le_bytes());
    ogg_page(&mut out, 0, 0, 1, &tags);
    let mut granule = 0u64;
    for (i, p) in packets.iter().enumerate() {
        granule += 48 * frame_ms as u64;
        let flags = if i + 1 == packets.len() { 4 } else { 0 };
        ogg_page(&mut out, flags, granule, i as u32 + 2, p);
    }
    out
}

fn read_f32(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn ffmpeg(args: &[&str]) {
    let st = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

fn seg_snr(reference: &[f32], decoded: &[f32], lag: usize, channels: usize) -> f32 {
    let n = reference.len().min(decoded.len().saturating_sub(lag));
    let seg = 960 * channels;
    let mut sum = 0f64;
    let mut cnt = 0;
    for s in (0..n.saturating_sub(seg)).step_by(seg) {
        let mut sig = 0f64;
        let mut err = 0f64;
        for i in s..s + seg {
            let r = reference[i] as f64;
            sig += r * r;
            err += (r - decoded[i + lag] as f64).powi(2);
        }
        if sig / seg as f64 > 1e-6 {
            sum += (10.0 * (sig / err.max(1e-12)).log10()).clamp(-10.0, 35.0);
            cnt += 1;
        }
    }
    (sum / cnt.max(1) as f64) as f32
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        for s in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (sn, cs) = (ang * k as f32).sin_cos();
                let a = s + k;
                let b = a + len / 2;
                let tr = re[b] * cs - im[b] * sn;
                let ti = re[b] * sn + im[b] * cs;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

thread_local! {
    static BANDS: std::cell::RefCell<[(f64, f64); 26]> = const { std::cell::RefCell::new([(0.0, 0.0); 26]) };
}

pub(crate) fn dump_bands(tag: &str) {
    BANDS.with(|b| {
        let mut b = b.borrow_mut();
        let v: Vec<String> = b
            .iter()
            .take(22)
            .map(|(s, n)| format!("{:.1}", s / n.max(1.0)))
            .collect();
        eprintln!("{tag} bands {}", v.join(" "));
        *b = [(0.0, 0.0); 26];
    });
}

pub(crate) fn fw_snr_seg(reference: &[f32], decoded: &[f32], lag: usize, top_hz: f32) -> f32 {
    const N: usize = 2048;
    const HOP: usize = 960;
    let edges = [
        0f32, 100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0, 1080.0, 1270.0, 1480.0,
        1720.0, 2000.0, 2320.0, 2700.0, 3150.0, 3700.0, 4400.0, 5300.0, 6400.0, 7700.0, 9500.0,
        12000.0, 15500.0, 20000.0,
    ];
    let n = reference.len().min(decoded.len().saturating_sub(lag));
    let mut total = 0f64;
    let mut frames = 0;
    let mut a = vec![0f32; N];
    let mut ai = vec![0f32; N];
    let mut b = vec![0f32; N];
    let mut bi = vec![0f32; N];
    let mut s = 0;
    while s + N <= n {
        let mut energy = 0f32;
        for i in 0..N {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos();
            a[i] = reference[s + i] * w;
            b[i] = decoded[s + i + lag] * w;
            ai[i] = 0.0;
            bi[i] = 0.0;
            energy += reference[s + i] * reference[s + i];
        }
        s += HOP;
        if energy / (N as f32) < 1e-5 {
            continue;
        }
        fft(&mut a, &mut ai);
        fft(&mut b, &mut bi);
        let mut num = 0f64;
        let mut den = 0f64;
        for w in edges.windows(2) {
            if w[0] >= top_hz {
                break;
            }
            let lo = (w[0] / 48000.0 * N as f32) as usize;
            let hi = ((w[1].min(top_hz)) / 48000.0 * N as f32) as usize;
            let mut x = 0f64;
            let mut e = 0f64;
            for k in lo.max(1)..hi {
                let mx = ((a[k] * a[k] + ai[k] * ai[k]) as f64).sqrt();
                let my = ((b[k] * b[k] + bi[k] * bi[k]) as f64).sqrt();
                x += mx * mx;
                e += (mx - my) * (mx - my);
            }
            let snr = (10.0 * (x / e.max(1e-12)).log10()).clamp(-10.0, 35.0);
            if std::env::var("FW_BANDS").is_ok() {
                BANDS.with(|b| {
                    let mut b = b.borrow_mut();
                    let i = edges.iter().position(|v| *v == w[0]).unwrap();
                    b[i].0 += snr;
                    b[i].1 += 1.0;
                });
            }
            let wt = x.powf(0.1);
            num += wt * snr;
            den += wt;
        }
        total += num / den.max(1e-12);
        frames += 1;
    }
    (total / frames.max(1) as f64) as f32
}

#[test]
#[ignore]
fn eval_against_libopus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/eval");
    std::fs::create_dir_all(&root).unwrap();
    let src =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/fx/speech/clips/resilience.mp4");
    let raw = root.join("speech.f32");
    crate::fixture::once(&raw, |part| {
        ffmpeg(&[
            "-ss",
            "5",
            "-t",
            "20",
            "-i",
            src.to_str().unwrap(),
            "-ac",
            "1",
            "-ar",
            "48000",
            "-f",
            "f32le",
            part.to_str().unwrap(),
        ]);
    });
    let pcm = read_f32(&raw);
    for br in std::env::var("EVAL_RATES")
        .map(|s| {
            s.split(',')
                .map(|v| v.parse().unwrap())
                .collect::<Vec<u32>>()
        })
        .unwrap_or(vec![12000, 16000, 24000, 32000])
    {
        let t = std::time::Instant::now();
        let packets = encode_packets(&pcm, 1, Bandwidth::Wideband, 20, br);
        let secs = t.elapsed().as_secs_f32();
        let bytes: usize = packets.iter().map(|p| p.len()).sum();
        let kbps = bytes as f32 * 8.0 / (pcm.len() as f32 / 48000.0) / 1000.0;
        let ours = root.join(format!("ours_{br}.opus"));
        std::fs::write(&ours, ogg_opus(&packets, 1, 20)).unwrap();
        let dec = root.join(format!("ours_{br}.f32"));
        ffmpeg(&[
            "-c:a",
            "libopus",
            "-i",
            ours.to_str().unwrap(),
            "-f",
            "f32le",
            "-ac",
            "1",
            "-ar",
            "48000",
            dec.to_str().unwrap(),
        ]);
        let d = read_f32(&dec);
        let (snr, lag) = snr_aligned(&pcm, &d, 600);
        let seg = seg_snr(&pcm, &d, lag, 1);
        let fw = fw_snr_seg(&pcm, &d, lag, 8000.0);
        let lib = root.join(format!("lib_{br}.opus"));
        ffmpeg(&[
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            "1",
            "-i",
            raw.to_str().unwrap(),
            "-c:a",
            "libopus",
            "-b:a",
            &br.to_string(),
            "-application",
            "voip",
            "-frame_duration",
            "20",
            lib.to_str().unwrap(),
        ]);
        let libdec = root.join(format!("lib_{br}.f32"));
        ffmpeg(&[
            "-c:a",
            "libopus",
            "-i",
            lib.to_str().unwrap(),
            "-f",
            "f32le",
            "-ac",
            "1",
            "-ar",
            "48000",
            libdec.to_str().unwrap(),
        ]);
        let ld = read_f32(&libdec);
        let lib_bytes = std::fs::metadata(&lib).unwrap().len();
        let (lsnr, llag) = snr_aligned(&pcm, &ld, 600);
        dump_bands("ours");
        let lseg = seg_snr(&pcm, &ld, llag, 1);
        let lfw = fw_snr_seg(&pcm, &ld, llag, 8000.0);
        dump_bands("libo");
        eprintln!(
            "{br}: ours {kbps:.1} kbps snr {snr:.2} seg {seg:.2} fw {fw:.2} lag {lag} ({:.0}x realtime) | libopus ~{:.1} kbps snr {lsnr:.2} seg {lseg:.2} fw {lfw:.2} lag {llag}",
            20.0 / secs,
            lib_bytes as f32 * 8.0 / 20.0 / 1000.0
        );
    }
}

#[test]
#[ignore]
fn debug_speech() {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/eval/speech.f32");
    let pcm = read_f32(&raw);
    let mut enc = SilkEncoder::new(1);
    let mut rc = RangeEncoder::new(1275);
    for chunk in pcm.chunks_exact(960).take(300) {
        rc.reset(1275);
        enc.encode(&mut rc, chunk, Bandwidth::Wideband, 312);
        enc.types.push(enc.frames[0].ind.signal_type);
    }
    let log = &enc.ch.log;
    for (i, w) in log.chunks(320).enumerate() {
        let s: f64 = w.iter().map(|v| (v.0 as f64).powi(2)).sum();
        let y: f64 = w.iter().map(|v| (v.1 as f64).powi(2)).sum();
        let e: f64 = w.iter().map(|v| (v.0 as f64 - v.1 as f64).powi(2)).sum();
        eprintln!(
            "frame {i}: type {} rms {:.0} out {:.0} snr {:.1}",
            enc.types[i],
            (s / 320.0).sqrt(),
            (y / 320.0).sqrt(),
            10.0 * (s / e.max(1.0)).log10()
        );
    }
}

pub(crate) fn ogg_packets(data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut p = 0;
    while p + 27 <= data.len() {
        let nseg = data[p + 26] as usize;
        let lace = &data[p + 27..p + 27 + nseg];
        let mut q = p + 27 + nseg;
        for &l in lace {
            cur.extend_from_slice(&data[q..q + l as usize]);
            q += l as usize;
            if l < 255 {
                out.push(std::mem::take(&mut cur));
            }
        }
        p = q;
    }
    out.drain(..2.min(out.len()));
    out
}

#[test]
#[ignore]
fn inspect_libopus() {
    let name = std::env::var("INSPECT").unwrap_or("lib_16000.opus".into());
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tmp/eval")
        .join(name);
    let packets = ogg_packets(&std::fs::read(path).unwrap());
    let mut dec = StreamDecoder::new(48000, 1).unwrap();
    let mut out = Vec::new();
    for p in packets.iter().take(300) {
        eprintln!("packet toc {} len {}", p[0] >> 3, p.len());
        dec.decode_interleaved(Some(p), &mut out).unwrap();
    }
}

#[test]
#[ignore]
fn decode_with_ours() {
    let name = std::env::var("INSPECT").unwrap_or("ours_16000.opus".into());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/eval");
    let packets = ogg_packets(&std::fs::read(root.join(&name)).unwrap());
    let pcm = decode_ours(&packets, 1);
    let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(root.join(format!("{name}.ourdec.f32")), bytes).unwrap();
}

#[test]
#[ignore]
fn error_by_position() {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/eval/speech.f32");
    let pcm = read_f32(&raw);
    let mut enc = SilkEncoder::new(1);
    let mut rc = RangeEncoder::new(1275);
    for chunk in pcm.chunks_exact(960).take(600) {
        rc.reset(1275);
        enc.encode(&mut rc, chunk, Bandwidth::Wideband, 312);
    }
    let log = &enc.ch.log;
    let mut pos = [0f64; 20];
    let mut sig = [0f64; 20];
    for (i, &(x, y)) in log.iter().enumerate() {
        pos[(i % 320) / 16] += ((x - y as f32) as f64).powi(2);
        sig[(i % 320) / 16] += (x as f64).powi(2);
    }
    let v: Vec<String> = pos
        .iter()
        .zip(sig.iter())
        .map(|(e, s)| format!("{:.1}", 10.0 * (s / e).log10()))
        .collect();
    if let Ok(f) = std::env::var("DUMP_FRAME") {
        let f: usize = f.parse().unwrap();
        for i in (f * 320..f * 320 + 320).step_by(4) {
            eprintln!("{i} {:.0} {}", log[i].0, log[i].1);
        }
    }
    eprintln!("snr by position {}", v.join(" "));
}

#[test]
#[ignore]
fn ltp_on_periodic() {
    let n = 48000;
    let mut pcm = vec![0f32; n];
    let period = 48000.0 / 125.0;
    let mut res = [[0f32; 2]; 3];
    let formants = [(600f32, 80f32), (1500.0, 100.0), (2500.0, 150.0)];
    for (i, o) in pcm.iter_mut().enumerate() {
        let src = if (i as f32 % period) < 1.0 { 1.0 } else { 0.0 };
        let mut y = 0f32;
        for (k, &(f, bw)) in formants.iter().enumerate() {
            let r = (-std::f32::consts::PI * bw / 48000.0).exp();
            let c = 2.0 * r * (2.0 * std::f32::consts::PI * f / 48000.0).cos();
            let v = src + c * res[k][0] - r * r * res[k][1];
            res[k][1] = res[k][0];
            res[k][0] = v;
            y += v * (1.0 - r);
        }
        *o = y;
    }
    let peak = pcm.iter().fold(0f32, |m, v| m.max(v.abs()));
    pcm.iter_mut().for_each(|v| *v *= 0.3 / peak);
    let mut enc = SilkEncoder::new(1);
    let mut rc = RangeEncoder::new(1275);
    for chunk in pcm.chunks_exact(960) {
        rc.reset(1275);
        enc.encode(&mut rc, chunk, Bandwidth::Wideband, 312);
    }
}

#[test]
#[ignore]
fn encoder_view_matches_decoder() {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp/eval/speech.f32");
    let pcm = read_f32(&raw);
    let pcm = &pcm[..48000 * 10];
    let mut enc = SilkEncoder::new(1);
    let mut rc = RangeEncoder::new(1275);
    let mut packets = Vec::new();
    for chunk in pcm.chunks_exact(960) {
        rc.reset(1275);
        enc.encode(&mut rc, chunk, Bandwidth::Wideband, 312);
        let nbytes = ((rc.tell() + 7) >> 3) as usize;
        let buf = rc.finish();
        let mut p = vec![(9u8) << 3];
        p.extend_from_slice(&buf[..nbytes]);
        packets.push(p);
    }
    let log = &enc.ch.log;
    let xs: Vec<f32> = log.iter().map(|v| v.0).collect();
    let ys: Vec<f32> = log.iter().map(|v| v.1 as f32).collect();
    let mut dec = crate::opus::StreamDecoder::new(16000, 1).unwrap();
    let mut out = Vec::new();
    for p in &packets {
        dec.decode_interleaved(Some(p), &mut out).unwrap();
    }
    let out: Vec<f32> = out.iter().map(|v| v * 32768.0).collect();
    let (s1, l1) = snr_aligned(&xs, &ys, 4);
    let (s2, l2) = snr_aligned(&ys, &out, 200);
    let (s3, l3) = snr_aligned(&xs, &out, 200);
    for i in (16000..16040).step_by(4) {
        eprintln!("{i}: x {:.0} xq {:.0} dec {:.0}", xs[i], ys[i], out[i + 13]);
    }
    eprintln!(
        "enc view x->xq {s1:.2} (lag {l1}); xq->decoder {s2:.2} (lag {l2}); x->decoder {s3:.2} (lag {l3})"
    );
}
