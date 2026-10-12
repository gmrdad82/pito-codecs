use super::*;
use crate::media::Packet;
use std::io::Cursor;

const MUTATIONS: usize = 2000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn avc() -> AvcConfig {
    AvcConfig::new(
        vec![vec![0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50]],
        vec![vec![0x68, 0xEB, 0xE3, 0xCB]],
    )
}

fn hevc() -> HevcConfig {
    HevcConfig {
        profile: 1,
        level: 93,
        chroma_format: 1,
        bit_depth_luma: 8,
        bit_depth_chroma: 8,
        length_size: 4,
        arrays: [32u8, 33, 34]
            .iter()
            .map(|&t| NalArray {
                completeness: true,
                nal_type: t,
                nalus: vec![vec![t << 1, 1, 0x0C, 0x01]],
            })
            .collect(),
        ..Default::default()
    }
}

fn good(brand: Brand, fragmented: bool) -> Vec<u8> {
    let video = TrackConfig::new(SampleEntry::h264(avc(), 64, 48), 90_000);
    let mut audio = TrackConfig::new(SampleEntry::aac(vec![0x12, 0x10], 2, 48_000, 0), 48_000);
    audio.media_start = Some(1024);
    let frames: Vec<(Packet, Packet)> = (0..24i64)
        .map(|i| {
            let order = [0i64, 3, 1, 2][(i % 4) as usize] + i / 4 * 4;
            let v = Packet {
                data: vec![0, 0, 0, 3, 0x65, i as u8, 7],
                pts: order * 3000 + 3000,
                dts: i * 3000,
                keyframe: i % 8 == 0,
            };
            let a = Packet {
                data: vec![0x21, i as u8, 0x5A],
                pts: i * 1024,
                dts: i * 1024,
                keyframe: true,
            };
            (v, a)
        })
        .collect();
    if fragmented {
        let mut w = FragmentedWriter::new(Vec::new(), 0.2);
        let (v, a) = (w.add_track(video).unwrap(), w.add_track(audio).unwrap());
        for (pv, pa) in &frames {
            w.push(v, pv).unwrap();
            w.push(a, pa).unwrap();
        }
        return w.finish().unwrap();
    }
    let mut w = Writer::new(Cursor::new(Vec::new()), Options { brand, reserve: 0 }).unwrap();
    let v = w.add_track(video).unwrap();
    let a = w.add_track(audio).unwrap();
    let e = w.add_entry(v, SampleEntry::hevc(hevc(), 64, 48)).unwrap();
    for (i, (pv, pa)) in frames.iter().enumerate() {
        if i == 16 {
            w.use_entry(v, e).unwrap();
        }
        w.push(v, pv).unwrap();
        w.push_sample(a, &pa.data, pa.pts, pa.dts, 1024, true)
            .unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn exercise(data: &[u8]) {
    let Ok(r) = Reader::new(data) else {
        return;
    };
    let _ = r.duration_seconds();
    for (ti, t) in r.tracks.iter().enumerate() {
        let _ = (t.codec(), t.extradata(), t.rotation(), t.channels());
        let _ = (t.sample_rate(), t.duration_seconds(), t.bitrate(), t.fps());
        let _ = (t.sample_at(0), t.sample_at(i64::MAX), t.sample_at(i64::MIN));
        let _ = (t.keyframe_before(usize::MAX), t.presentation(0));
        for e in &t.entries {
            if let Some(x) = e.codec.extradata() {
                let _ = AvcConfig::parse(&x);
                let _ = HevcConfig::parse(&x);
            }
        }
        for i in 0..t.samples.len() {
            let _ = r.packet(ti, i);
        }
    }
    for snap in [Snap::Outward, Snap::Inward] {
        let Ok(cut) = plan(&r, 0.1, 0.6, snap) else {
            continue;
        };
        let Ok(mut w) = Writer::new(Cursor::new(Vec::new()), Options::default()) else {
            continue;
        };
        for (ti, range) in &cut.tracks {
            let Ok(cfg) = TrackConfig::from_track(&r.tracks[*ti]) else {
                continue;
            };
            let Ok(dst) = w.add_track(cfg) else {
                continue;
            };
            let _ = copy_samples(&r, *ti, range.clone(), &mut w, dst, 0);
        }
        let _ = w.finish();
    }
}

fn mutate(rng: &mut Rng, base: &[u8]) -> Vec<u8> {
    let mut d = base.to_vec();
    for _ in 0..1 + rng.below(3) {
        let at = rng.below(d.len());
        match rng.below(4) {
            0 => d[at] ^= 1 << rng.below(8),
            1 => d[at] = rng.next() as u8,
            _ => {
                let v: u32 = match rng.below(6) {
                    0 => 0,
                    1 => 1,
                    2 => u32::MAX,
                    3 => 0x7FFF_FFFF,
                    4 => 8,
                    _ => rng.next() as u32 & 0xFFFF,
                };
                let end = (at + 4).min(d.len());
                d[at..end].copy_from_slice(&v.to_be_bytes()[..end - at]);
            }
        }
    }
    d
}

#[test]
fn malformed_files_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for (brand, fragmented) in [(Brand::Mp4, false), (Brand::Mov, false), (Brand::Mp4, true)] {
        let base = good(brand, fragmented);
        let r = Reader::new(base.as_slice()).unwrap();
        assert_eq!(r.tracks.len(), 2);
        assert_eq!(r.tracks[0].samples.len(), 24);
        for cut in (0..base.len()).filter(|&c| c < 400 || c % 7 == 0) {
            exercise(&base[..cut]);
        }
        for _ in 0..MUTATIONS {
            exercise(&mutate(&mut rng, &base));
        }
    }
    for config in [avc().to_bytes(), hevc().to_bytes()] {
        for cut in 0..config.len() {
            let _ = AvcConfig::parse(&config[..cut]);
            let _ = HevcConfig::parse(&config[..cut]);
        }
        for _ in 0..MUTATIONS {
            let d = mutate(&mut rng, &config);
            let _ = AvcConfig::parse(&d);
            let _ = HevcConfig::parse(&d);
        }
    }
}

struct Sparse(Vec<u8>, u64);

impl ByteSource for Sparse {
    fn len(&self) -> u64 {
        self.1
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        buf.fill(0);
        let start = (offset as usize).min(self.0.len());
        let n = (self.0.len() - start).min(buf.len());
        buf[..n].copy_from_slice(&self.0[start..start + n]);
        Ok(())
    }
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = (8 + body.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn empty_run(count: u32) -> Vec<u8> {
    let mut w = FragmentedWriter::new(Vec::new(), 0.2);
    let v = w
        .add_track(TrackConfig::new(SampleEntry::h264(avc(), 64, 48), 90_000))
        .unwrap();
    w.push(
        v,
        &Packet {
            data: vec![0, 0, 0, 3, 0x65, 1, 7],
            pts: 0,
            dts: 0,
            keyframe: true,
        },
    )
    .unwrap();
    let full = w.finish().unwrap();
    let id = Reader::new(full.as_slice()).unwrap().tracks[0].id;
    let moof = full.windows(4).position(|w| w == b"moof").unwrap() - 4;
    let mut d = full[..moof].to_vec();
    let mut tfhd = vec![0, 0x02, 0x00, 0x10];
    tfhd.extend(id.to_be_bytes());
    tfhd.extend(0u32.to_be_bytes());
    let mut trun = vec![0, 0, 0, 0];
    trun.extend(count.to_be_bytes());
    let mut mfhd = vec![0; 4];
    mfhd.extend(1u32.to_be_bytes());
    let traf = [bx(b"tfhd", &tfhd), bx(b"trun", &trun)].concat();
    d.extend(bx(
        b"moof",
        &[bx(b"mfhd", &mfhd), bx(b"traf", &traf)].concat(),
    ));
    d
}

#[test]
fn runs_of_empty_samples_are_bounded() {
    let started = std::time::Instant::now();
    assert!(Reader::new(Sparse(empty_run((1 << 26) - 1), 1 << 31)).is_err());
    assert!(started.elapsed().as_secs() < 5);
    let r = Reader::new(Sparse(empty_run(3), 1 << 31)).unwrap();
    assert_eq!(r.tracks[0].samples.len(), 3);
}
